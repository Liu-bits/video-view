# 打包 VideoView
#
# 一次跑完：编译 release、铺暂存目录、出安装包、出免安装 zip。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts/package.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/package.ps1 -SkipBuild
#
# 产物落在 artifacts/：
#   VideoView-Setup-<版本>.exe   NSIS 安装程序
#   VideoView-<版本>.zip          免安装压缩包，解压即用
#
# 产物里必须自带 libmpv-2.dll：对方机器上没有 Rust / Node，也没有系统级
# 的 mpv，少了这个 DLL 装完就是打不开。所以它是不是在暂存目录里、哈希对不对，
# 都要在打包脚本里再确认一次，不能只信构建时拷贝过。

param(
    # 跳过 cargo build，直接用现有的 target\release\video-view.exe
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

# 原生命令的退出码要自己判，不能让 PowerShell 替我们判。
#
# PowerShell 7.3 引入 $PSNativeCommandUseErrorActionPreference，7.4 起**默认 $true**：
# 原生命令退出码非 0 会直接变成一条终止性错误（NativeCommandError），
# 于是 $LASTEXITCODE = ... 那一行根本执行不到——
# 「ffmpeg 失败 -> 记下来 -> 汇总 -> exit 1」这套判定会被整个旁路，
# 用户看到的是一条裸的 PowerShell 错误记录，而不是我们设计好的失败信息。
# Windows PowerShell 5.1 不认这个变量，赋个值只是普通赋值，无副作用。
$PSNativeCommandUseErrorActionPreference = $false

# 控制台默认代码页不是 UTF-8，中文提示会显示成乱码
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }

$Root = Split-Path -Parent $PSScriptRoot
$TauriDir = Join-Path $Root "src-tauri"
$Artifacts = Join-Path $Root "artifacts"
$Stage = Join-Path $Artifacts "stage"

# ---- 版本号：单一来源是 Cargo.toml，资源里的版本号也是构建时嵌进去的 --------
# 只在 [package] 表里找 version。
#
# 直接全文搜第一个 `version = "..."` 靠的是「[package] 一定排在依赖前面」
# 这个约定。它不成立的那一天——有人为了可读性把 [dependencies] 挪到上面，
# 或者将来引入 workspace——读到的就是某个依赖的版本。而版本号往下决定了
# 产物文件名、注册表项、NSIS 拿到的 -DVERSION，全错而且**不报错**，
# 直到用户装完发现控制面板显示的版本和文件名对不上才暴露。
#
# 用 `[package]` 到下一个 `[` 之间这一段做锚，依赖表再靠前也影响不到。
$CargoToml = Join-Path $TauriDir "Cargo.toml"
$CargoText = Get-Content -LiteralPath $CargoToml -Raw -Encoding UTF8
$PackageTable = [regex]::Match($CargoText, '(?ms)^\[package\][^\S\r\n]*(.*?)(?=^\[|\z)')
if (-not $PackageTable.Success) {
    throw "在 $CargoToml 里找不到 [package] 表"
}
$VersionMatch = [regex]::Match($PackageTable.Groups[1].Value, '(?m)^[^\S\r\n]*version[^\S\r\n]*=[^\S\r\n]*"([^"]+)"')
if (-not $VersionMatch.Success) {
    throw "无法从 $CargoToml 的 [package] 表里读出 version（若用了 version.workspace 继承，这里要改成解析 cargo metadata）"
}
$Version = $VersionMatch.Groups[1].Value
Write-Host "版本：$Version"

# ---- 校验 app.manifest 的程序集版本与 Cargo.toml 一致 ------------------------
# 这处和下面的 exe FileVersion 是**两个独立**的版本号，都能被外部看到：
#
#   * `assets/app.manifest` 的 `assemblyIdentity/@version` —— build.rs 不替换它，
#     它是被 rc 原样嵌进 exe 的程序集清单版本。Windows 用它做程序集绑定，
#     某些界面（安装器列表、事件查看器）读的是它。
#   * `app.rc` 的 `FILEVERSION` / `FileVersion` —— 由 build.rs 从
#     `CARGO_PKG_VERSION` 注入，文件属性和控制面板读的是它。
#
# 两者不一致时对外就是一个「文件名 0.3.0、程序集清单 0.2.0」的 exe，而
# FileVersion 校验**抓不到**（它只看后者）。app.rc 的注释里写着「Cargo.toml
# 和 rc 脚本各写一遍的必然结果是某次只改了一处」，manifest 正是第三个必须
# 手动同步的地方，所以在这里补上检查。
$Manifest = Join-Path $TauriDir "assets\app.manifest"
if (-not (Test-Path -LiteralPath $Manifest)) {
    throw "找不到 $Manifest"
}
$ManifestMatch = [regex]::Match(
    [System.IO.File]::ReadAllText($Manifest),
    'assemblyIdentity[\s\S]*?version="([^"]+)"')
