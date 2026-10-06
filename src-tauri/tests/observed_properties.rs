//! 观测属性名的回归测试。
//!
//! ## 为什么这条测试必须存在
//!
//! `OBSERVED_PROPERTIES` 里的每个名字都会传给 `mpv_observe_property`，而
//! **名字写错的后果是整个程序起不来**：`configure` 里 `check()` 拿到错误码
//! 就返回 `Err`，`MpvPlayer::new` 失败，`run()` 弹一个「无法启动」的框。
//! 报错信息里只有属性名，排查时很容易先怀疑 D3D11、怀疑 DLL 版本，
//! 想不到是一行属性名拼错了。
//!
//! 0.4.0 加诊断项时就真的踩了这个坑：`vo-drop-frame-count`（mpv 旧文档里的
//! 名字）在这份 libmpv 上**根本不存在**，实测正确名字是 `frame-drop-count`。
//! 同时 `mistimed-frame-count`、`estimated-display-fps`、`video-bitrate`
//! 都不可读。这些都是靠临时探针逐个试出来的，不是查文档得来的。
//!
//! 所以这里钉两件事：
//!
//! 1. **名字能注册** —— `MpvPlayer::new` 内部会 observe 全部名字，能建出来
//!    就说明每个名字都是真属性。这条不需要 GPU。
//! 2. **格式对得上** —— 名字存在不代表声明的 `MPV_FORMAT_*` 对。按声明的
//!    格式去读，读到的错误只能是「属性当前不可用」（比如还没开始解码），
//    不能是格式/类型不匹配。

use std::path::Path;
use std::time::{Duration, Instant};

use video_view_lib::mpv::ffi::{
    MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG, MPV_FORMAT_INT64, MPV_FORMAT_STRING,
};
use video_view_lib::mpv::{InitOptions, MpvPlayer, OBSERVED_PROPERTIES};

fn headless() -> MpvPlayer {
    MpvPlayer::new(&InitOptions {
        wid: 0,
        headless: true,
    })
    .expect("headless player 应当能创建：OBSERVED_PROPERTIES 里有名字在 mpv 上不存在")
}

#[test]
fn 观测属性都能注册() {
    // `new` 内部对每个名字调 `mpv_observe_property` 并检查返回值。
    // 任何一个名字无效都会让这里 panic，错误信息里带着那个名字。
    let player = headless();
    let _ = player.shutdown();
    assert!(!OBSERVED_PROPERTIES.is_empty());
}

#[test]
fn 观测属性里有我们关心的那几项() {
    let names: Vec<&str> = OBSERVED_PROPERTIES.iter().map(|(n, _)| *n).collect();

    // 这几项各自有一个「静默出错就查不出来」的理由，少任何一项都要改测试
    // 并在注释里写清楚为什么。
    for required in [
        // D3D11VA 静默退回软解时唯一的信号
        "hwdec-current",
        // 解码帧是否走了零拷贝（软解 yuv420p / 硬解 d3d11）
        "video-params/pixelformat",
        // 两个丢帧计数。注意是 frame-drop-count，不是 vo-drop-frame-count
        "decoder-frame-drop-count",
        "frame-drop-count",
        // 源分辨率与输出分辨率（两者不同说明被缩放过）
        "video-params/w",
        "video-out-params/w",
        // 编码名
        "video-format",
    ] {
        assert!(
            names.contains(&required),
            "观测属性里少了 {required:?}，当前有：{names:?}"
        );
    }
}

#[test]
fn 观测属性的格式与声明一致() {
    let player = headless();
    let media = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/loop60s.mp4");
    if player.load_file(&media).is_err() {
        eprintln!("跳过：测试素材加载失败");
        let _ = player.shutdown();
        return;
    }
    // 等解码器真正建立起来，否则大半属性是「unavailable」，
    // 那样这条测试就什么都验不到。
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && player.get_property_string("video-format").is_err() {
        std::thread::sleep(Duration::from_millis(100));
    }

    let mut problems: Vec<String> = Vec::new();
    for (name, format) in OBSERVED_PROPERTIES {
        // 轮询几次：属性会在解码器建立的那一瞬间才可用，头几轮可能还是
        // unavailable，那不算问题。
        let mut last_err = String::new();
        let mut ok = false;
        for _ in 0..20 {
            match read(&player, name, *format) {
                Ok(_) => {
                    ok = true;
                    break;
                }
                Err(e) => {
                    last_err = e;
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        if !ok {
            // 「unavailable」= 属性当前没有值（正常，比如没在播）
            // 别的错误 = 名字不对或格式不对，那是要修的
            if !last_err.contains("unavailable") && !last_err.contains("not found") {
                problems.push(format!("{name}（格式 {format}）: {last_err}"));
            }
        }
    }
    let _ = player.shutdown();
    assert!(
        problems.is_empty(),
        "这些观测属性按声明的格式读不出来：\n  {}",
        problems.join("\n  ")
    );
}

/// 按声明的格式读一个属性。返回错误文本（用来区分 unavailable 与格式不匹配）。
fn read(player: &MpvPlayer, name: &str, format: i32) -> Result<String, String> {
    match format {
        MPV_FORMAT_STRING => player.get_property_string(name),
        MPV_FORMAT_DOUBLE => player.get_double_property(name).map(|v| format!("{v}")),
        MPV_FORMAT_FLAG => player.get_flag_property(name).map(|v| format!("{v}")),
        MPV_FORMAT_INT64 => player.get_int64_property(name).map(|v| format!("{v}")),
        other => Err(format!("未处理的格式 {other}")),
    }
}
