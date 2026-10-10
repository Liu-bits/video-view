//! 书签：用户在播放中标记的「名字 + 位置」。
//!
//! ## 与 [`resume`](crate::resume) 的分工
//!
//! `resume` 记的是**一条**：每个文件上次播到哪，由程序在退出时自动写、
//! 下次打开时自动跳。书签是**用户主动标**的：一份文件可以标很多个，
//! 每个都有名字，而且要能被点名跳过去。
//!
//! 所以两者不能共用一条记录 —— 把书签塞进 `resume` 会让「自动续播」
//! 变成「跳到最后一个书签」，那是完全不同的功能。
//!
//! ## 格式：三个长度前缀
//!
//! ```text
//! <路径字节数> <名字字节数> <秒数>\n<路径原文><名字原文>\n
//! ```
//!
//! 三个前缀里前两个是**长度**、第三个是数值，这样路径和名字里都能有换行
//! （Windows 允许文件名含 `\n`；名字是用户敲的，一个 Shift+Enter 就有了）。
//! 只用「按行切分」的话这两种内容都会被切成两半，得到两个都不存在的条目。
//!
//! 为什么不复用 `resume` 的两段式再加一个字段：那样解析器要处理
//! 「长度是路径的还是名字的」这种二义性，而这个格式里字段位置固定，
//! 读错了立刻就对不上后面的字节流，不存在「读出一个看起来合法的错记录」。
//!
//! ## 为什么不用 JSON
//!
//! 和 `resume` 同一个理由：唯一能写进去的东西就是「一个路径、一个名字、
//! 一个浮点数」，为此引 `serde` 或手写几百行解析器都不划算。而这个格式
//! 十几行写得完，且坏一个字节不会让整个文件读不出来。

use std::path::{Path, PathBuf};

/// 一条书签。
#[derive(Debug, Clone, PartialEq)]
pub struct Bookmark {
    /// 属于哪个视频文件
    pub path: PathBuf,
    /// 位置（秒）
    pub position: f64,
    /// 用户给的名字
    pub title: String,
}

/// 最多记多少条。
///
/// 给 2000：一个 40 集的番剧每集标 10 个关键点是 400 条，2000 留了五倍
/// 余量。而按每条 100 字节算是 200 KB —— 启动时顺序扫一遍是毫秒级，
/// 不会因为这个上限而卡。
pub const MAX_BOOKMARKS: usize = 2000;

/// 名字长度上限（字节）。
///
/// 名字是用户敲的，而文件在 `%LOCALAPPDATA%` 下、当前用户完全可写，
/// 所以这个长度是**不可信输入**的边界。不设上限的话，粘进去一段 64MB 的
/// 文本就能让每次启动分配几百 MB —— 一个纯数据项当内存放大器用。
///
/// 200 字节远超任何有意义的章节名（最长的是「第三章：主角发现真相那一段」
/// 这种，也就 60 多字节）。
pub const MAX_TITLE_BYTES: usize = 200;

/// 位置小于这个值就不记。
///
/// 与 `resume::MIN_SECONDS` 同值但**理由不同**：这里不是「没有记忆价值」，
/// 而是「开头 2 秒内的书签点回去等于没点」，而且用户多半是误按了 F2。
pub const MIN_SECONDS: f64 = 1.0;

