pub mod mpv;
mod surface;

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use mpv::{InitOptions, MpvEventMessage, MpvPlayer};

/// 事件名：所有 mpv 状态变化都通过这一个事件推给前端。
const EVENT_NAME: &str = "mpv_event";

/// 事件名：原生视频窗口的点击 / 双击 / 拖动窗口事件。
const NATIVE_EVENT: &str = "native_event";

/// 推给前端的事件载荷。
#[derive(Serialize, Clone)]
struct MpvEventPayload {
    name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// 当前播放状态快照。
#[derive(Serialize, Clone)]
struct PlaybackInfo {
    duration: f64,
    position: f64,
    paused: bool,
    volume: f64,
    muted: bool,
    media_title: String,
}

/// 应用状态。
struct AppState {
    player: Arc<MpvPlayer>,
    /// 主窗口与视频子窗口的句柄。
    ///
    /// 存 `isize` 而不是 `HWND`：HWND 内部是裸指针，不实现 Send/Sync，
    /// 而 Tauri 的托管状态要求 `Send + Sync`。
    parent_raw: isize,
    video_raw: isize,
    /// 命令行里传进来的待打开文件，取出后即清空，避免重复打开
    pending_file: Mutex<Option<String>>,
}

/// 统一返回给前端的错误。
struct AppError(String);

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl From<String> for AppError {
    fn from(value: String) -> Self {
        AppError(value)
    }
}

impl From<&str> for AppError {
    fn from(value: &str) -> Self {
        AppError(value.to_string())
    }
}

type AppResult<T> = Result<T, AppError>;

// ---------------------------------------------------------------- mpv 事件转发

fn emit(app: &AppHandle, payload: MpvEventPayload) {
    // 前端监听失败（比如窗口还没建好）不该让事件线程退出
    if let Err(e) = app.emit(EVENT_NAME, payload) {
        eprintln!("发送 mpv 事件失败: {e}");
    }
}

/// 把 mpv 事件转成统一载荷推给前端。
fn forward_event(app: AppHandle, message: MpvEventMessage) {
    let payload = match message {
        MpvEventMessage::Property(p) => MpvEventPayload {
            name: p.name,
            value: Some(p.value),
            error: None,
        },
        MpvEventMessage::EndFile => MpvEventPayload {
            name: "end-file",
            value: None,
            error: None,
        },
        MpvEventMessage::FileLoaded => MpvEventPayload {
            name: "file-loaded",
            value: None,
            error: None,
        },
        MpvEventMessage::Error(msg) => MpvEventPayload {
            name: "error",
            value: None,
            error: Some(msg),
        },
    };
    emit(&app, payload);
}

// ---------------------------------------------------------------- 命令

/// 创建播放核心并嵌入到窗口中，返回 libmpv 版本号供日志确认。
#[tauri::command]
fn init_player(app: AppHandle, window: tauri::Window) -> AppResult<String> {
    let parent = window.hwnd().map_err(|e| AppError::from(format!("获取窗口句柄失败: {e}")))?;
    let video = surface::create_video_window(parent).map_err(AppError::from)?;

    // mpv 侧统一关闭了用户配置与脚本加载（见 MpvPlayer::configure）
    let player = match MpvPlayer::new(&InitOptions {
        wid: video.0 as isize,
        headless: false,
    }) {
        Ok(p) => p,
        Err(e) => {
            return Err(AppError::from(format!("创建播放核心失败: {e}")));
        }
    };

    let api_version = player.api_version();
    let mpv_version = player
        .get_property_string("mpv-version")
        .unwrap_or_else(|_| "未知".to_string());

    // 事件线程需要能拿到 AppHandle 往前端发消息
    let app_for_events = app.clone();
    player.spawn_event_loop(Box::new(move |message| {
        forward_event(app_for_events.clone(), message);
    }));

    // 视频窗口的点击 / 双击 / 拖动窗口走同一通道转发给前端
    let app_for_native = app.clone();
    surface::set_native_handler(Box::new(move |name| {
        if let Err(e) = app_for_native.emit(NATIVE_EVENT, name) {
            eprintln!("发送原生事件失败: {e}");
        }
    }));

    app.manage(AppState {
        player: Arc::new(player),
        parent_raw: parent.0 as isize,
        video_raw: video.0 as isize,
        pending_file: Mutex::new(cli_file()),
    });

    Ok(format!(
        "libmpv {mpv_version} (API {}.{})",
        api_version >> 16,
        api_version & 0xffff
    ))
}

/// 同步视频区域位置。
///
/// 前端只在 resize / 布局变化时调用，这里做客户区→屏幕坐标换算后移动原生子窗口。
#[tauri::command]
fn sync_video_rect(
    state: State<'_, AppState>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> AppResult<()> {
    surface::set_video_window_bounds(
        surface::hwnd_from_raw(state.parent_raw),
        surface::hwnd_from_raw(state.video_raw),
        x,
        y,
        width,
        height,
    );
    Ok(())
}

/// 一次性拉取当前播放状态。
///
/// mpv 只会推送「变化过的」属性值。切换文件时，上一份文件的属性事件
/// 还在队列里，紧跟着又会来新文件的更新，光靠增量事件前端无法判断
/// 哪个值属于当前文件。所以文件就绪时由前端主动拉一次权威状态。
#[tauri::command]
fn get_playback_info(state: State<'_, AppState>) -> AppResult<PlaybackInfo> {
    let player = &state.player;
    Ok(PlaybackInfo {
        duration: player.get_double_property("duration").unwrap_or(0.0),
        position: player.get_double_property("time-pos").unwrap_or(0.0),
        paused: player.get_pause().unwrap_or(true),
        volume: player.get_volume().unwrap_or(100.0),
        muted: player.get_mute().unwrap_or(false),
        media_title: player.get_property_string("media-title").unwrap_or_default(),
    })
}

/// 取出命令行里指定的文件（如果有）。
///
/// 支持 `video-view.exe <路径>`，这样把文件拖到 exe 图标上就能直接播放，
/// 也为将来注册文件关联留好接口。取出一次后即清空。
#[tauri::command]
fn take_initial_file(state: State<'_, AppState>) -> AppResult<Option<String>> {
    let taken = state.pending_file.lock().unwrap().take();
    Ok(taken)
}

/// 打开系统文件对话框。
///
/// `rfd` 是阻塞 API，所以放进阻塞线程池，避免卡住 Tauri 的异步运行时。
#[tauri::command]
async fn pick_file() -> AppResult<Option<String>> {
    let chosen = tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("选择视频文件")
            .add_filter(
                "视频文件",
                &[
                    "mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v", "ts", "mpg", "mpeg",
                    "rmvb", "rm", "vob", "3gp", "ogv", "asf", "f4v", "mts", "m2ts", "divx",
                ],
            )
            .add_filter("所有文件", &["*"])
            .pick_file()
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| AppError::from(format!("文件对话框任务失败: {e}")))?;

