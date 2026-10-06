//! 应用外壳：主窗口、消息循环、界面状态与交互。
//!
//! 分工：
//!
//! * `app`（本模块）：窗口创建、消息分发、界面状态、定时刷新
//! * `surface`：mpv 的画面子窗口（纯 Win32，见该模块注释）
//! * `ui`：控制栏的布局 / 绘制 / 命中测试，不持有任何状态
//!
//! ## DPI
//!
//! 启动第一个动作就是打开 per-monitor DPI v2 感知，且必须在创建任何窗口之前：
//! 进程一旦创建了窗口，DPI 感知模式就固定了。顺序写错的表现是整个窗口
//! （包括所有文字）被系统按系统 DPI 拉伸成位图，边缘发虚——这正是
//! 「放大之后字体不清晰」的成因。
//!
//! 之后所有 Win32 坐标都是物理像素。`ui` 里按 DPI 做 DIP->像素换算，字体按
//! 同一 DPI 重建，系统缩放是 100% / 125% / 150% / 200% 都不用额外配置。

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::crashlog;
use crate::diag::{self, Diagnostics};
use crate::gpu;
use crate::lang;
use crate::settings::{self, Settings};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    GetLastError, GlobalFree, ERROR_CLASS_ALREADY_EXISTS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT,
    POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EndPaint,
    InvalidateRect, ScreenToClient, SelectObject, HBITMAP, HDC, HGDIOBJ, PAINTSTRUCT,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwareness, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, PROCESS_PER_MONITOR_DPI_AWARE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_0, VK_1, VK_2, VK_3, VK_4, VK_5, VK_6,
    VK_7, VK_8, VK_9, VK_C, VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE, VK_F, VK_F11, VK_HOME, VK_I,
    VK_LEFT, VK_M, VK_O, VK_OEM_2, VK_OEM_4, VK_OEM_5, VK_OEM_COMMA, VK_OEM_PERIOD, VK_RIGHT, VK_S,
    VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetMessageW, GetWindowLongPtrW, GetWindowRect, KillTimer, LoadCursorW,
    LoadIconW, MessageBoxW, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassExW,
    SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA,
    GWL_EXSTYLE, GWL_STYLE, HTCLIENT, HTTRANSPARENT, HWND_TOP, IDC_ARROW, IDC_HAND, MB_ICONERROR,
    MB_ICONINFORMATION, MB_OK, MESSAGEBOX_STYLE, MINMAXINFO, MSG, PM_REMOVE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_SHOW, WINDOW_EX_STYLE, WM_CAPTURECHANGED,
    WM_CLOSE, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
    WM_SETCURSOR, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_POPUP,
    WS_VISIBLE,
};

use crate::mpv::{InitOptions, MpvEventMessage, MpvPlayer};
use crate::surface;
use crate::ui::{self, Hit, Layout, PaintState, Panel};

/// 主窗口类名。
const APP_CLASS: &str = "VideoViewMain";

/// 首次显示时的客户区尺寸（DIP）。
///
/// 值在 `settings::Settings::default()` 里，**不在这里**再写一遍。
/// 窗口初始尺寸与「没有保存过设置时用多大」是同一件事的两个来源，
/// 各写一份的必然结果是某次只改了一处，然后「第一次启动的窗口」和
/// 「恢复设置的窗口」不一样大。
const MIN_CLIENT_W_DIP: i32 = 480;
const MIN_CLIENT_H_DIP: i32 = 320;

/// 进度刷新间隔（毫秒）。
///
/// 旧版让 mpv 每帧推一次 `time-pos` 事件过来：60fps 的视频就是每秒 60 次
/// 跨进程事件 + 60 次 JSON 序列化 + 60 次网页 DOM 更新。改成界面线程按固定
/// 节奏主动读，事件流量与帧率解耦。
const TICK_MS: u32 = 250;

/// 定时器 id。
const TIMER_TICK: usize = 1;

/// 自定义消息起点。`WM_APP` 是 0x8000，避开所有标准消息。
const WM_APP_USER: u32 = 0x8000;
/// mpv 事件线程投递的队列有新消息。
const WM_MPV_EVENT: u32 = WM_APP_USER + 1;
/// 画面被单击。
const WM_VIDEO_CLICK: u32 = WM_APP_USER + 2;
/// 画面被双击。
const WM_VIDEO_DBLCLICK: u32 = WM_APP_USER + 3;
/// 弹一个待处理的错误提示框。
///
/// 错误一律**排队**再 PostMessage 过来，而不是就地 MessageBoxW：
/// `MessageBoxW` 自带消息循环，会重入 `app_wnd_proc`，那里要再取一次
/// `&mut App`。就地弹的话，同一个 `App` 上会同时存在两个 `&mut`（UB），
/// 而且对话框期间派发的 `WM_TIMER` 会改状态，回到原函数后继续用被改过的值。
const WM_APP_ERROR: u32 = WM_APP_USER + 4;
/// 打开文件选择对话框。
///
/// 同样是为了不在持有 `&mut App` 时进入模态循环。rfd 的对话框同样会重入。
const WM_APP_OPEN_DIALOG: u32 = WM_APP_USER + 5;

/// 方向键 / 音量键的单次步长。
const SEEK_STEP: f64 = 10.0;
const VOLUME_STEP: f64 = 5.0;

/// `[` / `]` 每次把倍速乘/除这个数。
///
/// 乘而不是加减：倍速是乘性感知（0.5 → 1.0 → 2.0），
/// 加减法在 1.0 附近会给出 1.05 这种没人要的速度。
const SPEED_FACTOR: f64 = 1.25;

/// 嵌入 exe 的图标资源 id，与 assets/app.rc 里的 `IDI_APP_ICON` 对应。
///
/// 传给 `LoadIconW` 的「字符串」其实是把 id 直接当指针传（MAKEINTRESOURCE），
/// windows crate 没有提供 `MAKEINTRESOURCEW`，所以这里手工构造。
const APP_ICON_ID: u16 = 1;

/// 界面状态。只在主线程读写。
struct UiState {
    position: f64,
    duration: f64,
    paused: bool,
    volume: f64,
    muted: bool,
    title: String,
    /// 已经打开过文件
    has_file: bool,
    /// mpv 报 `idle-active`：当前没有正在播放的文件
    idle: bool,
    /// 当前倍速（`speed` 属性）。0.3.0 没有这个状态，0.4.0 加 `[` `]` 时引入。
    speed: f64,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            position: 0.0,
            duration: 0.0,
            // 打开文件后默认播放，所以初始不是暂停态
            paused: false,
            volume: 100.0,
            muted: false,
            title: String::new(),
            has_file: false,
            idle: true,
            speed: 1.0,
        }
    }
}

/// 控制栏上方那块区域显示什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelMode {
    /// 不显示
    None,
    /// 解码诊断
    Stats,
    /// 快捷键总览
    Help,
}

impl PanelMode {
    /// 显示这个面板时会占几行。`Layout` 靠它算高度。
    ///
    /// 两种语言下都取**编译期常量**的行数，不去 `Vec::len()`：布局在
    /// 绘制之前算好，行数必须是能免费拿到的常量，否则每帧一次堆分配。
    fn rows(self) -> usize {
        match self {
            PanelMode::None => 0,
            PanelMode::Stats => lang::DIAG_ROWS,
            PanelMode::Help => lang::HELP_ROWS,
        }
    }

    /// 切换到 `self` 之外的模式（再按一次同一个键就收起）。
    fn toggled_to(self, target: PanelMode) -> PanelMode {
        if self == target {
            PanelMode::None
        } else {
            target
        }
    }
}

/// 正在拖动的控件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dragging {
    None,
    Seek,
    Volume,
}

/// 退出全屏时要还原的窗口状态。
#[derive(Debug, Clone, Copy)]
struct SavedWindow {
    style: isize,
    ex_style: isize,
    rect: RECT,
}

/// 持久后备位图。
///
/// 一块和客户区等大的内存位图，`WM_PAINT` 里画它、再把脏区拷到屏幕。
///
/// 之所以常驻而不是每次新建：控制栏每 250ms 就要重画一次（进度条），
/// 而 1100x720 逻辑尺寸在 125% 缩放下是 1375x900 物理像素，32 位色约 4.9MB。
/// 每次重画都 alloc + memcpy + free 这 4.9MB，光这一项就能把内存占满。
///
/// 位图坐标系 = 客户区坐标系，所以 `ui::paint` 里的坐标可以直接用，
/// `BitBlt` 的源点和目标点用同一组客户区坐标即可。
///
/// 注意这个 DC 是**常驻**的：它带着上一次绘制留下的裁剪区活到下一次。
/// `ui::paint` 每次进入都会先 `SelectClipRgn(hdc, None)` 把裁剪区清回整块
/// 位图再按脏区设新的（`IntersectClipRect` 是与现有裁剪区取交，
/// 不清就会一轮轮收窄），这条约定写在 `ui::paint` 里，改那边要一起看。
struct BackBuffer {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl BackBuffer {
    /// 尺寸没变就复用。变了就重建（窗口缩放 / 跨屏 DPI 变化都会走到这里）。
    ///
    /// 返回位图是否可用。不可用时 `paint` 只画不贴——贴一张没选进 DC 的位图，
    /// 表现是界面错乱而不是干脆不动，那种 bug 比直接不画难查得多。
    fn ensure(&mut self, hdc: HDC, w: i32, h: i32) -> bool {
        // 最小化时客户区是 0x0，`CreateCompatibleBitmap(0, 0)` 必然失败
        if w <= 0 || h <= 0 {
            return false;
        }
        if self.w == w && self.h == h && !self.bitmap.is_invalid() {
            return true;
        }
        self.release();
        unsafe {
            let dc = CreateCompatibleDC(Some(hdc));
            if dc.is_invalid() {
                return false;
            }
            let bitmap = CreateCompatibleBitmap(hdc, w, h);
            if bitmap.is_invalid() {
                // DC 已经建出来了，必须在这里放掉：`release` 只在位图有效时
                // 才 DeleteDC，留到下一轮 ensure 就等于每次 resize 泄一个 DC
                let _ = DeleteDC(dc);
                return false;
            }
            let old = SelectObject(dc, bitmap.into());
            if old.is_invalid() {
                // 选不进 DC 的位图，删掉；此时它没被任何 DC 持有，删是安全的
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(dc);
                return false;
            }
            self.dc = dc;
            self.bitmap = bitmap;
            self.old = old;
        }
        self.w = w;
        self.h = h;
        true
    }

