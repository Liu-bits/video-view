//! 显示适配器信息，只为诊断报告服务。
//!
//! ## 为什么需要它
//!
//! D3D11VA 的失败是**按适配器**发生的：同一份代码在 Intel 上走硬解、在
//! 某块 AMD 上静默退回软解，或者反过来。所以「这台机器用的是哪块显卡」
//! 是排查解码问题的第一现场信息，诊断报告里必须有它。
//!
//! 这里用 `EnumDisplayDevicesW` 而不是 DXGI：
//!
//! * 它一次调用就给出用户认得的名字（`Intel(R) UHD Graphics 630`），
//!   而 DXGI 要 `CreateDXGIFactory1` + COM 接口 + 一堆 GUID，多几十行代码
//!   换来的还是同一个字符串。
//! * 它不需要初始化图形栈，也就不受「D3D11 创建失败」这件事影响——
//!   而 D3D11 创建失败恰恰是我们要诊断的情况之一。
//!
//! 局限要说清楚：`EnumDisplayDevicesW` 枚举的是**显示输出设备**，多显卡
//! 机器上它列的是接了显示器的那些适配器，独显在没有输出接口时可能不在
//! 列表里。所以这里同时给出设备名（`\\.\DISPLAY1`）与描述，报告里两者都在，
//! 读者能自己判断信息够不够。

use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ACTIVE};

/// 枚举出来的一块显示设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayDevice {
    /// `\\.\DISPLAY1` 这类设备名
    pub name: String,
    /// 用户认得的适配器名，如 `Intel(R) UHD Graphics 630`
    pub description: String,
    /// `DISPLAY_DEVICE_ACTIVE`：这块设备当前接在桌面上。
    ///
    /// 装了远程投屏 / 虚拟显示器的机器上会有大量 `active == false` 的条目
    /// （实测 13 条里 10 条），它们不影响当前用的是哪块卡。
    pub active: bool,
}

/// 枚举当前连接着的显示设备。
///
/// 枚举失败（headless、远程会话）时返回空 Vec —— 报告里会显示 `-`，
/// 而不是让整份报告生成失败。
pub fn display_devices() -> Vec<DisplayDevice> {
    let mut out = Vec::new();
    for i in 0..MAX_DISPLAYS {
        let mut dd = DISPLAY_DEVICEW {
            // cb 必须先填成结构体大小，否则 API 拒绝（这是 Win32 的老规矩）
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // 第二次调用必须保持 cb 不变
        let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0) };
        if !ok.as_bool() {
            break;
        }
        let description = utf16_field(&dd.DeviceString);
        if description.is_empty() {
            continue;
        }
        out.push(DisplayDevice {
            name: utf16_field(&dd.DeviceName),
            description,
            active: dd.StateFlags.contains(DISPLAY_DEVICE_ACTIVE),
        });
    }
    out
}

/// 一块设备的简短描述，给诊断报告用。
///
/// **只列「当前接在桌面上」的那些**（`DISPLAY_DEVICE_ACTIVE`），其余只给一个
/// 计数。原因是实测数据：开着 GameViewer 这类远程投屏软件时，
/// `EnumDisplayDevicesW` 会返回 **13 个**设备，其中 10 个是
/// `GameViewer Virtual Display Adapter`——它们没有被使用，却和真正在用的
/// 显卡混在同一个列表里。全列出来的话报告里那一行会长到没法看，
/// 而真正要回答的问题（「当前用的是哪块卡」）反而被淹掉。
///
/// 但被略过的数量一定要写出来：`gpu.adapter_others = 10` 这条信息本身就
/// 有价值——那 10 个虚拟显示器正是 D3D11VA 可能被选成默认适配器、
/// 从而**静默**退回软解的来源（见 `mpv/mod.rs` 里 `hwdec-current` 的注释）。
pub fn summary() -> String {
    let devices = display_devices();
    let (active, others): (Vec<&DisplayDevice>, Vec<&DisplayDevice>) =
        devices.iter().partition(|d| d.active);
    if active.is_empty() {
        return "-".to_string();
    }
    let list = active
        .iter()
        .map(|d| {
            if d.name.is_empty() {
                d.description.clone()
            } else {
                format!("{} ({})", d.description, d.name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if others.is_empty() {
        list
    } else {
        format!("{list} (+{} 未接入)", others.len())
    }
}

/// 没被使用的设备数量。诊断报告单独列一行。
pub fn inactive_count() -> usize {
    display_devices().iter().filter(|d| !d.active).count()
}

/// 枚举上限。显示器超过 16 台是闻所未闻，但给个上界免得万一 API 永远返回
/// TRUE 时把内存吃光。
const MAX_DISPLAYS: u32 = 16;

/// `[u16; N]` 形式的 Win32 字符串字段 -> `String`。
///
/// 字段是定长数组、以 NUL 结尾，不能直接 `String::from_utf16`（那会把
/// NUL 之后的填充垃圾也吃进去）。非法 UTF-16 返回空串而不是报错：
/// 驱动返回的字符串本来就只当显示用。
fn utf16_field(field: &[u16]) -> String {
    let end = field.iter().position(|&u| u == 0).unwrap_or(field.len());
    String::from_utf16(&field[..end]).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 定长数组里的_nul_之后的内容不会被当成字符串() {
        // 模拟 Win32 的定长字段：内容 + NUL + 残留垃圾
        let field = [
            b'I' as u16,
            b'n' as u16,
            b't' as u16,
            b'e' as u16,
            b'l' as u16,
            0,
            0xFFFF,
            0x1234,
        ];
        assert_eq!(utf16_field(&field), "Intel");
    }

    #[test]
    fn 没有_nul_时取满整个字段() {
        let field = [b'a' as u16, b'b' as u16, b'c' as u16];
        assert_eq!(utf16_field(&field), "abc");
    }

    #[test]
    fn 非法_utf16_退回空串而不是报错() {
        // 0xD800 是代理项的高位，单独出现是非法的 UTF-16
        let field = [0xD800u16, 0];
        assert_eq!(utf16_field(&field), "");
    }

    #[test]
    fn 空字段得到空串() {
        assert_eq!(utf16_field(&[0u16; 4]), "");
    }

    #[test]
    fn 枚举失败时_summary_也有形状() {
        // 这台机器有没有显示器都不影响调用方：拿不到就是 "-"
        let s = summary();
        assert!(!s.is_empty(), "summary 不能是空串，否则报告里那行会消失");
    }
}
