# 更新日志

本项目遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

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

[0.1.0]: https://github.com/Liu-bits/video-view/releases/tag/v0.1.0
