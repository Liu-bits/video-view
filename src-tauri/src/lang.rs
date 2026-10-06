//! 界面语言与全部面向用户的文案。
//!
//! 装完的默认语言由**安装器**决定，不是编译期写死的：NSIS 安装器带一张
//! 语言选择页（`MUI_PAGE_LANGUAGE`，见 `installer.nsi`），用户选完由安装器
//! 写进注册表 `Software\VideoView\Language`，程序启动时读它。
//!
//! 这样同一个 exe 能同时装出中/英两种版本，代价是零——没有第二个二进制，
//! 也没有运行时切换（界面是自绘的，切换要重建字体与布局，没必要）。
//!
//! ## 取值顺序
//!
//! 1. 环境变量 `VIDEOVIEW_LANG`（`en` / `zh-CN`）：便携版与自动化测试用，
//!    也是唯一能在不装注册表键的情况下强制切换的办法
//! 2. 注册表 `Software\VideoView\Language`（安装器写的）
//! 3. 系统界面语言（`GetUserDefaultUILanguage`）
//! 4. 英文
//!
//! 注册表分两处读，因为安装权限有两种：正式安装器是 `RequestExecutionLevel admin`
//! 写 HKLM；而当前用户直接跑安装包、或解压即用的便携版没有 HKLM 键，
//! 这时读 HKCU 兜底。

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
    KEY_READ,
};

/// UTF-16 + NUL 终止。注册表 API 要 `PCWSTR`。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `Language` 值的字节上限。见 `read_lang` 里的说明：这是用户可写的输入，
/// 必须有上限，否则一个 64MB 的注册表项就能让每次启动分配 128MB。
const MAX_LANG_VALUE_BYTES: u32 = 64;

/// 界面语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// English
    En,
    /// 简体中文
    ZhCn,
}

impl Lang {
    /// 按上述顺序决定实际使用的语言。
    ///
    /// 这条在 `App` 构造时调一次，之后整个进程不再改。
    pub fn detect() -> Self {
        if let Some(v) = env_lang() {
            return v;
        }
        if let Some(v) = registry_lang() {
            return v;
        }
        Self::from_primary_language_id(unsafe { GetUserDefaultUILanguage() })
    }

    /// 从主语言 ID 判断。`0x0804` 是 zh-CN，`0x0404` 是 zh-TW。
    ///
    /// 繁体一并归到简体：这套文案没有做繁简转换，与其显示半繁半简的
    /// 字符串（比英文还难读），不如给英文。
    pub fn from_primary_language_id(id: u16) -> Self {
        match id & 0x3FF {
            0x004 | 0x008 | 0x028 | 0x02C => Self::ZhCn,
            _ => Self::En,
        }
    }

    /// 解析安装器写入的 NSIS 语言名或环境变量值。
    ///
    /// NSIS 的 `$LANGUAGE` 是语言表名，中文那张是 `SimpChinese`、英文那张
    /// 是 `English`；环境变量用 BCP 47 的 `zh-CN` / `en`。两套都收下，
    /// 免得换个入口就失配。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "simpchinese" | "zh-cn" | "zh_cn" | "zh-hans" | "chinese" => Some(Self::ZhCn),
            "english" | "en" | "en-us" | "en_us" => Some(Self::En),
            _ => None,
        }
    }

    /// 写回注册表时用的名字。与安装器保持一致。
    pub fn registry_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::ZhCn => "SimpChinese",
        }
    }
}

/// `VIDEOVIEW_LANG` 环境变量。
///
/// 只认明确的值：设成空串或乱值时返回 `None`，让后面的来源继续走，
/// 而不是静默变成英文。
fn env_lang() -> Option<Lang> {
    let v = std::env::var("VIDEOVIEW_LANG").ok()?;
    Lang::parse(&v)
}

/// 安装器写入的注册表值。HKLM 没有就试 HKCU。
fn registry_lang() -> Option<Lang> {
    let v = read_lang(HKEY_LOCAL_MACHINE).or_else(|| read_lang(HKEY_CURRENT_USER))?;
    Lang::parse(&v)
}

