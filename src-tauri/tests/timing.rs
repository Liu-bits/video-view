//! A-B 循环与字幕/音频延迟的**真接口**测试。
//!
//! ## 为什么这些必须打真 mpv
//!
//! A-B 循环不是我们自己实现的循环 —— 它是 mpv 的 `ab-loop-a` / `ab-loop-b`
//! 两个属性。而实测（0.8.0 开发时）这个 libmpv 0.41 里：
//!
//! * `ab-loop-a` / `ab-loop-b` **存在**，`type` 是 `Time`，默认 `no`
//! * **没有** `ab-loop` 这个开关属性（`property not found`）
//! * `command ab-loop no` 报 `invalid parameter`
//!
//! 所以「关掉循环」只能是**把 a 设成 `no`**，而「循环是否生效」完全由
//! mpv 决定 —— 我们的代码只是设了两个属性。这条测试要证明的正是
//! 「设了 a 和 b 之后 mpv 真的在循环」，而不是「我们设了两个值」。
//!
//! ## 延迟的属性名与范围也是实测出来的
//!
//! * `sub-delay` 与 `audio-delay` 存在（Double，默认 0）
//! * **完全不夹范围**：`-60` 到 `600` 全收。所以夹取必须在我们这侧
//! * **`video-delay` 不存在** —— 「视频延迟」这个 mpv 属性在这个版本里
//!   没有，所以 0.8.0 只做了字幕与音频两个方向的延迟

use std::time::Duration;
use video_view_lib::mpv::{InitOptions, MpvPlayer};

fn player() -> Option<MpvPlayer> {
    MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: true,
    })
    .ok()
}

fn media(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/media")
        .join(name)
}

/// 读一个 double 属性。
fn get_f64(p: &MpvPlayer, name: &str) -> f64 {
    p.get_property_string(name)
        .unwrap_or_else(|e| panic!("读 {name} 失败：{e}"))
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("{name} 读回来的不是数字：{e}"))
}

fn media_60s() -> std::path::PathBuf {
    media("loop60s.mp4")
}

/// A 点和 B 点都设好之后，mpv **真的会**在这两点之间反复循环。
///
/// 判据是「播放位置**始终**落在 [a, b] 区间内」：设成 [2, 4] 秒，然后
/// 在 8 秒内反复采样 5 次，每一次都必须在区间里。不循环的话，位置会
/// 一路越过 4 秒 —— 这是二值的，不依赖帧率也不依赖机器快慢。
#[test]
fn 设了_a_b_之后_mpv_真的在循环() {
    let f = media_60s();
    if !f.is_file() {
        panic!("media file missing: {}", f.display());
    }
    let Some(p) = player() else {
        panic!("mpv player unavailable, test env is broken")
    };
    p.load_file(&f).expect("load_file");
    std::thread::sleep(Duration::from_millis(900));

    // 先跳到 2 秒，否则一设好 b 就已经越过 4 秒了
    p.set_string_property("ab-loop-a", "2").expect("设 A");
    p.run_command(&["seek", "2", "absolute"]).ok();
    std::thread::sleep(Duration::from_millis(300));
    p.set_string_property("ab-loop-b", "4").expect("设 B");

    for i in 0..6 {
        std::thread::sleep(Duration::from_millis(700));
        let pos = get_f64(&p, "time-pos");
        assert!(
            (2.0..=4.2).contains(&pos),
            "第 {i} 次采样时 time-pos = {pos}，跑出了 A-B 区间 [2, 4] —— 没有在循环"
        );
    }

    // 属性本身也读得回来（界面要靠它显示当前循环范围）
    assert_eq!(get_f64(&p, "ab-loop-a"), 2.0);
    assert_eq!(get_f64(&p, "ab-loop-b"), 4.0);

    let _ = p.shutdown();
}

/// 把 `ab-loop-a` 设成 `no` 就是「关掉循环」—— 因为这个 libmpv 里没有
/// `ab-loop` 开关属性。这条把「关掉之后真的不再循环」钉住。
///
/// 不钉住的后果很具体：界面说「已清除」而 mpv 还在循环，用户看到的是
/// 「我明明点了清除，怎么还在反复播」。
#[test]
fn 把_a_设成_no_就真的停止循环() {
    let f = media_60s();
    if !f.is_file() {
        panic!("media file missing: {}", f.display());
    }
    let Some(p) = player() else {
        panic!("mpv player unavailable, test env is broken")
    };
    p.load_file(&f).expect("load_file");
    std::thread::sleep(Duration::from_millis(900));

    p.set_string_property("ab-loop-a", "2").expect("设 A");
    p.run_command(&["seek", "2", "absolute"]).ok();
    std::thread::sleep(Duration::from_millis(300));
    p.set_string_property("ab-loop-b", "4").expect("设 B");
    std::thread::sleep(Duration::from_millis(500));

    // 清除
    p.set_string_property("ab-loop-a", "no").expect("设 a = no");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        p.get_property_string("ab-loop-a").unwrap_or_default(),
        "no",
        "a 应当读回 no"
    );

    // 之后位置应当能一路越过 4 秒
    p.run_command(&["seek", "2", "absolute"]).ok();
    let mut escaped = false;
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(700));
        if get_f64(&p, "time-pos") > 5.0 {
            escaped = true;
            break;
        }
    }
    assert!(escaped, "清除之后播放位置仍然过不去 5 秒 —— mpv 还在循环");

    let _ = p.shutdown();
}

