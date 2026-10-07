//! VideoView：基于 libmpv 的 Windows 桌面视频播放器。
//!
//! 组成：
//!
//! * [`mpv`] —— libmpv 的最小 FFI 封装（动态加载，无构建期 C 依赖）
//! * [`diag`] —— 解码健康度：把 mpv 的观测项收成能下判断的数据
//! * [`playlist`] —— 播放列表：拖一批文件进来、同名字幕自动挂载
//! * [`surface`] —— mpv 画面子窗口（纯 Win32）
//! * [`ui`] —— 控制栏的布局 / 绘制 / 命中测试
//! * `app` —— 主窗口与消息循环
//!
//! 整套界面是原生自绘的，没有 WebView、没有前端构建链：启动快、内存占用低，
//! 且 DPI 缩放由系统与 GDI 原生处理，不存在浏览器位图拉伸导致的文字发虚。

mod app;
pub mod cpu;
pub mod crashlog;
pub mod diag;
pub mod gpu;
pub mod lang;
pub mod menu;
pub mod mpv;
pub mod playlist;
pub mod settings;
mod surface;
pub mod track;
mod ui;

pub use app::run;
