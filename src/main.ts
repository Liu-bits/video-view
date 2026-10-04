import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import "./styles.css";

// ---------------------------------------------------------------- 元素引用

const stage = document.getElementById("stage") as HTMLDivElement;
const dropHint = document.getElementById("drop-hint") as HTMLDivElement;
const seek = document.getElementById("seek") as HTMLInputElement;
const volume = document.getElementById("volume") as HTMLInputElement;
const volumeIcon = document.getElementById("volume-icon") as HTMLSpanElement;
const timeCurrent = document.getElementById("time-current") as HTMLSpanElement;
const timeDuration = document.getElementById("time-duration") as HTMLSpanElement;
const fileName = document.getElementById("file-name") as HTMLSpanElement;
const btnOpen = document.getElementById("btn-open") as HTMLButtonElement;
const btnPlay = document.getElementById("btn-play") as HTMLButtonElement;
const btnStop = document.getElementById("btn-stop") as HTMLButtonElement;

// ---------------------------------------------------------------- 播放器状态

interface PlayerState {
  duration: number;
  position: number;
  paused: boolean;
  volume: number;
  muted: boolean;
  loaded: boolean;
}

const state: PlayerState = {
  duration: 0,
  position: 0,
  paused: true,
  volume: 100,
  muted: false,
  loaded: false,
};

let draggingSeek = false; // 用户拖动进度条时暂停跟随 mpv 的 position 更新
let draggingVolume = false; // 同理，拖动音量时不要被 volume 事件回写

/**
 * 是否还在等新文件就绪。
 *
 * mpv 只推送「发生过变化」的属性值，而切换文件时上一个文件的属性事件
 * 还在队列里，紧跟着又会来新文件的更新。只靠增量事件，前端无法分辨哪个
 * duration 属于当前文件 —— 表现为新视频刚打开就显示上一个视频的时长。
 *
 * 解决办法：以 mpv 的 FILE_LOADED 作为同步点。在它到达之前忽略所有属性
 * 增量事件，到达后再调 get_playback_info 拉一次权威状态。
 */
let awaitingFile = false;

// ---------------------------------------------------------------- 工具函数

