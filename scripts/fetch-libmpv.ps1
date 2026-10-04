# 获取 libmpv-2.dll
#
# libmpv 是运行时动态加载的第三方二进制，不入库（见 .gitignore）。
# 首次克隆后执行一次本脚本，开发和打包才能正常找到播放核心。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-libmpv.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-libmpv.ps1 -Force
#
# 版本与哈希全部锁在 scripts/libmpv.lock.json 里，本脚本不做任何版本选择：
# 下载确切的 tag 下确切的资产，逐层校验 SHA-256，任何一层对不上就中止，
# 不会把未经校验的二进制放进构建输入路径。
#
# 为什么必须锁：libmpv-2.dll 会被 LoadLibrary 进本进程并执行，
# 等于把一个 116 MB 的可执行代码的来源完全交给上游。校验环节一旦缺失，
# 上游账号被顶替、release 资源被替换、或传输链路被动手脚，都会直接变成
# 「打开这个播放器就跑了别人的代码」。

param(
    # 重新下载并覆盖已有的 DLL（会先校验已有文件，校验不过则拒绝覆盖）
    [switch]$Force
)

$ErrorActionPreference = "Stop"

# 控制台默认代码页不是 UTF-8，中文提示会显示成乱码
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }

$Root = Split-Path -Parent $PSScriptRoot
$LockFile = Join-Path $PSScriptRoot "libmpv.lock.json"
$Dest = Join-Path $Root "src-tauri\libmpv-2.dll"

function Get-Sha256([string]$Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

if (-not (Test-Path -LiteralPath $LockFile)) {
    throw "缺少锁定文件：$LockFile"
}

$Lock = Get-Content -LiteralPath $LockFile -Raw -Encoding UTF8 | ConvertFrom-Json
foreach ($Field in @("repo", "tag", "asset", "archiveSha256", "dllSha256")) {
    if (-not $Lock.$Field) {
        throw "锁定文件缺少字段 $Field"
    }
}

Write-Host "锁定版本：$($Lock.repo) @ $($Lock.tag)"
Write-Host "资产      ：$($Lock.asset)"

# ---- 本地已有的 DLL：先校验，通过就到此为止 --------------------------------
# 顺带解决「本地这份文件是否可信」的问题，而不只是「下载的这份可不可信」。
if ((Test-Path -LiteralPath $Dest) -and (-not $Force)) {
    $Existing = Get-Sha256 $Dest
    if ($Existing -eq $Lock.dllSha256) {
        Write-Host "已存在且哈希匹配，跳过：$Dest"
        exit 0
    }
    throw @"
本地已有的 libmpv-2.dll 与锁定哈希不一致，疑似被替换或损坏：
  文件：$Dest
  实际：$Existing
  期望：$($Lock.dllSha256)
不做任何修改。确认来源可信后，用 -Force 重新下载覆盖。
"@
}

# ---- 下载 -------------------------------------------------------------------
$Url = "https://github.com/$($Lock.repo)/releases/download/$($Lock.tag)/$($Lock.asset)"
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("libmpv-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null
$Archive = Join-Path $Tmp $Lock.asset
$Extract = Join-Path $Tmp "extract"
New-Item -ItemType Directory -Path $Extract -Force | Out-Null

try {
    Write-Host "下载：$Url"
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest -Uri $Url -OutFile $Archive -UseBasicParsing -TimeoutSec 600

    # ---- 第 1 层：压缩包哈希 -----------------------------------------------
    $ArchiveHash = Get-Sha256 $Archive
    if ($ArchiveHash -ne $Lock.archiveSha256) {
        throw @"
压缩包哈希不匹配，已中止，没有解压也没有写入任何构建目录：
  实际：$ArchiveHash
  期望：$($Lock.archiveSha256)
"@
    }
    Write-Host "压缩包哈希校验通过"

    # ---- 解压 ---------------------------------------------------------------
    # 用 tar（Windows 自带 bsdtar）。解压目标固定在新建的临时目录里。
    & tar -xf $Archive -C $Extract
    if ($LASTEXITCODE -ne 0) {
        throw "解压失败，tar 退出码 $LASTEXITCODE"
    }

    # ---- 定位 DLL，并确认它没有逃出临时目录 -------------------------------
    # 归档里的条目名如果带 `..` 或绝对路径，理论上可以写到临时目录之外；
    # 所以不直接信任 Find 的结果，而是校验解析后的完整路径仍在 $Extract 内。
    #
    # 另外要求「有且只有一个」同名文件：将来换的归档若带了多个
    # libmpv-2.dll（例如同时含 mpv 与 mpv-lgpl 两套构建），「取第一个」的
    # 结果取决于文件系统枚举顺序，锁定值就变成不确定的了。
    $Matches = @(Get-ChildItem -LiteralPath $Extract -Recurse -File -Filter "libmpv-2.dll")
    if ($Matches.Count -ne 1) {
        throw "压缩包里有 $($Matches.Count) 个 libmpv-2.dll，预期恰好 1 个。归档布局可能已变，需要人工确认该取哪一个并更新锁定文件。"
    }

    $Found = $Matches[0]

    $ExtractRoot = [System.IO.Path]::GetFullPath($Extract).TrimEnd('\') + '\'
    $Resolved = [System.IO.Path]::GetFullPath($Found.FullName)
    if (-not $Resolved.StartsWith($ExtractRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw @"
压缩包里的条目试图写到临时目录之外，已中止：
  条目：$($Found.FullName)
"@
    }

    # ---- 第 2 层：解压出来的 DLL 哈希 ---------------------------------------
    $DllHash = Get-Sha256 $Resolved
    if ($DllHash -ne $Lock.dllSha256) {
        throw @"
解压出的 libmpv-2.dll 哈希不匹配，已中止：
  实际：$DllHash
  期望：$($Lock.dllSha256)
"@
    }
    Write-Host "DLL 哈希校验通过"

    Copy-Item -LiteralPath $Resolved -Destination $Dest -Force
    Write-Host "已写入：$Dest"
}
finally {
    Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}
