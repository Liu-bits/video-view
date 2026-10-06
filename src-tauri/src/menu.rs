//! 画面右键菜单。
//!
//! ## 为什么用系统原生菜单，而不是自己画一块
//!
//! 自己画要处理：无障碍（键盘导航 / 读屏）、DPI 缩放、右键抬起后的焦点、
//! 菜单在屏幕边缘的翻转、子菜单的悬停延迟、禁用态的灰度、高对比度模式下的
//! 配色… `TrackPopupMenuEx` 这些全是系统做好的，而且是用户熟悉的样子。
//!
//! 本项目的界面是 GDI 手绘的（控制栏、面板），但**菜单不是界面** —— 它是
//! 一次性的系统交互层，用系统组件没有代价，还白拿一套完整的键盘操作。
//!
//! ## 高级功能为什么放这里，而不是「只有」这里
//!
//! 快捷键**一个都没删**。它们是零成本的（`handle_key` 里本来就有），
//! 而菜单是「不知道有快捷键」的用户唯一的发现途径。菜单是快捷键的**第二个
//! 入口**，不是替代品 —— 两个都在，才既有肌肉记忆又有可发现性。
//!
//! ## 命令 ID 怎么编号
//!
//! 手写常量，不用资源 ID：菜单是运行时拼出来的，没有资源表可查，
//! 自己编号最容易读，也最容易看出哪段空着。
//! `1..=999` 给固定项，`1000` 起给动态生成的轨道项 —— 固定项必须全小于
//! `TRACK_BASE`，`固定命令id互不重复` 那条测试盯着这条不变量。
//!
//! ## 坐标
//!
//! 传入的是**屏幕**坐标，直接喂给 `TrackPopupMenuEx`。
//!
//! 曾经写成「收客户区坐标、内部 `ClientToScreen` 转一次」—— 那是个陷阱：
//! 唯一的调用方是 `GetMessagePos`，它给的**本来就是屏幕坐标**，于是坐标
//! 被转了两次。实测症状是菜单整体偏移一个非客户区的大小（窗口在屏幕上
//! 偏下时最明显：鼠标在 (69,265)、菜单出现在 (138,343)）。
//!
//! 现在由调用方保证传屏幕坐标，`show` 不做任何转换。多一次转换不多花什么，
//! 但错一次就是「菜单弹在奇怪的地方」而功能全对 —— 这种 bug 从代码上
//! 看不出来，只能靠截图比对位置抓到。

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, EnableMenuItem, SetForegroundWindow,
    TrackPopupMenuEx, HMENU, MF_CHECKED, MF_ENABLED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING,
    MF_UNCHECKED, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
};

use crate::lang;
use crate::track::{describe, TrackSet};

/// 菜单项被选中时返回的命令 ID。
pub type Command = u16;

/// 动态轨道项的起始 ID。见模块说明里关于编号的那段。
pub const TRACK_BASE: u16 = 1000;

/// 「没有轨道」时的占位项 ID。
///
/// 用 `0xffff` 而不是 0：0 在 Win32 菜单里有特殊含义（`AppendMenuW` 的
/// `MF_STRING` + `uidNewItem = 0` 会被当成「按位置插入」），而且它和
/// `TRACK_BASE` 起的动态段不冲突。
const NO_TRACK_ID: u16 = 0xffff;

/// 固定命令项的 ID。
pub mod id {
    pub const OPEN: u16 = 1;
    pub const PLAY_PAUSE: u16 = 2;
    pub const STOP: u16 = 3;

    // 音轨 / 字幕轨的**父菜单项**没有 ID：`MF_POPUP` 时 `uidNewItem` 必须是
    // 子菜单句柄，点它只是展开、不返回命令。子菜单里的项才有 ID。
    pub const SUB_OFF: u16 = 21;

    pub const SCREENSHOT: u16 = 30;
    pub const STEP_BACK: u16 = 31;
    pub const STEP_FWD: u16 = 32;
    pub const SLOWER: u16 = 33;
    pub const SPEED_RESET: u16 = 34;
    pub const FASTER: u16 = 35;

    pub const JUMP_START: u16 = 36;
    pub const BACK_10S: u16 = 37;
    pub const FWD_10S: u16 = 38;
    pub const JUMP_END: u16 = 39;

