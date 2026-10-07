; VideoView 安装包脚本
;
; 由 scripts/package.ps1 调用：
;   makensis /INPUTCHARSET UTF8 -DVERSION=0.7.0 installer.nsi
;
; 产物是一个零外部依赖的安装程序：播放核心 libmpv-2.dll 会装到 $INSTDIR，
; 播放器 exe 与它同目录、运行时按同目录查找。对方的机器上不需要装
; Python / Node / Rust，也不需要手工处理 DLL。
;
; 路径约定：makensis 以本脚本所在目录（src-tauri\）为基准，${BUILD_DIR} 指向
; scripts/package.ps1 铺好的暂存目录，里面只有要装进 $INSTDIR 的文件。

; 版本号由 scripts/package.ps1 用 -DVERSION 传进来，保证和 Cargo.toml 一致。
; 这里保留的默认值只是为了让脚本能被手工执行时也能跑起来。
!ifndef VERSION
  !define VERSION "0.7.0"
!endif
!define OUT_FILE "..\artifacts\VideoView-Setup-${VERSION}.exe"
; 暂存目录：由 scripts/package.ps1 铺好，里面只有要装进 $INSTDIR 的文件
!define BUILD_DIR "..\artifacts\stage"

; ---- 变量声明 ----
;
; NSIS 要求 `Var` 必须在**使用它的 Function / Section 之前**声明。
; 这一段放在文件最前面而不是跟着各自的逻辑走，就是因为 `.onInit`
; （全文第一个 Function）要用 `$MakeDesktopShortcut` —— 声明跟在
; 语言页那一段的话，`.onInit` 里的 `StrCpy` 会报 "aborting creation
; process"，而报错信息里**不说是哪个变量**，只看行号很难反应过来。
;
; 桌面快捷方式的勾选框句柄与选择结果。
;
; 结果由 `${NSD_OnClick}` 回调写进 `$MakeDesktopShortcut`，**不是**在
; `SecMain` 里去读控件状态 —— `Page custom` 只有页面回调、没有 leave 钩子，
; 等到 Section 执行时 `nsDialogs::Show` 已经返回、对话框已销毁，
; `NSD_GetState` 拿到的句柄失效。下面 `$LangChoice` 那段注释写的正是这个坑。
Var ShortcutBox
Var MakeDesktopShortcut

; 快捷方式落地的位置与名字。`$DESKTOP` 由 NSIS 解析成当前用户的桌面
; （落「公用桌面」还是「用户桌面」由 NSIS 与安装权限决定，我们不插手）。
!define DESKTOP_SHORTCUT "$DESKTOP\VideoView.lnk"

!define PRODUCT "VideoView 视频播放器"

Name "${PRODUCT}"
OutFile "${OUT_FILE}"
Unicode true
InstallDir "$PROGRAMFILES64\VideoView"
InstallDirRegKey HKLM "Software\VideoView" "InstallDir"
RequestExecutionLevel admin

; ---- 注册表视图：明确用 64 位 ----
;
; 放在 `.onInit` 里而不是顶层：`SetRegView` 是**运行时**命令，在顶层
; （Section 之外）makensis 报 "command SetRegView not valid outside Section
; or Function"。
;
; 为什么需要：makensis 是 **32 位** 进程，而 `RequestExecutionLevel admin`
; 只提升权限、**不切换注册表视图**。默认视图下
;
;     WriteRegStr HKLM "Software\VideoView" "Language" "$LANGUAGE"
;
; 写进去的是 `HKLM\SOFTWARE\Wow6432Node\VideoView`，而 `video-view.exe` 是
; 64 位（`InstallDir` 是 `$PROGRAMFILES64`），它用未重定向的
; `HKLM\Software\VideoView` 去读 —— 读不到。症状就是「安装器里选了简体中文，
; 程序界面还是英文」。
;
; 顺带记实测结论：本机 NSIS 3.11 实测把 HKCU 的键写进了 **64 位视图**
; （不是 Wow6432Node），也就是说这个版本的默认值恰好是对的。但那是个没有
; 文档承诺的默认值 —— 依赖它等于把「语言选择能否生效」押在「用户装的 NSIS
; 版本恰好默认 64 位」上。显式写一行是零成本，去掉这个依赖。
;
; 注意这条**同时**影响 `InstallDirRegKey`（在 Section 之前就要读），
; 所以它必须在 `.onInit` 里——`.onInit` 在任何页面显示之前跑完。
Function .onInit
  SetRegView 64
  ; 桌面快捷方式默认**不**建。
  ;
  ; 反过来（默认 1）的话 `/S` 静默安装会往每个用户的桌面上放图标 ——
  ; 无人值守的部署不该做这件事。交互安装时 `LangPage` 会把它改成 1。
  ;
  ; 这一行不能省。NSIS 的 `Var` 初值是空串，`${If} $MakeDesktopShortcut == 1`
  ; 碰上空串的比较行为不保证，而「静默装完多一个图标」是个只有用户看得见
  ; 的小毛病 —— 不如从一开始就明确写 0。
  StrCpy $MakeDesktopShortcut 0
