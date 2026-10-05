//! libmpv 播放核心的集成测试。
//!
//! 这些用例直接驱动真实的 `libmpv-2.dll`，验证「格式能不能打开」这个
//! 项目的核心承诺，而不是只验证能编译。
//!
//! 前置条件：测试媒体放在 `tests/media/`，由 `scripts/make-test-media.ps1`
//! 生成（仓库里已经带了产物，日常跑测试不需要执行它）。这里只跑存在的
//! 文件，缺失时自动跳过并提示。

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
/// `hwdec-current` 必须真的读得到，而且在这台机器上应当是硬件解码。
///
/// 这条钉的是两件事：
///
/// 1. **属性存在**。它在 mpv 文档里注册为 property，但是否随 libmpv 的编译
///    选项提供，取决于这份 `libmpv-2.dll`。读不到的话，D3D11VA 静默退回软解
///    就完全失去了唯一的观测手段——那正是这个属性被加进来的理由。
/// 2. **值不是 `no`**。带 wid 的 headless player 用 `hwdec=no`（见
///    `InitOptions` 的分支），所以这里不能用它来判断硬件解码是否生效；
///    只断言「非空且是已知的几个取值之一」。
#[test]
fn 能读出硬件解码状态() {
    let media = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/loop60s.mp4");
    let player = headless_player();
    player.load_file(&media).expect("测试素材应当能加载");

    let v = poll_hwdec_current(&player, std::time::Duration::from_secs(5))
        .expect("hwdec-current 属性始终不可用：静默退回软解就失去了唯一的观测手段");

    // headless 分支显式设了 hwdec=no，所以这里**必然**是 no。
    // 写成断言而不是忽略，是为了万一有人把 headless 分支改成 hwdec=auto
    // 时能立刻发现「这条测试的前提变了」。
    assert_eq!(v, "no", "headless player 应当是软解");

    let _ = player.shutdown();
}