/// 只设了 a、没设 b 时**不会**循环。
///
/// 这条钉的是菜单里「没标 A 点时标 B 点置灰」那条规则的另一半：既然 mpv
/// 要求两个都有效，那么只设一个就必须**没有效果**，而不是「循环 0 秒」
/// 或者「从 a 播到片尾」。
#[test]
fn 只设_a_不会循环() {
    let f = media_60s();
    if !f.is_file() {
        panic!("media file missing: {}", f.display());
    }
    let Some(p) = player() else {
        panic!("mpv player unavailable, test env is broken")
    };
    p.load_file(&f).expect("load_file");
    std::thread::sleep(Duration::from_millis(900));

    p.set_string_property("ab-loop-a", "2").expect("设 A");
    p.run_command(&["seek", "2", "absolute"]).ok();

    let mut escaped = false;
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(700));
        if get_f64(&p, "time-pos") > 5.0 {
            escaped = true;
            break;
        }
    }
    assert!(escaped, "只设了 A 没有循环 —— 位置应当能一路往前走");

    let _ = p.shutdown();
}

/// 字幕延迟与音频延迟：写得进、读得回、且**不随换文件重置**。
///
/// 最后一条是 0.8.0 的一个明确决定：`App` 的 `UiState::sub_delay` 注释里
/// 写了「不落盘，但跨文件保留」，理由是「一次会话里看一批同样编码问题的
/// 文件时逐个重调很烦」。这是 mpv 的全局属性行为，测试把它钉住，
/// 免得哪天有人以为它会重置而在 `open()` 里加一句「清理」—— 而那句话
/// 会和注释直接矛盾。
#[test]
fn 延迟写得进读得回且跨文件保留() {
    let Some(p) = player() else {
        panic!("mpv player unavailable, test env is broken")
    };

    for (name, v) in [("sub-delay", -0.25), ("audio-delay", 0.35)] {
        p.set_string_property(name, &format!("{v}"))
            .expect("设延迟");
        let back = get_f64(&p, name);
        assert!((back - v).abs() < 1e-6, "{name} 写 {v} 读回 {back}");
    }

    let f = media_60s();
    if !f.is_file() {
        panic!("media file missing: {}", f.display());
    }
    p.load_file(&f).expect("load_file");
    std::thread::sleep(Duration::from_millis(900));
    assert!(
        (get_f64(&p, "sub-delay") - -0.25).abs() < 1e-6,
        "换文件之后 sub-delay 被重置了"
    );
    assert!(
        (get_f64(&p, "audio-delay") - 0.35).abs() < 1e-6,
        "换文件之后 audio-delay 被重置了"
    );

    let _ = p.shutdown();
}

/// mpv 对延迟**完全不夹范围**，所以夹取必须在我们这一侧。
///
/// 这条钉的是「为什么 `App::DELAY_LIMIT` 存在」：如果不夹，用户连按几十次
/// `-` 之后 `sub-delay` 会变成 -600 秒，也就是字幕被推到 10 分钟之后，
/// 用户看到的现象是「字幕没了」而界面上没有任何地方能看出原因。
#[test]
fn mpv_对延迟完全不夹范围_所以夹取必须在我们这侧() {
    let Some(p) = player() else {
        panic!("mpv player unavailable, test env is broken")
    };
    for v in ["-60.0", "-600.0", "600.0"] {
        assert!(
            p.set_string_property("sub-delay", v).is_ok(),
            "mpv 居然收不了 {v} —— 那我们的夹取范围可以放宽"
        );
    }
    assert!(
        (get_f64(&p, "sub-delay") - 600.0).abs() < 1e-6,
        "mpv 应当原样收下 600 秒"
    );
    let _ = p.shutdown();
}

/// **`video-delay` 在这个 libmpv 里不存在。**
///
/// 「视频延迟」是用户对「音画不同步」最直觉的一个方向，但它对应的是 mpv 的
/// `video-delay` 属性 —— 实测这个版本（libmpv 0.41）里**没有**。
///
/// 这条测试的作用是「哪天有了就提醒我们把它接上」：它现在红的不是功能，
/// 而是一条关于**不做什么**的记录。反过来说，如果有人凭手册写了
/// `set_property("video-delay", ...)`，那条不会编译失败也不会 clippy 报警，
/// 只是运行时返回 `property not found` 然后弹一个错误框 —— 和 0.6.1 那个
/// `sub-encoding` 的坑一模一样。
#[test]
fn video_delay_这个属性不存在() {
    let Some(p) = player() else {
        panic!("mpv player unavailable, test env is broken")
    };
    let err = p
        .set_string_property("video-delay", "0.1")
        .expect_err("`video-delay` 不该存在；0.8.0 因此只做了字幕与音频两个方向");
    assert!(
        err.contains("property not found"),
        "报的是 {err:?} —— 如果不是「属性不存在」，说明 libmpv 升级后有了它，这条要改成「去接上它」"
    );
    let _ = p.shutdown();
}
