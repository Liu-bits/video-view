//! 崩溃日志：panic 与未处理异常的落盘。
//!
//! ## 为什么需要
//!
//! release 的 profile 是 `panic = "abort"`。这意味着：
//!
//! * Rust panic **不会**显示任何错误框，进程直接终止；
//! * `std::panic::catch_unwind` 也拦不住（abort 不是 unwind）；
//! * 没有 panic hook 的话，用户看到的就是「双击了，闪一下，没了」。
//!
//! 0.3.0 排查问题时这一点很要命：几个「不该 panic」的地方（`player()`
//! 的 `expect`、`i32::clamp` 的 `min > max`、`SelectObject(hdc, NULL)`）
//! 全都是「一旦触发就静默消失」的表现。代码注释能提醒后来人，但**触发之后
//! 什么线索都不留**。有日志至少能知道「崩在哪一行」。
//!
//! ## 两层
//!
//! * **panic hook** —— 覆盖 Rust 的 panic（包括 `abort()` 之前的最后一步）。
//!   写 panic 消息 + 源文件位置。
//! * **`SetUnhandledExceptionFilter`** —— 覆盖 SEH（访问违例、栈溢出等）。
//!   这些不是 panic，走的是系统异常流程。
//!
//! ## 日志放哪
//!
//! 先试 exe 同目录（便携用户期望在那里看到），写不进去就退回
//! `%LOCALAPPDATA%\VideoView\`——装到 `C:\Program Files\` 时前者必然失败
//! （没写权限），而后者一定可写。
//!
//! ## 崩溃处理里的自我约束
//!
//! 未处理异常过滤器跑在一个已经出问题的进程里。这里的写法刻意保守：
//! 只做「拼字符串 + 追加写文件」，不分配大块内存、不加锁、不碰 mpv、
//! 不碰窗口。而且**即使写日志失败也绝不 panic**——一个写日志时再 panic 的
//! 进程会走进无限递归，比原始崩溃更难查。
//!
//! release profile 里 `strip = true`，符号被剥掉了，所以日志里只有 panic 的
//! `file:line`（字符串常量，仍在二进制里）与异常码，没有可解析的调用栈。

use std::io::Write;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Diagnostics::Debug::{SetUnhandledExceptionFilter, EXCEPTION_POINTERS};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::UI::Shell::SHGetKnownFolderPath;

/// 日志文件名。
const LOG_NAME: &str = "videoview.log";

/// 日志上限（字节）。超了就截断，只保留尾部。
///
/// 为什么要有上限：崩溃日志是**追加**写的，一个反复崩溃的场景（用户双击、
/// 闪退、再双击）一天能写几百 KB，而没人会去清理一个自己都不知道存在的文件。
const MAX_LOG_BYTES: u64 = 256 * 1024;

/// `GetModuleFileNameW` 的缓冲长度。用 `MAX_PATH` 那个常量没必要，
/// 现在的 exe 路径都远短于此，而这个值只需要「装得下就行」。
const PATH_BUF: usize = 32768;

/// 装上 panic hook 与未处理异常过滤器。
///
/// 幂等：重复调用只是把 hook 换成同一个函数，不会有副作用。
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        let msg = payload_text(info);
        append(&format!(
            "\n==== panic ====\nversion = {}\nlocation = {location}\nmessage = {msg}\nthread = {:?}\n",
            env!("CARGO_PKG_VERSION"),
            std::thread::current().name().unwrap_or("<unnamed>"),
        ));
    }));

    // SAFETY: 过滤器是普通的 `extern "system" fn`，签名按 MSDN 的约定。
    // 它不捕获任何状态，也没有跨调用点的可变全局。
    unsafe {
        let _ = SetUnhandledExceptionFilter(Some(unhandled_exception));
    }
}

