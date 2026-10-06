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
//!
//! 分段，每段一个用途，互不重叠：
//!
//! | 段 | 用途 |
//! |---|---|
//! | `1..=99` | 固定项（`id::*`） |
//! | `1000..=1999` | 音轨项 |
//! | `2000..=2999` | 字幕轨项 |
//! | `3000..=3099` | 字幕编码项 |
//! | `3100..=3199` | 字幕大小项 |
//!
//! **音轨和字幕轨曾经共用 `1000 + n` 一段**，靠「当前有没有字幕轨」来判断
//! `n` 是哪一组的下标。那是个真实的歧义：两组的第 3 条是两条不同的轨，
//! 猜错就是「切到了另一组的第 3 条」。现在两组各占一段，接收方一眼就能
//! 知道是哪一组，不需要任何启发式判断。
//!
//! 代价是段变多了，所以 `固定命令id互不重复` 那条测试之外，
//! `decode` 会先把 ID 落到段、再落到段内下标，两层都不越界才算过。
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

/// 动态项各段的起始 ID。见模块说明里关于编号的那张表。
///
/// 每一段留 1000 个位置：轨道多的 MKV（各语种音轨 + 完整字幕组）轻松上到
/// 三四十条，而**最多能画多少行**不归这里管 —— 面板那边按实际行数排版、
/// 高度不够时截断（见 `app::panel_drawn_rows`）。所以给到 1000 已经很宽松，
/// 真撞上了 `decode` 会返回 `None` 而不是悄悄切错轨。
pub mod base {
    /// 音轨项
    pub const AUDIO: u16 = 1000;
    /// 字幕轨项
    pub const SUB: u16 = 2000;
    /// 字幕编码项（`sub-codepage`）
    pub const CODING: u16 = 3000;
    /// 字幕大小项（`sub-scale`）
    pub const SCALE: u16 = 3100;
}

/// 每段的长度（能放多少个项）。
///
/// **显式逐段列出来，而不是一个统一的段长。** 曾经给所有段同一个
/// `SEG_LEN = 1000`，结果 `CODING`(3000) + 1000 越过了 `SCALE`(3100)：
/// 大小子菜单的第 0 项解出来是「字幕轨第 0 条」，点一下会去切字幕轨。
/// 那不是理论问题 —— 写完 `decode` 的第一版单测立刻就红了。
///
/// 音轨 / 字幕轨给 1000 是因为条目数运行期才知道（各语种音轨 + 完整字幕组
/// 的 MKV 上三四十条很常见），而**能画多少行**不归这里管 —— 面板那边按
/// 实际行数排版、高度不够时截断（见 `app::panel_drawn_rows`）。
/// 编码 / 大小的条目数是编译期常量（`CODINGS.len()` / `SCALES.len()`），
/// 各留 40 就够，剩下的空位靠「越界解成 `Unknown`」兜住。
pub const SEGMENTS: &[(u16, u16)] = &[
    (base::AUDIO, 1000),
    (base::SUB, 1000),
    (base::CODING, 40),
    (base::SCALE, 40),
];

/// 「没有轨道」时的占位项 ID。
///
/// 用 `0xffff` 而不是 0：0 在 Win32 菜单里有特殊含义（`AppendMenuW` 的
/// `MF_STRING` + `uidNewItem = 0` 会被当成「按位置插入」），而且它和
/// 各动态段都不冲突。
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

