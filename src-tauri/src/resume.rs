//! 上次播放位置的记忆。
//!
//! ## 为什么是文件而不是注册表
//!
//! 位置是「**按文件**记的」—— 一个 40 集的番剧要记 40 条，而注册表是
//! 「按值名记的」，塞 40 条得自己编一套 `pos_<hash>` 这样的值名，
//! 而那个 hash 要么会碰撞，要么得把路径本身当值名（注册表值名有长度限制，
//! 长路径会被截断，而截断之后两个文件可能撞在一起 —— 于是记得串了）。
//!
//! 一行一个条目更简单：解析是 O(n) 的线性扫描，读写都是顺序追加，
//! 而条目数上限本来就小（几百条文件也就几十 KB）。
//!
//! ## 格式：长度前缀 + 路径原文
//!
//! ```text
//! <路径字节数> <秒数>\n<路径字节>
//! ```
//!
//! 路径里**可以**有换行（Windows 允许），所以不能简单地按行切分。
//! 长度前缀让解析无歧义：读一个十进制数、空格、再读一个十进制数、
//! 然后按第一个换行切，剩下的 `n` 个字节就是路径。
//!
//! 为什么不用 JSON：那要引 `serde`（或手写一个几百行的解析器），而这个
//! 格式十几行就能写完且没有边界情况 —— 唯一能写进去的东西就是路径与一个
//! 浮点数。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 记多少条。
///
/// 给 500 是有依据的：一个追番文件夹放 500 集绰绰有余，而这个文件按
/// 每条 80 字节算是 40 KB。超过上限时按**路径哈希**丢（见 `prune`）——
/// 不是按「最久没看的先丢」，那个要额外记写入时间，不值当。
pub const MAX_ENTRIES: usize = 500;

/// 位置小于这个值就**不记**。
///
/// 开头两秒的位置没有记忆价值，而每次打开都会写一次 —— 记下来只是让文件
/// 一直增长，还要在下次打开时再 load 一次 0.1 秒然后 seek 回去。
pub const MIN_SECONDS: f64 = 2.0;

/// 距离片尾小于这个秒数时**删掉**那条记录而不是更新它。
///
/// 理由：看到片尾就意味着看完了，下次打开应该从头开始。
///
/// 给 30 秒是因为片尾字幕通常在那之前；给太短（比如 5 秒）的话，用户拖
/// 回去看最后一句台词就会被当成「看完了」，下次从头开始 —— 那比多记一条
/// 没用数据更烦人。
pub const TAIL_SECONDS: f64 = 30.0;

