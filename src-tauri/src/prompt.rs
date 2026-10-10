//! 模态文本输入框：一个带输入框的小窗口，供「请输入名字」这类提问用。
//!
//! ## 为什么自己画，而不是用现成的东西
//!
//! 现成的选项都不合适：
//!
//! * `MessageBoxW` 没有输入框（只有 OK / Yes / No / Cancel）
//! * `IFileDialog` / `GetOpenFileNameW` 是**选文件**的。让用户为了一个书签
//!   名字去浏览目录树，是把简单事变复杂
//! * comctl32 v6 的 `InputBox` 需要额外依赖，且不可定制（不能预填、
//!   不能校验、不能本地化按钮文字）
//!
//! 自己的代价是一个小窗口过程，换来的是：能预填、能用系统 UI 字体、
//! 按系统 DPI 缩放、键盘行为（Tab 切换 / Enter 确认 / Esc 取消）与
//! 系统对话框完全一致（靠 `IsDialogMessageW` 白拿）。
//!
//! ## 行为约定
//!
//! * **Enter** = 确认、**Esc** = 取消（由 `IsDialogMessageW` 提供）
//! * 父窗口在弹窗期间被禁用，所以用户点不到背后的播放器
//! * 取消与空输入都返回 `None` —— 调用方不需要区分「按了取消」和
//!   「填了空白」，两者都意味着「没给出名字」
//!
//! ## 关于生命周期
//!
//! 结果通过 `GWLP_USERDATA` 传出：窗口过程是 `extern "system"`，
//! 拿不到 Rust 的闭包环境。指针指向调用栈上的 `Option<String>`，
//! 它在模态循环返回之前一直有效 —— 循环就嵌在
//! [`ask_text`] 这个栈帧里，所以这是安全的。
//!
//! 注意全程**只经由裸指针写**，不构造 `&mut`：窗口过程运行在同一个
//! 线程上，但把栈地址当成 `&'static mut` 会在优化下引入未定义行为。

// 导入路径**全部实测过**（在 windows 0.62.2 的源码里逐个查过定义位置，
// 不是按直觉写的）。这个 crate 的模块划分有几处反直觉，记下来免得再踩：
//
//   * `SS_LEFT` 在 `System::SystemServices`，不在 `UI::Controls`
//   * `EnableWindow` / `SetFocus` 在 `UI::Input::KeyboardAndMouse`，
//     不在 `UI::WindowsAndMessaging`
//   * `GetDpiForWindow` 在 `UI::HiDpi`
//   * `IsDialogMessageW` 在 `UI::WindowsAndMessaging`，不在 `UI::Controls`
//     （`EM_SETSEL` 才在 `UI::Controls`）
//   * `HBRUSH` / `GetStockObject` / `COLOR_BTNFACE` / `DEFAULT_GUI_FONT`
//     全在 `Graphics::Gdi`
//
// 查证方法（比反复试快）：
//   Grep "pub const SS_LEFT:" 在
//   ~/.cargo/registry/src/*/windows-0.62.2/src/Windows/Win32/
use std::ptr::{addr_of_mut, null_mut};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, COLOR_BTNFACE, DEFAULT_GUI_FONT, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::SS_LEFT;
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetDlgItem, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW, IsWindow, PostMessageW,
    PostQuitMessage, RegisterClassExW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, ES_AUTOHSCROLL, ES_LEFT, GWLP_USERDATA, HMENU, IDCANCEL, IDOK,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_DPICHANGED, WM_GETMINMAXINFO, WM_KEYDOWN, WM_NCCREATE, WM_SETFONT, WM_SIZE, WNDCLASSEXW,
    WS_CHILD, WS_CLIPCHILDREN, WS_EX_DLGMODALFRAME, WS_EX_TOPMOST, WS_OVERLAPPED, WS_POPUP,
    WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

/// 控件 ID。运行时拼的窗口，没有资源表可查，自己编号最容易读。
mod ctl {
    /// 输入框
    pub const EDIT: i32 = 1001;
    /// 确定按钮
    pub const OK: i32 = 1002;
    /// 取消按钮
    pub const CANCEL: i32 = 1003;
    /// 提示标签
    pub const LABEL: i32 = 1004;
}

