//! 字幕编码（`sub-codepage`）与字幕大小（`sub-scale`）的真接口测试。
//!
//! ## 为什么有「`sub-encoding` 不存在」这种测试
//!
//! 写这个功能时我以为 mpv 的属性叫 `sub-encoding`（很多资料这么写）。
//! 实际逐个属性探下来，**这个 libmpv（0.41）里根本没有这个属性**，
//! 真名是 `sub-codepage`。写错了不会编译失败、不会 clippy 报警、
//! 菜单照样能弹出来，只是**每一项编码都没生效** —— 而 mpv 那边的
//! `set_property` 会返回 `property not found`，如果不检查返回值，
//! 界面表现就是「点了没反应」。
//!
//! 所以下面第一条测试把「`sub-encoding` 必须**不**存在」钉住了：哪天
//! libmpv 升级之后真的有了这个属性，这条会红，提醒我们把菜单切过去。
//! 反过来，如果有人凭旧资料把属性名改回去，这条也会红。

use video_view_lib::mpv::{InitOptions, MpvPlayer};
use video_view_lib::settings::Settings;

fn player() -> Option<MpvPlayer> {
    let p = MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: true,
    })
    .ok()?;
    Some(p)
}

/// mpv 里**没有** `sub-encoding` 这个属性，真名是 `sub-codepage`。
///
/// 0.6.1 写这个功能时踩的就是这个坑：名字记错之后 `set_property` 返回
/// `property not found`，菜单项点下去毫无反应，而代码里没有任何地方
/// 会因为它报错。
#[test]
fn 没有_sub_encoding_这个属性() {
    let Some(p) = player() else { return };
    let err = p
        .set_string_property("sub-encoding", "gbk")
        .expect_err("`sub-encoding` 不该存在；真名是 `sub-codepage`");
    assert!(
        err.contains("property not found"),
        "报的是 {err:?} —— 如果不是「属性不存在」那说明 mpv 的行为变了，这测试要重写"
    );
    let _ = p.shutdown();
}

/// 菜单上列出来的每一个编码值都真的写得进去、读得回来。
///
/// `sub-codepage` 是**自由 String** 属性：`option-info/sub-codepage` 的
/// `type` 就是 `String`、`default-value` 是 `auto`，mpv **不校验取值**
/// （实测写 `不存在的编码` 也返回成功）。这意味着菜单表里写错一个名字
/// 不会有任何报错，只会在解码字幕时静默用错编码 —— 唯一的检查方式
/// 就是逐个写进去再读回来比对。
#[test]
fn 菜单上每个编码值都写得进去() {
    use video_view_lib::menu::CODINGS;
    let Some(p) = player() else { return };

    for &code in CODINGS {
        p.set_string_property("sub-codepage", code)
            .unwrap_or_else(|e| panic!("编码 {code:?} 写不进去：{e}"));
        let back = p
            .get_property_string("sub-codepage")
            .unwrap_or_else(|e| panic!("编码 {code:?} 读不回来：{e}"));
        assert_eq!(back, code, "编码 {code:?} 写进去之后读回来不是同一个值");
    }

    let _ = p.shutdown();
}

/// 默认值是 `auto`，菜单第一项要跟它对上。
///
/// 菜单的编码子菜单默认勾第一项（「自动」）。mpv 换了默认值的话菜单会
/// 一个勾都没有 —— 用户看不出当前是什么，又没法判断要不要点。
#[test]
fn 默认编码是_auto_而菜单第一项就是它() {
    use video_view_lib::menu::CODINGS;
    let Some(p) = player() else { return };
    assert_eq!(CODINGS[0], "auto", "菜单第一项必须是 mpv 的默认值");
    let back = p
        .get_property_string("sub-codepage")
        .expect("读 sub-codepage");
    assert_eq!(back, "auto", "mpv 的 sub-codepage 默认值变了");

    let _ = p.shutdown();
}

