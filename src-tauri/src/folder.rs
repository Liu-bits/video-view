//! 「打开文件夹」：把一个目录里的视频收进播放列表。
//!
//! ## 为什么单独一个文件
//!
//! 排序规则是这个功能里**唯一有争议的部分**，而它必须和「哪些算视频」
//! 待在同一个模块里 —— 那份扩展名表同时被拖放分派、文件对话框过滤器、
//! 以及这里的扫描共用，改一处漏一处就会出现「拖进来能播、扫进来不能播」。
//!
//! ## 排序：按文件名自然序
//!
//! 「按文件名还是按修改时间」两种做法都会有人不满意，这是我在 0.6.2 的
//! 已知限制里写下的原话。这里选**文件名自然序**，理由是：
//!
//! * 「同一个文件夹里的第 01 集、第 02 集……」是最常见的诉求，自然序直接
//!   满足它；而按修改时间会给出「刚下载的那一集排最前」，对追剧文件夹
//!   恰好是最没用的顺序
//! * 自然序对「第 2 集」与「第 10 集」给出 2 < 10，而**字典序**给出
//!   10 < 2 —— 后者正是资源管理器默认行为的坑，用户会踩
//! * 修改时间排序**不稳定**：每扫一次顺序都可能不同（同一个时间戳的
//!   文件），而列表顺序变了用户会以为程序有 bug
//!
//! 想要按修改时间的人可以用拖放自己排 —— 那本来就是拖放的用途。
//!
//! ## 「视频」按扩展名判定，不问 mpv
//!
//! 候选文件先按扩展名过滤，**只有这些**才交给 mpv 去开。原因：
//!
//! * 一个文件夹里可能有 `.txt` / `.nfo` / 封面图 / 字幕文件。全交给 mpv
//!   的话，第一个不认识的就会弹一个「播放核心拒绝了一条命令」
//! * mpv 能开的格式远多于「用户认为的视频」（`.mka` / `.ts` / `.m4v` …），
//!   但一份**白名单**加上「未知的一律跳过」比逐个试错更可预测
//! * 白名单与 `menu`/`track` 那边共用同一份（`VIDEO_EXTS`），所以
//!   「菜单里能打开的扩展名」与「这里扫得到的扩展名」永远一致

use std::path::{Path, PathBuf};

/// 算作视频的扩展名（小写，不含点）。
///
/// 与 `menu`/`lang` 那边给文件对话框的过滤器**保持同一份** —— 两份表
/// 必然会漂移，而漂移的表现是「文件对话框里能选、扫文件夹却扫不到」。
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "mpg", "mpeg", "m2ts", "vob",
    "3gp", "rmvb", "ogv", "asf", "3g2", "divx", "mts", "m2v", "f4v", "mp4v",
];

/// 这个扩展名算视频吗。
pub fn is_video_ext(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) => {
            let lower = e.to_ascii_lowercase();
            VIDEO_EXTS.iter().any(|v| *v == lower)
        }
        None => false,
    }
}

/// 自然序比较：把文件名里的**数字段**按数值比，其余按字符比。
///
/// 这是「`第 2 集` 排在 `第 10 集` 前面」的关键。纯字典序（也就是
/// `str::cmp`）会给出 `10 < 2`，因为 `'1' < '2'` —— 资源管理器默认就是
/// 这么排的，于是用户要先看到第 10 集才能看到第 2 集。
///
/// 实现是标准的「两路归并」：一路读数字串、一路读非数字串，数字段先去掉
/// 前导零，再按位数与字典序比（相当于按数值比，但不解析成整数），非数字段
/// 按 `char` 比。
///
/// ## 为什么不解析成整数
///
/// 名字里可能有很长的数字；直接比较去掉前导零后的位数与各位字符，既能给出
/// 数值顺序，也不会受 `u64` 上限影响。数值相等时用原始数字串打破平局，
/// 避免文件扫描顺序影响最终排序。
///
/// ## 大小写
///
/// 非数字段用 `char` 的码值比，所以 `B` < `a`。这是有意的：**稳定且
/// 跨平台一致**。用 `to_lowercase()` 比会让 `Ä` 之类的字符在不同语言
/// 环境下给出不同顺序，而文件名的排序只有「稳定」这一条是硬要求。
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let na = take_digits(&mut ai);
                    let nb = take_digits(&mut bi);
                    match cmp_digits(&na, &nb) {
                        Ordering::Equal => {}
                        other => return other,
                    }
                } else {
                    ai.next();
                    bi.next();
                    match x.cmp(&y) {
                        Ordering::Equal => {}
                        other => return other,
                    }
                }
            }
        }
    }
}