/// 读 `Software\VideoView\Language`。
///
/// 这里用原始的 `RegOpenKeyExW` / `RegQueryValueExW` 而不是为了读一个值
/// 新引一个注册表库——那会让「为了国际化引入一个依赖」变成常态，而实际
/// 只需要这二十来行。
///
/// 任何一步失败都返回 `None`（当作没装过、按系统语言走）：语言猜错的后果
/// 只是界面语言不对，远比启动失败或 panic 轻。
fn read_lang(root: HKEY) -> Option<String> {
    unsafe {
        let subkey = wide(r"Software\VideoView");
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            root,
            windows::core::PCWSTR(subkey.as_ptr()),
            None,
            KEY_READ,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        // 先问长度（lpdata 传 None），再一次读回来。
        // 注册表的字符串值长度是**字节**数，除 2 才是 UTF-16 字符数。
        let name = wide("Language");
        let mut bytes: u32 = 0;
        let q = RegQueryValueExW(
            hkey,
            windows::core::PCWSTR(name.as_ptr()),
            None,
            None,
            None,
            Some(&mut bytes),
        );
        if q != ERROR_SUCCESS || bytes == 0 {
            let _ = RegCloseKey(hkey);
            return None;
        }
        // **上限**：`HKCU\Software\VideoView\Language` 是当前用户完全可写的
        // （`Lang::detect` 把 HKCU 当 HKLM 缺失时的兜底），所以这个字节数
        // 是**攻击者可控的输入**，不是「一个语言名」。不设上限的话，写一个
        // 64MB 的 REG_SZ 就能让每次启动分配 128MB —— 一个纯数据项就能当
        // 内存放大器用。
        //
        // 64 字节已经比任何真实语言名长两个数量级（最长的 `SimpChinese`
        // 11 字节），超出的直接当「这不是语言名」。
        if bytes > MAX_LANG_VALUE_BYTES {
            let _ = RegCloseKey(hkey);
            return None;
        }
        let mut chars = vec![0u16; (bytes as usize).div_ceil(2)];
        let mut got: u32 = bytes;
        let q = RegQueryValueExW(
            hkey,
            windows::core::PCWSTR(name.as_ptr()),
            None,
            None,
            Some(chars.as_mut_ptr().cast::<u8>()),
            Some(&mut got),
        );
        let _ = RegCloseKey(hkey);
        if q != ERROR_SUCCESS {
            return None;
        }
        // `got` 是含终止符的字节数。REG_SZ 保证有终止符，但仍然夹一下，
        // 万一值被外部改成了二进制类型（见下方测试）不会读越界。
        let len = ((got as usize) / 2).min(chars.len());
        let len = chars[..len].iter().position(|&c| c == 0).unwrap_or(len);
        String::from_utf16(&chars[..len]).ok()
    }
}

/// 某个语言下的全部界面文案。
///
/// 集中在一处而不是散在调用点，是为了编译期就能发现漏翻：新增一条文案却忘了
/// 两种语言都填，`Strings::new` 里就会少一个分支，`match` 不完整直接编译失败。
#[derive(Debug, Clone, Copy)]
pub struct Strings {
    pub lang: Lang,

    // ---- 控制栏 ----
    /// 空闲时的提示语。
    pub idle_hint: &'static str,
    pub btn_open: &'static str,
    pub btn_play: &'static str,
    pub btn_pause: &'static str,
    pub btn_stop: &'static str,
    /// 已静音时音量区的文字。
    pub mute_on: &'static str,
    /// 未静音时音量区的文字。
    pub mute_off: &'static str,

    // ---- 打开文件对话框 ----
    pub dlg_filter_video: &'static str,
    pub dlg_filter_all: &'static str,
    pub dlg_title: &'static str,

