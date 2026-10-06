//! 用户设置：音量、静音、面板开关、窗口尺寸，落在 HKCU。
//!
//! ## 为什么不放在安装器里
//!
//! 这些是**运行时**偏好，不是安装期偏好：装到 `C:\Program Files\` 的用户
//! 对 `HKLM\Software\VideoView` 没有写权限，而装到用户目录的又不一定有。
//! 所以一律写 `HKCU\Software\VideoView`——两种安装方式下都一定可写。
//!
//! 与 `lang::read_lang` 的区别：那里要读 HKLM 是因为安装器可能写在那儿
//! （提权安装 vs 非提权安装），而这里只有我们自己写，读 HKCU 就够。
//!
//! ## 失败一律不打断使用
//!
//! 读失败返回默认值、写失败返回 `Err`。设置只是偏好，拿不到就让用户用
//! 默认值重新设一次，不能因为注册表被组策略锁了就把程序起不来。
//!
//! ## 存什么、不存什么
//!
//! 存：音量、静音、窗口尺寸 —— 都是**偏好**，用户希望下次还是这样。
//!
//! **不存**：解码诊断面板的开合。它是「暂停下来看一眼」的临时视图，不是
//! 偏好；记住它意味着每次启动画面区都矮一截，而用户按下 `I` 的动机是
//! 「现在想看」，不是「以后每次都想看」。
//!
//! ## 写盘的频率
//!
//! 拖动音量滑块会连着改几十次 `volume`，每 250ms 一次的定时器如果每次都写，
//! 一个拖动就是几十次注册表写。所以这里是「标脏 + 节流」：改动只置一个标志，
//! 由 `App::tick` 按 `FLUSH_INTERVAL` 落一次，退出时再强制落一次。

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};

/// 设置所在的注册表子键（相对 HKCU）。
pub const SUBKEY: &str = r"Software\VideoView";

/// 写入节流间隔（毫秒）。与 `app::TICK_MS` 同量级，但比它长。
pub const FLUSH_INTERVAL_MS: u128 = 2000;

/// 注册表值的字节上限。
///
/// `HKCU` 下的键用户可写，一个超大值会让每次启动分配等量的内存——和
/// `lang.rs` 里那个 64 字节上限是同一类问题，只是这里的值我们自己要写，
/// 所以只要**读**的时候设上限就够。
const MAX_VALUE_BYTES: u32 = 64;

/// 用户设置。
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// 0~100
    pub volume: f64,
    pub muted: bool,
    /// 上次关闭时的客户区尺寸（DIP）
    pub client_w: i32,
    pub client_h: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // 与 `UiState::default()` 的初始音量一致，否则「设置生效」这件事
            // 表现为第一次启动音量突然变了一个值
            volume: 100.0,
            muted: false,
            // 与 app.rs 里的 DEFAULT_CLIENT_W/H_DIP 对齐
            client_w: 1100,
            client_h: 720,
        }
    }
}

impl Settings {
    /// 读设置。任何一步失败都退回默认值，不报错。
    pub fn load() -> Self {
        Self::load_from(SUBKEY)
    }

    /// 写设置。返回 `Err` 只是「这次没存上」，不影响播放。
    pub fn save(&self) -> Result<(), String> {
        self.save_to(SUBKEY)
    }

    // ---- 内部：子键可指定，测试才能用一个临时键而不动真实设置 ----

    fn load_from(subkey: &str) -> Self {
        let mut s = Self::default();

        unsafe {
            if let Some(v) = read_sz(HKEY_CURRENT_USER, subkey, "Volume")
                .as_deref()
                .and_then(parse_volume)
            {
                s.volume = v;
            }
            if let Some(v) = read_dword(HKEY_CURRENT_USER, subkey, "Muted") {
                s.muted = v != 0;
            }
            if let Some(v) = read_dword(HKEY_CURRENT_USER, subkey, "ClientW") {
                s.client_w = v as i32;
            }
            if let Some(v) = read_dword(HKEY_CURRENT_USER, subkey, "ClientH") {
                s.client_h = v as i32;
            }
        }

        s.sanitized()
    }

