//! 控制栏：布局、绘制、命中测试。
//!
//! 控制栏不是一堆子控件，而是主窗口客户区底部的一条自绘区域：
//! `WM_PAINT` 里用 GDI 一次画完，`WM_LBUTTONDOWN` 里用同一套布局做命中
//! 测试。这么做有两个原因：
//!
//! 1. 少一层窗口。子控件各有各的 HWND，就得处理焦点、Tab 顺序、命中优先级；
//!    这里所有交互都在主窗口内完成，键盘事件直接落在主窗口上。
//! 2. 尺寸全部以 DIP（96 DPI 下的逻辑像素）定义，绘制时按窗口当前 DPI 换算，
//!    字体也按同一 DPI 重建。125% / 150% / 200% 缩放下自动等比放大，
//!    且位图与字形都由 GDI 直接按设备像素光栅化——不存在「先按 1 倍画、
//!    再由系统拉伸」那种整体模糊。
//!
//! 视觉沿用原来的深色工具风格：硬边、等宽时间码、无渐变无圆角。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, FrameRect, GetMonitorInfoW,
    IntersectClipRect, MonitorFromWindow, SelectClipRgn, SelectObject, SetBkMode, SetTextColor,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_NORMAL, GDI_REGION_TYPE,
    HBRUSH, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

/// 视觉沿用深色播放器工具风格：深底、硬边、橙色强调色。
const C_STAGE: COLORREF = rgb(0x00, 0x00, 0x00);
const C_PANEL: COLORREF = rgb(0x16, 0x19, 0x1c);
const C_LINE: COLORREF = rgb(0x2a, 0x2f, 0x34);
const C_TEXT: COLORREF = rgb(0xc8, 0xcd, 0xd2);
const C_DIM: COLORREF = rgb(0x6b, 0x74, 0x7c);
const C_ACCENT: COLORREF = rgb(0xff, 0x9f, 0x1c);
const C_BTN: COLORREF = rgb(0x22, 0x26, 0x2a);
const C_BTN_HOVER: COLORREF = rgb(0x2c, 0x31, 0x36);
const C_BTN_EDGE: COLORREF = rgb(0x3c, 0x43, 0x4a);

/// 控制栏高度（同时也是视频区与控制栏的分界线）。
pub const CTRL_H_DIP: i32 = 62;

const PAD_X_DIP: i32 = 10;
const PAD_TOP_DIP: i32 = 6;
const GAP_DIP: i32 = 8;
const TRACK_ROW_DIP: i32 = 16;
const BTN_W_DIP: i32 = 48;
const BTN_H_DIP: i32 = 26;
const TIME_W_DIP: i32 = 52;
const MUTE_W_DIP: i32 = 30;
const VOL_W_DIP: i32 = 96;
const SLIDER_THICK_DIP: i32 = 3;
const THUMB_DIP: i32 = 11;

/// 字体大小（DIP）。比旧版网页界面的 11/12/13px 略放大一档：
/// 旧版在 125% 缩放下时间码只有 13.75 物理像素高，窗口拉大后很难看清。
const FONT_UI_DIP: i32 = 14;
const FONT_MONO_DIP: i32 = 13;
const FONT_HINT_DIP: i32 = 15;

/// 16.6 定点色（Windows 的 COLORREF 是 0x00BBGGRR，不是 CSS 的 #RRGGBB）。
const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

/// DIP -> 物理像素。
///
/// 窗口尺寸、控件位置、字体高度全部先算成物理像素再交给 GDI，
/// 这样在任意 DPI 下都是整数像素绘制，不会出现半像素模糊。
pub fn dip_to_px(dip: i32, dpi: u32) -> i32 {
    ((i64::from(dip) * i64::from(dpi) + 48) / 96) as i32
}

/// 内部简写。
fn px(dip: i32, dpi: u32) -> i32 {
    dip_to_px(dip, dpi)
}

/// 窗口所在显示器的整块区域（不含任务栏裁剪）。
///
/// 全屏要铺满的是显示器本身而不是「工作区」——工作区会留出任务栏的位置，
/// 那样就不是全屏了。
pub fn monitor_rect(hwnd: HWND) -> RECT {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcMonitor
        } else {
            // 拿不到就退回客户区，至少不会给出一个空矩形
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            rc
        }
    }
}

/// 窗口的 DPI。至少按 96 算，避免除出 0 尺寸。
pub fn dpi_for_window(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }.max(96)
}

// ---------------------------------------------------------------- 布局

/// 一次布局计算的结果。绘制与命中测试共用，保证两者永远一致。
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub controls: RECT,
    pub time_current: RECT,
    pub time_duration: RECT,
    pub seek: RECT,
    pub btn_open: RECT,
    pub btn_play: RECT,
    pub btn_stop: RECT,
    pub file_name: RECT,
    pub mute: RECT,
    pub volume: RECT,
    /// 影院模式下控制栏高度为 0，视频区占满整个客户区。
    pub theatre: bool,
}