/// 布局常量，单位是 DIP（96 DPI 下的像素）。
///
/// 全部走 DIP 再按父窗口的 DPI 换算成像素：进程是
/// `PER_MONITOR_AWARE_V2`，而新创建的窗口**不会**被系统自动缩放
/// （只有 `WM_DPICHANGED` 那条路），所以不换算就是 150% 屏幕上
/// 一个小 90 像素高的弹窗。
mod dip {
    /// 窗口宽度
    pub const WIDTH: i32 = 380;
    /// 提示文字那一行
    pub const LABEL_H: i32 = 18;
    /// 输入框高度
    pub const EDIT_H: i32 = 24;
    /// 按钮高度（等于系统默认按钮高度，用户改过系统设置时跟随）
    pub const BTN_H: i32 = 23;
    /// 按钮宽度
    pub const BTN_W: i32 = 84;
    /// 内边距
    pub const PAD: i32 = 12;
    /// 控件之间的间隙
    pub const GAP: i32 = 8;
}

/// 提问「请输入一句话」，返回 `Some(用户输入的内容，已去首尾空白)`。
///
/// `owner` 一般传主窗口：弹窗以它为父、居中于它、并在它上面禁用。
///
/// `initial` 是输入框的预填内容（例如书签的默认名字 `12:34`），
/// **打开时全选**（`EM_SETSEL` 全选）—— 用户按下就直接打字覆盖，
/// 而「光标在末尾」不符合「这个名字要换」的心智。
///
/// 返回 `None`：用户按了取消 / Esc / 关了窗口，或者确认了一个
/// **全空白**的输入。后者是有意的 —— 一个没有名字的书签在菜单里
/// 和分隔行长得一样，等于没有。
pub fn ask_text(owner: HWND, title: &str, prompt: &str, initial: &str) -> Option<String> {
    // SAFETY: 全部是 Win32 调用；指针的有效期由下面的模态循环保证。
    unsafe { ask_text_inner(owner, title, prompt, initial) }
}

