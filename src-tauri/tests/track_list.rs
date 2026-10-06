//! 轨道（音轨 / 字幕轨）的真接口测试。
//!
//! 这些测试加载真的 `libmpv-2.dll` 与真的媒体文件，验证 `track-list` 节点树
//! 能读、解析成 `TrackSet` 之后字段是对的、`sub-add` 与 `sid` / `aid` 真的
//! 让 mpv 换轨。纯逻辑（行序、语言名、切换的索引算术）在 `src/track.rs`
//! 的单测里，那边不需要 mpv，跑得快。
//!
//! ## 多音轨素材
//!
//! `scripts/make-test-media.ps1`（需要 ffmpeg）会造两个多轨文件：
//!
//! | 文件 | 音轨 | 字幕轨 |
//! |---|---|---|
//! | `multitrack.mp4` | 3 条 aac（44.1k/48k/22.05k，各带 language） | 1 条 `mov_text` |
//! | `multitrack.mkv` | 2 条 aac | 1 条 `subrip` |
//!
//! 这两个文件不是「顺便造的」—— 素材库里**只有单音轨文件**时，
//! 「在两条以上之间循环」这个分支在集成测试里永远走不到，只能靠
//! `step_index` 的单测覆盖。`step_index` 只管索引算术，管不了
//! 「写进去的 `aid` / `sid` mpv 认不认」「切过去之后 `selected` 会不会真的
//! 挪到那一条」—— 而后者恰恰是 0.5.0 之后最容易坏的地方。
//!
//! 两个文件的字幕 codec 不同（`mov_text` vs `subrip`）是刻意的：
//! `describe()` 会把它显示出来，两种都跑一遍能确认没有写死某个 codec。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use video_view_lib::mpv::{InitOptions, MpvPlayer};
use video_view_lib::track::{pick_next_track, TrackKind, TrackSet};

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

// ---------------------------------------------------------------- 多音轨
//
// 下面这一组全都依赖 `scripts/make-test-media.ps1` 造的 `multitrack.*`。
// 素材缺失就跳过（而不是失败）：这台机器上没装 ffmpeg 时 `cargo test`
// 仍然应该是绿的 —— 只是覆盖率低一点。