/// 菜单里可选的字幕编码，映射到 mpv 的 `sub-codepage`。
///
/// ## 这个列表是查出来的，不是拍脑袋列的
///
/// mpv **没有** `sub-encoding` 这个属性 —— 名字是 `sub-codepage`，
/// 而且它是自由 String（`option-info/sub-codepage` 的 `type` 就是 `String`、
/// `default-value` 是 `auto`），任何字符串都写得进去、写错了也不报错，
/// 只在真正解码字幕时才炸。所以这张表的每个值都得是有依据的：
///
/// - `auto` = mpv 的默认值，让 uchardet 去猜
/// - `gbk` / `gb18030` / `big5` = 简体中文与繁体中文的老字幕（GB2312 已被
///   GBK 完全覆盖，不再单列）
/// - `shift-jis` / `euc-kr` = 日文与韩文的老字幕
/// - `latin-1` = 西欧那一票（这条是**兜底**，不是常用）
///
/// 标签照抄编码名，因为那些本来就是专有名词（GBK / Big5 / Shift-JIS /
/// EUC-KR / Latin-1），翻译成中文反而对不上 mpv 文档和 iconv 的名字。
/// 只有 `auto` 需要一个译名，走 `lang::Strings::sub_encoding_auto`。
///
/// ## 一个必须知道的坑：mpv 先猜 UTF-8
///
/// 按 mpv 手册，判定顺序是「`+codepage` 强制 → 数据像 UTF-8 就当 UTF-8 →
/// 指定的 codepage → uchardet 猜 → UTF-8-BROKEN」。也就是说
/// **把编码选成 GBK，对一个本来就长得像 UTF-8 的文件不起作用**。
/// 老字幕文件一般有 UTF-8 之外的字节，会落到第三步，所以实践中够用；
/// 真要连 UTF-8 也强行按 GBK 解，mpv 的写法是 `--sub-codepage=+gbk`，
/// 本菜单**故意不给**这个选项 —— 那会把 UTF-8 字幕整个变成乱码，
/// 而用户没有「我确定这个文件就是 GBK」的知识去做这种选择。
///
/// ## 只对文本字幕文件有效
///
/// 手册明确：「in particular subtitles in mkv files are always assumed to be
/// UTF-8」。内嵌字幕（mov_text / MKV SRT）由解封装器转成 UTF-8，
/// 这个选项对它们没有作用。所以菜单项的文案里带了「外挂字幕」。
pub const CODINGS: &[&str] = &[
    "auto",
    "gbk",
    "gb18030",
    "big5",
    "shift-jis",
    "euc-kr",
    "latin-1",
];

/// 菜单里可选的字幕大小，映射到 mpv 的 `sub-scale`。
///
/// mpv 的取值范围是 `0.1..10`，这里只给六个常用档。做成**档位**而不是
/// 自由输入框是因为菜单本来就没有输入框，而「多大」这个问题绝大多数人
/// 只会在几个档之间挑。
///
/// 50% 不是给「字幕太大了」用的（那种情况用户会想要更小），是给小字号
/// 字幕在 4K 上铺满全屏时用的。
pub const SCALES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

/// 菜单命令落到哪一段。
///
/// 有了分段之后，接收方**不需要**再猜「这个 ID 是音轨还是字幕轨」。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decoded {
    /// 固定项，值就是 `id::*` 里的常量
    Fixed(u16),
    /// 第 `n` 条音轨
    AudioTrack(usize),
    /// 第 `n` 条字幕轨
    SubTrack(usize),
    /// 把 `sub-codepage` 设成这个值
    SubCoding(&'static str),
    /// 把 `sub-scale` 设成这个值
    SubScale(f64),
    /// 占位项或不该出现的东西。不当错误处理。
    Unknown,
}

