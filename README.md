# VideoView v0.1.0

轻量级 Windows 桌面视频播放器，基于 Tauri + libmpv。

## 功能

- 打开本地视频文件（拖放 / 文件对话框 / 命令行参数）
- 播放 / 暂停 / 停止
- 进度条拖动跳转，显示当前时间与总时长
- 音量控制 / 静音
- 画面自适应窗口：缩放窗口时视频区跟随重排，按原始宽高比缩放
- 影院模式（隐藏控制栏）与 `F11` 全屏
- 快捷键：`空格` 播放/暂停、`Ctrl+O` 打开、`←` / `→` 快退快进、
  `F` 影院模式、`F11` 全屏、`Esc` 退出影院模式 / 全屏；
  单击画面切换播放暂停，双击画面切换影院模式
- 支持 MP4、MKV、AVI、MOV、WebM、FLV、WMV 等常见容器
- 支持 H.264、HEVC、VP9、AV1、MPEG-4 等视频编码
- 支持 AAC、MP3、Opus、Vorbis、FLAC 等音频编码

## 技术栈

- **前端**：原生 TypeScript + Vite（无框架）
- **后端**：Tauri 2 + Rust
- **播放核心**：libmpv（动态加载 `libmpv-2.dll`，构建期零 C 依赖）
- **视频窗口**：Win32 子窗口（`CreateWindowExW`），mpv 直接渲染到 HWND

## 构建

```powershell
# 1. 获取 libmpv（按 scripts/libmpv.lock.json 锁定的版本下载并校验哈希）
.\scripts\fetch-libmpv.ps1

# 2. 安装依赖
npm install

# 3. 构建（前端 + Rust + NSIS 安装包）
npm run tauri build
```

产物：
- `src-tauri/target/release/video-view.exe`（便携版）
- `src-tauri/target/release/bundle/nsis/VideoView_0.1.0_x64-setup.exe`（安装程序）

## 运行

```powershell
# 打开文件
.\video-view.exe "D:\path\to\video.mp4"

# 或双击 exe 后拖入视频文件
```

## 测试

```powershell
# 12 种格式解码集成测试
cd src-tauri
cargo test --test mpv_integration

# mpv 选项验证
cargo test --test all_options
```

## 系统要求

- Windows 10 64-bit 或更高
- 无需安装 Rust / Node.js（运行时只需 `libmpv-2.dll`，已打包）

## 供应链

播放核心是被 `LoadLibrary` 进本进程并执行的第三方二进制，因此下载环节
不做任何版本选择：

- 版本与两个 SHA-256（压缩包、解压出的 DLL）锁定在
  [`scripts/libmpv.lock.json`](scripts/libmpv.lock.json)
- `fetch-libmpv.ps1` 只下载锁定 tag 下的锁定资产，逐层校验哈希，
  任何一层对不上就中止，不会把未校验的二进制写进构建输入路径
- 本地已有的 `libmpv-2.dll` 每次也会校验；不匹配时脚本拒绝覆盖，
  需要显式 `-Force`

因此「谁构建、什么时候构建」不影响产物里的播放核心是哪一份字节。

## 安全相关行为

- 不读取 `%APPDATA%\mpv` 下的用户配置，也不加载任何 mpv 脚本
  （`config=no`、`load-scripts=no`、`ytdl=no`）
- 只接受磁盘上确实存在的普通文件。mpv 的 `loadfile` 第一个参数是 URL 而不
  只是文件名，校验点放在 `MpvPlayer::load_file` 这一个入口上
- 正式版只在 exe 同级与 `resources/` 查找 `libmpv-2.dll`，
  不向上遍历父目录
- 前端启用严格 CSP，`script-src` 仅 `'self'`

## 许可证

GPL-3.0-or-later，详见 [LICENSE](LICENSE)。

第三方组件许可见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
