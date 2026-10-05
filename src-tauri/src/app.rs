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

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EndPaint,
    InvalidateRect, ScreenToClient, SelectObject, HBITMAP, HDC, HGDIOBJ, PAINTSTRUCT,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwareness, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, PROCESS_PER_MONITOR_DPI_AWARE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_F,
    VK_F11, VK_LEFT, VK_M, VK_O, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, KillTimer, LoadCursorW, LoadIconW, MessageBoxW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, GWLP_USERDATA, GWL_EXSTYLE, GWL_STYLE, HTCLIENT, HTTRANSPARENT, HWND_TOP,
    IDC_ARROW, IDC_HAND, MB_ICONERROR, MB_OK, MINMAXINFO, MSG, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SW_SHOW, WINDOW_EX_STYLE, WM_CAPTURECHANGED, WM_CLOSE, WM_CREATE,
    WM_DESTROY, WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR,
    WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
};

use crate::mpv::{InitOptions, MpvEventMessage, MpvPlayer};
use crate::surface;
use crate::ui::{self, Hit, Layout, PaintState};

/// 主窗口类名。
const APP_CLASS: &str = "VideoViewMain";

/// 首次显示时的客户区尺寸（DIP），与旧版 tauri.conf.json 里的 width/height 一致。
const DEFAULT_CLIENT_W_DIP: i32 = 1100;
const DEFAULT_CLIENT_H_DIP: i32 = 720;
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
    pending_errors: Vec<(String, String)>,
    /// 有人请求打开文件对话框，真正的模态调用放到 `WM_APP_OPEN_DIALOG` 里做。
    open_dialog: bool,
}

/// 排队等弹的错误提示上限。
///
/// 循环报错时（比如解码失败 mpv 每秒抛一次事件）队列不能无限长，
/// 否则用户还没点掉第一个框，内存就先被几十条一模一样的消息吃掉了。
const MAX_QUEUED_ERRORS: usize = 8;

impl App {
    fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            video: HWND::default(),
            player: None,
            events: Arc::new(Mutex::new(Vec::new())),
            state: UiState::default(),
            dragging: Dragging::None,
            drag_x: 0,
            theatre: false,
            fullscreen: false,
            saved: None,
            // DPI 真正生效后 WM_CREATE 会立刻重建字体
            theme: ui::Theme::new(96),
            dpi: 96,
            hover: Hit::None,
            client_w: 0,
            client_h: 0,
            layout: Layout::new(0, 0, 96, false),
            back: BackBuffer {
                dc: HDC::default(),
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                w: 0,
                h: 0,
            },
            pending_errors: Vec::new(),
            open_dialog: false,
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
        let dpi = ui::dpi_for_window(self.hwnd);
        if dpi != self.dpi || !self.theme.matches(dpi) {
            self.dpi = dpi;
            self.theme = ui::Theme::new(dpi);
        }

        self.layout = Layout::new(self.client_w, self.client_h, self.dpi, self.theatre);

