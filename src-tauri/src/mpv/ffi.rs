//! libmpv 的最小 FFI 声明 + 运行时动态加载。
//!
//! 为什么不用现成的 crate：`mpv-client` / `libmpv2` 都依赖 bindgen 或要求
//! 构建期存在 MSVC import lib（`mpv.lib`）。而 Windows 上分发的 libmpv 只有
//! DLL，没有配套的 `.lib`，装个 libclang 也解决不了链接问题。
//!
//! 这里改为运行时用 `LoadLibrary` 加载 DLL 并手工声明用到的符号，好处是：
//! - 构建期零 C 依赖，只要 rustc + linker 即可，clone 下来就能编译
//! - 单 exe 分发时 DLL 放同级目录即可，不需要改链接参数

use std::ffi::{c_char, c_double, c_int, c_uint, c_void};

use libloading::Library;

// ---------------------------------------------------------------- mpv_format

pub const MPV_FORMAT_NONE: c_int = 0;
pub const MPV_FORMAT_STRING: c_int = 1;
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_INT64: c_int = 4;
pub const MPV_FORMAT_DOUBLE: c_int = 5;
pub const MPV_FORMAT_NODE: c_int = 6;
pub const MPV_FORMAT_NODE_ARRAY: c_int = 7;
pub const MPV_FORMAT_NODE_MAP: c_int = 8;
pub const MPV_FORMAT_BYTE_ARRAY: c_int = 9;

/// 「不选」这条轨道时 `sid` / `aid` 的取值。
///
/// mpv 的 choice 属性用字符串表示，`"no"` 是「明确不要」，`"auto"` 是
/// 「让 mpv 自己定」。关字幕要用 `"no"` 而不是 `-1` —— 实测 `sid = -1`
/// 报 `MPV_ERROR_PROPERTY_FORMAT`，症状是「菜单里点了关闭，字幕还在」。
pub const OFF_TRACK: &str = "no";

// `MPV_FORMAT_NODE` / `NODE_ARRAY` / `NODE_MAP` 三个值是**实测出来的**，不是
// 照 `client.h` 抄的 —— 抄错了不会编译失败、不会崩溃，只会「安静地返回空」。
//
// 探针（`tests/zz_track_probe.rs`，用完删）对 `track-list` 逐个 format 试的结果：
//
//   format=0  err  unsupported format
//   format=1  ok    但返回的是**字符串**（OSD_STRING），按节点读是垃圾
//   format=2  ok    同上
//   format=3  err  unsupported format（track-list 不是 flag）
//   format=4  err  unsupported format
//   format=5  err  unsupported format
//   format=6  ok    node.format = 7，list 非空   <- 请求用这个
//   format=7  err  unsupported format
//   format=8  err  unsupported format
//
// 两个坑：
//
// 1. 请求时要用 `MPV_FORMAT_NODE`（6），但 mpv **回填的** `node.format`
//    是 `MPV_FORMAT_NODE_ARRAY`（7）——`track-list` 是有序数组不是对象。
//    所以遍历时 6 / 7 / 8 三个值都当「类列表」处理，漏掉任何一个都会把
//    整棵树变成空列表。
// 2. 曾经把 `MPV_FORMAT_NODE` 写成 2（那是 `OSD_STRING`）：`get_property`
//    返回**成功**，`convert_node` 走到兜底分支返回空列表，全程无异常。
//    这类错误只能靠实测发现。

// ---------------------------------------------------------------- mpv_node
//
// `track-list` 这类属性不是标量，而是一棵**树**：`mpv_get_property` 传
// MPV_FORMAT_NODE 时填进来一个 `mpv_node`，里面按 format 决定哪个联合成员
// 有效；子节点是 `mpv_node_list`（一个带 keys 的值数组）。
//
// 取出来的树**归调用方释放**，必须调 `mpv_free_node_contents` —— 它会递归
// free 掉每一个 `char *`、每一个 `values` / `keys` 数组。忘了调就是泄漏，
// 而这个泄漏在长时间播放 + 频繁切轨的场景下会很明显。
//
// 结构体布局必须与 `mpv_node.h` 逐一对应。`u` 是联合体，按最大成员对齐；
// 这里显式写成两个字段并手工算偏移是危险的，所以直接照抄头文件的嵌套
// 定义，让 Rust 的 `repr(C)` 去做布局。

#[repr(C)]
pub struct MpvNodeList {
    pub num: c_int,
    /// 长度为 `num` 的节点数组（列表的**值**）
    pub values: *mut MpvNode,
    /// 长度为 `num` 的键数组（对象的**键**）。列表（而非对象）时为 NULL。
    pub keys: *mut *mut c_char,
}

#[repr(C)]
pub struct MpvByteArray {
    pub data: *mut c_void,
    pub size: usize,
}

