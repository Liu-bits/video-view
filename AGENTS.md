# AGENTS.md

VideoView 的工作约定。**任何人在动手改代码之前必须先读 [`NEXT.md`](NEXT.md)。**

---

## 必读：NEXT.md

`NEXT.md` 是这个项目**唯一**的进度真相来源。它记录：

- 当前正在做哪个版本
- 这一版做到哪一步了
- 剩下什么没做、按什么顺序做
- 上一版留下了什么坑

不要凭记忆、不要看 git log 去猜进度 —— `NEXT.md` 就是为此存在的。
如果发现 `NEXT.md` 与代码实际状态不符，**先修 `NEXT.md`**，再继续干活。

## 每完成一个阶段，必须改写 NEXT.md

一个「阶段」= 一个版本（`0.9.0`、`1.0.0`…）或一次修复批次。阶段完成的
判定标准是：

- `cargo check` 干净
- `cargo test --lib` 全绿（0 失败）
- `cargo fmt --all --check` 干净
- 要发版的话：`package.ps1` 退出码 0、tag 已打、release 已发

**阶段一结束，立刻把 `NEXT.md` 改写成下一个版本的计划**，而不是等到下次
想起来。新版本那一节至少要写清楚：

1. 这一版要解决什么问题（不是「加功能」，是「解决什么问题」）
2. 明确的验收标准（能写成测试的写成测试）
3. 已知的技术约束 / 前人踩过的坑

`NEXT.md` 里的「已知坑」一节尤其重要 —— 它记的是**上一版实际踩到的东西**，
不是猜的。踩一个坑就记一条，比事后翻聊天记录便宜得多。

---

## 需要持续维护的文件

这个项目的维护成本，大头不在写代码，而在**同一件事有好几个地方要改**。
下面按「改了 A 就必须改 B」把文件分组。**动手前扫一眼自己这次会碰到哪几组。**

### 一、进度与文档

| 文件 | 作用 | 什么时候必须改 |
|---|---|---|
| `NEXT.md` | **唯一的进度真相来源** | 每个阶段结束立刻改写；发现与代码不符立即修 |
| `CHANGELOG.md` | 面向用户的版本变更（「加 / 改 / 明确不做」三段式） | 每次发版，必须与该版本的 tag 同步 |
| `README.md` | 项目门面 + 用户可见功能清单 | 加/改任何用户可见功能时；**首行版本号随发版更新** |
| `CONTRIBUTING.md` | 人类协作者的构建 / 验证 / 发版流程 | 流程变化时（尤其改 `ci.yml` 后） |
| `AGENTS.md` | AI 协作约定（本文件） | 约定本身变化时 |
| `THIRD_PARTY_NOTICES.md` | 第三方许可声明 | 增删第三方二进制或依赖时（例如换 libmpv 版本） |

### 二、版本号 —— 一次发版必须同步改 4 处

| 位置 | 说明 |
|---|---|
| `src-tauri/Cargo.toml` | `[package] version`，**这是源头** |
| `src-tauri/Cargo.lock` | 跟着 cargo 自动走，别手改 |
| `src-tauri/installer.nsi` | 安装包版本。**保持 CRLF**（`.gitattributes` 里是 `-text`） |
| `src-tauri/assets/app.manifest` | exe 清单里的版本 |
| `README.md` 首行 | `# VideoView vX.Y.Z` |

漏改的代价很具体：`build.rs` 从 `Cargo.toml` 读版本写进 exe 资源段，
`.nsi` 决定安装包显示什么 —— 两处不一致时，用户报的版本号对不上实际代码。

### 三、界面文案

| 文件 | 规则 |
|---|---|
| `src-tauri/src/lang.rs` | `en()` 与 `zh_cn()` **必须同时改**。少一个字段编译不过 —— 刻意设计，**别用 `#[cfg]` 绕过** |

按钮文字若能用系统自带的 `IDOK` / `IDCANCEL`（见 `prompt.rs`），就不要新增翻译项 ——
系统会按用户语言自动翻译，自己写死第三份表在非中英文系统上是错的。

### 四、工程规范与 CI

| 文件 | 作用 | 什么时候改 |
|---|---|---|
| `.github/workflows/ci.yml` | CI 门禁，4 个 job（fmt / clippy / check / test） | 增删检查项；**clippy 从软卡点转强制时** |
| `src-tauri/rust-toolchain.toml` | 工具链通道固定为 `stable` | 几乎不动（不要钉死版本号） |
| `src-tauri/rustfmt.toml` | 格式基线 | 只允许 **stable 支持的选项**，nightly-only 的加进来会被静默忽略 |
| `src-tauri/clippy.toml` | lint 阈值 | 巨型文件拆完之后可以收紧 |
| `.editorconfig` ↔ `.gitattributes` | **一对**：前者管编辑器敲什么，后者管入库存什么 | 改一个必须核对另一个，否则行尾反复抖动 |
| `.gitignore` | 忽略规则 | 新增本地私有目录时 |

### 五、构建与脚本