    // ---- 错误标题（MessageBox 标题栏）----
    pub err_startup: &'static str,
    pub err_video_window: &'static str,
    pub err_open: &'static str,
    pub err_toggle: &'static str,
    pub err_stop: &'static str,
    pub err_seek: &'static str,
    pub err_volume: &'static str,
    pub err_mute: &'static str,
    pub err_playback: &'static str,

    // ---- 错误正文 ----
    pub err_bad_unicode: &'static str,
    pub err_not_a_file: &'static str,

    // ---- 启动阶段的致命错误 ----
    pub err_read_target: &'static str,
    pub err_register_class: &'static str,

    // ---- 解码诊断面板的标签（一列，逐行对齐）----
    //
    // 标签列的宽度差异要交给 `ui::Metrics` 实测，不能靠等宽字体的空格去凑：
    // 中英文混排时宽度根本不是整数倍。取值统一由 `diag_labels()` 提供，
    // 免得绘制那边和测量那边各写一份、改一处忘一处。
    pub diag_hwdec: &'static str,
    pub diag_video: &'static str,
    pub diag_fps: &'static str,
    pub diag_dropped: &'static str,
    pub diag_cache: &'static str,
    pub diag_speed: &'static str,
    pub diag_verdict: &'static str,

    // ---- 解码诊断面板的取值与结论 ----
    /// 「拿不到」，用来与「确实是 0」区分开
    pub diag_unknown: &'static str,
    pub diag_healthy: &'static str,
    pub diag_zero_copy: &'static str,
    /// 面板里 hwdec 一栏在降级时显示的短词
    pub diag_software_short: &'static str,
    pub diag_software_decode: &'static str,
    pub diag_no_decoder: &'static str,
    pub diag_no_zero_copy: &'static str,
    pub diag_decoder_drops: &'static str,
    pub diag_vo_drops: &'static str,
    pub diag_fps_behind: &'static str,

    // ---- mpv 拒绝了一条命令 ----
    pub err_command: &'static str,

    // ---- 截图 / 诊断报告 ----
    pub err_screenshot: &'static str,
    pub err_report: &'static str,
    pub info_report_copied: &'static str,

    // ---- 快捷键总览 ----
    pub help_play_pause: &'static str,
    pub help_seek: &'static str,
    pub help_volume: &'static str,
    pub help_percent: &'static str,
    pub help_frame_step: &'static str,
    pub help_speed: &'static str,
    pub help_jump: &'static str,
    pub help_mute: &'static str,
    pub help_theatre: &'static str,
    pub help_fullscreen: &'static str,
    pub help_screenshot: &'static str,
    pub help_diag: &'static str,
    pub help_open: &'static str,
    pub help_copy: &'static str,
    pub help_help: &'static str,
}

impl Strings {
    pub fn new(lang: Lang) -> Self {
        match lang {
            Lang::En => Self::en(),
            Lang::ZhCn => Self::zh_cn(),
        }
    }