/// 把菜单返回的命令 ID 翻译成「哪个段里的第几项」。
///
/// 两层都越界就返回 `Unknown` 而不是 `unwrap`：ID 来自 Win32，理论上只有
/// 我们自己写进去的值会回来，但越界能测出来比崩在用户脸上好。
pub fn decode(cmd: Command) -> Decoded {
    // 占位项先拦掉：它是 `u16::MAX`，本来也落不进任何段（各段最远到 3140），
    // 但明确写出来比「碰巧落不进」好 —— 万一以后有人加段加到 65000，
    // 这里就是唯一拦住它的地方。
    if cmd == NO_TRACK_ID {
        return Decoded::Unknown;
    }
    for (start, len) in SEGMENTS {
        let Some(off) = cmd.checked_sub(*start) else {
            continue;
        };
        if off >= *len {
            continue;
        }
        let n = off as usize;
        return match *start {
            base::AUDIO => Decoded::AudioTrack(n),
            base::SUB => Decoded::SubTrack(n),
            base::CODING => match CODINGS.get(n) {
                Some(v) => Decoded::SubCoding(v),
                None => Decoded::Unknown,
            },
            base::SCALE => match SCALES.get(n) {
                Some(v) => Decoded::SubScale(*v),
                None => Decoded::Unknown,
            },
            // `SEGMENTS` 里出现了一个上面没处理的新起点。加段的人忘了在
            // 这里加分支 —— 宁可当未知，也不要把别的段的语义套上去。
            _ => Decoded::Unknown,
        };
    }
    Decoded::Fixed(cmd)
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
    /// 当前的 `sub-codepage`。用来给编码子菜单打勾。
    ///
    /// 空串（mpv 那边读不到）会让**没有任何一项被打勾** —— 与其猜一个
    /// 「大概是 auto」去打勾，不如都不勾：用户看到的是一个正常的、
    /// 暂时没选中任何项的单选组，而不是一个错误的勾。
    pub sub_codepage: &'a str,
    /// 当前的 `sub-scale`。用来给大小子菜单打勾。
    pub sub_scale: f64,
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
                append(sub, base::AUDIO + n as u16, &describe(t, s), t.selected);
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

/// 字幕子菜单。第一项恒是「关闭字幕」，末尾是两个嵌套子菜单。
///
/// ## 编码与大小为什么塞进字幕子菜单，而不是主菜单再开两项
///
/// 它们是**只对字幕生效**的设置（mpv 的 `sub-codepage` / `sub-scale`），
/// 而主菜单已经 24 项了。塞进来的好处是：会为了「字幕乱码」去翻字幕菜单的
/// 人，和会为了「字幕太大」去翻字幕菜单的**是同一批** —— 用户对「字幕的
/// 事情在字幕菜单里找」这个心智模型本来就成立。代价是三层深，但 Windows
/// 上这就是 `设置 ▸ 字幕` 的样子，不是缺陷。
///
/// 代价是**没有字幕轨时这两个子菜单也跟着置灰**。这是可接受的：没有字幕轨
/// 时既没有乱码可修、也没有大小要调；而用户拖一个 `.srt` 进来之后轨就
/// 出现了，菜单立刻可用。
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
                append(sub, base::SUB + n as u16, &describe(t, s), t.selected);
            }
            separator(sub);
            append_coding_sub(sub, s, snap);
            append_scale_sub(sub, s, snap);
        }
        // 字幕那个多一个条件：**关字幕**在没有字幕轨时也没意义
        // （本来就没字幕在放，关不掉什么）。
        append_sub(menu, s.menu_sub, sub, snap.loaded && !empty);
    }
}

/// 「编码」子菜单，挂在字幕子菜单末尾。
fn append_coding_sub(parent: HMENU, s: &lang::Strings, snap: Snapshot<'_>) {
    // SAFETY: 只调 Win32 菜单 API。
    unsafe {
        let Ok(sub) = CreatePopupMenu() else { return };
        for (n, &code) in CODINGS.iter().enumerate() {
            append(
                sub,
                base::CODING + n as u16,
                coding_label(code, s),
                code == snap.sub_codepage,
            );
        }
        append_sub(parent, s.menu_sub_coding, sub, true);
    }
}

/// 「大小」子菜单，挂在字幕子菜单末尾。
fn append_scale_sub(parent: HMENU, s: &lang::Strings, snap: Snapshot<'_>) {
    // SAFETY: 同 `append_coding_sub`。
    unsafe {
        let Ok(sub) = CreatePopupMenu() else { return };
        for (n, &v) in SCALES.iter().enumerate() {
            // 标签是「125%」这种纯数字 + 百分号，任何语言都一样，
            // 所以不进 `lang::Strings`（那里全是「一条文案一个键」，
            // 塞一个每次现算的字符串进去会让那个约定失效）。
            append(
                sub,
                base::SCALE + n as u16,
                &format!("{:.0}%", v * 100.0),
                (v - snap.sub_scale).abs() < 1e-6,
            );
        }
        append_sub(parent, s.menu_sub_scale, sub, true);
    }
}