    fn save_to(&self, subkey: &str) -> Result<(), String> {
        let s = self.sanitized();
        unsafe {
            let mut hkey = HKEY::default();
            let status = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(wide(subkey).as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            );
            if status != ERROR_SUCCESS {
                return Err(format!("RegCreateKeyExW({subkey}) failed: {status:?}"));
            }
            let r = (|| {
                write_dword(hkey, "Muted", u32::from(s.muted))?;
                write_dword(hkey, "ClientW", s.client_w as u32)?;
                write_dword(hkey, "ClientH", s.client_h as u32)?;
                write_sz(hkey, "Volume", &format!("{:.2}", s.volume))
            })();
            let _ = RegCloseKey(hkey);
            r
        }
    }

    /// 把越界 / 离谱的值拉回可用范围。
    ///
    /// 这一步不是防御性编程的洁癖：这些值来自注册表，可能被别的程序改坏，
    /// 也可能来自旧版本的写法。带着 `volume = 1e30` 启动的表现是音量条
    /// 永远是满的、`client_w = -1` 的表现是窗口根本创建不出来。
    fn sanitized(&self) -> Self {
        Self {
            volume: if self.volume.is_finite() {
                self.volume.clamp(0.0, 100.0)
            } else {
                100.0
            },
            muted: self.muted,
            // 下限与 app.rs 的 MIN_CLIENT_*_DIP 对齐；上限给到 8K 全屏宽度，
            // 再大只可能是脏数据
            client_w: self.client_w.clamp(320, 16384),
            client_h: self.client_h.clamp(240, 16384),
        }
    }
}

/// 解析音量字符串。
///
/// 接受带不带空格、正负号、`%` 后缀：`"80"`、`" 80 "`、`"80%"` 都行。
/// 写成 REG_SZ 而不是 REG_DWORD 是因为浮点数没有整数表示，`80.5` 用 DWORD
/// 存要么截断要么得乘 10 再除回来。
fn parse_volume(text: &str) -> Option<f64> {
    let t = text.trim().trim_end_matches('%').trim();
    let v: f64 = t.parse().ok()?;
    v.is_finite().then_some(v)
}

/// UTF-16 + NUL 结尾，供注册表 API 用。
///
/// ## 注意：返回的 `Vec` 必须活得和 `PCWSTR` 一样久
///
/// `PCWSTR(some_vec.as_ptr())` 里如果 `some_vec` 是临时量，那条语句结束
/// 时 `Vec` 就析构了，指针悬空。**直接当参数传是安全的**（临时量活到整个
/// 语句结束），**绑定到变量就不安全**：
///
/// ```ignore
/// // 危险：vec 在这一行结束时就没了
/// let p = PCWSTR(wide(s).as_ptr());
/// // 安全：buf 与 p 活到同一时刻
/// let buf = wide(s);
/// let p = PCWSTR(buf.as_ptr());
/// ```
///
/// 这个坑在 `read_sz` 里真的踩过一次：十次里有三次读不到值，而单跑一次
/// 永远通过。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn open_read(root: HKEY, subkey: &str) -> Option<HKEY> {
    let mut hkey = HKEY::default();
    let status = RegOpenKeyExW(
        root,
        PCWSTR(wide(subkey).as_ptr()),
        None,
        KEY_READ,
        &mut hkey,
    );
    (status == ERROR_SUCCESS).then_some(hkey)
}

