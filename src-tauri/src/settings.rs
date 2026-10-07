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
    /// **高级模式**：开着才给 0.3.0 起的那些高级项（解码诊断、轨道菜单、
    /// 播放列表、字幕编码/大小、复制诊断报告），关着它们在右键菜单里
    /// **置灰**。
    ///
    /// 默认 `true`。理由见 `App::toggle_advanced` 的注释 ——
    /// 默认关掉等于「装好之后按 `I` 没反应」，那不是简化，是功能坏了。
    pub advanced: bool,
    /// 上次的播放倍速（mpv 的 `speed`）。
    ///
    /// 记住它的理由跟音量一样：**用户调过就是想要它**。倍速是那种
    /// 「看番要 1.5 倍、看教程要 2 倍」的场景，每次启动都重置成 1.0
    /// 的话每部片子都要重新按 `[` 好几下。
    pub speed: f64,
    /// 上次的字幕大小（mpv 的 `sub-scale`，0~100）。
    pub sub_scale: f64,
    /// 上次的字幕编码（mpv 的 `sub-codepage`）。空串 = 用 mpv 默认值。
    pub sub_codepage: String,
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
            advanced: true,
            speed: 1.0,
            sub_scale: 1.0,
            sub_codepage: String::new(),
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
            // 注册表里没有这一项时**默认开着**。老版本升级上来的人
            // 不该因为多了个开关就发现 `I` 没反应了 —— 那个键是这一版
            // 才写的，读不到就等于「没关过」。
            if let Some(v) = read_dword(HKEY_CURRENT_USER, subkey, "Advanced") {
                s.advanced = v != 0;
            }
            // 倍速 / 字幕大小 / 字幕编码：读不到就用默认值。
            // 三个都用**字符串**存而不是 `DWORD`，因为倍速与字幕大小是
            // 小数，DWORD 存会丢精度（1.25 -> 1）。注册表其实有
            // `REG_QWORD`，但它是整数，仍然存不了小数。
            if let Some(v) = read_sz(HKEY_CURRENT_USER, subkey, "Speed").as_deref() {
                if let Some(x) = parse_finite(v) {
                    s.speed = x;
                }
            }
            if let Some(v) = read_sz(HKEY_CURRENT_USER, subkey, "SubScale").as_deref() {
                if let Some(x) = parse_finite(v) {
                    s.sub_scale = x;
                }
            }
            // 编码**原样存**：合法值由 mpv 那边兜底校验，而这里
            // 自造一份白名单只会带来「升级 mpv 加了新编码但我们不认」
            // 这种问题。存空串 = 用户没改过 = 用默认值。
            if let Some(v) = read_sz(HKEY_CURRENT_USER, subkey, "SubCodepage") {
                s.sub_codepage = v;
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
                write_dword(hkey, "Advanced", u32::from(s.advanced))?;
                write_sz(hkey, "Volume", &format!("{:.2}", s.volume))?;
                // 倍速与字幕大小存**三位小数**。为什么不存更多：唯一会用到这份精度的
                // 地方是「记住上次的倍速」，而 `SPEED_FACTOR` 是 1.25，
                // 连按几次得到的是 1.25 / 1.5625 / 1.953125 ——
                // 截成三位是 1.562，与真值的差是 4e-4，对播放速度来说
                // 远低于可感知的程度，而且这个误差**不累积**：下一次按 `]`
                // 是拿 1.562 去乘的，不是拿 1.5625 乘。
                // 为什么不存更少：`.2` 会把 1.5625 变成 1.56，而「上次是
                // 1.5625」这个信息在右键菜单那一项（`重置速度` 的可用性）
                // 上还有用。
                //
                // `sanitized` 已经把它们夹进合法范围，所以这里写出来的
                // 一定是有限的，`{:.3}` 不会出 `NaN`。
                write_sz(hkey, "Speed", &format!("{:.3}", s.speed))?;
                write_sz(hkey, "SubScale", &format!("{:.3}", s.sub_scale))?;
                // 空串**也要写**：mpv 的默认值是「不指定编码」，
                // 而 Windows 会跳过「空」不是「空串」，所以写 `"(default)"`
                // 之类会变成一个用户没选过的编码。空串就是空串，
                // 读回来时 `read_sz` 给 `Some("")`，`sanitized` 再归一。
                write_sz(hkey, "SubCodepage", &s.sub_codepage)
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
    /// 把越界 / 脏值夹回合法范围。
    ///
    /// 独立成一个 `pub`（而不是只留私有方法）是为了让**集成测试**
    /// （`tests/sub_settings.rs`）复用同一套归一逻辑 —— 那条测试要验的
    /// 恰恰是「`sanitized` 夹过的范围整个落在 mpv 声明的范围内」，如果测试
    /// 自己再写一遍夹取，两个定义迟早不一样，而那种不一致的现场是
    /// 「界面显示 1.0 倍、实际 3 倍」，极难查。
    pub fn sanitized(&self) -> Self {
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
            // bool 没有越界可言，过来就行
            advanced: self.advanced,
            // mpv 的 `speed` 合法范围是 0.01~100（0 会被当成「暂停」，负数会让
            // 播放倒着跑或者直接报错）。上界 16 与 `scale_speed` 一致。
            //
            // **下界刻意取 0.0625 而不是「更好看的」0.25**：这个值必须与
            // `App::scale_speed` 和 `mpv::set_speed` 的下界**完全一致**。
            // 三处不一致的后果很具体 —— 用户连按 `[` 到 0.0625 倍，
            // 落盘的是 0.0625，下次启动被这里拉回 0.25，**播放速度凭空
            // 变了 4 倍**。早期版本这里写的是 0.25 而另两处是 0.0625，
            // 就是这个 bug。
            //
            // 0.0625 倍本身没什么实用价值，但「记住用户设过的东西」比
            // 「把它拉回一个我认为更合理的值」重要：拉回来等于程序擅自
            // 改了用户的设置，而且没有任何提示。
            speed: if self.speed.is_finite() {
                self.speed.clamp(0.0625, 16.0)
            } else {
                1.0
            },
            // `sub-scale` 实测范围是 0~100（`option-info` 查的，
            // 手册上写的 0.1~10 不准）。0 是合法的（等于不显示字幕），
            // 所以下界不抬。
            sub_scale: if self.sub_scale.is_finite() {
                self.sub_scale.clamp(0.0, 100.0)
            } else {
                1.0
            },
            // 编码**只做长度限制**，不校验内容。
            //
            // 校验就得维护一份白名单，而 mpv 的编码列表会随版本变长 ——
            // 白名单落后于 mpv 的后果是「用户在新版 mpv 里能选，我们却
            // 把它丢了」。交给 mpv 判非法值：它会忽略并保持原编码。
            //
            // 限制长度是为了不让一个手滑写进去的超长字符串把
            // `MAX_VALUE_BYTES` 的配额吃满——读比写更容易失败，
            // 而「写得进读不出」的组合会让这个值永远卡在坏状态。
            //
            // **比较的是 UTF-16 字节数，不是 `str::len()`。**
            // `MAX_VALUE_BYTES` 是给 `read_sz` 用的，而注册表里的
            // `REG_SZ` 是 UTF-16 —— `write_sz` 写出去的是 `c.len() * 2 + 2`
            // 字节（多的那 2 是结尾的 NUL）。拿 `str::len()` 去比 64 的话，
            // 一个 40 字符的编码名（`len() == 40 < 64` 通过）写出去是
            // 82 字节，下次 `read_sz` 里 `needed > MAX_VALUE_BYTES` 直接
            // 读不出来 —— 静默退回默认值。也就是这段注释要防的那个 case
            // 它自己没防住。
            sub_codepage: {
                let c = self.sub_codepage.trim();
                // +2 是结尾的 NUL；`<=` 而不是 `<` 是因为
                // `read_sz` 在 `needed == MAX_VALUE_BYTES` 时仍然读得到
                let bytes = c.len() * 2 + 2;
                if bytes <= MAX_VALUE_BYTES as usize {
                    c.to_string()
                } else {
                    String::new()
                }
            },
        }
    }
}