unsafe fn ask_text_inner(owner: HWND, title: &str, prompt: &str, initial: &str) -> Option<String> {
    let hinst = GetModuleHandleW(None).ok()?;
    let class = w!("VideoViewPrompt");
    register_class(hinst.into(), class)?;

    let mut result: Option<String> = None;
    let result_ptr: *mut Option<String> = addr_of_mut!(result);

    let title_w = wide(title);
    let prompt_w = wide(prompt);
    let initial_w = wide(initial);

    // 父窗口不可见 / 已销毁时退回屏幕中心。「弹窗出现在屏幕外，用户
    // 以为程序没反应」是最糟的失败形态，所以这里不用父窗口尺寸也算。
    let dpi = if IsWindow(Some(owner)).as_bool() {
        GetDpiForWindow(owner).max(96)
    } else {
        96
    };
    let px = |d: i32| -> i32 { (d as f64 * dpi as f64 / 96.0).round() as i32 };

    // 不带 WS_VISIBLE：先建子控件再显示，否则会闪一下空窗口。
    let style = WS_OVERLAPPED | WS_POPUP | WS_SYSMENU | WS_CLIPCHILDREN;
    let ex = WS_EX_DLGMODALFRAME | WS_EX_TOPMOST;
    let hwnd = CreateWindowExW(
        ex,
        class,
        PCWSTR(title_w.as_ptr()),
        style,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        px(dip::WIDTH),
        px(100),
        Some(owner),
        Some(HMENU(null_mut())),
        Some(hinst.into()),
        None,
    )
    .ok()?;

    // 只存地址，不建引用 —— 见模块说明「关于生命周期」。
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, result_ptr as isize);

    // 按最终客户区高度摆控件，再把窗口调到这个尺寸 + 边框。
    let want_client_h =
        px(dip::PAD * 2 + dip::LABEL_H + dip::GAP + dip::EDIT_H + dip::GAP + dip::BTN_H);
    let mut wr = std::mem::zeroed();
    AdjustWindowRectEx(&mut wr, style, false, WINDOW_EX_STYLE(0)).ok()?;
    SetWindowPos(
        hwnd,
        None,
        0,
        0,
        px(dip::WIDTH) + (wr.right - wr.left),
        want_client_h + (wr.bottom - wr.top),
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE,
    )
    .ok()?;

    // `build_children` 返回 `()` —— 它的失败方式是「控件没建出来」，
    // 而弹窗仍然可用（只是空），所以这里不中断。
    build_children(hwnd, hinst.into(), &prompt_w, &initial_w, px);
    center_on(hwnd, owner, px);
    // 从这里往下，几个 Win32 调用的返回值（`BOOL` = 成功与否）都不关心：
    // 窗口已经建出来了，显示成不成功、能不能抢到前台，都不影响后面那个
    // 模态循环跑不跑得起来。windows crate 给它们标了 `#[must_use]`，
    // 统一用 `let _ =` 显式丢弃 —— 让这堆噪音留在原地，真正的警告
    // （比如何处漏了 `?`）就会被淹掉。
    let _ = ShowWindow(hwnd, SW_SHOW);

    // 模态：禁用父窗口 → 自己的消息循环 → 恢复。
    //
    // 不用 `DialogBoxIndirectParam`：那是 comdlg32 的模板机制，而主窗口是
    // 自绘 + 自建消息泵，混进第二套对话框约定会让「一套消息循环」的
    // 前提破掉。
    let _ = EnableWindow(owner, false);
    let _ = SetForegroundWindow(hwnd);

    let mut msg = std::mem::zeroed();
    loop {
        // `GetMessageW` 在这一版 windows crate 里返回裸 `BOOL`（不是 `Result`）：
        //   *  0 = WM_QUIT（我们在 WM_DESTROY 里发的）
        //   * -1 = 出错（例如无效的 hwnd）
        // 两种都该退出循环，所以判 `<= 0` 而不是 `== 0`。
        let ret = GetMessageW(&mut msg, None, 0, 0);
        if ret.0 <= 0 {
            break;
        }
        // `IsDialogMessageW` 把 Tab / Enter / Esc 处理掉，并且**吞掉**
        // 它处理过的那些消息 —— 返回 true 时不能再 Dispatch，否则
        // 「按 Tab」会既切焦点又插一个制表符。
        if !IsDialogMessageW(hwnd, &msg).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    let _ = EnableWindow(owner, true);
    // 焦点还给父窗口：否则关掉弹窗后主窗口没有焦点，键盘快捷键
    // 要用户先点一下画面才恢复。
    let _ = SetForegroundWindow(owner);

    result.and_then(|s| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    })
}

/// 读回输入框内容。`None` = 控件不存在（不该发生，读成空串更安全）。
unsafe fn read_edit(hwnd: HWND) -> String {
    // `GetDlgItem` 返回 `Result<HWND>`：控件不在时就这一种失败，
    // 读成空串比往下传一个无效句柄安全。
    let Ok(edit) = GetDlgItem(Some(hwnd), ctl::EDIT) else {
        return String::new();
    };
    let n = GetWindowTextLengthW(edit);
    if n <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (n as usize) + 1];
    let got = GetWindowTextW(edit, &mut buf);
    buf.truncate(got.max(0) as usize);
    String::from_utf16_lossy(&buf)
}

