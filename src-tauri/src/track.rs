//! 轨道：字幕轨与音轨。
//!
//! ## 为什么单独一个模块
//!
//! mpv 的 `track-list` 是一个**节点树**，实测（mpv 0.41、`loop60s.mp4`）
//! 每条轨道有 28 个字段，其中大半是这里用不上的（`ff-index`、`src-id`、
//! `main-selection`、`commentary`、`albumart`…）。把这 28 个字段原样搬进
//! 界面状态没有意义，只会让「哪些字段是真的在用」这件事看不出来。
//!
//! 所以这里做的是**收窄**：只留下界面要显示、要比较、要发给 mpv 的那几个，
//! 并把每个字段「取不到时怎么办」写清楚 —— mpv 的字段是**按轨道类型**
//! 出现的（`demux-samplerate` 只在音频轨上有，`demux-w` 只在视频轨上），
//! 照着一种轨道去读另一种必然读到 `None`。
//!
//! ## 轨道的顺序有意义
//!
//! `track-list` 是**有序数组**（实测 `node.format = MPV_FORMAT_NODE_ARRAY`），
//! 数组里第一个 `default: true` 的就是默认轨。所以这里保留原顺序，
//! 不用 map 也不用排序 —— 排序会把「默认轨」这个信息丢掉，而 mpv 的
//! `cycle sub` 之类的命令也是按这个顺序走的。

use crate::lang;
use crate::mpv::MpvValue;

/// 一条轨道。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// mpv 侧用来指认这条轨道的 `id`。发给 `sid` / `aid` 的就是它。
    pub id: i64,
    pub kind: TrackKind,
    /// 短标签（`h264` / `aac`），给界面拼一行文字用
    pub codec: String,
    /// 完整描述（`H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10`）。
    ///
    /// 太长，界面上不用；留着是因为**报告**里它比 `codec` 有用得多
    /// （同一个 `codec` 名字在不同 profile 下行为差别很大）。
    pub codec_desc: String,
    /// 语言码（`und` / `eng` / `chi`）。`und` 是「未标记」，界面上不显示。
    pub language: String,
    /// 这条轨道带的名字（内嵌字幕轨常见，外挂的没有）
    pub title: String,
    /// 当前是否在用。mpv 用 `selected` 标记，不是靠比较 id。
    pub selected: bool,
    /// 是否是文件默认的轨
    pub default: bool,
    /// 是否是外挂字幕（`external`）。外挂的可以移出，文件内嵌的不行。
    pub external: bool,
    /// 音频轨：声道数与采样率。字幕轨没有这些字段。
    pub audio_channels: Option<i64>,
    pub sample_rate: Option<i64>,
}

/// 轨道类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    /// 字幕。mpv 里叫 `sub`，不叫 `subtitle`。
    Sub,
}

impl TrackKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            "sub" => Some(Self::Sub),
            _ => None,
        }
    }

    /// 切轨属性名。mpv 用 `aid` / `sid`，视频轨没有对应的（视频轨不能切）。
    pub fn property(self) -> Option<&'static str> {
        match self {
            TrackKind::Video => None,
            TrackKind::Audio => Some("aid"),
            TrackKind::Sub => Some("sid"),
        }
    }
}

/// 一个视频里的全部可选轨道（不含视频轨本身 —— 视频轨没有「切」这回事）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackSet {
    pub audio: Vec<Track>,
    pub sub: Vec<Track>,
}

impl TrackSet {
    /// 从 `track-list` 的节点树解析。
    ///
    /// 单条轨道解析失败**不丢弃整棵树**：mpv 未来加字段、加类型时，
    /// 一条看不懂的轨道不该让所有轨道都消失。
    pub fn from_node(root: &MpvValue) -> Self {
        let mut set = Self::default();
        let Some(items) = root.list() else {
            return set;
        };
        for (_, node) in items {
            // 列表项是对象；不是就跳过（实测是对象，但别假设）
            if node.list().is_none() {
                continue;
            }
            let Some(kind_s) = node.get_str("type") else {
                continue;
            };
            let Some(kind) = TrackKind::parse(kind_s) else {
                // mpv 还有 `attachment`（封面之类），这里不接
                continue;
            };
            // `id` 缺了就没法切它，只能跳过
            let Some(id) = node.get_int("id") else {
                continue;
            };
            let metadata = node.get("metadata");
            let track = Track {
                id,
                kind,
                codec: node.get_str("codec").unwrap_or_default().to_string(),
                codec_desc: node.get_str("codec-desc").unwrap_or_default().to_string(),
                // `und` = undefined。显示出来是噪声
                language: metadata
                    .and_then(|m| m.get_str("language"))
                    .filter(|l| !l.is_empty() && *l != "und")
                    .unwrap_or_default()
                    .to_string(),
                title: metadata
                    .and_then(|m| m.get_str("title"))
                    .unwrap_or_default()
                    .to_string(),
                selected: node.get_flag("selected").unwrap_or(false),
                default: node.get_flag("default").unwrap_or(false),
                external: node.get_flag("external").unwrap_or(false),
                audio_channels: node.get_int("audio-channels"),
                sample_rate: node.get_int("demux-samplerate"),
            };
            match kind {
                TrackKind::Audio => set.audio.push(track),
                TrackKind::Sub => set.sub.push(track),
                TrackKind::Video => {}
            }
        }
        set
    }