FunctionEnd
ShowInstDetails show
ShowUninstDetails show

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName"     "${PRODUCT}"
VIAddVersionKey "CompanyName"     "VideoView"
VIAddVersionKey "FileDescription" "${PRODUCT}"
VIAddVersionKey "FileVersion"     "${VERSION}"
VIAddVersionKey "LegalCopyright"  "GPL-3.0-or-later"
VIAddVersionKey "ProductVersion"  "${VERSION}"

; 安装包最大的一块是 libmpv-2.dll（约 116MB 可执行代码），LZMA solid
; 能压掉一大半，装起来快得多；代价是安装时 CPU 多花几秒。
SetCompressor /SOLID lzma
SetCompressorDictSize 64

BrandingText "VideoView ${VERSION} · GPL-3.0-or-later"

!define MUI_ICON "icons\icon.ico"
!define MUI_UNICON "icons\icon.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\video-view.exe"
!define MUI_FINISHPAGE_RUN_TEXT "启动 VideoView"

!include "MUI2.nsh"
!include "FileFunc.nsh"

; 许可正文来自暂存目录里由 package.ps1 复制好的副本（仓库里的 LICENSE 没有扩展名，
; 而 MUI 只认 .txt/.rtf/.html）
; ---- 界面语言 ----
;
; 安装器让用户在 English / 简体中文 之间选，选中的记进注册表，
; 程序启动时读它（见 src/lang.rs）。
;
; ## 为什么是自绘页
;
; 直觉上应该写 `!insertmacro MUI_PAGE_LANGUAGE`，但**这个宏在 MUI2 里已经
; 不存在**了：MUI 时代有，MUI2 把它移除了（连同 `Pages\Language.nsh` 整个
; 文件）。实测 makensis 直接报
;
;     !insertmacro: macro named "MUI_PAGE_LANGUAGE" not found!
;
; 更根本的是：MUI2 没有「按 $LANGUAGE 切换安装器界面」的机制了。
; `MUI_LANGUAGE` 现在只负责建立语言表（供 `${MUI_TEXT_*}` 之类的常量用），
; 不会再自动弹语言选择页。所以选择页必须自己画——下面用 nsDialogs。
;
; 注意 `MUI_PAGE_*` 必须全部写在 `MUI_LANGUAGE` **之前**：MUI2 的
; `MUI_LANGUAGEEX` 宏在插入时会检查「MUI_PAGE_* 是否已经写过」，没写就报
; warning。顺序反了不会编译失败，但语言表不会生效——症状是
; `$LANG_SimpChinese`、`$MUI_BTN_NEXT` 等常量全部变成 "unknown variable"，
; 页面上出现的是变量名本身而不是文案。

; 语言页必须是**第一页**，所以 `Page custom` 写在所有 `MUI_PAGE_*` 之前。
; NSIS 按声明顺序决定页面顺序——写在后面的话，实测它会排在「安装完成」页
; 之后（探测时抓到的顺序是：许可 -> 目录 -> 正在安装 -> 完成 -> 语言页）。
; 语言选择出现在「安装完成」之后没有任何意义。
;
; `MUI_LANGUAGE` 仍然必须写在**所有** `MUI_PAGE_*` 之后（理由见下），
; 而它不是页面声明、不影响页面顺序。所以顺序是：
;   Page custom（第一页）-> MUI_PAGE_*（后续页）-> MUI_LANGUAGE（语言表）
Page custom LangPage