/// 解析记忆文件。**读不出的条目跳过，后面的继续读。**
///
/// 记忆文件是**便利功能**，坏了不该拦住用户看片 —— 拦住的代价（打不开）
/// 远大于丢几条位置记录。所以这里是「逐条跳过并重新同步」，不是
/// 「一坏就放弃整个文件」。
///
/// ## 重新同步怎么做
///
/// 记录是长度前缀的，读对了就能直接跳到下一条；但**一旦连长度字段都读不
/// 出来**（某个字节不是数字），就没有可信的下标了 —— 只能往前挪一个字节
/// 再试。
///
/// 这么挪会不会退化成 O(n²)？会：一份 40 KB 的纯数字垃圾会让 `read_usize`
/// 每次都扫到文件末尾。所以有 `MAX_RESYNC` 这个上限，而且计的是**连续**
/// 失败 —— 几处零散的坏记录之间隔着好记录时计数会清零，不会误伤；整份
/// 文件从头坏到尾时，代价被 `MAX_RESYNC × 文件长度` 封住。
pub fn parse(text: &str) -> HashMap<PathBuf, f64> {
    let mut map = HashMap::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut fails = 0usize;
    while i < bytes.len() {
        // 1. 路径字节数
        let Some((n, after_n)) = read_usize(bytes, i) else {
            if !resync(bytes, &mut i, &mut fails) {
                break;
            }
            continue;
        };
        // 2. 一个空格
        if bytes.get(after_n) != Some(&b' ') {
            if !resync(bytes, &mut i, &mut fails) {
                break;
            }
            continue;
        }
        // 3. 秒数
        let Some((secs, after_secs)) = read_usize(bytes, after_n + 1) else {
            if !resync(bytes, &mut i, &mut fails) {
                break;
            }
            continue;
        };
        // 4. 一个换行
        if bytes.get(after_secs) != Some(&b'\n') {
            if !resync(bytes, &mut i, &mut fails) {
                break;
            }
            continue;
        }
        let path_start = after_secs + 1;
        // **必须用 checked_add。** `n` 是从文件里读出来的十进制数，而
        // `usize::MAX` 是合法输入，`parse::<usize>()` 也成功。那么
        // `path_start + n` 在 release 下**回绕**成小数，下面
        // `path_end > bytes.len()` 就拦不住，接着
        // `&bytes[path_start..path_end]` 因为 `path_end < path_start`
        // 而 panic —— 「用户目录里的纯文本被改坏一个字节」就能让播放器
        // 起不来，正好违反这个模块开头「坏了不该拦住用户看片」的承诺。
        let Some(path_end) = path_start.checked_add(n) else {
            if !resync(bytes, &mut i, &mut fails) {
                break;
            }
            continue;
        };
        if path_end > bytes.len() {
            // 声称的路径长度超出文件末尾：这条坏了。挪一格再试。
            i = path_start;
            if !resync(bytes, &mut i, &mut fails) {
                break;
            }
            continue;
        }
        // `text` 是 `&str`，整个缓冲区一定是合法 UTF-8 —— 但
        // `&bytes[path_start..path_end]` **可能切在多字节字符中间**
        //（长度字段被改小了就会这样），这时 `from_utf8` 会失败。
        //
        // 用 `from_utf8` 而不是 `from_utf8_lossy`：丢字节的路径会指向另一个
        // 文件，而那正是「记串了」最难受的形态。
        let Ok(p) = std::str::from_utf8(&bytes[path_start..path_end]) else {
            i = skip_record(bytes, path_end);
            fails += 1;
            continue;
        };
        map.insert(PathBuf::from(p), secs as f64);
        fails = 0;
        i = skip_record(bytes, path_end);
    }
    map
}

/// 连续重新同步次数的上限。
///
/// 给 64：几处零散的坏记录远远够用；而「整份文件从头坏到尾」的代价被
/// `64 × 文件长度` 封住，40 KB 的文件也就是几百万次字符比较。
const MAX_RESYNC: usize = 64;

/// 重新同步：跳到**下一个换行之后**再重试。返回 `false` 表示收手。
///
/// ## 为什么是「跳到下一个换行」而不是「往前挪一个字节」
///
/// 记录总是从行首开始（`<长度> <秒数>\n<路径>\n`），所以下一个换行之后的
/// 位置就是一个**可能的**记录起点。逐字节挪的问题实测很清楚：一段
/// `"999 notanumber\n"` 里每个字节都不是数字开头，于是白扫 14 个字节
/// 才碰上换行 —— 能恢复，但慢得离谱。
///
/// 更糟的是逐字节挪**对齐不住**：长度字段被改大或改小之后，`skip_record`
/// 的落点本身就错了，接下来每一轮都在错的位置上解析，一错到底。
/// 按行对齐至少保住了「回到记录边界」这个不变式。
///
/// ## 它救不了什么
///
/// **不能**从任意损坏里精确恢复。一个被改小的长度字段会解析出一个看起来
/// 合法的路径（`short` 也是合法文件名），而那条假记录会把后面的真记录
/// 一起带偏。要做到「任何损坏都能精确恢复」得上校验和，那是给一个便利
/// 功能加的复杂度，不值当。
///
/// 能保证的是：文件**被截断**（写盘中途断电、同步盘冲突留下半个文件）之后，
/// 前面的完整记录都还在 —— 那才是现实里最常见的形态。
fn resync(bytes: &[u8], i: &mut usize, fails: &mut usize) -> bool {
    *fails += 1;
    if *fails > MAX_RESYNC {
        return false;
    }
    // 找下一个换行；找不到说明到文件末尾了，外层 `while` 会退出
    match bytes[*i..].iter().position(|&b| b == b'\n') {
        Some(off) => *i += off + 1,
        None => *i = bytes.len(),
    }
    true
}

/// 从路径末尾跳过那条记录后面的换行，返回下一个条目的起点。
///
/// **两条路径都必须调它**：成功的那条要跳（否则下一轮读到 `\n` 直接收手，
/// 结果只认得第一条），UTF-8 校验失败的那条也要跳（漏了它的话下一轮
/// 同样收手，**后面所有条目一起丢**）。
fn skip_record(bytes: &[u8], path_end: usize) -> usize {
    // `serialize` 在路径之后写了一个 `\n`，好让文本里每条记录各占两行、
    // 人能看。
    let mut i = path_end;
    if bytes.get(i) == Some(&b'\n') {
        i += 1;
    }
    i
}