    /// 释放 GDI 资源并复位成「未分配」状态。
    ///
    /// 这里逐字段清空而不是 `*self = BackBuffer::default()`：`BackBuffer`
    /// 实现了 `Drop`，整体赋值会先 drop 掉旧值，而 drop 又会调回 `release`，
    /// 于是无限递归直到爆栈。
    ///
    /// `dc` 与 `bitmap` 分别判断：位图建失败时 dc 是有效的，只有位图有效
    /// 才一起清的写法会漏掉那个 dc。
    fn release(&mut self) {
        unsafe {
            if !self.bitmap.is_invalid() {
                if !self.dc.is_invalid() && !self.old.is_invalid() {
                    let _ = SelectObject(self.dc, self.old);
                }
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
        self.dc = HDC::default();
        self.bitmap = HBITMAP::default();
        self.old = HGDIOBJ::default();
        self.w = 0;
        self.h = 0;
    }
}

impl Drop for BackBuffer {
    fn drop(&mut self) {
        self.release();
    }
}

/// 整个应用的状态。
struct App {
    hwnd: HWND,
    video: HWND,
    /// `WM_CREATE` 失败的原因。
    ///
    /// 原本是直接在 `WM_CREATE` 里弹 `MessageBoxW`，但那是在
    /// `CreateWindowExW` 尚未返回时嵌套跑一个模态消息循环（见该分支的
    /// 注释）。改成存下来、由 `create_and_loop` 在窗口创建彻底结束后再弹。
    create_error: Option<String>,
    /// 播放核心。窗口先建（WM_CREATE 里造画面窗口），mpv 再建，
    /// 因为 mpv 的 `wid` 就是画面窗口的句柄。
    player: Option<Arc<MpvPlayer>>,
    /// mpv 事件线程投递的消息队列。
    ///
    /// 事件线程不碰界面状态（那些数据属于主线程），只往队列里塞消息并
    /// PostMessage 一个自定义消息唤醒主线程去取。
    events: Arc<Mutex<Vec<MpvEventMessage>>>,
    state: UiState,
    dragging: Dragging,
    /// 拖动时的鼠标 x，用于把位置换算成进度 / 音量
    drag_x: i32,
    /// 影院模式：隐藏控制栏，画面铺满窗口
    theatre: bool,
    fullscreen: bool,
    saved: Option<SavedWindow>,
    /// 字体 + 画刷缓存。整体在 DPI 变化时重建。
    theme: ui::Theme,
    /// 当前语言的界面文案。构造时确定一次，之后整个进程不变。
    strings: lang::Strings,
    dpi: u32,
    /// 鼠标停在哪个可点击控件上，用于高亮
    hover: Hit,
    /// 客户区尺寸（物理像素）
    client_w: i32,
    client_h: i32,
    layout: Layout,
    back: BackBuffer,
    /// 排队等着弹的错误提示，`WM_APP_ERROR` 时一次弹完。
    ///
    /// 不就地 `MessageBoxW` 是因为它自带消息循环，会重入窗口过程再取一次
    /// `&mut App`；两个同时存活的独占引用是 UB，而且对话框期间派发的
    /// `WM_TIMER` 会改状态，回来后原函数继续用被改过的值。
    pending_errors: Vec<(String, String, MESSAGEBOX_STYLE)>,
    /// 有人请求打开文件对话框，真正的模态调用放到 `WM_APP_OPEN_DIALOG` 里做。
    open_dialog: bool,
    /// 解码诊断数据。`hwdec-current` 等观测项收在这里。
    ///
    /// 0.3.0 里 `hwdec-current` 落在 `UiState` 上但**从不显示**——接了一半的
    /// 死路。0.4.0 把它连同其余观测项一起搬进 `diag::Diagnostics`，
    /// 并且真的画出来。
    diag: Diagnostics,
    /// 控制栏上方那块区域显示什么。
    panel: PanelMode,
    /// 诊断面板的行数据。缓存下来是因为 `Diagnostics::rows` 每帧要
    /// `format!` 七个字符串，而面板只在打开时才需要它们。
    panel_rows: Vec<(String, String)>,
    /// 快捷键总览的行数据。同样缓存：`help_rows()` 返回的是**值**，
    /// 而 `PaintState` 里放的是引用，每帧重新构造一个临时数组再借出去
    /// 是过不了的（返回引用局部临时值）。
    help_rows: [(&'static str, &'static str); lang::HELP_ROWS],
    /// 用户设置。启动时读一次，之后改动只置 `prefs_dirty`。
    prefs: Settings,
    prefs_dirty: bool,
    /// 上次落盘的时刻，用于节流（拖动音量不该写几十次注册表）。
    prefs_last_flush: std::time::Instant,
}

/// 排队等弹的错误提示上限。
///
/// 循环报错时（比如解码失败 mpv 每秒抛一次事件）队列不能无限长，
/// 否则用户还没点掉第一个框，内存就先被几十条一模一样的消息吃掉了。
const MAX_QUEUED_ERRORS: usize = 8;

impl App {
    /// `prefs` 由调用方读好后传进来——不在这里读，避免有两个来源。
    fn new(prefs: Settings) -> Self {
        // 语言先定下来：控件宽度是按当前语言的文案**实测**出来的，
        // 所以 Theme 必须在 strings 之后建，顺序不能反。
        let strings = lang::Strings::new(lang::Lang::detect());
        // DPI 真正生效后 WM_CREATE 会立刻重建字体，这里先用 96 占位
        let theme = ui::Theme::new(96, &strings);
        // 面板**不从设置里恢复**，见 settings 模块的说明：它是临时视图，
        // 记住它会让每次启动画面区都矮一截
        let panel = PanelMode::None;
        let layout = Layout::new(0, 0, 96, false, panel.rows(), &theme.metrics);
        // 必须在 struct 字面量之外取：`strings` 会被 move 进字段，之后再借它
        // 就是「use after move」
        let help_rows = strings.help_rows();
        Self {
            hwnd: HWND::default(),
            video: HWND::default(),
            create_error: None,
            player: None,
            events: Arc::new(Mutex::new(Vec::new())),
            // 音量与静音从设置里取。初始值与 `UiState::default()` 一致，
            // 所以「设置生效」不会表现为第一次启动音量突然变了一个值
            state: UiState {
                volume: prefs.volume,
                muted: prefs.muted,
                ..UiState::default()
            },
            dragging: Dragging::None,
            drag_x: 0,
            theatre: false,
            fullscreen: false,
            saved: None,
            strings,
            theme,
            dpi: 96,
            hover: Hit::None,
            client_w: 0,
            client_h: 0,
            layout,
            back: BackBuffer {
                dc: HDC::default(),
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                w: 0,
                h: 0,
            },
            pending_errors: Vec::new(),
            open_dialog: false,
            diag: Diagnostics::default(),
            panel,
            panel_rows: Vec::new(),
            help_rows,
            prefs,
            prefs_dirty: false,
            prefs_last_flush: std::time::Instant::now(),
        }
    }

    /// 播放核心。窗口创建完成之后才存在。
    ///
    /// # Panics
    /// 窗口创建流程走完之前调用会 panic；这个不变量由 `create_and_loop`
    /// 的语句顺序保证，不是运行期可能出现的情况。
    fn player(&self) -> &Arc<MpvPlayer> {
        self.player
            .as_ref()
            .expect("播放核心尚未初始化：窗口创建流程有问题")
    }

    /// 按当前状态重算布局，并同步给画面窗口。
    ///
    /// 窗口缩放、DPI 变化、影院 / 全屏切换都走这一条路径，避免各处各算一遍
    /// 算出互相不一致的结果。
    fn relayout(&mut self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        self.client_w = (rc.right - rc.left).max(0);
        self.client_h = (rc.bottom - rc.top).max(0);

        // 字体必须跟着 DPI 重建，否则 GDI 会拉伸上一档 DPI 的字形。
        // 这就是「放大之后字发虚」的第二个来源。
        // 语言也在这里比对：换语言后文案宽度全变，字体与布局都得重算。
        let dpi = ui::dpi_for_window(self.hwnd);
        if dpi != self.dpi || !self.theme.matches(dpi, self.strings.lang) {
            self.dpi = dpi;
            self.theme = ui::Theme::new(dpi, &self.strings);
        }

        self.layout = Layout::new(
            self.client_w,
            self.client_h,
            self.dpi,
            self.theatre,
            self.panel.rows(),
            &self.theme.metrics,
        );

        // 画面子窗口的下沿是 **`panel.top` 而不是 `controls.top`**。
        //
        // 没有面板时两者相等（`panel` 是零高度、贴在控制栏顶边上），
        // 所以这条对旧行为没有影响。有面板时必须用 `panel.top`——mpv 的
        // 画面是**原生子窗口**，它盖在主窗口的一切绘制之上，边界给到
        // `controls.top` 就等于让它伸到面板底下把面板整块遮住。
        let video_rect = RECT {
            left: 0,
            top: 0,
            right: self.client_w,
            bottom: self.layout.panel.top.max(0),
        };
        surface::set_video_bounds(self.video, &video_rect);
    }

    /// 只重画底部那条：控制栏（以及它上面的面板）。
    ///
    /// 用 `chrome` 而不是 `controls`：面板打开时数据每 250ms 变一次，
    /// 只失效控制栏的话面板会一直是打开那一刻的样子。`chrome` 就是
    /// 「面板 + 控制栏」，仍然只是底部一条，代价与原来一样。
    fn invalidate_controls(&self) {
        if self.theatre {
            return;
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&self.layout.chrome), false);
        }
    }

    /// 切换面板。
    ///
    /// 面板高度变了，所以必须 `relayout` + 整窗重画：只重画控制栏的话，
    /// 新露出来的那一块面板区域永远不会被画（后备位图里是旧内容）。
    ///
    /// **影院模式下什么都不做。** 影院模式的定义就是「什么都没有」，
    /// 面板在那里是看不见的（`Layout::new` 里 `theatre` 时面板高度为 0）。
    /// 如果这里仍然改状态，用户在影院模式按了 `I`、什么都没看到，
    /// 退出影院后面板却莫名其妙冒出来了——按了没反应、过一会儿自己出现，
    /// 比「按了确实没反应」难解释得多。
    fn set_panel(&mut self, mode: PanelMode) {
        if self.theatre {
            return;
        }
        self.panel = mode;
        // 诊断面板的行数据每帧都要 `format!`，只在面板打开时重建
        self.panel_rows = match mode {
            PanelMode::Stats => self.diag.rows(&self.strings),
            _ => Vec::new(),
        };
        self.prefs_touch();
        self.relayout();
        self.invalidate_all();
    }

    /// 面板内容，供 `paint` 用。
    /// 设置有改动，标脏等落盘。
    fn prefs_touch(&mut self) {
        self.prefs.volume = self.state.volume;
        self.prefs.muted = self.state.muted;
        self.prefs_dirty = true;
    }

    /// 把设置写回注册表（节流）。
    ///
    /// 拖动音量滑块会连着改几十次 `volume`，每 250ms 写一次注册表没有必要。
    /// 这里按 `FLUSH_INTERVAL_MS` 节流，退出时再无条件落一次。
    fn prefs_flush(&mut self, force: bool) {
        if !self.prefs_dirty {
            return;
        }
        if !force && self.prefs_last_flush.elapsed().as_millis() < settings::FLUSH_INTERVAL_MS {
            return;
        }
        // 窗口尺寸存的是 **DIP**：用户把窗口从 125% 的屏拖到 200% 的屏，
        // 物理像素会翻倍而用户感知到的尺寸没变。存物理像素会让下次启动
        // 在新屏上变成一个巨大的窗口。
        //
        // 全屏 / 影院 / 最小化时 `client_w/h` 是 0 或整个屏幕，那不是
        // 「用户想要的窗口大小」，这时不记。
        if !self.fullscreen && !self.theatre && self.client_w > 0 && self.client_h > 0 {
            let dpi = self.dpi.max(1) as i64;
            self.prefs.client_w = ((i64::from(self.client_w) * 96) / dpi) as i32;
            self.prefs.client_h = ((i64::from(self.client_h) * 96) / dpi) as i32;
        }

        // 存不上不是错误：设置只是偏好，让用户下次重新设一次就行，
        // 反复弹框只会变成噪声。所以两种结果都清掉 dirty。
        let _ = self.prefs.save();
        self.prefs_dirty = false;
        self.prefs_last_flush = std::time::Instant::now();
    }

