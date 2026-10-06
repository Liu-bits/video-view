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
//!
//! ## 多语言
//!
//! 文案不在本模块里散落，而是由 `lang::Strings` 统一提供。控件**宽度全部
//! 按当前文案实测计算**（见 `Metrics`），没有写死的 DIP 常量——中文两字与
//! 英文单词长度差很多，写死必然有一边被截断或另一边空一大块。

use crate::lang;
use crate::track::TrackRow;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, FrameRect, GetDC,
    GetMonitorInfoW, GetTextExtentPoint32W, GetTextMetricsW, IntersectClipRect, MonitorFromWindow,
    ReleaseDC, SelectClipRgn, SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_NORMAL, GDI_REGION_TYPE, HBRUSH, HDC, HFONT,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, TEXTMETRICW, TRANSPARENT,
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
/// 轨道菜单：光标所在行。比悬停更深，两者要能一眼分开。
const C_ROW_HI: COLORREF = rgb(0x33, 0x3a, 0x41);
/// 轨道菜单：鼠标悬停行。
const C_ROW_HOVER: COLORREF = rgb(0x26, 0x2b, 0x31);
/// 轨道菜单：分组标题的底色，比面板底色略深，段落感来自这里。
const C_PANEL_DEEP: COLORREF = rgb(0x10, 0x12, 0x15);

/// 控制栏高度（同时也是视频区与控制栏的分界线）。
pub const CTRL_H_DIP: i32 = 62;

/// 控件距离左右边缘的内边距。
///
/// 原来是 10，看着「左半边全糊在边上」——三颗按钮紧贴左边框，而右边
/// 音量组挤成一团，中间又是空的，视觉重心整个压在左侧。14 加上后面
/// 按文字算出来的按钮宽度，三组控件才分布得开。
const PAD_X_DIP: i32 = 14;
const PAD_TOP_DIP: i32 = 6;
/// 同组控件之间的间距。
const GAP_DIP: i32 = 8;
/// 左边按钮组 / 中间文件名 / 右边音量组**之间**的间距。
///
/// 比 `GAP_DIP` 大：这三组是三个独立的信息块，而不是一串控件。组内 8、
/// 组间 20，眼睛扫过去能自然把它们读成三块。
const CLUSTER_GAP_DIP: i32 = 20;
const TRACK_ROW_DIP: i32 = 16;

/// 按钮高度下限（实际取「文字高 + 上下内边距」与它的较大者）。
const BTN_H_DIP: i32 = 26;
/// 按钮内文字左右的内边距。
///
/// 按钮宽度不再写死，而是「实测文字宽 + 2 × 这个」。原来 `BTN_W_DIP = 48`
/// 是照着中文两字量的：英文 "Pause" / "Volume" 根本放不下，会被
/// `DT_END_ELLIPSIS` 截成 "Pau…" —— 英文版必须整体重做按钮尺寸的根因。
const BTN_PAD_X_DIP: i32 = 12;
/// 按钮内文字上下的内边距。
const BTN_PAD_Y_DIP: i32 = 5;

/// 时间码列宽下限。实际宽度取「实测 `88:88:88` 宽」与它的较大者，
/// 这样 h:mm:ss 也不会被切。
const TIME_W_DIP: i32 = 52;
const VOL_W_DIP: i32 = 96;
const SLIDER_THICK_DIP: i32 = 3;
const THUMB_DIP: i32 = 11;

/// 诊断 / 快捷键面板的排版常量。
///
/// 面板沿用控制栏的硬边风格：无渐变、无圆角、无半透明。GDI 的 `FillRect`
/// 本来也没有 alpha 通道，硬色块反而和 `C_LINE` 边框、`C_DIM` 文字这套
/// 语言是一致的。
const PANEL_PAD_X_DIP: i32 = 14;
/// 面板上下各留一点，不让第一行/最后一行贴着边框。
const PANEL_PAD_Y_DIP: i32 = 4;
/// 面板里标签列与值列之间的间距。
const PANEL_COL_GAP_DIP: i32 = 14;

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
    /// 诊断 / 快捷键面板的区域。没有面板时是一个零高度的矩形（top == bottom）。
    pub panel: RECT,
    /// 面板 + 控制栏的并集（两者上下相邻，所以是一个矩形）。
    ///
    /// 上层把脏区裁到这里，而 `paint` 把这块整体填成面板色——两边必须用
    /// 同一个矩形，见 `paint` 里关于「有文件在播时」的说明。
    pub chrome: RECT,
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

    // ---- 面板行的几何（轨道菜单的命中测试要用）----
    //
    // 这三个值在 `Layout::new` 里算好存下来，而不是让 `hit` 现场重算：
    // `row_height` 依赖 `Metrics` 与 dpi，而 `hit` 只有坐标。重算的话要么
    // 把 Metrics 也塞进 Layout（布局对象开始背着字体度量，职责变味），
    // 要么在 `hit` 里用常数行高（和绘制用的行高差几个像素，点中的行会偏）。
    //
    /// 面板里**第一行**的顶边（与 `draw_panel` 里那个 `top` 同一个值）
    pub panel_row_top: i32,
    /// 一行的高度（`draw_panel` 与命中测试必须用同一个）
    pub panel_row_h: i32,
    /// 真正画出来的行数。
    ///
    /// **不一定**等于 `panel_rows`：面板高度被 `min(controls_top)` 夹过时
    /// 底部几行会被截掉。用 `panel_rows` 判界的话，用户能点到一条画在
    /// 屏幕外（被控制栏盖住）的轨道，选中之后界面上却看不到变化。
    pub panel_drawn_rows: usize,
}

