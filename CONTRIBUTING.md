# 贡献指南

VideoView 是一个 Windows 桌面视频播放器：**纯 Win32 自绘界面 + libmpv 播放核心**，
没有 WebView、没有前端构建链。前端只有一个 exe，启动快、内存低、DPI 由系统原生处理。

技术栈：Rust 2021 / `windows` crate (0.62) / `libloading` 动态加载 libmpv / `rfd` 原生对话框。
界面文案中英双语，没有第三方 UI 框架。

---

## 1. 环境要求

| 需要 | 说明 |
|---|---|
| Windows 10/11 x64 | 项目只构建 Windows 目标，没有跨平台计划 |
| Rust stable ≥ 1.77 | 由 `src-tauri/rust-toolchain.toml` 固定通道，`Cargo.toml` 的 `rust-version` 是 MSRV 唯一真相来源 |
| rustfmt + clippy | 同上，工具链文件里已声明为必需组件 |
| Windows SDK | 仅影响图标与版本信息。缺了只告警不中断（`build.rs` 是防御性的） |
| libmpv-2.dll | **不入库**，用 `scripts/fetch-libmpv.ps1` 获取，版本由 `scripts/libmpv.lock.json` 锁定 |

---

## 2. 上手

所有 cargo 命令都在 **`src-tauri/`** 下执行（项目根是工作区，crate 在子目录）。

```powershell
git clone <repo> && cd src-tauri
powershell -ExecutionPolicy Bypass -File ..\scripts\fetch-libmpv.ps1   # 取 libmpv
cargo run                                                              # 构建并启动
```

`build.rs` 会把 `libmpv-2.dll` 复制到 exe 同级目录，所以 `cargo run` 开箱即用。

---

## 3. 验证纪律（最重要的一节）

**写完文件 ≠ 代码能跑。** 声称「做完了」之前必须真的跑过、看到真实输出。

| 命令 | 何时必须跑 | 通过标准 |
|---|---|---|
| `cargo fmt --all --check` | 每次提交前 | 退出码 0 |
| `cargo check --all-targets` | 每次提交前 | 退出码 0 |
| `cargo test --lib` | 每次提交前 | **0 失败** |
| `cargo clippy --all-targets` | 每次提交前 | **0 警告** |
| `scripts\verify-ui.ps1` | 碰了界面 / 菜单 / 快捷键 | **全部项通过**（项数随版本增长，以脚本输出为准，别在文档里写死数字） |
| `scripts\package.ps1` | 发版时 | 退出码 0，能出 zip / NSIS 安装包 |

### 单元测试数与「修复前会失败」

- `cargo test --lib` 的**测试数是契约**：当前基线 231（206 + 书签模块 25）。
  新增功能必须让这个数只增不减。
- 新测试要能**在修复前失败**。一个恒过的测试什么也证明不了 ——
  写完之后把修复代码临时注释掉，确认测试真的红了，再改回来。

### 三件 CI 管不到、只能在本地做的事

1. **集成测试**（`tests/*.rs`）：要真的加载 libmpv dll 并读 `tests/media/` 里的媒体文件。
2. **UI 校验**（`scripts/verify-ui.ps1`）：要真的起窗口、发菜单消息。runner 没有可交互桌面。
3. **打包**（`scripts/package.ps1`）：要 dll + NSIS，产物还要传 Release。

环境原因跑不了的（例如 clippy 二进制被占用），**明说「这部分未验证」及原因**，
不要含糊过去。

### Lint

`cargo clippy --all-targets` 是 **CI 强制卡点**，要求 **0 警告**。
0.9.0 收尾时清掉了最后 3 条（两处 `HINSTANCE` 的无用 `.into()`、
一处 `needless_borrow`）—— 门禁只有在干净的时候才立得住，
一条永远黄的检查只会训练人去忽略它。

`clippy.toml` **只调阈值，不启用额外 lint 组**：`app.rs` / `ui.rs` 这类
巨型文件上开激进 lint，输出会被「函数太长」「参数太多」淹没，而它们真正
指向的是**该拆文件**这个结构问题 —— 用 lint 兜结构问题，只会让真问题
被忽略。

---

## 4. 代码风格