!insertmacro MUI_PAGE_LICENSE "${BUILD_DIR}\LICENSE.txt"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; 语言表的名字要与 `lang::Lang::registry_name` 一致：English / SimpChinese。
;
; 顺序的含义：静默 /S 安装不经过选择页，NSIS 会用**第一张表**，所以
; English 写在前面 = 静默安装默认英文。这也是产品想要的方向。
;
; 必须放在 `MUI_PAGE_*` 全部之后：MUI2 的 `MUI_LANGUAGEEX` 宏在插入时会检查
; 「是否已经声明过 MUI_PAGE_*」，没写就报 warning。顺序反了不会编译失败，
; 但语言表不生效——症状是 `$LANG_SimpChinese`、`$MUI_BTN_NEXT` 等常量
; 全部变成 "unknown variable"，页面上直接显示变量名。
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

!include "nsDialogs.nsh"

; ---- 语言选择页 ----
;
; 页面声明 `Page custom LangPage` 在本节之前（必须排在所有 MUI_PAGE_* 前面，
; 理由见那里）。
;
; 标题字号：这里**不设字体**。想要「跟 MUI 其余页面一样的大标题」，得先从
; 模板里取出 FontBig 那个 HFONT 再 WM_SETFONT，而 nsDialogs 没有提供这个宏、
; MUI2 也没有（实测整个 Contrib\Modern UI 2 里没有任何 FONT 相关宏）。
; 为此写几十行 Win32 取字体代码不值得：这一页只有两行说明加两个单选框，
; 默认字体完全够读。
Var LangDialog
Var LangEn
Var LangZh
Var LangChoice

; 页面上的文案。
;
; 用变量而不是 `$\"…$\"` 字面量：`$\"` 会把转义引号**原样留在字符串里**，
; 实测控件文字变成 `"Choose`、`"This`（只有首字符被截断，因为 NSIS 把它当
; 一个参数处理），页面上直接显示多出来的引号。
; `StrCpy $S "带空格"` 再把 `$S` 当参数传才是对的。
Var S


; ---- 语言选择页：一个普通的自定义页 ----
;
; ## 为什么不用 `MUI_PAGE_COMPONENTS`
;
; 试过，两个原因都不成立：
;
; 1. `MUI_PAGE_LANGUAGE` 在 MUI2 里已被移除（连 `Pages\Language.nsh` 都
;    没有了），makensis 直接报 "macro not found"。
; 2. 拿 components 页来改也不行。它的 `MUI_COMPONENTSPAGE_INTERFACE` 与
;    `MUI_PAGEDECLARATION_COMPONENTS` 是一对配套的 define，重复定义会报
;    "already defined"；而只改其一会让页面的 SHOW 函数仍然去
;    `FindWindow` 找 1006/1017/1032 这几个控件 ID——它们一个都不存在，
;    结果是页面上留一个空白列表框，还要另外找东西盖住它。
;
; 所以直接用最原始的 `PageEx custom`：MUI2 的其它页（MUI_PAGE_DIRECTORY
; 等）与自定义页是可以混排的，Back / Next / Cancel 三个按钮自己画。
; 代价是按钮位置和文案要自己管，换来的是这一页与其他 MUI 页完全解耦。