/// 编码项的标签。只有 `auto` 需要翻译，其余照抄编码名。
fn coding_label(code: &'static str, s: &lang::Strings) -> &'static str {
    if code == "auto" {
        s.sub_encoding_auto
    } else {
        // `CODINGS` 是 `&[&'static str]` 的字面量表，所以这里拿到的确实是
        // `'static`，可以直接返回而不分配。
        code
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
        find_opt(m, want).unwrap_or_else(|| {
            panic!(
                "菜单里找不到 {want:?}；现有：{:?}",
                (0..count(m) as u32)
                    .map(|i| label(m, i))
                    .collect::<Vec<_>>()
            )
        })
    }

    /// 找不到时返回 `None` 而不是 panic —— 用来断言「某一项**不**在菜单里」。
    fn find_opt(m: HMENU, want: &str) -> Option<u32> {
        (0..count(m) as u32).find(|i| label(m, *i).as_deref() == Some(want))
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
        snap_full(tracks, "auto", 1.0)
    }

    /// 能指定字幕编码 / 大小的快照，测那两个子菜单的打勾用。
    fn snap_full<'a>(tracks: &'a TrackSet, codepage: &'static str, scale: f64) -> Snapshot<'a> {
        // `'a` 只借 `tracks`；`codepage` 是 `&'static str`（测试里传的全是
        // 字面量），所以不用跟着 `'a` 走。
        Snapshot {
            loaded: true,
            paused: false,
            speed: 1.0,
            theatre: false,
            fullscreen: false,
            diag_open: false,
            sub_codepage: codepage,
            sub_scale: scale,
            tracks,
        }
    }

    #[test]
    fn 音轨与字幕轨的命令不再共用同一段() {
        // 这条守着 0.6.1 修掉的那个设计瑕疵：以前两组都从 `TRACK_BASE + n`
        // 起，接收方只能靠「当前有没有字幕轨」猜 `n` 是哪一组的下标，
        // 而两组都各有第 3 条时猜错就是「切到了另一组的第 3 条」。
        assert_ne!(base::AUDIO, base::SUB, "音轨与字幕轨必须各占一段");
        // 两段的内容对调也必须解出不同类型
        assert_ne!(decode(base::SUB), decode(base::AUDIO));
        assert_eq!(decode(base::AUDIO), Decoded::AudioTrack(0));
        assert_eq!(decode(base::SUB), Decoded::SubTrack(0));
        assert_eq!(decode(base::AUDIO + 7), Decoded::AudioTrack(7));
        assert_eq!(decode(base::SUB + 7), Decoded::SubTrack(7));
    }

    #[test]
    fn 固定命令解码回自己() {
        assert_eq!(decode(id::OPEN), Decoded::Fixed(id::OPEN));
        assert_eq!(decode(id::FULLSCREEN), Decoded::Fixed(id::FULLSCREEN));
        assert_eq!(decode(id::COPY_REPORT), Decoded::Fixed(id::COPY_REPORT));
    }

    #[test]
    fn 占位项解成未知而不是当成某条轨() {
        assert_eq!(decode(NO_TRACK_ID), Decoded::Unknown);
    }

    #[test]
    fn 段内下标越界解成未知() {
        // ID 来自 Win32，理论上只有我们自己写进去的会回来。但越界能测出来
        // 好过在用户脸上 panic —— 而 `n` 越界恰好是「菜单和状态不同步」时
        // 最可能发生的情况（切轨之后菜单还没重画又弹了一次）。
        for (start, len) in SEGMENTS {
            // 段尾之后那个值**可能**正好是下一段的起点（各段首尾相接，
            // 比如 `AUDIO` 的段尾 2000 就是 `SUB` 的起点），所以不能断言
            // 它是 `Fixed`。能断言的是：它**不再**被当成原来那一段的项。
            let start = *start;
            let out = decode(start + len);
            assert!(
                !matches!(
                    (start, out),
                    (base::AUDIO, Decoded::AudioTrack(_))
                        | (base::SUB, Decoded::SubTrack(_))
                        | (base::CODING, Decoded::SubCoding(_))
                        | (base::SCALE, Decoded::SubScale(_))
                ),
                "段 {start} 越界后的 {out:?} 还被当成了这一段的项"
            );
        }
        // 编码 / 大小段里「在段内但超出表长」的位置必须解成未知。
        // 这两个表的条数（7 / 6）小于段长（各 40），所以段内确实有空位。
        assert_eq!(
            decode(base::CODING + CODINGS.len() as u16),
            Decoded::Unknown
        );
        assert_eq!(decode(base::SCALE + SCALES.len() as u16), Decoded::Unknown);
        // 段内的合法下标仍然能解出来（防止上面的段长把整段都废了）
        assert_eq!(decode(base::CODING), Decoded::SubCoding("auto"));
        assert_eq!(decode(base::SCALE), Decoded::SubScale(0.5));
    }

    #[test]
    fn 各段互不重叠() {
        // 曾经所有段都用同一个 `SEG_LEN = 1000`，于是 `CODING`(3000) 的
        // 段尾 4000 越过了 `SCALE`(3100)。表现是「大小子菜单的第 0 项
        // 被解成字幕轨第 0 条」。这条测试就是为了不让它回去。
        for (i, (a, alen)) in SEGMENTS.iter().enumerate() {
            for (b, _) in SEGMENTS.iter().skip(i + 1) {
                assert!(
                    a + alen <= *b,
                    "段 {a}（长度 {alen}）越过了段 {b} 的起点，两段重叠"
                );
            }
        }
        // 编码 / 大小的条目数必须放得进各自的段
        assert!(CODINGS.len() as u16 <= 40, "编码表比段还长");
        assert!(SCALES.len() as u16 <= 40, "大小表比段还长");
        // 而且不能撞上占位 ID。
        //
        // 比的是 `SEGMENT_CEILING` 而不是 `NO_TRACK_ID`：后者等于
        // `u16::MAX`，`x <= u16::MAX` 恒真，clippy 会以
        // `absurd_extreme_comparisons` 直接拒掉这条测试（而
        // `x < u16::MAX` 是常量，clippy 同样会以「断言恒真」警告）。
        // 真要守的是「各段都待在低位区」，4000 是给这个约束一个具体数字，
        // 而占位 ID 的具体值由 `固定命令id互不重复且都在动态段之前` 守着
        // 「占位项解码成 Unknown」。
        const SEGMENT_CEILING: u16 = 4000;
        assert!(
            SEGMENTS.iter().all(|(s, l)| s + l <= SEGMENT_CEILING),
            "有段越过了 {SEGMENT_CEILING}，离占位 ID 太近"
        );
        // 顺带确认 `SEGMENT_CEILING` 这个数字本身还站得住
        assert_eq!(decode(NO_TRACK_ID), Decoded::Unknown);
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
        assert_eq!(cmd(sub.0, 0), u32::from(base::AUDIO));
        assert_eq!(cmd(sub.0, 1), u32::from(base::AUDIO) + 1);
    }

    #[test]
    fn 字幕子菜单第一项恒是关闭() {
        let s = st();
        let mut set = TrackSet::default();
        set.sub.push(sub(1, true));
        let m = owned(&s, snap(&set));
        let pos = find(m.0, s.menu_sub);
        let sub = Owned(sub_handle(m.0, pos));
        // 关闭 + 分隔 + 1 条轨 + 分隔 + 编码子菜单 + 大小子菜单 = 6
        assert_eq!(count(sub.0), 6, "字幕子菜单的构成变了");
        assert_eq!(cmd(sub.0, 0), id::SUB_OFF as u32);
        // 正在放字幕时「关闭」不该打勾
        assert!(!is_checked(sub.0, 0));
        assert!(is_checked(sub.0, 2), "第 2 项是分隔线，第 3 项才是那条轨");
    }

    #[test]
    fn 编码与大小是字幕子菜单里的两个子菜单() {
        let s = st();
        let mut set = TrackSet::default();
        set.sub.push(sub(1, true));
        let m = owned(&s, snap(&set));
        let sub = Owned(sub_handle(m.0, find(m.0, s.menu_sub)));

        let cpos = find(sub.0, s.menu_sub_coding);
        let spos = find(sub.0, s.menu_sub_scale);
        assert!(!is_grayed(sub.0, cpos), "编码子菜单不该置灰");
        assert!(!is_grayed(sub.0, spos), "大小子菜单不该置灰");

        let coding = Owned(sub_handle(sub.0, cpos));
        assert_eq!(
            count(coding.0),
            CODINGS.len() as i32,
            "编码子菜单每种编码一项"
        );
        let scale = Owned(sub_handle(sub.0, spos));
        assert_eq!(
            count(scale.0),
            SCALES.len() as i32,
            "大小子菜单每个档位一项"
        );
    }

    #[test]
    fn 编码子菜单给当前编码打勾() {
        let s = st();
        let mut set = TrackSet::default();
        set.sub.push(sub(1, true));
        // 当前是 gbk（模拟用户从 GBK 字幕切过来）
        let m = owned(&s, snap_full(&set, "gbk", 1.0));
        let sub = Owned(sub_handle(m.0, find(m.0, s.menu_sub)));
        let coding = Owned(sub_handle(sub.0, find(sub.0, s.menu_sub_coding)));
        let want = CODINGS
            .iter()
            .position(|c| *c == "gbk")
            .expect("表里有 gbk");
        let checked: Vec<bool> = (0..count(coding.0) as u32)
            .map(|i| is_checked(coding.0, i))
            .collect();
        assert!(
            checked[want],
            "当前编码那项要打勾，实际勾在 {:?}",
            checked.iter().position(|b| *b)
        );
        assert_eq!(
            checked.iter().filter(|b| **b).count(),
            1,
            "这是个单选组，只能有一项打勾"
        );
    }

    #[test]
    fn 大小子菜单给当前大小打勾() {
        let s = st();
        let mut set = TrackSet::default();
        set.sub.push(sub(1, true));
        let m = owned(&s, snap_full(&set, "auto", 1.25));
        let sub = Owned(sub_handle(m.0, find(m.0, s.menu_sub)));
        let scale = Owned(sub_handle(sub.0, find(sub.0, s.menu_sub_scale)));
        let want = SCALES
            .iter()
            .position(|v| (v - 1.25).abs() < 1e-6)
            .expect("表里有 1.25");
        assert!(is_checked(scale.0, want as u32));
        assert_eq!(
            (0..count(scale.0) as u32)
                .filter(|i| is_checked(scale.0, *i))
                .count(),
            1,
            "只能有一档打勾"
        );
    }

    #[test]
    fn 读不到当前编码时一项都不打勾() {
        let s = st();
        let mut set = TrackSet::default();
        set.sub.push(sub(1, true));
        // 空串 = mpv 那边读不到。此时**不猜**，宁可都不勾
        let m = owned(&s, snap_full(&set, "", 1.0));
        let sub = Owned(sub_handle(m.0, find(m.0, s.menu_sub)));
        let coding = Owned(sub_handle(sub.0, find(sub.0, s.menu_sub_coding)));
        assert!(
            (0..count(coding.0) as u32).all(|i| !is_checked(coding.0, i)),
            "读不到时不该瞎勾一项"
        );
    }

    #[test]
    fn 编码与大小子菜单里的每一项都有对应命令且能解码回来() {
        let s = st();
        let mut set = TrackSet::default();
        set.sub.push(sub(1, true));
        let m = owned(&s, snap(&set));
        let sub = Owned(sub_handle(m.0, find(m.0, s.menu_sub)));

        let coding = Owned(sub_handle(sub.0, find(sub.0, s.menu_sub_coding)));
        for (n, _) in CODINGS.iter().enumerate() {
            let c = cmd(coding.0, n as u32) as u16;
            assert_eq!(decode(c), Decoded::SubCoding(CODINGS[n]));
        }
        let scale = Owned(sub_handle(sub.0, find(sub.0, s.menu_sub_scale)));
        for (n, v) in SCALES.iter().enumerate() {
            let c = cmd(scale.0, n as u32) as u16;
            assert_eq!(decode(c), Decoded::SubScale(*v));
        }
    }

    #[test]
    fn 没有字幕轨时编码与大小跟着一起消失() {
        let s = st();
        let m = owned(&s, snap(&EMPTY));
        let sub = Owned(sub_handle(m.0, find(m.0, s.menu_sub)));
        assert_eq!(count(sub.0), 1, "没有字幕轨时只有一个占位项");
        assert!(
            find_opt(sub.0, s.menu_sub_coding).is_none(),
            "没有字幕轨就不该出现编码子菜单"
        );
        assert!(find_opt(sub.0, s.menu_sub_scale).is_none());
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
            all.iter().all(|c| *c < base::AUDIO),
            "固定命令 ID 必须小于动态段起点 base::AUDIO（{}）",
            base::AUDIO
        );
        // 「不撞上占位 ID」由 `各段互不重叠` 那条统一盯着，这里不重复。
        // 真正的护栏是「占位项置灰、点它什么都不发生」，那条在
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
