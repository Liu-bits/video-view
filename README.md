# VideoView v0.2.0

轻量级 Windows 桌面视频播放器，纯 Win32 自绘界面 + libmpv 播放核心。

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
- 界面按显示器 DPI 自动缩放，没有开关也没有「高质量/兼容」之类选项

## 技术栈

- **界面**：原生 Win32 + GDI 手绘，没有浏览器、没有 WebView、没有前端工程
- **语言**：Rust（`windows` crate 直接调 Win32 API）
- **播放核心**：libmpv（运行时动态加载 `libmpv-2.dll`，构建期零 C 依赖）
- **视频窗口**：Win32 子窗口（`CreateWindowExW`），mpv 直接渲染到 HWND

界面这一层是 v0.2.0 的重写。原来用的是 Tauri + WebView2，
Chromium 进程光内存就占掉 320MB，而且文字受位图缩放影响，
放大后在 125% / 150% 缩放下会发虚。换成 GDI 之后文字按字体 hinting
直接画到目标设备，放大后依然锐利。

内存占用实测（125% 缩放、1100×720 窗口）：

| 状态 | 工作集 | private | 进程数 |
| --- | --- | --- | --- |
| 播放 | 133 MB | 113 MB | 1 |
| 空闲 | 47 MB | 33 MB | 1 |

这两个数是**磁盘上的 exe 原地运行**时从任务管理器读的，不含任何打包处理。
v0.1.0 是 7 个进程共 442 MB 工作集。

## 构建

前置条件：Rust 1.77+、NSIS 3.x（仅打包安装包时需要）。

Windows SDK（提供 `rc.exe`）**可选**：它只用来把图标和版本信息嵌进 exe。
`build.rs` 找不到 `rc.exe` 时只打印一条告警并继续，产物照样能编译能跑，
只是没有图标、`FileVersion` 为 0。调试时不必先装 SDK。

```powershell
# 1. 获取 libmpv（按 scripts/libmpv.lock.json 锁定的版本下载并校验哈希）
powershell -ExecutionPolicy Bypass -File scripts\fetch-libmpv.ps1

# 2. 编译
cd src-tauri
cargo build --release

# 3. 打安装包 + 免安装 zip
cd ..
powershell -ExecutionPolicy Bypass -File scripts\package.ps1
```

`package.ps1` 自己会校验 exe 里嵌的 `FileVersion` 与 `Cargo.toml` 一致，
所以**发布前必须装上 Windows SDK**——否则那个校验会因为版本号为 0 而中止，
打包流程不会静默产出一个「控制面板显示 0.0.0」的包。

产物在 `artifacts/`：

| 文件 | 说明 |
| --- | --- |
| `VideoView-Setup-<版本>.exe` | NSIS 安装程序，压缩后约 33 MB（解压约 116 MB） |
| `VideoView-<版本>.zip` | 免安装压缩包，解压即用 |

两个产物都含 `libmpv-2.dll`，装出来/解压出来共约 116 MB，
压缩后 33 MB（安装包）/ 46 MB（zip）——体积基本全是这个 DLL。

`package.ps1` 在铺暂存目录时会再校验一次 `libmpv-2.dll` 的 SHA-256，
并核对 exe 里嵌的文件版本号与 `Cargo.toml` 是否一致，任一不符就中止——
发布出去的包不该出现「文件名写 0.2.0、资源里却是 0.1.0」这种不一致。

## 运行

```powershell
# 打开文件
.\video-view.exe "D:\path\to\video.mp4"

# 或双击 exe 后拖入视频文件
```

## 测试

```powershell
cd src-tauri

# 格式解码、播放控制等集成测试
cargo test

# 静态检查与格式，必须 0 warning
cargo clippy --all-targets
cargo fmt --check
```

界面是 GDI 自绘的，`cargo test` 覆盖不到「像素真的贴对了地方」。
发布前手动跑一次端到端验收（会抢前台窗口、发送键鼠事件）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\verify-ui.ps1
```

## 系统要求

- Windows 10 64-bit 或更高
- 无需安装 Rust / Node.js（运行时只需 `libmpv-2.dll`，已随包分发）

## 供应链

播放核心是被 `LoadLibrary` 进本进程并执行的第三方二进制，因此下载环节
不做任何版本选择：

- 版本与两个 SHA-256（压缩包、解压出的 DLL）锁定在
  [`scripts/libmpv.lock.json`](scripts/libmpv.lock.json)
- `fetch-libmpv.ps1` 只下载锁定 tag 下的锁定资产，逐层校验哈希，
  任何一层对不上就中止，不会把未校验的二进制写进构建输入路径
- 本地已有的 `libmpv-2.dll` 每次也会校验；不匹配时脚本拒绝覆盖，
  需要显式 `-Force`
- `package.ps1` 在打包时再校验一次，确保进包的 DLL 就是锁定的那一份

因此「谁构建、什么时候构建」不影响产物里的播放核心是哪一份字节。

## 安全相关行为

- 不读取 `%APPDATA%\mpv` 下的用户配置，也不加载任何 mpv 脚本
  （`config=no`、`load-scripts=no`、`ytdl=no`）
- 只接受磁盘上确实存在的普通文件。mpv 的 `loadfile` 第一个参数是 URL 而不
  只是文件名，校验点放在 `MpvPlayer::load_file` 这一个入口上
- 正式版只在 exe 同级查找 `libmpv-2.dll`，不向上遍历父目录，
  也不探测子目录——否则 `C:\Program Files\` 或用户目录里任意一个同名
  DLL 都可能被优先加载（DLL 劫持）
- 安装程序不改动文件关联。想关联的第一次启动时自己选一次即可

## 许可证

GPL-3.0-or-later，详见 [LICENSE](LICENSE)。

第三方组件许可见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。