/// 解析书签文件。**读不出的条目跳过，后面的继续读。**
///
/// 与 `resume::parse` 同一个立场：书签是便利功能，坏了不该拦住用户看片。
/// 拦住的代价（打不开视频）远大于丢几条书签。
pub fn parse(text: &str) -> Vec<Bookmark> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0usize;
    let mut fails = 0usize;
    while i < b.len() {
        // 1. 路径字节数
        let Some((path_len, after)) = read_usize(b, i) else {
            if !resync(b, &mut i, &mut fails) {
                break;
            }
            continue;
        };
        // 2. 一个空格
        if b.get(after) != Some(&b' ') {
            if !resync(b, &mut i, &mut fails) {
                break;
            }
            continue;
        }
        // 3. 名字字节数
        let Some((title_len, after)) = read_usize(b, after + 1) else {
            if !resync(b, &mut i, &mut fails) {
                break;
            }
            continue;
        };
        // 4. 一个空格
        if b.get(after) != Some(&b' ') {
            if !resync(b, &mut i, &mut fails) {
                break;
            }
            continue;
        }
        // 5. 秒数
        let Some((pos, after)) = read_usize(b, after + 1) else {
            if !resync(b, &mut i, &mut fails) {
                break;
            }
            continue;
        };
        // 6. 一个换行
        if b.get(after) != Some(&b'\n') {
            if !resync(b, &mut i, &mut fails) {
                break;
            }
            continue;
        }
        // 7. 路径 + 名字的正文。**checked_add**：两个长度都来自文件，
        // `usize::MAX` 是合法输入，回绕后越界检查会失效，然后切片 panic ——
        // 「用户目录里的纯文本被改坏一个字节」就能让播放器起不来。
        let body = after + 1;
        let Some(path_end) = body.checked_add(path_len) else {
            if !resync(b, &mut i, &mut fails) {
                break;
            }
            continue;
        };
        let Some(end) = path_end.checked_add(title_len) else {
            if !resync(b, &mut i, &mut fails) {
                break;
            }
            continue;
        };
        if end > b.len() {
            if !resync(b, &mut i, &mut fails) {
                break;
            }
            continue;
        }
        // 长度字段被改小就会切在多字节字符中间。用 `from_utf8` 而不是
        // `from_utf8_lossy`：丢字节的路径会指向另一个文件，而那正是
        // 「书签跳到别的地方去了」最难受的形态。
        let (Ok(path_str), Ok(title_str)) = (
            std::str::from_utf8(&b[body..path_end]),
            std::str::from_utf8(&b[path_end..end]),
        ) else {
            i = skip_record(b, end);
            fails += 1;
            continue;
        };
        out.push(Bookmark {
            path: PathBuf::from(path_str),
            position: pos as f64,
            title: title_str.to_string(),
        });
        fails = 0;
        i = skip_record(b, end);
    }
    out
}

/// 连续重新同步次数的上限。理由同 `resume::MAX_RESYNC`。
const MAX_RESYNC: usize = 64;

/// 重新同步：跳到下一个换行之后。返回 `false` 表示收手。
///
/// 按行对齐保住「回到记录边界」这个不变式；逐字节挪会在长度字段被改过
/// 之后一错到底。详见 `resume::resync` 的说明。
fn resync(b: &[u8], i: &mut usize, fails: &mut usize) -> bool {
    *fails += 1;
    if *fails > MAX_RESYNC {
        return false;
    }
    match b[*i..].iter().position(|&c| c == b'\n') {
        Some(off) => *i += off + 1,
        None => *i = b.len(),
    }
    true
}

/// 跳过正文后面那个换行，返回下一条的起点。
///
/// 成功与 UTF-8 校验失败**两条路径都要调**：漏了后者的话下一轮直接读到
/// `\n` 收手，后面所有条目一起丢。
fn skip_record(b: &[u8], end: usize) -> usize {
    let mut i = end;
    if b.get(i) == Some(&b'\n') {
        i += 1;
    }
    i
}

/// 读完一个十进制数字。返回 `(值, 数字后面的下标)`。
fn read_usize(b: &[u8], mut i: usize) -> Option<(usize, usize)> {
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    std::str::from_utf8(&b[start..i])
        .ok()?
        .parse()
        .ok()
        .map(|v| (v, i))
}

/// 序列化。每条写成「一行头 + 一行正文」。
///
/// 位置非有限、为负、或小于 [`MIN_SECONDS`] 的**直接跳过**而不是写个 0：
/// 那样下次加载出来是一条「0 秒的书签」，用户点它等于没跳。
///
/// 名字为空白的也跳过 —— 那等价于没起名字，而空名字在菜单里和分隔行
/// 长得一样。
pub fn serialize(list: &[Bookmark]) -> String {
    let mut out = String::new();
    for bm in list {
        if !bm.position.is_finite() || bm.position < MIN_SECONDS {
            continue;
        }
        if bm.title.trim().is_empty() {
            continue;
        }
        // 名字超长就截到字节边界上 —— 截出来的半个 UTF-8 字符会让整个
        // 文件的 `parse` 在这一条上失败（`from_utf8` 拒绝），连带后面
        // 的条目一起受影响。
        let title = truncate_utf8(&bm.title, MAX_TITLE_BYTES);
        if title.is_empty() {
            continue;
        }
        let path = bm.path.to_string_lossy();
        out.push_str(&format!(
            "{} {} {}\n",
            path.len(),
            title.len(),
            bm.position as u64
        ));
        out.push_str(&path);
        out.push_str(title);
        out.push('\n');
    }
    out
}