/// 序列化。`map` 里的每一条写成两行：`<路径字节数> <秒数>` 换行、路径原文
/// 换行。
///
/// 路径后面那个换行**不是**为了好看才加的 —— `parse` 靠它切下一条记录
/// （见 `skip_record`）。它同时让文件用记事本打开时一条占两行、能看懂。
///
/// 秒数非有限或不为正的那几条**直接跳过**而不是写个 0：那样会在下次启动
/// 时被 `resume_to` 拒掉，白占地方还让人以为「记了个 0 秒」。
pub fn serialize(map: &HashMap<PathBuf, f64>) -> String {
    let mut out = String::new();
    for (p, secs) in map {
        if !secs.is_finite() || *secs <= 0.0 {
            continue;
        }
        let path = p.to_string_lossy();
        out.push_str(&format!("{} {}\n", path.len(), *secs as u64));
        out.push_str(&path);
        out.push('\n');
    }
    out
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
    let s = std::str::from_utf8(&b[start..i]).ok()?;
    // 溢出就当这一行坏了
    s.parse().ok().map(|v| (v, i))
}

/// 该记下这个位置吗？返回 `Some(秒)` 或 `None`（不记 / 该删）。
///
/// 三个结果：
/// - `Some(s)`：正常记住
/// - `None`：不用记（太靠近片头）
/// - 片尾：`None` **并且**调用方要删掉这条 —— 所以用枚举而不是 `Option`，
///   「不记」与「删掉」在行为上不一样，混成一个 `None` 就会漏删。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Keep {
    /// 记住这个秒数
    At(f64),
    /// 不记，但**保留**已有记录
    TooEarly,
    /// 不记，并且**删掉**已有记录（看到片尾了）
    Finished,
}

pub fn decide(position: f64, duration: Option<f64>) -> Keep {
    if !position.is_finite() || position < MIN_SECONDS {
        return Keep::TooEarly;
    }
    // 时长未知（比如某些流）时不能判断片尾，只按片头处理
    if let Some(d) = duration {
        if d.is_finite() && d > 0.0 && d - position <= TAIL_SECONDS {
            return Keep::Finished;
        }
    }
    Keep::At(position)
}

/// 超出上限时丢掉一部分。
///
/// **丢的是路径哈希最小的一段**，不是「最久没看的那些」。
///
/// 为什么不做「最久没看的先丢」：那需要给每条记一个写入时间，而 `Item`
/// 那一侧并不带这个信息 —— 为了一个 500 条上限的便利功能给记忆文件加
/// 时间戳，收益与复杂度不成比例。
///
/// 为什么选哈希而不是「随便丢」：哈希让丢的集合是**确定的**（同一批数据
/// 每次丢同样那些），而用户看到的是「我记了几百个文件里的某一个忘了进度」，
/// 不是「每次启动随机换一批文件记不住」。后者会让人怀疑是不是程序坏了。
///
/// **稳定的范围是「同一个二进制」**：`DefaultHasher` 的算法没有稳定性
/// 保证，升级 Rust 版本之后可能换掉。跨版本仍然确定，只是换了一个同样
/// 确定的集合 —— 对「表现可预期」这个目标没有影响。
pub fn prune(map: &mut HashMap<PathBuf, f64>, max: usize) {
    if map.len() <= max {
        return;
    }
    let mut keys: Vec<PathBuf> = map.keys().cloned().collect();
    keys.sort_by_key(|p| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        p.hash(&mut h);
        h.finish()
    });
    let drop_n = map.len() - max;
    for k in keys.into_iter().take(drop_n) {
        map.remove(&k);
    }
}