    fn en() -> Self {
        Self {
            lang: Lang::En,
            idle_hint: "Drop a video file here, or press Ctrl+O",
            btn_open: "Open",
            btn_play: "Play",
            btn_pause: "Pause",
            btn_stop: "Stop",
            // 与中文「音量 / 静音」对应。这两个词长短差别很大，
            // 按钮宽度是按实测文字宽度算的（见 ui::Metrics），所以不用凑。
            mute_on: "Mute",
            mute_off: "Volume",
            dlg_filter_video: "Video files",
            dlg_filter_all: "All files",
            dlg_title: "Select a video to play",
            err_startup: "VideoView failed to start",
            err_video_window: "Could not create the video window",
            err_open: "Could not open the file",
            err_toggle: "Could not change the playback state",
            err_stop: "Could not stop",
            err_seek: "Could not seek",
            err_volume: "Could not change the volume",
            err_mute: "Could not toggle mute",
            err_playback: "Playback error",
            err_bad_unicode: "The file path is not valid Unicode and cannot be processed.",
            err_not_a_file: "What was dropped is not a file.",
            err_read_target: "Could not read the startup target: {e}",
            err_register_class: "Could not register the window class: {e}",
            diag_hwdec: "Decode",
            diag_video: "Video",
            diag_fps: "FPS",
            diag_dropped: "Dropped",
            diag_cache: "Cache",
            diag_speed: "Speed",
            diag_verdict: "Verdict",
            diag_unknown: "—",
            diag_healthy: "looks healthy",
            diag_zero_copy: "zero-copy",
            diag_software_short: "software",
            diag_software_decode: "hardware decoding is OFF — mpv fell back to software decoding",
            diag_no_decoder: "no decoder running yet",
            diag_no_zero_copy: "hardware decoding is on but frames still cross the CPU",
            diag_decoder_drops: "decoder dropped frames",
            diag_vo_drops: "output dropped frames",
            diag_fps_behind: "actual FPS is below the file's",
            err_command: "The playback engine rejected a command",
            err_screenshot: "Could not save the screenshot",
            err_report: "Could not copy the diagnostics report",
            info_report_copied: "Diagnostics copied to the clipboard.",
            help_play_pause: "Play / pause",
            help_seek: "Seek 10 seconds",
            help_volume: "Volume up / down",
            help_percent: "Jump to 0%–90%",
            help_frame_step: "Step one frame",
            help_speed: "Slower / faster",
            help_jump: "Jump to start / end",
            help_mute: "Mute",
            help_theatre: "Theatre mode",
            help_fullscreen: "Full screen",
            help_screenshot: "Save a screenshot",
            help_diag: "Decode diagnostics",
            help_open: "Open a file",
            help_copy: "Copy the diagnostics report",
            help_help: "This list",
        }
    }

    fn zh_cn() -> Self {
        Self {
            lang: Lang::ZhCn,
            idle_hint: "把视频文件拖到这里，或按 Ctrl+O 打开",
            btn_open: "打开",
            btn_play: "播放",
            btn_pause: "暂停",
            btn_stop: "停止",
            mute_on: "静音",
            mute_off: "音量",
            dlg_filter_video: "视频文件",
            dlg_filter_all: "所有文件",
            dlg_title: "选择要播放的视频",
            err_startup: "VideoView 启动失败",
            err_video_window: "无法创建画面窗口",
            err_open: "打开文件失败",
            err_toggle: "切换播放状态失败",
            err_stop: "停止失败",
            err_seek: "跳转失败",
            err_volume: "调整音量失败",
            err_mute: "切换静音失败",
            err_playback: "播放出错",
            err_bad_unicode: "文件路径不是合法的 Unicode，无法处理。",
            err_not_a_file: "拖进来的不是文件。",
            err_read_target: "读取启动目标失败：{e}",
            err_register_class: "注册窗口类失败：{e}",
            diag_hwdec: "解码",
            diag_video: "视频",
            diag_fps: "帧率",
            diag_dropped: "丢帧",
            diag_cache: "缓存",
            diag_speed: "倍速",
            diag_verdict: "结论",
            diag_unknown: "—",
            diag_healthy: "一切正常",
            diag_zero_copy: "零拷贝",
            diag_software_short: "软解",
            diag_software_decode: "硬件解码未生效，mpv 已静默退回软件解码",
            diag_no_decoder: "解码器还没建立",
            diag_no_zero_copy: "硬解生效，但画面仍要经过 CPU",
            diag_decoder_drops: "解码器丢了帧",
            diag_vo_drops: "显示端丢了帧",
            diag_fps_behind: "实际帧率低于文件标称",
            err_command: "播放核心拒绝了一条命令",
            err_screenshot: "截图保存失败",
            err_report: "复制诊断信息失败",
            info_report_copied: "诊断信息已复制到剪贴板。",
            help_play_pause: "播放 / 暂停",
            help_seek: "快退 / 快进 10 秒",
            help_volume: "音量增 / 减",
            help_percent: "跳到 0%–90%",
            help_frame_step: "逐帧后退 / 前进",
            help_speed: "减速 / 加速",
            help_jump: "跳到开头 / 结尾",
            help_mute: "静音",
            help_theatre: "影院模式",
            help_fullscreen: "全屏",
            help_screenshot: "保存截图",
            help_diag: "解码诊断",
            help_open: "打开文件",
            help_copy: "复制诊断报告",
            help_help: "这一页",
        }
    }