/// 按**字节**上限截断，但**不切开一个 UTF-8 字符**。
///
/// 从尾部回退到最近的字符边界。UTF-8 的续���字节（`0b10xxxxxx`）都不是
/// 字符边界，所以一直退到主字节为止就是安全落点。
fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 加一条书签。返回 `true` 表示写进去了。
///
/// **同名同文件视为「移动」而不是「新增」**：用户在一个位置按了 F2 之后
/// 又往前挪了两秒再按一次，他想要的是那一个书签被挪过去，而不是多出
/// 两个指向同一个章节、名字还一样的条目 —— 后者在菜单里根本分不清。
///
/// 名字撞了但文件不同 → 两条都留（不同文件可以有同名章节）。
pub fn add(list: &mut Vec<Bookmark>, bm: Bookmark) -> bool {
    if !bm.position.is_finite() || bm.position < MIN_SECONDS {
        return false;
    }
    if bm.title.trim().is_empty() {
        return false;
    }
    if let Some(existing) = list
        .iter_mut()
        .find(|b| b.path == bm.path && b.title == bm.title)
    {
        existing.position = bm.position;
    } else {
        list.push(bm);
    }
    prune(list);
    true
}

/// 删掉某个文件上的全部书签。返回删了几条。
pub fn remove_file(list: &mut Vec<Bookmark>, path: &Path) -> usize {
    let before = list.len();
    list.retain(|b| b.path != path);
    before - list.len()
}

/// 超出上限时丢掉一部分。
///
/// **丢的是路径+名字哈希最小的一段**，理由与 `resume::prune` 一样：
/// 丢的集合必须是**确定的**，否则用户会觉得「随机有书签被删了」。
/// 这里在路径之外把名字也并进哈希 —— 否则同一个文件上最先标的那批
/// 名字会连带整条被丢，看起来像「老书签先没了」。
pub fn prune(list: &mut Vec<Bookmark>) {
    if list.len() <= MAX_BOOKMARKS {
        return;
    }
    use std::hash::{Hash, Hasher};
    let mut keyed: Vec<(u64, usize)> = list
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            b.path.hash(&mut h);
            b.title.hash(&mut h);
            (h.finish(), i)
        })
        .collect();
    keyed.sort_unstable();
    let drop_n = list.len() - MAX_BOOKMARKS;
    let mut drop: Vec<usize> = keyed.into_iter().take(drop_n).map(|(_, i)| i).collect();
    drop.sort_unstable();
    for i in drop.into_iter().rev() {
        list.remove(i);
    }
}

/// 某个文件上的书签，按位置升序。
///
/// **每次调用都克隆**是刻意的：菜单是一弹就关的东西，一次排序 + 几条
/// `String` 克隆的代价可以忽略；而借用会让 `Snapshot` 的生命周期
/// 再套一层（菜单构造期间不能改书签列表），那个约束比这点开销难维护得多。
///
/// 位置相同的按名字排 —— 两个书签在同一秒时（用户连按两次 F2）顺序
/// 才稳定，否则 `HashMap` 之类的无序容器会让菜单项每次打开顺序都不同。
pub fn for_file(list: &[Bookmark], path: &Path) -> Vec<Bookmark> {
    let mut out: Vec<Bookmark> = list.iter().filter(|b| b.path == path).cloned().collect();
    out.sort_by(|a, b| {
        a.position
            .partial_cmp(&b.position)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
    });
    out
}