| 文件 | 作用 | 什么时候改 |
|---|---|---|
| `src-tauri/Cargo.toml` 的 `windows` features | 每个 Win32 API 都挂在一个 feature 后面 | **引入新 Win32 API 时**，漏加就报「在此作用域找不到」 |
| `scripts/libmpv.lock.json` | libmpv 版本锁（下载校验用） | 升级 libmpv 时 |
| `scripts/fetch-libmpv.ps1` | 取 libmpv dll（`libmpv-2.dll` 不入库） | 下载源或校验方式变化时 |
| `scripts/package.ps1` | 打包 exe + zip + NSIS 安装包 | 发布流程变化时 |
| `scripts/verify-ui.ps1` | UI 自动校验（**全部项须通过**） | 界面 / 菜单 / 快捷键变化时 |
| `scripts/make-test-media.ps1` | 生成测试媒体 | 新增编解码格式时 |
| `scripts/measure-usage.ps1` | 内存 / CPU 测量 | 测量口径变化时 |

> **`verify-ui.ps1` 必须带 BOM。** PowerShell 5.1 读无 BOM 的 UTF-8 `.ps1`
> 会按 GBK 解，中文全变乱码。另外别在 `.ps1` 里用 `cmd` 的 `cd /d` 语法 ——
> 在 pwsh 里会变成 `D:\d\Video-view`，静默失败。

### 六、测试与产物

| 路径 | 说明 |
|---|---|
| `src-tauri/tests/media/` | 测试媒体（**入库**，约 23 M）。新增格式时要加样本，别删旧的 |
| `src-tauri/tests/*.rs` | 集成测试。需要 libmpv dll + 媒体文件，**只在本地跑**，不进 CI |
| `artifacts/` | 本地打包产物。**不入库**（GitHub Release 才是发布通道），发版后清理旧版本 |

### 七、本地私有目录 —— 绝不入库

`.workbuddy/`（本地工作记忆）、`.claude/`、`.cursor/` 等 AI 工具数据目录
属于个人笔记，只对写它的那台机器有意义。已写进 `.gitignore`。

**提交前用 `git status --short` 扫一眼，确认没有这类目录出现。**

---

## 语言

- 与用户沟通、写文档、写注释：**中文**
- 代码标识符、提交信息：跟随既有惯例
- 软件界面文案：**双语**（见上面「需要持续维护的文件 · 三」）

## 验证纪律

- 声称「做完了」之前，必须真的跑过 `cargo check` 与 `cargo test --lib`，
  并看到真实输出。写完文件 ≠ 代码能跑。
- `cargo fmt --all --check` 是 CI 强制卡点，本地也要干净。
- 环境原因跑不了（比如 clippy 二进制被占用），**明说「这部分未验证」及原因**，
  不要含糊过去。
- 新功能要有测试。测试要能**在修复前失败**：一个恒过的测试什么也证明不了。
- `cargo test --lib` 的**测试数是契约**（当前基线见 `NEXT.md`），只增不减。

## 不要做的事

- 不要动 `v0.1.0`–`v0.8.0` 这些已发布的 tag，也不要改 `release/*` 分支
- 不要直接向 `main` 提交。走 feature 分支 → fast-forward 回 main
- 不要把 `.workbuddy/` 等本地私有目录提交上去
- 不要改全局 `~/.gitconfig`；推送用隔离的 `GIT_CONFIG_GLOBAL`
- 不要在没有确认的情况下删改用户文件；迁移/删除这类不可逆操作先问
- 不要引入新的运行时依赖来解决一个能自己写的小问题（这个项目的
  `resume.rs` / `bookmark.rs` 都是手写格式解析，就是为了不引 `serde`）

## 项目结构速查

```
.github/workflows/
  ci.yml        CI 门禁：fmt(强制) / clippy(软卡点) / check(强制) / test(强制)
src-tauri/
  rustfmt.toml        格式基线（只用 stable 选项）
  clippy.toml         lint 阈值
  rust-toolchain.toml 工具链 = stable 通道
  src/
  app.rs        主窗口、消息循环、界面状态、快捷键    （最大的文件，4431 行）
  mpv/          libmpv 的最小 FFI 封装
  menu.rs       右键菜单：命令 ID 空间 + 各子菜单构建
  lang.rs       全部界面文案（中英双语，改一处必须改两处）
  ui.rs         控制栏布局 / 绘制 / 命中测试（无状态）
  playlist.rs   播放列表、外挂字幕匹配
  resume.rs     上次播放位置记忆（文件，不是注册表）
  bookmark.rs   书签：名字 + 位置
  prompt.rs     模态文本输入框
  track.rs      音轨 / 字幕轨
  diag.rs       解码健康度诊断
  gpu.rs        显示适配器枚举（诊断报告用）
  cpu.rs        CPU 拓扑（解码线程数）
  settings.rs   持久化设置
  crashlog.rs   崩溃日志 + 数据目录
  folder.rs     文件夹扫描、自然排序
  surface.rs    mpv 画面子窗口
scripts/
  fetch-libmpv.ps1    取 libmpv（版本由 libmpv.lock.json 锁）
  package.ps1         打包 exe + zip + NSIS 安装包
  verify-ui.ps1       UI 自动校验（必须带 BOM）
  make-test-media.ps1 生成测试媒体
  measure-usage.ps1   内存 / CPU 测量
```