/// 解析一个有限的小数。空串 / `NaN` / `inf` / 任何非数字一律 `None`
/// ——调用方那时的动作是「用默认值」，不是「报错」。
///
/// 与 `parse_volume` 分开是因为它不做「去掉尾部 `%`」那一步：
/// 那是音量专用的（有人会往注册表里手打 `80%`），而倍速与字幕大小
/// 带了 `%` 就是错的输入，不该被默默接受。
fn parse_finite(text: &str) -> Option<f64> {
    let v: f64 = text.trim().parse().ok()?;
    v.is_finite().then_some(v)
}

/// 解析音量。**容忍尾部 `%`**（`"80%"` / `" 80 "` / `"80"` 都行），
/// 因为音量是注册表里唯一一个「用户可能手打」的值，而 DWORD 存不下
/// `80.5`。
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

    /// `parse_finite` 之前一个直接单测都没有，而它是 `sanitized` 里
    /// `is_finite` 分支的**唯一**触发来源：`"1e999"` 会被 Rust 的
    /// `f64::parse` 接受成 `inf`，`"NaN"` / `"inf"` 同理 —— 那三个如果不
    /// 挡住，就会一路进到 `{:.3}` 格式化里变成 `inf`，写进注册表之后
    /// 下次启动再读还是 `inf`，倍速标签直接画出 `infx`。
    #[test]
    fn parse_finite_只收真正的有限小数() {
        assert_eq!(parse_finite("1.25"), Some(1.25));
        assert_eq!(parse_finite("  0.5  "), Some(0.5));
        assert_eq!(parse_finite("16"), Some(16.0));
        assert_eq!(parse_finite("1e2"), Some(100.0));
        // 下面这些才是重点
        assert_eq!(parse_finite(""), None);
        assert_eq!(parse_finite("   "), None);
        assert_eq!(parse_finite("abc"), None);
        assert_eq!(parse_finite("NaN"), None);
        assert_eq!(parse_finite("nan"), None);
        assert_eq!(parse_finite("inf"), None);
        assert_eq!(parse_finite("Infinity"), None);
        // Rust 的 f64::parse 接受这个并返回 inf
        assert_eq!(parse_finite("1e999"), None);
        assert_eq!(parse_finite("-1e999"), None);
        // **不带** `%` 后缀：`80%` 对音量是常见的手误，对倍速/字幕大小
        // 则是错输入，不该被默默接受（那是 `parse_volume` 才管的）
        assert_eq!(parse_finite("80%"), None);
    }

    #[test]
    fn 越界的值会被拉回可用范围() {
        let s = Settings {
            volume: 1e30,
            muted: false,
            client_w: -5,
            client_h: 0,
            advanced: true,
            speed: 1.0,
            sub_scale: 1.0,
            sub_codepage: String::new(),
        };
        let s = s.sanitized();
        assert_eq!(s.volume, 100.0);
        assert!(s.client_w >= 320, "{}", s.client_w);
        assert!(s.client_h >= 240, "{}", s.client_h);
        // 越界的媒体设置也要被夹住 —— 之前这条测试只管音量与窗口大小，
        // 0.7.0 加进来的三个字段没人管，于是它们的第一版夹取范围写错了
        // （下界 0.25 vs 另两处的 0.0625）也没人发现
        let wild = Settings {
            speed: 1e30,
            sub_scale: -5.0,
            sub_codepage: "x".repeat(200),
            ..Settings::default()
        }
        .sanitized();
        assert!(wild.speed <= 16.0, "倍速上限没夹住：{}", wild.speed);
        assert!(
            wild.sub_scale >= 0.0,
            "字幕大小下限没夹住：{}",
            wild.sub_scale
        );
        assert_eq!(
            wild.sub_codepage, "",
            "超长的编码名应当归一成空串而不是写得进读不出"
        );
    }

    #[test]
    fn 音量为_nan_时退回默认值而不是零() {
        let s = Settings {
            volume: f64::NAN,
            ..Settings::default()
        };
        assert_eq!(s.sanitized().volume, 100.0);
    }

    /// 高级模式**默认开**。
    ///
    /// 这一条是产品决定，不是实现细节：默认关掉等于「装好之后按 `I` /
    /// `T` 没反应」，而没有任何界面元素告诉用户「你得先去菜单里开一个开关」。
    /// 第一次启动就把功能藏起来，收到的是「功能坏了」的报告。
    #[test]
    fn 高级模式默认是开的() {
        assert!(Settings::default().advanced);
        // 注册表里没有这个键时也按开着处理 —— 从旧版本升级上来的人
        // 不该因为多了个开关就发现功能不见了
        let key = test_subkey("advanced-default");
        delete_test_key(&key);
        assert!(Settings::load_from(&key).advanced);
        delete_test_key(&key);
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
            advanced: false,
            speed: 1.25,
            sub_scale: 1.75,
            sub_codepage: "gbk".to_string(),
        };
        want.save_to(&key).expect("应当能写进 HKCU 的测试子键");

        let got = Settings::load_from(&key);
        assert_eq!(got.volume, want.volume, "音量必须是精确往返");
        assert_eq!(got.muted, want.muted);
        assert_eq!(got.client_w, want.client_w);
        assert_eq!(got.client_h, want.client_h);
        // 高级模式也要往返。这一项**默认开**，所以测试里特意存 `false`：
        // 存 `true` 的话，读不到时的默认值也是 `true`，测不出「真的存进去了」
        assert_eq!(got.advanced, want.advanced, "高级模式没存住");
        // 倍速与字幕大小存的是**字符串**，所以这里要验的是「小数没被截断」。
        // 用 1.25 / 1.75 而不是 1.0 / 2.0 是有意的：后者用 DWORD 也能存，
        // 换成 DWORD 的实现一样能过这个测试 —— 而那正是需要防的那种退化。
        assert_eq!(
            got.speed, want.speed,
            "倍速必须是精确往返（1.25 不能变成 1.00）"
        );
        assert_eq!(got.sub_scale, want.sub_scale, "字幕大小必须是精确往返");
        assert_eq!(got.sub_codepage, want.sub_codepage, "字幕编码没存住");

        delete_test_key(&key);
    }

    #[test]
    fn 记着的媒体设置被手改成非法值时归一() {
        let key = test_subkey("sanitize-media");
        delete_test_key(&key);
        let raw = Settings {
            // 0 倍速在 mpv 里是「暂停」，负倍速会让播放倒着跑
            speed: -3.0,
            sub_scale: 9999.0,
            sub_codepage: "  ".to_string(),
            ..Settings::default()
        };
        raw.save_to(&key).expect("应当能写进 HKCU 的测试子键");
        let got = Settings::load_from(&key);
        // 默认值与 `App::scale_speed` / `mpv::set_speed` 的下界必须一致
        assert!(
            got.speed >= 0.0625,
            "倍速不该被归一成低于下限的值：{}",
            got.speed
        );
        assert!(
            got.speed <= 16.0,
            "倍速不该被归一成高于上限的值：{}",
            got.speed
        );
        assert!(
            (0.0..=100.0).contains(&got.sub_scale),
            "字幕大小应当落在 0~100，实际 {}",
            got.sub_scale
        );
        assert_eq!(got.sub_codepage, "", "纯空白的编码应当归一成空串");

        delete_test_key(&key);
    }

    #[test]
    fn 字幕编码的值可能带空白_读的时候要_trim_掉() {
        // `save_to` 与 `load_from` 都不是 trim 的，只 `sanitized` trim。
        // 目的是「用户从别的软件拷了一串带空格的编码名过来」能被接受，
        // 而不是变成一条 mpv 认不出来的属性值。
        let key = test_subkey("codepage-space");
        delete_test_key(&key);
        let raw = Settings {
            sub_codepage: "  big5  ".to_string(),
            ..Settings::default()
        };
        raw.save_to(&key).expect("应当能写进 HKCU 的测试子键");
        assert_eq!(Settings::load_from(&key).sub_codepage, "big5");

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