/// 读一个 REG_SZ。超长 / 非字符串 / 不存在都返回 `None`。
unsafe fn read_sz(root: HKEY, subkey: &str, name: &str) -> Option<String> {
    let hkey = open_read(root, subkey)?;
    // name_buf 必须活到下面两次 RegQueryValueExW 调用之外。
    //
    // 写成 let value_name = PCWSTR(wide(name).as_ptr()); 的话，wide()
    // 返回的 Vec 是临时量，在这行结束时就析构了，而 value_name 里存的是
    // 它的指针 —— 悬空。之后两次 API 调用拿到的是**已被复用的内存**。
    //
    // 这个 bug 极其阴险：这种写法几乎总是「能用」（分配器不会擦除刚释放的
    // 块），所以单跑一次永远通过；而 81 个测试并行、分配模式一变，那块内存
    // 就被别的分配覆盖，属性名变成乱码、读不到值、退回默认值。实测就是
    // 「注册表往返」那条测试十次里失败三次。
    let name_buf = wide(name);
    let value_name = PCWSTR(name_buf.as_ptr());

    // 第一遍只问大小。`lpcbData` 既是输入也是输出：传 NULL 表示
    // 「只告诉我这个值多大」，不往 lpData 里写任何东西。
    let mut kind = REG_SZ;
    let mut needed = 0_u32;
    let status = RegQueryValueExW(
        hkey,
        value_name,
        None,
        Some(&mut kind),
        None,
        Some(&mut needed),
    );
    if status != ERROR_SUCCESS || needed > MAX_VALUE_BYTES {
        let _ = RegCloseKey(hkey);
        return None;
    }
    // 类型必须正好是 REG_SZ。别的（含 REG_BINARY / REG_DWORD）一律不读，
    // 免得把一段二进制当成字符串去 UTF-16 转换。
    if kind != REG_SZ {
        let _ = RegCloseKey(hkey);
        return None;
    }

    // REG_SZ 以 NUL 结尾，长度是偶数字节数。多留一个单元给 NUL。
    let mut buf = vec![0_u16; (needed as usize / 2) + 1];
    let mut got = (buf.len() * 2) as u32;
    let status = RegQueryValueExW(
        hkey,
        value_name,
        None,
        Some(&mut kind),
        Some(buf.as_mut_ptr() as *mut u8),
        Some(&mut got),
    );
    let _ = RegCloseKey(hkey);
    if status != ERROR_SUCCESS {
        return None;
    }
    // 再核一次：两次调用之间别的进程可能改了这个值
    if got > MAX_VALUE_BYTES {
        return None;
    }
    // 丢掉 NUL 终止符
    let units = (got as usize / 2).saturating_sub(1);
    buf.truncate(units);
    String::from_utf16(&buf).ok()
}

/// 读一个 REG_DWORD。
unsafe fn read_dword(root: HKEY, subkey: &str, name: &str) -> Option<u32> {
    let hkey = open_read(root, subkey)?;
    let mut kind = REG_DWORD;
    let mut out: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;
    let status = RegQueryValueExW(
        hkey,
        PCWSTR(wide(name).as_ptr()),
        None,
        Some(&mut kind),
        Some(&mut out as *mut u32 as *mut u8),
        Some(&mut size),
    );
    let _ = RegCloseKey(hkey);
    (status == ERROR_SUCCESS && kind == REG_DWORD).then_some(out)
}

unsafe fn write_dword(hkey: HKEY, name: &str, value: u32) -> Result<(), String> {
    let bytes = value.to_ne_bytes();
    let status = RegSetValueExW(
        hkey,
        PCWSTR(wide(name).as_ptr()),
        None,
        REG_DWORD,
        Some(&bytes),
    );
    (status == ERROR_SUCCESS)
        .then_some(())
        .ok_or(format!("{name}: {status:?}"))
}

unsafe fn write_sz(hkey: HKEY, name: &str, text: &str) -> Result<(), String> {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    buf.push(0);
    let status = RegSetValueExW(
        hkey,
        PCWSTR(wide(name).as_ptr()),
        None,
        REG_SZ,
        Some(std::slice::from_raw_parts(
            buf.as_ptr() as *const u8,
            buf.len() * 2,
        )),
    );
    (status == ERROR_SUCCESS)
        .then_some(())
        .ok_or(format!("{name}: {status:?}"))
}

/// 测试专用的子键。
///
/// **每个测试必须用自己那一个。** Rust 默认并行跑测试，共用一个子键时
/// 一个测试的清理会把另一个刚写进去的数据擦掉——而且这个失败只在全量跑
/// 时出现、单独跑单个测试永远通过，是最难查的那一类测试 bug（第一版就是
/// 这么写的，`cargo test settings` 单独跑全过、`cargo test` 全量跑挂）。
///
/// 用 `Software\VideoView\Test\<name>` 而不是真实子键：测试不该动用户数据。
#[cfg(test)]
fn test_subkey(name: &str) -> String {
    format!("{SUBKEY}\\Test\\{name}")
}