impl Layout {
    /// 按客户区尺寸与当前 DPI 计算布局。
    ///
    /// `client` 的宽高是物理像素（本进程是 per-monitor DPI 感知，
    /// Win32 坐标与客户区尺寸都已经是设备像素，不需要任何虚拟化换算）。
    pub fn new(client_w: i32, client_h: i32, dpi: u32, theatre: bool) -> Self {
        let pad_x = px(PAD_X_DIP, dpi);
        let gap = px(GAP_DIP, dpi);
        let ctrl_h = if theatre { 0 } else { px(CTRL_H_DIP, dpi) };

        let mut controls = RECT {
            left: 0,
            top: (client_h - ctrl_h).max(0),
            right: client_w,
            bottom: client_h,
        };

        let pad_top = px(PAD_TOP_DIP, dpi);
        let track_h = px(TRACK_ROW_DIP, dpi);
        let time_w = px(TIME_W_DIP, dpi);
        let track_y = controls.top + pad_top;

        let time_current = rect(pad_x, track_y, time_w, track_h);
        let time_duration = rect(client_w - pad_x - time_w, track_y, time_w, track_h);

        let seek_x = time_current.right + gap;
        let seek_w = (time_duration.left - gap - seek_x).max(gap);
        let seek = rect(seek_x, track_y, seek_w, track_h);

        let btn_y = track_y + track_h + gap;
        let btn_h = px(BTN_H_DIP, dpi);
        let btn_w = px(BTN_W_DIP, dpi);
        let btn_open = rect(pad_x, btn_y, btn_w, btn_h);
        let btn_play = rect(btn_open.right + gap, btn_y, btn_w, btn_h);
        let btn_stop = rect(btn_play.right + gap, btn_y, btn_w, btn_h);

        let vol_w = px(VOL_W_DIP, dpi);
        let mute_w = px(MUTE_W_DIP, dpi);
        let volume = rect(client_w - pad_x - vol_w, btn_y, vol_w, btn_h);
        let mute = rect(volume.left - gap - mute_w, btn_y, mute_w, btn_h);

        let file_x = btn_stop.right + gap;
        let file_w = (mute.left - gap - file_x).max(0);
        let file_name = rect(file_x, btn_y, file_w, btn_h);

        // 影院模式下控制栏整体收起，但仍然给出一个「不在屏幕上」的矩形，
        // 这样上层判断「点到了控制栏」时不会误中视频区。
        if theatre {
            controls = RECT {
                left: 0,
                top: client_h,
                right: 0,
                bottom: client_h,
            };
        }

        Self {
            controls,
            time_current,
            time_duration,
            seek,
            btn_open,
            btn_play,
            btn_stop,
            file_name,
            mute,
            volume,
            theatre,
        }
    }

    /// 命中测试。坐标是相对客户区的物理像素。
    pub fn hit(&self, x: i32, y: i32) -> Hit {
        if self.theatre {
            return Hit::Video;
        }
        if !contains(&self.controls, x, y) {
            return Hit::Video;
        }
        for (rect, hit) in [
            (&self.btn_open, Hit::Open),
            (&self.btn_play, Hit::Play),
            (&self.btn_stop, Hit::Stop),
            (&self.mute, Hit::Mute),
            (&self.seek, Hit::Seek),
            (&self.volume, Hit::Volume),
        ] {
            if contains(rect, x, y) {
                return hit;
            }
        }
        Hit::None
    }

    /// 命中区域内的横向比例（0.0~1.0），用于把鼠标位置换算成进度 / 音量。
    pub fn ratio(&self, hit: Hit, x: i32) -> f64 {
        let rect = match hit {
            Hit::Seek => &self.seek,
            Hit::Volume => &self.volume,
            _ => return 0.0,
        };
        let w = rect.right - rect.left;
        if w <= 0 {
            return 0.0;
        }
        (f64::from(x - rect.left) / f64::from(w)).clamp(0.0, 1.0)
    }
}

/// 可点击的区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    None,
    /// 落在视频区（画面区域）。原生视频子窗口在上面时消息由它转交。
    Video,
    Open,
    Play,
    Stop,
    Seek,
    Volume,
    Mute,
}

// ---------------------------------------------------------------- 绘制状态

/// 绘制需要的全部界面状态。
pub struct PaintState<'a> {
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub volume: f64,
    pub muted: bool,
    pub loaded: bool,
    pub title: &'a str,
    /// 正在拖动进度条：此时进度由鼠标位置决定，不跟随播放进度。
    pub dragging_seek: bool,
    /// 拖动时显示的秒数。
    pub drag_seconds: f64,
    /// 影院模式：控制栏不画，视频区占满。
    pub theatre: bool,
    /// 没有加载文件时，在视频区中央画提示文字。
    pub idle: bool,
    /// 鼠标当前停在哪个控件上，用于按钮高亮。
    pub hover: Hit,
}