    Ok(chosen)
}

/// 加载并播放指定视频文件。
///
/// `path` 来自文件对话框 / 拖放 / 命令行，都是外部输入。真正的校验放在
/// `MpvPlayer::load_file` —— 那是路径进入 mpv 的唯一入口，校验点只有一个
/// 才不会在将来新增调用方时漏掉。
#[tauri::command]
fn open_file(state: State<'_, AppState>, path: String) -> AppResult<()> {
    state.player.load_file(&path).map_err(AppError::from)
}

/// 播放 / 暂停切换。
#[tauri::command]
fn toggle_pause(state: State<'_, AppState>) -> AppResult<()> {
    state.player.toggle_pause().map_err(AppError::from)
}

/// 停止播放。
#[tauri::command]
fn stop(state: State<'_, AppState>) -> AppResult<()> {
    state.player.stop().map_err(AppError::from)
}

/// 跳转到指定秒数。
#[tauri::command]
fn seek_to(state: State<'_, AppState>, seconds: f64) -> AppResult<()> {
    state.player.seek(seconds).map_err(AppError::from)
}

/// 设置音量，取值 0~100。
#[tauri::command]
fn set_volume(state: State<'_, AppState>, value: f64) -> AppResult<()> {
    state
        .player
        .set_volume(value.clamp(0.0, 100.0))
        .map_err(AppError::from)
}

/// 设置静音状态。
#[tauri::command]
fn set_mute(state: State<'_, AppState>, muted: bool) -> AppResult<()> {
    state.player.set_mute(muted).map_err(AppError::from)
}

/// 切换窗口全屏（F11）。
#[tauri::command]
fn toggle_fullscreen(window: tauri::Window) -> AppResult<()> {
    let current = window
        .is_fullscreen()
        .map_err(|e| AppError::from(format!("读取全屏状态失败: {e}")))?;
    window
        .set_fullscreen(!current)
        .map_err(|e| AppError::from(format!("切换全屏失败: {e}")))?;
    Ok(())
}

/// 退出全屏（ESC）。非全屏时调用无副作用。
#[tauri::command]
fn exit_fullscreen(window: tauri::Window) -> AppResult<()> {
    window
        .set_fullscreen(false)
        .map_err(|e| AppError::from(format!("退出全屏失败: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------- 入口

/// 从命令行参数里取第一个看起来像文件路径的参数。
///
/// 跳过 exe 自身，也跳过以 `-` 开头的开关参数（留给将来加命令行选项）。
fn cli_file() -> Option<String> {
    std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            init_player,
            sync_video_rect,
            get_playback_info,
            take_initial_file,
            pick_file,
            open_file,
            toggle_pause,
            stop,
            seek_to,
            set_volume,
            set_mute,
            toggle_fullscreen,
            exit_fullscreen,
        ])
        .build(tauri::generate_context!())
        .expect("创建 Tauri 应用失败")
        .run(|app, event| {
            // 只在真正退出时拆掉播放核心。
            //
            // 这个回调在**每个事件**时都会执行，包括频繁触发的
            // MainEventsCleared。如果无条件调用 shutdown()，mpv 会在
            // 启动后几十毫秒内被 quit 掉，表现为「文件加载了但完全不播放」。
            if let tauri::RunEvent::Exit = event {
                if let Some(state) = app.try_state::<AppState>() {
                    // 退出路径上已经没有 UI 可以提示错了，写到 stderr 即可
                    if let Err(e) = state.player.shutdown() {
                        eprintln!("关闭播放核心失败: {e}");
                    }
                }
            }
        });
}