impl Layout {
    /// 按客户区尺寸、当前 DPI 与文案实测尺寸计算布局。
    ///
    /// `client` 的宽高是物理像素（本进程是 per-monitor DPI 感知，
    /// Win32 坐标与客户区尺寸都已经是设备像素，不需要任何虚拟化换算）。
    ///
    /// `m` 是当前语言 + DPI 下量出来的文字尺寸，**所有控件宽度都由它算出**，
    /// 不再有写死的 DIP 常量。这样中文两字与英文单词都恰好放下，
    /// 切换语言不需要另一套布局。
    ///
    /// `panel_rows` 是面板要显示多少行，0 表示不显示。面板画在控制栏**上面**，
    /// 所以控制栏（以及 mpv 的画面子窗口）整体上移这么多——画面区随之变矮。
    ///
    /// ## 为什么面板是「把控制栏顶上去」而不是浮在画面上
    ///
    /// mpv 的画面是一个**原生子窗口**，它整个盖在视频区上，而我们在主窗口的
    /// `WM_PAINT` 里画的东西全在它**下面**。想让浮层盖住画面就得再开一个
    /// 置顶的子窗口并自己管 Z 序（resize / 影院 / 全屏切换都得重排，还要
    /// 处理鼠标穿透），那是几十行容易出 z 序 bug 的窗口代码。
    ///
    /// 让控制栏长高一截则是零风险：布局、脏区裁剪、画面子窗口尺寸全都走
    /// 现成的那条路径（`relayout` 里本来就是同一个函数在算），而且面板只在
    /// 用户主动打开时存在，代价是那段时间画面矮一点。诊断面板本来就是
    /// 「暂停下来看一眼」的东西，这个取舍划算。
    pub fn new(
        client_w: i32,
        client_h: i32,
        dpi: u32,
        theatre: bool,
        panel_rows: usize,
        m: &Metrics,
    ) -> Self {
        let pad_x = px(PAD_X_DIP, dpi);
        let gap = px(GAP_DIP, dpi);
        let cluster_gap = px(CLUSTER_GAP_DIP, dpi);
        let ctrl_h = if theatre { 0 } else { px(CTRL_H_DIP, dpi) };

        // 控制栏**永远**贴着客户区底部，面板在它上面。
        //
        // 这一点很容易写反：把 `controls.top` 设成「底部减去控制栏再减去面板」
        // 会让 `controls` 这个矩形把面板那一段也包进去，于是
        // `controls.bottom - controls.top` 变成「控制栏 + 面板」，
        // 按钮的位置也跟着偏到面板里去（实测：控制栏高度从 78 变成 247，
        // 播放键画到了面板那一行）。面板是独立的一块，两块相邻而不是包含。
        let controls_top = (client_h - ctrl_h).max(0);

        // 影院模式下不显示面板：影院模式的定义就是「什么都没有」
        let panel_h = if theatre {
            0
        } else {
            // 客户区不够高时夹住，否则面板会把控制栏顶到屏幕外
            panel_height(panel_rows, m, dpi).min(controls_top)
        };
        let panel_top = (controls_top - panel_h).max(0);

        let mut controls = RECT {
            left: 0,
            top: controls_top,
            right: client_w,
            bottom: client_h,
        };
        // 面板紧贴控制栏上方
        let panel = RECT {
            left: 0,
            top: panel_top,
            right: client_w,
            bottom: controls_top,
        };
        // 面板与控制栏上下相邻，并集就是从 panel.top 到客户区底的一条
        let chrome = RECT {
            left: 0,
            top: panel_top,
            right: client_w,
            bottom: client_h,
        };

        let pad_top = px(PAD_TOP_DIP, dpi);
        let track_h = px(TRACK_ROW_DIP, dpi);
        // 量不出来（0）时退回原常量，至少保证时间码有地方显示
        let time_w = px(TIME_W_DIP, dpi).max(m.time_w);
        let track_y = controls.top + pad_top;

        let time_current = rect(pad_x, track_y, time_w, track_h);
        let time_duration = rect(client_w - pad_x - time_w, track_y, time_w, track_h);

        let seek_x = time_current.right + gap;
        let seek_w = (time_duration.left - gap - seek_x).max(gap);
        let seek = rect(seek_x, track_y, seek_w, track_h);

        // ---- 第二行：左边按钮组 / 中间文件名 / 右边音量组 ----
        //
        // 按钮宽度 = 实测文字宽 + 左右内边距。文字宽度为 0（度量失败）时
        // 仍然给一个 48 DIP 的兜底，免得整排塌成看不见的一条。
        let btn_pad = px(BTN_PAD_X_DIP, dpi);
        let btn_pad_y = px(BTN_PAD_Y_DIP, dpi);
        let btn_w = |text_w: i32| (text_w.max(px(48, dpi)) + 2 * btn_pad).max(px(48, dpi));
        // 高度取「文字高 + 上下内边距」与下限的较大者：换了语言或 DPI 之后
        // 行高不会把文字挤出框外。
        let btn_h = (m.ui_h + 2 * btn_pad_y).max(px(BTN_H_DIP, dpi));
        let btn_y = track_y + track_h + gap;

        let open_w = btn_w(m.open);
        let play_w = btn_w(m.play_pause_w());
        let stop_w = btn_w(m.stop);

        let btn_open = rect(pad_x, btn_y, open_w, btn_h);
        let btn_play = rect(btn_open.right + gap, btn_y, play_w, btn_h);
        let btn_stop = rect(btn_play.right + gap, btn_y, stop_w, btn_h);
        let left_end = btn_stop.right;

        // 右边音量组
        let vol_w = px(VOL_W_DIP, dpi);
        let mute_w = btn_w(m.mute_w());
        let volume = rect(client_w - pad_x - vol_w, btn_y, vol_w, btn_h);
        let mute = rect(volume.left - gap - mute_w, btn_y, mute_w, btn_h);
        let right_start = mute.left;

        // 中间留给文件名。两侧各留一个「组间距」，宽度夹到 0 以上——
        // 窗口窄到放不下时文件名区域消失，而不是把左右两组挤到重叠。
        let file_left = left_end + cluster_gap;
        let file_right = (right_start - cluster_gap).max(file_left);
        let file_name = rect(file_left, btn_y, file_right - file_left, btn_h);

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

        // 面板行的几何。`row_top` 必须和 `draw_panel` 里的 `top` 是同一个
        // 表达式，否则命中测试会整体偏一个内边距（实测差 PANEL_PAD_Y_DIP）。
        let row_h = row_height(m, dpi);
        let row_top = panel_top + px(PANEL_PAD_Y_DIP, dpi);
        // 画几行：与 `draw_panel` 的循环条件（`y < controls.top`）逐字对应，
        // 再**夹到真实行数**。
        //
        // 那个 `min` 不能省：面板没被夹住时高度正好是
        // `rows * row_h + 2 * pad_y`，于是 `avail = rows * row_h + pad_y`，
        // `ceil(avail / row_h)` 会算出 `rows + 1` —— 底部那半个内边距被
        // 当成了第 `rows + 1` 行。命中测试就会在面板最下面那一条内边距里
        // 报出一个根本不存在的行号（`Hit::TrackRow(rows)`）。
        let drawn_rows = if row_h > 0 && controls_top > row_top {
            let avail = controls_top - row_top;
            // 整数向上取整：最后一行只露出一半也算一行（与绘制行为一致）
            let fits = ((avail + row_h - 1) / row_h).max(0) as usize;
            fits.min(panel_rows)
        } else {
            0
        };

        Self {
            controls,
            panel,
            chrome,
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
            panel_row_top: row_top,
            panel_row_h: row_h,
            panel_drawn_rows: drawn_rows,
        }
    }

    /// 坐标落在面板的第几行。
    ///
    /// 与 `hit` 分开是因为鼠标移动也要用它（悬停高亮），而 `hit` 是
    /// 点击用的。共用一份实现免得两边的边界条件慢慢长歪。
    ///
    /// **上下内边距都不属于任何行。** `panel_row_top` 比 `panel.top` 低一个
    /// `PANEL_PAD_Y_DIP`，所以 `contains` 会把顶部那条留白也算进面板；不加
    /// 显式判断的话 `y - panel_row_top` 是负数，Rust 的整数除法向 0 截断
    /// （`-3 / 20 == 0`），顶部留白会被判成第 0 行 —— 于是「点面板最上面
    /// 那条空白」会选中第一条轨。底部的留白本来就靠
    /// `i < panel_drawn_rows` 挡住了，这里让两边一致。
    pub fn panel_row_at(&self, x: i32, y: i32) -> Option<usize> {
        if self.theatre || self.panel_drawn_rows == 0 || self.panel_row_h <= 0 {
            return None;
        }
        if !contains(&self.panel, x, y) || y < self.panel_row_top {
            return None;
        }
        let i = ((y - self.panel_row_top) / self.panel_row_h).max(0) as usize;
        if i < self.panel_drawn_rows {
            Some(i)
        } else {
            None
        }
    }

