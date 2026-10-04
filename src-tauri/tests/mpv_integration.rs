//! libmpv 播放核心的集成测试。
//!
//! 这些用例直接驱动真实的 `libmpv-2.dll`，验证「格式能不能打开」这个
//! 项目的核心承诺，而不是只验证能编译。
//!
//! 前置条件：测试媒体放在 `tests/media/`，可用 `scripts/make-test-media.sh`
//! 之类的工具生成。这里只跑存在的文件，缺失时自动跳过并提示。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use video_view_lib::mpv::{InitOptions, MpvEventMessage, MpvPlayer};

/// 测试用的媒体目录。可用 `VIDEO_VIEW_TEST_MEDIA` 覆盖。
fn media_dir() -> PathBuf {
    std::env::var("VIDEO_VIEW_TEST_MEDIA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media"))
}

/// 收集需要验证的媒体文件。
fn test_files() -> Vec<PathBuf> {
    let dir = media_dir();
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("读取测试媒体目录失败")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("mp4" | "mkv" | "avi" | "webm" | "flv" | "wmv" | "mov" | "ogg")
                )
        })
        .collect();
    files.sort();
    files
}

/// 建一个不带视频输出的 mpv 实例。
///
/// 测试不需要画面，`vo=null` 可以完全绕开 D3D，避免在没有 GPU 的
/// CI 环境里因为视频输出初始化失败而误报。
fn headless_player() -> MpvPlayer {
    MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: true,
    })
    .expect("创建 mpv 实例失败（libmpv-2.dll 是否就位？）")
}

#[test]
fn 能加载_dll_并初始化() {
    let player = headless_player();
    // libmpv 的 API 版本号编码为 (major << 16) | minor
    let version = player.api_version();
    assert_eq!(
        version >> 16,
        2,
        "期望 libmpv 2.x，实际 API 版本 {}.{}",
        version >> 16,
        version & 0xffff
    );
    player.shutdown().expect("quit 命令应有效");
}

/// 逐个打开测试媒体，确认能读出时长，即真的完成了解码初始化。
#[test]
fn 能播放各种格式() {
    let files = test_files();
    if files.is_empty() {
        eprintln!(
            "跳过：{} 下没有测试媒体",
            media_dir().display()
        );
        return;
    }
    assert!(
        files.len() >= 5,
        "测试媒体过少（{} 个），覆盖不到主要格式",
        files.len()
    );

    let player = headless_player();
    let (tx, rx) = mpsc::channel();
    player.spawn_event_loop(Box::new(move |msg| {
        let _ = tx.send(msg);
    }));

    let mut failures = Vec::new();

    for file in &files {
        let name = file.file_name().unwrap_or_default().to_string_lossy();

        let _ = player.stop();
        if player.load_file(&file.to_string_lossy()).is_err() {
            failures.push(format!("{name}: loadfile 命令被拒绝"));
            continue;
        }

        let deadline = Instant::now() + Duration::from_secs(15);

        // 第一步：等 FileLoaded 当作同步点。
        // mpv 初始化时会把所有观察属性的当前值（含 duration=0、idle-active=true）
        // 一次性推入事件队列。不先消费到 FileLoaded，就会把上一份文件的残留
        // 事件误当成本文件的状态。
        let mut loaded = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok(MpvEventMessage::FileLoaded) => {
                    loaded = true;
                    break;
                }
                Ok(MpvEventMessage::Error(e)) => {
                    failures.push(format!("{name}: {e}"));
                    break;
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }

        if !loaded {
            println!("  FAIL {name:<16} 未收到 FileLoaded");
            failures.push(format!("{name}: 15 秒内没有收到 FileLoaded"));
            continue;
        }

        // 第二步：此刻队列里的属性事件已属于本文件，读出时长
        let mut duration = 0.0_f64;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok(MpvEventMessage::Property(p)) if p.name == "duration" => {
                    if let Some(v) = p.value.as_f64() {
                        if v > 0.0 {
                            duration = v;
                            break;
                        }
                    }
                }
                Ok(MpvEventMessage::Error(e)) => {
                    failures.push(format!("{name}: {e}"));
                    break;
                }
                _ => {}
            }
        }

        if duration > 0.0 {
            println!("  OK  {name:<16} 时长 {duration:.2}s");
        } else {
            println!("  FAIL {name:<16} 未能读出时长");
            failures.push(format!("{name}: FileLoaded 之后没有读到 duration"));
        }
    }

    let _ = player.shutdown();

    assert!(
        failures.is_empty(),
        "以下文件无法播放:\n{}",
        failures.join("\n")
    );
}

