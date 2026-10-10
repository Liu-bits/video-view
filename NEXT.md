# NEXT.md

> **动手改代码前先读这个文件。** 约定见 [`AGENTS.md`](AGENTS.md)。

## 一句话现状

**0.9.0 已完成（书签 + 截图路径）。工作区干净：`cargo check --all-targets`、
`cargo fmt --all --check`、`cargo clippy --all-targets` 全绿，
`cargo test --lib` 235 项全过（基线 206）。**

下一版是 **1.0.0 —— 稳定性**。

---

## 当前阶段：1.0.0 —— 稳定性

### 这一版要解决什么问题

不是「加功能」，是**长时间开着不崩、不泄漏**。用户第 5 条诉求（「继续优化
内存占用、CPU 占用、响应速度」）落到三条**可验证**的指标：

| 指标 | 目标 | 现状 |
|---|---|---|
| 内存 | 连续播放 2 小时，工作集不持续增长 | 未知，待测 |
| CPU | 空闲（暂停 / 无文件）时 < 1% | 未知，待测 |
| 崩溃 | 连续播放 2 小时不崩 | 未测 |

**第一件事是测量，不是改代码。** `scripts/measure-usage.ps1` 已经在了，
先用它拿一份基线再决定改哪里 —— 没有基线就动手，改完也说不清到底有没有用。

### 怀疑方向（按可能性排序）

1. **GDI 对象泄漏**。最长寿的嫌疑对象：
   - `ui.rs` 的后备位图（`BackBuffer`）在每次 `relayout` 时重建 ——
     `release()` 每条路径都走到了吗？
   - `surface.rs` 的画面子窗口
   - **最快的测法**：任务管理器里给 `video-view.exe` 加一列「GDI 对象」，
     盯着看。数字只涨不落就是泄漏，而且 5 分钟就能得到答案
2. **`tick` 频率**。现在 `TICK_MS = 250`，空闲时它也每 250ms 醒一次去读
   `time-pos`。**先测空闲 CPU 再决定降不降** —— 本来就只有 0.3% 的话，
   降频是白改，还牺牲进度条的跟手感
3. **拖动进度条时的 seek 节流**。`Dragging::Seek` 期间是不是每帧都在发
   seek？是的话 mpv 每次都要重新定位 + 解码，这是「一拖就飙 CPU」的典型
   成因
4. **mpv 的 `video-sync` / `audio-sync`**。默认值对本地文件够用，但动它
   会影响音画同步行为 —— **属于要单独一版来做的事**，别混进稳定性版

### 验收标准

- [ ] 连续播放 2 小时不崩（**必须真的跑**，不是看着代码说没问题）
- [ ] 工作集曲线在 30 分钟后基本走平（允许缓涨，不允许线性增长）
- [ ] 空闲 CPU < 1%（任务管理器与 `measure-usage.ps1` 两个来源互证）
- [ ] 拖动进度条时 CPU 不出现持续 100% 单核
- [ ] `cargo test --lib` ≥ 235 全绿；新增测试要能**在修复前失败**
- [ ] 手上有**改造前 / 改造后**两份测量数据，否则不算验证过

---

## 上一版（0.9.0）交付了什么

- **书签**：`F2` 或右键菜单 ▸ 书签。添加时先暂停 → 弹窗问名字（预填当前
  位置、打开即全选）→ 存进数据目录的纯文本。子菜单列**当前文件**的书签，
  选中即跳转；「删除本文件全部」在没有书签时置灰
- **截图可以选路径**：弹一个预填好的输入框，默认仍是图片目录里的
  `<视频名>-videoview-<时间戳>.png`，回车即接受
- **模态文本输入框**（`prompt.rs`）：书签与截图共用
- 单元测试 206 → 235；clippy 0 警告；新增 CI 与工程规范脚手架

---

## 已知坑（**踩一个记一条**）

### 0.9.0 这一轮踩到的

1. **windows crate 0.62 的模块划分反直觉，别凭记忆写 import。**
   实测（在 crate 源码里逐个查过定义位置）：

   | 符号 | 真实位置 |
   |---|---|
   | `SS_LEFT` | `Win32::System::SystemServices`（**不在** `UI::Controls`） |
   | `EnableWindow` / `SetFocus` | `Win32::UI::Input::KeyboardAndMouse` |
   | `IsDialogMessageW` | `Win32::UI::WindowsAndMessaging`（`EM_SETSEL` 才在 `UI::Controls`） |
   | `GetDpiForWindow` | `Win32::UI::HiDpi` |
   | `HBRUSH` / `GetStockObject` / `COLOR_*` / `DEFAULT_GUI_FONT` | `Win32::Graphics::Gdi` |

   查证方法（比反复试快得多）：
   ```
   Grep "pub const SS_LEFT:" 于
   ~/.cargo/registry/src/*/windows-0.62.2/src/Windows/Win32/
   ```