    /// 命中测试。坐标是相对客户区的物理像素。
    pub fn hit(&self, x: i32, y: i32) -> Hit {
        if self.theatre {
            return Hit::Video;
        }
        // 面板区域不是控件，点在上面的效果等同于点画面（单击切播放/暂停）。
        // 不这么归类的话 `Hit::None` 会让光标变成箭头、单击什么也不做。
        //
        // 轨道菜单例外：那里面每一行都是可点的，所以先问一次落在第几行。
        // 注意 `hit` 只报几何、不管语义 —— 面板不是菜单时上层会忽略这个
        // `Hit::TrackRow`（诊断/快捷键面板里点行仍然是「点画面」）。
        if contains(&self.panel, x, y) {
            if let Some(row) = self.panel_row_at(x, y) {
                return Hit::TrackRow(row);
            }
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
    /// 轨道菜单的第几行（**行**下标，含分组标题）
    TrackRow(usize),
}

// ---------------------------------------------------------------- 面板

/// 控制栏上方那块可选区域的内容。
///
/// 两种形态都是「左列 + 右列」，区别只在左列是什么：诊断面板左列是标签
/// （本地化），快捷键面板左列是按键（语言中立）。列宽都取 `Metrics` 里实测的
/// 最大值，所以中英文都不会挤在一起。
#[derive(Debug, Clone, Copy)]
pub enum Panel<'a> {
    /// 解码诊断：`diag::Diagnostics::rows` 给出的 `(标签, 值)`
    Stats(&'a [(String, String)]),
    /// 快捷键总览
    Help(&'a [(&'static str, &'static str)]),
    /// 音轨 / 字幕轨菜单
    Tracks {
        rows: &'a [TrackRow],
        /// 光标停在第几行（含不可选的分组标题行，所以是**行**下标不是
        /// 「可选行」下标 —— 这样 ↑↓ 与鼠标点击用的是同一套坐标）
        cursor: usize,
    },
}

impl Panel<'_> {
    /// 这一行是不是诊断面板的结论行。
    ///
    /// 结论行用强调色，是为了让「正常 / 有问题」在一屏字里第一眼就被看到。
    /// 快捷键面板与轨道菜单没有结论行。
    fn is_stats_last(&self, i: usize) -> bool {
        matches!(self, Panel::Stats(rows) if i + 1 == rows.len())
    }
}

// ---------------------------------------------------------------- 绘制状态

/// 当前语言 + 当前 DPI 下，各段文字的**实测**尺寸（物理像素）。
///
/// 这些值由 `Theme::new` 在建好字体后量一次得到，`Layout` 据此算控件尺寸。
///
/// ## 为什么必须实测，不能写死常量
///
/// 之前所有宽度都是 DIP 常量（`BTN_W_DIP = 48`、`MUTE_W_DIP = 30`），
/// 那些数字是照着中文两字量的：14 DIP 的 "打开" 约 28px，塞进 48 还算宽裕，
/// 但塞进 30 就只剩 1 DIP 余量 —— 这就是实测截图里「音量」两个字贴着框、
/// 右边又紧挨滑块的原因。而英文的 "Pause"（约 40px）、"Volume"（约 48px）
/// 在 48 DIP 的框里直接放不下，会被截成 "Pau…" / "Vol…"。中文能用常量的
/// 前提是「文案长度已知且固定」，加了英文之后这个前提就不成立了。
///
/// 量一次的成本可以忽略：`Theme` 只在 DPI 或语言变化时重建，一��创建
/// 里多 7 次 `GetTextExtentPoint32W`。
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    /// 按钮 / 标签文字在 UI 字体下的宽度。
    pub open: i32,
    pub play: i32,
    pub pause: i32,
    pub stop: i32,
    pub mute_on: i32,
    pub mute_off: i32,
    /// UI 字体单行高度，用作按钮高度下限。
    pub ui_h: i32,
    /// 时间码列宽，按最坏情况 `88:88:88` 量。
    pub time_w: i32,
    /// 面板里标签列的宽度：取所有诊断标签里最宽的那个。
    ///
    /// 必须实测：中文「丢帧」两字和英文 `Dropped` 差一倍多，写死任何一个
    /// 都会让另一边要么被截、要么和值之间多出一大截空隙。
    pub diag_label_w: i32,
    /// 快捷键面板里按键列的宽度，取所有按键里最宽的。
    pub help_key_w: i32,
    /// 面板一行的高度（等宽字体行高 + 上下各一点留白）。
    pub row_h: i32,
}

impl Metrics {
    /// 播放 / 暂停按钮取两者较宽的那个。
    ///
    /// 这样切换播放状态时按钮宽度不变、整排按钮不会左右跳。一个会呼吸的
    /// 布局比一个略宽的按钮糟糕得多。
    pub fn play_pause_w(&self) -> i32 {
        self.play.max(self.pause)
    }

    /// 静音 / 音量标签取两者较宽的那个，理由同上。
    pub fn mute_w(&self) -> i32 {
        self.mute_on.max(self.mute_off)
    }
}

/// 面板里一行的高度（物理像素）。
///
/// **布局与绘制必须用同一个函数。** 原来这里算一遍（`m.row_h` 为 0 时退回
/// `px(FONT_MONO_DIP + 6, 96)`——DPI 写死成 96），`draw_panel` 里又用
/// `m.row_h.max(px(FONT_MONO_DIP, dpi))` 算一遍。两个表达式不等：150% 缩放下
/// 布局算出的面板比绘制需要的高 2 像素，最后一行文字的下沿被面板下沿切掉。
/// 这类「同一个量在两个地方各算一次」的错只有把两处收敛成一个函数才根治。
fn row_height(m: &Metrics, dpi: u32) -> i32 {
    let floor = px(FONT_MONO_DIP + 4, dpi);
    if m.row_h > 0 {
        m.row_h.max(floor)
    } else {
        floor
    }
}

/// 面板总高度（物理像素）。
///
/// 行数乘行高，加上上下各一点内边距。没有面板时返回 0。
pub fn panel_height(rows: usize, m: &Metrics, dpi: u32) -> i32 {
    if rows == 0 {
        return 0;
    }
    rows as i32 * row_height(m, dpi) + 2 * px(PANEL_PAD_Y_DIP, dpi)
}

/// 面板里两列之间的间距。
fn panel_col_gap(dpi: u32) -> i32 {
    px(PANEL_COL_GAP_DIP, dpi)
}

/// 量一段文字在 `font` 下的宽度（物理像素）。
fn measure_w(hdc: HDC, font: HFONT, text: &str) -> i32 {
    // 换字体失败（字体为空、或 DC 状态异常）时不量，直接 0。
    // 原来是无条件换、量、再无条件换回去，而 `old` 可能就是 NULL——
    // 那等于往 DC 上选了个空对象。`Layout` 对 0 宽度有下限兜底，
    // 控件不会塌掉。
    let old = unsafe { SelectObject(hdc, font.into()) };
    if old.is_invalid() {
        return 0;
    }
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    buf.push(0);
    // 签名要的是 `*mut SIZE`，所以这里必须传 `&mut`。clippy 的
    // needless_pass_by_ref_mut 在这个 FFI 绑定上是误报。
    let mut size = SIZE { cx: 0, cy: 0 };
    // 用不带 DT_CALCRECT 的版本：只要宽度，高度从 tmHeight 取。
    // 返回 FALSE 时返回 0，调用方会走「至少给个下限」的分支。
    let ok = unsafe { GetTextExtentPoint32W(hdc, &buf, &mut size) };
    unsafe {
        let _ = SelectObject(hdc, old);
    }
    if ok.as_bool() {
        size.cx
    } else {
        0
    }
}

/// 量一段文字在 `font` 下的单行高度（物理像素）。
fn measure_h(hdc: HDC, font: HFONT) -> i32 {
    // 同 `measure_w`：换字体失败就不量，也不往 DC 上塞空对象。
    let old = unsafe { SelectObject(hdc, font.into()) };
    if old.is_invalid() {
        return 0;
    }
    let mut tm = TEXTMETRICW::default();
    let got = unsafe { GetTextMetricsW(hdc, &mut tm) };
    unsafe {
        let _ = SelectObject(hdc, old);
    }
    if got.as_bool() {
        // tmHeight 含上下伸部部分，用它当行高最接近 DrawTextW 的排版结果
        tm.tmHeight
    } else {
        0
    }
}

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
    /// 轨道菜单：鼠标停在第几行（用于「悬停即选中」的预览高亮）。
    pub hover_row: Option<usize>,
    /// 当前语言的文案。
    pub strings: &'a lang::Strings,
    /// 要显示的面板。`None` = 不显示（这时控制栏顶到客户区底部）。
    pub panel: Option<Panel<'a>>,
}

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