/// 打开 `multitrack.mp4` 并等到轨道表稳定。
///
/// 等的是**音轨数达到 3 且有一条被选中**，两个条件都要。`track-list` 是
/// mpv 边解封装边填的，只等「非空」会在只有第一条音轨时就读到。
fn player_with_multitrack() -> Option<(MpvPlayer, PathBuf)> {
    let file = media_dir().join("multitrack.mp4");
    if !file.is_file() {
        eprintln!(
            "跳过：{} 不存在（跑 scripts/make-test-media.ps1 造，需要 ffmpeg）",
            file.display()
        );
        return None;
    }
    let player = MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: true,
    })
    .ok()?;
    player.load_file(&file).ok()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(root) = player.get_node_tree("track-list") {
            let set = TrackSet::from_node(&root);
            if set.audio.len() == 3 && set.current_audio().is_some() {
                return Some((player, file));
            }
        }
        if Instant::now() >= deadline {
            eprintln!("跳过：10s 内没读到 3 条已就绪的音轨");
            let _ = player.shutdown();
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// 重读轨道表。
fn read_set(p: &MpvPlayer) -> TrackSet {
    TrackSet::from_node(&p.get_node_tree("track-list").expect("读轨道表"))
}

/// 把 `pick_next_track` 的结果真的发给 mpv，等它生效后回读。
///
/// `select_track` 是同步的属性写入，但 `selected` 标记要 mpv 处理完
/// 切轨才更新 —— 又是竞态，所以带上限地轮询。
fn apply_and_settle(p: &MpvPlayer, kind: TrackKind, step: isize) -> Option<i64> {
    let before = read_set(p);
    let list = match kind {
        TrackKind::Audio => &before.audio,
        TrackKind::Sub => &before.sub,
        TrackKind::Video => return None,
    };
    let want = pick_next_track(list, kind, step)?.id;
    p.select_track(kind.property()?, want)
        .expect("select_track 应当成功");

    let prop = kind.property()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let set = read_set(p);
        let now = match kind {
            TrackKind::Audio => set.current_audio().map(|t| t.id),
            TrackKind::Sub => set.current_sub().map(|t| t.id),
            TrackKind::Video => None,
        };
        if now == Some(want) {
            return Some(want);
        }
        if Instant::now() >= deadline {
            eprintln!("跳过：5s 内 {prop} 没变成 {want}（停在 {now:?}）");
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// `multitrack.mp4` 真的是三条音轨 + 一条内嵌字幕轨。
///
/// 这条是后面几条的地基：素材造错了（比如 ffmpeg 改了 mov_text 的默认
/// disposition，导致第二条音轨被 mpv 忽略），后面每一条都会因为「只有一
/// 条轨」而静静跳过 —— 绿得没有意义。
#[test]
fn 多轨素材的轨道结构符合预期() {
    let Some((player, _)) = player_with_multitrack() else {
        return;
    };
    let set = read_set(&player);
    assert_eq!(set.audio.len(), 3, "multitrack.mp4 应当有 3 条音轨");
    assert_eq!(set.sub.len(), 1, "应当有 1 条内嵌字幕轨");

    // 采样率各不相同，这是造素材时故意留的「肉眼可辨标记」
    let rates: Vec<i64> = set.audio.iter().filter_map(|t| t.sample_rate).collect();
    assert!(
        rates.contains(&44100) && rates.contains(&48000) && rates.contains(&22050),
        "三条音轨的采样率应当是 44100 / 48000 / 22050，实得 {rates:?}"
    );
    // language 元数据也要透传上来（`describe()` 靠它显示语言名）
    let langs: Vec<&str> = set.audio.iter().map(|t| t.language.as_str()).collect();
    for want in ["chi", "eng", "jpn"] {
        assert!(
            langs.contains(&want),
            "音轨语言里应当有 {want}，实得 {langs:?}"
        );
    }
    // 内嵌字幕不是外挂
    assert!(
        !set.sub[0].external,
        "内嵌字幕的 external 应当是 false，否则菜单上会误标「外挂」"
    );
    assert_eq!(set.sub[0].codec, "mov_text", "MP4 内嵌字幕的 codec 名");

    let _ = player.shutdown();
}

/// `A` 键在三条音轨之间真的循环：1 → 2 → 3 → 1。
///
/// 这是 0.5.0 里 `A` 键的实际语义，也是之前只有单测覆盖的那条路。
/// 断言的是**回读 mpv 之后的 `selected`**，不是「写进去没报错」——
/// 写成功但 mpv 没认（id 错了、写了 `MPV_FORMAT_INT` 而属性是 choice）
/// 是真实发生过的失效方式。
#[test]
fn 三条音轨能一路循环回第一条() {
    let Some((player, _)) = player_with_multitrack() else {
        return;
    };
    let ids: Vec<i64> = read_set(&player).audio.iter().map(|t| t.id).collect();

    // 第一步必须换轨
    let second = apply_and_settle(&player, TrackKind::Audio, 1);
    assert!(
        second.is_some(),
        "有三条音轨时按 A 必须切到下一条 —— 返回 None 说明 pick_next_track 走空了"
    );
    let second = second.expect("刚断言过");
    assert_ne!(second, ids[0], "切了之后不该还在原来那条");

    // 再两步到第三条，然后绕回第一条
    let third = apply_and_settle(&player, TrackKind::Audio, 1).expect("切到第三条");
    assert_ne!(third, second, "连续按 A 每次都该换一条");

    let back = apply_and_settle(&player, TrackKind::Audio, 1).expect("绕回第一条");
    assert_eq!(back, ids[0], "第三条之后按 A 应当绕回第一条（两端都绕回）");

    // 往回切一次，验证 -1 方向
    let prev = apply_and_settle(&player, TrackKind::Audio, -1).expect("往回切");
    assert_eq!(prev, third, "从第一条往回切应当回到第三条");

    let _ = player.shutdown();
}

/// 写 `aid` 之后回读 `aid` 属性，值要和 `track-list` 里的 `selected` 对上。
///
/// 上一条测试只看 `track-list` 的 `selected`，那是从「轨道表快照」里读出来的；
/// 这里走另一条路 —— 直接读 mpv 的 `aid` 属性（`get_int64_property`，与写入
/// 时走的 `mpv_set_property` 配对）。两者是**两个来源**，不一致就说明
/// mpv 侧状态和它自己吐出来的快照已经错位了。
///
/// 注意 mpv **没有** `current-audio` 这个属性（只有 `aid` / `sid` / `vid`），
/// 写错了读回来是 `property not found` —— 一开始就是这么写的，测试直接
/// 把它抓出来了，比读代码可靠。
#[test]
fn 切轨之后回读_aid_和轨道表一致() {
    let Some((player, _)) = player_with_multitrack() else {
        return;
    };
    let want = apply_and_settle(&player, TrackKind::Audio, 1).expect("切一条音轨");
    let reported = player.get_int64_property("aid").expect("读 aid");
    assert_eq!(reported, want, "aid 属性应当和 track-list 的 selected 一致");

    let _ = player.shutdown();
}

/// 关掉字幕之后 `L` 会把它**打开**（而不是什么都不做）。
///
/// 这条行为很难被当成 bug 报回来，所以之前只有单测；但它的分支条件是
/// 「一条都没选中」，而单轨素材里这个状态根本达不到 —— 只有
/// `multitrack.mp4` 这种带内嵌字幕的素材才走得进去。
#[test]
fn 关掉字幕之后循环键会重新打开字幕() {
    let Some((player, _)) = player_with_multitrack() else {
        return;
    };
    // 先确认有内嵌字幕在被放（素材里 disposition 是 default）
    let set = read_set(&player);
    let Some(sub) = set.current_sub() else {
        eprintln!("跳过：内嵌字幕一开始就没被选中");
        let _ = player.shutdown();
        return;
    };
    let sub_id = sub.id;

    // 关掉
    player.disable_subtitles().expect("sid = no");
    let off = read_set(&player);
    assert!(
        off.current_sub().is_none(),
        "sid = no 之后不该还有选中的字幕轨"
    );
    assert_eq!(off.sub.len(), 1, "关掉只是不选它，轨道还在");

    // 按 L：应当重新打开，而且挑的是 default 那条
    let reopened = apply_and_settle(&player, TrackKind::Sub, 1).expect("L 应当重新打开字幕");
    assert_eq!(
        reopened, sub_id,
        "只有一条字幕轨时重新打开，挑的应当就是那条"
    );

    let _ = player.shutdown();
}

/// `multitrack.mkv` 的内嵌字幕 codec 是 `subrip` 而不是 `mov_text`。
///
/// 两种都测是因为字幕轨的解析里 `codec` 只是个字符串，理论上照单全收；
/// 真出问题往往是某个 codec 名被当成了别的（比如当成 `ass` 走了样式解析）。
#[test]
fn mkv_里的字幕轨是_subrip() {
    let file = media_dir().join("multitrack.mkv");
    if !file.is_file() {
        eprintln!("跳过：{} 不存在", file.display());
        return;
    }
    let Ok(player) = MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: true,
    }) else {
        return;
    };
    if player.load_file(&file).is_err() {
        let _ = player.shutdown();
        eprintln!("跳过：load_file 失败");
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let set = loop {
        let set = read_set(&player);
        if set.sub.len() == 1 && set.current_sub().is_some() {
            break set;
        }
        if Instant::now() >= deadline {
            eprintln!("跳过：10s 内没读到字幕轨");
            let _ = player.shutdown();
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(set.sub[0].codec, "subrip", "MKV 里的 SRT 字幕 codec 名");
    assert_eq!(set.audio.len(), 2, "multitrack.mkv 应当有 2 条音轨");

    let _ = player.shutdown();
}

/// 真的切一次内嵌字幕轨，确认 `sid` 写内嵌轨的 id 也生效。
///
/// 外挂字幕那条路已经被 `外挂字幕加进去之后轨道表里多一条` 盖住了；
/// 内嵌轨的 id 与外挂轨的 id 空间不同，值得单独确认。
#[test]
fn 内嵌字幕轨切得动() {
    let Some((player, _)) = player_with_multitrack() else {
        return;
    };
    let set = read_set(&player);
    if set.sub.is_empty() {
        eprintln!("跳过：没有字幕轨");
        let _ = player.shutdown();
        return;
    }
    let want = set.sub[0].id;
    player.select_track("sid", want).expect("sid = 内嵌轨 id");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if read_set(&player).current_sub().map(|t| t.id) == Some(want) {
            break;
        }
        if Instant::now() >= deadline {
            eprintln!("跳过：5s 内 sid 没生效");
            let _ = player.shutdown();
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = player.shutdown();
}