/** 秒 -> mm:ss / h:mm:ss */
function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const total = Math.floor(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

function setVolumeIcon(): void {
  volumeIcon.textContent = state.muted || state.volume === 0 ? "静音" : "音量";
}

/** 把 0..1000 的滑块值反算成秒 */
function seekSliderToSeconds(value: number): number {
  if (state.duration <= 0) return 0;
  return (value / 1000) * state.duration;
}

function renderProgress(): void {
  if (!draggingSeek) {
    const ratio = state.duration > 0 ? state.position / state.duration : 0;
    const clamped = Math.max(0, Math.min(1, ratio));
    seek.value = String(Math.round(clamped * 1000));
    seek.style.setProperty("--pct", `${(clamped * 100).toFixed(2)}%`);
  }
  timeCurrent.textContent = formatTime(state.position);
  timeDuration.textContent = formatTime(state.duration);
}

function renderPlayButton(): void {
  btnPlay.textContent = state.paused ? "播放" : "暂停";
}

function renderTransportState(): void {
  const enabled = state.loaded;
  seek.disabled = !enabled;
  btnPlay.disabled = !enabled;
  btnStop.disabled = !enabled;
}

// ---------------------------------------------------------------- 原生窗口同步
//
// libmpv 把画面渲染到一个原生子窗口上，而子窗口的位置由 Win32 决定，
// 不会跟随网页布局变化。因此窗口缩放 / 控制栏高度变化后，
// 必须把 #stage 在客户区中的位置换算成屏幕坐标再同步给 Rust 侧。

let syncQueued = false;

function syncStageRect(): void {
  if (syncQueued) return;
  syncQueued = true;
  requestAnimationFrame(() => {
    syncQueued = false;
    const rect = stage.getBoundingClientRect();
    // mpv 会按窗口尺寸重新计算画面宽高比，这里给一个下限避免 0 尺寸窗口
    invoke("sync_video_rect", {
      x: Math.round(rect.left),
      y: Math.round(rect.top),
      width: Math.max(1, Math.round(rect.width)),
      height: Math.max(1, Math.round(rect.height)),
    }).catch((err) => console.error("同步视频区域失败:", err));
  });
}

// stage 布局一变（窗口缩放、影院模式），同步原生窗口
new ResizeObserver(() => {
  syncStageRect();
}).observe(stage);

// ---------------------------------------------------------------- 与 Rust 通信

/** 打开原生文件对话框并播放选中的文件 */
async function openViaDialog(): Promise<void> {
  try {
    const path = await invoke<string | null>("pick_file");
    if (path) await loadFile(path);
  } catch (err) {
    console.error("打开文件失败:", err);
  }
}

async function loadFile(path: string): Promise<void> {
  resetState();
  awaitingFile = true;
  try {
    await invoke("open_file", { path });
    state.loaded = true;
    renderTransportState();
    syncStageRect();
  } catch (err) {
    awaitingFile = false;
    console.error("加载视频失败:", err);
    alert(`无法打开该文件：\n${path}\n\nmpv 报错：${String(err)}`);
  }
}

function resetState(): void {
  state.duration = 0;
  state.position = 0;
  state.paused = true;
  state.loaded = false;
  dropHint.classList.remove("hidden");
  fileName.textContent = "";
  renderProgress();
  renderPlayButton();
  renderTransportState();
}

/** 文件就绪后主动拉一次权威状态，覆盖掉之前被忽略的增量事件 */
async function syncPlaybackInfo(): Promise<void> {
  try {
    const info = await invoke<PlaybackInfo>("get_playback_info");
    state.duration = info.duration;
    state.position = info.position;
    state.paused = info.paused;
    state.volume = Math.round(info.volume);
    state.muted = info.muted;
    if (info.media_title) fileName.textContent = info.media_title;
    volume.value = String(state.volume);
    setVolumeIcon();
    renderProgress();
    renderPlayButton();
  } catch (err) {
    console.error("读取播放状态失败:", err);
  }
}

/** 切换播放 / 暂停 */
async function togglePause(): Promise<void> {
  try {
    await invoke("toggle_pause");
  } catch (err) {
    console.error("切换播放状态失败:", err);
  }
}

async function stop(): Promise<void> {
  try {
    awaitingFile = false;
    await invoke("stop");
    resetState();
  } catch (err) {
    console.error("停止失败:", err);
  }
}

async function seekTo(seconds: number): Promise<void> {
  state.position = Math.max(0, Math.min(state.duration || 0, seconds));
  renderProgress();
  try {
    await invoke("seek_to", { seconds: state.position });
  } catch (err) {
    console.error("跳转失败:", err);
  }
}

async function changeVolume(value: number): Promise<void> {
  state.volume = value;
  volume.value = String(value);
  if (value > 0 && state.muted) state.muted = false;
  setVolumeIcon();
  try {
    await invoke("set_volume", { value });
  } catch (err) {
    console.error("设置音量失败:", err);
  }
}

async function toggleMute(): Promise<void> {
  state.muted = !state.muted;
  setVolumeIcon();
  try {
    await invoke("set_mute", { muted: state.muted });
  } catch (err) {
    console.error("设置静音失败:", err);
  }
}

async function toggleFullscreen(): Promise<void> {
  try {
    await invoke("toggle_fullscreen");
  } catch (err) {
    console.error("切换全屏失败:", err);
  }
}

async function exitFullscreen(): Promise<void> {
  try {
    await invoke("exit_fullscreen");
  } catch (err) {
    console.error("退出全屏失败:", err);
  }
}

/** 切换影院模式：隐藏控制栏，画面占满窗口（视频宽高比仍然保持） */
function toggleTheatre(): void {
  document.body.classList.toggle("theatre");
  requestAnimationFrame(syncStageRect);
}

// ---------------------------------------------------------------- mpv 事件处理

interface MpvEvent {
  name: string;
  value?: unknown;
  error?: string;
}

interface PlaybackInfo {
  duration: number;
  position: number;
  paused: boolean;
  volume: number;
  muted: boolean;
  media_title: string;
}

function handleMpvEvent({ name, value, error }: MpvEvent): void {
  // 新文件还没就绪时，增量事件可能属于上一个文件，直接丢弃
  if (awaitingFile && name !== "file-loaded" && name !== "error" && name !== "end-file") {
    return;
  }

  switch (name) {
    case "file-loaded":
      // 同步点：拉一次权威状态，再恢复接收增量事件
      awaitingFile = false;
      // 欢迎界面的隐藏依赖 idle-active=false 事件，但该事件在 awaitingFile
      // 期间会被上面的拦截逻辑丢弃，所以这里主动隐藏。
      dropHint.classList.add("hidden");
      state.loaded = true;
      renderTransportState();
      void syncPlaybackInfo();
      break;

    case "time-pos":
      // mpv 在没有活动文件时会把该属性置为 null（Rust 侧解码成 null）。
      // 这不是「进度归零」，直接忽略，否则播放中途会闪回 00:00。
      if (typeof value === "number") {
        state.position = value;
        renderProgress();
      }
      break;

    case "duration":
      // 同理：duration 变 null 通常是文件被卸载，此时保持上一份值，
      // 由 resetState / idle-active 负责清零。
      if (typeof value === "number") {
        state.duration = value;
        renderProgress();
      }
      break;

    case "pause":
      state.paused = value === true;
      renderPlayButton();
      break;

    case "volume":
      if (typeof value === "number") {
        state.volume = Math.round(value);
        // 用户正在拖动时不要回写，否则滑块会跟手抖
        if (!draggingVolume) volume.value = String(state.volume);
        if (state.volume > 0) state.muted = false;
        setVolumeIcon();
      }
      break;

    case "media-title":
      if (typeof value === "string") fileName.textContent = value;
      break;

    case "idle-active":
      if (value === true) {
        dropHint.classList.remove("hidden");
        state.loaded = false;
        state.paused = true;
        renderPlayButton();
        renderTransportState();
      } else {
        dropHint.classList.add("hidden");
      }
      break;

    case "end-file":
      // 播放到结尾：进度归位但保留文件信息，方便用户重播。
      // 时长为 0（未知）时保持原值，避免总时长闪成 00:00。
      if (state.duration > 0) state.position = state.duration;
      state.paused = true;
      renderProgress();
      renderPlayButton();
      break;

    case "error":
      awaitingFile = false;
      console.error("mpv 错误:", error);
      alert(`播放出错：\n${String(error ?? "未知错误")}`);
      break;

    default:
      break;
  }
}

/**
 * 原生视频窗口转发过来的鼠标事件。
 *
 * 视频子窗口整个盖在 WebView2 之上，落在画面上的鼠标消息网页收不到，
 * 必须走 Rust 侧的 native_event 通道（点击 = 播放/暂停，双击 = 影院模式）。
 */
function handleNativeEvent(name: string): void {
  switch (name) {
    case "click":
      // 双击的第一次点击也会走到这里，延迟一小段时间等 dblclick 到达，
      // 避免「双击切换影院模式」同时又触发一次暂停。
      if (pendingClick !== null) return;
      pendingClick = window.setTimeout(() => {
        pendingClick = null;
        void togglePause();
      }, 250);
      break;

    case "dblclick":
      if (pendingClick !== null) {
        clearTimeout(pendingClick);
        pendingClick = null;
      }
      toggleTheatre();
      break;

    default:
      break;
  }
}

/** 待确认的单击定时器，双击到达时撤销。 */
let pendingClick: number | null = null;

// ---------------------------------------------------------------- 事件绑定

seek.addEventListener("pointerdown", () => {
  draggingSeek = true;
});

seek.addEventListener("pointerup", () => {
  draggingSeek = false;
  seekTo(seekSliderToSeconds(Number(seek.value)));
});

seek.addEventListener("input", () => {
  // 拖动过程中只更新显示，松手才真正 seek
  seek.style.setProperty("--pct", `${(Number(seek.value) / 10).toFixed(2)}%`);
  timeCurrent.textContent = formatTime(seekSliderToSeconds(Number(seek.value)));
});

volume.addEventListener("pointerdown", () => {
  draggingVolume = true;
});

volume.addEventListener("pointerup", () => {
  draggingVolume = false;
});

volume.addEventListener("input", () => {
  changeVolume(Number(volume.value));
});

volumeIcon.addEventListener("click", toggleMute);
btnOpen.addEventListener("click", openViaDialog);
btnPlay.addEventListener("click", togglePause);
btnStop.addEventListener("click", stop);

// 画面上的双击走原生通道（handleNativeEvent），这里覆盖欢迎页的空白区域
stage.addEventListener("dblclick", toggleTheatre);

document.addEventListener("keydown", (e) => {
  // 焦点在滑块上时方向键 / 空格要留给滑块自己处理
  if (e.target instanceof HTMLInputElement) return;

  switch (e.key) {
    case " ":
    case "Spacebar":
      e.preventDefault();
      void togglePause();
      break;
    case "o":
      if (e.ctrlKey || e.metaKey) {
        e.preventDefault();
        void openViaDialog();
      }
      break;
    case "F11":
      e.preventDefault();
      void toggleFullscreen();
      break;
    case "f":
    case "F":
      toggleTheatre();
      break;
    case "Escape":
      if (document.body.classList.contains("theatre")) {
        document.body.classList.remove("theatre");
        requestAnimationFrame(syncStageRect);
      } else {
        void exitFullscreen();
      }
      break;
    case "ArrowRight":
      e.preventDefault();
      void seekTo(state.position + 10);
      break;
    case "ArrowLeft":
      e.preventDefault();
      void seekTo(state.position - 10);
      break;
    default:
      break;
  }
});

// 拖放打开文件
void getCurrentWebview().onDragDropEvent((event) => {
  if (event.payload.type === "over") {
    stage.classList.add("dragover");
  } else if (event.payload.type === "drop") {
    stage.classList.remove("dragover");
    const [first] = event.payload.paths;
    if (first) void loadFile(first);
  } else {
    stage.classList.remove("dragover");
  }
});

// ---------------------------------------------------------------- 启动

async function bootstrap(): Promise<void> {
  // 先挂监听再初始化：init_player 之后 mpv 立刻开始推事件，
  // 顺序反了会丢掉 file-loaded 之前的事件。
  await listen<MpvEvent>("mpv_event", (e) => handleMpvEvent(e.payload));
  await listen<string>("native_event", (e) => handleNativeEvent(e.payload));

  try {
    const version = await invoke<string>("init_player");
    console.log("libmpv 已启动:", version);
    syncStageRect();
  } catch (err) {
    console.error("初始化播放核心失败:", err);
    dropHint.textContent = `播放核心初始化失败：${String(err)}`;
  }

  renderPlayButton();
  renderTransportState();
  setVolumeIcon();
  renderProgress();

  // 命令行里指定了文件就直接播放（拖到 exe 图标上打开）
  try {
    const initial = await invoke<string | null>("take_initial_file");
    if (initial) await loadFile(initial);
  } catch (err) {
    console.error("处理命令行文件失败:", err);
  }
}

void bootstrap();