/// 读掉一段连续数字，返回它的文本。
fn take_digits<I: Iterator<Item = char>>(it: &mut std::iter::Peekable<I>) -> String {
    let mut s = String::new();
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    s
}

/// 两串数字去掉前导零后按位数、再按字典序比；数值相等时按原文打破平局。
fn cmp_digits(a: &str, b: &str) -> std::cmp::Ordering {
    // 前导零去掉再比数值：`007` 与 `7` 应当相等（它们指向同一个「第 7 集」）
    let ta = a.trim_start_matches('0');
    let tb = b.trim_start_matches('0');
    if ta.len() != tb.len() {
        return ta.len().cmp(&tb.len());
    }
    if ta != tb {
        return ta.cmp(tb);
    }
    // 数值相等但写法不同（`7` vs `007`）：继续比剩下的字符，
    // 让排序**完全确定** —— 否则文件顺序会依赖扫描顺序
    a.cmp(b)
}

/// 扫一个目录，返回排序好的视频路径。
///
/// 只看**当前这一层**，不进子目录。理由：
///
/// * 「打开文件夹」的常见诉求是「看这个文件夹里的一集电影」或者
///   「扫一个番剧的文件夹」—— 后者如果那个文件夹里还有 `BD/` `字幕/` 之类
///   的子目录，递归会把几百个文件塞进列表，而用户要的是那 12 集
/// * 递归的另一个问题是**扫得慢**且可能在环里（目录联接）
/// * 真要递归的话，用户可以自己打开子目录 —— 那本来就是这个功能的入口
///
/// **扫不到就返回空 vec**，不报错：文件夹里没有视频是很正常的事，
/// 而弹一个错误框只会让用户以为程序坏了。调用方负责给一句人话。
///
/// ## 大小写与扩展名
///
/// 扩展名一律转小写再比（`.MP4` 与 `.mp4` 是同一个东西）。
/// 文件名排序按**完整路径的文件名部分**，不含目录 —— 同一个目录里的
/// 文件名部分就唯一了，带上目录名只会让所有文件的公共前缀参与比较。
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_video_ext(p))
        .collect();
    out.sort_by(|a, b| {
        let na = a.file_name().map(|s| s.to_string_lossy().into_owned());
        let nb = b.file_name().map(|s| s.to_string_lossy().into_owned());
        natural_cmp(na.as_deref().unwrap_or(""), nb.as_deref().unwrap_or(""))
    });
    out
}

/// 读取当前目录里的外挂字幕，供播放列表按视频名配对。
///
/// 视频保持 `scan` 排好的顺序；字幕只作为 `Playlist::from_paths` 的输入，
/// 不占播放列表条目。与拖入视频+字幕时使用同一套匹配规则。
pub fn scan_subtitles(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && crate::playlist::is_subtitle_path(path))
        .collect()
}

