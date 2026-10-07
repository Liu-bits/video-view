//! 播放列表。
//!
//! ## 这一版解决的具体问题
//!
//! 拖放只取第一个文件，于是「把 `movie.mp4` 和 `movie.zh.srt` 一起拖进来」
//! 这种最常见的用法里，**字幕被静默丢掉** —— 丢进去的是第一个文件，
//! 排序取决于资源管理器怎么给，而 `.srt` 排在 `.mp4` 前面还是后面都不对：
//! 排在前面就变成「拖了个字幕进来，可当前没有视频」；
//! 排在后面就变成「拖了个视频进来，那个字幕不见了」。
//! 两种都**不报错**，用户只会觉得「这个软件不支持外挂字幕」。
//!
//! 所以这里做两件事：
//!
//! 1. **视频全部进列表**，按拖放顺序，第一个开始播，播完自动接下一个。
//! 2. **同名的字幕挂到对应视频上**（`movie.mp4` + `movie.zh.srt` ->
//!    `movie.mp4` 带着那条字幕）。匹配规则是「字幕的文件名去掉最后一个
//!    点之前的那段」是视频名的**前缀**：`movie` 能匹配 `movie.zh`、
//!    `movie.en`，但匹配不上 `other`。
//!
//! 拖进来只有字幕、当前又没有视频时，仍然按「给当前视频外挂」处理 ——
//! 那本来就是 0.5.0 定的语义，不改。

use std::path::{Path, PathBuf};

/// 播放列表面板的一行。
///
/// 与 `track::TrackRow` 分开而不是复用：轨道行有「分组标题」「说明」
/// 「不可选」这些形态，播放列表的每一行都是可选的一首，
/// 硬套过去只会让绘制分支多出一堆永远走不到的判断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistRow {
    /// 文件名（面板上显示的那一串）
    pub title: String,
    /// 是不是正在放的那一条
    pub current: bool,
    /// 有没有随这条视频一起拖进来的外挂字幕。画一个 ▸ 标记。
    pub has_sidecar: bool,
}

/// 一条播放列表项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub path: PathBuf,
    /// 与这条视频同名的外挂字幕（拖放时一起进来的那个）。
    ///
    /// `None` 的两种含义要分清：**没有随这条视频一起拖进来**，以及
    /// **文件确实没有同名字幕**。界面上都显示成「无外挂字幕」，所以
    /// 这里不需要区分 —— 真要区分的话得存一个 `Option<PathBuf>` 加
    /// 一个「已查过没有」的标记，而那个信息用户看不到、也不用得到。
    pub sidecar: Option<PathBuf>,
    /// 界面显示的名字。
    ///
    /// 取**文件名**（含扩展名）而不是全路径：路径在控制栏上放不下，
    /// 而文件名在同一个列表里就足以区分。完整路径在 tooltip 与
    /// 「复制诊断报告」里都有。
    pub title: String,
}

/// 播放列表。
///
/// `current` 是**下标**而不是 `Option`：`items` 恒非空（构造时保证），
/// 没有媒体这件事由 `App::state.has_file` 表达。分成两处表达「没在播」
/// 只会带来「下标指向一个不存在的文件」这种中间状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Playlist {
    items: Vec<Item>,
    current: usize,
}

/// 拖放一批文件之后的结果：先播哪一个，以及要外挂哪几条字幕。
///
/// `sidecars` 只包含**跟着第一个视频一起拖进来**的那些 —— 别的视频的
/// 字幕要等切到那个视频时再挂。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub index: usize,
    pub sidecars: Vec<PathBuf>,
}