; 声明方式：`PageEx custom` + `PageExEnd` 里直接写 `PageCallbacks`。
; 没有 `PageExCustom` 这个命令——那是网上旧示例里的写法，NSIS 3.x
; 会直接报 "Invalid command"。
;
; MUI2 自己的 welcome 页（见 Contrib\Modern UI\System.nsh）也是这个写法，
; 照它来不会踩版本差异。
; 声明方式：`PageEx custom` + `PageExEnd` 里直接写 `PageCallbacks`。
; 没有 `PageExCustom` 这个命令——那是网上旧示例里的写法，NSIS 3.x
; 会直接报 "Invalid command"。
;
; MUI2 自己的 welcome 页（见 Contrib\Modern UI\System.nsh）也是这个写法，
; 照它来不会踩版本差异。
;
; PageCallbacks 有两种形式：`(create, leave)` 或 `(pre, show, leave)`。
; 这里要用三参数那种——页面内容要在 show 回调里用 nsDialogs 画。
;
; 为什么不用标签（如 `LangPage: PageEx custom`）：NSIS 会把 PageCallbacks
; 的参数原样拼进页面表，前面带标签就变成跳转标签，报
; "Label declaration not valid outside of function"。
;
; `custom` 页的标准写法就是 nsDialogs 官方教程里那个：
;
;     Page custom LangPage
;     Function LangPage   ; 这是**唯一**的回调，nsDialogs::Create/Show 都在里面
;
; 之前那几版写法都被 makensis 拒了，各有各的原因，记在这里免得再踩：
;
;   * `PageEx custom` + `PageCallbacks a b c`（三参数）报 "Usage: PageCallbacks
;     ([creator] [leave]) | ([pre] [show] [leave])"。`custom` 页没有 pre/show
;     分开的那套语义，只有「进入页面时调用一次」的单个回调。
;   * `PageExCustom` 报 "Invalid command"——那是旧示例里的写法，NSIS 3.x 没有。
;   * `LangPage: PageEx custom` 报 "Label declaration not valid outside of
;     function"，PageCallbacks 的参数会被当命令拼进页面表，不能带标签。
;   * `MUI_PAGE_LANGUAGE` 报 "macro not found"——MUI2 已移除该宏。
;   * `MUI_PAGE_CUSTOMPAGE` 同样不存在（MUI2 只提供 welcome / license /
;     components / directory / instfiles / finish）。
;
; 所以用最朴素的 `Page custom`，回调里自己建对话框。代价是 Back / Next /
; Cancel 要自己画——下面就是这么做的。
; 这份 NSIS（3.11 + Tauri 附带的 nsDialogs）比常见的用法**旧一代**，
; 以下几点是实测（逐个宏编译探测）出来的，不是照抄网上的例子：
;
;   * `${NSD_Create*} x y w h text` 五个参数**在创建时就定位**，没有
;     `NSD_SetPosition`（探测报 "Invalid command"）。所以所有坐标必须在
;     创建那一刻就写对，之后不能再挪。
;   * `${NSD_Check} 控件HWND` 只收**一个**参数。传 `对话框 控件` 会报
;     "requires 1 parameter(s), passed 2"。
;   * 没有 `NSD_Click`（报 "Plugin function not found"），回车绑定得用
;     `SendMessage 对话框 WM_COMMAND IDOK`。
;   * 没有 `NSD_OnClick`。按钮行为只能在页面函数末尾统一按 `$0`（上一个
;     创建的控件是 Next）判断，或者干脆用控件 ID 走 WM_COMMAND。
;   * `${NSD_OnChange} / ${NSD_OnClick} 控件回调函数` 是可用的，但
;     `__NSD_OnControlEvent` 内部会 `Push $0` / `Push $1`，所以**不能**在
;     紧挨着的下一行写 `Pop $0`：宏参数会连同后面的行一起被吃掉，
;     makensis 报 `Pop expects 1 parameters, got 6`。
;   * 没有 `$LangChoice` 这种「最后点击的单选框序号」变量（自己声明一个
;     `Var LangChoice` 就可以）。
;   * MUI2 的自定义页没有「描述区」宏可用（`MUI_FUNCTION_DESCRIPTION_*`
;     绑在 components 页上），所以提示文字自己画。
;
; 单选框被点中时的回调。回调在对话框还活着的时候触发，所以这里记下的值
; 是可靠的。
;
; 为什么不能「离开页面时再去问控件」：`nsDialogs::Show` 一返回，对话框就被
; 销毁，那时 `NSD_GetState` 拿到的是失效句柄。实测那次读出来的是 `5046576`
;（一个被复用的 HWND 数值），于是无论用户点哪个都判成 English，而且全程
; 不报错。这正是「在错误的时机问已经死掉的对象」的典型症状：不是返回
; FALSE，而是返回了一个看起来像整数的错误答案。
;
; 也试过 `${NSD_CreateTimer}` 定时轮询（`${NSD_GetState}` 本身可用），实测
; 一次都没被调用过——这版 nsDialogs 的定时器在自定义页上不工作。
; `${NSD_OnClick}` 可用，所以用回调。
Function onPickedEnglish
  StrCpy $LangChoice 0