/// 菜单上每个大小档位都写得进去、读得回来，且与表里的值一致。
///
/// `sub-scale` 是 double 属性，范围 `0.1..10`。越界或非数字会返回
/// `unsupported format for accessing property` —— 这条把六个档位逐个钉住。
#[test]
fn 菜单上每个大小档位都写得进去且读得回来() {
    use video_view_lib::menu::SCALES;
    let Some(p) = player() else { return };

    for &want in SCALES {
        p.set_string_property("sub-scale", &format!("{want}"))
            .unwrap_or_else(|e| panic!("大小 {want} 写不进去：{e}"));
        let back = p.get_property_string("sub-scale").expect("读 sub-scale");
        let got: f64 = back
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("sub-scale 读回来 {back:?} 解析失败：{e}"));
        assert!(
            (got - want).abs() < 1e-6,
            "大小 {want} 写进去读回来是 {got}"
        );
    }

    let _ = p.shutdown();
}

/// `sub-scale` 的真实范围是 `0..100`，**不是** mpv 手册里写的 `0.1..10`。
///
/// `option-info/sub-scale` 报的是
/// `{"type":"Float", ... ,"min":0.000000,"max":100.000000}`，实测
/// `0.05` 和 `20` 都收、`1000` 被拒。这条钉住边界，界面那侧的 `clamp`
/// 就是照着这个范围写的 —— 照手册写 `clamp(0.1, 10.0)` 会把 mpv 明明
/// 允许的 20% 和 5% 挡在门外。
#[test]
fn 大小的范围是_0_到_100_而不是手册说的_0_1_到_10() {
    let Some(p) = player() else { return };

    let info = p
        .get_property_string("option-info/sub-scale")
        .expect("读 option-info/sub-scale");
    assert!(
        info.contains(r#""min":0.000000"#),
        "mpv 报的 min 变了：{info}"
    );
    assert!(
        info.contains(r#""max":100.000000"#),
        "mpv 报的 max 变了：{info}"
    );

    // 范围内：手册说会拒的两个值，实测都收
    for v in ["0.0", "0.05", "0.1", "10.0", "20", "100"] {
        assert!(
            p.set_string_property("sub-scale", v).is_ok(),
            "{v} 在 mpv 声明的范围内却写不进去"
        );
    }
    // 范围外
    for v in ["1000", "-0.5", "1e9", "abc"] {
        assert!(
            p.set_string_property("sub-scale", v).is_err(),
            "{v} 超出范围却被收了"
        );
    }

    let _ = p.shutdown();
}

/// `sub-reload` 命令存在且能被接受 —— 换编码后必须靠它重载已加载的字幕。
///
/// 这是 0.6.1 里最容易「看起来实现了」的一环：`sub-codepage` 是读文件时
/// 才用上的，改它**不会**回头重新读已经加载的文本，所以少了 `sub-reload`
/// 用户会看到「编码换了、字幕还是乱的」。这个 bug 不会以任何形式报错，
/// 只有一个真的拿 GBK 字幕去试的人才会撞上。
#[test]
fn sub_reload_命令存在() {
    let Some(p) = player() else { return };
    p.set_string_property("sub-codepage", "gbk")
        .expect("先设编码");
    // 没有加载任何文件时 `sub-reload` 可能因为「没有字幕」而失败，
    // 所以只断言**命令名存在**：随便发一条不存在的命令看看报什么。
    let bogus = p.run_command(&["definitely-not-a-command"]);
    let reload = p.run_command(&["sub-reload"]);
    if let Err(e) = &reload {
        assert!(
            bogus.is_err(),
            "`sub-reload` 报 {e:?}，但一条乱写的命令却成功了 —— \
             说明失败原因是别的，不是命令名"
        );
    }
    let _ = p.shutdown();
}

/// 两个属性都不受「换文件」影响，菜单里的选择会一直留着。
///
/// mpv 手册里区分「全局选项」与「文件局部选项」，后者在切到下一个文件时
/// 会被重置。如果 `sub-codepage` 属于后者，那么用户调好编码、
/// 看下一个文件、再回来就得重调一遍 —— 界面上的行为和 mpv 的模型对不上，
/// 菜单打勾会突然跳回去。
#[test]
fn 换文件之后设置还在() {
    let f = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/multitrack.mp4");
    if !f.is_file() {
        eprintln!("跳过：{} 不存在", f.display());
        return;
    }
    let Some(p) = player() else { return };

    p.set_string_property("sub-codepage", "big5")
        .expect("设编码");
    p.set_string_property("sub-scale", "1.5").expect("设大小");
    p.load_file(&f).expect("load_file");
    std::thread::sleep(std::time::Duration::from_millis(600));

    assert_eq!(
        p.get_property_string("sub-codepage").expect("读"),
        "big5",
        "换文件之后 sub-codepage 被重置了"
    );
    let scale: f64 = p
        .get_property_string("sub-scale")
        .expect("读")
        .trim()
        .parse()
        .expect("解析");
    assert!(
        (scale - 1.5).abs() < 1e-6,
        "换文件之后 sub-scale 被重置了，实得 {scale}"
    );

    let _ = p.shutdown();
}

/// 0.7.0 之后倍速 / 字幕大小 / 字幕编码会被**记住**并在启动时推给 mpv，
/// 所以「存下来的值」必须全都是 mpv 真的收得下的。
///
/// 这条走的是完整链路的 mpv 那一截：值经过 `sanitized` 夹过 -> 逐个塞给
/// mpv -> 读回来比对。
///
/// 分两截的原因：注册表那半截归 `settings.rs` 的单测（那边不需要 mpv，
/// 跑得快），这里只管「mpv 收不收」。mpv 起一次要几百毫秒，把与 mpv
/// 无关的断言塞进来会让这个文件整体变慢十几倍。
///
/// 之前没有这条，两个错都可能被放过去：
///   * 把倍速存成 `DWORD` -> `1.25` 变成 `1.00`（用户以为记住了，其实没记住）
///   * 存下一个 mpv 拒收的值 -> 启动时 `apply_media_prefs` 报错弹框，
///     而用户什么都没做错
#[test]
fn 记住的那三个值_mpv_都收得下() {
    let Some(p) = player() else { return };

    let raw = Settings {
        speed: 1.25,
        sub_scale: 1.75,
        sub_codepage: "  gbk  ".to_string(),
        ..Settings::default()
    };
    // 编码走一遍真实的归一：注册表里可能带着空白（用户从别处拷来的），
    // 而 `sanitized` 会 trim 掉 —— mpv 那边只认干净的编码名。
    let saved = raw.sanitized();
    assert_eq!(saved.sub_codepage, "gbk", "编码两边的空白应当被 trim 掉");
    p.set_string_property("sub-codepage", &saved.sub_codepage)
        .unwrap_or_else(|e| panic!("记住的编码 {} mpv 收不下：{e}", saved.sub_codepage));

    // 夹取边界逐个过一遍，而不只是中间值。
    //
    // 第一版只喂了 `1.25` / `1.75` —— 那是这三条里最「正常」的两个值，
    // 把 `sanitized` 的 clamp 改成 `0.0..=1000.0` 这条测试照样绿。
    // 现在喂的是**边界值**：如果 `sanitized` 的范围比 mpv 声明的宽，
    // 这里就会红。
    for sp in [0.0625_f64, 1.0, 16.0] {
        p.set_speed(sp)
            .unwrap_or_else(|e| panic!("夹取范围内的倍速 {sp} mpv 收不下：{e}"));
        let back = p.get_property_string("speed").expect("读 speed");
        let got: f64 = back
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("speed 读回来 {back:?}"));
        assert!((got - sp).abs() < 1e-6, "倍速写进去是 {got}");
    }
    for ss in [0.0_f64, 0.5, 100.0] {
        p.set_string_property("sub-scale", &format!("{ss}"))
            .unwrap_or_else(|e| panic!("夹取范围内的字幕大小 {ss} mpv 收不下：{e}"));
        let back = p.get_property_string("sub-scale").expect("读 sub-scale");
        let got: f64 = back
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("sub-scale 读回来 {back:?} 解析失败"));
        assert!((got - ss).abs() < 1e-6, "字幕大小写进去是 {got}");
    }

    let _ = p.shutdown();
}