/// 删掉一个测试子键。
#[cfg(test)]
fn delete_test_key(subkey: &str) {
    use windows::Win32::System::Registry::RegDeleteKeyW;
    unsafe {
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(wide(subkey).as_ptr()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 音量字符串的解析容得下人手写出来的几种形状() {
        assert_eq!(parse_volume("80"), Some(80.0));
        assert_eq!(parse_volume(" 80.5 "), Some(80.5));
        assert_eq!(parse_volume("80%"), Some(80.0));
        assert_eq!(parse_volume("0"), Some(0.0));
        // 解析不出来就返回 None，让上层用默认值，绝不猜
        assert_eq!(parse_volume(""), None);
        assert_eq!(parse_volume("loud"), None);
        assert_eq!(parse_volume("NaN"), None);
        assert_eq!(parse_volume("inf"), None);
    }

    #[test]
    fn 越界的值会被拉回可用范围() {
        let s = Settings {
            volume: 1e30,
            muted: false,
            client_w: -5,
            client_h: 0,
        };
        let s = s.sanitized();
        assert_eq!(s.volume, 100.0);
        assert!(s.client_w >= 320, "{}", s.client_w);
        assert!(s.client_h >= 240, "{}", s.client_h);
    }

    #[test]
    fn 音量为_nan_时退回默认值而不是零() {
        let s = Settings {
            volume: f64::NAN,
            ..Settings::default()
        };
        assert_eq!(s.sanitized().volume, 100.0);
    }

    #[test]
    fn 注册表往返能拿回原值() {
        let key = test_subkey("roundtrip");
        delete_test_key(&key);
        let want = Settings {
            volume: 62.5,
            muted: true,
            client_w: 1440,
            client_h: 900,
        };
        want.save_to(&key).expect("应当能写进 HKCU 的测试子键");

        let got = Settings::load_from(&key);
        assert_eq!(got.volume, want.volume, "音量必须是精确往返");
        assert_eq!(got.muted, want.muted);
        assert_eq!(got.client_w, want.client_w);
        assert_eq!(got.client_h, want.client_h);

        delete_test_key(&key);
    }

    #[test]
    fn 键不存在时返回默认值而不是报错() {
        let key = test_subkey("missing");
        delete_test_key(&key);
        assert_eq!(Settings::load_from(&key), Settings::default());
    }

    #[test]
    fn 值是别的类型时当没写而不是乱解读() {
        // HKCU 下的键用户可写。把 Volume 写成 REG_DWORD 之后，读回来必须是
        // 默认值——而不是把 4 个字节当成 UTF-16 去转换出一个乱码音量。
        let key = test_subkey("wrongtype");
        delete_test_key(&key);
        unsafe {
            let mut hkey = HKEY::default();
            let status = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(wide(&key).as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            );
            assert_eq!(status, ERROR_SUCCESS, "建测试键失败：{status:?}");
            write_dword(hkey, "Volume", 0xDEAD_BEEF).unwrap();
            write_dword(hkey, "Muted", 7).unwrap();
            let _ = RegCloseKey(hkey);
        }
        let got = Settings::load_from(&key);
        assert_eq!(
            got.volume,
            Settings::default().volume,
            "Volume 类型不对就该用默认值，而不是解读出乱码"
        );
        // Muted 是 REG_DWORD，类型对，仍然按非 0 读成 true
        assert!(got.muted, "Muted 是 REG_DWORD，应当读成 true");
        delete_test_key(&key);
    }

    #[test]
    fn 默认值和初始界面状态对得上() {
        // 音量默认值与 UiState::default() 一致，否则第一次启动音量会跳一下
        assert_eq!(Settings::default().volume, 100.0);
        assert!(!Settings::default().muted);
    }
}