if (-not $ManifestMatch.Success) {
    throw "无法从 $Manifest 里读出 assemblyIdentity/@version"
}
$ManifestVersion = $ManifestMatch.Groups[1].Value
$ExpectedManifest = $Version + '.0'
if ($ManifestVersion -ne $ExpectedManifest) {
    throw @"
app.manifest 的 assemblyIdentity/@version 是 $ManifestVersion，应为 $ExpectedManifest。
build.rs 不会替换这个值，必须手动同步 src-tauri/assets/app.manifest。
（它和 exe 的 FileVersion 是两个独立的版本号，只改一处不会被下面的检查发现。）
"@
}

# ---- 校验嵌进 exe 的版本号与 Cargo.toml 一致 ---------------------------------
# app.rc 的 FILEVERSION / FileVersion 由 build.rs 从 CARGO_PKG_VERSION 注入，
# 所以这里不需要（也不该）去同步 app.rc —— 漏的是重新 build。症状是
# 「控制面板还显示上一版、文件名已经是新版」。
$Exe = Join-Path $TauriDir "target\release\video-view.exe"
if (-not (Test-Path -LiteralPath $Exe)) {
    throw "找不到 $Exe（是否忘了 -SkipBuild？）"
}
$FileVer = (Get-Item -LiteralPath $Exe).VersionInfo.FileVersion
if ($FileVer -notmatch ("^" + [regex]::Escape($Version) + "(\.|$)")) {
    throw @"
exe 里的文件版本号是 $FileVer，与 Cargo.toml 的 $Version 不一致。
FileVersion 由 build.rs 从 CARGO_PKG_VERSION 注入，所以要重新
  cargo build --release
（用了 -SkipBuild 时最容易漏这一步）。
"@
}