    pub const THEATRE: u16 = 50;
    pub const FULLSCREEN: u16 = 51;
    pub const DIAG: u16 = 52;
    pub const HELP: u16 = 53;
    pub const COPY_REPORT: u16 = 54;
}

/// 构造菜单需要的应用状态。
///
/// 持**借用**而不是克隆：`TrackSet` 里有 `String`，克隆一份是几十次堆分配，
/// 而菜单是一弹就关的东西。
///
/// `'a` 的存在是为了让借用检查器看清两阶段的边界 —— `build` 期间只能拿到
/// 不可变借用，命令**执行**要在 `show` 返回、借用结束之后：
///
/// ```ignore
/// let cmd = {
///     let snap = menu::Snapshot { tracks: &self.tracks, ..snap };
///     menu::show(self.hwnd, x, y, &self.strings, snap)   // 不可变借用
/// };                                                       // 借用在这里结束
/// if let Some(cmd) = cmd {
///     self.run_menu_command(cmd)                          // 现在可以 &mut
/// }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Snapshot<'a> {
    /// 有没有正在播的媒体。菜单里大部分命令都依赖它。
    pub loaded: bool,
    pub paused: bool,
    pub speed: f64,
    pub theatre: bool,
    pub fullscreen: bool,
    /// 诊断面板开着吗 —— 用它给菜单项打勾
    pub diag_open: bool,
    pub tracks: &'a TrackSet,
}