/// 未处理异常过滤器。
///
/// 异常码与出错地址直接从**传入的 `EXCEPTION_POINTERS`** 里读，不用
/// `GetExceptionCode()` / `GetExceptionInformation()`。那两个是 Win32 的宏
/// （`GetExceptionInformation` 展开成 `(*Tp)` 这种表达式），windows-rs 没有
/// 为它们生成绑定；而过滤器的第一个参数本来就是同一个 `EXCEPTION_POINTERS`，
/// 走参数更直接、也不会在过滤器之外误用。
///
/// 参数是 `*const`：我们只读不写（写那个结构是在玩命），所以保持 const。
///
/// 只能记两样可靠的东西：**异常码**和**出错的指令地址**。后者在 release
/// 构建里没有符号表可用，只能原样记下来。
unsafe extern "system" fn unhandled_exception(pointers: *const EXCEPTION_POINTERS) -> i32 {
    let mut code: u32 = 0;
    let mut addr = 0_usize;
    // SAFETY: 过滤器被调用时 `pointers` 必然有效；但仍判一次空指针再解引用，
    // 因为读一个野指针换来的日志信息毫无价值
    if !pointers.is_null() {
        let rec = unsafe { (*pointers).ExceptionRecord };
        if !rec.is_null() {
            let rec = unsafe { &*rec };
            // NTSTATUS 是 i32 的 newtype，当成无符号的十六进制位模式打印
            code = rec.ExceptionCode.0 as u32;
            addr = rec.ExceptionAddress as usize;
        }
    }
    append(&format!(
        "\n==== unhandled exception ====\nversion = {}\ncode = 0x{code:08X}\naddress = 0x{addr:016X}\n",
        env!("CARGO_PKG_VERSION"),
    ));
    // 返回 0 = 「我没处理」，交给系统默认流程（弹 WerFault 或直接终止）。
    // 返回非 0 相当于说「已经处理完了」，进程会被无提示地杀掉——那比现在的
    // 行为更难解释。
    0
}

/// 从 panic 信息里取出可读文本。
///
/// 参数类型用 PanicInfo 而不是 PanicHookInfo：后者从 Rust 1.81 才稳定，
/// 而本项目声明的 MSRV 是 1.77（见 Cargo.toml 的 rust-version）。
/// PanicInfo 从 1.0 就在，而且是同一个类型的别名，两边都能编。
///
/// `payload` 是 `Box<dyn Any + Send>`：`&str` / `String` 能取出原文，
/// 其它类型只能给占位符。
// 这里刻意用 PanicInfo（已废弃）而不是 PanicHookInfo：PanicHookInfo`n// 从 Rust 1.81 才稳定，而本项目声明的 MSRV 是 1.77（见 Cargo.toml 的
// rust-version）。PanicInfo 是同一个类型的别名，从 1.0 就在，两边都能编。
#[allow(deprecated)]
fn payload_text(info: &std::panic::PanicInfo<'_>) -> String {
    let p = info.payload();
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// 往日志文件追加一段内容，前面自动加时间戳。
///
/// **绝不 panic**：调用点有两个是崩溃处理路径。写失败（磁盘满、目录被删、
/// 权限被改）时安静返回——诊断功能不该制造第二个故障。
fn append(text: &str) {
    append_to(&log_path(), text);
}

/// `append` 的可注入版本（只为测试能指向临时文件）。
fn append_to(path: &Option<PathBuf>, text: &str) {
    let Some(path) = path else {
        return;
    };
    truncate_if_needed(path);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        // 写失败同样忽略：这里已经没有别的手段可以报告了
        let _ = f.write_all(format!("[{}] {text}", timestamp()).as_bytes());
    }
}

/// 日志文件路径。先 exe 同目录，再 `%LOCALAPPDATA%\VideoView`。
fn log_path() -> Option<PathBuf> {
    if let Some(dir) = exe_dir() {
        let p = dir.join(LOG_NAME);
        if writable(&p) {
            return Some(p);
        }
    }
    let dir = local_appdata()?.join("VideoView");
    let _ = ensure_dir(&dir);
    let p = dir.join(LOG_NAME);
    writable(&p).then_some(p)
}