/// 界面字符串。
pub const IDLE_HINT: &str = "把视频文件拖到这里，或按 Ctrl+O 打开";
pub const LABEL_OPEN: &str = "打开";
pub const LABEL_MUTE_ON: &str = "静音";
pub const LABEL_MUTE_OFF: &str = "音量";

/// 当前 DPI 下的字体集合。DPI 变化时整套重建。
pub struct Fonts {
    dpi: u32,
    pub ui: HFONT,
    pub mono: HFONT,
    pub hint: HFONT,
}

impl Fonts {
    pub fn new(dpi: u32) -> Self {
        Self {
            dpi,
            ui: make_font(FONT_UI_DIP, dpi, "Segoe UI"),
            mono: make_font(FONT_MONO_DIP, dpi, "Consolas"),
            hint: make_font(FONT_HINT_DIP, dpi, "Segoe UI"),
        }
    }

    /// 字体是否还适用于当前 DPI。DPI 变了就重建，避免拉伸字形。
    pub fn matches(&self, dpi: u32) -> bool {
        self.dpi == dpi
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [self.ui, self.mono, self.hint] {
            if !f.is_invalid() {
                unsafe {
                    let _ = DeleteObject(f.into());
                }
            }
        }
    }
}

/// 绘制资源：字体 + 画刷。
///
/// 打包成一个结构体是因为两者生命周期完全一致（都只在 DPI 变化时重建
/// 或全程不变），而分开传会让 `paint` / `draw_slider` 的参数列表长到
/// 一眼看不清哪个是资源、哪个是状态。`paint` 现在是「DC + 画布尺寸 +
/// 布局 + 资源 + 状态 + 脏区」六类输入。
pub struct Theme {
    fonts: Fonts,
    brushes: Brushes,
}

impl Theme {
    pub fn new(dpi: u32) -> Self {
        Self {
            fonts: Fonts::new(dpi),
            brushes: Brushes::new(),
        }
    }

    /// 字体是否还适用于当前 DPI。DPI 变了整套重建，避免拉伸字形。
    pub fn matches(&self, dpi: u32) -> bool {
        self.fonts.matches(dpi)
    }

    pub fn dpi(&self) -> u32 {
        self.fonts.dpi
    }

    /// 拿 `color` 的画笔，作用域到返回的守卫被丢掉为止。
    ///
    /// 缓存里的画笔由 `Theme` 拥有，守卫析构时不会删它；缓存没命中时
    /// 新建的画笔归守卫，析构时删掉。调用方（`fill` / `stroke`）不用
    /// 关心这件事——`FillRect` 不会把画笔留在 DC 上，函数返回就完事。
    fn brush(&mut self, color: COLORREF) -> BrushHandle {
        match self.brushes.get(color) {
            Some(brush) => BrushHandle {
                brush,
                owned: false,
            },
            None => BrushHandle {
                brush: unsafe { CreateSolidBrush(color) },
                owned: true,
            },
        }
    }
}

/// 一次绘制里用完即弃的画笔。
///
/// 只在两种情况下出现：缓存满了（见 `MAX_CACHED_BRUSHES`），或者
/// `CreateSolidBrush` 失败。这类画笔不归 `Brushes` 管，所以 `Drop` 里删。
///
/// 之前这里是把「超上限就不缓存」的句柄直接丢给调用方、谁也不删——
/// 一帧漏一个 GDI 句柄，跑久了进程的对象表就撑爆了。
struct BrushHandle {
    brush: HBRUSH,
    /// true = 这个画笔归我，析构时要删。
    owned: bool,
}

impl BrushHandle {
    fn h(&self) -> HBRUSH {
        self.brush
    }
}

impl Drop for BrushHandle {
    fn drop(&mut self) {
        if self.owned && !self.brush.is_invalid() {
            unsafe {
                let _ = DeleteObject(self.brush.into());
            }
        }
    }
}

/// 画刷缓存。
///
/// 之前每次 `fill` / `stroke` 都是「`CreateSolidBrush` → 用 → `DeleteObject`」。
/// 一帧控制栏有十来次填充，而刷新率是每秒 4 次（`app::TICK_MS`），也就是每秒
/// 40 次 GDI 对象创建 + 销毁。GDI 对象走内核句柄表，创建/删除都要过一遍
/// 系统调用，在小窗口上不明显，但这是纯开销。
///
/// 用颜色查表缓存后，一帧里 `CreateSolidBrush` 的次数是 0（颜色全是
/// `C_*` 那些编译期常量，禁用态也只是它们按固定 alpha 调暗的结果），
/// 整个进程生命周期内总共建 10 来个刷子。
///
/// ## 为什么跨 DC 复用是安全的
///
/// 能长期持有的前提是「这个 GDI 对象只会被选进**格式相同**的 DC」。
/// 这里成立：画笔只进后备位图那一个内存 DC，而它是
/// `CreateCompatibleDC(窗口 DC)`，窗口没挂 `WS_EX_LAYERED`，resize 时
/// 重建出来的格式也一致。（`HFONT` 同理——那也正是字体能按 DPI 重建、
/// 跨 resize 继续用的原因。）
///
/// 反过来说，"`FillRect` / `FrameRect` 用完会把画笔从 DC 上恢复掉"这条
/// 虽然也成立，却不是这里该依赖的性质：它只说明画笔不会被**留在** DC 上，
/// 并不能推出"换别的 DC 也能用"。
pub struct Brushes {
    items: Vec<(COLORREF, HBRUSH)>,
}