/// 挑一个「最该先放」的文件来播。
///
/// 有正在播的那个 → 就是它。没有 → 取第一个，也就是自然序里的第一个。
///
/// 为什么要「有就继续用」而不是每次都跳到第一个：用户在文件夹里看片看到
/// 第 5 集，然后临时去文件夹里加了两个文件，再回来时**不该被弹回第 1 集**。
pub fn preferred_start(files: &[PathBuf], current: Option<&Path>) -> Option<usize> {
    if let Some(cur) = current {
        if let Some(i) = files.iter().position(|p| p == cur) {
            return Some(i);
        }
    }
    if files.is_empty() {
        None
    } else {
        Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::Playlist;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 只清理本测试成功创建的目录，不碰其他测试或进程留下的文件。
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时间早于纪元")
                .as_nanos();
            for _ in 0..100 {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let dir = std::env::temp_dir()
                    .join(format!("vv-scan-test-{}-{nonce}-{id}", std::process::id()));
                match std::fs::create_dir(&dir) {
                    Ok(()) => return Self(dir),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("建测试目录 {dir:?} 失败：{e}"),
                }
            }
            panic!("找不到可用的测试目录名");
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_files(dir: &Path, names: &[&str]) {
        for name in names {
            std::fs::write(dir.join(name), b"x").expect("写测试文件");
        }
    }

    fn sorted(v: &[&str]) -> Vec<String> {
        let ps: Vec<PathBuf> = v
            .iter()
            .map(|s| PathBuf::from(format!("d:\\v\\{s}")))
            .collect();
        let mut out = ps;
        out.sort_by(|a, b| {
            natural_cmp(
                &a.file_name().unwrap().to_string_lossy(),
                &b.file_name().unwrap().to_string_lossy(),
            )
        });
        out.iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn 数字段按数值比() {
        // 这就是「第 2 集排在第 10 集前面」那条
        assert_eq!(
            sorted(&["第 10 集.mp4", "第 2 集.mp4"]),
            vec!["第 2 集.mp4", "第 10 集.mp4"]
        );
        // 字典序会给错答案，把这条钉住就是为了防止有人「简化」回 `cmp`
        let mut dict = ["第 10 集.mp4", "第 2 集.mp4"];
        dict.sort();
        assert_eq!(dict[0], "第 10 集.mp4", "字典序确实是错的（这是对照组）");
    }

    #[test]
    fn 多段数字分别比() {
        assert_eq!(
            sorted(&["S01E10.mp4", "S01E02.mp4", "S02E01.mp4"]),
            vec!["S01E02.mp4", "S01E10.mp4", "S02E01.mp4"]
        );
        assert_eq!(
            sorted(&["1080p.mp4", "720p.mp4", "2160p.mp4"]),
            vec!["720p.mp4", "1080p.mp4", "2160p.mp4"],
            "按数值：720 < 1080 < 2160。字典序会给 1080 < 2160 < 720"
        );
    }

    #[test]
    fn 前导零按同一个数比_但排序仍然确定() {
        // `007` 与 `7` 数值相等，但用原文打破平局以确定顺序
        assert_eq!(natural_cmp("007.mp4", "7.mp4"), std::cmp::Ordering::Less);
        assert_eq!(
            natural_cmp("007.mp4", "7.mp4"),
            natural_cmp("7.mp4", "007.mp4").reverse()
        );
        // 但**排序必须完全确定**：同一批文件扫两次顺序一样。
        // 下面两次用不同的初始顺序，扫完应当一致。
        let a = vec![
            PathBuf::from("d:\\v\\7.mp4"),
            PathBuf::from("d:\\v\\007.mp4"),
            PathBuf::from("d:\\v\\06.mp4"),
        ];
        let b = vec![
            PathBuf::from("d:\\v\\007.mp4"),
            PathBuf::from("d:\\v\\06.mp4"),
            PathBuf::from("d:\\v\\7.mp4"),
        ];
        let key = |v: &mut Vec<PathBuf>| {
            v.sort_by(|x, y| {
                natural_cmp(
                    &x.file_name().unwrap().to_string_lossy(),
                    &y.file_name().unwrap().to_string_lossy(),
                )
            });
            v.iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(key(&mut a.clone()), key(&mut b.clone()));
    }

    #[test]
    fn 超长数字不溢出() {
        // 21 位数字装不进 u64，按去掉前导零后的位数比，不会溢出
        let big = "999999999999999999999.mp4";
        let small = "1.mp4";
        assert_eq!(natural_cmp(big, small), std::cmp::Ordering::Greater);
        assert_eq!(natural_cmp(small, big), std::cmp::Ordering::Less);
    }

    #[test]
    fn 空与非数字也能比() {
        assert_eq!(natural_cmp("", ""), std::cmp::Ordering::Equal);
        assert_eq!(natural_cmp("", "a"), std::cmp::Ordering::Less);
        assert_eq!(natural_cmp("a", ""), std::cmp::Ordering::Greater);
        assert_eq!(natural_cmp("abc", "abd"), std::cmp::Ordering::Less);
        // 数字与非数字：数字段的码值比非数字段小，所以数字在前
        assert_eq!(natural_cmp("1", "a"), std::cmp::Ordering::Less);
    }

    #[test]
    fn 扩展名判定忽略大小写() {
        assert!(is_video_ext(Path::new("a.mp4")));
        assert!(is_video_ext(Path::new("a.MP4")));
        assert!(is_video_ext(Path::new("a.MkV")));
        assert!(!is_video_ext(Path::new("a.srt")));
        assert!(!is_video_ext(Path::new("a.txt")));
        assert!(!is_video_ext(Path::new("a")), "没有扩展名不算");
        assert!(!is_video_ext(Path::new("mp4")), "叫 mp4 的无扩展名文件不算");
        // 中文文件名里的扩展名一样认
        assert!(is_video_ext(Path::new("第一集.mp4")));
    }

    #[test]
    fn 扫描只看视频_而且排好序() {
        let dir = TestDir::new();
        write_files(
            dir.path(),
            &[
                "第 10 集.mp4",
                "第 2 集.mp4",
                "第 1 集.mkv",
                "第 3 集.MP4",
                "说明.txt",
                "封面.jpg",
                "第 2 集.srt",
                "readme",
            ],
        );
        // 子目录里的视频**不**收（只看当前这一层）
        std::fs::create_dir(dir.path().join("BD")).unwrap();
        std::fs::write(dir.path().join("BD/bonus.mp4"), b"x").unwrap();

        let got = scan(dir.path());
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["第 1 集.mkv", "第 2 集.mp4", "第 3 集.MP4", "第 10 集.mp4"],
            "只收视频、按自然序、不进子目录"
        );
    }

    #[test]
    fn 扫字幕只收当前目录里的常见字幕() {
        let dir = TestDir::new();
        write_files(
            dir.path(),
            &[
                "a.srt",
                "b.ASS",
                "c.sSa",
                "d.VTT",
                "e.sub",
                "readme",
                "movie.MP4",
            ],
        );
        std::fs::create_dir(dir.path().join("假字幕.srt")).unwrap();
        std::fs::create_dir(dir.path().join("BD")).unwrap();
        std::fs::write(dir.path().join("BD/inside.srt"), b"x").unwrap();

        // read_dir 不承诺顺序；只检查集合，不给字幕扫描强加排序要求。
        let mut got = scan_subtitles(dir.path());
        got.sort();
        let mut expected: Vec<_> = ["a.srt", "b.ASS", "c.sSa", "d.VTT"]
            .iter()
            .map(|name| dir.path().join(name))
            .collect();
        expected.sort();
        assert_eq!(got, expected, "只收本层的字幕文件，扩展名不区分大小写");
        assert!(scan_subtitles(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn 扫描视频与字幕重建列表_保持自然序并选中当前项() {
        let dir = TestDir::new();
        write_files(
            dir.path(),
            &[
                "Ep10.mp4",
                "Ep2.MP4",
                "Ep1.mkv",
                "Ep1.srt",
                "Ep2.zh.SRT",
                "Ep10.ass",
                "Ep2extra.srt",
                "无扩展名",
                "说明.txt",
            ],
        );
        std::fs::create_dir(dir.path().join("BD")).unwrap();
        std::fs::write(dir.path().join("BD/bonus.mp4"), b"x").unwrap();
        std::fs::write(dir.path().join("BD/Ep2.en.srt"), b"x").unwrap();

        let files = scan(dir.path());
        assert_eq!(
            files,
            ["Ep1.mkv", "Ep2.MP4", "Ep10.mp4"]
                .iter()
                .map(|name| dir.path().join(name))
                .collect::<Vec<_>>(),
            "列表只包含本层视频，按自然序排列"
        );
        let current = dir.path().join("Ep2.MP4");
        let start = preferred_start(&files, Some(&current)).expect("有视频可播放");
        assert_eq!(start, 1, "已有当前项不应跳回第一集");
        let mut paths = files.clone();
        paths.extend(scan_subtitles(dir.path()));
        let (pl, loaded) = Playlist::from_paths(&paths);
        let mut pl = pl.expect("扫描结果应建出列表");
        assert_eq!(
            pl.items()
                .iter()
                .map(|it| it.path.clone())
                .collect::<Vec<_>>(),
            files
        );
        assert_eq!(pl.items()[0].sidecar, Some(dir.path().join("Ep1.srt")));
        assert_eq!(pl.items()[1].sidecar, Some(dir.path().join("Ep2.zh.SRT")));
        assert_eq!(pl.items()[2].sidecar, Some(dir.path().join("Ep10.ass")));
        // from_paths 的加载信息针对第一条；打开文件夹应取选中项自己的字幕。
        assert_eq!(
            loaded.expect("加载信息").sidecars,
            vec![dir.path().join("Ep1.srt")]
        );
        let sidecars: Vec<_> = pl.items()[start].sidecar.iter().cloned().collect();
        assert_eq!(sidecars, vec![dir.path().join("Ep2.zh.SRT")]);
        assert!(pl.select(start));
        assert_eq!(pl.current_item().path, current);
        assert_eq!(pl.current_item().sidecar, sidecars.first().cloned());
    }

    #[test]
    fn 扫不存在的目录不报错() {
        // 目录被删了 / 没权限 / 是个网络位置 —— 都只该返回空
        assert!(scan(Path::new(r"z:\definitely\not\here")).is_empty());
    }

    #[test]
    fn 起始项_有正在播的就用它() {
        let files = vec![PathBuf::from("d:\\v\\a.mp4"), PathBuf::from("d:\\v\\b.mp4")];
        assert_eq!(
            preferred_start(&files, Some(Path::new("d:\\v\\b.mp4"))),
            Some(1)
        );
        assert_eq!(
            preferred_start(&files, None),
            Some(0),
            "没有正在播的就从第一个开始"
        );
        assert_eq!(
            preferred_start(&files, Some(Path::new("d:\\v\\gone.mp4"))),
            Some(0),
            "正在播的那个不在新列表里就从第一个开始"
        );
        assert_eq!(preferred_start(&[], None), None);
    }
}