    /// 整窗重画。影院 / 全屏切换、空闲态切换时用。
    fn invalidate_all(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// 打开一个文件。路径校验由 `load_file` 做。
    fn open(&mut self, path: &Path) {
        // 换文件时先把界面归位，否则会短暂显示上一段视频的时间与时长
        self.state.has_file = true;
        self.state.idle = false;
        self.state.position = 0.0;
        self.state.duration = 0.0;
        self.state.speed = 1.0;
        self.state.title = file_title(path);
        // 诊断数据必须清。不清的话上一段视频的分辨率和丢帧数会留在面板上，
        // 看起来像新文件也丢了 10 帧——那是误导（丢帧计数 mpv 自己会重置，
        // 但解码器建好之前读到的还是旧文件的值）。
        self.diag.reset();
        self.refresh_panel_rows();
        surface::set_video_visible(self.video, true);
        self.relayout();
        self.invalidate_all();

        if let Err(e) = self.player().load_file(path) {
            self.report_error(self.strings.err_open, &e);
            // 加载失败要退回空闲态，否则界面永远停在「已加载但没在播」的样子：
            // 文件名显示着、播放/停止按钮是亮的（点了也只是又弹一次同样的错）、
            // 而画面区没有 mpv 子窗口盖着——`App::paint` 会按 `has_file` 跳过
            // 视频区底色，于是那块露出后备位图里的旧内容，看起来就是
            // 「打开失败的视频」而不是「打开失败」。
            self.state.has_file = false;
            self.state.idle = true;
            self.state.title.clear();
            self.state.position = 0.0;
            self.state.duration = 0.0;
            self.diag.reset();
            self.refresh_panel_rows();
            surface::set_video_visible(self.video, false);
            self.relayout();
            self.invalidate_all();
        }
    }

    /// 诊断面板打开时重建行数据。
    ///
    /// 只在两种时候需要：面板刚打开、以及诊断数据刚变。`tick` 里刷新数据
    /// 之后也走这里。
    fn refresh_panel_rows(&mut self) {
        if self.panel == PanelMode::Stats {
            self.panel_rows = self.diag.rows(&self.strings);
        }
    }

    /// 播放 / 暂停切换。没有文件时先弹文件对话框。
    fn toggle_pause(&mut self) {
        if !self.state.has_file {
            self.pick_file();
            return;
        }
        if let Err(e) = self.player().toggle_pause() {
            self.report_error(self.strings.err_toggle, &e);
        }
        // 立即更新按钮文字，不等 mpv 事件绕一圈
        self.state.paused = !self.state.paused;
        self.invalidate_controls();
    }

    fn stop(&mut self) {
        if !self.state.has_file {
            return;
        }
        if let Err(e) = self.player().stop() {
            self.report_error(self.strings.err_stop, &e);
        }
        self.state.has_file = false;
        self.state.idle = true;
        self.state.position = 0.0;
        self.state.duration = 0.0;
        self.state.title.clear();
        // 画面窗口藏起来，露出欢迎提示
        surface::set_video_visible(self.video, false);
        self.invalidate_all();
    }

    /// 跳转到指定秒数。
    fn seek_absolute(&mut self, seconds: f64) {
        let target = seconds.clamp(0.0, self.state.duration.max(0.0));
        if let Err(e) = self.player().seek(target) {
            self.report_error(self.strings.err_seek, &e);
        }
        self.state.position = target;
        self.invalidate_controls();
    }

    fn set_volume(&mut self, volume: f64) {
        let v = volume.clamp(0.0, 100.0);
        if let Err(e) = self.player().set_volume(v) {
            self.report_error(self.strings.err_volume, &e);
        }
        self.state.volume = v;
        self.prefs_touch();
        self.invalidate_controls();
    }

    fn toggle_mute(&mut self) {
        let next = !self.state.muted;
        if let Err(e) = self.player().set_mute(next) {
            self.report_error(self.strings.err_mute, &e);
        }
        self.state.muted = next;
        self.prefs_touch();
        self.invalidate_controls();
    }

    /// 倍速乘一个系数（`[` 减速 / `]` 加速）。
    ///
    /// 乘而不是加减：倍速在感知上就是乘性的（0.5 / 1 / 2），
    /// 1.0 附近用加减法会给出 1.25 这种没人主动想要的速度。
    fn scale_speed(&mut self, factor: f64) {
        if !self.state.has_file {
            return;
        }
        let next = (self.state.speed * factor).clamp(0.0625, 16.0);
        if let Err(e) = self.player().set_speed(next) {
            self.report_error(self.strings.err_command, &e);
            return;
        }
        // 立即更新，不等 mpv 的属性事件绕一圈（和 toggle_pause 同一个理由）
        self.state.speed = next;
        self.refresh_panel_rows();
        self.invalidate_controls();
    }

    /// 逐帧步进。
    ///
    /// 逐帧之后自动暂停，否则按一下 `,` 画面只闪一帧就继续播了，
    /// 用户根本没看清。mpv 的 `frame-step` 自己不会暂停。
    fn step_frame(&mut self, forward: bool) {
        if !self.state.has_file {
            return;
        }
        if let Err(e) = self.player().frame_step(forward) {
            self.report_error(self.strings.err_command, &e);
            return;
        }
        if !self.state.paused {
            if let Err(e) = self.player().set_pause(true) {
                self.report_error(self.strings.err_toggle, &e);
            }
            self.state.paused = true;
        }
        self.invalidate_controls();
    }

    /// 跳到整个视频的百分之几（数字键 0–9）。
    ///
    /// 0 是开头、9 是 90%，**没有 100%** —— 与 mpv 自身的行为一致，
    /// 免得用户按完 9 期待看到结尾却停在倒数第二秒。
    fn seek_percent(&mut self, digit: u32) {
        if !self.state.has_file {
            return;
        }
        self.seek_absolute(self.state.duration * f64::from(digit) / 10.0);
    }

    /// 保存当前画面到图片目录。
    fn screenshot(&mut self) {
        if !self.state.has_file {
            return;
        }
        let Some(path) = self.screenshot_path() else {
            self.report_error(self.strings.err_screenshot, "no writable pictures folder");
            return;
        };
        if let Err(e) = self.player().screenshot_to_file(&path) {
            self.report_error(self.strings.err_screenshot, &e);
        }
    }

    /// 截图文件名：`<视频名>-videoview-<时间戳>.png`。
    ///
    /// 文件名里带视频名而不是只有时间戳：一次截十几张之后，时间戳根本
    /// 分不出哪张是哪张。
    fn screenshot_path(&self) -> Option<PathBuf> {
        let dir = crashlog::pictures_dir()?;
        let stem: String = self
            .state
            .title
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(48)
            .collect();
        let stem = if stem.is_empty() {
            "clip".to_string()
        } else {
            stem
        };
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Some(dir.join(format!("{stem}-videoview-{secs}.png")))
    }

    /// 生成诊断报告并复制到剪贴板。
    ///
    /// 这一步是整个诊断面板最实用的部分：面板只能看当前这一秒，而用户
    /// 报 bug 时能贴出来的只有一段文字。
    fn copy_report(&mut self) {
        let text = self.build_report();
        match copy_to_clipboard(self.hwnd, &text) {
            Ok(()) => self.report_info(self.strings.info_report_copied),
            Err(e) => self.report_error(self.strings.err_report, &e),
        }
    }

    /// 组出完整诊断报告（纯文本，键 = 值）。
    fn build_report(&self) -> String {
        let Some(player) = &self.player else {
            return "VideoView: player not initialized\n".to_string();
        };
        // 报告要反映「此刻」，所以先主动读一遍高频项再生成——
        // 面板是每 250ms 才刷新的，复制到剪贴板的可能是半秒前的数据。
        let mut d = self.diag.clone();
        d.refresh(player);
        let ctx = diag::ReportContext {
            app_version: env!("CARGO_PKG_VERSION"),
            mpv_api_version: player.api_version(),
            os: std::env::consts::OS,
            physical_cores: crate::cpu::physical_cores().unwrap_or(0),
            gpu: &gpu::summary(),
            gpu_inactive: gpu::inactive_count(),
            dpi: self.dpi,
            strings: &self.strings,
        };
        d.report(&ctx)
    }

    /// 影院模式：隐藏控制栏，画面占满整个客户区。
    fn toggle_theatre(&mut self) {
        self.theatre = !self.theatre;
        self.relayout();
        self.invalidate_all();
    }

    /// 请求打开文件对话框。
    ///
    /// 这里**不**直接调 rfd 的模态对话框，只置个标志再 PostMessage。
    /// 理由同 `report_error`：rfd 的对话框自带消息循环，会重入窗口过程，
    /// 而那时上层还持有一个 `&mut App`。真正开对话框的代码在
    /// `WM_APP_OPEN_DIALOG` 分支里，那里的借用已经结束了。
    fn pick_file(&mut self) {
        self.open_dialog = true;
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_APP_OPEN_DIALOG, WPARAM(0), LPARAM(0));
        }
    }

    /// 定时器：主动读一次播放进度，刷新诊断面板，落盘设置。
    fn tick(&mut self) {
        // 设置落盘要走完这一整段，所以放在最前面无条件做
        self.prefs_flush(false);

        // 拖动中以鼠标位置为准，不跟着播放进度跳
        if self.dragging == Dragging::Seek {
            self.refresh_stats_panel();
            return;
        }
        if !self.state.has_file {
            self.refresh_stats_panel();
            return;
        }
        let Ok(pos) = self.player().get_double_property("time-pos") else {
            return;
        };
        if (pos - self.state.position).abs() > 1e-3 {
            self.state.position = pos;
            self.invalidate_controls();
        }
        self.refresh_stats_panel();
    }

    /// 面板可见时刷新高频诊断项。
    ///
    /// **面板不可见时一次属性都不读**——这是「按需观测」的关键。诊断数据
    /// 全部来自 mpv 的属性查询，每次查询都要跨一次 FFI 边界并让 mpv 加锁；
    /// 每秒 4 次不疼，但完全没有必要在用户没打开面板时付这个钱。
    ///
    /// 快捷键面板是纯静态文案，不需要刷新。
    fn refresh_stats_panel(&mut self) {
        if self.panel != PanelMode::Stats {
            return;
        }
        // `self.diag.refresh(self.player())` 会同时可变借用 `self.diag` 与
        // 不可变借用整个 `self`（`player()` 是个 `&self` 方法），编译器直接
        // 拒绝。把 `Arc` 克隆一份，第二个借用就地结束。
        let player = Arc::clone(self.player());
        self.diag.refresh(&player);
        self.panel_rows = self.diag.rows(&self.strings);
        self.invalidate_controls();
    }