/// 删掉文件已经不存在的那些条目。
///
/// **只在能确定「永远看不到」的地方用。** `is_file()` 在断开的状态上会返回
/// `false` —— U 盘没插、NAS 关机、`Z:` 映射盘未连接、网络盘 SMB 超时 ——
/// 而那不是「文件没了」，只是「现在看不到」。拿它当删除依据就是**永久丢数据**：
/// 这些条目会被删掉，随后的落盘把删减后的表写回去，等介质接回来时位置已经找不回。
///
/// 所以调用方（`App::load_resume`）**不调它**，只靠 `prune` 的条数上限控制
/// 文件大小。留在这里是因为这个判断本身没错，错的是把它用在哪。
///
/// 附带一个**性能**问题：`is_file()` 是同步 IO，放在启动路径（消息循环还没
/// 开始）上，几百条记录全在网络盘时会一格一格等 SMB 超时，启动窗口能冻住
/// 几十秒。
pub fn drop_missing(map: &mut HashMap<PathBuf, f64>) {
    map.retain(|p, _| p.is_file());
}

/// 位置读出来之后该怎么用。
///
/// 返回 `None` 的三种情况都是「不该跳」：没记录、记录太靠前（片头，
/// 跳过去等于没跳）、记录超过时长（文件被换成了短的，或者时长读错了）。
///
/// 最后一跳特别重要：直接 `seek` 到一个超出时长的位置，mpv 会跳到片尾，
/// 用户看到的是「一打开就播完了」。
pub fn resume_to(stored: Option<f64>, duration: Option<f64>) -> Option<f64> {
    let s = stored?;
    if !s.is_finite() || s < MIN_SECONDS {
        return None;
    }
    if let Some(d) = duration {
        if !d.is_finite() || d <= 0.0 {
            return None;
        }
        // 留 5 秒余量：跳到「离片尾不到 5 秒」的地方不如从头放
        if s >= d - 5.0 {
            return None;
        }
    }
    Some(s)
}

/// 拼出「已从上次的位置继续」这类提示里的时间。
///
/// 单独抽出来是因为格式化在两个地方都要用（提示与菜单），而两处不一致的话
/// 用户会看到「1:05:03」和「1小时5分3秒」两种写法。
pub fn format_position(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0:00".to_string();
    }
    let total = secs.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// 把 seek 的落点夹到合法范围。
///
/// **时长未知（`0`）时不能拿它当上界。** 这是 0.7.0 开发时踩的一个坑：
/// 续播是在 `FileLoaded` 那一刻发起的，而那一刻容器才刚读出来、
/// `state.duration` 还是 `0`。原来的写法是
/// `seconds.clamp(0.0, duration.max(0.0))`，于是「跳到 30 秒」被夹成
/// 「跳到 0 秒」—— 而且**一路不报错**：mpv 收到的是一次合法的
/// `seek 0`，`seek` 本身返回 `Ok`。
///
/// 现象是提示写着「已从上次的位置继续 0:30」，画面却从头开始播，
/// 界面上没有任何异常。所以时长未知时只保下界，上界交给 mpv ——
/// 它知道真实时长，超了它自己会夹。
pub fn clamp_seek_target(seconds: f64, duration: f64) -> f64 {
    // NaN 单独挡：它一路传到 `seek` 会变成 "seek NaN"，mpv 忽略还是报错
    // 都不确定，而「从头开始」是唯一能猜得对的结果。
    //
    // ±∞ 不挡：时长已知时 `clamp` 自然把它夹到两端，时长未知时让它原样
    // 过去 —— mpv 知道真实时长，它自己会夹。
    if seconds.is_nan() {
        return 0.0;
    }
    if duration.is_finite() && duration > 0.0 {
        return seconds.clamp(0.0, duration);
    }
    seconds.max(0.0)
}