2. **这一版的 `GetMessageW` 返回裸 `BOOL`，不是 `Result`。**
   `0` = `WM_QUIT`，`-1` = 出错 —— 两种都该退出循环，所以判 `<= 0`。

3. **`IDOK` / `IDCANCEL` 是 `MESSAGEBOX_RESULT` newtype**，不是整数。
   不能写 `match id { IDOK => ... }`（常量 newtype 当不了整数 pattern），
   也不能 `IDOK as u16`。用 `id == IDOK.0` 比较。

4. **句柄形参普遍是 `Option<HWND>` / `Option<HMENU>`。**
   `IsWindow()`、`GetDlgItem()`、`PostMessageW()` 都收 `Option`；
   而 `GetDlgItem` 返回的是 `Result<HWND>`，不是 `Option`。

5. **样式常量分属三种类型，混用编译不过。**
   `WS_*` 是 `WINDOW_STYLE`，`ES_*` / `BS_*` 是裸 `i32`，
   `SS_LEFT` 是 `STATIC_STYLES`。合并时各自取 `.0` 或 `as u32`。

6. **`HBRUSH(整数)` 要显式转成指针**：这一版里 `HBRUSH` 是
   `*mut c_void` 的 newtype。Win32 用「小于 16 的整数」表示系统颜色，
   所以写 `HBRUSH((COLOR_x.0 + 1) as *mut std::ffi::c_void)`。

7. **测试辅助函数里「按位置取东西」会被新功能打破。**
   `menu.rs` 的 `last_submenu` 取的是「最后一个子菜单」，理由是 0.8.0 时
   「时间」最后加；0.9.0 把**书签**子菜单接在它后面，4 条测试一起挂。
   已改成按标题取（`timing_submenu`）。
   **凡是按位置取的辅助函数，都要先想「会不会被加在后面的东西顶掉」。**

8. **测试数据自己写错，比实现错更难发现。**
   `bookmark.rs` 的「坏条目在中间也不影响后面的」里，本该写 `5 2 11`
   （路径 5 字节 + 名字 2 字节）却写成了 `5 4 11` —— 数据自相矛盾，
   那一条也被当成坏数据跳过，测试只剩 1 条。**改测试数据前先手算一遍
   它是否自洽。**

9. **`grep -c $'\r'` 在这台机器的 Git Bash 里不可靠**（对纯 LF 文件也返回
   总行数，看着像「每行都有 CR」）。检测行尾请用字节级
   `bytes.count(b"\r\n")`，或 `git ls-files --eol`。

### 更早的（保留，别删）

- **不要在 `build()` 内部定义 `fn`** —— Win32 菜单构建器的辅助函数一律平级
- **`Iterator::eq()` 返回 `bool`，不是迭代器** —— 链式接不上
- **改了语义要立刻重跑相关测试** —— 改菜单启用条件 = 改契约
- **PowerShell 5.1 读无 BOM 的 UTF-8 `.ps1` 会按 GBK 解**，中文全变乱码
- **别在 `.ps1` 里用 `cmd` 的 `cd /d` 语法**（在 pwsh 里会静默失败）

---

## 后续版本计划

- **1.1.0 —— 硬件解码的可见性**
  - 「硬件解码没生效」从「只能手动打开诊断面板才知道」变成主动提示
  - 菜单里能切换解码后端，并当场看到结果
  - **依据**：现有代码已经是厂商中立的 —— `gpu-context=d3d11` +
    `hwdec=auto`，且**刻意不设** `d3d11-adapter`（那是前缀匹配，绑死
    「Intel」会让 AMD / NVIDIA 机器匹配失败，而 mpv 匹配失败时只发一条
    warning 就回落默认适配器，等于没配）。所以这一版不是「加厂商支持」，
    而是**让静默降级变得可见**

---

## 硬性约束（每次发版都要检查）

- 不动 `v0.1.0`–`v0.9.0` 的 tag，不动 `release/*` 分支
- 不直接向 `main` 提交，走 feature 分支 + fast-forward
- 推送用隔离的 `GIT_CONFIG_GLOBAL`，不改全局 `~/.gitconfig`
- 界面文案中英双语必须同时改（`lang.rs` 少一个字段就编译不过）
- 不为小问题引新依赖（`resume.rs` / `bookmark.rs` 都是手写解析器）
- `.workbuddy/` 等本地私有目录**绝不入库**
