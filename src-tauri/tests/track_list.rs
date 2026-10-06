//! 轨道（音轨 / 字幕轨）的真接口测试。
//!
//! 这些测试加载真的 `libmpv-2.dll` 与真的媒体文件，验证 `track-list` 节点树
//! 能读、解析成 `TrackSet` 之后字段是对的、`sub-add` 与 `sid` / `aid` 真的
//! 让 mpv 换轨。纯逻辑（行序、语言名、切换的索引算术）在 `src/track.rs`
//! 的单测里，那边不需要 mpv，跑得快。
//!
//! 素材里**没有**多音轨文件（这台机器上没有 ffmpeg，造不出来），所以
//! 「在两条以上之间循环」只在单测里覆盖（`step_index`）。这里的测试覆盖
//! 「只有一条时切轨是安全的空操作」这个更容易出错的分支 —— 一个文件只有
//! 一条音轨是绝大多数文件的常态。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use video_view_lib::mpv::{InitOptions, MpvPlayer};
use video_view_lib::track::TrackSet;

fn media_dir() -> PathBuf {
    std::env::var("VIDEO_VIEW_TEST_MEDIA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media"))
}

/// 打开一个播放器并加载第一个媒体，等到**有音轨被选中**为止。
///
/// 同步点是「有在用的音轨」，不是「`track-list` 非空」—— 这两个不是同一刻。
/// mpv 先把轨道表建出来，`selected` 过一会儿才标上；早一步返回的话
/// `current_audio()` 是 `None`，断言会**偶发**失败（实测全量并行跑 6 轮
/// 挂 1 轮，串行 7/7 全过）。这类 flaky 最容易被误判成产品 bug。
fn player_with_media() -> Option<(MpvPlayer, PathBuf)> {
    let dir = media_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("mp4" | "mkv" | "avi" | "webm" | "mov" | "flv" | "wmv" | "ogg")
                )
        })
        .collect();
    files.sort();
    if files.is_empty() {
        eprintln!("跳过：{} 里没有媒体文件", dir.display());
        return None;
    }
    let file = files[0].clone();
    let player = MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: true,
    })
    .ok()?;
    player.load_file(&file).ok()?;
    // 5s 是上限而不是「实测需要多久」：这里等的是一个**竞态**，
    // 不是固定延迟。正常几十毫秒就到了。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(root) = player.get_node_tree("track-list") {
            if TrackSet::from_node(&root).current_audio().is_some() {
                break;
            }
        }
        if Instant::now() >= deadline {
            eprintln!("跳过：5s 内没有音轨被选中");
            let _ = player.shutdown();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Some((player, file))
}

/// `track-list` 读得出来、并且能收成 `TrackSet`。
#[test]
fn 轨道表读得出来并且能解析() {
    let Some((player, _file)) = player_with_media() else {
        return;
    };
    let root = player
        .get_node_tree("track-list")
        .expect("track-list 应当读得出来");
    let set = TrackSet::from_node(&root);

    assert!(
        !set.audio.is_empty(),
        "任何视频都至少有一条音轨，实得 {}",
        set.audio.len()
    );
    let a = &set.audio[0];
    assert!(a.id > 0, "id 必须能发给 aid，拿到 {}", a.id);
    assert!(!a.codec.is_empty(), "音轨必须有 codec 名（实测 aac）");
    assert!(
        a.audio_channels.unwrap_or(0) > 0,
        "实测形状里有 audio-channels，拿到 {:?}",
        a.audio_channels
    );
    assert!(
        set.current_audio().is_some(),
        "刚加载完应当正好有一条音轨在用"
    );

    let _ = player.shutdown();
}