/// 缓存的画刷上限。
///
/// 颜色都是常量，稳态下只用得到十来个；给个宽松的上限纯粹是防止将来有人
/// 拿颜色做 key 时不小心塞进无限增长的集合。超了之后 `get` 返回 `None`，
/// 由 `Theme::brush` 现建一个用完即删的——正确性和句柄都不受影响。
const MAX_CACHED_BRUSHES: usize = 64;

impl Brushes {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// 取缓存里 `color` 的画笔，没有就新建并存下。
    ///
    /// 返回 `None` 表示「不给缓存用」：要么缓存已满，要么 `CreateSolidBrush`
    /// 失败。两种情况下调用方都要自己拿一个用完即删的画笔。
    pub fn get(&mut self, color: COLORREF) -> Option<HBRUSH> {
        if let Some((_, brush)) = self.items.iter().find(|(c, _)| *c == color) {
            return Some(*brush);
        }
        if self.items.len() >= MAX_CACHED_BRUSHES {
            return None;
        }
        let brush = unsafe { CreateSolidBrush(color) };
        // 失败返回 NULL，缓存进去的话以后每次都会拿到这个坏句柄，
        // 而「有没有成功」这件事被缓存本身回答不了。宁可每次重试。
        if brush.is_invalid() {
            return None;
        }
        self.items.push((color, brush));
        Some(brush)
    }
}

impl Default for Brushes {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Brushes {
    fn drop(&mut self) {
        for (_, brush) in self.items.drain(..) {
            unsafe {
                let _ = DeleteObject(brush.into());
            }
        }
    }
}

/// 按 DPI 造一个字体。
///
/// 高度传负数表示「字符高度（em）」，正好对应 CSS 的 `font-size`；
/// 用像素单位而不是磅，是为了和布局里其它尺寸共用同一个 DPI 换算。
fn make_font(size_dip: i32, dpi: u32, face: &str) -> HFONT {
    let height = -px(size_dip, dpi);
    let name: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            FF_DONTCARE.0.into(),
            PCWSTR(name.as_ptr()),
        )
    }
}

