# 更新日志

本项目遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

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

[0.2.0]: https://github.com/Liu-bits/video-view/releases/tag/v0.2.0
[0.1.0]: https://github.com/Liu-bits/video-view/releases/tag/v0.1.0