/// 播放控制和进度跳转的基本行为。
#[test]
fn 支持播放暂停与跳转() {
    let files = test_files();
    let Some(file) = files.first() else {
        eprintln!("跳过：没有测试媒体");
        return;
    };

    let player = headless_player();
    let (tx, rx) = mpsc::channel();
    player.spawn_event_loop(Box::new(move |msg| {
        let _ = tx.send(msg);
    }));

    player.load_file(&file.to_string_lossy()).expect("加载失败");

    // 等解码真正开始
    let mut started = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(MpvEventMessage::Property(p))
                if p.name == "time-pos" && p.value.as_f64().unwrap_or(0.0) > 0.0 =>
            {
                started = true;
                break;
            }
            _ => {}
        }
    }
    assert!(started, "播放位置始终为 0，文件可能没真正在播");

    // 暂停后位置不应再前进。
    // 这里直接读属性而不用事件：`time-pos` 只在值变化时推送，暂停后它不再变，
    // 也就不会再有事件，依赖事件流会误判成「暂停失败」。
    player.set_pause(true).expect("暂停失败");
    // 等 pause 属性真正生效
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !player.get_pause().unwrap_or(false) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(player.get_pause().expect("读取 pause 失败"), "pause 属性未生效");

    std::thread::sleep(Duration::from_millis(200));
    let pos_before = player.get_double_property("time-pos").expect("读取 time-pos 失败");
    std::thread::sleep(Duration::from_millis(800));
    let pos_after = player.get_double_property("time-pos").expect("读取 time-pos 失败");

    player.set_pause(false).expect("取消暂停失败");

    assert!(
        (pos_after - pos_before).abs() < 0.2,
        "暂停期间播放位置仍在前进（{pos_before} -> {pos_after}）"
    );

    // toggle_pause 必须真的把 pause 属性翻过来。
    //
    // 之前这个用例只用 set_pause（走 mpv_set_property），而 toggle_pause
    // 走的是 mpv_command_async，两条不同的路径。结果 toggle_pause 里的
    // 命令名在 mpv 0.41 上无效、被 mpv 丢弃，而测试全绿 —— 播放/暂停按钮、
    // 空格键、点击画面实际上全是坏的。
    assert!(
        !player.get_pause().expect("读取 pause 失败"),
        "toggle_pause 测试前应处于播放状态"
    );
    player.toggle_pause().expect("toggle_pause 失败");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !player.get_pause().unwrap_or(false) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        player.get_pause().expect("读取 pause 失败"),
        "toggle_pause 没有把 pause 切成 true"
    );

    player.toggle_pause().expect("再次 toggle_pause 失败");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && player.get_pause().unwrap_or(true) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !player.get_pause().expect("读取 pause 失败"),
        "toggle_pause 没有把 pause 切回 false"
    );

    // 跳转：跳到 3 秒后再回读 time-pos
    let target = 3.0_f64;
    player.seek(target).expect("跳转失败");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut reached = false;
    while Instant::now() < deadline {
        if let Ok(MpvEventMessage::Property(p)) = rx.recv_timeout(Duration::from_millis(300)) {
            if p.name == "time-pos" && p.value.as_f64().unwrap_or(0.0) >= target - 0.5 {
                reached = true;
                break;
            }
        }
    }
    assert!(reached, "跳转到 {target}s 后位置没有跟上");

    // 音量和静音
    player.set_volume(42.0).expect("设置音量失败");
    let vol = player.get_volume().expect("读取音量失败");
    assert!((vol - 42.0).abs() < 1.0, "音量未生效，当前 {vol}");

    player.set_mute(true).expect("静音失败");
    assert!(player.get_mute().expect("读取静音失败"), "静音未生效");
    player.set_mute(false).expect("取消静音失败");

    let _ = player.shutdown();
}

/// 打不开的文件必须在入口就报错，不能丢给 mpv。
///
/// 早先的实现是把路径原样交给 `loadfile`，由 mpv 在打开阶段失败、再通过事件
/// 循环报回来。那样对用户是「点了没反应然后弹一个笼统的 mpv 错误」，而且
/// 中间这段时间 mpv 内部可能已经把路径当成 URL 处理过了。
#[test]
fn 打不开的文件会立即报错() {
    let player = headless_player();

    let err = player
        .load_file("Z:/__video_view_definitely_missing__.mp4")
        .expect_err("不存在的文件应该在入口就被拒绝");

    assert!(
        err.contains("找不到文件"),
        "错误信息应说明是文件不存在，实际是：{err}"
    );

    // 目录也不是要打开的东西
    let err = player
        .load_file(env!("CARGO_MANIFEST_DIR"))
        .expect_err("目录应该在入口就被拒绝");
    assert!(err.contains("找不到文件"), "实际是：{err}");

    let _ = player.shutdown();
}

/// mpv 的 `loadfile` 第一个参数是 URL，不是文件名。
///
/// mpv 0.41 支持 http / smb / lavf / avconcat / archive / env / memory / fd
/// 等一批协议，而 0.40 起已经没有 `protocol-white` 选项可用，所以只能在入口
/// 挡：要求「确实是磁盘上已存在的普通文件」，`D:\a.mp4` 与 `http://…` 由此分清。
#[test]
fn 拒绝把协议_url_当成文件打开() {
    let player = headless_player();

    // 这些串如果被当成文件名，Windows 解析出来都不是已存在的文件
    for url in [
        "http://example.com/video.mp4",
        "https://example.com/video.mp4",
        "smb://server/share/video.mp4",
        "lavf://movie=video.mp4",
        "concat://a.mp4|b.mp4",
        "avconcat://a.mp4|b.mp4",
        "archive://a.mp4",
        "env://SECRET",
        "fd://3",
        "memory://buf",
        "file:///C:/Windows/System32/drivers/etc/hosts",
    ] {
        let err = player
            .load_file(url)
            .expect_err("协议 URL 不应该被当成文件接受");
        assert!(
            err.contains("找不到文件"),
            "{url} 的错误信息应说明文件不存在，实际是：{err}"
        );
    }

    // 空串和只有空白同样要挡住
    assert!(player.load_file("").is_err(), "空路径应该被拒绝");
    assert!(player.load_file("   ").is_err(), "纯空白路径应该被拒绝");

    let _ = player.shutdown();
}