    /// 当前正在用的音频轨。
    pub fn current_audio(&self) -> Option<&Track> {
        self.audio.iter().find(|t| t.selected)
    }

    /// 当前正在用的字幕轨（关着字幕时是 `None`）。
    pub fn current_sub(&self) -> Option<&Track> {
        self.sub.iter().find(|t| t.selected)
    }
}

/// 在 `len` 条轨里从 `pos` 走 `step` 格，**两端都绕回**。
///
/// 单独抽成纯函数是为了能单测 —— 这个算术在 `app.rs` 里，而 `App` 要测它
/// 得先建出一个真的 `MpvPlayer`；这里只需要一个整数。
///
/// 绕回是有意的：字幕轨经常有两条以上，「按 `L` 循环」是用户预期的行为；
/// 不绕回的话按过头就停住，用户会以为漏了一条轨。
///
/// 用 `rem_euclid` 而不是 `%`：Rust 的 `%` 对负数给**负**的余数
/// （`-1 % 3 == -1`），往回切第一轨时会算出 `-1`，再拿去索引就 panic 了。
/// `rem_euclid` 的模数取正，结果恒在 `[0, len)`。
pub fn step_index(len: usize, pos: usize, step: isize) -> Option<usize> {
    if len == 0 || pos >= len {
        return None;
    }
    Some((pos as isize + step).rem_euclid(len as isize) as usize)
}