#[repr(C)]
pub union MpvNodeUnion {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: c_double,
    pub list: *mut MpvNodeList,
    pub ba: *mut MpvByteArray,
}

#[repr(C)]
pub struct MpvNode {
    pub u: MpvNodeUnion,
    pub format: c_int,
}

// ---------------------------------------------------------------- mpv_event_id
//
// 取值来自 mpv 2.x 的 client.h。这些枚举值并不连续，写错会读到错误的事件。

pub const MPV_EVENT_NONE: c_int = 0;
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
pub const MPV_EVENT_LOG_MESSAGE: c_int = 2;
pub const MPV_EVENT_GET_PROPERTY_REPLY: c_int = 3;
pub const MPV_EVENT_SET_PROPERTY_REPLY: c_int = 4;
pub const MPV_EVENT_COMMAND_REPLY: c_int = 5;
pub const MPV_EVENT_START_FILE: c_int = 6;
pub const MPV_EVENT_END_FILE: c_int = 7;
pub const MPV_EVENT_FILE_LOADED: c_int = 8;
pub const MPV_EVENT_CLIENT_MESSAGE: c_int = 16;
pub const MPV_EVENT_VIDEO_RECONFIG: c_int = 17;
pub const MPV_EVENT_AUDIO_RECONFIG: c_int = 18;
pub const MPV_EVENT_SEEK: c_int = 20;
pub const MPV_EVENT_PLAYBACK_RESTART: c_int = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;
pub const MPV_EVENT_QUEUE_OVERFLOW: c_int = 23;

// ---------------------------------------------------------------- mpv_end_file_reason

pub const MPV_END_FILE_REASON_EOF: c_int = 0;
pub const MPV_END_FILE_REASON_STOP: c_int = 2;
pub const MPV_END_FILE_REASON_QUIT: c_int = 3;
pub const MPV_END_FILE_REASON_ERROR: c_int = 4;

// ---------------------------------------------------------------- 结构体

#[repr(C)]
pub struct MpvEvent {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct MpvEventProperty {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct MpvEventEndFile {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: c_int,
    pub playlist_insert_num_entries: c_int,
}

// ---------------------------------------------------------------- 函数签名

type FnCreate = unsafe extern "system" fn() -> *mut c_void;
type FnInitialize = unsafe extern "system" fn(*mut c_void) -> c_int;
type FnDestroy = unsafe extern "system" fn(*mut c_void);
type FnSetOptionString =
    unsafe extern "system" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type FnSetProperty =
    unsafe extern "system" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int;
type FnSetPropertyString =
    unsafe extern "system" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type FnGetProperty =
    unsafe extern "system" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int;
type FnObserveProperty = unsafe extern "system" fn(*mut c_void, u64, *const c_char, c_int) -> c_int;
type FnCommand = unsafe extern "system" fn(*mut c_void, *mut *const c_char) -> c_int;
type FnCommandAsync = unsafe extern "system" fn(*mut c_void, u64, *mut *const c_char) -> c_int;
type FnWaitEvent = unsafe extern "system" fn(*mut c_void, c_double) -> *mut MpvEvent;
type FnErrorString = unsafe extern "system" fn(c_int) -> *const c_char;
type FnFree = unsafe extern "system" fn(*mut c_void);
pub type FnFreeNodeContents = unsafe extern "system" fn(*mut MpvNode);
// 注意：libmpv 里所有返回状态码的函数都返回 `int`，`mpv_client_api_version`
// 返回的是 `unsigned int`。声明成 u64 会读到 eax 的高位垃圾，是堆损坏的常见来源。
type FnClientApiVersion = unsafe extern "system" fn() -> c_uint;
type FnClientName = unsafe extern "system" fn(*mut c_void) -> *const c_char;

// ---------------------------------------------------------------- API 表

/// 已加载的 libmpv 符号表。
///
/// `Library` 必须在所有使用这些函数指针的线程都结束后才允许 drop，
/// 因此它和 `Mpv` handle 一起由 `MpvContext` 持有，事件线程先 join 再释放。
pub struct MpvApi {
    pub create: FnCreate,
    pub initialize: FnInitialize,
    pub destroy: FnDestroy,
    pub set_option_string: FnSetOptionString,
    pub set_property: FnSetProperty,
    pub get_property: FnGetProperty,
    pub observe_property: FnObserveProperty,
    pub command: FnCommand,
    pub command_async: FnCommandAsync,
    pub wait_event: FnWaitEvent,
    pub error_string: FnErrorString,
    pub set_property_string: FnSetPropertyString,
    pub free: FnFree,
    /// 释放 `mpv_get_property(MPV_FORMAT_NODE)` 取出来的整棵树。
    ///
    /// 少一个符号就不能加载 DLL —— 所以它是**必需**符号而不是可选的：
    /// 宁可加载失败给出明确报错，也不要等到第一次切轨时才在运行中崩。
    pub free_node_contents: FnFreeNodeContents,
    pub client_api_version: FnClientApiVersion,
    pub client_name: FnClientName,
    // 持有 DLL，必须放在最后 drop
    _lib: Library,
}

impl MpvApi {
    /// 从 `path` 加载 libmpv。
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        // RTLD_LOCAL：不让 libmpv 的符号污染宿主进程（本程序是 exe，无所谓，
        // 但保持局部加载语义更干净）。DLL 上的符号解析仍走默认搜索路径。
        let lib = unsafe { Library::new(path) }
            .map_err(|e| format!("加载 {} 失败: {e}", path.display()))?;