/// 量出所有文案在当前字体下的尺寸。
///
/// 只借一次屏幕 DC：借/还各是一次系统调用，而 `GetTextExtentPoint32W`
/// 本来就必须在某个 DC 上跑。
fn measure_metrics(fonts: &Fonts, strings: &lang::Strings) -> Metrics {
    // DC 借不出来（极少见）就让全部尺寸为 0，由 Layout 的下限兜底。
    // windows 0.62 的 `GetDC` 返回裸 `HDC`，借失败是空句柄而不是 Option。
    let hdc = unsafe { GetDC(None) };
    if hdc.is_invalid() {
        return Metrics {
            open: 0,
            play: 0,
            pause: 0,
            stop: 0,
            mute_on: 0,
            mute_off: 0,
            ui_h: 0,
            time_w: 0,
            diag_label_w: 0,
            help_key_w: 0,
            row_h: 0,
        };
    };
    let ui = fonts.ui;
    let mono = fonts.mono;
    let m = Metrics {
        open: measure_w(hdc, ui, strings.btn_open),
        play: measure_w(hdc, ui, strings.btn_play),
        pause: measure_w(hdc, ui, strings.btn_pause),
        stop: measure_w(hdc, ui, strings.btn_stop),
        mute_on: measure_w(hdc, ui, strings.mute_on),
        mute_off: measure_w(hdc, ui, strings.mute_off),
        ui_h: measure_h(hdc, ui),
        // h:mm:ss 是最坏情况，按它量一列的宽度：短的不会浪费，长的不会被切
        time_w: measure_w(hdc, mono, "88:88:88"),
        // 标签列 / 按键列取各自行里最宽的那个。`max()` 起点是 0（度量失败），
        // 绘制侧对 0 有兜底，不会塌成一列贴左边
        diag_label_w: strings
            .diag_labels()
            .iter()
            .map(|t| measure_w(hdc, ui, t))
            .max()
            .unwrap_or(0),
        help_key_w: strings
            .help_rows()
            .iter()
            .map(|(k, _)| measure_w(hdc, mono, k))
            .max()
            .unwrap_or(0),
        // 行高按等宽字体量 + 一点上下留白，值列与标签列用同一个字体
        row_h: measure_h(hdc, mono) + 4,
    };
    unsafe {
        let _ = ReleaseDC(None, hdc);
    }
    m
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
    /// 各段文字的实测尺寸，`Layout` 靠它算控件大小。
    pub metrics: Metrics,
    /// 当前语言。字体之外只有语言变了也要重建（文案宽度不同）。
    lang: lang::Lang,
}

impl Theme {
    /// 按 DPI 与语言建立绘制资源。
    ///
    /// 建立过程中用屏幕 DC 量一遍所有文案宽度。这里用屏幕 DC 而不是目标
    /// 窗口的 DC 是安全的：本模块的字体都用 `CreateFontW` 的**负高度**
    /// （字符高度，单位物理像素）创建，字形大小已经是绝对值、不再受 DC 的
    /// DPI 影响，所以在哪个 DC 上量出来的宽度都一样。
    pub fn new(dpi: u32, strings: &lang::Strings) -> Self {
        let fonts = Fonts::new(dpi);
        // 量不出来时（极端情况下 GetTextExtentPoint32W 失败）全部为 0，
        // `Layout` 里有「至少给个下限」的兜底，不会塌成 0 宽。
        let metrics = measure_metrics(&fonts, strings);
        Self {
            fonts,
            brushes: Brushes::new(),
            metrics,
            lang: strings.lang,
        }
    }

    /// 字体是否还适用于当前 DPI 与语言。DPI 变了或换了语言就整套重建。
    pub fn matches(&self, dpi: u32, lang: lang::Lang) -> bool {
        self.fonts.matches(dpi) && self.lang == lang
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
        //
        // **已经有文件在播时整块跳过**：mpv 的子窗口尺寸是
        // `0,0,client_w,controls.top`（见 `App::relayout`），把视频区整个盖住，
        // 影院模式下 `controls.top == client_h`，连整块客户区都盖住。
        // 这时再填一遍是纯浪费，而且不是「便宜一点点的浪费」：4K 下这是
        // 3840x2160 = 830 万次写像素，每次 `WM_PAINT` 都做一遍；同时它还把这
        // 8.3MB 区域的每一页都摸成驻留，WorkingSet 里凭空多出几十 MB。
        //
        // `App::paint` 同时把脏区夹到 `layout.chrome`（面板 + 控制栏），
        // 所以这里跳过的区域也不会被 BitBlt 贴出去——两边必须一起改，
        // 只改一个会让屏幕上出现旧画面。
        if !s.loaded {
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
        }
        if s.theatre {
            return;
        }

        // 面板与控制栏一起填。分两次填看着也行，但面板与控制栏之间那条缝
        // 在某些缩放下会露出后备位图里的旧像素（两次 FillRect 的边界各自
        // 取整，中间的 1px 谁都不画）。一次填掉就没这个问题。
        fill(hdc, theme, &layout.chrome, C_PANEL);
        // 整块 chrome 的顶边。有面板时这条线是面板的上沿，没有面板时
        // 就是控制栏的上沿——所以永远画在 chrome.top
        fill(
            hdc,
            theme,
            &RECT {
                left: layout.chrome.left,
                top: layout.chrome.top,
                right: layout.chrome.right,
                bottom: layout.chrome.top + 1,
            },
            C_LINE,
        );

        // ---- 面板（诊断 / 快捷键 / 轨道菜单）----
        if let Some(panel) = s.panel {
            draw_panel(hdc, theme, layout, panel, s.hover_row, s.strings);
        }

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
        //
        // 三组控件：左边按钮、中间文件名、右边音量。文件名的矩形就是中间
        // 那一整段（两側各留一个组间距），绘制时居中 —— 这样视线在
        // 「按钮 — 文件名 — 音量」之间均匀分布，而不是全部堆在左 1/4。
        let hovered = |h: Hit| s.hover == h;
        draw_button(
            hdc,
            theme,
            &layout.btn_open,
            s.strings.btn_open,
            hovered(Hit::Open),
            false,
        );
        let play_label = if s.paused {
            s.strings.btn_play
        } else {
            s.strings.btn_pause
        };
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
            s.strings.btn_stop,
            hovered(Hit::Stop),
            !s.loaded,
        );

        let name = if s.loaded { s.title } else { "" };
        if !name.is_empty() {
            // 居中而不是右对齐：右对齐会让文件名贴着「音量」那组，两团文字
            // 挤在窗口右端，左边又空一大片。
            text_center_ellipsis(hdc, theme.fonts.mono, C_DIM, name, &layout.file_name);
        }