- **格式全交给 rustfmt**，不要手工对齐、不要为了「看着整齐」调整换行。
  配置在 `src-tauri/rustfmt.toml`，**只用 stable 支持的选项**。
  想用 `imports_granularity` / `wrap_comments` 这类 nightly-only 项？
  不行 —— 那会导致「本地 nightly 格式化过、CI stable 检查不过」反复出现。
- **注释写「为什么」，不写「是什么」。** 代码本身说明做了什么；
  注释要留下的是「为什么是这个写法」和「不这么写会怎样」。
  本项目很多注释记的是踩过的坑（例如 `.nsi` 为什么必须是 `-text`），
  这类注释价值最高，别删。
- **界面文案中英双语必须同时改**（`lang.rs` 的 `en()` 与 `zh_cn()`）。
  少填一个字段编译不过 —— 这是刻意设计，**不要用 `#[cfg]` 绕过**。
- **不为小问题引新依赖。** `resume.rs` / `bookmark.rs` 都是手写格式解析，
  就是为了不引 `serde`。加依赖前先问「这个能不能自己写 200 行搞定」。

---

## 5. 提交信息

跟随既有风格，看 `git log` 对齐即可。要点：

- 一个提交做一件事，不要把重构和功能混在一起
- 标题说清**解决了什么问题**，正文补充**为什么这么解**（和注释同样的取向）
- 提交前跑过第 3 节的命令

---

## 6. 分支与发版

```
main                ← 只接受 fast-forward，不直接提交
feat/vX.Y.Z-主题     ← 功能分支
release/vX.Y.Z      ← 已发布版本，冻结
```

发版流程：

1. 改版本号：`Cargo.toml` / `Cargo.lock` / `installer.nsi` / `assets/app.manifest`
   （`.nsi` 保持 CRLF）
2. `scripts\package.ps1` 退出码 0
3. `git checkout -b feat/vX.Y.Z-主题` → 提交 → 回 `main` → `git merge --ff-only`
4. `git tag -a vX.Y.Z`
5. 推送 + `gh release create`

**推送时用隔离的 git 配置**，不要去改全局 `~/.gitconfig`：

```powershell
$env:GIT_CONFIG_GLOBAL = "$env:TEMP\vv-gitconfig-iso"
```

---

## 7. 不要做的事

- 不动 `v0.1.0`–`v0.8.0` 这些**已发布的 tag**，也不动 `release/*` 分支
- **不直接向 `main` 提交**，走 feature 分支 + fast-forward
- 删除 / 迁移用户文件这类不可逆操作，先问
- 不在 `.ps1` 里用 `cmd` 的 `cd /d` 语法（在 pwsh 里会静默失败）
- 不给无 BOM 的 UTF-8 `.ps1` 写中文（PowerShell 5.1 会按 GBK 解，中文全变乱码）

---

## 8. 目录速查

```
src-tauri/src/
  app.rs        主窗口、消息循环、界面状态、快捷键
  mpv/          libmpv 的最小 FFI 封装（动态加载，无构建期 C 依赖）
  menu.rs       右键菜单：命令 ID 空间 + 各子菜单构建
  lang.rs       全部界面文案（中英双语，改一处必须改两处）
  ui.rs         控制栏布局 / 绘制 / 命中测试（无状态）
  playlist.rs   播放列表、外挂字幕匹配
  resume.rs     上次播放位置记忆
  bookmark.rs   书签
  prompt.rs     模态文本输入框
  track.rs      音轨 / 字幕轨
  diag.rs       解码健康度诊断
  gpu.rs        显示适配器枚举
  cpu.rs        CPU 拓扑（解码线程数）
  settings.rs   持久化设置
  crashlog.rs   崩溃日志 + 数据目录
  folder.rs     文件夹扫描、自然排序
  surface.rs    mpv 画面子窗口
scripts/
  fetch-libmpv.ps1   取 libmpv（版本由 libmpv.lock.json 锁）
  package.ps1        打包 exe + zip + NSIS
  verify-ui.ps1      UI 自动校验（全部项须通过）
  make-test-media.ps1 生成测试媒体
  measure-usage.ps1  内存 / CPU 测量
```

**`NEXT.md` 是这个项目唯一的进度真相来源** —— 动手前先读它，做完了改它。