        // 逐个取符号。`*sym` 会把 Symbol 解引用成裸函数指针的拷贝，
        // 这样就不带 libloading 的生命周期约束，但仍要求 _lib 不先于它 drop。
        unsafe {
            let create = *lib
                .get::<FnCreate>(b"mpv_create\0")
                .map_err(|_| "libmpv 缺少符号 mpv_create".to_string())?;
            let initialize = *lib
                .get::<FnInitialize>(b"mpv_initialize\0")
                .map_err(|_| "libmpv 缺少符号 mpv_initialize".to_string())?;
            let destroy = *lib
                .get::<FnDestroy>(b"mpv_destroy\0")
                .map_err(|_| "libmpv 缺少符号 mpv_destroy".to_string())?;
            let set_option_string = *lib
                .get::<FnSetOptionString>(b"mpv_set_option_string\0")
                .map_err(|_| "libmpv 缺少符号 mpv_set_option_string".to_string())?;
            let set_property = *lib
                .get::<FnSetProperty>(b"mpv_set_property\0")
                .map_err(|_| "libmpv 缺少符号 mpv_set_property".to_string())?;
            let set_property_string = *lib
                .get::<FnSetPropertyString>(b"mpv_set_property_string\0")
                .map_err(|_| "libmpv 缺少符号 mpv_set_property_string".to_string())?;
            let get_property = *lib
                .get::<FnGetProperty>(b"mpv_get_property\0")
                .map_err(|_| "libmpv 缺少符号 mpv_get_property".to_string())?;
            let observe_property = *lib
                .get::<FnObserveProperty>(b"mpv_observe_property\0")
                .map_err(|_| "libmpv 缺少符号 mpv_observe_property".to_string())?;
            let command = *lib
                .get::<FnCommand>(b"mpv_command\0")
                .map_err(|_| "libmpv 缺少符号 mpv_command".to_string())?;
            let command_async = *lib
                .get::<FnCommandAsync>(b"mpv_command_async\0")
                .map_err(|_| "libmpv 缺少符号 mpv_command_async".to_string())?;
            let wait_event = *lib
                .get::<FnWaitEvent>(b"mpv_wait_event\0")
                .map_err(|_| "libmpv 缺少符号 mpv_wait_event".to_string())?;
            let error_string = *lib
                .get::<FnErrorString>(b"mpv_error_string\0")
                .map_err(|_| "libmpv 缺少符号 mpv_error_string".to_string())?;
            let free = *lib
                .get::<FnFree>(b"mpv_free\0")
                .map_err(|_| "libmpv 缺少符号 mpv_free".to_string())?;
            let free_node_contents = *lib
                .get::<FnFreeNodeContents>(b"mpv_free_node_contents\0")
                .map_err(|_| "libmpv 缺少符号 mpv_free_node_contents".to_string())?;
            let client_api_version = *lib
                .get::<FnClientApiVersion>(b"mpv_client_api_version\0")
                .map_err(|_| "libmpv 缺少符号 mpv_client_api_version".to_string())?;
            let client_name = *lib
                .get::<FnClientName>(b"mpv_client_name\0")
                .map_err(|_| "libmpv 缺少符号 mpv_client_name".to_string())?;

            Ok(Self {
                create,
                initialize,
                destroy,
                set_option_string,
                set_property,
                set_property_string,
                get_property,
                observe_property,
                command,
                command_async,
                wait_event,
                error_string,
                free,
                free_node_contents,
                client_api_version,
                client_name,
                _lib: lib,
            })
        }
    }

    /// 把 mpv 错误码转成可读文本。
    pub fn error_message(&self, code: c_int) -> String {
        if code >= 0 {
            return format!("success ({code})");
        }
        unsafe {
            let ptr = (self.error_string)(code);
            if ptr.is_null() {
                format!("未知错误 (code {code})")
            } else {
                std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        }
    }
}

// 符号全是不透明的裸函数指针，只在 MpvApi 存活期间有效。
// MpvContext 会保证 handle 的所有使用者都 join 后才 drop MpvApi。
unsafe impl Send for MpvApi {}
unsafe impl Sync for MpvApi {}