impl Playlist {
    /// 用一批路径重建列表。**返回 `None` 表示一个视频都没有。**
    ///
    /// 路径按调用方给的顺序保留：用户从资源管理器里多选拖进来，
    /// 那个顺序就是他想听的顺序，重排反而是自作主张。
    pub fn from_paths(paths: &[PathBuf]) -> (Option<Self>, Option<Loaded>) {
        let mut videos: Vec<PathBuf> = Vec::new();
        let mut subs: Vec<PathBuf> = Vec::new();
        for p in paths {
            if is_subtitle_path(p) {
                subs.push(p.clone());
            } else {
                videos.push(p.clone());
            }
        }
        if videos.is_empty() {
            return (None, None);
        }
        let items: Vec<Item> = videos
            .iter()
            .map(|v| Item {
                title: file_title(v),
                sidecar: match_subs(v, &subs),
                path: v.clone(),
            })
            .collect();
        let loaded = Loaded {
            index: 0,
            sidecars: items[0].sidecar.iter().cloned().collect(),
        };
        (Some(Self { items, current: 0 }), Some(loaded))
    }

    /// 单个文件。列���里只有它自己。
    pub fn single(path: PathBuf) -> Self {
        let title = file_title(&path);
        Self {
            items: vec![Item {
                path,
                sidecar: None,
                title,
            }],
            current: 0,
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn current(&self) -> usize {
        self.current
    }

    pub fn current_item(&self) -> &Item {
        // `current` 恒在界内：`from_paths` 构造时必然非空，
        // `select` / `step` 都会夹住。`[]` 只是让编译器闭嘴。
        &self.items[self.current.min(self.items.len() - 1)]
    }

    /// 切到第 `i` 条。越界返回 `false` 且不动 `current`。
    pub fn select(&mut self, i: usize) -> bool {
        if i >= self.items.len() {
            return false;
        }
        self.current = i;
        true
    }

    /// 下一条。
    ///
    /// **到末尾不绕回**，返回 `None` 让调用方去处理「播完了」。
    ///
    /// 不绕回是有意的：播放列表是「一组要按顺序看完的东西」，
    /// 循环播放要用户显式开（mpv 的 `--loop-playlist`），默认循环会让人
    /// 错过「已经放到最后一段」这个信号。
    ///
    /// 不叫 `next` 而叫 `advance`：`Playlist` 不是迭代器，而 `next` 这个名字
    /// 在 Rust 里几乎专属于 `Iterator` —— clippy 会直接警告
    /// (`should_implement_trait`)，读代码的人也会先愣一下。
    pub fn advance(&mut self) -> Option<&Item> {
        let n = self.current + 1;
        if n >= self.items.len() {
            return None;
        }
        self.current = n;
        Some(&self.items[n])
    }

    /// 上一条。到开头不动，返回 `None`。
    pub fn prev(&mut self) -> Option<&Item> {
        if self.current == 0 {
            return None;
        }
        self.current -= 1;
        Some(&self.items[self.current])
    }

    /// 播完之后调：把 `current` 标到「已经没有下一条」。
    ///
    /// 单独一个方法而不是让 `next` 去动 `current`，是因为调用方
    /// 要先看一眼 `next()` 的结果再决定要不要加载下一个文件。
    /// 这个方法只在**用户手动打开某个文件**之后用来同步下标，
    /// 那条文件不一定在列表里（`Ctrl+O` 打开的文件就不在）。
    pub fn sync_to_path(&mut self, path: &Path) {
        if let Some(i) = self.items.iter().position(|it| it.path == path) {
            self.current = i;
        }
    }
    /// 确保 `path` 在列表里。不在就变成一个只含它的单条列表。
    ///
    /// ## 为什么需要这一步
    ///
    /// 列表不是只有「拖一批文件进来」才会被填。`Ctrl+O` 打开的文件、快捷键
    /// `N`/`B` 切的文件、命令行给的第一个文件 —— 这些路径都**不经过**
    /// `from_paths`，于是列表要么还是初始那个空壳，要么压根不含这个文件。
    ///
    /// 后果是面板空着：这一版之前 `P` 面板一直是空的，因为 `App::new` 里
    /// 塞的是 `Playlist::single(PathBuf::new())` —— 一个**路径为空**的假列表。
    /// 而 `current_item()` 返回的是它，于是 `rows()` 是空的、面板区高度为 0，
    /// 用户看到的是「按了 `P` 什么也没发生」。
    ///
    /// 这里做成「不在就变成单条列表」而不是「强行 append」：`Ctrl+O` 打开的
    /// 文件如果被 append 进去，用户会看到列表莫名其妙多了一条，而那条从来
    /// 不是他加进来的。
    pub fn ensure_contains(&mut self, path: &Path) {
        if self.items.iter().any(|it| it.path == path) {
            self.sync_to_path(path);
        } else {
            *self = Self::single(path.to_path_buf());
        }
    }
}
/// 面板行数据。由列表编出来，`playlist` 或 `current` 变了就重编。
pub fn rows(pl: &Playlist) -> Vec<PlaylistRow> {
    pl.items()
        .iter()
        .enumerate()
        .map(|(i, it)| PlaylistRow {
            title: it.title.clone(),
            current: i == pl.current(),
            has_sidecar: it.sidecar.is_some(),
        })
        .collect()
}

/// 找出与 `video` 同名的外挂字幕。
///
/// 规则：字幕的文件名（去掉最后一个扩展名）以视频的文件名为**前缀**。
/// `movie.mp4` 匹配 `movie.zh.srt`、`movie.en.srt`、`movie.srt`，
/// 不匹配 `other.srt` 与 `movie2.srt`。
///
/// ## 为什么用「前缀」而不是「完全相等」
///
/// 完全相等只能匹配 `movie.mp4` + `movie.srt`，而带语言后缀的那种
/// （`movie.zh.srt` / `movie.en.srt` / `movie.chs.ass`）才是现实里
/// 成套摆放的主要形式 —— 一个番剧的目录里通常有好几条同名字幕。
///
/// 前缀会带来一个歧义：`movie.mp4` 同时匹配 `movie.srt` 与
/// `movie2.srt`（因为 `movie2` 以 `movie` 开头）。所以这里要求
/// 后缀的第一段是**非空且不含分隔符**的一段，也就是
/// `movie.zh` 匹配、`movie2` 不匹配。判据是字幕名去掉视频名之后
/// 剩下的那一段必须以 `.` 开头。
fn match_subs(video: &Path, subs: &[PathBuf]) -> Option<PathBuf> {
    let vname = video.file_name()?.to_str()?;
    // 多个同名字幕时取**第一个**，不排序 —— 拖放的顺序是用户的意图。
    subs.iter().find(|s| belongs_to(s, vname)).cloned()
}

/// `sub` 是不是 `video_name` 那一组的外挂字幕。
///
/// `video_name` 是**视频文件名含扩展名**（`movie.mp4`）。
fn belongs_to(sub: &Path, video_name: &str) -> bool {
    let Some(video_stem) = strip_ext(video_name) else {
        return false;
    };
    let Some(sub_stem) = sub.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    // `movie` 与 `movie.zh` 都算 `movie.mp4` 的字幕
    if sub_stem.eq_ignore_ascii_case(video_stem) {
        return true;
    }
    // 带语言后缀的那种。**必须连点一起比**，否则 `movie2` 会匹配上 `movie`
    // —— 那是个真会发生的错：目录里常有 `movie.mp4` 与 `movie2.mp4`。
    let prefix_len = video_stem.len();
    sub_stem.len() > prefix_len
        && sub_stem[..prefix_len].eq_ignore_ascii_case(video_stem)
        && sub_stem.as_bytes()[prefix_len] == b'.'
}

/// 去掉**最后一个**扩展名。
///
/// 不能用 `Path::file_stem` —— 它对 `movie.zh.srt` 返回 `movie.zh`，
/// 而这里要的是 `movie`（字幕那一组的名字是与**视频名**比，视频没有
/// 那么多段）。
fn strip_ext(name: &str) -> Option<&str> {
    let dot = name.rfind('.')?;
    if dot == 0 {
        return None;
    }
    Some(&name[..dot])
}

/// 这个扩展名是不是字幕文件。
///
/// 只认最常见的四种（`.srt` / `.ass` / `.ssa` / `.vtt`），其余一律当媒体。
/// 字幕格式还有很多（`.sub` / `.idx` / `.smi` / `.lrc`），但
/// **判错的后果不对称**：把字幕当视频打开会失败并报错（用户看得见），
/// 把视频当字幕加载则是一条不存在的外挂轨（用户看不见）。
pub fn is_subtitle_path(p: &Path) -> bool {
    let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "srt" | "ass" | "ssa" | "vtt"
    )
}

/// 界面上显示的文件名。
///
/// **不去扩展名**：`movie.mp4` 与 `movie.mkv` 在同一个列表里时，
/// 去掉扩展名就分不出是两个文件了，宁可多占几个字符。
pub fn file_title(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn 拖一个视频就是一个单条列表() {
        let (pl, loaded) = Playlist::from_paths(&[p(r"D:\v\a.mp4")]);
        let pl = pl.expect("应当建出列表");
        assert_eq!(pl.len(), 1);
        assert_eq!(pl.current(), 0);
        assert_eq!(pl.current_item().title, "a.mp4");
        let l = loaded.expect("应当有加载信息");
        assert_eq!(l.index, 0);
        assert!(l.sidecars.is_empty());
    }

    #[test]
    fn 拖多个视频按拖放顺序排() {
        let (pl, loaded) =
            Playlist::from_paths(&[p(r"D:\v\c.mp4"), p(r"D:\v\a.mkv"), p(r"D:\v\b.webm")]);
        let pl = pl.expect("列表");
        // 不重排：用户选中的顺序就是他想听的顺序
        assert_eq!(pl.items()[0].title, "c.mp4");
        assert_eq!(pl.items()[1].title, "a.mkv");
        assert_eq!(pl.items()[2].title, "b.webm");
        assert_eq!(loaded.expect("加载信息").index, 0);
    }

    #[test]
    fn 一个视频都没有时返回空() {
        let (pl, loaded) = Playlist::from_paths(&[p(r"D:\v\a.srt"), p(r"D:\v\b.ass")]);
        assert!(pl.is_none(), "只有字幕时不该建列表");
        assert!(loaded.is_none());
    }

    #[test]
    fn 同名字幕挂到对应视频上() {
        let (pl, loaded) = Playlist::from_paths(&[p(r"D:\v\movie.mp4"), p(r"D:\v\movie.zh.srt")]);
        let pl = pl.expect("列表");
        assert_eq!(
            pl.items()[0].sidecar,
            Some(p(r"D:\v\movie.zh.srt")),
            "movie.zh.srt 应当挂到 movie.mp4 上"
        );
        // 而且要跟着第一个视频一起挂上去
        assert_eq!(
            loaded.expect("加载信息").sidecars,
            vec![p(r"D:\v\movie.zh.srt")]
        );
    }

    #[test]
    fn 带语言后缀的成套字幕各归各位() {
        let (pl, _) = Playlist::from_paths(&[
            p(r"D:\v\movie.mp4"),
            p(r"D:\v\movie.zh.srt"),
            p(r"D:\v\movie.en.ass"),
        ]);
        let pl = pl.expect("列表");
        // 多个同名字幕取拖放顺序里的第一个
        assert_eq!(pl.items()[0].sidecar, Some(p(r"D:\v\movie.zh.srt")));
    }

    #[test]
    fn 前缀相同但不是同名的字幕不挂() {
        // `movie2.srt` 的 stem 是 `movie2`，与 `movie.mp4` 不相等
        let (pl, _) = Playlist::from_paths(&[p(r"D:\v\movie.mp4"), p(r"D:\v\movie2.srt")]);
        let pl = pl.expect("列表");
        assert_eq!(pl.items()[0].sidecar, None, "movie2.srt 不该挂到 movie.mp4");
    }

    #[test]
    fn 完全同名的字幕挂得上() {
        let (pl, _) = Playlist::from_paths(&[p(r"D:\v\movie.mp4"), p(r"D:\v\movie.srt")]);
        let pl = pl.expect("列表");
        assert_eq!(pl.items()[0].sidecar, Some(p(r"D:\v\movie.srt")));
    }

    #[test]
    fn 字幕在视频之前出现也照样挂上() {
        // 资源管理器给的顺序不保证视频在前
        let (pl, loaded) = Playlist::from_paths(&[p(r"D:\v\movie.zh.srt"), p(r"D:\v\movie.mp4")]);
        let pl = pl.expect("列表");
        assert_eq!(pl.len(), 1, "只有一条视频，列表就只有一条");
        assert_eq!(pl.items()[0].sidecar, Some(p(r"D:\v\movie.zh.srt")));
        assert_eq!(loaded.expect("加载").sidecars.len(), 1);
    }

    #[test]
    fn 字幕不会变成列表项() {
        let (pl, _) = Playlist::from_paths(&[p(r"D:\v\a.mp4"), p(r"D:\v\a.srt"), p(r"D:\v\b.mp4")]);
        let pl = pl.expect("列表");
        assert_eq!(pl.len(), 2, "字幕不进列表");
        assert!(pl.items().iter().all(|i| !is_subtitle_path(&i.path)));
    }

    #[test]
    fn 字幕只跟第一个视频走() {
        let (pl, loaded) =
            Playlist::from_paths(&[p(r"D:\v\a.mp4"), p(r"D:\v\b.mp4"), p(r"D:\v\b.zh.srt")]);
        let pl = pl.expect("列表");
        let l = loaded.expect("加载");
        // 现在播 a，所以 b 的字幕**不**该被挂上
        assert!(l.sidecars.is_empty());
        // 但它记在 b 那一条上，切过去的时候挂
        assert_eq!(pl.items()[1].sidecar, Some(p(r"D:\v\b.zh.srt")));
    }

    #[test]
    fn 大小写不同也算同一个名字() {
        let (pl, _) = Playlist::from_paths(&[p(r"D:\v\Movie.mp4"), p(r"D:\v\movie.zh.srt")]);
        assert_eq!(
            pl.expect("列表").items()[0].sidecar,
            Some(p(r"D:\v\movie.zh.srt"))
        );
    }

    #[test]
    fn 扩展名大写也认得出是字幕() {
        let (pl, _) = Playlist::from_paths(&[p(r"D:\v\a.mp4"), p(r"D:\v\a.SRT")]);
        assert_eq!(pl.expect("列表").items()[0].sidecar, Some(p(r"D:\v\a.SRT")));
    }

    #[test]
    fn 下一个到末尾就停() {
        let mut pl = Playlist::from_paths(&[p("a.mp4"), p("b.mp4"), p("c.mp4")])
            .0
            .expect("列表");
        assert_eq!(pl.current(), 0);
        assert_eq!(
            pl.advance().map(|i| i.title.clone()).as_deref(),
            Some("b.mp4")
        );
        assert_eq!(
            pl.advance().map(|i| i.title.clone()).as_deref(),
            Some("c.mp4")
        );
        assert_eq!(pl.advance(), None, "末尾之后没有下一个");
        assert_eq!(pl.current(), 2, "停在最后一条，不动");
        assert_eq!(pl.prev().map(|i| i.title.clone()).as_deref(), Some("b.mp4"));
    }

    #[test]
    fn 上一个到开头就停() {
        let mut pl = Playlist::from_paths(&[p("a.mp4"), p("b.mp4")])
            .0
            .expect("列表");
        assert_eq!(pl.prev(), None);
        assert_eq!(pl.current(), 0);
    }

    #[test]
    fn 选越界不改变下标() {
        let mut pl = Playlist::from_paths(&[p("a.mp4"), p("b.mp4")])
            .0
            .expect("列表");
        assert!(!pl.select(9), "越界应当返回 false");
        assert_eq!(pl.current(), 0);
        assert!(pl.select(1));
        assert_eq!(pl.current(), 1);
    }

    #[test]
    fn 单条列表上下都是空操作() {
        let mut pl = Playlist::single(p("only.mp4"));
        assert_eq!(pl.advance(), None);
        assert_eq!(pl.prev(), None);
        assert_eq!(pl.len(), 1);
        assert_eq!(pl.current_item().title, "only.mp4");
    }

    #[test]
    fn 不在列表里的文件会变成单条列表() {
        // 这是 `Ctrl+O` 打开文件的路径：不经过 `from_paths`，
        // 所以必须由 `ensure_contains` 补上，否则面板空着
        let mut pl = Playlist::single(p("a.mp4"));
        pl.ensure_contains(&p(r"d:\other\z.mkv"));
        assert_eq!(pl.len(), 1);
        assert_eq!(pl.current_item().title, "z.mkv");
    }

    #[test]
    fn 已经在列表里的文件只同步下标() {
        let mut pl = Playlist::from_paths(&[p("a.mp4"), p("b.mp4"), p("c.mp4")])
            .0
            .expect("列表");
        pl.ensure_contains(&p("c.mp4"));
        assert_eq!(pl.len(), 3, "已经在列表里就不该重建成单条");
        assert_eq!(pl.current(), 2);
    }

    #[test]
    fn 面板行每一项都在_且当前项有标记() {
        let (mut pl, _) = Playlist::from_paths(&[p("a.mp4"), p("b.mp4"), p("c.mp4")]);
        let pl = pl.as_mut().expect("列表");
        let r = rows(pl);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].title, "a.mp4");
        assert!(r[0].current, "第一条正在放");
        assert!(!r[1].current);
        // 往前走一格，标记要挪到第二条
        pl.advance();
        let r = rows(pl);
        assert!(!r[0].current);
        assert!(r[1].current);
        // 再走一格到第三条
        pl.advance();
        let r = rows(pl);
        assert!(r[2].current);
        assert!(!r[1].current, "标记只能有一条");
    }

    #[test]
    fn 面板行标出有外挂字幕的条目() {
        let (pl, _) = Playlist::from_paths(&[p("a.mp4"), p("b.mp4"), p("b.zh.srt")]);
        let r = rows(pl.as_ref().expect("列表"));
        assert!(!r[0].has_sidecar, "a.mp4 没有同名字幕");
        assert!(r[1].has_sidecar, "b.mp4 有 b.zh.srt");
    }

    #[test]
    fn 面板永远不为空() {
        // `Playlist::single` 是初始值，所以 `rows()` 必须有一行。
        // 面板空着 = 面板区高度 0 = 用户看到「按了 P 什么也没发生」
        let pl = Playlist::single(p("only.mp4"));
        assert_eq!(rows(&pl).len(), 1);
    }

    #[test]
    fn 按路径同步下标() {
        let mut pl = Playlist::from_paths(&[p("a.mp4"), p("b.mp4"), p("c.mp4")])
            .0
            .expect("列表");
        pl.sync_to_path(&p("c.mp4"));
        assert_eq!(pl.current(), 2);
        // 不在列表里的路径不动下标（`Ctrl+O` 打开的文件就是这样）
        pl.sync_to_path(&p(r"d:\other\z.mp4"));
        assert_eq!(pl.current(), 2);
    }

    #[test]
    fn 文件名保留扩展名() {
        assert_eq!(file_title(&p(r"D:\v\movie.mp4")), "movie.mp4");
        assert_eq!(file_title(&p("noext")), "noext");
    }

    #[test]
    fn 字幕扩展名识别() {
        for e in ["srt", "SRT", "ass", "ssa", "vtt", "VTT"] {
            assert!(is_subtitle_path(&PathBuf::from(format!("a.{e}"))), "{e}");
        }
        for e in [
            "mp4", "mkv", "webm", "mov", "avi", "mp3", "flac", "sub", "idx",
        ] {
            assert!(!is_subtitle_path(&PathBuf::from(format!("a.{e}"))), "{e}");
        }
        assert!(!is_subtitle_path(&p("noextension")));
    }
}