FunctionEnd

Function onPickedChinese
  StrCpy $LangChoice 1
FunctionEnd

; 带空格的长文案**必须**先 `StrCpy` 进变量再当参数传（见上面 `Var S` 的说明）。
Function LangPage
  nsDialogs::Create 1018
  Pop $LangDialog
  ${If} $LangDialog == error
    Abort
  ${EndIf}

  ; 文案先过变量，见上面 Var S 的说明。
  StrCpy $S "Choose the VideoView interface language"
  ${NSD_CreateLabel} 20 25 340 20 $S

  ; 说明。这一句不能省：不说清「选择会被记住」，用户面对一个没有预览的
  ; 选项只能靠猜，而这一页又没有事后更改的入口。
  StrCpy $S "This choice is saved and used every time the app starts."
  ${NSD_CreateLabel} 20 48 340 16 $S

  ; 两个单选框。同一组里第二个必须用 AdditionalRadioButton：单选框的互斥
  ; 是靠「同组相邻创建」实现的，两个独立的 RadioButton 会各自独立勾选。
  ;
  ; **选择结果由 `${NSD_OnClick}` 回调记进 $LangChoice**，不是在离开页面时
  ; 去读控件状态——原因见上面 `onPickedChinese` 的说明。
  ;
  ; 默认选中第一项（English），与上面语言表的顺序一致，也就是与「静默安装
  ; 用第一张表」保持同一个默认值。
  StrCpy $LangChoice 0
  ${NSD_CreateFirstRadioButton} 30 80 200 16 English
  Pop $LangEn
  ${NSD_Check} $LangEn
  ${NSD_OnClick} $LangEn onPickedEnglish

  StrCpy $S "简体中文"
  ${NSD_CreateAdditionalRadioButton} 30 105 200 16 $S
  Pop $LangZh
  ${NSD_OnClick} $LangZh onPickedChinese

  ; 桌面快捷方式。
  ;
  ; 和语言单选框放在**同一页**而不是新开一页，理由：两者都是「安装选项」，
  ; 为一个勾选框多开一页要写 40 行 nsDialogs 样板，而这一页已经有标题和
  ; 说明文字了，位置顺手就够。
  ;
  ; **默认勾选。** 用户明确要了桌面快捷方式，而这是个纯增量：不勾也不会
  ; 影响任何功能，只是桌面上少一个图标；勾了的话多一个图标，删掉也容易。
  ;
  ; 位置放在单选框下面 20px：视觉上「单选组」和「勾选框」之间要有一点
  ; 间隔，否则读起来像第三个单选项。
  StrCpy $S "Create a shortcut on the desktop"
  ${NSD_CreateCheckbox} 30 132 300 16 $S
  Pop $ShortcutBox
  ; 默认值在**创建控件之后**设置，不是 CreateCheckbox 的第四个参数 ——
  ; 那个参数是「文本」，不是「是否勾选」。用 `${NSD_Check}` 才不会
  ; 静默失败（之前 `NSD_GetState` 那条注释里说的坑就是这个性质）。
  ${NSD_Check} $ShortcutBox
  ${NSD_OnClick} $ShortcutBox onToggleShortcut

  ; 勾选结果在页面显示**之前**先定好初值。
  ;
  ; `${NSD_Check}` 只改控件状态，**不会**触发 `${NSD_OnClick}` —— 所以
  ; 如果不在这里 `StrCpy $MakeDesktopShortcut 1`，用户勾着框一路点 Next
  ; 而回调一次都没跑，SecMain 读到的是 0，勾选框形同虚设。这正是
  ; 「控件显示的状态」和「我们记下来的状态」必须分开设的地方。
  StrCpy $MakeDesktopShortcut 1

  ; Cancel / Next。这一页是第一页，所以**不画** Back：留一个点不动的按钮
  ; 在这里比不画更糟，用户会以为程序卡住了。
  StrCpy $S "Cancel"
  ${NSD_CreateButton} 20 150 70 22 $S
  StrCpy $S "Next >"
  ${NSD_CreateButton} 200 150 70 22 $S

  ; 回车 = Next。这版 nsDialogs 没有 `nsDialogs::Click`（探测报
  ; "Plugin function not found"），只能把 WM_COMMAND/IDOK 直接发给对话框。
  SendMessage $LangDialog ${WM_COMMAND} 1 0

  nsDialogs::Show