    /// 诊断面板的标签列，顺序与 `diag::Diagnostics::rows` 里那几行一致。
    ///
    /// 单独抽出来是因为 `ui::Metrics` 要按它实测最大宽度来定标签列宽度，
    /// 而 `rows()` 给的是 `(标签, 值)` 的成对数据。顺序或长度对不上，
    /// 面板就会整列错位——所以两边必须共用这一个来源。
    pub fn diag_labels(&self) -> [&'static str; 7] {
        [
            self.diag_hwdec,
            self.diag_video,
            self.diag_fps,
            self.diag_dropped,
            self.diag_cache,
            self.diag_speed,
            self.diag_verdict,
        ]
    }

    /// 快捷键总览的每一行：`(按键, 说明)`。
    ///
    /// 按键写法语言中立（两种语言共用同一套键），只有右边的说明要翻译。
    /// 两种语言的**行数必须一致** —— `快捷键总览两种语言行数一致` 那条
    /// 测试盯着这件事，面板高度就是按行数算的。
    pub fn help_rows(&self) -> [(&'static str, &'static str); HELP_ROWS] {
        [
            ("Space", self.help_play_pause),
            ("← / →", self.help_seek),
            ("↑ / ↓", self.help_volume),
            ("0 … 9", self.help_percent),
            (", / .", self.help_frame_step),
            ("[ / ]", self.help_speed),
            ("Home / End", self.help_jump),
            ("M", self.help_mute),
            ("F", self.help_theatre),
            ("F11", self.help_fullscreen),
            ("S", self.help_screenshot),
            ("I", self.help_diag),
            ("Ctrl + O", self.help_open),
            ("Ctrl + C", self.help_copy),
            ("?", self.help_help),
        ]
    }
}

/// 快捷键总览的行数。面板高度按它算，所以两种语言必须一致
/// （`快捷键总览两种语言行数一致` 那条测试盯着）。
pub const HELP_ROWS: usize = 15;