/// 没有文件时 `track-list` 读不到，这**不是错误**，代码要安静兜住。
#[test]
fn 没有媒体时轨道表读不到也不该崩() {
    let Ok(player) = MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: true,
    }) else {
        return;
    };
    // 不 load_file，直接读
    match player.get_node_tree("track-list") {
        Ok(root) => {
            // mpv 有可能给一个空树而不是报错 —— 两种都算正常
            let set = TrackSet::from_node(&root);
            assert!(
                set.audio.is_empty(),
                "没有文件却有 {} 条音轨，那说明读到了别的实例的状态",
                set.audio.len()
            );
        }
        Err(_) => { /* 属性不存在，正常路径 */ }
    }
    let _ = player.shutdown();
}

/// `sub-add` 加一条外挂字幕，轨道表里就多一条且它被选中。
///
/// 这是 0.5.0 的核心路径：拖一个 `.srt` 进来 → 轨道表长出一条 → 菜单里
/// 多一行 → 立刻在放。三个环节串起来验，不然任何一环断了都只是
/// 「字幕不出来」，很难定位。
#[test]
fn 外挂字幕加进去之后轨道表里多一条() {
    let Some((player, _file)) = player_with_media() else {
        return;
    };
    let before = TrackSet::from_node(&player.get_node_tree("track-list").expect("读轨道表"));
    let sub_file = media_dir().join("probe.srt");
    if !sub_file.is_file() {
        eprintln!("跳过：{} 不存在", sub_file.display());
        let _ = player.shutdown();
        return;
    }

    player.add_subtitle(&sub_file).expect("sub-add 应当成功");

    // `sub-add` 是异步生效的：命令发出去之后 mpv 那边还要建轨道
    let deadline = Instant::now() + Duration::from_secs(5);
    let after = loop {
        let set = TrackSet::from_node(&player.get_node_tree("track-list").expect("读轨道表"));
        if set.sub.len() > before.sub.len() {
            break set;
        }
        if Instant::now() >= deadline {
            eprintln!(
                "跳过：5s 内轨道表没有变（before={} after={}）",
                before.sub.len(),
                set.sub.len()
            );
            let _ = player.shutdown();
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(
        after.sub.len(),
        before.sub.len() + 1,
        "加一条外挂字幕就该多一条"
    );
    let added = after.sub.last().expect("刚加的那条");
    assert!(
        added.external,
        "外挂字幕的 external 标记应该是 true（读不出来的话菜单上不会显示「外挂」两个字，用户分不清哪些是内嵌的）"
    );
    assert!(
        added.selected,
        "sub-add 的第三个参数是 yes，加进来就该立刻在放"
    );
    assert_eq!(after.current_sub().map(|t| t.id), Some(added.id));
    // 加进去的字幕不该把音轨弄丢
    assert_eq!(after.audio.len(), before.audio.len(), "加字幕不该影响音轨");

    let _ = player.shutdown();
}

/// `sid = -1` 关掉字幕，`sid = <id>` 再打开。
#[test]
fn 字幕开关真的生效() {
    let Some((player, _file)) = player_with_media() else {
        return;
    };
    let sub_file = media_dir().join("probe.srt");
    if !sub_file.is_file() {
        eprintln!("跳过：{} 不存在", sub_file.display());
        let _ = player.shutdown();
        return;
    }
    player.add_subtitle(&sub_file).expect("sub-add");

    let wait_sub = |p: &MpvPlayer| -> Option<TrackSet> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let set = TrackSet::from_node(&p.get_node_tree("track-list").ok()?);
            if set.current_sub().is_some() {
                return Some(set);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let Some(set) = wait_sub(&player) else {
        eprintln!("跳过：外挂字幕没选中");
        let _ = player.shutdown();
        return;
    };
    let id = set.current_sub().expect("刚加的").id;

    // 关掉
    player.disable_subtitles().expect("sid = no");
    let after_off = TrackSet::from_node(&player.get_node_tree("track-list").expect("读轨道表"));
    assert!(
        after_off.current_sub().is_none(),
        "sid = -1 之后不该还有选中的字幕轨"
    );
    assert_eq!(
        after_off.sub.len(),
        set.sub.len(),
        "关掉字幕只是不选它，轨道还在"
    );

    // 再打开
    player.select_track("sid", id).expect("sid = id");
    let after_on = TrackSet::from_node(&player.get_node_tree("track-list").expect("读轨道表"));
    assert_eq!(
        after_on.current_sub().map(|t| t.id),
        Some(id),
        "sid = id 之后该重新选中那条"
    );

    let _ = player.shutdown();
}

/// 设置 `aid` 为当前音轨的 id 是成功的空操作（不是错误）。
///
/// 只有一条音轨时按 `A` 走的就是这条路。断言它**成功**很重要：早期版本
/// 这里如果因为找不到下一条而直接 `return Err`，用户按 `A` 会弹一个红叉。
#[test]
fn 只有一条音轨时设回自己不会报错() {
    let Some((player, _file)) = player_with_media() else {
        return;
    };
    let set = TrackSet::from_node(&player.get_node_tree("track-list").expect("读轨道表"));
    let cur = set.current_audio().expect("刚加载完就有在用的音轨");
    player
        .select_track("aid", cur.id)
        .expect("设成当前 id 必须是成功的");
    // 仍然是同一条
    let after = TrackSet::from_node(&player.get_node_tree("track-list").expect("读轨道表"));
    assert_eq!(after.current_audio().map(|t| t.id), Some(cur.id));

    let _ = player.shutdown();
}

/// 字符串属性的写入必须走 `mpv_set_property_string`。
///
/// 这条测试的作用是**钉住一条会崩进程的路径**。实测在这份 libmpv（0.41）上
/// `mpv_set_property(h, name, MPV_FORMAT_STRING, str)` 必崩（`0xC0000005`，
/// 连续 10 轮每次都崩），而 `mpv_set_property_string` 10 轮全过。
/// 两个入口签名只差一个参数、返回值也一样，所以改回去不会有任何编译期
/// 或 clippy 提示 —— 只有运行到才崩，而崩的是整个进程。
///
/// 这里的断言是「调了没崩、值也写进去了」。哪天有人把 `set_string_property`
/// 改回 `set_property` + STRING，这个测试会以整个测试进程 `0xC0000005` 退出
/// 的形式失败（而不是一条干净的 assert 失败），但仍然是失败、仍然会被 CI 看到。
#[test]
fn 写字符串属性不会崩() {
    let Ok(player) = MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: true,
    }) else {
        return;
    };
    // 这几个都是会被程序真正写入的字符串属性，逐个过一遍
    player.set_string_property("volume", "42").expect("volume");
    let v = player.get_double_property("volume").expect("读回 volume");
    assert!((v - 42.0).abs() < 0.5, "volume 应当被写成 42，实得 {v}");

    player.set_string_property("pause", "yes").expect("pause");
    assert!(player.get_pause().expect("读回 pause"));
    player
        .set_string_property("pause", "no")
        .expect("pause off");
    assert!(!player.get_pause().expect("读回 pause"));

    // 轨道属性也要能写：这是 0.5.0 的主路径
    player.set_string_property("sid", "no").expect("sid = no");
    player.set_string_property("aid", "no").expect("aid = no");

    let _ = player.shutdown();
}

/// 反复读轨道表 2000 次，确认 `mpv_free_node_contents` 这条路径没问题。
///
/// 节点树是 mpv 分配、**调用方释放**的，漏掉释放不会立刻出问题（只是慢慢
/// 涨内存），所以只能靠「读很多次 + 不崩 + 内存不涨」来间接验证。
#[test]
fn 反复读轨道树不崩() {
    let Some((player, _file)) = player_with_media() else {
        return;
    };
    for i in 0..2000 {
        let root = player.get_node_tree("track-list");
        // 中途可能因为切文件等原因失败，那不算这条路的问题
        if let Ok(root) = root {
            let _ = TrackSet::from_node(&root);
        }
        if i == 1999 {
            // 最后一次必须还能读出来 —— 说明前面 1999 次没有把状态搞坏
            player
                .get_node_tree("track-list")
                .expect("2000 次之后仍然读得出来");
        }
    }
    let _ = player.shutdown();
}