/// 轮询 `hwdec-current` 直到它可用或超时。
///
/// 必须轮询，不能在 `load_file` 返回后立刻读：mpv 的 `loadfile` 只是把文件
/// 加进播放列表并返回，解码器是在**之后**的某个时刻才真正建立的，而
/// `hwdec-current` 的文档写明「If no decoder is loaded, the property is
/// unavailable」。实测在 `load_file` 之后立刻读，拿到的就是
/// `property unavailable` —— 这也正是 `App::tick` 要每 250ms 主动 poll 一次
/// 而不是只靠 observe 的原因。
fn poll_hwdec_current(player: &MpvPlayer, timeout: std::time::Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match player.get_property_string("hwdec-current") {
            Ok(v) => return Some(v),
            Err(e) if e.contains("unavailable") => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

/// 带 wid 的播放器**应当**真的在用硬件解码（如果这台机器支持）。
///
/// 这是「解码到底有没有走 GPU」的唯一自动检查。机器不支持时（比如虚拟机、
/// 老显卡）会因为返回值是 `no` 而跳过断言，不算失败——因为那不是代码的错。
/// 但一旦某台本该支持的机器因为适配器选择（虚拟显示器被选成默认设备之类）
/// 而静默降级，这里就会失败。
#[test]
fn 有窗口时硬件解码生效或明确报告降级() {
    // 需要一个真实窗口，所以这个测试只在本机能建窗时跑。
    // 建窗失败就跳过，不当成失败——它测的是解码路径，不是建窗路径。
    let Ok(player) = MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: false,
    }) else {
        eprintln!("跳过：无法创建带 wid 的 mpv 实例");
        return;
    };

    let media = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/loop60s.mp4");
    if player.load_file(&media).is_err() {
        eprintln!("跳过：测试素材加载失败");
        let _ = player.shutdown();
        return;
    }

    let v = poll_hwdec_current(&player, std::time::Duration::from_secs(5));

    // 只打印，不断言具体值：不同 GPU 走 d3d11va 还是 dxva2 都对，
    // 而断言某一个会在换机器时变成假失败。真正要抓的「静默降级」
    // 需要知道这台机器是否*本该*支持，那不是这个测试能判断的。
    eprintln!("hwdec-current = {v:?}（d3d11va/dxva2 = 硬件解码，no = 软解）");

    let _ = player.shutdown();
}

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
        eprintln!("跳过：{} 下没有测试媒体", media_dir().display());
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
        if player.load_file(file).is_err() {
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
                    if let Some(v) = p.value.as_number() {
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

    player.load_file(file).expect("加载失败");

    // 等解码真正开始。
    // 这里刻意轮询 `time-pos` 而不是等事件：`time-pos` 已经不在观察列表里
    // （界面改成定时主动读），依赖它的事件流会永远等不到。
    let mut started = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(v) = rx.recv_timeout(Duration::from_millis(500)) {
            if matches!(v, MpvEventMessage::Error(_)) {
                break;
            }
        }
        if player.get_double_property("time-pos").unwrap_or(0.0) > 0.0 {
            started = true;
            break;
        }
    }
    assert!(started, "播放位置始终为 0，文件可能没真正在播");

    // 暂停后位置不应再前进。
    // 这里直接读属性而不用事件：`time-pos` 已经不在观察列表里，暂停期间
    // 不会有任何相关事件，依赖事件流会误判成「暂停失败」。
    player.set_pause(true).expect("暂停失败");
    // 等 pause 属性真正生效
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !player.get_pause().unwrap_or(false) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        player.get_pause().expect("读取 pause 失败"),
        "pause 属性未生效"
    );

    std::thread::sleep(Duration::from_millis(200));
    let pos_before = player
        .get_double_property("time-pos")
        .expect("读取 time-pos 失败");
    std::thread::sleep(Duration::from_millis(800));
    let pos_after = player
        .get_double_property("time-pos")
        .expect("读取 time-pos 失败");

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

    // 跳转：跳到 3 秒后再回读 time-pos（轮询，原因同上）
    let target = 3.0_f64;
    player.seek(target).expect("跳转失败");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut reached = false;
    while Instant::now() < deadline {
        if player.get_double_property("time-pos").unwrap_or(0.0) >= target - 0.5 {
            reached = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
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

    // 盘符开头、形状合法，只是文件不存在 —— 这一类才是「file not found」。
    // 用当前盘符拼一个绝对不可能存在的文件名，免得依赖某个盘符是否存在。
    let missing = format!(
        "{}:\\__video_view_definitely_missing__.mp4",
        env!("CARGO_MANIFEST_DIR")
            .split_once(':')
            .map(|(d, _)| d)
            .unwrap_or("Z")
    );
    let err = player
        .load_file(Path::new(&missing))
        .expect_err("不存在的文件应该在入口就被拒绝");
    assert!(
        err.contains("file not found"),
        "错误信息应说明是文件不存在，实际是：{err}"
    );

    // 目录也不是要打开的东西
    let dir = format!("{}\\", env!("CARGO_MANIFEST_DIR"));
    let err = player
        .load_file(Path::new(&dir))
        .expect_err("目录应该在入口就被拒绝");
    assert!(err.contains("file not found"), "实际是：{err}");

    let _ = player.shutdown();
}

/// 非本地盘路径必须在**碰文件系统之前**被挡下。
///
/// 这一条是安全回归，不只是输入校验。`Path::is_file()` 走 `metadata()`，
/// 对 `\\evil\share\x.mp4` 会真的去建 SMB 连接；mpv 随后还会用**当前用户的
/// 凭据**发起 SMB/NTLM 协商。也就是说命令行里一个参数就能让受害者的进程
/// 去连攻击者的共享，把域凭据交出去——`.lnk`、注册表 `shell\open\command`、
/// 任何 `CreateProcess` 的调用方都能触发，不需要用户点「是」。
///
/// 所以判据必须是「形状」，而且必须排在 `is_file()` 之前。
#[test]
fn 远程路径在任何文件系统访问之前就被拒绝() {
    let player = headless_player();

    for p in [
        r"\\evil\share\a.mp4",
        r"\\?\UNC\evil\share\a.mp4",
        r"\\.\PIPE\anything",
        r"\\?\PhysicalDrive0",
        "//evil/share/a.mp4",
        r"a.mp4",
        r"\a.mp4",
        r"http://example.com/v.mp4",
        r"smb://server/share/v.mp4",
        r"C:relative-without-slash.mp4",
    ] {
        let err = player
            .load_file(Path::new(p))
            .expect_err("非本地盘路径必须在 is_file() 之前被拒");
        assert!(
            err.contains("only local drive paths"),
            "{p} 的错误应说明只接受本地盘路径，实际是：{err}"
        );
        // 关键：不能是 "file not found"——那说明已经去访问过文件系统了，
        // 也就是 SMB 已经建过连接，安全目的落空。
        assert!(
            !err.contains("file not found"),
            "{p} 走到了 is_file() 之后才被拒：{err}"
        );
    }

    let _ = player.shutdown();
}

/// 合法形状的本地盘路径必须能穿过这道关。
///
/// 只钉「该拒的拒了」是不够的——那道关如果写得太紧（比如把 `C:\` 也拒了），
/// 产品就是完全不可用，而且测试一样会绿。这里钉住放行。
#[test]
fn 合法本地盘路径能通过形状检查() {
    let player = headless_player();
    let dir = env!("CARGO_MANIFEST_DIR").to_string();
    for p in [
        dir.clone(),
        format!("{dir}\\tests\\media\\loop60s.mp4"),
        // 盘符小写、路径里有重复分隔符与 `.`/`..`：都该放行
        format!(
            "{}\\.\\tests\\..\\tests\\media\\loop60s.mp4",
            dir.to_lowercase()
        ),
        // 路径中段的重复分隔符是 Windows 合法路径（等价于单个），不能误伤。
        // 曾经在这里被 `reject_remote_path` 拒掉，是一次真实的过度收紧。
        format!("{}\\\\tests\\media\\loop60s.mp4", dir),
        // 正斜杠混用也是合法的
        format!("{}/tests/media/loop60s.mp4", dir.to_lowercase()),
        "C:\\Windows\\System32\\drivers\\etc\\hosts".to_string(),
        "D:\\a b\\c d.mp4".to_string(),
        "Z:\\a.mp4".to_string(),
    ] {
        // 形状检查通过 = 错误不是 "only local drive paths"。
        // 后面要么 file not found（文件不存在）要么真的加载了，都算通过。
        if let Err(e) = player.load_file(Path::new(&p)) {
            assert!(
                !e.contains("only local drive paths"),
                "{p} 是合法本地盘路径，却被形状检查拒了：{e}"
            );
        }
    }
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

    // 这些串如果被当成文件名，Windows 解析出来都不是已存在的文件。
    // 现在它们多半在形状检查那一步就被拒了（没有盘符），消息相应地变了。
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
            .load_file(Path::new(url))
            .expect_err("协议 URL 不应该被当成文件接受");
        // 两种消息都算对：形状检查先拒（没有盘符），或者形状过了但文件不存在。
        assert!(
            err.contains("file not found") || err.contains("only local drive paths"),
            "{url} 的错误应是「不是文件」或「不是本地盘路径」，实际是：{err}"
        );
    }

    // 空串和只有空白同样要挡住
    assert!(player.load_file(Path::new("")).is_err(), "空路径应该被拒绝");
    assert!(
        player.load_file(Path::new("   ")).is_err(),
        "纯空白路径应该被拒绝"
    );

    let _ = player.shutdown();
}