        let video_rect = RECT {
            left: 0,
            top: 0,
            right: self.client_w,
            bottom: self.layout.controls.top.max(0),
        };
        surface::set_video_bounds(self.video, &video_rect);
    }

    /// 只重画控制栏：视频区由 mpv 自己画，重画没有意义。
    fn invalidate_controls(&self) {
        if self.theatre {
            return;
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&self.layout.controls), false);
        }
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
        self.state.title = file_title(path);
        surface::set_video_visible(self.video, true);
        self.relayout();
        self.invalidate_all();

        if let Err(e) = self.player().load_file(path) {
            self.report_error("打开文件失败", &e);
        }
    }

    /// 播放 / 暂停切换。没有文件时先弹文件对话框。
    fn toggle_pause(&mut self) {
        if !self.state.has_file {
            self.pick_file();
            return;
        }
        if let Err(e) = self.player().toggle_pause() {
            self.report_error("切换播放状态失败", &e);
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
            self.report_error("停止失败", &e);
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
            self.report_error("跳转失败", &e);
        }
        self.state.position = target;
        self.invalidate_controls();
    }

    fn set_volume(&mut self, volume: f64) {
        let v = volume.clamp(0.0, 100.0);
        if let Err(e) = self.player().set_volume(v) {
            self.report_error("调整音量失败", &e);
        }
        self.state.volume = v;
        self.invalidate_controls();
    }

    fn toggle_mute(&mut self) {
        let next = !self.state.muted;
        if let Err(e) = self.player().set_mute(next) {
            self.report_error("切换静音失败", &e);
        }
        self.state.muted = next;
        self.invalidate_controls();
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

    /// 定时器：主动读一次播放进度。
    fn tick(&mut self) {
        // 拖动中以鼠标位置为准，不跟着播放进度跳
        if self.dragging == Dragging::Seek || !self.state.has_file {
            return;
        }
        let Ok(pos) = self.player().get_double_property("time-pos") else {
            return;
        };
        if (pos - self.state.position).abs() > 1e-3 {
            self.state.position = pos;
            self.invalidate_controls();
        }
    }

    /// 处理 mpv 事件线程投递过来的消息。
    fn drain_events(&mut self) {
        let batch = {
            let mut queue = self.events.lock().unwrap();
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
                    self.report_error("播放出错", &msg);
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
        let dirty = if fresh {
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
                    // 先取出来再构造 PaintState：里面那个 `&mut self.theme`
                    // 已经借走了整个 `self`，同一表达式里再调 `self` 的方法
                    // 就是同时存在借用
                    drag_seconds,
                    theatre: self.theatre,
                    idle: self.state.idle,
                    hover: self.hover,
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
        if self.pending_errors.len() >= MAX_QUEUED_ERRORS {
            // 循环报错（比如解码失败时每秒一次）时别把内存吃光：
            // 丢最早的那条，留最近的
            self.pending_errors.remove(0);
        }
        self.pending_errors
            .push((caption.to_string(), detail.to_string()));
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
                    let _ = SetTimer(Some(hwnd), TIMER_TICK, TICK_MS, None);
                    app.invalidate_all();
                    let _ = SetFocus(Some(hwnd));
                    LRESULT(0)
                }
                Err(e) => {
                    message_box(hwnd, "无法创建画面窗口", &e);
                    // 返回 -1 让 CreateWindowExW 失败，调用方统一报错退出
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
                let (caption, detail) = (&mut *app_ptr).pending_errors.remove(0);
                message_box(hwnd, &caption, &detail);
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
                if let Some(path) = pick_video_dialog() {
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
fn pick_video_dialog() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter(
            "视频文件",
            &[
                "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "mpg", "mpeg",
            ],
        )
        .add_filter("所有文件", &["*"])
        .set_title("选择要播放的视频")
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
        _ if key(VK_M.0) => {
            (&mut *app_ptr).toggle_mute();
            true
        }
        _ => false,
    }
}

/// 处理拖放。只取第一个文件，忽略目录。
unsafe fn handle_drop(app_ptr: *mut App, hdrop: HDROP) {
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
            (&mut *app_ptr).report_error("无法打开", "文件路径不是合法的 Unicode，无法处理。");
            return;
        }
    };
    let path = PathBuf::from(units);
    if path.is_file() {
        (&mut *app_ptr).open(&path);
    } else {
        (&mut *app_ptr).report_error("无法打开", "拖进来的不是文件。");
    }
}

/// 模态错误提示。release 下没有控制台，这是唯一的反馈通道。
fn message_box(owner: HWND, caption: &str, detail: &str) {
    let text: Vec<u16> = format!("{caption}\n\n{detail}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let cap: Vec<u16> = "VideoView"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(text.as_ptr()),
            PCWSTR(cap.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
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
pub fn run() {
    enable_dpi_awareness();

    // 文件对话框用 COM，需要 STA。失败不致命，只是打不开对话框。
    let com_ok = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();

    if let Err(e) = create_and_loop() {
        message_box(HWND::default(), "VideoView 启动失败", &e);
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

/// 注册窗口类 + 创建主窗口 + 创建 mpv + 消息循环。
fn create_and_loop() -> Result<(), String> {
    unsafe {
        let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(PCWSTR::null())
            .map_err(|e| format!("获取模块句柄失败: {e}"))?;
        let hinstance = HINSTANCE(module.0);
        register_app_class(hinstance)?;

        // 窗口尺寸是物理像素（DPI 感知已开），所以这里要自己乘 DPI。
        let system_dpi = GetDpiForSystem().max(96);
        let width = ui::dip_to_px(DEFAULT_CLIENT_W_DIP, system_dpi);
        let height = ui::dip_to_px(DEFAULT_CLIENT_H_DIP, system_dpi);

        // App 必须在 CreateWindowExW 之前建好：lpParam 指向它，
        // WM_NCCREATE 里会挂到 GWLP_USERDATA。
        let mut app = Box::new(App::new());

        let class_name = wide(APP_CLASS);
        let title = wide("VideoView");
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            // WS_CLIPCHILDREN：画面子窗口整块盖住视频区，而我们的 WM_PAINT
            // 在整窗重画时（影院 / 全屏切换）会往整个客户区贴图。没有它，
            // 系统不会在贴图前排除子窗口区域，等于白贴一遍被完全遮住的部分。
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            None,
            None,
            Some(hinstance),
            Some(&mut *app as *mut App as *mut c_void),
        )
        .map_err(|e| format!("创建主窗口失败: {e}"))?;
        app.hwnd = hwnd;

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
            queue.lock().unwrap().push(event);
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
            return Err(format!("注册窗口类失败: {}", err.0));
        }
    }
    Ok(())
}

/// 字符串 -> 以 NUL 结尾的 UTF-16 宽字符串。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{blit_points, BackBuffer, RECT};
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
}