    /// 处理 mpv 事件线程投递过来的消息。
    fn drain_events(&mut self) {
        let batch = {
            let mut queue = self.events.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *queue)
        };
        let mut layout_dirty = false;
        for ev in batch {
            match ev {
                MpvEventMessage::Property(change) => match change.name {
                    "duration" => self.state.duration = change.value.as_number().unwrap_or(0.0),
                    "pause" => self.state.paused = change.value.as_flag().unwrap_or(false),
                    "volume" => self.state.volume = change.value.as_number().unwrap_or(100.0),
                    "mute" => self.state.muted = change.value.as_flag().unwrap_or(false),
                    "media-title" => {
                        // mpv 报的 media-title 就是文件名；不信任它做路径，
                        // 这里只用来显示
                        let title = change.value.as_text().unwrap_or_default();
                        if !title.is_empty() {
                            self.state.title = title.to_string();
                        }
                    }
                    // 播完 / 打开失败后 mpv 回到空闲态，这时画面窗口要藏起来
                    "idle-active" if change.value.as_flag().unwrap_or(false) => {
                        layout_dirty |= self.enter_idle();
                    }
                    // 硬件解码实际用的哪一个。`no` = 静默退回软解。
                    //
                    // 以及其余的解码观测项（分辨率、丢帧、零拷贝…）。
                    // 全部交给 `Diagnostics` 处理，它知道每个属性名对应哪个字段，
                    // 也知道「拿不到值」和「值是 0」是两回事。
                    "hwdec-current"
                    | "video-format"
                    | "video-params/pixelformat"
                    | "video-params/w"
                    | "video-params/h"
                    | "video-out-params/w"
                    | "video-out-params/h"
                    | "decoder-frame-drop-count"
                    | "frame-drop-count"
                    | "current-vo"
                    | "video-sync" => self.diag.apply(&change),
                    // `speed` 是我们自己在 `scale_speed` 里改的，但 mpv 也会
                    // 报变化（比如换了文件）。两边都要更新，否则面板上的倍速
                    // 和 `[` `]` 键的累积会越差越远。
                    "speed" => {
                        if let Some(v) = change.value.as_number() {
                            self.state.speed = v;
                        }
                    }
                    _ => {}
                },
                MpvEventMessage::EndFile => {
                    layout_dirty |= self.enter_idle();
                }
                MpvEventMessage::FileLoaded => {
                    self.state.has_file = true;
                    self.state.idle = false;
                    surface::set_video_visible(self.video, true);
                    layout_dirty = true;
                }
                MpvEventMessage::Error(msg) => {
                    layout_dirty |= self.enter_idle();
                    self.report_error(self.strings.err_playback, &msg);
                }
                // mpv 拒绝了一条命令。命令名拼错时这是**唯一**的信号——
                // `command_async` 本身返回成功，没有这个事件的话，按了键
                // 什么都不发生、也没有任何日志。
                MpvEventMessage::CommandError(msg) => {
                    self.report_error(self.strings.err_command, &msg);
                }
            }
        }
        if layout_dirty {
            self.relayout();
            self.invalidate_all();
        } else {
            self.invalidate_controls();
        }
    }

    /// 回到空闲态（没有正在播放的文件）。返回是否需要重新布局。
    fn enter_idle(&mut self) -> bool {
        let was_active = self.state.has_file || !self.state.idle;
        self.state.has_file = false;
        self.state.idle = true;
        self.state.position = 0.0;
        self.state.duration = 0.0;
        surface::set_video_visible(self.video, false);
        was_active
    }

    /// 重画脏区（双缓冲）。
    ///
    /// 位图常驻且与客户区等大，坐标系就是客户区坐标，所以这里不搬移原点，
    /// 只把 `dirty` 这一块拷到屏幕。
    ///
    /// 绘制本身也只画 `dirty`（见 `ui::paint` 的说明）。所以要保证
    /// 「脏区里的内容一定是最新的」：位图刚重建时（resize / 首次显示）
    /// 它是空白的，此时**必须整块画一遍**，否则屏幕上会是半黑半新。
    fn paint(&mut self, hdc: HDC, rc_paint: &RECT) {
        // ensure 会把 w/h 更新成新尺寸，所以重建与否要在它之前判
        let fresh = self.back.w != self.client_w || self.back.h != self.client_h;
        // 位图是新建的，就画整个客户区
        let mut dirty = if fresh {
            RECT {
                left: 0,
                top: 0,
                right: self.client_w,
                bottom: self.client_h,
            }
        } else {
            // 统一在这里夹到客户区内，后面绘制和 BitBlt 用的是同一个矩形。
            //
            // 这不是洁癖：两处各夹一次的话，理论上可以出现「绘制那边夹完是
            // 空矩形、于是一个像素没画；BitBlt 那边拿未夹的原坐标去贴」——
            // 结果是把后备位图里**别的位置**的旧内容糊到屏幕上，然后
            // `EndPaint` 把这块更新区清掉，那块从此不再重绘。
            // Windows 保证 `rcPaint` 不越界，所以今天触发不了；
            // 但两个消费者必须看同一个矩形，否则这条推理就靠"恰好不会发生"撑着。
            ui::clamp_to_client(rc_paint, self.client_w, self.client_h)
        };

        // 有文件在播时，视频区被 mpv 的子窗口整个盖住（`relayout` 里把它摆成
        // `0,0,client_w,controls.top`），那一块既不用我们画、也不能我们画——
        // 贴上去只会盖住正在播的画面。
        //
        // 这里把脏区裁到控制栏，`ui::paint` 里对应的视频区底色填充也随之跳过
        // （那边判的是同一个 `loaded`）。两边必须一致：只夹脏区不跳填充，
        // 视频区仍会被写；只跳填充不夹脏区，旧画面会被 BitBlt 贴回去。
        //
        // 省下来的不只是 CPU：4K 下不再每帧摸 8.3MB，那几十 MB 也就不会被
        // 算进 WorkingSet。对「4G 内存」这个目标，这是我们自己的代码里
        // 最大的一笔。
        if self.state.has_file {
            dirty = intersect_rect(dirty, &self.layout.chrome);
        }

        let w = dirty.right - dirty.left;
        let h = dirty.bottom - dirty.top;
        if w <= 0 || h <= 0 {
            return;
        }
        // ensure 返回 false = 位图不可用（最小化时客户区 0x0，或 GDI 分配失败）。
        // 这种情况下照样把控制栏画进后备位图——之后用户把窗口还原回来，
        // 位图里就是最新的内容，不需要等下一次 250ms 定时器才刷新。
        // 但**不能贴**：拿一张没选进 DC 的位图当 BitBlt 源，屏幕上会是错乱的
        // 图案，比干脆不动难查得多。
        let blittable = self.back.ensure(hdc, self.client_w, self.client_h);
        let mem = self.back.dc;
        if mem.is_invalid() {
            return;
        }
        let drag_seconds = self.drag_seconds();
        // 借用整个 `self.theme`（可变）与 `panel`（不可变）不能共存，所以
        // 面板那两个 slice 先用裸指针「借」出来，绘制完立刻还原。
        //
        // 这么做而不是把 PaintState 的字段拆开逐个构造，是因为 `ui::paint`
        // 一次性拿到全部状态才不会漏掉某个字段（漏了就是界面上少一块东西，
        // 而这种 bug 从签名上看不出来）。指针只在同一个 unsafe 块里用完，
        // 生命周期不会逃出去。
        let panel = match self.panel {
            PanelMode::Help => Some(Panel::Help(&self.help_rows)),
            PanelMode::Stats => Some(Panel::Stats(&self.panel_rows)),
            PanelMode::None => None,
        };
        unsafe {
            ui::paint(
                mem,
                self.client_w,
                self.client_h,
                &self.layout,
                &mut self.theme,
                &PaintState {
                    position: self.state.position,
                    duration: self.state.duration,
                    paused: self.state.paused,
                    volume: self.state.volume,
                    muted: self.state.muted,
                    loaded: self.state.has_file,
                    title: &self.state.title,
                    dragging_seek: self.dragging == Dragging::Seek,
                    drag_seconds,
                    theatre: self.theatre,
                    idle: self.state.idle,
                    hover: self.hover,
                    strings: &self.strings,
                    panel,
                },
                &dirty,
            );

            if blittable {
                let (dest_x, dest_y, src_x, src_y) = blit_points(&dirty);
                let _ = windows::Win32::Graphics::Gdi::BitBlt(
                    hdc,
                    dest_x,
                    dest_y,
                    w,
                    h,
                    Some(mem),
                    src_x,
                    src_y,
                    windows::Win32::Graphics::Gdi::SRCCOPY,
                );
                let _ = windows::Win32::Graphics::Gdi::GdiFlush();
            }
        }
    }

    /// 拖动进度条时，鼠标位置对应的秒数。
    fn drag_seconds(&self) -> f64 {
        self.layout.ratio(Hit::Seek, self.drag_x) * self.state.duration.max(0.0)
    }

    /// 排队一条错误提示，等 `WM_APP_ERROR` 来弹。
    ///
    /// 这里**不能**直接调 `message_box`：`MessageBoxW` 自带消息循环，会重入
    /// `app_wnd_proc`，那里要再取一次 `&mut App`。同一个 `App` 上同时存在两个
    /// `&mut` 是 UB（Stacked Borrows 层面），而且对话框期间派发的 `WM_TIMER`
    /// 会改 `App` 的状态，回到原函数后继续用被改过的值。
    ///
    /// 同一条消息可能被 Post 多次，所以由 `pending_errors` 兜住，弹的时候一次
    /// 弹完。取的是 `mem::take` 而不是 `&mut self`，因为这个方法要在已经持有
    /// `&mut App` 的调用链里被调到。
    fn report_error(&mut self, caption: &str, detail: &str) {
        self.queue_notice(caption, detail, MB_ICONERROR);
    }

    /// 纯信息提示（「已复制」这类成功反馈）。
    ///
    /// 与 `report_error` 走同一条排队通道，区别只在图标：挂红叉的框
    /// 会被理解成「出错了」，而「诊断信息已复制」是成功。
    fn report_info(&mut self, caption: &str) {
        self.queue_notice(caption, "", MB_ICONINFORMATION);
    }

    fn queue_notice(&mut self, caption: &str, detail: &str, icon: MESSAGEBOX_STYLE) {
        if self.pending_errors.len() >= MAX_QUEUED_ERRORS {
            // 循环报错（比如解码失败时每秒一次）时别把内存吃光：
            // 丢最早的那条，留最近的
            self.pending_errors.remove(0);
        }
        self.pending_errors
            .push((caption.to_string(), detail.to_string(), icon));
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_APP_ERROR, WPARAM(0), LPARAM(0));
        }
    }
}

// ---------------------------------------------------------------- 窗口过程

/// 双缓冲贴图的源点与目标点，返回 `(dest_x, dest_y, src_x, src_y)`。
///
/// 这对坐标就是「控制栏不刷新」的根因所在，所以抽成函数并用单测钉住约定：
///
/// * `BeginPaint` 返回的 DC 原点仍在**客户区 `(0,0)`**。更新区只是被裁剪，
///   没有被平移，所以目标点必须用脏区自己的客户区坐标。写成 `(0,0)`
///   会把控制栏贴到窗口顶部、被视频子窗口盖住，屏幕上看起来就是
///   「内部状态一直在变、界面却永远是第一次全量绘制的样子」。
/// * 后备位图与客户区同尺寸同坐标系，因此源点和目标点取同一组值。
///
/// 另一个曾经踩过的坑：想让绘制坐标跟着脏区走，可以用
/// `SetWindowOrgEx` 平移内存 DC 的原点，但它要的是**负**偏移
/// （客户区 `(0,0)` 落在位图的哪个像素），而 GDI 直接拒绝负值。
/// 与其绕开这个坑，不如让位图和客户区保持同一套坐标系。
fn blit_points(dirty: &RECT) -> (i32, i32, i32, i32) {
    (dirty.left, dirty.top, dirty.left, dirty.top)
}

