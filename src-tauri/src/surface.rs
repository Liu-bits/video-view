//! mpv 的视频输出窗口管理。
//!
//! mpv 把画面渲染到一个原生子窗口上，而这个子窗口的位置由 Win32 决定。
//! 界面层算出视频区矩形后调用 `set_video_bounds` 把它摆到位。
//!
//! 视频子窗口整个盖在主窗口之上，所以落在画面里的鼠标消息网页——现在是
//! 主窗口——都收不到。因此这里自定义了窗口过程，把点击 / 双击 / 拖放
//! 转交给上层：单击画面切换播放暂停，双击画面切换影院模式。
//!
//! 坐标一律是**物理像素**。本进程是 per-monitor DPI 感知（见 `lib.rs`
//! 里的 `SetProcessDpiAwarenessContext`），Win32 坐标与客户区尺寸本身
//! 就是设备像素，这里不需要也不应该再做 DPI 换算。

use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetParent, LoadCursorW, RegisterClassExW, SendMessageW,
    SetWindowPos, CS_DBLCLKS, HWND_TOP, IDC_ARROW, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_SHOWWINDOW, WM_DROPFILES, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_PARENTNOTIFY,
    WNDCLASSEXW, WS_CHILD, WS_EX_NOACTIVATE, WS_VISIBLE,
};

/// 视频子窗口的窗口类名。自动化脚本按这个名字找画面窗口，改名要同步改脚本。
const VIDEO_CLASS: &str = "VideoViewVideoChild";

