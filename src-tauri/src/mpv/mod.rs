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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ffi::*;

/// 需要观察的属性。变更时通过 `mpv_event` 推给前端。
///
/// 这里不依赖 `reply_userdata` 做映射，而是拿事件里的属性名反查，
/// 少一层状态，代价只是一次极短的线性查找。
const OBSERVED_PROPERTIES: &[(&str, c_int)] = &[
    ("time-pos", MPV_FORMAT_DOUBLE),
    ("duration", MPV_FORMAT_DOUBLE),
    ("pause", MPV_FORMAT_FLAG),
    ("volume", MPV_FORMAT_DOUBLE),
    ("mute", MPV_FORMAT_FLAG),
    ("media-title", MPV_FORMAT_STRING),
    ("idle-active", MPV_FORMAT_FLAG),
];

/// mpv 报告的属性变化，前端据此更新 UI。
#[derive(Debug, Clone)]
pub struct PropertyChange {
    pub name: &'static str,
    pub value: serde_json::Value,
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
}

/// 一条在途异步命令持有的参数内存。
///
/// `_ptrs` 指向 `_args` 自己的堆内存；两者一起 drop，所以只要结构还活着，
/// 指针就一定指向有效内存。
struct PendingCommand {
    _args: Box<[CString]>,
    _ptrs: Box<[*const c_char]>,
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
            return Err("mpv_create 返回空句柄".to_string());
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
        })
    }

    /// mpv 的 API 版本号，高 16 位是 major，低 16 位是 minor。
    pub fn api_version(&self) -> u32 {
        unsafe { (self.api.client_api_version)() }
    }

    fn configure(
        api: &MpvApi,
        handle: *mut c_void,
        options: &InitOptions,
    ) -> Result<(), String> {
        unsafe {
            let set_opt = |name: &str, value: &CString| -> Result<(), String> {
                let cname =
                    CString::new(name).map_err(|_| format!("选项名 {name:?} 包含空字节"))?;
                let code = (api.set_option_string)(handle, cname.as_ptr(), value.as_ptr());
                check(api, code, &format!("设置选项 {name}"))
            };

            // 渲染目标窗口：mpv 用 Direct3D 直接画到这个 HWND。
            let wid = CString::new(options.wid.to_string())
                .map_err(|_| "非法窗口句柄".to_string())?;
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
                // 不要求窗口消息循环泵送，避免 Tauri 主线程不泵送消息导致黑屏。
                // d3d11-flip-model 选项只能在 VO 初始化后用 set_property 设置，
                // 无法在 mpv_initialize 之前用 set_option 设置（返回 -5）。
                set_opt("vo", &CString::new("gpu").unwrap())?;
                set_opt("gpu-context", &CString::new("d3d11").unwrap())?;
                // 硬件解码自动选择，不兼容时回落到软件解码。
                set_opt("hwdec", &CString::new("auto-safe").unwrap())?;
                // 关掉 mpv 自带的 OSD/控制条，控制栏由前端负责。
                set_opt("osc", &CString::new("no").unwrap())?;
                set_opt("osd-level", &CString::new("0").unwrap())?;
            }

            check(api, (api.initialize)(handle), "mpv_initialize")?;

            for (name, format) in OBSERVED_PROPERTIES {
                let cname =
                    CString::new(*name).map_err(|_| "属性名包含空字节".to_string())?;
                let code =
                    (api.observe_property)(handle, 0, cname.as_ptr(), *format);
                check(api, code, &format!("观察属性 {name}"))?;
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
                            if let Some(msg) = decode_property(event.data) {
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
                            if let Ok(mut map) = pending.lock() {
                                map.remove(&event.reply_userdata);
                            }
                        }
                        _ => {}
                    }
                }
            })
            .expect("创建 mpv 事件线程失败");

        *self.event_thread.lock().unwrap() = Some(thread);
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
                CString::new(*a).map_err(|_| format!("命令参数 {a:?} 包含空字节"))?,
            );
        }
        let args: Box<[CString]> = owned.into_boxed_slice();

        let mut ptrs: Vec<*const c_char> = args.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null()); // mpv 要求以 NULL 结尾
        let ptrs: Box<[*const c_char]> = ptrs.into_boxed_slice();

        // Box 的堆缓存在 move 前后地址不变，所以这里取到的指针在 move 之后
        // 依然指向 args 的内容。
        let args_ptr = ptrs.as_ptr() as *mut *const c_char;

        // reply_userdata 用来把回复对回这条命令，事件线程据此归还内存。
        let reply_id = self.next_reply_id.fetch_add(1, Ordering::Relaxed);
        self.pending.lock().unwrap().insert(
            reply_id,
            PendingCommand {
                _args: args,
                _ptrs: ptrs,
            },
        );

        let code = unsafe { (self.api.command_async)(self.handle, reply_id, args_ptr) };
        if code < 0 {
            // 命令没被接受，mpv 不会发回复，这里立刻收回内存
            self.pending.lock().unwrap().remove(&reply_id);
            return Err(format!("下发命令失败: {}", self.api.error_message(code)));
        }
        Ok(())
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
        check(&self.api, code, &format!("设置属性 {name}"))
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
        check(&self.api, code, &format!("设置属性 {name}"))
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
    pub fn load_file(&self, path: &str) -> Result<(), String> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("文件路径为空".to_string());
        }
        if trimmed.contains('\0') {
            return Err("文件路径包含空字节".to_string());
        }
        if !Path::new(trimmed).is_file() {
            return Err(format!("找不到文件：{trimmed}"));
        }
        // loadfile <url> <flags>；replace 表示替换当前播放列表
        self.command(&["loadfile", trimmed, "replace"])
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

    /// 设置音量。mpv 允许 0~130，这里前端限制在 100。
    pub fn set_volume(&self, volume: f64) -> Result<(), String> {
        self.set_double_property("volume", volume)
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
        self.join_event_thread();
        unsafe { (self.api.destroy)(self.handle) };
        result
    }

    fn join_event_thread(&self) {
        if let Some(thread) = self.event_thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        // 正常路径是显式 shutdown()；这里兜底，保证线程一定被 join，
        // 否则 MpvApi（以及它持有的 DLL）会在事件线程仍在运行时被卸载。
        self.shutdown.store(true, Ordering::Relaxed);
        self.join_event_thread();
    }
}