/// 两个矩形的交集，没有交集时返回零面积矩形。
///
/// 返回 `right == left || bottom == top` 表示空，调用方现有的
/// `w <= 0 || h <= 0` 判断就会跳过绘制与 BitBlt——这和 `clamp_to_client`
/// 表达「这一帧没什么要画」的方式一致，不用在这里另立一套约定。
fn intersect_rect(a: RECT, b: &RECT) -> RECT {
    let left = a.left.max(b.left);
    let top = a.top.max(b.top);
    let right = a.right.min(b.right);
    let bottom = a.bottom.min(b.bottom);
    // 任一方向反了就归一成空矩形，而不是留下 left > right 的反向矩形
    if right <= left || bottom <= top {
        return RECT {
            left,
            top,
            right: left,
            bottom: top,
        };
    }
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

/// 全屏切换的窗口操作计划。
///
/// 单独一个结构体是为了能跨过 `SetWindowPos` 传递参数：它全是 Copy 数据，
/// 调用方在调 `SetWindowPos` 前后都不必持有 `&mut App`（见 `toggle_fullscreen`）。
#[derive(Debug, Clone, Copy)]
struct FullscreenPlan {
    style: isize,
    ex_style: isize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// 进全屏时保存原窗口信息，退出全屏时由调用方清空
    save: Option<SavedWindow>,
    /// 目标是进入还是退出全屏
    entering: bool,
}

/// 切换全屏，三段式，每段之间都不持有 `&mut App`。
///
/// 分段的原因：`SetWindowPos`（尤其带 `SWP_FRAMECHANGED` 时）会**同步**
/// 给本窗口派发 WM_NCCALCSIZE / WM_WINDOWPOSCHANGED / WM_SIZE，那串消息会
/// 重入 `app_wnd_proc`，里面会再取一次 `&mut App`。如果调用方在整个过程里
/// 一直持着那个借用，同一个 `App` 上就同时存在两个 `&mut`——按 Rust 的
/// 借用规则是 UB，而且重入期间改过的状态（比如新的客户区尺寸）可能被外层
/// 用旧假设覆盖回去。
fn toggle_fullscreen(app_ptr: *mut App) {
    let hwnd = unsafe { (*app_ptr).hwnd };
    let Some(plan) = plan_fullscreen(unsafe { &mut *app_ptr }) else {
        return;
    };

    apply_fullscreen(hwnd, plan);

    let app = unsafe { &mut *app_ptr };
    app.saved = plan.save;
    app.fullscreen = plan.entering;
    app.relayout();
    app.invalidate_all();
}

/// 全屏切换第一阶段：改窗口样式、算好目标矩形。不派发任何消息。
///
/// 返回 `None` 表示状态已经不一致（该退全屏但没有存档），调用方什么都不用做。
///
/// 用「去掉边框 + 铺满显示器」而不是切显示模式：切模式会改分辨率、影响别的
/// 程序，还要让 mpv 重建 vo，反而更容易出错。播放器场景下伪全屏的视觉结果
/// 与真全屏一致。
fn plan_fullscreen(app: &mut App) -> Option<FullscreenPlan> {
    unsafe {
        if app.fullscreen {
            let saved = app.saved?;
            let _ = SetWindowLongPtrW(app.hwnd, GWL_STYLE, saved.style);
            let _ = SetWindowLongPtrW(app.hwnd, GWL_EXSTYLE, saved.ex_style);
            Some(FullscreenPlan {
                style: saved.style,
                ex_style: saved.ex_style,
                x: saved.rect.left,
                y: saved.rect.top,
                w: saved.rect.right - saved.rect.left,
                h: saved.rect.bottom - saved.rect.top,
                save: None,
                entering: false,
            })
        } else {
            let mut wr = RECT::default();
            let _ = GetWindowRect(app.hwnd, &mut wr);
            let save = SavedWindow {
                style: GetWindowLongPtrW(app.hwnd, GWL_STYLE),
                ex_style: GetWindowLongPtrW(app.hwnd, GWL_EXSTYLE),
                rect: wr,
            };
            let style = (WS_POPUP.0 | WS_VISIBLE.0) as isize;
            let _ = SetWindowLongPtrW(app.hwnd, GWL_STYLE, style);
            let monitor = ui::monitor_rect(app.hwnd);
            Some(FullscreenPlan {
                style,
                ex_style: save.ex_style,
                x: monitor.left,
                y: monitor.top,
                w: monitor.right - monitor.left,
                h: monitor.bottom - monitor.top,
                save: Some(save),
                entering: true,
            })
        }
    }
}

/// 全屏切换第二阶段：真正调 `SetWindowPos`。
///
/// 刻意**不带** `&mut App`：它会重入窗口过程（见 `toggle_fullscreen` 的注释）。
fn apply_fullscreen(hwnd: HWND, plan: FullscreenPlan) {
    unsafe {
        let _ = plan.style;
        let _ = plan.ex_style;
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            plan.x,
            plan.y,
            plan.w,
            plan.h,
            SWP_FRAMECHANGED | SWP_SHOWWINDOW,
        );
    }
}