/// 书签文件放在 `dir` 底下，文件名 `bookmarks.txt`。
///
/// 与 [`resume::file_in`] 同一个约定：`dir` 必须已经是
/// `%LOCALAPPDATA%\VideoView`（`crashlog::appdata_dir()` 给的），这里
/// **不再**拼一层 —— 之前拼出双层 `VideoView` 导致 `fs::write` 静默
/// 返回 `NotFound`，书签功能「完全没生效」而界面上看不出任何异常。
pub fn file_in(dir: &Path) -> PathBuf {
    dir.join("bookmarks.txt")
}

/// 名字太长时给菜单用的短版本。
///
/// 菜单项宽度受屏幕宽度限制（见 `build_timing` 里关于菜单翻转的说明），
/// 而书签名字是用户自由敲的，不截的话一个三十字的名字就能把菜单顶到
/// 屏幕外。按**字符**截而不是按字节 —— 按字节截会切出半个字符。
pub fn short_title(title: &str, max_chars: usize) -> String {
    if title.chars().count() <= max_chars {
        return title.to_string();
    }
    let mut s: String = title.chars().take(max_chars.saturating_sub(1)).collect();
    s.push('\u{2026}');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn bm(path: &str, position: f64, title: &str) -> Bookmark {
        Bookmark {
            path: p(path),
            position,
            title: title.to_string(),
        }
    }

    #[test]
    fn 空文件解析出空表() {
        assert!(parse("").is_empty());
        assert!(parse("\n\n\n").is_empty());
    }

    #[test]
    fn 一条往返() {
        let list = vec![bm(r"d:\v\a.mp4", 123.0, "开场")];
        let back = parse(&serialize(&list));
        assert_eq!(back, list);
    }

    #[test]
    fn 多条往返() {
        let list = vec![
            bm(r"d:\v\a.mp4", 10.0, "第一段"),
            bm(r"d:\v\a.mp4", 200.0, "第二段"),
            bm(r"d:\v\b.mkv", 5.0, "别的文件"),
        ];
        assert_eq!(parse(&serialize(&list)), list);
    }

    /// 名字里**可以**有换行 —— 用户敲 Shift+Enter 就是。
    /// 按行切分的格式在这里会把它切成两半。
    #[test]
    fn 名字里有换行也认得出来() {
        let list = vec![bm(r"d:\v\a.mp4", 42.0, "第一行\n第二行")];
        assert_eq!(parse(&serialize(&list)), list);
    }

    #[test]
    fn 路径里有换行没问题() {
        let list = vec![bm("d:\\v\\we\nird.mp4", 42.0, "章节")];
        assert_eq!(parse(&serialize(&list)), list);
    }

    #[test]
    fn 路径里有中文没问题() {
        let list = vec![bm(r"d:\视频\第一集.mp4", 99.0, "片头")];
        assert_eq!(parse(&serialize(&list)), list);
    }

    #[test]
    fn 名字里有空格没问题() {
        let list = vec![bm("d:\\my videos\\a b.mp4", 7.0, "a b c")];
        assert_eq!(parse(&serialize(&list)), list);
    }

    /// 坏数据必须放在**中间**。全放在末尾的话，无论实现是「逐条跳过」
    /// 还是「一坏就放弃整个文件」，这个测试都过 —— 它证明不了任何东西。
    #[test]
    fn 坏条目在中间也不影响后面的() {
        let mut text = String::new();
        text.push_str("5 3 5\na.mp4abc\n"); // 正常
        text.push_str("12 x 5\n"); // 秒数不是数字
        text.push_str("99 5 5\nshort\n"); // 声称路径 99 字节，文件里没有
                                          // 正常：路径 `b.mp4` 5 字节 + 名字 `xy` 2 字节。
                                          // 这里原本写的是 `5 4 11` —— 4 字节的名字在正文里放不下
                                          // （`b.mp4xy` 去掉换行只有 7 字节，而 5 + 4 = 9），于是这一条
                                          // 也被当成坏数据跳过，测试只剩 1 条。**测试数据自己写错**，
                                          // 比实现错更常见，而且更难看出来。
        text.push_str("5 2 11\nb.mp4xy\n");

        let list = parse(&text);
        assert_eq!(list.len(), 2, "两条好的都要保住：{list:?}");
        assert_eq!(list[0].title, "abc");
        assert_eq!(list[1].title, "xy");
    }

    /// 长度字段溢出不能崩。`usize::MAX` 是合法十进制输入，回绕后
    /// 越界检查失效，切片 panic —— 「改坏一个字节就让播放器起不来」。
    #[test]
    fn 长度字段溢出不会崩() {
        for t in [
            "18446744073709551615 5 5\nx\n",
            "99999999999999999999999999 5 5\nx\n",
            "5 18446744073709551615 5\nx\n",
            "18446744073709551615 18446744073709551615 5\nx\n",
        ] {
            let list = parse(t);
            assert!(list.is_empty(), "{t:?} 应当解析出空表，而不是 panic");
        }
    }

    #[test]
    fn 全是坏数据不会死循环() {
        for t in [
            " ",
            "\n",
            "0 \n",
            "x",
            "x x x\nx\n",
            "999999999999999999999999999999999 1 1\nz\n",
        ] {
            let list = parse(t);
            assert!(list.len() <= 1, "{t:?} 解析出了意外多的条目");
        }
    }

    /// 长度字段被改小 → 切在多字节字符中间 → `from_utf8` 失败。
    /// 那一条要跳过，而且**后面完好的条目要保住**。
    #[test]
    fn 路径切在多字节字符中间不影响后面的条目() {
        // 「中」是 3 字节，长度写 1 = 只切出半个字符
        let mut text = String::new();
        text.push_str("1 3 5\n");
        text.push_str("中.mp4abc\n");
        text.push_str("5 1 9\n");
        text.push_str("c.mp4x\n");

        let list = parse(&text);
        assert!(
            !list
                .iter()
                .any(|b| b.path.to_string_lossy().contains('\u{FFFD}')),
            "不该把半个字符当成路径：{list:?}"
        );
        assert_eq!(list.len(), 1, "坏条目之后的条目要保住");
        assert_eq!(list[0].title, "x");
    }

    #[test]
    fn 位置非法与空名字都不写() {
        let list = vec![
            bm("a.mp4", 0.5, "太靠前"),
            bm("b.mp4", f64::NAN, "NaN"),
            bm("c.mp4", -3.0, "负数"),
            bm("d.mp4", 10.0, "   "),
            bm("e.mp4", 10.0, ""),
        ];
        let back = parse(&serialize(&list));
        assert!(back.is_empty(), "这五条都不该写出去：{back:?}");
    }

    #[test]
    fn 同名同文件是移动而不是新增() {
        let mut list = Vec::new();
        assert!(add(&mut list, bm("a.mp4", 10.0, "章节")));
        assert!(add(&mut list, bm("a.mp4", 50.0, "章节")));
        assert_eq!(list.len(), 1, "同名同文件应当只有一条");
        assert_eq!(list[0].position, 50.0, "位置要被更新");
    }

    #[test]
    fn 同名不同文件两条都留() {
        let mut list = Vec::new();
        add(&mut list, bm("a.mp4", 10.0, "开场"));
        add(&mut list, bm("b.mp4", 10.0, "开场"));
        assert_eq!(list.len(), 2, "不同文件可以有同名章节");
    }

    #[test]
    fn 非法位置与空名字加不进去() {
        let mut list = Vec::new();
        assert!(!add(&mut list, bm("a.mp4", 0.0, "章节")));
        assert!(!add(&mut list, bm("a.mp4", f64::NAN, "章节")));
        assert!(!add(&mut list, bm("a.mp4", 10.0, " ")));
        assert!(list.is_empty());
    }

    #[test]
    fn 删某个文件的书签() {
        let mut list = vec![
            bm("a.mp4", 10.0, "一"),
            bm("a.mp4", 20.0, "二"),
            bm("b.mp4", 10.0, "三"),
        ];
        assert_eq!(remove_file(&mut list, &p("a.mp4")), 2);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, p("b.mp4"));
    }

    #[test]
    fn 按文件取出来并按位置排序() {
        let list = vec![
            bm("a.mp4", 200.0, "后面"),
            bm("b.mp4", 1.0, "别的文件"),
            bm("a.mp4", 10.0, "前面"),
        ];
        let got = for_file(&list, &p("a.mp4"));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].title, "前面");
        assert_eq!(got[1].title, "后面");
    }

    /// 位置相同时按名字排 —— 否则同一秒的两个书签顺序会随机，
    /// 用户每次打开菜单看到的排列都不一样。
    #[test]
    fn 位置相同时顺序稳定() {
        let list = vec![bm("a.mp4", 10.0, "B"), bm("a.mp4", 10.0, "A")];
        let got = for_file(&list, &p("a.mp4"));
        assert_eq!(got[0].title, "A");
        assert_eq!(got[1].title, "B");
    }

    #[test]
    fn 裁剪到上限() {
        let mut list: Vec<Bookmark> = (0..MAX_BOOKMARKS + 500)
            .map(|i| bm(&format!("f{i}.mp4"), i as f64 + 10.0, "章节"))
            .collect();
        prune(&mut list);
        assert_eq!(list.len(), MAX_BOOKMARKS);
    }

    /// 裁剪必须丢**同一批**，否则用户会觉得「随机有书签没了」。
    #[test]
    fn 裁剪是稳定的() {
        let build = || -> Vec<Bookmark> {
            (0..MAX_BOOKMARKS + 500)
                .map(|i| bm(&format!("f{i}.mp4"), i as f64 + 10.0, "章节"))
                .collect()
        };
        let mut a = build();
        let mut b = build();
        prune(&mut a);
        prune(&mut b);
        let ka: Vec<_> = a
            .iter()
            .map(|x| (x.path.clone(), x.title.clone()))
            .collect();
        let kb: Vec<_> = b
            .iter()
            .map(|x| (x.path.clone(), x.title.clone()))
            .collect();
        assert_eq!(ka, kb, "两次裁剪必须丢掉同一批");
    }

    /// 截断不能切开 UTF-8 字符，否则整条记录的 `from_utf8` 会失败，
    /// 连带**后面**的条目一起丢。
    #[test]
    fn 截断不切开字符() {
        let s = "中文标题很长很长";
        for n in 0..40 {
            let t = truncate_utf8(s, n);
            assert!(
                std::str::from_utf8(t.as_bytes()).is_ok(),
                "n={n} 截出了半个字符"
            );
            assert!(t.len() <= n, "n={n} 截超了");
        }
    }

    #[test]
    fn 超长名字被截断后还能往返() {
        let long = "很长的章节名".repeat(100);
        let list = vec![bm("a.mp4", 10.0, &long)];
        let text = serialize(&list);
        let back = parse(&text);
        assert_eq!(back.len(), 1, "截断后的记录仍要读得回来");
        assert!(back[0].title.len() <= MAX_TITLE_BYTES);
    }

    #[test]
    fn 菜单用的短名字() {
        assert_eq!(short_title("短", 10), "短");
        assert_eq!(short_title("一二三四五", 3), "一二…");
        // 超长时不切开字符
        let got = short_title(&"中文".repeat(50), 5);
        assert_eq!(got.chars().count(), 5);
    }

    /// 书签文件只拼一层 `VideoView` —— 与 `resume` 同一个坑。
    #[test]
    fn 书签文件只拼一层_video_view() {
        let dir = crate::crashlog::appdata_dir().expect("拿不到 %LOCALAPPDATA%\\VideoView");
        let text = file_in(&dir).to_string_lossy().into_owned();
        assert!(text.ends_with(r"VideoView\bookmarks.txt"), "实际是 {text}");
        assert_eq!(text.matches("VideoView").count(), 1, "路径 {text}");
        assert!(dir.is_dir(), "{} 不存在", dir.display());
    }

    #[test]
    fn 书签文件能在真实目录里往返() {
        let Some(dir) = crate::crashlog::appdata_dir() else {
            return;
        };
        let path = dir.join("bookmarks-selftest.txt");
        let list = vec![bm(r"d:\v\a.mp4", 12.0, "章节")];
        assert!(std::fs::write(&path, serialize(&list)).is_ok());
        let back = parse(&std::fs::read_to_string(&path).unwrap_or_default());
        assert_eq!(back, list);
        let _ = std::fs::remove_file(&path);
    }
}