// ---------------------------------------------------------------- 事件解码

fn check(api: &MpvApi, code: c_int, what: &str) -> Result<(), String> {
    if code < 0 {
        Err(format!("{what} 失败: {}", api.error_message(code)))
    } else {
        Ok(())
    }
}

fn decode_property(data: *mut c_void) -> Option<MpvEventMessage> {
    if data.is_null() {
        return None;
    }
    let prop = unsafe { &*(data as *const MpvEventProperty) };
    let name = lookup_property(prop.name)?;

    let value = if prop.data.is_null() {
        serde_json::Value::Null
    } else {
        match name {
            "pause" | "mute" | "idle-active" => {
                serde_json::Value::Bool(unsafe { *(prop.data as *const c_int) != 0 })
            }
            "volume" | "time-pos" | "duration" => {
                serde_json::Value::from(unsafe { *(prop.data as *const f64) })
            }
            "media-title" => {
                let ptr = unsafe { *(prop.data as *const *const c_char) };
                if ptr.is_null() {
                    serde_json::Value::Null
                } else {
                    // 注意：这里读的是 mpv 内部持有的字符串指针，
                    // 不是 mpv 为调用方分配的副本，因此绝不能 mpv_free。
                    // （只有 mpv_get_property 返回的字符串才需要释放。）
                    let text = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
                    serde_json::Value::String(text)
                }
            }
            _ => serde_json::Value::Null,
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
            "播放失败: {}",
            api.error_message(error)
        )))
    } else {
        Some(MpvEventMessage::EndFile)
    }
}

/// 把 mpv 返回的属性名映射回静态字符串，便于前端按固定字段名匹配。
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
/// 1. 环境变量 `MPV2_DLL` —— 排查打包问题时手动指定
/// 2. 可执行文件所在目录，以及打包后的 `resources/` 子目录
/// 3. debug 构建额外向上找几级、并回退到源码目录
///
/// 向上找只对 debug 开放：cargo 会把测试/示例的可执行文件放进
/// `target/<profile>/deps/`，而 `build.rs` 把 DLL 放在 `target/<profile>/`。
/// 正式版本如果也向上找，就等于允许 `C:\Program Files\` 或用户目录里
/// 任意一个同名 DLL 被优先加载（DLL 劫持），所以 release 只认 exe 同级
/// 和 `resources/`。
fn resolve_library_path() -> Result<PathBuf, String> {
    if let Ok(custom) = std::env::var("MPV2_DLL") {
        let p = PathBuf::from(custom);
        if p.exists() {
            return Ok(p);
        }
    }

    let mut candidates = Vec::new();

    if let Some(mut dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        // 同级优先：打包后 Tauri 把 DLL 放在 exe 同级
        candidates.push(dir.join(DLL_NAME));
        candidates.push(dir.join("resources").join(DLL_NAME));
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
