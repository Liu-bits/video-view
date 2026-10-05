//! CPU 拓扑查询。
//!
//! 单独成文件而不是塞进 `mpv` 模块：这里问的是「这台机器是什么样」，
//! 和 mpv、和解码都无关。将来界面层要按核心数决定重绘线程数之类，
//! 也能直接复用。
//!
//! ## 为什么需要「物理核」而不是 `std::thread::available_parallelism()`
//!
//! `available_parallelism()` 在 Windows 上的实现就是
//! `GetSystemInfo().dwNumberOfProcessors` —— **逻辑**处理器数，也就是把
//! 超线程算进去的总数（Rust 1.77 的报错文案里直接写的是
//! 「The number of **hardware threads** is not known」）。
//!
//! 2 物理核 + 4 线程的机器（i3-3xxx 全系）上它返回 4。拿这个数字去开解码
//! 线程，就是**在 2 个物理核上跑 4 个 runnable 线程**：调度器把两个线程塞进
//! 一个执行单元，而软件解码本来就是 ALU-bound，超线程对它几乎没有收益，
//! 代价是每核两个线程各要一份参考帧列表、帧缓冲池元数据，L1/L2 冲突上升。
//! 净效果是更慢，不是更快。

use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, RelationProcessorCore,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

/// 物理核心数。
///
/// 取不到时返回 `None`：API 不存在（Win7 以下）、`VirtualAlloc` 失败、
/// 或者返回的结构里出现了 `Size` 为 0 的条目（那会让 `align_to` 的
/// 步进变成死循环 —— 必须防住）。
///
/// 调用方应当把它当成「锦上添花的输入」：拿不到就退回逻辑核数，
/// 退回值只是比物理核略激进，不影响正确性。
pub fn physical_cores() -> Option<usize> {
    // 第一趟：不带缓冲区，只问要多少字节。
    //
    // 这一趟**必定**返回 `Err(ERROR_INSUFFICIENT_BUFFER)`，那是正常流程，
    // 不能 `?` 直接返回。
    let mut needed: u32 = 0;
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut needed) };
    if needed == 0 {
        return None;
    }

    // 按报告的字节数分配，向上对齐到结构体边界。
    //
    // `vec![0u8; needed]` 的元素是 `u8`（对齐 1），所以 `align_to` 才是
    // 必须的。返回的 Err 记下对齐后的实际长度，用它来遍历。
    let buf = vec![0u8; needed as usize];
    // `align_to`（不是 `try_align_to`）对 `u8` 一定成功——`u8` 的对齐要求是 1，
    // 任何分配都能满足，所以不存在 Err 分支。unsafe 只在「把结果当成另一种
    // 类型的切片」这一步。
    let (ptr, _mid, _tail) = unsafe { buf.align_to::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>() };
    let mut got: u32 = needed;
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(ptr.as_ptr().cast_mut().cast()),
            &mut got,
        )
    }
    .ok()?;

    // 遍历。每个条目带自己的 `Size`，必须按它步进——结构体里有个 union，
    // 不同 Relationship 的实际长度不同，不能用 `size_of` 一把梭。
    let mut count = 0usize;
    let mut offset = 0usize;
    while offset + std::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>() <= buf.len() {
        // 只借用 Relationship / Size 两个字段，不碰 union，避免 union 字段
        // 的对齐与初始化问题。
        let entry = unsafe {
            &*buf
                .as_ptr()
                .add(offset)
                .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>()
        };
        let size = entry.Size as usize;
        if size == 0 {
            // 0 会让 offset 永远不前进，死循环。宁可少算几个核。
            return None;
        }
        if entry.Relationship == RelationProcessorCore {
            count += 1;
        }
        offset += size;
    }
    if count == 0 {
        None
    } else {
        Some(count)
    }
}

/// 解码线程数应该在的取值范围。
///
/// 上限 4：H.264 的帧级/切片级并行在超过 4 之后收益迅速衰减，而每条线程
/// 都要多背一份参考帧列表与帧缓冲池元数据。给 8 核机器开 16 条线程只会更慢。
const MAX_DECODE_THREADS: usize = 4;

/// 给 mpv 的 `vd-lavc-threads` 用的线程数。
///
/// 物理核优先，拿不到就退回逻辑核数，再夹到 `[1, MAX_DECODE_THREADS]`。
///
/// 下限 1 而不是 0：0 在 `vd_lavc.c` 里被解释成「由 libavcodec 自己决定」
/// （等价于不设），而我们在这里要的是一个明确的、不随外部环境漂移的值。
pub fn decode_threads() -> usize {
    physical_cores()
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1)
        .clamp(1, MAX_DECODE_THREADS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 这台机器必须能报出物理核数，且至少为 1。
    ///
    /// 报不出不是「功能坏了」而是「退化路径」，所以只在这里要求必须能报——
    /// 否则 `decode_threads` 的主路径在 CI 上永远不被执行。
    #[test]
    fn 能报出物理核数() {
        let n = physical_cores().expect("取不到物理核数：API 调用或结构体布局不对");
        assert!(n >= 1, "物理核数不可能是 {n}");
    }

    /// `decode_threads` 落在合法区间，且不会因为机器核数多就失控。
    #[test]
    fn 解码线程数被夹在合法区间() {
        let t = decode_threads();
        assert!((1..=MAX_DECODE_THREADS).contains(&t), "越界：{t}");
    }

    /// 关键回归：逻辑核数 > 物理核数时，线程数应该取**物理**核数。
    ///
    /// 这正是 i3-3xxx（2C/4T）的形状 —— 修之前这里会拿到 4。
    /// 只在本机真的是超线程时才断言，否则这条在单核 CI 上没有意义。
    #[test]
    fn 超线程机器上取物理核而不是逻辑核() {
        let logical = std::thread::available_parallelism().ok().map(|n| n.get());
        let physical = physical_cores();
        match (logical, physical) {
            (Some(l), Some(p)) if l > p => {
                // 本机是超线程形状：确认结果不超过物理核数
                assert!(
                    decode_threads() <= p,
                    "逻辑核 {l} > 物理核 {p}，但线程数取了 {}",
                    decode_threads()
                );
            }
            _ => {
                // 不是超线程形状，或者取不到：至少确认线程数合法
                assert!((1..=MAX_DECODE_THREADS).contains(&decode_threads()));
            }
        }
    }

    /// 物理核数不可能超过逻辑核数。
    ///
    /// 这条抓的是 API 用错（比如把 `RelationProcessorCache` 的条目也数进去，
    /// 或者 union 步进写错导致重复计数）—— 那会让线程数虚高，且不报错。
    #[test]
    fn 物理核数不超过逻辑核数() {
        if let (Some(l), Some(p)) = (
            std::thread::available_parallelism().ok().map(|n| n.get()),
            physical_cores(),
        ) {
            assert!(p <= l, "物理核 {p} > 逻辑核 {l}，条目数多半数错了");
        }
    }
}
