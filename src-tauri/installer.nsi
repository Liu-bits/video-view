; VideoView 安装包脚本
;
; 由 scripts/package.ps1 调用：
;   makensis /INPUTCHARSET UTF8 -DVERSION=0.2.0 installer.nsi
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
  !define VERSION "0.2.0"
!endif
!define OUT_FILE "..\artifacts\VideoView-Setup-${VERSION}.exe"
; 暂存目录：由 scripts/package.ps1 铺好，里面只有要装进 $INSTDIR 的文件
!define BUILD_DIR "..\artifacts\stage"

!define PRODUCT "VideoView 视频播放器"

Name "${PRODUCT}"
OutFile "${OUT_FILE}"
Unicode true
InstallDir "$PROGRAMFILES64\VideoView"
InstallDirRegKey HKLM "Software\VideoView" "InstallDir"
RequestExecutionLevel admin
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
!insertmacro MUI_PAGE_LICENSE "${BUILD_DIR}\LICENSE.txt"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Section "VideoView" SecMain
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

SectionEnd

Section "Uninstall"
  ; 先结束进程再删文件，否则正在播放时 exe 被占用会删不掉
  nsExec::ExecToLog 'taskkill /F /IM video-view.exe'
  Sleep 500
  RMDir /r "$INSTDIR"

  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\VideoView"
  DeleteRegKey HKLM "Software\VideoView"
SectionEnd