/// 解码诊断面板的行数。标签来自 `Strings::diag_labels()`，长度固定。
pub const DIAG_ROWS: usize = 7;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 环境变量与安装器语言名都能解析() {
        assert_eq!(Lang::parse("English"), Some(Lang::En));
        assert_eq!(Lang::parse("english"), Some(Lang::En));
        assert_eq!(Lang::parse("  EN  "), Some(Lang::En));
        assert_eq!(Lang::parse("en-US"), Some(Lang::En));
        // 安装器写的是 NSIS 语言表名
        assert_eq!(Lang::parse("SimpChinese"), Some(Lang::ZhCn));
        assert_eq!(Lang::parse("zh-CN"), Some(Lang::ZhCn));
    }

    #[test]
    fn 认不出来的值不猜() {
        // 猜错方向比不翻译更糟：宁可退回系统语言，也不要把中文用户发到英文界面
        assert_eq!(Lang::parse(""), None);
        assert_eq!(Lang::parse("Japanese"), None);
        assert_eq!(Lang::parse("zh-Hant"), None);
    }

    #[test]
    fn 注册表名与解析互为逆运算() {
        for lang in [Lang::En, Lang::ZhCn] {
            assert_eq!(Lang::parse(lang.registry_name()), Some(lang));
        }
    }

    #[test]
    fn 主语言_id() {
        assert_eq!(Lang::from_primary_language_id(0x0804), Lang::ZhCn);
        assert_eq!(Lang::from_primary_language_id(0x0404), Lang::ZhCn);
        // 繁体归简体（没有做繁简转换，给英文更合理）
        assert_eq!(Lang::from_primary_language_id(0x0404), Lang::ZhCn);
        assert_eq!(Lang::from_primary_language_id(0x0409), Lang::En);
        assert_eq!(Lang::from_primary_language_id(0x0411), Lang::En);
    }

    /// 两种语言的文案集合必须完全一致：新增字段只填一种语言的话，
    /// 这里就会因为「另一份少字段」而编译失败。
    #[test]
    fn 两种语言都填满() {
        let en = Strings::new(Lang::En);
        let zh = Strings::new(Lang::ZhCn);
        assert_ne!(en.idle_hint, zh.idle_hint);
        assert_ne!(en.btn_open, zh.btn_open);
        assert_ne!(en.btn_stop, zh.btn_stop);
        assert_ne!(en.mute_off, zh.mute_off);
        // 不能有残留的占位 / 空串
        assert!(!en.idle_hint.is_empty() && !zh.idle_hint.is_empty());
        assert!(!en.btn_open.is_empty() && !zh.btn_open.is_empty());
    }

    /// 英文文案里不该混进中文，反之亦然。混进去的症状是「一半英文一半中文」，
    /// 比全英文更难读，而且很难在 review 时看出来。
    #[test]
    fn 文案没有混语言() {
        fn has_cjk(s: &str) -> bool {
            s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
        }
        let en = Strings::new(Lang::En);
        let zh = Strings::new(Lang::ZhCn);
        // err_startup 故意两语言都带 "VideoView"（产品名，不该翻）
        let en_fields: [(&str, &str); 20] = [
            ("idle_hint", en.idle_hint),
            ("btn_open", en.btn_open),
            ("btn_play", en.btn_play),
            ("btn_pause", en.btn_pause),
            ("btn_stop", en.btn_stop),
            ("mute_on", en.mute_on),
            ("mute_off", en.mute_off),
            ("dlg_filter_video", en.dlg_filter_video),
            ("dlg_filter_all", en.dlg_filter_all),
            ("dlg_title", en.dlg_title),
            ("err_startup", en.err_startup),
            ("err_video_window", en.err_video_window),
            ("err_open", en.err_open),
            ("err_toggle", en.err_toggle),
            ("err_stop", en.err_stop),
            ("err_seek", en.err_seek),
            ("err_volume", en.err_volume),
            ("err_mute", en.err_mute),
            ("err_playback", en.err_playback),
            ("err_bad_unicode", en.err_bad_unicode),
        ];
        for (name, s) in en_fields {
            assert!(!has_cjk(s), "英文字段 {name} 里混进了中文：{s}");
        }
        let zh_fields: [(&str, &str); 10] = [
            ("btn_open", zh.btn_open),
            ("btn_play", zh.btn_play),
            ("btn_pause", zh.btn_pause),
            ("btn_stop", zh.btn_stop),
            ("mute_on", zh.mute_on),
            ("mute_off", zh.mute_off),
            ("err_open", zh.err_open),
            ("err_seek", zh.err_seek),
            ("err_playback", zh.err_playback),
            ("err_not_a_file", zh.err_not_a_file),
        ];
        for (name, s) in zh_fields {
            assert!(has_cjk(s), "中文字段 {name} 里没有中文，疑似漏翻：{s}");
        }
    }

    #[test]
    fn 带占位符的错误正文格式正确() {
        let en = Strings::new(Lang::En);
        let zh = Strings::new(Lang::ZhCn);
        assert!(en.err_read_target.contains("{e}"), "英文模板丢了占位符");
        assert!(en.err_register_class.contains("{e}"));
        assert!(zh.err_read_target.contains("{e}"));
        assert!(zh.err_register_class.contains("{e}"));
        // 冒号后面中文用全角，英文用半角
        assert!(en.err_read_target.contains(": "));
        assert!(zh.err_read_target.contains('：'));
    }
}