/// 弹出菜单并等用户选完。返回选中的命令（按 Esc / 点外面关掉则 `None`）。
///
/// 坐标是**屏幕**坐标 —— 见模块说明里「坐标」那一段。
pub fn show(
    owner: HWND,
    screen_x: i32,
    screen_y: i32,
    s: &lang::Strings,
    snap: Snapshot<'_>,
) -> Option<Command> {
    let menu = build(s, snap)?;

    // SAFETY: 句柄来自 `CreatePopupMenu`，下面的 early return 都过
    // `destroy` 释放；`TrackPopupMenuEx` 期间菜单由系统接管，调用期间
    // `menu` 仍在栈上有效（不 move、不 drop）。
    unsafe {
        // 菜单弹出时窗口会失去激活状态。**不**先 `SetForegroundWindow`
        // 的话，用户点完菜单窗口不会拿回焦点 —— 下一次键盘快捷键没反应，
        // 要点标题栏才恢复。这是 Win32 弹出菜单的经典坑。
        let _ = SetForegroundWindow(owner);

        // TPM_RETURNCMD：直接返回命令 ID，而不是发 WM_COMMAND。
        // 走 WM_COMMAND 的话命令会在我们的消息循环里被派发，与本函数
        // 自己的调用栈交错 —— 一次调用一次结果清楚得多。
        //
        // TPM_RIGHTBUTTON：子菜单也支持右键点开（只用左键的话要悬停等
        // 一会儿才展开）。
        let cmd = TrackPopupMenuEx(
            menu,
            (TPM_LEFTALIGN | TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
            screen_x,
            screen_y,
            owner,
            None,
        );
        let _ = DestroyMenu(menu);
        // TPM_RETURNCMD 时返回值就是命令 ID；用户取消时是 0。
        if cmd.0 == 0 {
            None
        } else {
            Some(cmd.0 as u16)
        }
    }
}

/// 拼出菜单。`None` = 菜单建不出来（`CreatePopupMenu` 失败）。
fn build(s: &lang::Strings, snap: Snapshot<'_>) -> Option<HMENU> {
    // SAFETY: 这个函数只调 Win32 菜单 API，不碰 `App` 的状态。
    // 每条 `AppendMenuW` 的失败都被忽略：少一项不至于让整个菜单弹不出来，
    // 而这些 API 除了句柄无效基本不失败。
    unsafe {
        let menu = CreatePopupMenu().ok()?;

        append(menu, id::OPEN, s.menu_open, false);
        // 没有媒体时这几项**置灰**而不是不画：位置不变，用户学得到布局，
        // 灰着也比「凭空少了两项」清楚为什么点不动。
        append(menu, id::PLAY_PAUSE, s.menu_play_pause, false);
        enable(menu, id::PLAY_PAUSE, snap.loaded);
        append(menu, id::STOP, s.menu_stop, false);
        enable(menu, id::STOP, snap.loaded);
        separator(menu);

        build_audio(menu, s, snap);
        build_sub(menu, s, snap);
        separator(menu);

        append(menu, id::SCREENSHOT, s.menu_screenshot, false);
        enable(menu, id::SCREENSHOT, snap.loaded);
        append(menu, id::STEP_BACK, s.menu_step_back, false);
        enable(menu, id::STEP_BACK, snap.loaded);
        append(menu, id::STEP_FWD, s.menu_step_fwd, false);
        enable(menu, id::STEP_FWD, snap.loaded);
        separator(menu);

        append(menu, id::SLOWER, s.menu_slower, false);
        enable(menu, id::SLOWER, snap.loaded);
        append(menu, id::SPEED_RESET, s.menu_speed_reset, false);
        // 「重置」只在真的变速过时才可用。恒可用的话按了没反应，
        // 用户会以为程序坏了。
        enable(
            menu,
            id::SPEED_RESET,
            snap.loaded && (snap.speed - 1.0).abs() > 1e-6,
        );
        append(menu, id::FASTER, s.menu_faster, false);
        enable(menu, id::FASTER, snap.loaded);
        separator(menu);

        append(menu, id::JUMP_START, s.menu_jump_start, false);
        enable(menu, id::JUMP_START, snap.loaded);
        append(menu, id::BACK_10S, s.menu_back_10s, false);
        enable(menu, id::BACK_10S, snap.loaded);
        append(menu, id::FWD_10S, s.menu_fwd_10s, false);
        enable(menu, id::FWD_10S, snap.loaded);
        append(menu, id::JUMP_END, s.menu_jump_end, false);
        enable(menu, id::JUMP_END, snap.loaded);
        separator(menu);

        append(menu, id::THEATRE, s.menu_theatre, snap.theatre);
        append(menu, id::FULLSCREEN, s.menu_fullscreen, snap.fullscreen);
        append(menu, id::DIAG, s.menu_diag, snap.diag_open);
        append(menu, id::HELP, s.menu_help, false);
        separator(menu);

        append(menu, id::COPY_REPORT, s.menu_copy_report, false);

        Some(menu)
    }
}

/// 音频子菜单。
///
/// 没有音轨时**画一个灰的子菜单**而不是不画。少一项会让后面每一项往上挪，
/// 而人的肌肉记忆会停在老位置 —— 第二次打开菜单就点到别的命令。
/// `TrackPopupMenu` 不记位置，但用户记得。
fn build_audio(menu: HMENU, s: &lang::Strings, snap: Snapshot<'_>) {
    // SAFETY: 只调 Win32 菜单 API。
    unsafe {
        let Ok(sub) = CreatePopupMenu() else { return };
        let empty = snap.tracks.audio.is_empty();
        if empty {
            append(sub, NO_TRACK_ID, s.menu_no_audio, false);
            enable(sub, NO_TRACK_ID, false);
        } else {
            for (n, t) in snap.tracks.audio.iter().enumerate() {
                append(sub, TRACK_BASE + n as u16, &describe(t, s), t.selected);
            }
        }
        // 可用条件是「有媒体**且**有轨」，两个都要。
        //
        // 只看 `loaded` 会漏掉「有媒体但文件本身没有音轨」—— 无声录屏、
        // 纯图形视频都是这种。那种情况下菜单会给一个能点开的灰壳子，
        // 展开后只有一行「（这个文件没有音轨）」。两个条件都满足才置灰，
        // 用户看到的就是一个明确的灰项。
        append_sub(menu, s.menu_audio, sub, snap.loaded && !empty);
    }
}

/// 字幕子菜单。第一项恒是「关闭字幕」。
fn build_sub(menu: HMENU, s: &lang::Strings, snap: Snapshot<'_>) {
    // SAFETY: 同 `build_audio`。
    unsafe {
        let Ok(sub) = CreatePopupMenu() else { return };
        let empty = snap.tracks.sub.is_empty();
        if empty {
            append(sub, NO_TRACK_ID, s.menu_no_sub, false);
            enable(sub, NO_TRACK_ID, false);
        } else {
            append(
                sub,
                id::SUB_OFF,
                s.track_off,
                snap.tracks.current_sub().is_none(),
            );
            separator(sub);
            for (n, t) in snap.tracks.sub.iter().enumerate() {
                append(sub, TRACK_BASE + n as u16, &describe(t, s), t.selected);
            }
        }
        // 字幕那个多一个条件：**关字幕**在没有字幕轨时也没意义
        // （本来就没字幕在放，关不掉什么）。
        append_sub(menu, s.menu_sub, sub, snap.loaded && !empty);
    }
}

// ---------------------------------------------------------------- Win32 小工具

/// 加一项。
///
/// `MF_STRING` 而不是 `MF_OWNERDRAW`：自己画要处理字体、DPI、高对比度
/// 模式下画出来的字可能看不见，而系统画的自动都对。
///
/// # Safety
/// `menu` 必须是有效的菜单句柄。
unsafe fn append(menu: HMENU, cmd: u16, text: &str, checked: bool) {
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let flags = if checked { MF_CHECKED } else { MF_UNCHECKED };
    let _ = AppendMenuW(menu, MF_STRING | flags, cmd as usize, PCWSTR(w.as_ptr()));
}

/// 加一条分隔线。
///
/// # Safety
/// `menu` 必须是有效的菜单句柄。
unsafe fn separator(menu: HMENU) {
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0usize, PCWSTR::null());
}