        let mute_label = if s.muted || s.volume <= 0.0 {
            s.strings.mute_on
        } else {
            s.strings.mute_off
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
                s.strings.idle_hint,
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
    // `i32::clamp` 在 `min > max` 时 **panic**（"assertion failed: min <= max"）。
    // 今天的调用方传进来的 `client_w/h` 都过了 `.max(0)`，所以触发不了；
    // 但这是 `pub(crate)` 的纯函数，签名里也没写「非负是调用方的义务」。
    // 将来任何一处忘了 `.max(0)`（或者最小化瞬间拿到负值），在
    // `panic = "abort"` 下就是**整个进程直接消失、没有 MessageBox**——
    // 为了一个本可以是零面积矩形的情况。
    //
    // 负尺寸归一成 0（零面积），与「夹完为空」的既有约定一致。
    let w = client_w.max(0);
    let h = client_h.max(0);
    RECT {
        left: dirty.left.clamp(0, w),
        top: dirty.top.clamp(0, h),
        right: dirty.right.clamp(0, w),
        bottom: dirty.bottom.clamp(0, h),
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

/// 画面板的内容（底色与顶边由 `paint` 统一填）。
///
/// 行高来自 `Metrics::row_h`（等宽字体行高 + 一点留白），左列宽度来自
/// `diag_label_w` / `help_key_w`，两者都是**实测**的——这与控制栏里按钮宽度
/// 按实测文字宽度算是同一条原则：文案长度会变（两种语言），写死必然有一边
/// 被截或留一大截空隙。
///
/// 值列允许放不下就省略号截断，不换行、不缩放字号：诊断信息被切掉一半
/// 比字号小一号更难读，而省略号至少让用户知道右边还有东西。
unsafe fn draw_panel(
    hdc: HDC,
    theme: &mut Theme,
    layout: &Layout,
    panel: Panel<'_>,
    hover_row: Option<usize>,
    strings: &lang::Strings,
) {
    let dpi = theme.dpi();
    let pad_x = px(PANEL_PAD_X_DIP, dpi);
    let gap = panel_col_gap(dpi);
    let row_h = row_height(&theme.metrics, dpi);
    let top = layout.panel.top + px(PANEL_PAD_Y_DIP, dpi);
    let full_width = (layout.panel.right - pad_x).max(pad_x);

    // 轨道菜单是**整行**形态：左边是标记（当前在用的 / 默认的），
    // 右边铺满。不走两列 —— 菜单项的文字长度差别很大（「中文 · 2 声道 aac」
    // 与「关闭」），按两列对齐反而会在标记列上留一大片空白。
    if let Panel::Tracks { rows, cursor } = panel {
        let mut y = top;
        for (i, row) in rows.iter().enumerate() {
            if y >= layout.controls.top {
                break;
            }
            let line = RECT {
                left: pad_x,
                top: y,
                right: full_width,
                bottom: y + row_h,
            };
            match row {
                TrackRow::Heading(title) => {
                    // 分组标题：整行填成略深的底色，一眼分得开段落
                    fill(
                        hdc,
                        theme,
                        &RECT {
                            left: pad_x - px(4, dpi),
                            right: full_width,
                            ..line
                        },
                        C_PANEL_DEEP,
                    );
                    text_in(
                        hdc,
                        theme.fonts.mono,
                        C_ACCENT,
                        title,
                        &line,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
                    );
                }
                TrackRow::Note(text) => {
                    // 说明行：不可选，所以用 dim 色（选中色会让人以为能点）
                    text_in(
                        hdc,
                        theme.fonts.mono,
                        C_DIM,
                        text,
                        &line,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
                    );
                }
                TrackRow::OffSubtitle { active } => {
                    let mark = if *active { "●" } else { "" };
                    draw_track_row(
                        hdc,
                        theme,
                        &line,
                        i,
                        strings.track_off,
                        *active,
                        mark,
                        dpi,
                        hover_row,
                        cursor,
                    );
                }
                TrackRow::Item {
                    label,
                    active,
                    is_default,
                    ..
                } => {
                    // 标记放在最左：● = 正在用，▸ = 文件默认轨。两个可以同时成立
                    // （默认轨正在放），所以那种情况两个都画
                    let mark = match (*active, *is_default) {
                        (true, true) => "●▸",
                        (true, false) => "●",
                        (false, true) => "▸",
                        (false, false) => "",
                    };
                    draw_track_row(
                        hdc, theme, &line, i, label, *active, mark, dpi, hover_row, cursor,
                    );
                }
            }
            y += row_h;
        }
        return;
    }

    let (col1, rows): (i32, Vec<(&str, &str)>) = match panel {
        Panel::Stats(rows) => (
            theme.metrics.diag_label_w.max(px(48, dpi)),
            rows.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(),
        ),
        Panel::Help(rows) => (
            theme.metrics.help_key_w.max(px(64, dpi)),
            rows.iter().map(|(k, v)| (*k, *v)).collect(),
        ),
        // 上面那个 `if let` 命中就 `return` 了，所以这里走不到。写成兜底的
        // 空列表而不是 `unreachable!()`：本项目的 release profile 是
        // `panic = "abort"`，`unreachable!()` 等于整个进程直接消失、没有
        // MessageBox（`clamp_to_client` 的注释专门论证过这一点）。为一个
        // 逻辑上不可达的分支承担「用户在界面上什么都看不到」的风险不划算。
        Panel::Tracks { .. } => (px(48, dpi), Vec::new()),
    };

    let mut y = top;
    for (i, (key, value)) in rows.iter().enumerate() {
        if y >= layout.controls.top {
            break;
        }
        let line = RECT {
            left: pad_x,
            top: y,
            // 值列右边界就是客户区右边缘减去内边距
            right: full_width,
            bottom: y + row_h,
        };
        // 左列画标签色、右列画正文色：一眼能分清哪半边是键、哪半边是值
        text_in(
            hdc,
            theme.fonts.mono,
            C_DIM,
            key,
            &RECT {
                right: pad_x + col1,
                ..line
            },
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
        text_in(
            hdc,
            theme.fonts.mono,
            // 最后一行是结论，正常时用强调色标出来
            if panel.is_stats_last(i) {
                C_ACCENT
            } else {
                C_TEXT
            },
            value,
            &RECT {
                left: pad_x + col1 + gap,
                ..line
            },
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
        y += row_h;
    }
}

/// 画轨道菜单里的一行（标记 + 文字 + 高亮）。
///
/// 高亮有三级，视觉上必须能分开：光标所在行（最亮，用户按 ↑↓ 移动的就是它）、
/// 鼠标悬停行（次亮）、正在使用的那条（文字用强调色）。三者可以重合，
/// 所以底色与文字色是两套独立信号，不能只靠其中一个。
#[allow(clippy::too_many_arguments)]
unsafe fn draw_track_row(
    hdc: HDC,
    theme: &mut Theme,
    line: &RECT,
    index: usize,
    label: &str,
    active: bool,
    mark: &str,
    dpi: u32,
    hover_row: Option<usize>,
    cursor: usize,
) {
    // 光标与悬停用**不同深浅**的两级底色。用户按住 ↑↓ 连续移动时，
    // 光标在下面走；如果和悬停同色，「我现在选的是哪一行」就看不出来了。
    if index == cursor {
        fill(hdc, theme, line, C_ROW_HI);
    } else if hover_row == Some(index) {
        fill(hdc, theme, line, C_ROW_HOVER);
    }
    let mark_w = px(26, dpi);
    if !mark.is_empty() {
        text_in(
            hdc,
            theme.fonts.mono,
            C_ACCENT,
            mark,
            &RECT {
                right: line.left + mark_w,
                ..*line
            },
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    }
    text_in(
        hdc,
        theme.fonts.mono,
        if active { C_ACCENT } else { C_TEXT },
        label,
        &RECT {
            left: line.left + mark_w,
            ..*line
        },
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
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

/// 居中单行文字，放不下时尾部加省略号。
///
/// 文件名可能有几十个字符，而中间那一段宽度有限。`DT_END_ELLIPSIS` 与
/// `DT_CENTER` 可以同时用：`DrawTextW` 会先截断再把截断后的结果居中。
unsafe fn text_center_ellipsis(hdc: HDC, font: HFONT, color: COLORREF, text: &str, rect: &RECT) {
    text_in(
        hdc,
        font,
        color,
        text,
        rect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
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
    // 字体可能创建失败（`make_font` 不检查 `CreateFontW` 的返回值），
    // 这时 `SelectObject` 拿到 NULL 字体、返回的 `old_font` 也是 NULL。
    // 原写法无条件把 `old_font` 塞回去，于是在**常驻的后备位图 DC** 上执行
    // `SelectObject(hdc, NULL)`——DC 进入「没有选中绘图对象」状态，之后
    // `App::paint` 的 `BitBlt` 直接失败，而它的返回值被丢弃，于是控制栏
    // 整个会话空白、没有任何日志。
    //
    // 这里加一道判断：拿不到有效旧对象就不往下画（画不出文字总好过把 DC
    // 弄坏），并且只在拿到有效旧对象时才恢复。
    let old_font = SelectObject(hdc, font.into());
    if old_font.is_invalid() || font.is_invalid() {
        return;
    }
    let old_color = SetTextColor(hdc, color);
    // 背景模式必须还原：这个 DC 是常驻的，遗留 TRANSPARENT 会让下一次
    // 用背景色的绘制（比如控件边框）出现意外。
    // `SetBkMode` 的入参是 `BACKGROUND_MODE`、返回值是旧值的 `i32`（GDI 原型如此），
    // 所以还原时必须包一层转换。
    let old_bk = SetBkMode(hdc, TRANSPARENT);

    let mut buf: Vec<u16> = text.encode_utf16().collect();
    buf.push(0);
    let mut r = *rect;
    DrawTextW(hdc, &mut buf, &mut r, flags);

    SetBkMode(
        hdc,
        windows::Win32::Graphics::Gdi::BACKGROUND_MODE(old_bk as u32),
    );
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

    /// 按指定语言真实量一遍文案尺寸，交给 `Layout`。
    ///
    /// 不能用手编的固定宽度：这一版布局的按钮宽度**就是**由文字宽度算出来的，
    /// 拿假值去测等于把「英文标签放不下」这个真问题绕了过去。
    fn metrics_for(lang: lang::Lang) -> Metrics {
        let s = lang::Strings::new(lang);
        let fonts = Fonts::new(120);
        measure_metrics(&fonts, &s)
    }

    fn layout_for(client_w: i32, client_h: i32, theatre: bool) -> Layout {
        Layout::new(
            client_w,
            client_h,
            120,
            theatre,
            0,
            &metrics_for(lang::Lang::ZhCn),
        )
    }

    /// 带面板的布局。
    fn layout_with_panel(client_w: i32, client_h: i32, rows: usize) -> Layout {
        Layout::new(
            client_w,
            client_h,
            120,
            false,
            rows,
            &metrics_for(lang::Lang::ZhCn),
        )
    }

    // ---- 面板（0.4.0）----------------------------------------------------

    /// 面板行数为 0 时，布局与没有面板时**完全一样**。
    ///
    /// 这是「面板是可选的」这个约定的回归保护：0 行不能因为多乘了个 0
    /// 就让控制栏偏掉 1 像素（那种偏移在截图里看不出来，但会让整窗重画的
    /// 判定出现一条缝）。
    #[test]
    fn 零行面板等于没有面板() {
        let a = layout_for(1375, 900, false);
        let b = layout_with_panel(1375, 900, 0);
        assert_eq!(a.controls, b.controls);
        assert_eq!(a.panel, b.panel, "没有面板时 panel 应当是零高度");
        assert_eq!(a.chrome, b.chrome);
    }

    /// 面板吃掉的是**画面区**，控制栏自己一动不动。
    ///
    /// 这条约定有两个方向，缺一个都会出可见的问题：
    ///
    /// * 控制栏矩形不能包含面板（写成 `client_h - ctrl_h - panel_h` 就会
    ///   把面板那一段包进 `controls`，按钮跟着偏到面板那一行——实测控制栏
    ///   高度从 78 变成 247）。
    /// * 控制栏**也不该**上移。它永远贴着客户区底部，面板在它上面；
    ///   变矮的是画面区。所以 `controls` 两个版本必须逐字段相等。
    #[test]
    fn 面板只吃画面区_控制栏逐字段不变() {
        let plain = layout_for(1375, 900, false);
        let with = layout_with_panel(1375, 900, lang::DIAG_ROWS);

        assert_eq!(
            with.controls, plain.controls,
            "控制栏矩形必须与没有面板时完全一致"
        );
        // 画面区的下沿 = 面板的上沿。`App::relayout` 就是拿这个值去摆
        // mpv 的画面子窗口的，所以它必须真的比控制栏顶边高
        assert!(
            with.panel.top < with.controls.top,
            "面板应当占掉画面区的一部分：panel.top={} controls.top={}",
            with.panel.top,
            with.controls.top
        );
        assert_eq!(
            with.panel.top,
            with.controls.top - panel_height(lang::DIAG_ROWS, &metrics_for(lang::Lang::ZhCn), 120),
            "画面区下沿应当正好是「控制栏顶边减去面板高度」"
        );
    }

    /// 面板紧贴控制栏上方，中间不留缝。
    #[test]
    fn 面板紧贴控制栏() {
        let with = layout_with_panel(1375, 900, lang::DIAG_ROWS);
        assert_eq!(with.panel.bottom, with.controls.top, "面板与控制栏之间有缝");
        assert_eq!(with.panel.left, 0);
        assert_eq!(with.panel.right, 1375);
        assert!(with.panel.top < with.panel.bottom, "面板高度必须是正的");
    }

    /// chrome 是面板与控制栏的并集 —— 上层把脏区裁到这里，绘制填到这里。
    #[test]
    fn chrome_覆盖面板与控制栏() {
        let with = layout_with_panel(1375, 900, lang::DIAG_ROWS);
        assert_eq!(with.chrome.top, with.panel.top);
        assert_eq!(with.chrome.bottom, with.controls.bottom);
        // chrome 必须在 panel 与 controls 之外都够大：夹脏区时两块都要留住
        assert!(with.chrome.top <= with.panel.top);
        assert!(with.chrome.bottom >= with.controls.bottom);
    }

    /// 行数越多面板越高；两个面板的行数都是编译期常量且不相等。
    #[test]
    fn 面板高度随行数增长() {
        let stats = layout_with_panel(1375, 900, lang::DIAG_ROWS);
        let help = layout_with_panel(1375, 900, lang::HELP_ROWS);
        assert!(
            help.panel.bottom - help.panel.top > stats.panel.bottom - stats.panel.top,
            "快捷键 {} 行应当比诊断 {} 行更高",
            lang::HELP_ROWS,
            lang::DIAG_ROWS
        );
        // 高度正好等于行数 × 行高（面板上下内边距各留一点）
        let m = metrics_for(lang::Lang::ZhCn);
        let expected = panel_height(lang::HELP_ROWS, &m, 120);
        assert_eq!(help.panel.bottom - help.panel.top, expected);
    }

    /// 客户区不够高时，面板不能把控制栏顶到屏幕外。
    ///
    /// 客户区比「控制栏 + 面板」还矮时（窗口拖得很小），面板高度必须被夹住，
    /// 否则 `controls.top` 会变成负数、按钮画到客户区外面。
    #[test]
    fn 客户区很矮时面板被夹住() {
        let m = metrics_for(lang::Lang::ZhCn);
        // 比「控制栏 + 满面板」还矮一点
        let tight_h = px(CTRL_H_DIP, 120) + px(20, 120);
        let l = Layout::new(480, tight_h, 120, false, lang::HELP_ROWS, &m);
        assert!(l.controls.top >= 0, "controls.top 不能为负");
        assert_eq!(l.panel.bottom, l.controls.top, "面板下沿就是控制栏顶边");
        assert!(
            l.panel.bottom - l.panel.top <= tight_h,
            "面板高度 {0} 超过客户区 {1}",
            l.panel.bottom - l.panel.top,
            tight_h
        );
        // 画面区下沿不能跑到客户区外面去
        assert!(l.panel.top >= 0);
        assert!(l.panel.top <= l.controls.top);
    }

    /// 影院模式下不显示面板。
    #[test]
    fn 影院模式下没有面板() {
        let l = Layout::new(
            1375,
            900,
            120,
            true,
            lang::HELP_ROWS,
            &metrics_for(lang::Lang::ZhCn),
        );
        assert_eq!(l.panel.top, l.panel.bottom, "影院模式下面板应当是零高度");
        assert_eq!(l.controls.top, 900, "影院模式控制栏收起，画面占满");
    }

    /// 面板区域的命中。
    ///
    /// 契约是「`Layout` 只报几何、不管语义」：面板里有行的时候命中给出行号，
    /// **上层**（`app.rs`）看当前面板是不是菜单来决定这一行能不能点。所以这里
    /// 断言的是「行号算得对」，而不是「面板不可点」。
    ///
    /// 之前这个测试断言 `Hit::Video`，那是单一面板形态下的行为；加了轨道
    /// 菜单之后它不再成立 —— 硬留着会让菜单的点不中，或者逼着 `Layout`
    /// 背上「当前是什么面板」的知识。
    #[test]
    fn 面板区域的命中给出行号() {
        let l = layout_with_panel(1375, 900, lang::DIAG_ROWS);
        let cx = (l.panel.left + l.panel.right) / 2;

        // 第一行的中点 → 第 0 行
        let y0 = l.panel_row_top + l.panel_row_h / 2;
        assert_eq!(l.hit(cx, y0), Hit::TrackRow(0));
        // 最后一行（面板高度被行数整除时正好贴住底边）
        let last = l.panel_drawn_rows as i32 - 1;
        let yn = l.panel_row_top + last * l.panel_row_h + l.panel_row_h / 2;
        assert_eq!(l.hit(cx, yn), Hit::TrackRow(last as usize));

        // 行与行之间不该有「哪行都不属于」的缝：整数除法向下取整，
        // 点在行顶边沿上应该算本行、点在本行最后一像素仍算本行
        for row in 0..l.panel_drawn_rows as i32 {
            let top = l.panel_row_top + row * l.panel_row_h;
            assert_eq!(
                l.panel_row_at(cx, top),
                Some(row as usize),
                "第 {row} 行顶边"
            );
            assert_eq!(
                l.panel_row_at(cx, top + l.panel_row_h - 1),
                Some(row as usize),
                "第 {row} 行底边"
            );
        }

        // 控制栏里的按钮仍然是按钮
        let bx = (l.btn_play.left + l.btn_play.right) / 2;
        let by = (l.btn_play.top + l.btn_play.bottom) / 2;
        assert_eq!(l.hit(bx, by), Hit::Play);
    }

    /// 面板上方那一像素不属于任何行（那是画面）。
    #[test]
    fn 面板上边缘之外不是行() {
        let l = layout_with_panel(1375, 900, lang::DIAG_ROWS);
        let cx = (l.panel.left + l.panel.right) / 2;
        assert_eq!(l.panel_row_at(cx, l.panel.top - 1), None);
        // 面板底边之下是控制栏，也不是行
        assert_eq!(l.panel_row_at(cx, l.panel.bottom), None);
    }

    /// 影院模式 / 没有面板时，任何坐标都不该命中行。
    #[test]
    fn 没有菜单就没有行可命中() {
        let m = metrics_for(lang::Lang::ZhCn);
        let none = Layout::new(1375, 900, 120, false, 0, &m);
        assert_eq!(none.panel_drawn_rows, 0);
        let cx = (none.panel.left + none.panel.right) / 2;
        assert_eq!(none.panel_row_at(cx, 100), None);

        let theatre = Layout::new(1375, 900, 120, true, lang::HELP_ROWS, &m);
        assert_eq!(theatre.panel_drawn_rows, 0, "影院模式没有面板");
        assert_eq!(theatre.panel_row_at(cx, 100), None);
    }

    /// 窗口太矮时，画不出来的行**不该**被命中。
    ///
    /// 面板高度被 `min(controls_top)` 夹住时底部几行画不出来（`draw_panel`
    /// 里 `y >= controls.top` 就 `break`）。如果这时还能点到那些行，用户会
    /// 选中一条界面上完全看不见的轨 —— 菜单看起来「点了没反应」。
    #[test]
    fn 被夹掉的面板行不可命中() {
        // 客户区只够放得下 2 行整（第 2 行正好贴住控制栏顶边）
        let m = metrics_for(lang::Lang::ZhCn);
        let row_h = row_height(&m, 120);
        let ctrl_h = px(CTRL_H_DIP, 120);
        let pad_y = px(PANEL_PAD_Y_DIP, 120);
        let tight = ctrl_h + 2 * row_h + pad_y;
        let l = Layout::new(1375, tight, 120, false, lang::HELP_ROWS, &m);
        assert_eq!(
            l.panel_drawn_rows, 2,
            "只够画 2 行（实得 {}）",
            l.panel_drawn_rows
        );
        let cx = (l.panel.left + l.panel.right) / 2;

        // 画出来的那两行都要能点：第 2 行的底边就是 `panel.bottom`，
        // 面板最下面那一像素仍然属于第 2 行（点它是合法的）
        for row in 0..l.panel_drawn_rows as i32 {
            let top = l.panel_row_top + row * row_h;
            assert_eq!(l.panel_row_at(cx, top), Some(row as usize));
            assert_eq!(
                l.panel_row_at(cx, top + row_h - 1),
                Some(row as usize),
                "第 {row} 行最后一个像素"
            );
        }

        // 第 3 行开始就是控制栏，不是面板
        assert_eq!(l.panel_row_at(cx, l.panel.bottom), None);
        assert_eq!(l.panel_row_at(cx, l.panel.bottom + row_h), None);
    }

    /// 面板没被夹住时，`panel_drawn_rows` 必须**正好**等于行数。
    ///
    /// 这条盯的是 `drawn_rows` 那个 `min(panel_rows)`：少了它，底部内边距
    /// 会被算成多出来的一行，命中测试就会报出一个不存在的行号。
    #[test]
    fn 没被夹住时行数正好等于真实行数() {
        // 900px 客户区在 120 DPI 下大概放得下 36 行，取几组肯定放得下的。
        // 放不下的那种由 `被夹掉的面板行不可命中` 覆盖。
        for rows in [1usize, 2, lang::DIAG_ROWS, lang::HELP_ROWS, 30] {
            let l = layout_with_panel(1375, 900, rows);
            assert_eq!(
                l.panel_drawn_rows, rows,
                "{rows} 行时 drawn_rows 应该是 {rows}（实得 {}）",
                l.panel_drawn_rows
            );
            // 最后一行也要点得到
            let cx = (l.panel.left + l.panel.right) / 2;
            let last = rows as i32 - 1;
            let y = l.panel_row_top + last * l.panel_row_h + l.panel_row_h - 1;
            assert_eq!(l.panel_row_at(cx, y), Some(last as usize));
        }
    }

    /// 两种语言的标签列宽度都必须量出来，且为正。
    ///
    /// 面板的两列是对齐的，标签列宽度取自实测——如果某一门语言量出 0
    /// （度量失败），值列就会贴着左边缘。
    #[test]
    fn 两种语言的标签列都量得出来() {
        for l in [lang::Lang::ZhCn, lang::Lang::En] {
            let m = metrics_for(l);
            assert!(m.diag_label_w > 0, "{l:?} 的诊断标签列宽度是 0");
            assert!(m.help_key_w > 0, "{l:?} 的快捷键按键列宽度是 0");
            assert!(m.row_h > 0, "{l:?} 的行高是 0");
        }
    }

    /// 诊断标签的数量必须与 `DIAG_ROWS` 一致。
    ///
    /// 面板高度是按 `DIAG_ROWS`（编译期常量）算的，而标签是
    /// `Strings::diag_labels()` 给的。两者对不上就会「布局按 7 行算、
    /// 绘制画 6 行」或反过来——最后一行画到控制栏里，或者多出一段空白。
    #[test]
    fn 诊断标签数与行数常量一致() {
        for l in [lang::Lang::ZhCn, lang::Lang::En] {
            let s = lang::Strings::new(l);
            assert_eq!(
                s.diag_labels().len(),
                lang::DIAG_ROWS,
                "{l:?} 的标签数与 DIAG_ROWS 不一致"
            );
            assert_eq!(
                s.help_rows().len(),
                lang::HELP_ROWS,
                "{l:?} 的快捷键行数与 HELP_ROWS 不一致"
            );
        }
    }

    /// 单量一段文字的宽度，测试里断言「按钮装得下文字」时用。
    fn measure_w_hdc(text: &str, font: HFONT) -> i32 {
        // 借一次屏幕 DC 量完就还。跟 `measure_metrics` 同一个语义：字体按物理
        // 像素高度创建，换 DC 量出来的宽度一致。
        let hdc = unsafe { GetDC(None) };
        assert!(!hdc.is_invalid(), "借不到屏幕 DC");
        let w = measure_w(hdc, font, text);
        unsafe {
            let _ = ReleaseDC(None, hdc);
        }
        assert!(w > 0, "量不到 \"{text}\" 的宽度");
        w
    }

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
        let l = layout_for(1375, 900, true);
        assert_eq!(l.controls.top, l.controls.bottom);
        assert!(contains(&l.seek, l.seek.left + 1, l.seek.top + 1));
        // 影院模式下点控制栏旧位置应该算点到了视频区
        assert_eq!(l.hit(l.btn_play.left + 1, l.btn_play.top + 1), Hit::Video);
    }

    #[test]
    fn 正常模式命中与比例() {
        let l = layout_for(1375, 900, false);
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
        let l = layout_for(1375, 900, false);
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
        let l = Layout::new(480, 320, 96, false, 0, &metrics_for(lang::Lang::ZhCn));
        assert!(l.volume.right <= 480, "volume.right={}", l.volume.right);
        assert!(l.btn_stop.right < 480);
        // 文件名区域宽度可能为 0，但不应为负（rect 已经夹过）
        assert!(l.file_name.right >= l.file_name.left);
    }

    /// 按钮宽度必须容得下它要显示的文字，两种语言都要过。
    ///
    /// 这是「英文版需要重构每一个按钮的区块大小」那条需求的**回归防线**。
    /// 原来的宽度是写死的 48 DIP：中文「打开」正好，英文 "Pause"（实测约 40px
    /// 再加左右各 12 DIP 内边距 ≈ 64 DIP）就会被 `DT_END_ELLIPSIS` 截成 "Pau…"，
    /// 而这种截断在截图里非常不显眼，很容易漏过去。
    ///
    /// 断言用「实测文字宽 + 内边距 ≤ 按钮宽」，且两种语言、多个 DPI 都要过。
    #[test]
    fn 按钮装得下各自的文字() {
        for lang in [lang::Lang::ZhCn, lang::Lang::En] {
            let s = lang::Strings::new(lang);
            // 覆盖 100% / 125% / 150% / 200%
            for dpi in [96u32, 120, 144, 192] {
                let fonts = Fonts::new(dpi);
                let m = measure_metrics(&fonts, &s);
                let w_px = |dip: i32| px(dip, dpi);
                let l = Layout::new(
                    w_px(1375 * 96 / 120),
                    w_px(900 * 96 / 120),
                    dpi,
                    false,
                    0,
                    &m,
                );
                let pad = w_px(BTN_PAD_X_DIP);

                let cases: [(&str, &str, &RECT); 4] = [
                    (s.btn_open, "open", &l.btn_open),
                    // 播放 / 暂停按钮取两者较宽的那个，所以两个标签都要装得下
                    (s.btn_play, "play", &l.btn_play),
                    (s.btn_pause, "pause", &l.btn_play),
                    (s.btn_stop, "stop", &l.btn_stop),
                ];
                for (text, which, rect) in cases {
                    let w = measure_w_hdc(text, fonts.ui);
                    assert!(
                        rect.right - rect.left >= w + 2 * pad,
                        "{lang:?} @{dpi}dpi 的 {which} 按钮装不下 \"{text}\": \
                         文字 {w}px + 左右内边距 {} > 按钮 {}px",
                        2 * pad,
                        rect.right - rect.left
                    );
                }

                // 静音标签同理
                let mute_text = if s.mute_on.len() > s.mute_off.len() {
                    s.mute_on
                } else {
                    s.mute_off
                };
                let mw = measure_w_hdc(mute_text, fonts.ui);
                assert!(
                    l.mute.right - l.mute.left >= mw + 2 * pad,
                    "{lang:?} @{dpi}dpi 的 mute 区域装不下文字：{} > {}",
                    mw + 2 * pad,
                    l.mute.right - l.mute.left
                );
            }
        }
    }

    /// 切换播放状态时按钮宽度不能变。
    ///
    /// 「播放 / 暂停」和「音量 / 静音」在两种语言里长度都不同。如果按钮宽度跟着
    /// 当前文案走，那么每按一次空格，整排按钮就会左右跳一下——用户会以为
    /// 窗口抖了。`Metrics::play_pause_w` / `mute_w` 取两者较宽者就是为了这个。
    #[test]
    fn 播放状态切换时按钮不呼吸() {
        for lang in [lang::Lang::ZhCn, lang::Lang::En] {
            let s = lang::Strings::new(lang);
            let m = metrics_for(lang);
            assert!(
                m.play_pause_w() >= m.play && m.play_pause_w() >= m.pause,
                "{lang:?} 的 play_pause_w 小于其中之一"
            );
            assert!(
                m.mute_w() >= m.mute_on && m.mute_w() >= m.mute_off,
                "{lang:?} 的 mute_w 小于其中之一"
            );
            // 布局用的是 max 值，所以两种状态下的矩形完全一致
            let a = Layout::new(1375, 900, 120, false, 0, &m);
            let b = Layout::new(1375, 900, 120, false, 0, &m);
            assert_eq!(
                (a.btn_play.left, a.btn_play.right),
                (b.btn_play.left, b.btn_play.right)
            );
            assert!(s.btn_play != s.btn_pause || m.play == m.pause);
        }
    }

    /// 三组控件之间必须留出组间距，控件不得互相压。
    ///
    /// 回归点是「都偏左、中间一大片空白」：原来文件名的矩形是
    /// `btn_stop.right + gap` 到 `mute.left - gap`，中间那块空地没有任何东西，
    /// 视觉重心全压在左侧。改版之后文件名是中间那一整段（有内容居中画），
    /// 三组均匀分布。
    #[test]
    fn 三组控件分布均匀且不重叠() {
        for lang in [lang::Lang::ZhCn, lang::Lang::En] {
            let l = Layout::new(1375, 900, 120, false, 0, &metrics_for(lang));
            let gap = px(CLUSTER_GAP_DIP, 120);
            let small = px(GAP_DIP, 120);

            // 组内：按钮之间是组内间距
            assert_eq!(l.btn_play.left - l.btn_open.right, small);
            assert_eq!(l.btn_stop.left - l.btn_play.right, small);
            // 组间：按钮组 -> 文件名、音量组 -> 文件名，各留一个组间距
            assert_eq!(l.file_name.left - l.btn_stop.right, gap);
            assert_eq!(l.mute.left - l.file_name.right, gap);
            // 组内：静音标签与音量滑块
            assert_eq!(l.volume.left - l.mute.right, small);

            // 左右边距对称
            assert_eq!(l.btn_open.left, 1375 - l.volume.right, "左右边距不对称");

            // 文件名区域要有实际宽度，才谈得上「居中」
            assert!(
                l.file_name.right - l.file_name.left > gap * 2,
                "{lang:?} 下文件名区域太窄（{}px），居中没有意义",
                l.file_name.right - l.file_name.left
            );
        }
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