/// 主窗口过程。
///
/// `GWLP_USERDATA` 存 `*mut App`，在 `WM_NCCREATE` 挂上：这是
/// CreateWindowExW 最早会发回来的消息，比 WM_CREATE 早，而 WM_CREATE
/// 里要用到 App 的字段（客户区尺寸、DPI）。
///
/// # 借用约定
///
/// 整个函数**不在分支之前**建一个 `&mut App`。每个分支需要时就地
/// `&mut *app_ptr` 取一个短借用，用完立刻还回去。原因是有三类调用会
/// **同步重入**本函数：模态对话框（`MessageBoxW`、rfd）、`SetWindowPos`
/// （派发 WM_SIZE）、以及控件窗口自己发回来的消息。重入的那一层会再取一个
/// `&mut App`，两个同时存活的独占引用是 UB，而且外层会拿着被内层改过的
/// 状态继续算。凡是这类调用都在借用之外做（见 `toggle_fullscreen`、
/// `WM_APP_ERROR`、`WM_APP_OPEN_DIALOG` 三个分支）。
unsafe extern "system" fn app_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        let app_ptr = create.lpCreateParams as *mut App;
        if !app_ptr.is_null() {
            (*app_ptr).hwnd = hwnd;
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize);
        }
    }

    let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if app_ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    match msg {
        WM_CREATE => {
            DragAcceptFiles(hwnd, true);
            match surface::create_video_window(hwnd) {
                Ok(video) => {
                    let app = &mut *app_ptr;
                    app.video = video;
                    app.relayout();
                    // 空闲时先藏起来，让欢迎提示露出来
                    surface::set_video_visible(video, false);
                    // 注意：**这里不再 SetTimer**。定时器在
                    // `create_and_loop` 里、`app.player` 就位之后才武装，
                    // 否则「`player()` 还是 None 就收到 WM_TIMER」会撞上
                    // `.expect()` 那条 abort 路径。
                    app.invalidate_all();
                    let _ = SetFocus(Some(hwnd));
                    LRESULT(0)
                }
                Err(e) => {
                    // **不在这里弹 MessageBox。**
                    //
                    // `WM_CREATE` 是在 `CreateWindowExW` 尚未返回时派发出来的，
                    // 而 `MessageBoxW` 自带完整的模态循环 —— 也就是说它会在
                    // 「窗口正在被创建」的当中嵌套跑一圈消息派发。三个后果：
                    //
                    // 1. 用户在框上按 Esc / Alt+F4（`MB_OK` 的关闭按钮）→
                    //    `DestroyWindow` 在 `CreateWindowExW` 内部完成 →
                    //    然后 `WM_CREATE` 还返回 -1。Windows 对「你已经把它
                    //    销毁了、CreateWindowExW 又拿到 NULL」这个组合的
                    //    行为没有文档。
                    // 2. 嵌套循环里到达 `WM_SIZE` → `relayout()` →
                    //    `set_video_bounds`，而 `app.video` 此刻是
                    //    `HWND::default()`（`create_video_window` 刚失败）。
                    // 3. 嵌套循环里任何会碰 `App::player()` 的分支 →
                    //    `.expect("播放核心尚未初始化")` → `panic = "abort"`
                    //    下**整个进程直接消失**，连这句报错都看不到。
                    //
                    // 改成：把原因存进 `create_error`，`WM_CREATE` 返回 -1，
                    // 由 `create_and_loop` 在 `CreateWindowExW` 真正返回之后
                    // 再弹。那时机上窗口已定型、`player()` 不可达。
                    (*app_ptr).create_error = Some(e);
                    LRESULT(-1)
                }
            }
        }
        WM_ERASEBKGND => {
            // 整个客户区都由 WM_PAINT 画。返回 1 阻止系统先用背景色擦一遍，
            // 否则 resize / 拖窗口时能看到黑底闪烁。
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            (&mut *app_ptr).paint(hdc, &ps.rcPaint);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_SIZE => {
            (&mut *app_ptr).relayout();
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            // 限制最小尺寸，保证控制栏不会被挤到换行
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            let dpi = (*app_ptr).dpi.max(96);
            mmi.ptMinTrackSize = POINT {
                x: ui::dip_to_px(MIN_CLIENT_W_DIP, dpi),
                y: ui::dip_to_px(MIN_CLIENT_H_DIP, dpi),
            };
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // 系统已经算好了建议矩形（跨显示器移动时），照做即可。
            // SetWindowPos 在借用之外调：它会同步派发 WM_SIZE 回来。
            let suggested = *(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let app = &mut *app_ptr;
            app.relayout();
            app.invalidate_all();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_TICK => {
            (&mut *app_ptr).tick();
            LRESULT(0)
        }
        WM_MPV_EVENT => {
            (&mut *app_ptr).drain_events();
            LRESULT(0)
        }
        WM_VIDEO_CLICK => {
            (&mut *app_ptr).toggle_pause();
            LRESULT(0)
        }
        WM_VIDEO_DBLCLICK => {
            (&mut *app_ptr).toggle_theatre();
            LRESULT(0)
        }
        WM_APP_ERROR => {
            // MessageBoxW 自带消息循环，会重入本函数，所以这里是在**没有借用**
            // App 的状态下弹的（要点见文件头的「借用约定」）。
            //
            // 一次只弹一条：一条消息里可能攒了好几条（比如一次事件报了 3 个错），
            // 连着弹 3 个框用户没法说话。还有剩的就再 Post 一次，
            // 下一个消息循环迭代再弹——顺便也避免了「同一个错误无限弹框」
            // （一次 WM_APP_ERROR 里 while 循环清空队列，遇到循环报错时
            // 队列永远清不空，就变成用户点掉一个又弹一个）。
            if !(*app_ptr).pending_errors.is_empty() {
                let (caption, detail, icon) = (&mut *app_ptr).pending_errors.remove(0);
                message_box(hwnd, &caption, &detail, icon);
                if !(*app_ptr).pending_errors.is_empty() {
                    let _ = PostMessageW(Some(hwnd), WM_APP_ERROR, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_APP_OPEN_DIALOG => {
            // 同上：rfd 的对话框也是模态的，这里同样没有借用 App。
            // 标志先清再开框，避免对话框被重入时排队的消息重复开一个。
            if (*app_ptr).open_dialog {
                (*app_ptr).open_dialog = false;
                // 先取文案副本（Strings 是 Copy）：`pick_video_dialog` 之后要
                // 拿 `&mut App`，借用不能跨过模态调用
                let strings = (*app_ptr).strings;
                if let Some(path) = pick_video_dialog(&strings) {
                    (&mut *app_ptr).open(&path);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = point_from_lparam(lparam);
            let app = &mut *app_ptr;
            match app.dragging {
                Dragging::Seek => {
                    app.drag_x = x;
                    app.invalidate_controls();
                }
                Dragging::Volume => {
                    let ratio = app.layout.ratio(Hit::Volume, x);
                    // 拖动过程中只改界面，不下发 mpv：WM_MOUSEMOVE 的频率
                    // 能到 1kHz，每条都同步过一次 mpv_set_property 纯属浪费。
                    // 松手时 WM_LBUTTONUP 才把最终值发下去。
                    let v = (ratio * 100.0).clamp(0.0, 100.0);
                    app.state.volume = v;
                    app.invalidate_controls();
                }
                Dragging::None => {
                    let hit = app.layout.hit(x, y);
                    if hit != app.hover {
                        app.hover = hit;
                        app.invalidate_controls();
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // 点控制栏时把焦点抢回来：用户切到别的窗口再点回来，不抢焦点的话
            // 空格 / 方向键 / F11 全部失灵（键盘消息只发给有焦点的窗口）。
            let _ = SetFocus(Some(hwnd));
            let (x, y) = point_from_lparam(lparam);
            let app = &mut *app_ptr;
            match app.layout.hit(x, y) {
                Hit::Open => app.pick_file(),
                Hit::Play => app.toggle_pause(),
                Hit::Stop => app.stop(),
                Hit::Mute => app.toggle_mute(),
                Hit::Seek => {
                    if app.state.has_file && app.state.duration > 0.0 {
                        app.dragging = Dragging::Seek;
                        app.drag_x = x;
                        let _ = SetCapture(hwnd);
                    }
                }
                Hit::Volume => {
                    app.dragging = Dragging::Volume;
                    app.drag_x = x;
                    let ratio = app.layout.ratio(Hit::Volume, x);
                    app.set_volume(ratio * 100.0);
                    let _ = SetCapture(hwnd);
                }
                // 画面区域的点击由视频子窗口自己处理（见 surface.rs），
                // 主窗口这边不该收到
                Hit::Video | Hit::None => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let app = &mut *app_ptr;
            match app.dragging {
                Dragging::Seek => {
                    let seconds = app.drag_seconds();
                    app.dragging = Dragging::None;
                    app.seek_absolute(seconds);
                }
                Dragging::Volume => {
                    let v = app.state.volume;
                    app.dragging = Dragging::None;
                    // 拖动过程只改了界面，这里才真正下发（见 WM_MOUSEMOVE）
                    app.set_volume(v);
                }
                Dragging::None => {}
            }
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // 鼠标拖动途中捕获被别处抢走（任务栏介入、远程会话断开、
            // WM_CANCELMODE）。不处理的话 dragging 永远停在 Seek/Volume，
            // tick() 每 250ms 提前返回，进度条再也不刷新，而且界面没任何提示。
            if wparam.0 == 0 {
                let app = &mut *app_ptr;
                if app.dragging != Dragging::None {
                    let v = app.state.volume;
                    app.dragging = Dragging::None;
                    if v != app.player().get_volume().unwrap_or(v) {
                        let _ = app.player().set_volume(v);
                    }
                    app.invalidate_controls();
                }
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // 只有命中测试代码是「在客户区里」才自己定光标。
            // 非客户区（标题栏、边框、可缩放角）交回 DefWindowProcW，
            // 否则窗口边框上的双向缩放光标会被我们换成箭头，用户没法调大小。
            let hit_test = wparam.0 as u16;
            if hit_test != HTCLIENT as u16 && hit_test != HTTRANSPARENT as u16 {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            // lparam 里是屏幕坐标，要转成客户区坐标才能做命中测试
            let mut pt = POINT {
                x: (lparam.0 as i16) as i32,
                y: ((lparam.0 >> 16) as i16) as i32,
            };
            let _ = ScreenToClient(hwnd, &mut pt);
            let app = &mut *app_ptr;
            let hit = app.layout.hit(pt.x, pt.y);
            let idc = match hit {
                Hit::Open | Hit::Play | Hit::Stop | Hit::Mute => IDC_HAND,
                Hit::Seek | Hit::Volume => IDC_HAND,
                Hit::Video | Hit::None => IDC_ARROW,
            };
            if let Ok(cursor) = LoadCursorW(None, idc) {
                let _ = SetCursor(Some(cursor));
            }
            // 1 = 已处理，系统不要再设默认光标
            LRESULT(1)
        }
        WM_KEYDOWN => {
            if handle_key(app_ptr, wparam.0 as u16) {
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_DROPFILES => {
            handle_drop(app_ptr, HDROP(wparam.0 as *mut c_void));
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), TIMER_TICK);
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // 清掉指向 App 的指针。这一步不是可有可无的：
            // `create_and_loop` 里如果创建 mpv 失败会提前返回并 drop 掉
            // `Box<App>`，而窗口还活着、GWLP_USERDATA 还指着那块已释放内存，
            // 这时再来的任何消息（含 message_box 自己转的那一圈 WM_TIMER）
            // 都会解引用野指针。
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// `lparam` 里的打包坐标（LOWORD/HIWORD 各是有符号 16 位）。
fn point_from_lparam(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 as i16) as i32, ((lparam.0 >> 16) as i16) as i32)
}

/// 打开文件选择对话框。模态，调用点必须不持有 `&mut App`。
///
/// 单独成函数是为了让「模态」这件事在调用点一眼可见：调用它的地方要么在
/// 借用之外，要么就是 `WM_APP_OPEN_DIALOG` 那个借用已结束的分支。
///
/// 文案按当前语言取，所以要传 `&Strings`；拿 `App` 引用会与调用点的
/// `&mut App` 撞车（同 `handle_key` 的理由）。
fn pick_video_dialog(strings: &lang::Strings) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter(
            strings.dlg_filter_video,
            &[
                "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "mpg", "mpeg",
            ],
        )
        .add_filter(strings.dlg_filter_all, &["*"])
        .set_title(strings.dlg_title)
        .pick_file()
}

/// 键盘快捷键。返回 true 表示已处理。
///
/// 参数是裸指针而不是 `&mut App`：Esc/F11 分支会走 `toggle_fullscreen`，
/// 它内部要调 `SetWindowPos`，那会同步重入窗口过程。传引用的话，
/// 这个引用会一直活到 `handle_key` 返回，与重入那一层取到的 `&mut App` 撞车。
unsafe fn handle_key(app_ptr: *mut App, vk: u16) -> bool {
    let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
    let key = |k: u16| vk == k;
    match vk {
        _ if key(VK_SPACE.0) => {
            (&mut *app_ptr).toggle_pause();
            true
        }
        _ if key(VK_ESCAPE.0) => {
            // Esc 先退出影院模式，其次退出全屏
            let app = &mut *app_ptr;
            if app.theatre {
                app.toggle_theatre();
            } else if app.fullscreen {
                // 借用在这一句里就结束了：见 toggle_fullscreen 的注释
                toggle_fullscreen(app_ptr);
            }
            true
        }
        _ if key(VK_LEFT.0) => {
            let app = &mut *app_ptr;
            app.seek_absolute(app.state.position - SEEK_STEP);
            true
        }
        _ if key(VK_RIGHT.0) => {
            let app = &mut *app_ptr;
            app.seek_absolute(app.state.position + SEEK_STEP);
            true
        }
        _ if key(VK_UP.0) => {
            let app = &mut *app_ptr;
            app.set_volume(app.state.volume + VOLUME_STEP);
            true
        }
        _ if key(VK_DOWN.0) => {
            let app = &mut *app_ptr;
            app.set_volume(app.state.volume - VOLUME_STEP);
            true
        }
        _ if key(VK_HOME.0) => {
            (&mut *app_ptr).seek_absolute(0.0);
            true
        }
        _ if key(VK_END.0) => {
            let app = &mut *app_ptr;
            // 往回退一点点：直接跳到 duration 上有些播放器会停在最后一帧
            // 不动，看起来像「没跳过去」
            let target = (app.state.duration - 0.05).max(0.0);
            app.seek_absolute(target);
            true
        }
        _ if key(VK_F11.0) => {
            toggle_fullscreen(app_ptr);
            true
        }
        _ if key(VK_F.0) => {
            (&mut *app_ptr).toggle_theatre();
            true
        }
        _ if key(VK_O.0) && ctrl => {
            (&mut *app_ptr).pick_file();
            true
        }
        // Ctrl + C = 复制诊断报告。
        //
        // 面板只能看当前这一秒，而用户报 bug 时能贴出来的只有一段文字，
        // 所以这份报告才是整个诊断功能里最实用的部分，给它一个顺手的位置。
        // 界面上没有输入框，Ctrl+C 不与「复制选中文本」冲突。
        _ if key(VK_C.0) && ctrl => {
            (&mut *app_ptr).copy_report();
            true
        }
        _ if key(VK_M.0) => {
            (&mut *app_ptr).toggle_mute();
            true
        }
        _ if key(VK_S.0) => {
            (&mut *app_ptr).screenshot();
            true
        }
        // `I` = 解码诊断。再按一次收起
        _ if key(VK_I.0) => {
            let app = &mut *app_ptr;
            app.set_panel(app.panel.toggled_to(PanelMode::Stats));
            true
        }
        // `?` = 快捷键总览。在美式布局上是 Shift + `/`，
        // 虚拟键码与 `/` 相同（VK_OEM_2），所以不需要单独判 Shift
        _ if key(VK_OEM_2.0) => {
            let app = &mut *app_ptr;
            app.set_panel(app.panel.toggled_to(PanelMode::Help));
            true
        }
        // `[` 减速 / `]` 加速（VK_OEM_4 / VK_OEM_5）
        //
        // 这两个键的虚拟键码**依赖键盘布局**：德语布局上它们是 ä/ö，
        // 法语布局上位置又不一样。所以快捷键面板里只能写符号、不能写
        // 键名——在非美式布局上按出来的是别的字符。
        _ if key(VK_OEM_4.0) => {
            (&mut *app_ptr).scale_speed(1.0 / SPEED_FACTOR);
            true
        }
        _ if key(VK_OEM_5.0) => {
            (&mut *app_ptr).scale_speed(SPEED_FACTOR);
            true
        }
        // `,` / `.` 逐帧后退 / 前进（VK_OEM_COMMA / VK_OEM_PERIOD）
        _ if key(VK_OEM_COMMA.0) => {
            (&mut *app_ptr).step_frame(false);
            true
        }
        _ if key(VK_OEM_PERIOD.0) => {
            (&mut *app_ptr).step_frame(true);
            true
        }
        _ if key(VK_0.0) => {
            (&mut *app_ptr).seek_percent(0);
            true
        }
        _ if key(VK_1.0) => {
            (&mut *app_ptr).seek_percent(1);
            true
        }
        _ if key(VK_2.0) => {
            (&mut *app_ptr).seek_percent(2);
            true
        }
        _ if key(VK_3.0) => {
            (&mut *app_ptr).seek_percent(3);
            true
        }
        _ if key(VK_4.0) => {
            (&mut *app_ptr).seek_percent(4);
            true
        }
        _ if key(VK_5.0) => {
            (&mut *app_ptr).seek_percent(5);
            true
        }
        _ if key(VK_6.0) => {
            (&mut *app_ptr).seek_percent(6);
            true
        }
        _ if key(VK_7.0) => {
            (&mut *app_ptr).seek_percent(7);
            true
        }
        _ if key(VK_8.0) => {
            (&mut *app_ptr).seek_percent(8);
            true
        }
        _ if key(VK_9.0) => {
            (&mut *app_ptr).seek_percent(9);
            true
        }
        _ => false,
    }
}

/// 处理拖放。只取第一个文件，忽略目录。
unsafe fn handle_drop(app_ptr: *mut App, hdrop: HDROP) {
    // 先把文案取出来（`Strings` 是 Copy）��下面要拿 `&mut App` 调
    // `report_error`，那时就不能再借 `app_ptr.strings` 了。
    let strings = (*app_ptr).strings;
    let len = DragQueryFileW(hdrop, 0, None);
    if len == 0 {
        DragFinish(hdrop);
        return;
    }
    let mut buf = vec![0u16; len as usize + 1];
    DragQueryFileW(hdrop, 0, Some(&mut buf));
    // HDROP 是这次拖放唯一的句柄，处理完必须释放，否则会一直泄漏
    DragFinish(hdrop);

    // 丢开 NUL 终止符再转：DragQueryFileW 返回的是长度，不含终止符。
    // `String::from_utf16_lossy` 在遇到无法表示的字符（某些 emoji、变体
    // 选择符）时会插 U+FFFD，拼出来的路径指向一个不存在的文件，用户看到的
    // 却是「找不到文件」。
    let units = match String::from_utf16(&buf[..len as usize]) {
        Ok(s) => s,
        Err(_) => {
            (&mut *app_ptr).report_error(strings.err_open, strings.err_bad_unicode);
            return;
        }
    };
    let path = PathBuf::from(units);
    if path.is_file() {
        (&mut *app_ptr).open(&path);
    } else {
        (&mut *app_ptr).report_error(strings.err_open, strings.err_not_a_file);
    }
}

/// 模态提示框。release 下没有控制台，这是唯一的反馈通道。
///
/// `detail` 为空时不加空行——信息类提示（「已复制」）只有一个短句，
/// 后面挂两行空白很奇怪。
fn message_box(owner: HWND, caption: &str, detail: &str, icon: MESSAGEBOX_STYLE) {
    let body = if detail.is_empty() {
        caption.to_string()
    } else {
        format!("{caption}\n\n{detail}")
    };
    let text: Vec<u16> = body.encode_utf16().chain(std::iter::once(0)).collect();
    let cap: Vec<u16> = "VideoView"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(text.as_ptr()),
            PCWSTR(cap.as_ptr()),
            MB_OK | icon,
        );
    }
}

/// 剪贴板格式 CF_UNICODETEXT。
///
/// windows-rs 没有为剪贴板格式常量生成绑定（它们是 #define 而不是枚举），
/// 所以这里写数值 13 并把含义写在名字里。写错的话 SetClipboardData 会成功
/// ——剪贴板接受任意格式——但贴出来的是空的。
const CF_UNICODETEXT: u32 = 13;

/// 把文本放进剪贴板（CF_UNICODETEXT）。
///
/// 剪贴板是**全局共享**的一份数据，而 `SetClipboardData` 会把那块内存的
/// 所有权转给系统——所以这里必须用 `GMEM_MOVEABLE` 分配，并且一旦
/// `SetClipboardData` 成功就**不能再 free**（那是 Windows 的老规矩，
/// 双重释放会直接破坏全局剪贴板）。
///
/// 每一步都要 `CloseClipboard`。忘了关的话剪贴板会被锁住，其它程序
/// （包括用户自己再按一次快捷键）都写不进去，而且不会自动恢复——要等
/// 目标进程退出。
fn copy_to_clipboard(owner: HWND, text: &str) -> Result<(), String> {
    // OpenClipboard 会失败：剪贴板被别的进程开着且不放手。
    // 这是**正常**竞争，不是错误状态，重试一次通常就好了。
    let mut opened = unsafe { OpenClipboard(Some(owner)) }.is_ok();
    if !opened {
        std::thread::sleep(std::time::Duration::from_millis(50));
        opened = unsafe { OpenClipboard(Some(owner)) }.is_ok();
    }
    if !opened {
        return Err("the clipboard is in use by another application".to_string());
    }

    // 从这里开始无论走哪条分支都必须 CloseClipboard。用一个立刻执行的闭包
    // 包住，保证任何 early return 都不会漏掉关闭
    let result = (|| -> Result<(), String> {
        unsafe {
            EmptyClipboard().map_err(|e| format!("EmptyClipboard failed: {e}"))?;
        }
        // 字节数含结尾的 NUL。Win32 要的是**字节**数不是字符数
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        buf.push(0);
        // SAFETY: `buf` 是一段对齐的有效 UTF-16 存储，长度按字节算，
        // 而且是最后一步读（后面 `buf` 不再改动）
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len() * 2) };
        // SAFETY: 长度按字节算，且 buf 确实是 NUL 结尾的 UTF-16
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
            .map_err(|e| format!("GlobalAlloc failed: {e}"))?;
        // GlobalAlloc 返回的是全局锁，GlobalLock 才给出可直接写的指针
        let ptr = unsafe { GlobalLock(handle) };
        let ptr = ptr.cast::<u8>();
        if ptr.is_null() {
            unsafe {
                let _ = GlobalFree(Some(handle));
            }
            return Err("GlobalLock failed".to_string());
        }
        // SAFETY: GlobalLock 至少返回 `bytes.len()` 字节可写空间
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
        unsafe {
            let _ = GlobalUnlock(handle);
        }

        // 转移所有权给系统：成功之后 **不能** 再 GlobalFree
        match unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(handle.0))) } {
            Ok(_) => Ok(()),
            Err(e) => {
                // 没转移走，那块内存还是我们的，必须在这里释放
                unsafe {
                    let _ = GlobalFree(Some(handle));
                }
                Err(format!("SetClipboardData failed: {e}"))
            }
        }
    })();

    unsafe {
        let _ = CloseClipboard();
    }
    result
}

/// 文件名（不含目录与扩展名），显示在控制栏上。
fn file_title(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 命令行里要打开的文件。
///
/// 只接受第一个存在的文件；不存在就忽略（不弹窗），因为资源管理器关联传进来
/// 的路径可能已经失效。
fn initial_file() -> Option<PathBuf> {
    std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

// ---------------------------------------------------------------- 启动

/// 应用入口。内部已处理所有错误（弹窗提示），不会向上抛。
/// 启动早期（`App` 还没建起来、或正在建）那几个要弹错误框的地方用的文案。
///
/// 这些函数拿不到 `App::strings`，而把 `&Strings` 一路穿过
/// `run` / `create_and_loop` / `register_app_class` 会让参数列表变长、
/// 可读性变差。`Strings` 是 `Copy`，而 `Lang::detect()` 只是「读一个环境变量 +
/// 读一次注册表 + 一次 API」，只有**出错路径**才会调到这里，正常启动一次都不调。
fn startup_strings() -> lang::Strings {
    lang::Strings::new(lang::Lang::detect())
}

pub fn run() {
    enable_dpi_awareness();
    // 崩溃日志要在**任何**可能 panic 的代码之前装上。
    //
    // release 是 `panic = "abort"`，没有 hook 的话 panic 的表现就是「闪一下
    // 消失」——用户和开发者都拿不到任何信息。装 hook 只是一次 set_hook，
    // 失败也不会影响启动（`install` 内部不返回 Result，写日志失败时安静跳过）。
    crashlog::install();

    // 文件对话框用 COM，需要 STA。失败不致命，只是打不开对话框。
    let com_ok = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();

    if let Err(e) = create_and_loop() {
        let s = startup_strings();
        message_box(HWND::default(), s.err_startup, &e, MB_ICONERROR);
    }

    if com_ok {
        unsafe { CoUninitialize() };
    }
}

/// 打开 per-monitor DPI v2 感知。
///
/// 必须在创建任何窗口之前调用，且只能成功一次：进程一旦创建了窗口，DPI 感知
/// 模式就固定了。顺序写错的表现是整个窗口（含文字）被系统按系统 DPI 拉伸成
/// 位图，文字边缘发虚。
///
/// 优先 `SetProcessDpiAwarenessContext`（v2，非客户区也自动缩放），老系统退回
/// `SetProcessDpiAwareness`（v1，只有客户区）。
fn enable_dpi_awareness() {
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_ok() {
            return;
        }
        let _ = SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);
    }
}

/// `AdjustWindowRectEx` 失败时的边框粗略值（物理像素）。
///
/// 只是个兜底：换算失败的原因通常是窗口站 / 主题服务异常，那属于「系统
/// 有点问题」，不该让播放器起不来。
fn px_frame(dpi: u32) -> i32 {
    ui::dip_to_px(39, dpi)
}

/// 注册窗口类 + 创建主窗口 + 创建 mpv + 消息循环。
fn create_and_loop() -> Result<(), String> {
    // 设置在这里读一次，然后传给 `App::new`。
    //
    // 之前是 `App::new` 内部自己读、这里为了算窗口尺寸又读一遍 —— 两个来源，
    // 改一处忘一处就会「窗口尺寸按旧的、面板开关按新的」。只有一个来源时
    // 这类不一致不可能发生。
    //
    // `Settings::load` 在任何一步失败时都返回默认值，所以这里不需要 `?`。
    let saved = Settings::load();
    unsafe {
        let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(PCWSTR::null())
            .map_err(|e| format!("Could not get the module handle: {e}"))?;
        let hinstance = HINSTANCE(module.0);
        register_app_class(hinstance)?;

        // 设置里存的是**客户区**尺寸（DIP），而 `CreateWindowExW` 的
        // width/height 要的是**窗口**尺寸（含边框与标题栏）。
        //
        // 两者差一个 `AdjustWindowRectEx` 的量。不换算的话每次启动窗口都会
        // 比上次大一圈边框——用户拖到刚好的尺寸，关掉再开就变大了。
        let system_dpi = GetDpiForSystem().max(96);
        let client_w = ui::dip_to_px(saved.client_w.max(MIN_CLIENT_W_DIP), system_dpi);
        let client_h = ui::dip_to_px(saved.client_h.max(MIN_CLIENT_H_DIP), system_dpi);
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: client_w,
            bottom: client_h,
        };
        let style = WINDOW_EX_STYLE::default();
        let win_style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
        if AdjustWindowRectEx(&mut frame, win_style, false, style).is_err() {
            // 换算失败就退回「客户区 + 边框」的粗略估计，宁可窗口大一圈
            // 也不能因为这个失败而起不来
            frame.right = client_w + px_frame(system_dpi);
            frame.bottom = client_h + px_frame(system_dpi);
        }

        // App 必须在 CreateWindowExW 之前建好：lpParam 指向它，
        // WM_NCCREATE 里会挂到 GWLP_USERDATA。
        let mut app = Box::new(App::new(saved));

        let class_name = wide(APP_CLASS);
        let title = wide("VideoView");
        let hwnd = CreateWindowExW(
            style,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            // WS_CLIPCHILDREN：画面子窗口整块盖住视频区，而我们的 WM_PAINT
            // 在整窗重画时（影院 / 全屏切换）会往整个客户区贴图。没有它，
            // 系统不会在贴图前排除子窗口区域，等于白贴一遍被完全遮住的部分。
            win_style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            frame.right - frame.left,
            frame.bottom - frame.top,
            None,
            None,
            Some(hinstance),
            Some(&mut *app as *mut App as *mut c_void),
        )
        .map_err(|e| {
            // `WM_CREATE` 记下的原因比「创建主窗口失败」有用得多：前者是
            // 「无法创建画面窗口: <真实原因>」。后者之所以没用，是因为
            // `WM_CREATE` 返回 -1 时 `GetLastError` **根本不会被设置**，
            // 于是 `e` 往往显示成 0 号错误「操作成功完成」。
            match &app.create_error {
                Some(detail) => format!("{detail} (while creating the main window: {e})"),
                None => format!("Could not create the main window: {e}"),
            }
        })?;
        app.hwnd = hwnd;

        // 窗口创建期间（`WM_NCCREATE`…`WM_NCPAINT`）系统发了一批消息，处理过的
        // 已经处理掉了，绘制类（`WM_PAINT` / `WM_NCPAINT`）的还留在队列里。
        // 在这里抽干，后面 `MpvPlayer::new` 那几百毫秒（D3D11 设备创建 +
        // 适配器枚举）就不是面对一个空白窗口。
        pump_messages();

        // mpv 的 wid 就是画面窗口的句柄，所以必须在窗口创建之后。
        //
        // 失败必须先 `DestroyWindow` 再把错误抛出去，不能让 `?` 直接走：
        // 这时窗口已经活着、`GWLP_USERDATA` 指着上面那个 `Box<App>`、定时器
        // 也在跑。`?` 触发后 `app` 被 drop，窗口却还在，接着 `run()` 弹的
        // MessageBoxW 自己会转一圈消息循环，`WM_TIMER` 进到窗口过程里
        // 解引用那块已释放内存——这是最难查的那种崩溃。
        let player = match MpvPlayer::new(&InitOptions {
            wid: app.video.0 as isize,
            headless: false,
        }) {
            Ok(p) => p,
            Err(e) => {
                let _ = DestroyWindow(hwnd);
                return Err(e);
            }
        };
        app.player = Some(Arc::new(player));

        // 把保存下来的音量 / 静音推给 mpv。
        //
        // 必须在 player 就位**之后**：mpv 的默认音量是 100，而用户上次可能
        // 调到了 30。不推的话界面显示 30、实际响 100——「设置没生效」。
        //
        // 失败不弹框：这时 `initialize` 刚成功，而音量写失败（极少见）不该
        // 表现成「程序启动失败」。设置里的值已经是界面状态了，mpv 那一份
        // 差一点不影响用户看到的东西。
        let _ = app.player().set_volume(app.state.volume);
        let _ = app.player().set_mute(app.state.muted);

        // 250ms 定时器**必须等到 `player` 就位之后再武装**。
        //
        // 原来是在 `WM_CREATE` 里 `SetTimer` 的，那时离消息循环还有整整一个
        // `MpvPlayer::new`（D3D11 初始化，几百毫秒）。这段窗口里只要有任何
        // 一次消息泵被跑起来——`DllMain` 自己泵、`PeekMessage`、或者任何
        // 带自己消息循环的 API——`WM_TIMER` 就会进到窗口过程 → `tick()` →
        // `self.player()` → `.expect("播放核心尚未初始化")` →
        // `panic = "abort"` 下**整个进程静默消失，用户看不到任何提示**。
        //
        // 挪到这里之后，`tick()` 存在的时间范围内 `player` 一定已经是
        // `Some`，那条 abort 路径被彻底关掉。
        let _ = SetTimer(Some(hwnd), TIMER_TICK, TICK_MS, None);

        // 画面区域的点击 / 双击通过自定义消息回到主线程处理：
        // 画面子窗口的 wnd_proc 可能跑在 mpv 的线程上下文里，不能直接改状态
        let hwnd_raw = hwnd.0 as isize;
        surface::set_video_handler(Box::new(move |name| {
            let msg = match name {
                "click" => WM_VIDEO_CLICK,
                "dblclick" => WM_VIDEO_DBLCLICK,
                _ => return,
            };
            let target = HWND(hwnd_raw as *mut c_void);
            let _ = PostMessageW(Some(target), msg, WPARAM(0), LPARAM(0));
        }));

        let queue = app.events.clone();
        app.player().spawn_event_loop(Box::new(move |event| {
            queue.lock().unwrap_or_else(|e| e.into_inner()).push(event);
            let target = HWND(hwnd_raw as *mut c_void);
            let _ = PostMessageW(Some(target), WM_MPV_EVENT, WPARAM(0), LPARAM(0));
        }));

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);

        if let Some(path) = initial_file() {
            app.open(&path);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 退出前把设置强制落盘（`tick` 里是节流的，最后一次改动可能还没到
        // 落盘时刻）。窗口尺寸在这一刻才是准确的。
        app.prefs_flush(true);

        // 退出前把 mpv 收干净：它的事件线程还在读 DLL
        let _ = app.player().shutdown();
        drop(app);
        Ok(())
    }
}

/// 注册主窗口类。
///
/// 图标加载失败时**报错**而不是静默用空句柄：`build.rs` 在找不到 `rc.exe` 时
/// 只告警不失败，于是产物是个没有图标也没有版本信息的 exe。那种情况下让它
/// 在这里直接失败，比装出一个任务栏显示通用图标的程序好排查得多。
unsafe fn register_app_class(hinstance: HINSTANCE) -> Result<(), String> {
    let class_name = wide(APP_CLASS);
    // 资源 id 1 就是 assets/app.rc 里嵌进去的那个图标。
    // 这里不能图省事写 IDC_ICON——那是「系统默认应用图标」，
    // 任务栏和标题栏就会显示成那个通用图标，看不出是哪个程序。
    // （IDC_ICON 和 IDI_APPLICATION 恰好都是 32512，写哪个都一样错。）
    let icon = LoadIconW(Some(hinstance), PCWSTR(APP_ICON_ID as *const u16))
        .map_err(|_| "加载图标失败：exe 资源段里没有 IDI_APP_ICON（id 1）".to_string())?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        // CS_HREDRAW | CS_VREDRAW：客户区尺寸变化时整个重画。控制栏是自绘的，
        // 少了这句 resize 之后会留下残影。
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(app_wnd_proc),
        hInstance: hinstance,
        hIcon: icon,
        hIconSm: icon,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    // 重复注册同一个类返回 0 且 GetLastError 是 ERROR_CLASS_ALREADY_EXISTS，
    // 那是「已经在别处注册过了」的正常情况，不该当错误。
    // 但「因为别的原因注册失败」也不该被一起吞掉，所以只放过这一种。
    if RegisterClassExW(&wc) == 0 {
        let err = GetLastError();
        if err != ERROR_CLASS_ALREADY_EXISTS {
            // 直接替换占位符即可，外面不必再套一层 format!（clippy useless_format）
            return Err(startup_strings()
                .err_register_class
                .replace("{e}", &err.0.to_string()));
        }
    }
    Ok(())
}

/// 字符串 -> 以 NUL 结尾的 UTF-16 宽字符串。
/// 把当前线程消息队列里已经排队的消息全部处理掉。
///
/// `PeekMessage` + `PM_REMOVE` 循环，**不**进入阻塞等待——所以它不会在没有
/// 消息时卡住，只是把「已经排在那里的」清干净。
///
/// 存在的唯一理由是启动时序：`CreateWindowExW` 已经返回、但主消息循环还没
/// 开始的那个窗口里，队列里会积着若干绘制类消息。不抽干的话，
/// 紧接着的 `MpvPlayer::new`（D3D11 初始化）期间窗口是空白的；而反过来，
/// 如果那段初始化过程中有任何东西泵了一次消息，`WM_TIMER` 就会在
/// `player()` 还是 `None` 时进到窗口过程。
///
/// 抽干之后这两个问题都不存在：绘制在初始化之前完成，定时器在初始化之后
/// 才武装。
fn pump_messages() {
    let mut msg = MSG::default();
    loop {
        // 注意 `PeekMessageW` 返回 `BOOL`，不是 windows 0.62 里
        // `GetMessageW` 那套 `Result` 形状 —— 判 `as_bool()`。
        let got = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
        if !got.as_bool() {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{blit_points, intersect_rect, BackBuffer, RECT};
    use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
    use windows::Win32::Graphics::Gdi::{HBITMAP, HDC, HGDIOBJ};

    /// 回归测试：控制栏曾经再也不刷新。
    ///
    /// 起因是把 `BitBlt` 的目标点写成了 `(0, 0)`，以为 `BeginPaint` 给的
    /// DC 原点已经挪到 `rcPaint` 左上角。实际上更新区只是被裁剪、没有被
    /// 平移，DC 原点仍在客户区 `(0,0)`。结果控制栏被贴到窗口顶部、
    /// 盖在视频子窗口下面，屏幕上永远显示第一次全量绘制留下的静止画面：
    /// 时间码停在 `00:00`、进度条 thumb 钉在最左，而内部状态其实一直在推进。
    #[test]
    fn 贴图目标点必须是脏区自身的客户区坐标() {
        // 1375x900 客户区（1100x720 逻辑 @125%）、62 DIP = 78px 高的控制栏
        let controls = RECT {
            left: 0,
            top: 900 - 78,
            right: 1375,
            bottom: 900,
        };
        let (dest_x, dest_y, src_x, src_y) = blit_points(&controls);
        assert_eq!(
            (dest_x, dest_y),
            (0, 822),
            "目标点写成 (0,0) 会把控制栏贴到窗口顶部、被视频子窗口盖住"
        );
        // 后备位图与客户区同坐标系，源点跟着一起是脏区坐标
        assert_eq!((src_x, src_y), (0, 822));
    }

    /// 整客户区重绘（首次显示、resize、切屏）时目标点落在原点。
    ///
    /// 这条是上一条的对照：两种情况下算式相同，但只有这一种
    /// 「(0,0) 恰好是对的」，也正因为如此，脏区版本的错误不会被立刻发现。
    #[test]
    fn 全客户区重绘时目标点落在原点() {
        let all = RECT {
            left: 0,
            top: 0,
            right: 1375,
            bottom: 900,
        };
        assert_eq!(blit_points(&all), (0, 0, 0, 0));
    }

    /// 脏区不总是从 `(0,0)` 起步：音量条被拖到最右时脏区是右边一竖条。
    #[test]
    fn 右侧脏区也不会退化成原点() {
        let right_edge = RECT {
            left: 1200,
            top: 780,
            right: 1270,
            bottom: 800,
        };
        assert_eq!(blit_points(&right_edge), (1200, 780, 1200, 780));
    }

    /// 最小化时客户区是 0x0，必须直接放弃而不是去建一个 0x0 的位图。
    ///
    /// 两个后果：`CreateCompatibleBitmap(hdc, 0, 0)` 必然失败，
    /// 而 `CreateCompatibleDC` 在它之前已经建出来了。原来的 `release`
    /// 只在「位图有效」时才 DeleteDC，那个建出来却没人管的 DC 就留在
    /// 状态里；`ensure` 每 250ms 被 tick 调一次，每次 resize/最小化切换
    /// 都漏一个。GDI 句柄上限不高，几十次来回就够在任务管理器里看到
    /// 句柄数往上爬。
    #[test]
    fn 零尺寸客户区不分配后备位图() {
        unsafe {
            let screen_dc = GetDC(None);
            let mut back = BackBuffer {
                dc: HDC::default(),
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                w: 0,
                h: 0,
            };
            // 最小化：宽或高为 0
            assert!(!back.ensure(HDC(screen_dc.0), 1375, 0));
            assert!(
                back.dc.is_invalid(),
                "0 高度也建了 DC，等于每次最小化漏一个"
            );
            assert!(!back.ensure(HDC(screen_dc.0), 0, 900));
            assert!(back.dc.is_invalid(), "0 宽度也建了 DC");
            assert!(!back.ensure(HDC(screen_dc.0), -5, -5));
            assert!(back.dc.is_invalid(), "负尺寸也要挡住");

            // 正常尺寸：建成功，且重复调用不重建
            assert!(back.ensure(HDC(screen_dc.0), 1375, 900));
            assert!(!back.bitmap.is_invalid());
            assert!(!back.dc.is_invalid());
            let first_bitmap = back.bitmap.0;
            assert!(back.ensure(HDC(screen_dc.0), 1375, 900));
            assert_eq!(
                back.bitmap.0, first_bitmap,
                "尺寸没变却重建了，白白 alloc + free 一块 4.9MB 位图"
            );

            // 尺寸变了必须重建，否则贴图会按旧尺寸画
            assert!(back.ensure(HDC(screen_dc.0), 800, 600));
            assert_ne!(back.bitmap.0, first_bitmap, "尺寸变了却复用了旧位图");

            // resize 回 0（最小化）：ensure 提前返回 false，此时**刻意**保留
            // 旧的位图和 DC 不释放——用户把窗口还原回来时尺寸可能没变，
            // 直接复用比重建一块 4.9MB 的位图划算。要点是不能在这条路径上
            // 建出新的 0x0 位图（上面三条断言就是钉这个的）
            assert!(!back.ensure(HDC(screen_dc.0), 0, 0));
            assert!(
                !back.bitmap.is_invalid(),
                "保留旧位图，还原窗口时才能免重建"
            );

            ReleaseDC(None, screen_dc);
        }
    }

    /// `intersect_rect` 的三个不变量：结果被两个输入同时夹住、不相交时退化成
    /// 零面积、以及绝不产生 `left > right` 这种反向矩形。
    ///
    /// 反向矩形最危险：调用方现有的判断是 `w <= 0 || h <= 0`，而
    /// `left=100, right=40` 算出的 `w` 是 `-60`，能被挡住；可一旦有人改成
    /// 拿绝对值或者改成比较坐标，就会拿一个反向矩形去 `FillRect`——
    /// GDI 会画出一个上下颠倒的矩形，静默画错地方。
    #[test]
    fn 矩形交集的三个不变量() {
        let r = |l, t, rr, b| RECT {
            left: l,
            top: t,
            right: rr,
            bottom: b,
        };

        // 相交：脏区被控制栏夹住
        let whole = r(0, 0, 1582, 853);
        let controls = r(0, 775, 1582, 853);
        let got = intersect_rect(whole, &controls);
        assert_eq!(got, controls, "播放时整块脏区应该正好退化成控制栏");

        // 只交到一部分
        let got = intersect_rect(r(0, 700, 1582, 853), &controls);
        assert_eq!(got, r(0, 775, 1582, 853), "只交到下缘");

        // 脏区完全在控制栏上方（视频区）
        let got = intersect_rect(r(0, 0, 1582, 700), &controls);
        assert!(
            got.right <= got.left && got.bottom <= got.top,
            "不相交必须退化成零面积，实际 {got:?}"
        );

        // 完全不相交（左右错开）
        let got = intersect_rect(r(100, 0, 200, 100), &r(300, 0, 400, 100));
        assert!(
            got.right <= got.left && got.bottom <= got.top,
            "左右错开也必须退化成零面积，实际 {got:?}"
        );

        // 影院模式下控制栏是零面积矩形（0,client_h,0,client_h）
        let theatre_controls = r(0, 853, 0, 853);
        let got = intersect_rect(whole, &theatre_controls);
        assert!(
            got.right <= got.left && got.bottom <= got.top,
            "影院模式下不该留下任何可绘制的面积，实际 {got:?}"
        );

        // 输入本身反向时也不能产出反向结果
        let got = intersect_rect(r(200, 100, 40, 10), &whole);
        assert!(
            got.right <= got.left && got.bottom <= got.top,
            "反向输入也要归一成零面积，实际 {got:?}"
        );
    }
}