/// 记忆文件放在 `dir` 底下，文件名 `resume.txt`。
///
/// **`dir` 必须已经是应用自己的数据目录**（`crashlog::appdata_dir()`
/// 给的就是 `%LOCALAPPDATA%\VideoView`）。这个函数**不再**自己拼一层
/// `VideoView` —— 之前两处都拼，拼出了
/// `…\Local\VideoView\VideoView\resume.txt`，`fs::write` 静默返回
/// `NotFound`（`let _ =` 吃掉了），于是「记忆功能完全没生效」，
/// 而且**界面上看不出任何异常**。
///
/// 这就是为什么它不叫 `default_path`：那个名字会让人以为传
/// `%LOCALAPPDATA%` 进来也行。
pub fn file_in(dir: &Path) -> PathBuf {
    dir.join("resume.txt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn 空文件解析出空表() {
        assert!(parse("").is_empty());
        assert!(parse("\n\n\n").is_empty());
    }

    #[test]
    fn 一条往返() {
        let mut m = HashMap::new();
        m.insert(p(r"d:\v\a.mp4"), 123.0);
        let text = serialize(&m);
        let back = parse(&text);
        assert_eq!(back.get(&p(r"d:\v\a.mp4")), Some(&123.0));
    }

    #[test]
    fn 多条往返() {
        let mut m = HashMap::new();
        for (i, name) in ["a.mp4", "b.mkv", "c.webm"].iter().enumerate() {
            m.insert(p(name), (i as f64) * 10.0 + 1.0);
        }
        let back = parse(&serialize(&m));
        assert_eq!(back.len(), 3);
        for (i, name) in ["a.mp4", "b.mkv", "c.webm"].iter().enumerate() {
            assert_eq!(back.get(&p(name)), Some(&((i as f64) * 10.0 + 1.0)));
        }
    }

    /// 路径里**可以**有换行 —— Windows 允许文件名含 `\n`，而按行切分的
    /// 格式在这里会把它切成两半，得到两个都不存在的路径。
    #[test]
    fn 路径里有换行也认得出来() {
        let mut m = HashMap::new();
        let weird = p("d:\\v\\we\nird.mp4");
        m.insert(weird.clone(), 42.0);
        let text = serialize(&m);
        let back = parse(&text);
        assert_eq!(back.get(&weird), Some(&42.0));
        assert_eq!(back.len(), 1);
    }

    #[test]
    fn 路径里有空格没问题() {
        let mut m = HashMap::new();
        let spaced = p("d:\\my videos\\a b c.mp4");
        m.insert(spaced.clone(), 7.0);
        let back = parse(&serialize(&m));
        assert_eq!(back.get(&spaced), Some(&7.0));
    }

    #[test]
    fn 路径里有中文没问题() {
        let mut m = HashMap::new();
        let cn = p("d:\\视频\\第一集.mp4");
        m.insert(cn.clone(), 99.0);
        let back = parse(&serialize(&m));
        assert_eq!(back.get(&cn), Some(&99.0));
    }

    #[test]
    fn 坏条目在中间也不影响后面的() {
        // 这一条钉的是「跳过坏条目、继续往后读」。
        //
        // **坏数据必须放在中间。** 第一版把坏数据全 `push_str` 在末尾、
        // 好数据放前面，于是无论实现是「逐条跳过」还是「一坏就放弃整个
        // 文件」，这个测试都过 —— 它证明不了任何东西。
        //
        // 用的两种损坏都是**行内**的（行数没变，所以按行重新同步之后
        // 落点仍然在记录边界上）：
        //   * `"12 x\n"`  —— 秒数不是数字
        //   * `"99 5\nshort\n"` —— 声称路径 99 字节，文件里没有那么多
        // 长度字段被改**小**的那种（解析出一个合法但错误的路径）救不回来，
        // 那需要校验和，见 `resync` 的说明。
        let mut text = String::new();
        text.push_str("5 5\na.mp4\n");
        text.push_str("12 x\n");
        text.push_str("99 5\nshort\n");
        text.push_str("5 11\nb.mp4\n");

        let map = parse(&text);
        assert_eq!(
            map.get(Path::new("a.mp4")),
            Some(&5.0),
            "坏数据之前的好数据要保住"
        );
        assert_eq!(
            map.get(Path::new("b.mp4")),
            Some(&11.0),
            "坏数据**之后**的好数据也要保住 —— 实现成「一坏就放弃整个文件」的话这里会红"
        );
    }

    /// 长度字段溢出不能崩。
    ///
    /// `n` 是从文件里读出来的十进制数，而 `usize::MAX` 是合法的十进制输入。
    /// 早期版本写的是 `path_start + n`，release 下**回绕**成小数，越界检查
    /// 拦不住，接着 `&bytes[path_start..path_end]`（`path_end < path_start`）
    /// 直接 panic —— 「用户目录里的纯文本被改坏一个字节」就能让播放器起不来，
    /// 正好违反这个模块开头「坏了不该拦住用户看片」的承诺。
    #[test]
    fn 长度字段溢出不会崩() {
        for t in [
            "18446744073709551615 5\nx\n",       // usize::MAX
            "99999999999999999999999999 5\nx\n", // 超出 usize，parse 直接失败
            "18446744073709551610 5\nx\n",       // 加起来正好回绕到 path_start 附近
        ] {
            let map = parse(t);
            assert!(
                map.is_empty(),
                "{t:?} 应当解析出空表，而不是 panic 或者凭空给出条目"
            );
        }
    }

    /// 「跳过一格继续」这个策略最怕的就是推进不了下标（死循环）。
    /// `parse` 收 `&str`，所以整个缓冲区一定已经是合法 UTF-8 —— 能让
    /// `from_utf8` 失败的只有一种情况：**长度字段被改小了**，切出来的字节
    /// 片段落在某个多字节字符中间。
    ///
    /// 这一条以前**测不到**：第一版用 `from_utf8_lossy` 造非法字节，而那个
    /// 转换出来的替换字符本身是合法 UTF-8，于是 `from_utf8` 成功、断言看着
    /// 像过了，其实验的是别的东西。这里改成「声明一个切在字符中间的长度」。
    ///
    /// 而「漏掉尾随换行」那个 bug 就藏在这里：`skip_record` 的落点会停在
    /// 半个字符上，后面每一轮都从错的位置解析。
    #[test]
    fn 路径切在多字节字符中间不影响后面的条目() {
        // 「中」是 3 个 UTF-8 字节。长度字段写 1 = 只切出半个字符。
        // 后两条的长度前缀是 **5**（`c.mp4` / `d.mp4` 各 5 个字节，
        // 不含结尾那个换行）—— 写成 6 会解析出 `c.mp4\n`，而那也是一个
        // 「合法的路径」，于是断言会以一个莫名其妙的形式失败。
        let mut text = String::new();
        text.push_str("1 5\n");
        text.push_str("中.mp4\n");
        text.push_str("5 9\nc.mp4\n");
        text.push_str("5 3\nd.mp4\n");

        let map = parse(&text);
        assert!(
            !map.keys().any(|k| k.to_string_lossy().contains('\u{FFFD}')),
            "不该把半个字符当成路径记下来：{:?}",
            map.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            map.get(Path::new("c.mp4")),
            Some(&9.0),
            "坏条目之后的条目要保住"
        );
        assert_eq!(map.get(Path::new("d.mp4")), Some(&3.0));
    }

    /// 「跳过一格继续」这个策略最怕的就是推进不了下标（死循环）。
    /// 每个用例都跑一遍，确保 `parse` 一定返回。
    #[test]
    fn 全是坏数据不会死循环() {
        for t in [
            " ",
            "\n",
            "0 \n",
            "x",
            "x x\nx\n",
            "1 1\n\u{FFFD}",
            "999999999999999999999999999999999999999999 1\nz\n",
        ] {
            let map = parse(t);
            assert!(map.len() <= 1, "{t:?} 解析出了意外多的条目");
        }
    }
    #[test]
    fn 裁剪到上限() {
        let mut m = HashMap::new();
        for i in 0..600 {
            m.insert(p(&format!("f{i}.mp4")), i as f64 + 10.0);
        }
        prune(&mut m, MAX_ENTRIES);
        assert_eq!(m.len(), MAX_ENTRIES);
    }

    #[test]
    fn 裁剪是稳定的_同一份数据丢同样那些() {
        let build = || {
            let mut m = HashMap::new();
            for i in 0..600 {
                m.insert(p(&format!("f{i}.mp4")), i as f64 + 10.0);
            }
            m
        };
        let mut a = build();
        let mut b = build();
        prune(&mut a, MAX_ENTRIES);
        prune(&mut b, MAX_ENTRIES);
        let mut ka: Vec<_> = a.keys().cloned().collect();
        let mut kb: Vec<_> = b.keys().cloned().collect();
        ka.sort();
        kb.sort();
        assert_eq!(
            ka, kb,
            "两次裁剪必须丢掉同一批，否则用户会觉得「随机有集记不住」"
        );
    }

    #[test]
    fn 不超过上限时不动() {
        let mut m = HashMap::from([(p("a.mp4"), 1.0)]);
        prune(&mut m, MAX_ENTRIES);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn 不存在的文件被清掉() {
        let mut m = HashMap::new();
        // Cargo.toml 在当前目录相对路径下大概率存在；用一个绝对的不存在路径
        m.insert(p(r"z:\definitely\not\here.mp4"), 10.0);
        drop_missing(&mut m);
        assert!(m.is_empty());
    }

    #[test]
    fn 时间格式化() {
        assert_eq!(format_position(0.0), "0:00");
        assert_eq!(format_position(9.0), "0:09");
        assert_eq!(format_position(65.0), "1:05");
        assert_eq!(format_position(605.0), "10:05");
        // 超过一小时才带小时段
        assert_eq!(format_position(3661.0), "1:01:01");
        // 四舍五入而不是截断：9.6 秒应当显示 0:10
        assert_eq!(format_position(9.6), "0:10");
        // 非法值不能变成「NaN:NaN」
        assert_eq!(format_position(f64::NAN), "0:00");
        assert_eq!(format_position(-1.0), "0:00");
    }

    #[test]
    fn 时长未知时不能拿零当上界() {
        // 这就是那个静默失效：`FileLoaded` 那一刻 duration 就是 0，
        // 原来的 clamp 会把 30 秒夹成 0。
        assert_eq!(clamp_seek_target(30.0, 0.0), 30.0);
        assert_eq!(clamp_seek_target(0.5, 0.0), 0.5);
        // NaN 时长也不能当上界
        assert_eq!(clamp_seek_target(30.0, f64::NAN), 30.0);
        // 负时长同样
        assert_eq!(clamp_seek_target(30.0, -1.0), 30.0);
    }

    #[test]
    fn 时长已知时照常夹() {
        assert_eq!(clamp_seek_target(30.0, 60.0), 30.0);
        assert_eq!(clamp_seek_target(90.0, 60.0), 60.0);
        assert_eq!(clamp_seek_target(-5.0, 60.0), 0.0);
    }

    #[test]
    fn 非法输入归零而不是传播() {
        // `clamp` 遇到 NaN 会返回 NaN（f64::clamp 自己会 panic 的版本
        // 更糟），而 NaN 一路传到 `seek` 命令里会变成 "seek NaN"，
        // mpv 报错还是忽略都不确定 —— 干脆在这里挡住。
        assert_eq!(clamp_seek_target(f64::NAN, 60.0), 0.0);
        assert_eq!(clamp_seek_target(f64::NAN, 0.0), 0.0);
        assert_eq!(clamp_seek_target(f64::INFINITY, 60.0), 60.0);
        assert_eq!(
            clamp_seek_target(f64::INFINITY, 0.0),
            f64::INFINITY.max(0.0)
        );
    }

    #[test]
    fn 记忆文件只拼一层_video_view() {
        // 这个测试存在的唯一理由是防**双拼**：
        // `crashlog::appdata_dir()` 已经拼过一层 `VideoView`，而
        // `file_in` 早期版本还拼了第二层，写盘静默 `NotFound`。
        //
        // 断言的是**调用方拼出来的完整路径**，而不是某一个函数的输出 ——
        // 单测 `file_in` 自己抓不到双拼，因为错在两个函数的组合上。
        let dir = crate::crashlog::appdata_dir().expect("拿不到 %LOCALAPPDATA%\\VideoView");
        let full = file_in(&dir);
        let text = full.to_string_lossy();
        assert!(
            text.ends_with(r"VideoView\resume.txt"),
            "路径应当是 …\\VideoView\\resume.txt，实际是 {text}"
        );
        assert_eq!(
            text.matches("VideoView").count(),
            1,
            "VideoView 出现了 {0} 次（路径 {1}）",
            text.matches("VideoView").count(),
            text
        );
        // 目录必须是真的存在 —— `file_in` 只拼名字，建目录是
        // `appdata_dir` 的活，而双拼会让写盘必然失败
        assert!(dir.is_dir(), "{} 不存在", dir.display());
        assert!(dir.ends_with("VideoView"));
    }

    #[test]
    fn 记忆文件能在真实目录里往返() {
        // 上面那条验路径形状，这条验「真写得出、真读得回」。
        // 用 `file_in` 写进 `appdata_dir`，中间夹一个别的名字以免
        // 碰到正在用的 resume.txt。
        let Some(dir) = crate::crashlog::appdata_dir() else {
            return;
        };
        let path = dir.join("resume-selftest.txt");
        let mut m = HashMap::new();
        m.insert(p(r"d:\v\a.mp4"), 12.0);
        assert!(
            std::fs::write(&path, serialize(&m)).is_ok(),
            "写不了 {path:?}"
        );
        let back = parse(&std::fs::read_to_string(&path).unwrap_or_default());
        assert_eq!(back.get(&p(r"d:\v\a.mp4")), Some(&12.0));
        let _ = std::fs::remove_file(&path);
    }
}