/// 菜单里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackRow {
    /// 分组标题，不可选（不可选的行不参与光标移动）
    Heading(&'static str),
    /// 一行说明，没有轨道可选时用。
    ///
    /// 存在的理由：没有文件（或者文件里一条可选轨都没有）时菜单要是
    /// **空的**，按 `T` 就完全没有反应 —— 用户分不清是「坏了」还是
    /// 「这个文件本来就没有字幕」。给一行字把这件事说清楚。
    /// 与 `Heading` 的区别是它不划分分组：`group_of_row` 只认 `Heading`。
    Note(&'static str),
    /// 一条可选轨道
    Item {
        /// 回到 `TrackSet` 时要用的下标（分组各自独立编号）
        index: usize,
        label: String,
        /// 是否正在用（画成强调色）
        active: bool,
        /// 是否是文件默认轨（画个标记）
        is_default: bool,
    },
    /// 「关闭字幕」—— mpv 用 `sid = no` 表示关掉
    OffSubtitle { active: bool },
}

impl TrackRow {
    pub fn selectable(&self) -> bool {
        matches!(self, TrackRow::Item { .. } | TrackRow::OffSubtitle { .. })
    }
}

/// 菜单里一行的「身份」。
///
/// 行下标不是身份：切一次轨，`selected` 会变，行数与顺序都可能跟着变
/// （外挂字幕加载完多一行），沿用旧下标会让光标跳到别的轨上。所以重建
/// 菜单行之后靠这个把光标放回**同一条**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowId {
    /// 分组标题（音频 / 字幕）。同一次运行内语言不变，标题是稳定的
    Heading(&'static str),
    /// 「这个文件没有可选轨道」那一行
    Note(&'static str),
    /// 第 `id` 号轨。带 `kind` 是因为音轨和字幕轨的 id 各自从 1 开始
    Track(TrackKind, i64),
    /// 「关闭字幕」那一行
    OffSubtitle,
}

/// 第 `i` 行的身份。`None` = 下标越界，或该行没法指认到某条轨。
///
/// **必须用与 `rows` 配套的那份 `set`**。`Item { index }` 里的 `index` 是
/// **组内**下标，只有和生成这批 `rows` 时的 `set` 放一起才翻译得出来；
/// 拿新 `set` 去查旧 `rows` 会得到错的 id，光标就会静默跳到别的轨上。
pub fn row_id(rows: &[TrackRow], set: &TrackSet, s: &lang::Strings, i: usize) -> Option<RowId> {
    match rows.get(i)? {
        TrackRow::Heading(t) => Some(RowId::Heading(t)),
        TrackRow::Note(t) => Some(RowId::Note(t)),
        TrackRow::OffSubtitle { .. } => Some(RowId::OffSubtitle),
        TrackRow::Item { index, .. } => {
            let kind = group_of_row(rows, s, i)?;
            // 列表与菜单行不一致时返回 `None`，`cursor_for` 会退到第一个可选行
            let t = group_list(set, kind).get(*index)?;
            Some(RowId::Track(kind, t.id))
        }
    }
}

/// 第 `i` 行属于音频组还是字幕组。
///
/// 往上找最近的分组标题。不靠 `Item { index }` 判断 —— 那是**组内**编号，
/// 一个数字说明不了是第几组的第几条。
///
/// 下标越界、或者该行**自己就是**标题时返回 `None`：标题不属于任何分组，
/// 而只往上找会把「字幕标题」判成上一组的音频。调用方（`row_id` /
/// `activate_row`）本来也只对 `Item` 行问这个，但让它成为全函数比让每个
/// 调用方各自记住「别问标题行」可靠。
pub fn group_of_row(rows: &[TrackRow], s: &lang::Strings, i: usize) -> Option<TrackKind> {
    if i >= rows.len() || matches!(rows[i], TrackRow::Heading(_)) {
        return None;
    }
    for row in rows[..i].iter().rev() {
        if let TrackRow::Heading(t) = row {
            return Some(if *t == s.track_sub {
                TrackKind::Sub
            } else {
                TrackKind::Audio
            });
        }
    }
    None
}

/// 某个分组对应的轨道列表。
pub fn group_list(set: &TrackSet, kind: TrackKind) -> &[Track] {
    match kind {
        TrackKind::Sub => &set.sub,
        _ => &set.audio,
    }
}

/// 光标该停在哪一行。`want` 找不到就退到第一个可选行。
pub fn cursor_for(
    rows: &[TrackRow],
    set: &TrackSet,
    s: &lang::Strings,
    want: Option<RowId>,
) -> usize {
    if let Some(w) = want {
        if let Some(i) = (0..rows.len()).find(|i| row_id(rows, set, s, *i) == Some(w)) {
            return i;
        }
    }
    (0..rows.len()).find(|i| rows[*i].selectable()).unwrap_or(0)
}

/// 把 `TrackSet` 编成菜单行。
///
/// 行序固定为「音频 → 字幕」，与 mpv 自己菜单的惯例一致（视频轨不可切，
/// 不出现）。
///
/// 一条轨都没有时给一行说明而不是返回空 —— 见 `TrackRow::Note`。
pub fn build_rows(set: &TrackSet, s: &lang::Strings) -> Vec<TrackRow> {
    let mut rows = Vec::new();

    if !set.audio.is_empty() {
        rows.push(TrackRow::Heading(s.track_audio));
        for (i, t) in set.audio.iter().enumerate() {
            rows.push(TrackRow::Item {
                index: i,
                label: describe(t, s),
                active: t.selected,
                is_default: t.default,
            });
        }
    }

    if !set.sub.is_empty() {
        rows.push(TrackRow::Heading(s.track_sub));
        rows.push(TrackRow::OffSubtitle {
            active: set.current_sub().is_none(),
        });
        for (i, t) in set.sub.iter().enumerate() {
            rows.push(TrackRow::Item {
                index: i,
                label: describe(t, s),
                active: t.selected,
                is_default: t.default,
            });
        }
    }

    if rows.is_empty() {
        rows.push(TrackRow::Note(s.track_none));
    } else {
        // 有轨可列时补一行操作提示。
        //
        // 存在的理由：菜单打开后 `↑` `↓` 不再调音量、`Enter` 才是选中 ——
        // 这套键位和播放器其余部分不一样，不说一声的话用户按 `↑` 听到音量
        // 变了会以为程序有 bug。放在菜单里而不是只写在 `?` 那一页，是因为
        // 用户学切轨时看的是菜单，不是帮助页。
        rows.push(TrackRow::Note(s.track_hint));
    }

    rows
}

/// 一条轨道的一行文字。
///
/// 每一段之间用 ` · ` 分隔，**不靠空格**。这一点是实测截图逼出来的：
/// 早一版拼出来是 `1 44 声道aac` —— `44` 是采样率（44100 → 44），
/// 紧跟在声道数后面、又没有单位，读起来像「44 声道」。空格分隔在这里
/// 是有害的，因为相邻两段都是数字。
///
/// 顺序是「用途 → 语言 → 声道 / 采样率 → 编码」，因为 mpv 菜单也是这么排的：
/// 多音轨片子里「哪条是国语」比「它是 aac 还是 ac3」重要得多。
pub fn describe(t: &Track, s: &lang::Strings) -> String {
    // 标题 > 语言码 > 编码名。都没有就只剩编码名，此时不加任何前缀
    let primary = if !t.title.is_empty() {
        t.title.clone()
    } else if !t.language.is_empty() {
        language_name(&t.language).to_string()
    } else {
        String::new()
    };

    // 先收集非空的一段段，最后用分隔符拼起来 —— 拼着加的话很容易在某一段
    // 缺失时留下多余的分隔符（开头或结尾挂一个 `·`）
    let mut parts: Vec<String> = Vec::new();
    if !primary.is_empty() {
        parts.push(primary);
    }

    match t.kind {
        TrackKind::Audio => {
            if let Some(ch) = t.audio_channels.filter(|c| *c > 0) {
                let mut seg = format!("{ch} {}", s.track_unit_channels);
                // 采样率带单位。48000 显示成 `48 kHz` 而不是光一个 `48` ——
                // 光数字会被当成声道数或者帧率
                if let Some(sr) = t.sample_rate.filter(|r| *r > 0) {
                    // 44100 → `44.1 kHz`；48000 → `48 kHz`。一位小数够了，
                    // 更高位（192000 → 192）没有播放器会care
                    let khz = sr as f64 / 1000.0;
                    if (khz - khz.round()).abs() < 0.05 {
                        seg.push_str(&format!(" · {} kHz", khz.round() as i64));
                    } else {
                        seg.push_str(&format!(" · {khz:.1} kHz"));
                    }
                }
                parts.push(seg);
            }
        }
        TrackKind::Sub => {
            if t.external {
                parts.push(s.track_external.to_string());
            }
        }
        TrackKind::Video => {}
    }

    if !t.codec.is_empty() {
        parts.push(t.codec.clone());
    }

    if parts.is_empty() {
        // 什么信息都没有的行不能是空白 —— 用户会以为界面坏了
        return s.track_unknown.to_string();
    }
    parts.join(" · ")
}

/// 语言码 -> 用户认得的写法。
///
/// 只认 mpv 会自己写出来的那些（ISO 639-2/B 为主，`und` 由调用方滤掉）。
/// 认不出来的**原样返回**语言码而不是猜一个：语言名列表有 180 项，
/// 猜错的代��比代码本身更误导。
fn language_name(code: &str) -> &str {
    match code {
        "chi" | "zho" | "zh" => "中文",
        "eng" => "English",
        "jpn" => "日本語",
        "kor" => "한국어",
        "rus" => "Русский",
        "ger" | "deu" => "Deutsch",
        "fre" | "fra" => "Français",
        "spa" => "Español",
        "ita" => "Italiano",
        "por" => "Português",
        "ara" => "العربية",
        "hin" => "हिन्दी",
        "tha" => "ไทย",
        "vie" => "Tiếng Việt",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpv::MpvValue;

    fn v(pairs: &[(&str, MpvValue)]) -> MpvValue {
        MpvValue::List(
            pairs
                .iter()
                .map(|(k, v)| (Some(k.to_string()), v.clone()))
                .collect(),
        )
    }
    fn s(x: &str) -> MpvValue {
        MpvValue::Text(x.to_string())
    }
    fn i(x: i64) -> MpvValue {
        MpvValue::Int(x)
    }
    fn b(x: bool) -> MpvValue {
        MpvValue::Flag(x)
    }

    /// 一条音频轨，形状照实测的 28 字段里我们关心的那部分
    fn audio_node(id: i64, lang: &str, selected: bool) -> MpvValue {
        v(&[
            ("id", i(id)),
            ("type", s("audio")),
            ("codec", s("aac")),
            ("codec-desc", s("AAC (Advanced Audio Coding)")),
            ("selected", b(selected)),
            ("default", b(true)),
            ("external", b(false)),
            ("audio-channels", i(2)),
            ("demux-samplerate", i(48000)),
            (
                "metadata",
                v(&[("language", s(lang)), ("handler_name", s("SoundHandler"))]),
            ),
        ])
    }

    fn sub_node(id: i64, lang: &str, selected: bool, external: bool) -> MpvValue {
        v(&[
            ("id", i(id)),
            ("type", s("sub")),
            ("codec", s("subrip")),
            ("selected", b(selected)),
            ("default", b(false)),
            ("external", b(external)),
            ("metadata", v(&[("language", s(lang))])),
        ])
    }

    fn strings() -> lang::Strings {
        lang::Strings::new(lang::Lang::ZhCn)
    }

    /// 造一条音轨（`id` 可控，因为上面几条测试要靠它区分「同一条」）
    fn mk_audio(id: i64, lang_code: &str, selected: bool, default: bool) -> Track {
        Track {
            id,
            kind: TrackKind::Audio,
            codec: "aac".into(),
            codec_desc: String::new(),
            language: lang_code.into(),
            title: String::new(),
            selected,
            default,
            external: false,
            audio_channels: Some(2),
            sample_rate: Some(48000),
        }
    }

    #[test]
    fn 解析实测形状的轨道列表() {
        let root = MpvValue::List(vec![
            (None, audio_node(1, "eng", true)),
            (None, audio_node(2, "chi", false)),
            (None, sub_node(1, "chi", true, true)),
        ]);
        let set = TrackSet::from_node(&root);
        assert_eq!(set.audio.len(), 2);
        assert_eq!(set.sub.len(), 1);
        let a = &set.audio[0];
        assert_eq!(a.id, 1);
        assert_eq!(a.codec, "aac");
        assert_eq!(a.language, "eng");
        assert!(a.selected && a.default);
        assert_eq!(a.audio_channels, Some(2));
        assert_eq!(a.sample_rate, Some(48000));
        assert_eq!(set.current_audio().map(|t| t.id), Some(1));
        assert_eq!(set.current_sub().map(|t| t.id), Some(1));
    }

    #[test]
    fn 顺序被保留_默认轨不会被排到后面去() {
        let root = MpvValue::List(vec![
            (None, audio_node(7, "eng", false)),
            (None, audio_node(3, "chi", true)),
        ]);
        let set = TrackSet::from_node(&root);
        // 原顺序，不按 id 排序
        assert_eq!(set.audio[0].id, 7);
        assert_eq!(set.audio[1].id, 3);
    }

    #[test]
    fn und_语言不显示() {
        let root = MpvValue::List(vec![(None, audio_node(1, "und", true))]);
        let set = TrackSet::from_node(&root);
        assert_eq!(set.audio[0].language, "", "und 不该当成语言码留着");
    }

    #[test]
    fn 缺字段的轨道照样能用() {
        // 只有 id 和 type，其余全缺 —— mpv 对不同轨种给的字段不一样
        let root = MpvValue::List(vec![(None, v(&[("id", i(5)), ("type", s("sub"))]))]);
        let set = TrackSet::from_node(&root);
        assert_eq!(set.sub.len(), 1);
        let t = &set.sub[0];
        assert_eq!(t.codec, "");
        assert_eq!(t.language, "");
        assert!(!t.selected);
        assert_eq!(t.audio_channels, None);
    }

    #[test]
    fn 缺_id_的轨道被跳过() {
        // 没有 id 就没法用 sid/aid 指认它，留着只能显示不能切
        let root = MpvValue::List(vec![
            (None, v(&[("type", s("audio"))])),
            (None, audio_node(2, "eng", true)),
        ]);
        let set = TrackSet::from_node(&root);
        assert_eq!(set.audio.len(), 1);
        assert_eq!(set.audio[0].id, 2);
    }

    #[test]
    fn 不认识的轨道类型被跳过但不牵连别的() {
        let root = MpvValue::List(vec![
            (None, v(&[("id", i(1)), ("type", s("attachment"))])),
            (None, audio_node(2, "eng", true)),
        ]);
        let set = TrackSet::from_node(&root);
        assert_eq!(set.audio.len(), 1, "封面那条不该出现，但音频那条要留着");
    }

    #[test]
    fn 非对象的列表项被跳过() {
        let root = MpvValue::List(vec![(None, s("乱码")), (None, audio_node(1, "eng", true))]);
        let set = TrackSet::from_node(&root);
        assert_eq!(set.audio.len(), 1);
    }

    #[test]
    fn 空树给出空的轨道集() {
        let set = TrackSet::from_node(&MpvValue::List(vec![]));
        assert!(set.audio.is_empty() && set.sub.is_empty());
        // 不是列表时也要安静返回，不能 panic
        let set = TrackSet::from_node(&s("不是树"));
        assert!(set.audio.is_empty());
    }

    #[test]
    fn 循环切轨两端都绕回() {
        // 往下一格
        assert_eq!(step_index(3, 0, 1), Some(1));
        assert_eq!(step_index(3, 1, 1), Some(2));
        assert_eq!(step_index(3, 2, 1), Some(0), "末尾绕回开头");
        // 往上一格
        assert_eq!(step_index(3, 2, -1), Some(1));
        assert_eq!(step_index(3, 1, -1), Some(0));
        // 这条是 `%` 会算错的地方：`-1 % 3 == -1`，拿去索引就 panic
        assert_eq!(step_index(3, 0, -1), Some(2), "开头绕回末尾");
        assert_eq!(step_index(2, 0, -1), Some(1));
        // 步长大于长度时也仍然落在范围内
        assert_eq!(step_index(3, 0, 5), Some(2));
        assert_eq!(step_index(3, 0, -7), Some(2));
        // 只有一条时走哪边都回到自己
        assert_eq!(step_index(1, 0, 1), Some(0));
        assert_eq!(step_index(1, 0, -1), Some(0));
    }

    #[test]
    fn 切换越界输入给不出下标() {
        assert_eq!(step_index(0, 0, 1), None, "没有轨就没有下标");
        assert_eq!(step_index(3, 3, 1), None, "pos 越界");
        assert_eq!(step_index(3, 99, -1), None);
    }

    #[test]
    fn 菜单行数随轨道数增长没有上限() {
        // 之前给过一个 `MAX_TRACK_ROWS = 29` 的上限，用来给布局算高度。
        // 现在布局按**实际**行数算、高度不够时按 `panel_drawn_rows` 截断，
        // 所以上限没有了：40 条音轨就该一行不少地列出来。
        //
        // 这条测试守着「别又把上限加回来」—— 加回来的话第 30 条轨就
        // 点不到了（点得到但画不出来，用户看到的是「点了没反应」）。
        let st = strings();
        let mut set = TrackSet::default();
        for n in 0..40 {
            set.audio.push(Track {
                id: n,
                kind: TrackKind::Audio,
                codec: "aac".into(),
                codec_desc: String::new(),
                language: String::new(),
                title: String::new(),
                selected: false,
                default: false,
                external: false,
                audio_channels: Some(2),
                sample_rate: Some(48000),
            });
        }
        let rows = build_rows(&set, &st);
        // 1 个分组标题 + 40 条轨 + 1 行底部操作提示
        assert_eq!(
            rows.len(),
            42,
            "40 条音轨 + 标题 + 提示行，不该有任何截断（实得 {} 行）",
            rows.len()
        );
        assert_eq!(
            rows.iter().filter(|r| r.selectable()).count(),
            40,
            "40 条音轨都得能被光标走到"
        );
    }

    #[test]
    fn 没有轨道时给一行说明而不是空菜单() {
        let s = strings();
        let rows = build_rows(&TrackSet::default(), &s);
        assert_eq!(rows.len(), 1, "空菜单的话按 T 会完全没有反应");
        assert!(
            matches!(rows[0], TrackRow::Note(_)),
            "这一行应当是说明行，实得 {:?}",
            rows[0]
        );
        assert!(!rows[0].selectable(), "说明行不能被光标选中");
    }

    #[test]
    fn 有轨道时给的是操作提示而不是没有轨道() {
        let st = strings();
        let root = MpvValue::List(vec![(None, audio_node(1, "eng", true))]);
        let set = TrackSet::from_node(&root);
        let rows = build_rows(&set, &st);

        // 「没有轨道」那行不该出现
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, TrackRow::Note(t) if *t == st.track_none)),
            "有轨道时不该显示「没有轨道」：{rows:?}"
        );
        // 但底部那行操作提示要在 —— 菜单开着的时候 ↑↓ 不再调音量，
        // 不说一声用户会以为程序坏了
        let notes: Vec<&&str> = rows
            .iter()
            .filter_map(|r| match r {
                TrackRow::Note(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(notes.len(), 1, "只该有一行提示：{rows:?}");
        assert_eq!(*notes[0], st.track_hint);
        // 提示行在最后，且不可选（光标不该停在它上面）
        assert!(matches!(rows.last(), Some(TrackRow::Note(_))));
        assert!(!rows.last().expect("非空").selectable());
    }

    #[test]
    fn 菜单里视频轨不出现() {
        let st = strings();
        let root = MpvValue::List(vec![
            (None, v(&[("id", i(1)), ("type", s("video"))])),
            (None, audio_node(2, "eng", true)),
        ]);
        let set = TrackSet::from_node(&root);
        let rows = build_rows(&set, &st);

        // 只有一个分组标题，且是音频那个
        let headings: Vec<&&str> = rows
            .iter()
            .filter_map(|r| match r {
                TrackRow::Heading(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(headings.len(), 1, "只该有音频一个分组：{rows:?}");
        assert_eq!(*headings[0], st.track_audio);

        // 可选行数 == 音轨数 + 字幕数（这里 1 + 0）。多出来的就是视频轨混进来了。
        let selectable = rows.iter().filter(|r| r.selectable()).count();
        assert_eq!(
            selectable,
            set.audio.len() + set.sub.len(),
            "可选行数必须正好等于音轨+字幕数，多出来的说明视频轨混进来了：{rows:?}"
        );
    }

    #[test]
    fn 重建之后光标回到同一条轨而不是同一个位置() {
        // 这条盯的是 `replace_tracks` 里「先算 keep 再赋值」的顺序。
        //
        // 场景：2 条音轨，光标在第 2 条（id=7）。切轨之后 mpv 把 id=7 标成
        // 选中、`selected` 变化，如果行序也跟着动，光标必须跟着**id** 走。
        // 如果 `row_id` 拿「新 tracks + 旧 rows」去算，组内下标会翻译成
        // 另一个 id，光标就静默跳到别的轨上 —— 而且不崩、不报错。
        let st = strings();
        let mut set = TrackSet::default();
        set.audio.push(mk_audio(1, "eng", false, true));
        set.audio.push(mk_audio(7, "chi", true, true));
        let rows = build_rows(&set, &st);
        // 行 0 = 音频标题，行 1 = id 1，行 2 = id 7
        assert_eq!(
            row_id(&rows, &set, &st, 2),
            Some(RowId::Track(TrackKind::Audio, 7))
        );

        // 切轨：id 7 不再是 selected，id 1 变成 selected
        let mut set2 = set.clone();
        set2.audio[0].selected = true;
        set2.audio[1].selected = false;
        let keep = row_id(&rows, &set, &st, 2);
        let rows2 = build_rows(&set2, &st);
        let after = cursor_for(&rows2, &set2, &st, keep);

        // 行数没变、位置也没变 —— 这条断言按现状是通过的。要抓住的是
        // 「keep 算错」的情况：把 keep 换成用新 set 算就会得到
        // RowId::Track(Audio, 1)，光标就会落到行 1。
        assert_eq!(
            row_id(&rows2, &set2, &st, after),
            Some(RowId::Track(TrackKind::Audio, 7)),
            "光标必须还指着 id 7 那条"
        );
    }

    #[test]
    fn 用新集合去查旧行会翻译出错的_id() {
        // 反面测试：证明上一条守的那个顺序真的有意义。
        //
        // 旧行里 `Item { index: 1 }` 是 id 7。用**新**集合（顺序变了）
        // 去翻译同一个下标，得到的是新集合的第 2 条 —— 完全另一条轨。
        let st = strings();
        let mut set = TrackSet::default();
        set.audio.push(mk_audio(1, "eng", false, true));
        set.audio.push(mk_audio(7, "chi", true, true));
        let rows = build_rows(&set, &st);

        // 新集合：第 2 条换成了 id 9
        let mut set2 = set.clone();
        set2.audio[1].id = 9;

        assert_eq!(
            row_id(&rows, &set, &st, 2),
            Some(RowId::Track(TrackKind::Audio, 7)),
            "配套的集合给出正确 id"
        );
        assert_eq!(
            row_id(&rows, &set2, &st, 2),
            Some(RowId::Track(TrackKind::Audio, 9)),
            "错配的集合给出另一个 id —— 所以 row_id 必须收到配套的那份"
        );
    }

    #[test]
    fn 分组归属靠上最近的标题而不是组内下标() {
        let st = strings();
        let root = MpvValue::List(vec![
            (None, audio_node(1, "eng", true)),
            (None, audio_node(2, "chi", false)),
            (None, sub_node(1, "chi", true, false)),
            (None, sub_node(2, "eng", false, false)),
        ]);
        let set = TrackSet::from_node(&root);
        let rows = build_rows(&set, &st);
        // 行：0 音频标题 / 1 音1 / 2 音2 / 3 字幕标题 / 4 关闭 / 5 字幕1 / 6 字幕2 / 7 提示
        assert_eq!(group_of_row(&rows, &st, 1), Some(TrackKind::Audio));
        assert_eq!(group_of_row(&rows, &st, 2), Some(TrackKind::Audio));
        assert_eq!(group_of_row(&rows, &st, 5), Some(TrackKind::Sub));
        assert_eq!(group_of_row(&rows, &st, 6), Some(TrackKind::Sub));
        // 分组标题自己不属于任何分组（它自己就是标题）
        assert_eq!(group_of_row(&rows, &st, 0), None);
        assert_eq!(group_of_row(&rows, &st, 3), None);
        // 越界
        assert_eq!(group_of_row(&rows, &st, 999), None);
    }

    #[test]
    fn 字幕菜单带一个关闭项() {
        let root = MpvValue::List(vec![(None, sub_node(1, "chi", true, true))]);
        let set = TrackSet::from_node(&root);
        let rows = build_rows(&set, &strings());
        assert!(
            rows.iter()
                .any(|r| matches!(r, TrackRow::OffSubtitle { active } if !*active)),
            "当前有字幕在放，「关闭」就不该是选中态"
        );
        // 选不中的行也必须 selectable —— 否则光标跳不过去
        assert!(rows.iter().filter(|r| r.selectable()).count() >= 2);
    }

    #[test]
    fn 描述里语言比编码更显眼() {
        let s = strings();
        let t = Track {
            id: 1,
            kind: TrackKind::Audio,
            codec: "aac".into(),
            codec_desc: String::new(),
            language: "chi".into(),
            title: String::new(),
            selected: false,
            default: false,
            external: false,
            audio_channels: Some(2),
            sample_rate: Some(48000),
        };
        let d = describe(&t, &s);
        assert_eq!(d, "中文 · 2 声道 · 48 kHz · aac", "{d}");
    }

    /// 相邻两段都是数字时必须有分隔符。
    ///
    /// 这条是实测截图逼出来的：早一版拼出 `1 44 声道aac`，那个 `44`
    /// 是采样率却紧跟在声道数后面、又没有单位，读起来像「44 声道」。
    #[test]
    fn 采样率必须带单位且和声道数分开() {
        let s = strings();
        let mk = |ch: i64, sr: i64| Track {
            id: 1,
            kind: TrackKind::Audio,
            codec: "aac".into(),
            codec_desc: String::new(),
            language: String::new(),
            title: String::new(),
            selected: false,
            default: false,
            external: false,
            audio_channels: Some(ch),
            sample_rate: Some(sr),
        };
        // 实测素材 loop60s.mp4 的形状：单声道 44100
        assert_eq!(describe(&mk(1, 44100), &s), "1 声道 · 44.1 kHz · aac");
        // 48000 不带小数
        assert_eq!(describe(&mk(2, 48000), &s), "2 声道 · 48 kHz · aac");
        // 一位小数的情况
        assert_eq!(describe(&mk(6, 96000), &s), "6 声道 · 96 kHz · aac");
        // 没有采样率时不该留下多余的分隔符
        assert_eq!(describe(&mk(2, 0), &s), "2 声道 · aac");
        // 没有声道数时也不该留下多余的分隔符
        assert_eq!(describe(&mk(0, 48000), &s), "aac");
        // 两者都没有
        assert_eq!(describe(&mk(0, 0), &s), "aac");
    }

    #[test]
    fn 描述里不会开头或结尾挂分隔符() {
        let s = strings();
        for (ch, sr) in [(0i64, 0i64), (2, 0), (0, 48000), (2, 48000), (-1, -1)] {
            let t = Track {
                id: 1,
                kind: TrackKind::Audio,
                codec: "aac".into(),
                codec_desc: String::new(),
                language: String::new(),
                title: String::new(),
                selected: false,
                default: false,
                external: false,
                audio_channels: Some(ch),
                sample_rate: Some(sr),
            };
            let d = describe(&t, &s);
            assert!(!d.starts_with(" · "), "开头挂了分隔符：{d:?}");
            assert!(!d.ends_with(" · "), "结尾挂了分隔符：{d:?}");
            assert!(!d.contains(" ·  · "), "出现了连续分隔符：{d:?}");
            assert!(!d.contains("  "), "出现了连续空格：{d:?}");
        }
    }

    #[test]
    fn 外挂字幕那一行带外挂标记() {
        let s = strings();
        let t = Track {
            id: 1,
            kind: TrackKind::Sub,
            codec: "subrip".into(),
            codec_desc: String::new(),
            language: "chi".into(),
            title: String::new(),
            selected: false,
            default: false,
            external: true,
            audio_channels: None,
            sample_rate: None,
        };
        let d = describe(&t, &s);
        assert!(d.contains("外挂"), "{d}");
        assert!(d.contains("subrip"), "{d}");
        assert!(d.starts_with("中文"), "{d}");
    }

    #[test]
    fn 什么都不剩时也要有一行字() {
        let s = strings();
        let t = Track {
            id: 1,
            kind: TrackKind::Sub,
            codec: String::new(),
            codec_desc: String::new(),
            language: String::new(),
            title: String::new(),
            selected: false,
            default: false,
            external: false,
            audio_channels: None,
            sample_rate: None,
        };
        let d = describe(&t, &s);
        assert!(!d.is_empty(), "空的轨道不能显示成空白行");
    }
}