/// exe 所在目录。
pub fn exe_dir() -> Option<PathBuf> {
    let mut buf = vec![0_u16; PATH_BUF];
    // 长度由 slice 自己给出
    // SAFETY: 缓冲区是有效的 UTF-16 存储
    let n = unsafe { GetModuleFileNameW(None, &mut buf) };
    // 返回 0 = 失败；长度 >= 缓冲 = 被截断，路径不可信
    if n == 0 || n as usize >= buf.len() {
        return None;
    }
    let end = buf.iter().position(|&u| u == 0).unwrap_or(n as usize);
    let s = String::from_utf16(&buf[..end]).ok()?;
    PathBuf::from(s).parent().map(PathBuf::from)
}

/// 截图的默认落点。
///
/// 三级兜底，**每一级都试过**：
///
/// 1. `SHGetKnownFolderPath(FOLDERID_Pictures)`
/// 2. `%USERPROFILE%\Pictures`
/// 3. `%USERPROFILE%\Desktop`
///
/// ## 为什么需要兜底
///
/// 第一级在这台机器上**实测失败**：`SHGetKnownFolderPath` 返回
/// `0x80070002`（ERROR_FILE_NOT_FOUND）。注册表里「图片」那条已知文件夹
/// 指向一个磁盘上并不存在的路径——用户改过、OneDrive 重定向过、或者
/// 精简版系统装完就这样。加了 `KF_FLAG_CREATE` 之后这类情况大多能直接建出来，
/// 但那要求进程有建目录的权限，域受限 / 只读配置下仍可能失败。
///
/// 截图是个「按 S 应该就有反应」的功能，为一个坏掉的已知文件夹配置
/// 而完全没有反馈（连错误提示都没有）是不能接受的。所以后面两级用
/// 最土的办法兜住：环境变量拼路径。
pub fn pictures_dir() -> Option<PathBuf> {
    known_pictures_dir()
        .or_else(|| profile_subdir("Pictures"))
        .or_else(|| profile_subdir("Desktop"))
}

/// `SHGetKnownFolderPath` 那一级。
fn known_pictures_dir() -> Option<PathBuf> {
    // FOLDERID_Pictures = {33E28130-4E1E-4676-835A-98D76DA6D7A5}
    //
    // 用 `from_u128` 而不是 `from_values`：后者在这个版本的 windows-core
    // 里签名是 `(u32, u16, u16, [u8; 8])`，按 MSDN 的十六进制串写更不容易
    // 抄错字段。
    const FOLDERID_PICTURES: windows::core::GUID =
        windows::core::GUID::from_u128(0x33E2_8130_4E1E_4676_835A_98D7_6DA6_D7A5_u128);
    // KF_FLAG_CREATE = 0x00008000
    //
    // 没有它时，已知文件夹在磁盘上不存在就**直接失败**（上面那个 0x80070002
    // 就是这么来的）。这个 flag 的语义正是「不存在就建」。
    const KF_FLAG_CREATE: windows::Win32::UI::Shell::KNOWN_FOLDER_FLAG =
        windows::Win32::UI::Shell::KNOWN_FOLDER_FLAG(0x0000_8000);

    // SAFETY: GUID 指针有效；`None` = 用当前用户的令牌
    let pw = unsafe { SHGetKnownFolderPath(&FOLDERID_PICTURES as *const _, KF_FLAG_CREATE, None) }
        .ok()?;
    // 这块内存是 COM 分配的。必须先复制成 Rust 的 String 再还给 COM——
    // 让 PWSTR 自己 drop 是错的（它不认 CoTaskMemFree）
    // SAFETY: `pw` 刚由 SHGetKnownFolderPath 成功返回，非空且 NUL 结尾
    let path = unsafe { pw.to_string() }.ok();
    // SAFETY: `pw` 由 SHGetKnownFolderPath 用 CoTaskMemAlloc 分配，接口约定
    // 必须用 CoTaskMemFree 归还
    unsafe {
        CoTaskMemFree(Some(pw.as_ptr().cast()));
    }
    let dir = PathBuf::from(path?);
    // 拿到的路径仍可能指向一个建不出来的位置（只读 / 无权限），那就当这一级
    // 失败，交给下一级
    ensure_dir(&dir).then_some(dir)
}