/// 建子控件并一次性摆好位置。
///
/// 固定布局（提示 → 输入框 → 右下两按钮）而不是布局引擎：本弹窗
/// 只有一个输入框，通用布局的代码量比硬编码大得多，而且没有任何
/// 「内容变了要重排」的场景。
unsafe fn build_children(
    hwnd: HWND,
    hinst: HINSTANCE,
    prompt: &[u16],
    initial: &[u16],
    px: impl Fn(i32) -> i32,
) {
    let mut c = std::mem::zeroed();
    if GetClientRect(hwnd, &mut c).is_err() {
        return;
    }
    let (cw, ch) = (c.right, c.bottom);
    let pad = px(dip::PAD);
    let gap = px(dip::GAP);
    let inner_w = cw - pad * 2;

    // 字体：系统 GUI 字体，跟着用户的「Segoe UI / 微软雅黑」设置走，
    // 我们不该替他们选。
    let font = GetStockObject(DEFAULT_GUI_FONT);

    // `style` 收 `u32` 而不是 `WINDOW_STYLE`：控件样式常量在这一版 crate 里
    // 分属不同模块、还是不同类型（`SS_LEFT` 是 `STATIC_STYLES`、`ES_*` /
    // `BS_*` 是裸 `i32`、`WS_*` 是 `WINDOW_STYLE`）。让每个调用点各自转换
    // 一次，比在这里定义一个包罗万象的类型要清楚。
    let make =
        |class: PCWSTR, text: &[u16], style: u32, x: i32, y: i32, w: i32, h: i32, id: i32| {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(text.as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
                x,
                y,
                w.max(1),
                h.max(1),
                Some(hwnd),
                // 子控件的 ID 走 `hMenu` 形参传（这是 Win32 的约定），
                // 所以要包成 `HMENU` —— 它不是真的菜单句柄，是个整数。
                Some(HMENU(id as *mut _)),
                Some(hinst),
                None,
            )
            .ok()
        };

    let label_y = pad;
    let edit_y = label_y + px(dip::LABEL_H) + gap;
    let btn_y = ch - pad - px(dip::BTN_H);
    let btn_w = px(dip::BTN_W);

    let label = make(
        w!("STATIC"),
        prompt,
        SS_LEFT.0,
        pad,
        label_y,
        inner_w,
        px(dip::LABEL_H),
        ctl::LABEL,
    );
    let edit = make(
        w!("EDIT"),
        initial,
        WS_TABSTOP.0 | ES_AUTOHSCROLL as u32 | ES_LEFT as u32,
        pad,
        edit_y,
        inner_w,
        px(dip::EDIT_H),
        ctl::EDIT,
    );
    // 按钮文字用系统自带的 IDOK / IDCANCEL：它们在系统语言下会自动
    // 翻译成「确定 / 取消」（或对应语言），而自己写死字符串就要维护
    // 第三份翻译表，且在非中英文系统上是错的。
    let ok = make(
        w!("BUTTON"),
        &wide_id(IDOK.0 as u16),
        WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
        cw - pad - btn_w * 2 - gap,
        btn_y,
        btn_w,
        px(dip::BTN_H),
        ctl::OK,
    );
    let cancel = make(
        w!("BUTTON"),
        &wide_id(IDCANCEL.0 as u16),
        WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
        cw - pad - btn_w,
        btn_y,
        btn_w,
        px(dip::BTN_H),
        ctl::CANCEL,
    );

    for ctl_hwnd in [label, edit, ok, cancel].into_iter().flatten() {
        SendMessageW(
            ctl_hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }

    // 输入框拿焦点且全选：用户直接打字覆盖预填内容。
    if let Some(edit) = edit {
        let _ = SetFocus(Some(edit));
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
}

/// `IDOK` / `IDCANCEL` 的系统字符串。
///
/// 不用 `GetDlgItemText` 之类去问系统 —— 直接用这两个常量对应的
/// 资源字符串即可：控件的 ID 就是 `IDOK` 时 Win32 会自动用系统翻译，
/// 而我们仍然需要一份文本去 `CreateWindowExW`，所以显式取一次。
fn wide_id(id: u16) -> Vec<u16> {
    // 不能写成 `IDOK => ...`：`IDOK` 是 `MESSAGEBOX_RESULT` newtype，
    // 既不能直接当整数 pattern，被打分值也已经 `as i32` 拆成裸整数了。
    // 用 guard 比较内部的 `.0`。
    match id as i32 {
        id if id == IDOK.0 => wide_id_str("OK"),
        id if id == IDCANCEL.0 => wide_id_str("Cancel"),
        _ => wide_id_str(""),
    }
}

fn wide_id_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 把弹窗居中到父窗口（或屏幕）上，并夹进屏幕可见范围。
///
/// 夹屏幕这一步不能省：多显示器场景下父窗口可能在负坐标区（左侧副屏），
/// 直接按父窗口中心算会落到主屏左边之外 —— 弹窗「不在那儿」。
unsafe fn center_on(hwnd: HWND, owner: HWND, px: impl Fn(i32) -> i32) {
    let _ = px;
    let mut me = std::mem::zeroed();
    if GetWindowRect(hwnd, &mut me).is_err() {
        return;
    }
    let (ww, wh) = (me.right - me.left, me.bottom - me.top);

    let mut pr = std::mem::zeroed();
    let have_parent = IsWindow(Some(owner)).as_bool() && GetWindowRect(owner, &mut pr).is_ok();
    let (mut x, mut y) = if have_parent {
        (
            pr.left + (pr.right - pr.left - ww) / 2,
            pr.top + (pr.bottom - pr.top - wh) / 2,
        )
    } else {
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        ((sw - ww) / 2, (sh - wh) / 2)
    };

    // 夹到主屏内。负坐标保留（左侧副屏是合法位置），只夹上界。
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    if sw > 0 && x > sw - ww {
        x = (sw - ww).max(0);
    }
    if sh > 0 && y > sh - wh {
        y = (sh - wh).max(0);
    }
    let _ = SetWindowPos(
        hwnd,
        None,
        x,
        y,
        0,
        0,
        SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE,
    );
}

/// 注册窗口类。重复注册会失败，那不是错误（`RegisterClassExW` 返回
/// 一个「类名已存在」的错误），所以返回值一律忽略。
unsafe fn register_class(hinst: HINSTANCE, class: PCWSTR) -> Option<()> {
    let _ = RegisterClassExW(&WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(prompt_wnd_proc),
        hInstance: hinst,
        hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
            None,
            windows::Win32::UI::WindowsAndMessaging::IDC_ARROW,
        )
        .ok()?,
        // Win32 的老约定：`hbrBackground` 里放一个**小于 16 的整数**
        // 表示「用第 N 个系统颜色当背景」，取值就是 `COLOR_x + 1`。
        // 所以这里是把一个整数直接当句柄塞进指针字段 —— 它不是真的指针，
        // 存的是索引。`HBRUSH` 在 windows 0.62 里是 `*mut c_void` 的
        // newtype，所以要显式转成指针类型。
        hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut std::ffi::c_void),
        lpszClassName: class,
        ..Default::default()
    });
    Some(())
}