# ---- 编译 -------------------------------------------------------------------
if (-not $SkipBuild) {
    # 上一次调试可能还开着播放器，锁住 target\release\video-view.exe，
    # 不先关掉的话 cargo 只会报一句「拒绝访问 os error 5」，看不出是谁占着
    $Running = @(Get-Process -Name "video-view" -ErrorAction SilentlyContinue)
    if ($Running.Count -gt 0) {
        Write-Host "结束正在运行的 video-view（$($Running.Count) 个）..."
        $Running | Stop-Process -Force
        Start-Sleep -Milliseconds 500
    }

    Push-Location $TauriDir
    try {
        Write-Host "cargo build --release ..."
        & cargo build --release
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build --release 失败，退出码 $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
}

# ---- 铺暂存目录 -------------------------------------------------------------
# 目标目录是脚本自己算出来的新建目录，但仍然按「先确认再写」的来：
# 清空前校验它确实是 artifacts 底下的 stage 目录，不是一个被指到别处的路径。
$ArtifactsFull = [System.IO.Path]::GetFullPath($Artifacts).TrimEnd('\')
$StageFull = [System.IO.Path]::GetFullPath($Stage)
if (-not $StageFull.StartsWith($ArtifactsFull + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "暂存目录不在 artifacts 之下，拒绝清空：$StageFull"
}
if (Test-Path -LiteralPath $StageFull) {
    Remove-Item -LiteralPath $StageFull -Recurse -Force
}
New-Item -ItemType Directory -Path $StageFull -Force | Out-Null

Copy-Item -LiteralPath $Exe -Destination (Join-Path $StageFull "video-view.exe") -Force

# libmpv 必须和 exe 同目录，且哈希必须是锁定文件里那个版本
$Lock = Get-Content -LiteralPath (Join-Path $PSScriptRoot "libmpv.lock.json") -Raw -Encoding UTF8 | ConvertFrom-Json
$DllCandidates = @(
    (Join-Path $TauriDir "libmpv-2.dll"),
    (Join-Path $TauriDir "target\release\libmpv-2.dll")
)
$Dll = $DllCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $Dll) {
    throw "找不到 libmpv-2.dll，先跑 scripts/fetch-libmpv.ps1"
}
$DllHash = (Get-FileHash -LiteralPath $Dll -Algorithm SHA256).Hash.ToLowerInvariant()
if ($DllHash -ne $Lock.dllSha256) {
    throw @"
libmpv-2.dll 哈希不匹配，拒绝打包：
  实际：$DllHash
  期望：$($Lock.dllSha256)
"@
}
Copy-Item -LiteralPath $Dll -Destination (Join-Path $StageFull "libmpv-2.dll") -Force
Write-Host ("libmpv-2.dll 哈希校验通过（{0:N1} MB）" -f ((Get-Item $Dll).Length / 1MB))

Copy-Item -LiteralPath (Join-Path $Root "THIRD_PARTY_NOTICES.md") -Destination $StageFull -Force
# MUI 的许可页只认 .txt/.rtf/.html，仓库里的 LICENSE 没有扩展名
Copy-Item -LiteralPath (Join-Path $Root "LICENSE") -Destination (Join-Path $StageFull "LICENSE.txt") -Force

# ---- 安装包 -----------------------------------------------------------------
# 查找顺序：机器级安装优先，per-user 的目录最后。
#
# `%LOCALAPPDATA%\tauri\NSIS` 是 Tauri 装的 NSIS，普通用户对该目录有完全
# 写权限。任何能在这个用户上下文里跑点东西的人都可以往那儿放一个
# makensis.exe，而本脚本会执行它、并让它往 artifacts/ 写文件——用的人
# 自己都不一定知道用的是哪个 makensis。Program Files 下的那几份只有
# 管理员能改，优先用它们等于把这条供应链收紧。
# 真的只剩 per-user 那份时也照用（下面会把选中哪个打印出来），总比没有。
#
# `ProgramW6432` 要排在最前面：在 64 位 PowerShell 里 `$env:ProgramFiles`
# 会被 WOW64 重定向到 `Program Files (x86)`，所以只写 `$env:ProgramFiles\NSIS`
# 的话，装在 `C:\Program Files\NSIS` 的那份永远找不到。
# `ProgramW6432` 在 32 位进程上是空的，正好被下面的过滤去掉。
$Makensis = @(
    "$env:ProgramW6432\NSIS\makensis.exe",
    "$env:ProgramFiles(x86)\NSIS\makensis.exe",
    "$env:ProgramFiles\NSIS\makensis.exe",
    "$env:LOCALAPPDATA\tauri\NSIS\makensis.exe"
) | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1

$SetupExe = Join-Path $Artifacts "VideoView-Setup-$Version.exe"
if ($Makensis) {
    Write-Host "makensis：$Makensis"
    Push-Location $TauriDir
    try {
        # 脚本里有中文注释，makensis 默认按系统 ANSI 读会直接报
        # "Bad text encoding" 而中止。
        # 版本号用 -D 传，输出路径由 installer.nsi 里的 !define 决定——
        # makensis 的 -XOutFile 实测没能覆盖脚本内的 OutFile，等于没传。
        & $Makensis "/INPUTCHARSET" "UTF8" "-DVERSION=$Version" "installer.nsi"
        if ($LASTEXITCODE -ne 0) {
            throw "makensis 失败，退出码 $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
    # 显式确认产物落在预期路径。makensis 成功退出但写到别处去了是有可能的
    # —— installer.nsi 用 `!define OUT_FILE` 指定输出，而 makensis 的
    # `-XOutFile` 实测覆盖不了它。少了这一步，脚本会去读一个不存在的
    # $SetupExe，报出来的是「找不到文件」，跟真正的原因（产物在别的路径）
    # 指向完全不同。
    if (-not (Test-Path -LiteralPath $SetupExe)) {
        $Actual = @(Get-ChildItem -LiteralPath $Artifacts -Filter "VideoView-Setup-*.exe" -ErrorAction SilentlyContinue)
        # 按可能性排序列出原因。`!define OUT_FILE` 那条排在最后：
        # 在 makensis 的工作目录仍是 src-tauri 的前提下 `..\artifacts\...`
        # 本来就是对的，版本对不上才是更常见的原因。
        throw @"
makensis 返回 0，但预期产物 $SetupExe 不存在（期望的版本号是 $Version）。

最可能的原因，按可能性排序：
  1. 版本号没传进去 —— installer.nsi 里的 `!ifndef VERSION` 兜底成了硬编码值，
     于是产物名里的版本与 Cargo.toml 对不上。确认调用里带了 -DVERSION=$Version。
  2. 产物被上一次运行留下的同名文件盖住/挪走了。上面列出了 artifacts 下现有的安装包。
  3. installer.nsi 的 `!define OUT_FILE` 被改成了别的路径。它的相对路径基准是
     .nsi 所在目录（src-tauri\），不是调用时的工作目录。

artifacts 下现有的安装包：$($Actual.Name -join ', ')
"@
    }
    Write-Host ("已生成：{0}（{1:N1} MB）" -f $SetupExe, ((Get-Item $SetupExe).Length / 1MB))
}
else {
    Write-Warning "没找到 makensis.exe，跳过安装包，只出 zip。装 NSIS 3.x 后重跑本脚本即可。"
}

# ---- 免安装 zip -------------------------------------------------------------
# 兜底：有人不想装东西，或者被杀软/组策略挡了安装程序，解压即用同样能跑。
$Zip = Join-Path $Artifacts "VideoView-$Version.zip"
if (Test-Path -LiteralPath $Zip) {
    Remove-Item -LiteralPath $Zip -Force
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
# 只打包 exe 与 libmpv-2.dll，把 116MB 的 DLL 再压一次能省掉一大半
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $StageFull, $Zip, [System.IO.Compression.CompressionLevel]::Optimal, $false)
Write-Host ("已生成：{0}（{1:N1} MB）" -f $Zip, ((Get-Item $Zip).Length / 1MB))

# 暂存目录不进仓库，但留着方便人肉检查产物内容
Remove-Item -LiteralPath $StageFull -Recurse -Force

Write-Host ""
Write-Host "产物列表："
Get-ChildItem -LiteralPath $Artifacts | ForEach-Object {
    "  {0,-40} {1,10:N1} MB" -f $_.Name, ($_.Length / 1MB)
}