FunctionEnd

; 勾选框变了就记下来。
;
; `${NSD_OnClick}` 在用户点它时才调，不在创建后调，也不随页面切换调 ——
; 所以初值必须由 LangPage 自己 `StrCpy`（见上面那段注释）。
Function onToggleShortcut
  ${NSD_GetState} $ShortcutBox $MakeDesktopShortcut
FunctionEnd

Section "VideoView" SecMain
  ; 语言选择的结果在这里落地。
  ;
  ; `$LangChoice` 由语言页的 OnChange 回调写入（0 = English，1 = 简体中文），
  ; 不是在这里去问控件：`Page custom` 只提供单个页面回调、没有 leave 钩子，
  ; 等到这里时 `nsDialogs::Show` 已经返回、对话框已被销毁，
  ; `NSD_GetState` 拿到的句柄失效，于是**无论点哪个都是 English**——
  ; 这正是这个 bug 最初的现场：不报错，但选择被静默丢掉。
  ${If} $LangChoice == 1
    StrCpy $LANGUAGE "SimpChinese"
  ${Else}
    StrCpy $LANGUAGE "English"
  ${EndIf}

  ; 先结束正在运行的进程，再动 $INSTDIR。
  ;
  ; 顺序反了会怎样：RMDir /r 删不掉的正是被占用的那两个文件（exe 与它
  ; 加载着的 libmpv-2.dll），NSIS 不报错也不中断，于是「升级安装」表面上
  ; 成功，实际上 $INSTDIR 里还是**上一版**的 exe + DLL——用户装完看到的
  ; 还是旧版，而且没有任何提示。taskkill 失败（进程本来就没开）也没关系，
  ; 它的退出码只写进日志。
  ;
  ; `/IM` 是按映像名全局匹配，不看路径：用户同时开着便携 zip 版时会被一起
  ; 收掉。对这个产品来说可以接受——总比留一个占着文件、导致升级没生效的
  ; 进程要好，而且两版本来就是同一个软件。
  nsExec::ExecToLog 'taskkill /F /IM video-view.exe'
  ; 必须等：taskkill /F 在目标进程终止后才返回，句柄回收基本同步，
  ; 这 500ms 是给极慢的机器（杀进程时 mpv 的 vo 线程还在 D3D 里）留的余量。
  ; 实测：先 taskkill 再 RMDir，exe 的写入时间戳会被刷新；不 taskkill 的话
  ; 安装器照样返回 0 但时间戳原封不动，也就是没真正升级。
  Sleep 500

  ; 整目录清掉再铺，避免上一版残留的文件留在磁盘上
  RMDir /r "$INSTDIR"
  SetOutPath "$INSTDIR"
  File "${BUILD_DIR}\video-view.exe"
  File "${BUILD_DIR}\libmpv-2.dll"
  File "${BUILD_DIR}\THIRD_PARTY_NOTICES.md"
  File "${BUILD_DIR}\LICENSE.txt"

  WriteRegStr HKLM "Software\VideoView" "InstallDir" "$INSTDIR"
  ; 用户在语言选择页选的语言。程序启动时读这个值决定界面语言
  ; （见 src/lang.rs::registry_lang）。
  ;
  ; $LANGUAGE 由自定义页的 `SetLanguage` 设为 English / SimpChinese，
  ; 与 `lang::Lang::registry_name` 一致。写它而不是写死某个值，是为了让
  ; 「安装器显示的语言」和「程序界面语言」永远一致——之前没有这个键，
  ; 程序只能猜系统语言，于是会出现「英文安装过程 + 中文界面」。
  ;
  ; /S 静默安装不经过语言页，$LANGUAGE 是 NSIS 启动时按系统语言选出的
  ; 表名，没有就落到第一张表（English）。
  WriteRegStr HKLM "Software\VideoView" "Language" "$LANGUAGE"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\VideoView" "DisplayName"     "${PRODUCT}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\VideoView" "DisplayVersion"  "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\VideoView" "Publisher"       "VideoView"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\VideoView" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\VideoView" "InstallLocation" "$INSTDIR"
  ; 控制面板里显示的大小，单位 KB
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\VideoView" "EstimatedSize" "$0"

  ; 文件关联不在安装阶段动。安装程序擅自改系统设置对用户是突袭，
  ; 想关联的第一次启动时自己选一次即可。

  ; ---- 桌面快捷方式 ----
  ;
  ; 装完 `$INSTDIR` 的文件之后才建：指向的 exe 必须已经存在，顺序反了会
  ; 留下一个指向空路径的快捷方式，点开报「找不到项目」。
  ;
  ; 写入前先删一次：`CreateShortCut` 在文件已存在时是覆盖，但升级安装时
  ; 上一版可能只写了一半（崩溃 / 断电），先删能覆盖到那种情况。
  ;
  ; `/S` 静默安装走不到自定义页，`$MakeDesktopShortcut` 保持 `.onInit` 里
  ; 设的初值 0，所以**静默安装不建快捷方式** —— 这正是想要的：无人值守的
  ; 部署不该往用户桌面上放图标。
  Delete "${DESKTOP_SHORTCUT}"
  ${If} $MakeDesktopShortcut == 1
    ; 五个参数，不写满八个。
    ;
    ; 实测（makensis 3.x，Tauri 自带的那份）：**写满**八参数
    ; 里有两个连续空参数 `"" ""` 的那种形式编译不过，报错落在 `${EndIf}`
    ; 那一行、只说 "aborting creation process"，不说是哪个参数 —— 看起来
    ; 像是 `${If}` 配对出了问题。逐个参数数量单独编译验证过：
    ;
    ;   2 参数 (link, target)                    -> OK
    ;   3 参数 (+ "")                             -> OK
    ;   5 参数 (+ "", icon, 0)                    -> OK
    ;   6 参数 (+ "", "", icon, 0)                -> FAIL
    ;
    ; 所以这里用五参数：第三个 `""` 是 `parameters`（空 = 启动不带命令行），
    ; 第四个是 `icon.file`，第五个是 `icon_index`。把 `icon.file` 指向 exe
    ; 自己，于是任务栏和 Alt-Tab 与桌面图标是同一张，不会出现「快捷方式
    ; 一个样子、启动起来又变一个」。
    CreateShortCut "${DESKTOP_SHORTCUT}" "$INSTDIR\video-view.exe" "" "$INSTDIR\video-view.exe" 0
  ${EndIf}

SectionEnd

Section "Uninstall"
  ; 先结束进程再删文件，否则正在播放时 exe 被占用会删不掉
  nsExec::ExecToLog 'taskkill /F /IM video-view.exe'
  Sleep 500

  ; 桌面快捷方式**必须删**。`RMDir /r "$INSTDIR"` 删不到桌面 —— 快捷方式在
  ; 桌面上，不在安装目录里。漏了这一步的话，卸载完桌面上还留着一个点开
  ; 报「找不到项目」的图标，而用户在「应用和功能」里看不到任何异常。
  ;
  ; 装了两次（不同用户 / 换了安装目录）也只删一份是刻意的：删多了会误删
  ; 另一个用户自己建的同名快捷方式，而那种情况比留一个死图标更糟。
  Delete "${DESKTOP_SHORTCUT}"

  RMDir /r "$INSTDIR"

  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\VideoView"
  DeleteRegKey HKLM "Software\VideoView"
SectionEnd