unsafe extern "system" fn prompt_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE | WM_GETMINMAXINFO | WM_DPICHANGED => DefWindowProcW(hwnd, msg, wparam, lparam),

        WM_SETFONT | WM_SIZE => LRESULT(0),

        WM_COMMAND => {
            // 只处理自己那几个控件的 ID；系统菜单、关闭按钮走 Def。
            //
            // 这里用 `if` 链而不是 `match`：`IDOK` / `IDCANCEL` 是
            // `MESSAGEBOX_RESULT` newtype，没法直接当整数 pattern。
            let id = (wparam.0 & 0xFFFF) as i32;
            if id == ctl::OK || id == IDOK.0 {
                // 结果只写一次，经由裸指针 —— 不构造 `&mut`。
                let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Option<String>;
                if !p.is_null() {
                    let text = read_edit(hwnd);
                    *p = if text.trim().is_empty() {
                        None
                    } else {
                        Some(text)
                    };
                }
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            } else if id == ctl::CANCEL || id == IDCANCEL.0 {
                let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Option<String>;
                if !p.is_null() {
                    *p = None;
                }
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }

        WM_CLOSE => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Option<String>;
            if !p.is_null() {
                *p = None;
            }
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }

        WM_DESTROY => {
            // 让 `GetMessageW` 返回 0，模态循环才有出口。
            // **只在真正销毁时发**：WM_CLOSE 会走到这里，但 Enter 确认
            // 走的是 `WM_COMMAND` → `DestroyWindow` → 也是这里，两条
            // 路径都发一次不会有问题（`PostQuitMessage` 幂等）。
            PostQuitMessage(0);
            LRESULT(0)
        }

        // Enter / Esc：`IsDialogMessageW` 已经处理过一轮了，走到这里
        // 说明焦点不在对话框的对话框部分（例如在系统菜单上）。
        // 显式兜住是因为「按下 Enter 整个窗口消失」是必须保证的。
        WM_KEYDOWN => match wparam.0 as u16 {
            v if v == VK_RETURN.0 => {
                let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(IDOK.0 as usize), LPARAM(0));
                LRESULT(0)
            }
            v if v == VK_ESCAPE.0 => {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        },

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
