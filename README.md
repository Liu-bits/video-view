# VideoView v0.4.0

轻量级 Windows 桌面视频播放器，纯 Win32 自绘界面 + libmpv 播放核心。

## 功能

- 打开本地视频文件（拖放 / 文件对话框 / 命令行参数）
- 播放 / 暂停 / 停止
- 进度条拖动跳转，显示当前时间与总时长
- 音量控制 / 静音（音量与窗口尺寸跨启动保存）
- 画面自适应窗口：缩放窗口时视频区跟随重排，按原始宽高比缩放
- 影院模式（隐藏控制栏）与 `F11` 全屏
- **解码诊断面板**（`I`）：硬解是否生效、是否零拷贝、丢帧、实际帧率，
  外加一条能照着改的结论。诊断报告 `Ctrl+C` 一键复制
- 截图（`S`），存到图片目录
- 快捷键：`空格` 播放/暂停、`Ctrl+O` 打开、`←` / `→` 快退快进、
  `↑` / `↓` 音量、`,` / `.` 逐帧、`[` / `]` 变速、`0`–`9` 百分比跳转、
  `Home` / `End` 开头结尾、`M` 静音、`S` 截图、`I` 诊断、
  `Ctrl+C` 复制诊断报告、`?` 快捷键总览、`F` 影院模式、`F11` 全屏、
  `Esc` 退出影院模式 / 全屏；
  单击画面切换播放暂停，双击画面切换影院模式
- 支持 MP4、MKV、AVI、MOV、WebM、FLV、WMV 等常见容器
- 支持 H.264、HEVC、VP9、AV1、MPEG-4 等视频编码
- 支持 AAC、MP3、Opus、Vorbis、FLAC 等音频编码
- 界面按显示器 DPI 自动缩放，没有开关也没有「高质量/兼容」之类选项
- 界面语言在安装时选择（English / 简体中文），选择结果记进注册表，
  程序每次启动按它显示；便携运行可用 `VIDEOVIEW_LANG=en` 或 `zh-CN` 覆盖
- 控件宽度按当前语言的**实测文字宽度**计算，不是写死的常量 ——
  中文两字与英文单词长度差很多，写死必然有一边被截断

## 技术栈

- **界面**：原生 Win32 + GDI 手绘，没有浏览器、没有 WebView、没有前端工程
- **语言**：Rust（`windows` crate 直接调 Win32 API）
- **播放核心**：libmpv（运行时动态加载 `libmpv-2.dll`，构建期零 C 依赖）
- **视频窗口**：Win32 子窗口（`CreateWindowExW`），mpv 直接渲染到 HWND

界面这一层是 v0.2.0 的重写。原来用的是 Tauri + WebView2，
Chromium 进程光内存就占掉 320MB，而且文字受位图缩放影响，
放大后在 125% / 150% 缩放下会发虚。换成 GDI 之后文字按字体 hinting
直接画到目标设备，放大后依然锐利。

内存与占用实测（125% 缩放，H.264 640×480，`hwdec-current = d3d11va`，
磁盘上的 exe 原地运行，不含任何打包处理）：

| 状态 | 窗口 | 工作集 | private | CPU（单核） | GPU 3D | GPU 解码 | 共享显存 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 空闲 | 1082×673 | 49 MB | 33 MB | 0.1–3% | ~0 | ~0 | 7 MB |
| 空闲 | 1882×953 | 47 MB | 33 MB | 0.1–3% | ~0 | ~0 | 13 MB |
| 播放 | 1082×673 | 135 MB | 115 MB | 9.9% | 4.0% | 0.7% | 46 MB |
| 播放 | 1882×953 | 142 MB | 122 MB | 9.2% | 7.4% | 0.8% | 64 MB |

连续播放 60 秒工作集无增长（130.4 → 125.7 MB，末值反而更低，说明没有泄漏）。
v0.1.0 是 7 个进程共 442 MB 工作集。

**关于播放时的 135 MB**：其中 **62.7 MB 是三个 Intel 驱动模块**
（`igc64.dll` / `igd11dxva64.dll` / `igd10umdgen11.dll`），是 D3D11 渲染路径的
固定开销，不是解码本身。实测关掉硬件解码内存几乎不变（110.8 → 106.4 MB），
因为 `vo=gpu` + `gpu-context=d3d11` 照样会加载它们 —— 而硬件解码更省 CPU，
所以保留。空闲态 47–49 MB。

**关于 GPU 那一列**：负载几乎全在 3D 引擎（把视频放大铺满窗口），不在解码。
3D 占用随窗口面积上涨，因为每帧要做一次全屏缩放。本表用的是仓库里最大的
测试素材（640×480），**不代表 1080p/4K** —— 那种分辨率下 Video Decode
会高一个数量级。

### 解码诊断

按 `I` 打开。它显示的不是「播放器自己的状态」而是 mpv 报的观测项，并且
给一条**结论**而不是把数字抄给你：

| 显示 | 含义 |
| --- | --- |
| `d3d11va + 零拷贝` | 硬解生效，且解码帧直接进了 GPU、没有 CPU 往返 |
| `软解` | **硬解没生效**，mpv 已静默退回软件解码 |
| `25.0/25.0` | 实际帧率 / 文件标称帧率。明显小于后者说明解码跟不上 |
| `0 / 0` | 解码器丢帧 / 显示端丢帧。启动瞬间丢几帧是正常的，持续上涨不是 |
| `结论` 一行 | 上面几项综合出来的判断 + 具体可查的原因 |

`Ctrl+C` 把同样的内容加上版本、mpv API 版本、物理核数、**显卡名**、
未接入的虚拟显示器数量、DPI 一起复制到剪贴板，可以直接贴进 issue。

**为什么要专门做这个**：D3D11VA 在某个适配器上做格式探测失败时，mpv
**不发任何 error 或 warning**，只是把 `hwdec` 从白名单里拿掉继续用软解。
在低端机上这是最难排查的性能问题 —— 画面能播，但 CPU 跑满、机器发烫，
用户只会觉得「这个播放器慢」。典型触发场景是机器上装了虚拟显示器
（远程控制 / 投屏 / 采集软件会装 IddCx 虚拟适配器），它可能出现在 DXGI
枚举里，一旦被 Windows 选成默认适配器就是这个结果。

本机实测：硬解生效（`d3d11va` + 零拷贝），但 `EnumDisplayDevicesW` 返回
**13 个**显示设备、其中 12 个未接入，绝大多数是远程投屏软件装的
`GameViewer Virtual Display Adapter`。这类机器上「静默降级」的概率明显更高，
所以诊断报告把未接入设备的数量单独列成一行。

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
并核对两处版本号与 `Cargo.toml` 是否一致，任一不符就中止：

- exe 里嵌的文件版本号（`FileVersion`，由 `build.rs` 从 `CARGO_PKG_VERSION`
  注入）
- `assets/app.manifest` 的程序集清单版本（`assemblyIdentity/@version`，
  `build.rs` **不**替换它，必须手动同步）

发布出去的包不该出现「文件名写 0.4.0、资源里却是 0.3.0」这种不一致。
两个检查缺一不可：只查 FileVersion 是抓不到清单版本漂移的。

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