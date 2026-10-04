//! mpv 的视频输出窗口管理。
//!
//! mpv 把画面渲染到一个原生子窗口上，而这个子窗口的位置由 Win32 决定，
//! 不会跟随前端网页布局变化。所以前端需要在 resize 后把视频区域的坐标
//! 传进来，这里负责创建子窗口并摆到对应位置。
//!
//! 视频子窗口整个盖在 WebView2 之上，网页收不到落在画面里的鼠标消息。
//! 因此这里自定义了窗口过程，把点击 / 双击转成事件回调交给前端：
//! 单击画面切换播放暂停，双击画面切换影院模式。

use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, LoadCursorW, RegisterClassExW, SetWindowPos,
    CS_DBLCLKS, IDC_ARROW, HWND_TOP, SWP_NOACTIVATE, SWP_SHOWWINDOW, WINDOW_EX_STYLE,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_PARENTNOTIFY, WNDCLASSEXW, WS_CHILD, WS_VISIBLE,
};

/// DPI 为 96 时是 100% 缩放。
const DPI_BASE: f64 = 96.0;

/// 视频子窗口的窗口类名。注册失败（已存在）时忽略，见 `register_video_class`。
const VIDEO_CLASS: &str = "VideoViewVideoChild";

/// 原生层转发给前端的事件回调（click / dblclick）。
type NativeHandler = Box<dyn Fn(&'static str) + Send + Sync>;

static NATIVE_HANDLER: Mutex<Option<NativeHandler>> = Mutex::new(None);

/// 注册原生子窗口事件回调，`init_player` 时调用一次。
pub fn set_native_handler(handler: NativeHandler) {
    *NATIVE_HANDLER.lock().unwrap() = Some(handler);
}

fn emit_native(name: &'static str) {
    if let Some(handler) = NATIVE_HANDLER.lock().unwrap().as_ref() {
        handler(name);
    }
}

/// 创建一个承载 mpv 画面的子窗口，父窗口是 `parent`。
pub fn create_video_window(parent: HWND) -> Result<HWND, String> {
    let hinstance = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map_err(|e| format!("获取模块句柄失败: {e}"))?;
    unsafe { register_video_class(HINSTANCE(hinstance.0)) };

    let class_name = wide(VIDEO_CLASS);
    let window_name = wide("");
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_name.as_ptr()),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            1,
            1,
            Some(parent),
            None,
            Some(HINSTANCE(hinstance.0)),
            None,
        )
        .map_err(|e| format!("创建视频渲染窗口失败: {e}"))
    }
}

/// 注册自定义窗口类。
///
/// 重复注册（同一进程内再次初始化）返回 ERROR_CLASS_ALREADY_EXISTS，忽略即可。
unsafe fn register_video_class(hinstance: HINSTANCE) {
    let class_name = wide(VIDEO_CLASS);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        // CS_DBLCLKS：让双击画面能收到 WM_LBUTTONDBLCLK（切换影院模式）
        style: CS_DBLCLKS,
        lpfnWndProc: Some(video_wnd_proc),
        hInstance: hinstance,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    // 失败也不中止：真正的问题会让后面的 CreateWindowExW 报错
    let _ = RegisterClassExW(&wc);
}

/// 视频子窗口的窗口过程。
///
/// mpv 用 Direct3D 自己绘制，不依赖窗口类的绘制行为，其余消息走默认处理。
unsafe extern "system" fn video_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // mpv 在容器窗口里又创建了自己的渲染子窗口，真实鼠标消息实际
        // 命中它；子窗口未处理的鼠标消息会以 WM_PARENTNOTIFY 送到这里。
        WM_PARENTNOTIFY => {
            let kind = (wparam.0 as u32) & 0xffff;
            match kind {
                WM_LBUTTONDOWN => {
                    emit_native("click");
                    LRESULT(0)
                }
                WM_LBUTTONDBLCLK => {
                    emit_native("dblclick");
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        // 直接落在容器窗口上的点击（mpv 子窗口还没创建 / 未覆盖时）
        WM_LBUTTONDOWN => {
            emit_native("click");
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            emit_native("dblclick");
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 把视频子窗口移动到指定位置。
///
/// `x` / `y` / `width` / `height` 是前端 `getBoundingClientRect()` 的值，
/// 单位是 **CSS 逻辑像素**（实测 125% DPI 下客户区物理 1375x900，stage
/// 传入 1100x658）。Win32 的窗口坐标是物理像素，必须乘 DPI scale 换算。
/// 曾经一度漏掉这层换算，视频窗口只有 1100x658 物理像素，盖不满 #stage，
/// 中央的欢迎文字会被这个偏小的黑窗口挡住。
pub fn set_video_window_bounds(
    parent: HWND,
    video: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    unsafe {
        let scale = window_scale(parent);

        // 窗口最小化时客户区可能为 0，此时直接摆位置会得到负尺寸。
        let mut client = RECT::default();
        if GetClientRect(parent, &mut client).is_err() {
            return;
        }
        let visible_w = client.right - client.left;
        let visible_h = client.bottom - client.top;
        if visible_w <= 0 || visible_h <= 0 {
            return;
        }

        // 逻辑像素 → 物理像素，换算完成后再 clamp 到客户区（物理）内。
        let w = ((width as f64 * scale).round() as i32).clamp(1, visible_w);
        let h = ((height as f64 * scale).round() as i32).clamp(1, visible_h);

        // x/y 是相对父窗口客户区的坐标，同样换算成物理像素。
        // 不做 ClientToScreen 屏幕坐标换算，避免 DPI 虚拟化叠加入口。
        let sx = (x as f64 * scale).round() as i32;
        let sy = (y as f64 * scale).round() as i32;

        // HWND_TOP：把视频窗口放到 WebView2 之上。
        // 视频窗口严格限制在 #stage 区域内（由 sync_video_rect 定位），
        // 控制栏在 #stage 下方，不会被遮挡；而 WebView2 的 HTML 背景是不透明的，
        // 若视频窗口在其下层，会被 #stage 的背景完全盖住（画面黑屏）。
        let _ = SetWindowPos(
            video,
            Some(HWND_TOP),
            sx,
            sy,
            w,
            h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// 窗口的 DPI 缩放比例，96 DPI 时为 1.0。
fn window_scale(hwnd: HWND) -> f64 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            // 老系统没有 per-monitor DPI，退回 100%
            1.0
        } else {
            dpi as f64 / DPI_BASE
        }
    }
}

/// 从原始句柄值还原 HWND。
///
/// `HWND` 内部是裸指针，不实现 `Send`/`Sync`，而 Tauri 的状态要求
/// `Send + Sync`。所以状态里存 `isize`，用的时候再还原。
pub fn hwnd_from_raw(raw: isize) -> HWND {
    HWND(raw as *mut std::ffi::c_void)
}

/// 字符串 -> 以 NUL 结尾的 UTF-16 宽字符串。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}