/// 加一个子菜单项。
///
/// **没有命令 ID 参数** —— `MF_POPUP` 时 `uidNewItem` 必须是子菜单句柄，
/// 点父菜单上这一项只是展开子菜单，不会「选中」什么。给它编一个命令 ID
/// 会让人以为点了能返回什么。
///
/// # Safety
/// 两个句柄都必须有效，且 `sub` 的所有权转交给 `menu`。
unsafe fn append_sub(menu: HMENU, text: &str, sub: HMENU, enabled: bool) {
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // 置灰用 `MF_GRAYED` 而不是 `MF_DISABLED`：置灰的子菜单**照样能被展开**，
    // 用户会看到一张灰着的空菜单 —— 「点开了却什么都不能选」比一开始
    // 就是灰的更难解释。
    let flags = if enabled { MF_STRING } else { MF_GRAYED };
    let _ = AppendMenuW(
        menu,
        MF_STRING | MF_POPUP | flags,
        sub.0 as usize,
        PCWSTR(w.as_ptr()),
    );
}

/// 启用 / 置灰一项。
///
/// # Safety
/// `menu` 必须是有效的菜单句柄。
unsafe fn enable(menu: HMENU, cmd: u16, on: bool) {
    // 原型是 3 参数（没有 by-position 变体），`cmd` 按**命令 ID** 解释。
    // 传 `MF_BYCOMMAND` 是多余的：它是 0，而这里的位置参数就是 flags。
    let flags = if on { MF_ENABLED } else { MF_GRAYED };
    let _ = EnableMenuItem(menu, cmd as u32, flags);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::{Track, TrackKind};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMenuItemCount, GetMenuItemInfoW, GetMenuState, MENUITEMINFOW, MENU_ITEM_MASK,
        MFS_CHECKED, MFS_GRAYED, MF_BYPOSITION, MIIM_ID, MIIM_STRING, MIIM_SUBMENU,
    };

    fn st() -> lang::Strings {
        lang::Strings::new(lang::Lang::ZhCn)
    }

    /// 测试专用的句柄包装：`CreatePopupMenu` 在测试里真的建了 Win32 菜单，
    /// 用完必须 `DestroyMenu`，否则每个测试漏一个菜单（用户态资源）。
    struct Owned(HMENU);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0 .0.is_null() {
                unsafe {
                    let _ = DestroyMenu(self.0);
                }
            }
        }
    }

    fn owned(s: &lang::Strings, snap: Snapshot<'_>) -> Owned {
        Owned(build(s, snap).expect("菜单应当建得出来"))
    }

    fn count(m: HMENU) -> i32 {
        unsafe { GetMenuItemCount(Some(m)) }
    }

    fn state(m: HMENU, pos: u32) -> u32 {
        unsafe { GetMenuState(m, pos, MF_BYPOSITION) }
    }

    fn is_checked(m: HMENU, pos: u32) -> bool {
        state(m, pos) & MFS_CHECKED.0 != 0
    }

    fn is_grayed(m: HMENU, pos: u32) -> bool {
        // `MFS_GRAYED == 3`（= MF_GRAYED | MF_DISABLED），不是 1。
        // 按 1 判的话 `MFS_DISABLED` 那一半会被漏掉。
        state(m, pos) & MFS_GRAYED.0 != 0
    }

    fn info(m: HMENU, pos: u32, mask: MENU_ITEM_MASK) -> MENUITEMINFOW {
        let mut ii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: mask,
            ..Default::default()
        };
        let ok = unsafe { GetMenuItemInfoW(m, pos, true, &mut ii) };
        assert!(ok.is_ok(), "读第 {pos} 项失败：{:?}", ok.err());
        ii
    }

    /// 取一项的标签。
    ///
    /// `GetMenuItemInfoW` **没有**「传缓冲区进去、它帮你填」那种参数形式 ——
    /// 字符串缓冲区要自己通过 `dwTypeData` 交进去，字符数用 `cch` 告诉它。
    /// 按 `GetMenuStringW` 的样子去猜会直接编译不过（参数个数不对），
    /// 所以这条注释留在这儿。
    fn label(m: HMENU, pos: u32) -> Option<String> {
        let mut buf = vec![0u16; 256];
        let mut ii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_STRING,
            dwTypeData: windows::core::PWSTR(buf.as_mut_ptr()),
            cch: buf.len() as u32,
            ..Default::default()
        };
        let ok = unsafe { GetMenuItemInfoW(m, pos, true, &mut ii) };
        if ok.is_err() {
            return None;
        }
        let end = buf.iter().position(|c| *c == 0).unwrap_or(0);
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    fn sub_handle(m: HMENU, pos: u32) -> HMENU {
        info(m, pos, MIIM_SUBMENU).hSubMenu
    }

    fn cmd(m: HMENU, pos: u32) -> u32 {
        info(m, pos, MIIM_ID).wID
    }

    fn find(m: HMENU, want: &str) -> u32 {
        (0..count(m) as u32)
            .find(|i| label(m, *i).as_deref() == Some(want))
            .unwrap_or_else(|| {
                panic!(
                    "菜单里找不到 {want:?}；现有：{:?}",
                    (0..count(m) as u32)
                        .map(|i| label(m, i))
                        .collect::<Vec<_>>()
                )
            })
    }

    fn audio(id: i64, selected: bool) -> Track {
        Track {
            id,
            kind: TrackKind::Audio,
            codec: "aac".into(),
            codec_desc: String::new(),
            language: "chi".into(),
            title: String::new(),
            selected,
            default: false,
            external: false,
            audio_channels: Some(2),
            sample_rate: Some(48000),
        }
    }

    fn sub(id: i64, selected: bool) -> Track {
        Track {
            id,
            kind: TrackKind::Sub,
            codec: "subrip".into(),
            codec_desc: String::new(),
            language: "eng".into(),
            title: String::new(),
            selected,
            default: false,
            external: true,
            audio_channels: None,
            sample_rate: None,
        }
    }

    /// 空的轨道集。
    ///
    /// 必须是 `static` 而不是 `&TrackSet::default()`：后者是临时值，
    /// 借用活不过下一条语句（`snap(&EMPTY)` 立刻编译不过）。
    static EMPTY: TrackSet = TrackSet {
        audio: Vec::new(),
        sub: Vec::new(),
    };

    fn snap(tracks: &TrackSet) -> Snapshot<'_> {
        Snapshot {
            loaded: true,
            paused: false,
            speed: 1.0,
            theatre: false,
            fullscreen: false,
            diag_open: false,
            tracks,
        }
    }

    #[test]
    fn 菜单建得出来且有内容() {
        let s = st();
        let m = owned(&s, snap(&EMPTY));
        assert!(count(m.0) > 10, "菜单项太少：{}", count(m.0));
    }

    #[test]
    fn 两种语言都能建出菜单且项数一致() {
        let mut counts = Vec::new();
        for l in [lang::Lang::ZhCn, lang::Lang::En] {
            let s = lang::Strings::new(l);
            let m = owned(&s, snap(&EMPTY));
            counts.push(count(m.0));
        }
        // 项数必须一致：少一项就意味着那一门语言漏了一条命令，
        // 而用户在另一种语言下会看到缺项的菜单却没人发现
        assert_eq!(counts[0], counts[1], "两种语言的菜单项数不一致");
    }

    #[test]
    fn 影院与诊断按当前状态打勾() {
        let s = st();
        let mut sn = snap(&EMPTY);
        sn.theatre = true;
        sn.diag_open = true;
        let m = owned(&s, sn);
        assert!(
            is_checked(m.0, find(m.0, s.menu_theatre)),
            "影院模式开着就该打勾"
        );
        assert!(
            is_checked(m.0, find(m.0, s.menu_diag)),
            "诊断面板开着就该打勾"
        );
        assert!(!is_checked(m.0, find(m.0, s.menu_fullscreen)));
    }

    #[test]
    fn 没有媒体时依赖媒体的项置灰() {
        let s = st();
        let mut sn = snap(&EMPTY);
        sn.loaded = false;
        let m = owned(&s, sn);
        for name in [
            s.menu_play_pause,
            s.menu_stop,
            s.menu_screenshot,
            s.menu_step_fwd,
            s.menu_step_back,
            s.menu_faster,
            s.menu_jump_end,
        ] {
            let i = find(m.0, name);
            assert!(is_grayed(m.0, i), "{name:?} 在没有媒体时应当置灰");
        }
        // 而这几项不依赖媒体，必须可用
        for name in [s.menu_open, s.menu_help, s.menu_theatre, s.menu_copy_report] {
            let i = find(m.0, name);
            assert!(!is_grayed(m.0, i), "{name:?} 不该依赖媒体");
        }
    }

    #[test]
    fn 没变速过时重置置灰() {
        let s = st();
        // speed = 1.0：没变速过，重置无意义
        let m1 = owned(&s, snap(&EMPTY));
        assert!(
            is_grayed(m1.0, find(m1.0, s.menu_speed_reset)),
            "没变速过时「重置」应当置灰"
        );
        // 变速过了就该能按
        let mut sn = snap(&EMPTY);
        sn.speed = 1.5;
        let m2 = owned(&s, sn);
        assert!(
            !is_grayed(m2.0, find(m2.0, s.menu_speed_reset)),
            "变速过之后「重置」必须可用"
        );
        // 1.0 + 一点点浮点误差也不该算「变速过」
        let mut sn = snap(&EMPTY);
        sn.speed = 1.0 + 1e-12;
        let m3 = owned(&s, sn);
        assert!(is_grayed(m3.0, find(m3.0, s.menu_speed_reset)));
    }

    #[test]
    fn 音轨子菜单列出每一条并给正在用的打勾() {
        let s = st();
        let mut set = TrackSet::default();
        set.audio.push(audio(1, false));
        set.audio.push(audio(2, true));
        let m = owned(&s, snap(&set));
        let pos = find(m.0, s.menu_audio);
        assert!(!is_grayed(m.0, pos), "有音轨时子菜单项不该置灰");
        let sub = Owned(sub_handle(m.0, pos));
        assert_eq!(count(sub.0), 2, "两条音轨就该两项");
        assert!(!is_checked(sub.0, 0));
        assert!(is_checked(sub.0, 1), "正在用的那条要打勾");
        assert_eq!(cmd(sub.0, 0), u32::from(TRACK_BASE));
        assert_eq!(cmd(sub.0, 1), u32::from(TRACK_BASE) + 1);
    }

    #[test]
    fn 字幕子菜单第一项恒是关闭() {
        let s = st();
        let mut set = TrackSet::default();
        set.sub.push(sub(1, true));
        let m = owned(&s, snap(&set));
        let pos = find(m.0, s.menu_sub);
        let sub = Owned(sub_handle(m.0, pos));
        assert_eq!(count(sub.0), 3, "关闭 + 分隔 + 1 条 = 3");
        assert_eq!(cmd(sub.0, 0), id::SUB_OFF as u32);
        // 正在放字幕时「关闭」不该打勾
        assert!(!is_checked(sub.0, 0));
        assert!(is_checked(sub.0, 2));
    }

    #[test]
    fn 字幕全关时关闭项就是选中态() {
        let s = st();
        let mut set = TrackSet::default();
        set.sub.push(sub(1, false));
        let m = owned(&s, snap(&set));
        let sub = Owned(sub_handle(m.0, find(m.0, s.menu_sub)));
        assert!(is_checked(sub.0, 0), "没有字幕在放，「关闭」就该是选中态");
    }

    #[test]
    fn 没有轨道时子菜单保留但置灰() {
        // 关键：不这么做的话父菜单少两项，后面每一项都往上挪，
        // 用户第二次打开时肌肉记忆停在老位置就点到别的命令
        let s = st();
        // 注意 `snap()` 的 `loaded` 是 true —— 「有媒体但文件没有音轨」
        // （无声录屏、纯图形视频）是真实情况，这种情况也必须置灰。
        // 只看 `loaded` 会给出一个能点开的灰壳子。
        let m = owned(&s, snap(&EMPTY));
        for name in [s.menu_audio, s.menu_sub] {
            let pos = find(m.0, name);
            assert!(is_grayed(m.0, pos), "{name:?} 在没有轨道时应当置灰");
            let sub = Owned(sub_handle(m.0, pos));
            assert_eq!(count(sub.0), 1, "仍然要有一个占位项保持布局");
            assert!(is_grayed(sub.0, 0), "占位那一项也要置灰");
        }
    }

    #[test]
    fn 没有媒体时子菜单同样置灰() {
        let s = st();
        let mut sn = snap(&EMPTY);
        sn.loaded = false;
        let m = owned(&s, sn);
        assert!(is_grayed(m.0, find(m.0, s.menu_audio)));
        assert!(is_grayed(m.0, find(m.0, s.menu_sub)));
    }

    #[test]
    fn 固定命令id互不重复且都在动态段之前() {
        // ID 是手写常量，重了的话菜单里点一个会执行另一个。
        // 这条是「手写编号」的护栏。
        let all = [
            id::OPEN,
            id::PLAY_PAUSE,
            id::STOP,
            id::SUB_OFF,
            id::SCREENSHOT,
            id::STEP_BACK,
            id::STEP_FWD,
            id::SLOWER,
            id::SPEED_RESET,
            id::FASTER,
            id::JUMP_START,
            id::BACK_10S,
            id::FWD_10S,
            id::JUMP_END,
            id::THEATRE,
            id::FULLSCREEN,
            id::DIAG,
            id::HELP,
            id::COPY_REPORT,
        ];
        let mut sorted = all.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "命令 ID 有重复");
        assert!(
            all.iter().all(|c| *c < TRACK_BASE),
            "固定命令 ID 必须小于 TRACK_BASE（{TRACK_BASE}）"
        );
        // 占位 ID 是 u16 的最大值。理论上轨道多到 `TRACK_BASE + n` 越过它时
        // 会撞上 —— 65535 条轨道的片子不存在，不值得为它加检查。真正的护栏
        // 是「占位项置灰，点它什么都不发生」，那条在
        // `没有轨道时子菜单保留但置灰` 里。
    }

    #[test]
    fn 每一条固定命令都能在菜单里找到() {
        // 反过来查：手写 ID 最容易犯的错是「加了菜单项但 ID 打错 / 忘了 enable」，
        // 那时 `find(cmd)` 找不到。这里按命令 ID 反查，钉住 ID 与实际
        // 菜单项是对应的。
        let s = st();
        let set = {
            let mut x = TrackSet::default();
            x.audio.push(audio(1, true));
            x.sub.push(sub(1, true));
            x
        };
        let m = owned(&s, snap(&set));
        for want in [
            id::OPEN,
            id::PLAY_PAUSE,
            id::STOP,
            id::SCREENSHOT,
            id::STEP_BACK,
            id::STEP_FWD,
            id::SLOWER,
            id::SPEED_RESET,
            id::FASTER,
            id::JUMP_START,
            id::BACK_10S,
            id::FWD_10S,
            id::JUMP_END,
            id::THEATRE,
            id::FULLSCREEN,
            id::DIAG,
            id::HELP,
            id::COPY_REPORT,
        ] {
            let found = (0..count(m.0) as u32).any(|i| cmd(m.0, i) == want as u32);
            assert!(found, "菜单里找不到命令 ID {want}");
        }
    }
}