/// 原生层转交上层的视频区事件。
///
/// * `"click"` —— 单击画面
/// * `"dblclick"` —— 双击画面
type VideoHandler = Box<dyn Fn(&'static str) + Send + Sync>;

static VIDEO_HANDLER: Mutex<Option<VideoHandler>> = Mutex::new(None);

/// 注册视频区事件回调，创建播放核心前调用一次。
pub fn set_video_handler(handler: VideoHandler) {
    *VIDEO_HANDLER.lock().unwrap() = Some(handler);
}

fn emit(name: &'static str) {
    // handler 内部只做 PostMessage，不回调本模块的锁，不存在重入死锁
    if let Some(handler) = VIDEO_HANDLER.lock().unwrap().as_ref() {
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
    let hwnd = unsafe {
        CreateWindowExW(
            // WS_EX_NOACTIVATE：画面吃掉点击后不能抢走键盘焦点，
            // 否则空格 / 方向键这些快捷键会发给视频窗口而不是主窗口。
            WS_EX_NOACTIVATE,
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
        .map_err(|e| format!("创建视频渲染窗口失败: {e}"))?
    };

    // 拖放要注册在真正被命中的那个窗口上。视频子窗口盖住了整个画面区，
    // 只在主窗口注册的话，拖到画面上的文件不会有任何反应。
    unsafe { DragAcceptFiles(hwnd, true) };
    Ok(hwnd)
}

/// 注册自定义窗口类。
///
/// 重复注册（同一进程内再次初始化）返回 ERROR_CLASS_ALREADY_EXISTS，忽略即可。
unsafe fn register_video_class(hinstance: HINSTANCE) {
    let class_name = wide(VIDEO_CLASS);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        // CS_DBLCLKS：让双击画面能收到 WM_LBUTTONDBLCLK（切换影院模式）
        // CS_DBLCLKS：让双击画面能收到 WM_LBUTTONDBLCLK（切换影院模式）。
        // 但它只保证「这个容器窗口」能收到——mpv 自己建的渲染子窗口属于
        // mpv 的窗口类，带不带 CS_DBLCLKS 取决于 mpv。所以 click 的去重
        // 不能依赖它，见 `handle_click`。
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
                WM_LBUTTONDOWN => handle_click(lparam),
                WM_LBUTTONDBLCLK => {
                    emit("dblclick");
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        // 直接落在容器窗口上的点击（mpv 子窗口还没创建 / 未覆盖时）
        WM_LBUTTONDOWN => handle_click(lparam),
        WM_LBUTTONDBLCLK => {
            emit("dblclick");
            LRESULT(0)
        }
        // 拖到画面上的文件：原样转交主窗口，由它负责 DragFinish 与打开文件。
        // 用 SendMessage 而不是 PostMessage，保证主窗口处理完（已经调用
        // DragFinish 释放 HDROP）之后这里才返回。
        WM_DROPFILES => {
            match GetParent(hwnd) {
                Ok(parent) => SendMessageW(parent, msg, Some(wparam), Some(lparam)),
                Err(_) => {
                    // 拿不到父窗口就没法转交，但 HDROP 仍然是这次拖放唯一的
                    // 句柄，直接交给 DefWindowProcW 不会释放它——每次这样漏一个，
                    // 而这个句柄要等进程退出才回收。得自己 DragFinish。
                    DragFinish(HDROP(wparam.0 as *mut c_void));
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 上一次报给上层的单击时间与位置，用来识别双击。
///
/// 之前双击的语义是「碰运气」的：能不能进影院模式取决于 mpv 自己创建的
/// 渲染子窗口的窗口类带没带 `CS_DBLCLKS`。带的话，一次双击是
/// `WM_LBUTTONDOWN` + `WM_LBUTTONDBLCLK`，于是一次双击既发了 click 又发了
/// dblclick——用户观感是「双击之后画面变暂停了」；不带的话就是两次
/// WM_LBUTTONDOWN，两次 click 互相抵消，影院模式永远进不去。
/// 两种结局都不对，而 mpv 的实现我们控制不了。
///
/// 所以改成自己判断：位置接近且间隔在系统双击时间内，就只发 dblclick、
/// 吃掉这次 click。发不发 dblclick 则看有没有真的收到 `WM_LBUTTONDBLCLK`
/// ——它由 Win32 判定，本模块只负责让 click 不重复。
static LAST_CLICK: Mutex<Option<(Instant, i32, i32)>> = Mutex::new(None);

/// 双击判定用的位置容差（物理像素）。
///
/// 系统双击时间内的两次点击，按下位置一般不会差超过几个像素；给 8px
/// 容忍手抖，又不至于把「两次快速点击别处」误判成双击。
const DBLCLICK_SLOP: i32 = 8;

/// 决定这次点击是不是双击的后半截，是的话就不发 `click`。
///
/// 抽成独立函数是为了能单测：双击去重的判断逻辑很短，但一旦改错，
/// 症状是「双击画面会同时暂停」或「影院模式进不去」——都是只有手动试
/// 播放才能发现的回归。
fn is_double_click_tail(x: i32, y: i32, now: Instant, dbl_timeout: Duration) -> bool {
    match LAST_CLICK.lock() {
        Ok(mut guard) => {
            let swallow = match *guard {
                Some((t, px, py)) => {
                    now.duration_since(t) <= dbl_timeout
                        && (x - px).abs() <= DBLCLICK_SLOP
                        && (y - py).abs() <= DBLCLICK_SLOP
                }
                None => false,
            };
            // 不管吞不吞都要刷新：吞掉的这次是双击的第二击，不刷新的话
            // 第三击会跟「双击的第一击」配对上
            *guard = Some((now, x, y));
            swallow
        }
        // 上一次点击时锁中毒了：宁可多发一次 click，也不要漏掉双击判定
        Err(_) => false,
    }
}

fn handle_click(lparam: LPARAM) -> LRESULT {
    let (x, y) = ((lparam.0 as i16) as i32, ((lparam.0 >> 16) as i16) as i32);
    // 系统双击时间是用户可在「鼠标属性」里改的，读当前值而不是写死常量
    let dbl_timeout = Duration::from_millis(unsafe { GetDoubleClickTime() } as u64);
    if !is_double_click_tail(x, y, Instant::now(), dbl_timeout) {
        emit("click");
    }
    LRESULT(0)
}

/// 把视频子窗口摆到视频区矩形上。
///
/// 坐标是相对父窗口客户区的物理像素。窗口最小化时客户区可能为 0，
/// 此时直接摆位置会得到负尺寸，直接跳过。
pub fn set_video_bounds(video: HWND, bounds: &RECT) {
    let w = bounds.right - bounds.left;
    let h = bounds.bottom - bounds.top;
    if w <= 0 || h <= 0 {
        return;
    }
    unsafe {
        // HWND_TOP：把视频窗口放到主窗口的绘制内容之上。视频窗口严格限制
        // 在视频区内，控制栏在它下方不受影响；而主窗口客户区背景不透明，
        // 视频窗口若在下层会被完全盖住（画面黑屏）。
        //
        // 不带 SWP_SHOWWINDOW：显示与否由 `set_video_visible` 统一决定
        // （空闲时隐藏，好让「拖放到这里」的提示露出来）。
        let _ = SetWindowPos(
            video,
            Some(HWND_TOP),
            bounds.left,
            bounds.top,
            w,
            h,
            SWP_NOACTIVATE,
        );
    }
}

/// 显示 / 隐藏画面窗口。
///
/// 空闲时隐藏视频窗口，露出主窗口自己画的欢迎提示——否则一个不透明的黑窗口
/// 盖在提示上面，什么都看不见。
pub fn set_video_visible(video: HWND, visible: bool) {
    let show = if visible {
        SWP_SHOWWINDOW
    } else {
        SWP_HIDEWINDOW
    };
    unsafe {
        let _ = SetWindowPos(
            video,
            None,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | show,
        );
    }
}

/// 字符串 -> 以 NUL 结尾的 UTF-16 宽字符串。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{is_double_click_tail, DBLCLICK_SLOP, LAST_CLICK};
    use std::sync::{Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    /// 把这几个测试串行化的锁。必须存在，理由见下。
    ///
    /// `is_double_click_tail` 的状态存在进程级 `static LAST_CLICK` 里，
    /// 而 `cargo test` 默认把测试**并行**跑在不同线程上。于是：
    ///
    ///   线程 A（第一次点击不吞）  reset()          -> LAST_CLICK = None
    ///   线程 B（双击第二击被吞）  第一次调用        -> LAST_CLICK = 某个时刻
    ///   线程 A                    断言 !is_tail()  -> 读到 B 留下的值 -> 失败
    ///
    /// 这个失败是随机的：取决于线程调度，本地连续跑十次可能都过，
    /// 换个机器、换个测试数量、换个 CPU 负载就炸。实测在全新 clone 里
    /// 编译后第一次跑就挂了（`位置差太远不吞`），而原仓库里重跑又过——
    /// 这种「有时过有时不过」的测试比没有测试更糟：它会让人怀疑产品代码。
    ///
    /// 修法是让这几个测试各自独占这份状态。要改的是测试的组织方式，
    /// 不是产品逻辑：`is_double_click_tail` 本身的判断是对的。
    static SERIAL: Mutex<()> = Mutex::new(());

    /// 取串行锁并绑定到当前作用域，作用域结束（测试结束）时自动释放。
    ///
    /// 必须返回 `MutexGuard` 而不是只在 `reset()` 里加锁：锁的生命周期
    /// 要覆盖**整个**测试体，reset 只是其中第一步。
    ///
    /// 用 `unwrap_or_else(|e| e.into_inner())` 而不是 `unwrap()`：
    /// 上一个测试 panic 时锁会中毒，之后每个测试都会 panic 在
    /// `.unwrap()` 上——一个失败把剩下四个全带崩，报错信息还完全指错方向。
    fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 清掉双击基准点。调用前必须已持有 `serial()`。
    fn reset() {
        *LAST_CLICK.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 单击必须真的发出去。
    ///
    /// 这是双击去重的另一半：`is_double_click_tail` 返回 true 就**不**发 click。
    /// 如果它对第一次点击也返回 true，「单击画面切换播放暂停」就彻底不工作了。
    #[test]
    fn 第一次点击不吞() {
        let _serial = serial();
        reset();
        let now = Instant::now();
        assert!(!is_double_click_tail(
            100,
            100,
            now,
            Duration::from_millis(500)
        ));
    }

    /// 双击的第二击必须被吞掉。
    ///
    /// 回归点：双击画面的语义是「进/出影院模式」。如果不吞这次 click，
    /// 一次双击就是「切了影院模式 + 又暂停了一次」，用户看到的是
    /// 「双击之后画面变暂停了」。
    #[test]
    fn 双击第二击被吞() {
        let _serial = serial();
        reset();
        let t0 = Instant::now();
        let dbl = Duration::from_millis(500);
        assert!(!is_double_click_tail(100, 100, t0, dbl));
        // 120ms 后、位置差 2px：Win32 会判成双击，这次 click 要吞
        assert!(is_double_click_tail(
            102,
            98,
            t0 + Duration::from_millis(120),
            dbl
        ));
    }

    /// 间隔太长不算双击，两次都是普通单击。
    ///
    /// 系统双击时间用户可调，这里要读传入值而不是写死常量，
    /// 否则用户把双击时间调到 1 秒后行为就错了。
    #[test]
    fn 超过双击间隔不吞() {
        let _serial = serial();
        reset();
        let t0 = Instant::now();
        let dbl = Duration::from_millis(500);
        assert!(!is_double_click_tail(100, 100, t0, dbl));
        assert!(!is_double_click_tail(
            100,
            100,
            t0 + Duration::from_millis(501),
            dbl
        ));
    }

    /// 位置差太远不算双击：在画面左边点一下、右边点一下很快，
    /// 不该被当成双击（那会吞掉一次本该生效的暂停切换）。
    #[test]
    fn 位置差太远不吞() {
        let _serial = serial();
        let dbl = Duration::from_millis(500);

        // 差一个像素以内：算双击的第二击，click 要被吞
        reset();
        let t0 = Instant::now();
        assert!(!is_double_click_tail(100, 100, t0, dbl));
        assert!(is_double_click_tail(
            100 + DBLCLICK_SLOP,
            100,
            t0 + Duration::from_millis(50),
            dbl
        ));

        // 差一个像素以外：两次都是普通单击。
        // 这里必须 reset：上一次调用已经把基准点推进到
        // (100 + DBLCLICK_SLOP, 100)，拿它当基准就成了「差 1px」
        reset();
        let t1 = Instant::now();
        assert!(!is_double_click_tail(100, 100, t1, dbl));
        assert!(!is_double_click_tail(
            100 + DBLCLICK_SLOP + 1,
            100,
            t1 + Duration::from_millis(50),
            dbl
        ));

        // y 方向同样要判：画面上下两端各点一下不是双击
        reset();
        let t2 = Instant::now();
        assert!(!is_double_click_tail(100, 100, t2, dbl));
        assert!(!is_double_click_tail(
            100,
            100 + DBLCLICK_SLOP + 1,
            t2 + Duration::from_millis(50),
            dbl
        ));
    }

    /// 吞掉的那次也要刷新时间戳。
    ///
    /// 不刷的话第三次快速点击会跟「双击的第一击」配对，于是连点三下变成
    /// 「click + 两次被吞」，第三下的暂停切换就丢了。
    #[test]
    fn 吞掉之后时间戳要刷新() {
        let _serial = serial();
        // 双击窗口取 100ms，三个时间点靠它分界：
        //   A@0ms    第一次点击，不吞
        //   B@60ms   距 A 60ms <= 100ms，吞
        //   C@110ms  距 B 50ms（该吞）但距 A 110ms（不该吞）
        // 所以 C 返回 true 就证明基准点推进到了 B；若 C 仍拿 A 当基准，
        // 110 > 100 会返回 false，第三下的暂停切换就丢了。
        let narrow = Duration::from_millis(100);
        reset();
        let t0 = Instant::now();
        assert!(!is_double_click_tail(100, 100, t0, narrow), "A 不该被吞");
        assert!(
            is_double_click_tail(100, 100, t0 + Duration::from_millis(60), narrow),
            "B 该被吞"
        );
        assert!(
            is_double_click_tail(100, 100, t0 + Duration::from_millis(110), narrow),
            "基准点没推进到被吞掉的 B，C 被误判成普通单击"
        );

        // 反过来，连点之后总要有停下来的时候：隔够双击窗口的第一次点击
        // 必须正常发出去，不能一直被吞
        assert!(
            !is_double_click_tail(100, 100, t0 + Duration::from_millis(500), narrow),
            "间隔超过双击窗口后还在吞 click"
        );
    }
}
