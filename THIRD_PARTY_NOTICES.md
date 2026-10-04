# 第三方组件声明

VideoView 以 **GPL-3.0-or-later** 发布（见 [LICENSE](LICENSE)）。

选用 GPL 的原因：播放核心 libmpv 静态链接了 FFmpeg，
而支持 H.264 / HEVC 必需 GPL 授权的 `libx264` / `libx265`。
FFmpeg 的 LGPL 构建不包含这两个编码器，无法满足「格式兼容优先」的目标。

## 运行时随包分发

### libmpv

- 用途：播放核心。解复用、解码、音视频同步、视频渲染
- 来源：<https://github.com/zhongfly/mpv-winbuild>
- 许可证：**GPL-3.0-or-later**
  （构建时启用了 FFmpeg 的 GPL 组件：
  `libx264`、`libx265`、`libvpx`、`libaom`、`libdav1d`、`libass`）
- 形态：静态链接进 `libmpv-2.dll`，运行时由本程序动态加载

### FFmpeg

- 用途：解复用与编解码，静态链接在 `libmpv-2.dll` 内部
- 来源：<https://ffmpeg.org/>
- 许可证：LGPL-2.1-or-later 与 GPL-2.0-or-later 的混合，
  取决于启用的组件。本项目分发的是启用 GPL 组件的构建。

### Tauri

- 用途：应用外壳与 WebView 承载
- 来源：<https://tauri.app/>
- 许可证：MIT / Apache-2.0

### libloading

- 用途：运行时加载 `libmpv-2.dll`
- 来源：<https://crates.io/crates/libloading>
- 许可证：Apache-2.0 OR MIT

### rfd

- 用途：系统文件对话框
- 来源：<https://crates.io/crates/rfd>
- 许可证：MIT

### windows

- 用途：创建与定位 mpv 的视频输出窗口
- 来源：<https://crates.io/crates/windows>
- 许可证：MIT OR Apache-2.0

## 不随包分发

### Microsoft Edge WebView2 Runtime

- 用途：承载界面
- 说明：本仓库不分发该运行时。安装程序会在目标机器缺失时下载安装包。
- 来源：<https://developer.microsoft.com/microsoft-edge/webview2/>
- 许可：需遵循 Microsoft 的 WebView2 使用条款

## 构建期依赖

Rust crates 与 npm 包各自适用其上游许可证，
均不随 Release 分发。清单见：

- `src-tauri/Cargo.lock`
- `package-lock.json`