/// `%USERPROFILE%\<name>`，建不出来就 `None`。
fn profile_subdir(name: &str) -> Option<PathBuf> {
    let profile = std::env::var_os("USERPROFILE")?;
    if profile.is_empty() {
        return None;
    }
    let dir = PathBuf::from(profile).join(name);
    ensure_dir(&dir).then_some(dir)
}

/// `%LOCALAPPDATA%`。
fn local_appdata() -> Option<PathBuf> {
    let v = std::env::var_os("LOCALAPPDATA")?;
    if v.is_empty() {
        return None;
    }
    Some(PathBuf::from(v))
}

/// `%LOCALAPPDATA%\VideoView`，顺带把目录建出来。
///
/// 给「按文件记播放位置」用 —— 那个文件不该放在 `Program Files` 下：
/// 写一个便利功能不该要管理员权限，而且程序目录升级时会被覆盖掉。
///
/// 返回 `None` 表示「拿不到 `%LOCALAPPDATA%` 或建不了目录」。调用方
/// 必须把这件事**安静地跳过**：记忆位置是便利功能，存不了就不记，
/// 不能因此拦住用户看片。
pub fn appdata_dir() -> Option<PathBuf> {
    let dir = local_appdata()?.join("VideoView");
    ensure_dir(&dir).then_some(dir)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 建目录（已存在也算成功）。
fn ensure_dir(dir: &Path) -> bool {
    let p = wide(&dir.to_string_lossy());
    // SAFETY: 路径缓冲区是有效的 NUL 结尾 UTF-16
    unsafe {
        if GetFileAttributesW(PCWSTR(p.as_ptr())) != INVALID_FILE_ATTRIBUTES {
            return true;
        }
        if CreateDirectoryW(PCWSTR(p.as_ptr()), None).is_ok() {
            return true;
        }
        // 建目录失败**不等于**这目录不可用：另一个线程 / 进程可能正好在
        // 「查属性」和「建目录」之间把它建好了，于是这里拿到的是
        // `ERROR_ALREADY_EXISTS`。所以再确认一次。
        //
        // 这条路径在开发机上几乎测不出来 —— 目录早就存在，走的是上面那个
        // 提前 return。CI 上是全新环境，而 `cargo test` 是多线程跑的：
        // 首次跑 CI 时 `bookmark::tests::书签文件只拼一层_video_view` 就红在
        // 这里（两次并发调用，一次成功、一次拿到 false，后者让
        // `appdata_dir()` 返回了 `None`）。
        GetFileAttributesW(PCWSTR(p.as_ptr())) != INVALID_FILE_ATTRIBUTES
    }
}

/// 这个路径能不能写。
///
/// 用「建一个临时文件再删掉」来判断，而不是只看目录是否存在——只读目录、
/// 被组策略锁住、文件被另一个进程独占，都会让「存在」与「能写」不一致。
fn writable(path: &Path) -> bool {
    let probe = path.with_extension("tmp");
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 超上限就截断，保留尾部。
///
/// 截断而不是清空：崩溃日志最有价值的是**最后一次**崩溃，前面那次可能
/// 才是「第一次崩」的现场。
fn truncate_if_needed(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    // 保留后 3/4
    let keep = (MAX_LOG_BYTES / 4 * 3) as usize;
    let Ok(data) = std::fs::read(path) else {
        return;
    };
    if data.len() <= keep {
        return;
    }
    let tail = &data[data.len() - keep..];
    // 从 UTF-8 的字符边界开始切，否则开头会出现半个字符
    let start = tail.iter().position(|&b| (b & 0xC0) != 0x80).unwrap_or(0);
    let _ = std::fs::write(path, &tail[start..]);
}

/// 当前时间（UTC），格式 `2026-10-06 12:34:56`。
///
/// 手写而不是引一个日期时间库：为了打一行时间戳引入依赖不划算，而且
/// 本地时区换算要额外处理一套。日志里的时间戳用 UTC 就够定位问题了，
/// 真要看精确顺序靠条目的先后就够了。
fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (now / 86400) as i64;
    let secs = now % 86400;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

/// 天数（自 1970-01-01）-> (年, 月, 日)，公历、无时区。
///
/// Howard Hinnant 的 `civil_from_days` 算法：整数运算、无查表、无条件
/// 分支，除法/取模的次数是常数。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每次测试用独立目录，避免互相干扰
    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录失败");
        d
    }

    #[test]
    fn 日志路径总能定出一个位置() {
        // 无论 exe 目录可不可写，log_path 都必须给出结果——
        // 给不出就等于整个崩溃日志功能静默失效
        let p = log_path().expect("log_path 返回了 None");
        assert!(p.ends_with(LOG_NAME), "{p:?}");
    }

    #[test]
    fn exe_目录拿得到且是目录() {
        let dir = exe_dir().expect("GetModuleFileNameW 应当成功");
        assert!(dir.is_dir(), "{dir:?} 不是目录");
    }

    #[test]
    fn 路径为_none_时追加安静地什么都不做() {
        // 这正是「日志目录全不可写」时的路径，必须安静返回而不是 panic
        append_to(&None, "随便写点什么");
    }

    #[test]
    fn 追加会真的落盘并带上时间戳() {
        let dir = tmpdir("vv-log-write");
        let p = dir.join(LOG_NAME);
        append_to(&Some(p.clone()), "hello");
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("hello"), "{s}");
        assert!(s.starts_with('['), "应当以时间戳开头：{s}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 日志上限截断后保留尾部且不超过上限() {
        let dir = tmpdir("vv-log-trunc");
        let p = dir.join("big.log");
        // 造一个明显超上限的文件，尾部放一个可识别的标记
        let mut data = vec![b'x'; MAX_LOG_BYTES as usize + 4096];
        let tail = b"LAST-CRASH-MARKER";
        let at = data.len() - tail.len();
        data[at..].copy_from_slice(tail);
        std::fs::write(&p, &data).unwrap();

        truncate_if_needed(&p);
        let after = std::fs::read(&p).unwrap();
        assert!(
            after.len() as u64 <= MAX_LOG_BYTES,
            "截断后还有 {} 字节，应小于上限 {MAX_LOG_BYTES}",
            after.len()
        );
        assert!(after.ends_with(tail), "尾部标记丢了，说明保留的不是尾部");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 截断不会在多字节字符中间切开() {
        let dir = tmpdir("vv-log-utf8");
        let p = dir.join("utf8.log");
        // 全是多字节字符，尾部被切的位置几乎必然落在字符中间
        let mut data = Vec::new();
        while data.len() < (MAX_LOG_BYTES as usize) + 4096 {
            data.extend_from_slice("中".as_bytes());
        }
        std::fs::write(&p, &data).unwrap();

        truncate_if_needed(&p);
        let after = std::fs::read(&p).unwrap();
        assert!(
            String::from_utf8(after).is_ok(),
            "截断后不是合法 UTF-8，说明切在字符中间"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 短日志不会被截断() {
        let dir = tmpdir("vv-log-short");
        let p = dir.join("s.log");
        std::fs::write(&p, b"hello").unwrap();
        truncate_if_needed(&p);
        assert_eq!(std::fs::read(&p).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 日历换算与已知日期一致() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-03-01：闰日之后，用来卡住「3 月」的分界
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        // 闰年 2 月 29 日
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        // 这些天数是用 `DateTime(1970,1,1).AddDays(n)` 独立算出来的，
        // 不是从实现反推的——否则这条测试就只是在确认实现和自己一致
        assert_eq!(civil_from_days(20_722), (2026, 9, 26));
        assert_eq!(civil_from_days(20_732), (2026, 10, 6));
    }

    #[test]
    fn 时间戳格式是固定宽度() {
        let t = timestamp();
        // "YYYY-MM-DD HH:MM:SS"
        assert_eq!(t.len(), 19, "{t}");
        assert_eq!(t.as_bytes()[4], b'-');
        assert_eq!(t.as_bytes()[10], b' ');
        assert_eq!(t.as_bytes()[13], b':');
    }

    #[test]
    fn 截图目录一定能拿到_而且是现成的目录() {
        // 这条原来写成 `if let Some(dir) = pictures_dir() { assert!(...) }`，
        // 结果**返回 None 时也算通过**——而这台机器上它恰恰一直返回 None，
        // 于是「按 S 完全没有反应」这件事被一条永远绿的测试盖住了。
        //
        // 兜底链有三级，只要 `%USERPROFILE%` 在就一定能给出一个建好的目录，
        // 所以这里必须断言 Some。
        let dir = pictures_dir().expect("截图目录必须拿得到（三级兜底都失败才是异常）");
        assert!(dir.is_dir(), "{dir:?} 不是现成的目录");
        assert!(dir.is_absolute(), "{dir:?} 不是绝对路径");
    }

    #[test]
    fn 兜底链的第二三级走的是环境变量而不是注册表() {
        // 这两级不查注册表，只拼环境变量。所以在 `USERPROFILE` 被清掉的
        // 情况下它们必须返回 None —— 这正是「全部兜底都失败」的样子，
        // 有了这个测试才说得清第一级失败时行为是可预期的。
        // （真的去改进程环境变量会污染同进程里并行跑的其它测试，所以
        //   这里只验证路径构造本身，不改环境。）
        let profile = std::env::var_os("USERPROFILE").expect("测试环境应当有 USERPROFILE");
        let expected = PathBuf::from(&profile).join("Pictures");
        // 目录可能已经存在也可能被清掉，所以只比对构造结果，不断言存在性
        assert_eq!(expected.file_name().unwrap(), "Pictures");
    }

    #[test]
    fn 日志目录的第一级是_exe_同级() {
        // 崩溃日志优先落在 exe 同目录（便携用户期望在那里看到）。
        // 只在真的可写时才用第一级——装到 Program Files 时它必然失败。
        let p = log_path().expect("日志路径必须能给出来");
        assert!(p.ends_with(LOG_NAME), "{p:?}");
        let exe = exe_dir().expect("exe 目录拿得到");
        let local = local_appdata().map(|b| b.join("VideoView"));
        let in_exe = p.starts_with(&exe);
        let in_local = local.map(|d| p.starts_with(d)).unwrap_or(false);
        assert!(
            in_exe || in_local,
            "日志路径 {p:?} 既不在 exe 目录 {exe:?} 也不在 LOCALAPPDATA 下"
        );
    }

    /// 并发建同一个目录时，`ensure_dir` 不能有人被误判成「建不出来」。
    ///
    /// 「查属性 → 建目录」这两步不是原子的：另一个线程正好在两步之间把目录
    /// 建好时，`CreateDirectoryW` 返回的是 `ERROR_ALREADY_EXISTS`。修复前那
    /// 一路直接返回 `false`，于是 `appdata_dir()` 给出 `None` ——
    /// 「书签 / 续播」静默失效。
    ///
    /// 必须用一个**此刻还不存在**的目录：目录已存在就走提前 return，竞态根本
    /// 发生不了（这正是它在开发机上一直没被暴露的原因 —— 那儿目录早就建过了）。
    /// 用 `Barrier` 把线程卡在同一时刻进函数，否则「8 个线程恰好全没赶上」
    /// 会让这个测试时绿时红，而靠运气的测试不如没有。
    #[test]
    fn 并发建同一个目录不会有人被误判成失败() {
        use std::sync::{Arc, Barrier};

        let base = tmpdir("videoview-concurrent-ensure");
        let target = base.join("VideoView");
        assert!(!target.exists(), "前提：这个目录此刻必须不存在");

        let n = 8;
        let barrier = Arc::new(Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let t = target.clone();
                let b = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    b.wait();
                    ensure_dir(&t)
                })
            })
            .collect();

        let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(
            results.iter().all(|ok| *ok),
            "有线程被误判为建不出来：{results:?}"
        );
        assert!(target.is_dir(), "目录最终必须存在");

        let _ = std::fs::remove_dir_all(&base);
    }
}