/// 按 `dirty` 把界面画进后备位图：视频区留空（原生子窗口盖在上面），
/// 控制栏自绘。
///
/// ## 为什么按脏区画
///
/// `invalidate_controls` 每 250ms 只让控制栏（1375x900 窗口下是 78px 高的一条）
/// 变脏，脏区之外的像素一个字节都没变。但画法如果不裁剪，每帧都要把整个
/// 1375x900 客户区填一遍底色再加一遍面板色——125% 缩放下那是每帧 1.2M 像素、
/// 每秒 4.8M 像素的纯写入，全部落在屏幕外（后备位图）的内存上。
///
/// 这里用 `IntersectClipRect` 把整个绘制过程裁到脏区，函数内部仍然照常画
/// 全部元素：GDI 的裁剪是硬件/驱动层的，裁掉的区域根本不会产生写入，
/// 而代码不用为「哪些元素落在脏区外」写一堆判断。
pub fn paint(
    hdc: HDC,
    client_w: i32,
    client_h: i32,
    layout: &Layout,
    theme: &mut Theme,
    s: &PaintState,
    dirty: &RECT,
) {
    let dirty = clamp_to_client(dirty, client_w, client_h);
    unsafe {
        // **必须**先把裁剪区清回整块位图，否则下面这一步的效果会逐帧衰减。
        //
        // `IntersectClipRect` 的语义是「把裁剪区换成「当前裁剪区 ∩ 传入矩形」」，
        // 它不是替换。而传进来的是常驻的后备位图 DC（`App::BackBuffer`），
        // 它带着上一次绘制留下的裁剪区一直活到下一次：
        //
        //   第 1 帧（全客户区）  clip = 整个位图 ∩ 整客户区 = 整客户区
        //   第 2 帧（控制栏）    clip = 整客户区 ∩ 控制栏   = 控制栏
        //   第 3 帧（更小的脏区） clip = 控制栏 ∩ 更小的脏区 = 那个更小的脏区
        //   ...
        //
        // 于是裁剪区只会越收越窄，永远长不回去。两个后果：
        //
        // 1. `App::paint` 里「位图刚重建 → 整块重画」这条补救被悄悄废掉。
        //    第二次 resize 之后位图确实是空白的，但绘制仍然被圈在上一帧那条
        //    控制栏窄带里，屏幕上就是半黑半新，而且再也不会自愈。
        // 2. 某一次传入的脏区与现有裁剪区没有交集时，`IntersectClipRect` 会把
        //    NULLREGION 装进 DC 并**保留**它。之后每次 paint 都在这里 return，
        //    而 `App::paint` 照样 BitBlt、把更新区一 `EndPaint` 清掉——
        //    控制栏从此整个会话冻结不动。
        //
        // `SelectClipRgn(hdc, None)` 的含义是「裁剪区设为整个绘图面」，
        // 正好是每帧开始时想要的起点。返回 FALSE 也不影响：clip box 一定
        // 覆盖整块位图，只是「整块」到底是 1x1 还是整面而已，画完的内容
        // 仍在位图内。
        let _ = SelectClipRgn(hdc, None);
        // 裁剪区设为脏区（已夹到客户区内）。取交集失败时整个绘制跳过：
        // 脏区完全在客户区外意味着没什么要画的，硬画只会把后备位图上
        // 已经正确的像素覆盖成错的。
        // 0 = NULLREGION，即交集为空
        if IntersectClipRect(hdc, dirty.left, dirty.top, dirty.right, dirty.bottom)
            == GDI_REGION_TYPE(0)
        {
            return;
        }
        // 视频区底色。原生视频窗口正常时盖在这一层上面。
        fill(
            hdc,
            theme,
            &RECT {
                left: 0,
                top: 0,
                right: client_w,
                bottom: client_h,
            },
            C_STAGE,
        );
        if s.theatre {
            return;
        }

        fill(hdc, theme, &layout.controls, C_PANEL);
        // 控制栏顶边
        fill(
            hdc,
            theme,
            &RECT {
                left: layout.controls.left,
                top: layout.controls.top,
                right: layout.controls.right,
                bottom: layout.controls.top + 1,
            },
            C_LINE,
        );

        // ---- 第一行：当前时间 / 进度条 / 总时长 ----
        let shown = if s.dragging_seek {
            s.drag_seconds
        } else {
            s.position
        };
        let shown_text = format_time(shown);
        let total_text = format_time(s.duration);

        text_center(
            hdc,
            theme.fonts.mono,
            C_TEXT,
            &shown_text,
            &layout.time_current,
        );
        text_center(
            hdc,
            theme.fonts.mono,
            C_DIM,
            &total_text,
            &layout.time_duration,
        );

        let seek_ratio = if s.duration > 0.0 {
            (shown / s.duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        draw_slider(
            hdc,
            theme,
            &layout.seek,
            seek_ratio,
            s.loaded,
            C_LINE,
            C_ACCENT,
        );

        // ---- 第二行：按钮 / 文件名 / 音量 ----
        let hovered = |h: Hit| s.hover == h;
        draw_button(
            hdc,
            theme,
            &layout.btn_open,
            LABEL_OPEN,
            hovered(Hit::Open),
            false,
        );
        let play_label = if s.paused { "播放" } else { "暂停" };
        draw_button(
            hdc,
            theme,
            &layout.btn_play,
            play_label,
            hovered(Hit::Play),
            !s.loaded,
        );
        draw_button(
            hdc,
            theme,
            &layout.btn_stop,
            "停止",
            hovered(Hit::Stop),
            !s.loaded,
        );

        let name = if s.loaded { s.title } else { "" };
        if !name.is_empty() {
            text_right(hdc, theme.fonts.mono, C_DIM, name, &layout.file_name);
        }

        let mute_label = if s.muted || s.volume <= 0.0 {
            LABEL_MUTE_ON
        } else {
            LABEL_MUTE_OFF
        };
        text_center(hdc, theme.fonts.ui, C_DIM, mute_label, &layout.mute);

        draw_slider(
            hdc,
            theme,
            &layout.volume,
            (s.volume / 100.0).clamp(0.0, 1.0),
            true,
            C_BTN_EDGE,
            C_ACCENT,
        );

        if s.idle {
            text_center(
                hdc,
                theme.fonts.hint,
                C_DIM,
                IDLE_HINT,
                &stage_rect(client_w, layout),
            );
        }
    }
}

/// 把脏区夹进客户区。
///
/// Windows 给的更新区理论上不会超出客户区，但 `dcPaint` 里拿到的是累计的
/// 更新区域，跨窗口 resize 之后可能残留上一帧的旧值。夹一次比在
/// `IntersectClipRect` 上赌它自己裁对更省事。
///
/// 夹完可能是空矩形（`left == right`），那正是「这一帧没什么要画的」的
/// 表达方式：`paint` 会跳过绘制，`App::paint` 会跳过 `BitBlt`。
///
/// `pub(crate)` 是因为 `App::paint` 必须对**同一个**矩形做两件事——按它裁剪
/// 绘制、按它 `BitBlt`。两边各夹一次就可能出现「绘制夹成空、BitBlt 拿原值」
/// 的错位，那会在屏幕上贴出一块杂色并永久失去重绘。
pub(crate) fn clamp_to_client(dirty: &RECT, client_w: i32, client_h: i32) -> RECT {
    RECT {
        left: dirty.left.clamp(0, client_w),
        top: dirty.top.clamp(0, client_h),
        right: dirty.right.clamp(0, client_w),
        bottom: dirty.bottom.clamp(0, client_h),
    }
}

/// 视频区矩形（控制栏以上的那部分）。
fn stage_rect(client_w: i32, layout: &Layout) -> RECT {
    RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: layout.controls.top.max(0),
    }
}

/// 用 `color` 对应的缓存画笔填充矩形。
///
/// 画笔来自 `Brushes` 缓存，不在这里创建也不在这里删——理由见 `Brushes`。
/// DC 上真正被选中的对象由 `FillRect` 内部临时设置，用完即恢复，
/// 所以缓存的画笔不会被别的绘制弄脏，可以安全复用。
unsafe fn fill(hdc: HDC, theme: &mut Theme, rect: &RECT, color: COLORREF) {
    let brush = theme.brush(color);
    FillRect(hdc, rect, brush.h());
}

/// 用 `color` 对应的缓存画笔画 1px 边框。
unsafe fn stroke(hdc: HDC, theme: &mut Theme, rect: &RECT, color: COLORREF) {
    let brush = theme.brush(color);
    FrameRect(hdc, rect, brush.h());
}

/// 在矩形里画居中单行文字。
unsafe fn text_center(hdc: HDC, font: HFONT, color: COLORREF, text: &str, rect: &RECT) {
    text_in(
        hdc,
        font,
        color,
        text,
        rect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
}

/// 右对齐（带省略号）。
unsafe fn text_right(hdc: HDC, font: HFONT, color: COLORREF, text: &str, rect: &RECT) {
    text_in(
        hdc,
        font,
        color,
        text,
        rect,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
}

unsafe fn text_in(
    hdc: HDC,
    font: HFONT,
    color: COLORREF,
    text: &str,
    rect: &RECT,
    flags: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    if text.is_empty() {
        return;
    }
    let old_font = SelectObject(hdc, font.into());
    let old_color = SetTextColor(hdc, color);
    SetBkMode(hdc, TRANSPARENT);

    let mut buf: Vec<u16> = text.encode_utf16().collect();
    buf.push(0);
    let mut r = *rect;
    DrawTextW(hdc, &mut buf, &mut r, flags);

    SetTextColor(hdc, old_color);
    SelectObject(hdc, old_font);
}

/// 画一个滑块：轨道 + 已填充部分 + 方块滑块。
unsafe fn draw_slider(
    hdc: HDC,
    theme: &mut Theme,
    rect: &RECT,
    ratio: f64,
    enabled: bool,
    track_color: COLORREF,
    fill_color: COLORREF,
) {
    let cy = (rect.top + rect.bottom) / 2;
    let w = rect.right - rect.left;
    if w <= 0 {
        return;
    }
    let dim = if enabled { 255 } else { 90 };
    let track = dim_color(track_color, dim);
    let fill_c = dim_color(fill_color, dim);

    // 轨道：以中线为基准，向上下各撑半个厚度
    let thick = px(SLIDER_THICK_DIP, theme.dpi());
    let track_rect = RECT {
        left: rect.left,
        top: cy - thick / 2,
        right: rect.right,
        bottom: cy + (thick + 1) / 2,
    };
    fill(hdc, theme, &track_rect, track);

    // 已填充部分
    let filled = rect.left + (w as f64 * ratio).round() as i32;
    if filled > rect.left {
        fill(
            hdc,
            theme,
            &RECT {
                right: filled.min(rect.right),
                ..track_rect
            },
            fill_c,
        );
    }

    // 滑块
    let t = px(THUMB_DIP, theme.dpi());
    let tx = (filled - t / 2).clamp(rect.left - t / 2, rect.right - t / 2);
    fill(
        hdc,
        theme,
        &RECT {
            left: tx,
            top: cy - t / 2,
            right: tx + t,
            bottom: cy + t / 2,
        },
        dim_color(C_TEXT, dim),
    );
}

/// 按钮：底色 + 1px 边框 + 居中文字。
unsafe fn draw_button(
    hdc: HDC,
    theme: &mut Theme,
    rect: &RECT,
    label: &str,
    hovered: bool,
    disabled: bool,
) {
    let bg = if hovered { C_BTN_HOVER } else { C_BTN };
    let edge = if hovered { C_BTN_EDGE } else { C_LINE };
    fill(hdc, theme, rect, bg);
    stroke(hdc, theme, rect, edge);

    let color = if disabled { C_DIM } else { C_TEXT };
    text_center(hdc, theme.fonts.ui, color, label, rect);
}

/// 按比例把颜色调暗（用于禁用态）。
fn dim_color(color: COLORREF, alpha: u8) -> COLORREF {
    let a = u32::from(alpha);
    let c = color.0;
    let r = ((c & 0xff) * a) / 255;
    let g = (((c >> 8) & 0xff) * a) / 255;
    let b = (((c >> 16) & 0xff) * a) / 255;
    COLORREF(r | (g << 8) | (b << 16))
}

/// 秒 -> mm:ss / h:mm:ss
pub fn format_time(seconds: f64) -> String {
    let seconds = if seconds.is_finite() && seconds > 0.0 {
        seconds
    } else {
        0.0
    };
    let total = seconds.floor() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

fn rect(left: i32, top: i32, width: i32, height: i32) -> RECT {
    RECT {
        left,
        top,
        right: left + width.max(0),
        bottom: top + height.max(0),
    }
}

fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 时间格式化() {
        assert_eq!(format_time(0.0), "00:00");
        assert_eq!(format_time(59.9), "00:59");
        assert_eq!(format_time(61.0), "01:01");
        assert_eq!(format_time(3661.0), "1:01:01");
        // mpv 在没有活动文件时会把 time-pos 报成 nan / 负数，不能显示成怪值
        assert_eq!(format_time(f64::NAN), "00:00");
        assert_eq!(format_time(-5.0), "00:00");
    }

    #[test]
    fn dip_按_dpi_换算() {
        assert_eq!(px(96, 96), 96);
        assert_eq!(px(96, 120), 120);
        assert_eq!(px(96, 144), 144);
        assert_eq!(px(62, 120), 78); // 控制栏 62 DIP -> 125% 下 78 物理像素
    }

    #[test]
    fn 影院模式下控制栏收起且不参与命中() {
        let l = Layout::new(1375, 900, 120, true);
        assert_eq!(l.controls.top, l.controls.bottom);
        assert!(contains(&l.seek, l.seek.left + 1, l.seek.top + 1));
        // 影院模式下点控制栏旧位置应该算点到了视频区
        assert_eq!(l.hit(l.btn_play.left + 1, l.btn_play.top + 1), Hit::Video);
    }

    #[test]
    fn 正常模式命中与比例() {
        let l = Layout::new(1375, 900, 120, false);
        assert_eq!(l.hit(l.btn_play.left + 1, l.btn_play.top + 1), Hit::Play);
        assert_eq!(l.hit(l.seek.left + 1, l.seek.top + 1), Hit::Seek);
        assert_eq!(l.hit(l.volume.right - 1, l.volume.top + 1), Hit::Volume);
        assert_eq!(l.hit(l.mute.left + 1, l.mute.top + 1), Hit::Mute);
        // 视频区
        assert_eq!(l.hit(10, 10), Hit::Video);
        // 进度条正中 = 50%
        let mid = (l.seek.left + l.seek.right) / 2;
        let r = l.ratio(Hit::Seek, mid);
        assert!((r - 0.5).abs() < 0.02, "ratio={r}");
        // 越界要夹住
        assert_eq!(l.ratio(Hit::Seek, l.seek.left - 50), 0.0);
        assert_eq!(l.ratio(Hit::Seek, l.seek.right + 50), 1.0);
    }

    #[test]
    fn 布局不重叠() {
        let l = Layout::new(1375, 900, 120, false);
        // 视频区底边 = 控制栏顶边
        assert_eq!(l.controls.top, 900 - px(62, 120));
        assert!(l.time_current.right < l.seek.left);
        assert!(l.seek.right < l.time_duration.left);
        assert!(l.btn_stop.right <= l.file_name.left + 1);
        assert!(l.file_name.right <= l.mute.left + 1);
        assert!(l.mute.right <= l.volume.left + 1);
        assert!(l.volume.right <= 1375);
    }

    #[test]
    fn 窄窗口下也不越界() {
        // 最小宽度 480 DIP * 96 = 480 物理像素（100% 缩放）
        let l = Layout::new(480, 320, 96, false);
        assert!(l.volume.right <= 480, "volume.right={}", l.volume.right);
        assert!(l.btn_stop.right < 480);
        // 文件名区域宽度可能为 0，但不应为负（rect 已经夹过）
        assert!(l.file_name.right >= l.file_name.left);
    }

    /// 画刷必须按颜色复用同一个句柄。
    ///
    /// 回归点：原来每次 `fill` 都是 `CreateSolidBrush` + `DeleteObject`，
    /// 一帧十来次、每秒四帧，纯粹的白开销。这里钉住「同色同句柄」，
    /// 顺手也钉住「异色异句柄」——只查前一条的话，把实现改成永远返回
    /// 第一个刷子（所有颜色画成一样）也能过。
    #[test]
    fn 画刷按颜色复用() {
        let mut b = Brushes::new();
        let first = b.get(C_PANEL).expect("第一次要颜色应当建出画刷");
        assert!(!first.is_invalid(), "CreateSolidBrush 失败");
        assert_eq!(
            b.get(C_PANEL),
            Some(first),
            "同色没有复用，白白重建了 GDI 对象"
        );
        // 中间隔着别的颜色再要一次原来的色，仍应命中同一句柄
        let other = b.get(C_ACCENT).expect("第二个颜色");
        assert_ne!(other, first, "不同颜色用了同一个画刷，界面会画成一片同色");
        assert_eq!(b.get(C_PANEL), Some(first));
    }

    /// `dim_color` 的结果也要走缓存：它只依赖入参，
    /// 所以相同 alpha 的结果必须命中同一句柄。
    ///
    /// 这里顺带钉住「禁用态的颜色不会因为是算出来的就漏进缓存之外」——
    /// `draw_slider` 每帧要对 C_LINE / C_ACCENT / C_TEXT 各调一次 `dim_color`，
    /// 算出来的值一样才能命中缓存。
    #[test]
    fn 调暗色同样命中缓存() {
        let mut b = Brushes::new();
        let on = dim_color(C_ACCENT, 255);
        let off = dim_color(C_ACCENT, 90);
        let first = b.get(on).expect("正常态颜色");
        assert_eq!(b.get(dim_color(C_ACCENT, 255)), Some(first));
        assert_eq!(dim_color(C_ACCENT, 255), on, "同样的输入必须算出同样的颜色");
        // 同一个函数算两次 90 也要能复用，但要和 255 的那个不是同一个
        let dim = b.get(off).expect("禁用态颜色");
        assert_eq!(b.get(dim_color(C_ACCENT, 90)), Some(dim));
        assert_ne!(dim, first, "禁用态和正常态画成了同一个颜色");
    }

    /// 缓存装满之后必须**拒缓存**（返回 `None`），而不是偷偷再塞一个。
    ///
    /// 这条钉的是 `Theme::brush` 的 `owned` 分支。超上限还往里塞的话，
    /// 缓存会变成无界的（万一将来有人拿动画颜色当 key）；
    /// 塞不下了还返回 `Some` 的话，`BrushHandle` 会以为这画笔归缓存管、
    /// 于是永远不删——每帧漏一个 GDI 句柄，跑久了进程就难看查地卡住。
    #[test]
    fn 缓存满了之后拒缓存() {
        let mut b = Brushes::new();
        let mut cached = 0usize;
        let mut refused = 0usize;
        // 颜色取 0..=73，全部互不相同
        for i in 0..(MAX_CACHED_BRUSHES as u32 + 10) {
            let color = COLORREF(i);
            match b.get(color) {
                Some(_) => cached += 1,
                None => refused += 1,
            }
        }
        assert_eq!(
            cached, MAX_CACHED_BRUSHES,
            "缓存不该超过上限 MAX_CACHED_BRUSHES"
        );
        assert_eq!(refused, 10, "超出的 10 个颜色都该被拒掉");
        assert_eq!(b.items.len(), MAX_CACHED_BRUSHES);
        // 装满之后，已缓存的颜色仍然命中——不能让后来的调用挤掉先前的
        let first = b.items[0];
        assert_eq!(b.get(first.0), Some(first.1));
    }

    /// 脏区要夹到客户区内，夹完不能变成反向矩形。
    ///
    /// 回归点：`IntersectClipRect` 对 left > right 的矩形返回 0（整个绘制被跳过），
    /// 而 Windows 在窗口被移动/遮挡时给出的更新区偶尔会有负坐标。不夹的话
    /// 表现是「偶发地整个控制栏不刷新」，比不裁剪更难查。
    #[test]
    fn 脏区夹到客户区内() {
        let r = clamp_to_client(
            &RECT {
                left: -5,
                top: -5,
                right: 100,
                bottom: 100,
            },
            1375,
            900,
        );
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 0, 100, 100));

        // 越界的右下角被裁到客户区边界
        let r = clamp_to_client(
            &RECT {
                left: 1300,
                top: 880,
                right: 2000,
                bottom: 2000,
            },
            1375,
            900,
        );
        assert_eq!((r.right, r.bottom), (1375, 900));

        // 完全在客户区外的脏区夹成 0x0，`IntersectClipRect` 会返回 0
        let r = clamp_to_client(
            &RECT {
                left: 2000,
                top: 2000,
                right: 2100,
                bottom: 2100,
            },
            1375,
            900,
        );
        assert_eq!((r.left, r.top, r.right, r.bottom), (1375, 900, 1375, 900));
    }
}
