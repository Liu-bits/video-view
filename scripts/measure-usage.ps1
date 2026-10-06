# 采集运行时的资源占用，给 README 里那张表供数。
#
#   powershell -ExecutionPolicy Bypass -File scripts/measure-usage.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/measure-usage.ps1 -Media tests/media/h264-4k.mp4
#
# 表里那些数字以前是手工用任务管理器看眼睛抄的。这次改成脚本采，理由不是
# 「更科学」，而是 640×480 / 1080p / 4K 三行要**互相可比** —— 手抄的三个
# 数字来自三个不同的时间点，其中至少一次的窗口大小和另外两次不同，
# 而 3D 占用对窗口面积极其敏感（README 里已经写过这一点），这种偏差
# 足以让「4K 的 Video Decode 高一个数量级」这句话变成假的。
#
# GPU 那一列取自 `GPU Engine` 性能计数器，也就是任务管理器「引擎」列的
# 同一个数据源（`engtype_3D` / `engtype_VideoDecode` / `engtype_Copy`）。
# 不用 `nvidia-smi` 那类工具：这里测的是「D3D11 硬解有没有真的在跑」，
# 而那个信息只有 Windows 的分引擎计数里有。

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

$Root = Split-Path -Parent $PSScriptRoot
$Exe = Join-Path $Root "src-tauri\target\release\video-view.exe"

if (-not (Test-Path $Exe)) {
    Write-Error "找不到 $Exe。先跑 scripts/package.ps1 或 cargo build --release。"
    exit 1
}

# 默认按仓库里的媒体逐个测。顺序刻意从小到大：跑完之后 Windows 的文件
# 缓存里已经热了，后面的不会因为首次读盘而虚高。
$DefaultMedia = @(
    "h264.mp4",       # 320×240
    "loop60s.mp4",    # 640×480，时长够长
    "h264-1080p.mp4", # 1280×720
    "h264-4k.mp4",    # 3840×2160
    "hevc-4k.mp4"     # 3840×2160，HEVC —— 看两种解码器的差别
)
$MediaDir = Join-Path $Root "src-tauri\tests\media"

$param_Media = $null
if ($args.Count -gt 0) { $param_Media = $args[0] }

# 采样窗口大小。必须**固定**，否则 3D 那列没有可比性。
$WinW = 1882
$WinH = 953

# 每个文件的采样时长。取中位数而不是平均值：一次播放里 CPU 会因为
# 首次解码、seek、窗口缩放而抖，取中位数能把这些尖峰剔掉。
$SampleSec = 12
$IntervalMs = 400

# 仓库里的测试素材大多是 **3 秒**（`make-test-media.ps1` 里刻意做的，
# 免得 4K 素材让仓库多几百 MB）。3 秒在 12 秒采样窗口里早就播完了 ——
# 第一次跑这个脚本时 1080p / 4K 两行的 CPU 和 GPU 全是 0，不是「4K 不花钱」，
# 是**根本没在放**。
#
# 解决办法是在 %TEMP% 里造一份 60 秒的循环副本再测，用
# `-stream_loop -1 -c copy`：不解码、只重封包，4K 实测 0.4 秒。
# 为什么不直接把仓库里的素材做长：4K 60 秒是 148 MB（testsrc2 噪声大，
# 压不下去），放仓库里不可接受。
$LoopSec = 60
$LoopDir = Join-Path $env:TEMP "vv-measure-media"
$ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if (-not $ffmpeg) {
    $ffmpeg = Get-ChildItem "$env:LOCALAPPDATA\ffmpeg" -Recurse -Filter "ffmpeg.exe" -ErrorAction SilentlyContinue |
        Select-Object -First 1 -ExpandProperty FullName
}

function Get-LoopedCopy([string]$file) {
    if (-not $ffmpeg) {
        Write-Host "  找不到 ffmpeg：素材不足 $LoopSec 秒，采样会落在播放结束之后（数字会是 0）。"
        Write-Host "  装一个（winget install Gyan.FFmpeg）或者只传一个够长的素材进来。"
        return $file
    }
    New-Item -ItemType Directory -Path $LoopDir -Force | Out-Null
    $out = Join-Path $LoopDir (Split-Path -Leaf $file)
    & $ffmpeg -hide_banner -loglevel error -y -stream_loop -1 -i $file -t $LoopSec -c copy $out
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $out)) {
        Write-Host "  造循环副本失败，用原文件（采样可能落在播放结束之后）"
        return $file
    }
    return $out
}

Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class VVWin {
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int h2, bool repaint);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
}
"@

# 取中位数而不是平均值。
#
# 平均值会把「首次解码」「seek」「窗口从 1082 拉到 1882」这类一次性尖峰
# 揉进结果里，而尖峰的幅度和分辨率无关 —— 4K 上揉一个和 320×240 上
# 揉一个，最后两行数字的差距会被这个共同的噪声掩盖掉。
#
# 用**函数**而不是 scriptblock 变量（`& $med $ws`）：调用符 `&` 在哈希表
# 字面量的值表达式里解析不了，报的是「表达式中包含意外的标记 &」，
# 位置指向最后一行、跟真正的原因隔着几十行。
function Get-Median([double[]]$a) {
    if ($null -eq $a -or $a.Count -eq 0) { return 0.0 }
    $s = $a | Sort-Object
    return [double]$s[[int][math]::Floor($s.Count / 2)]
}

function Get-GpuEngines([int]$targetPid) {
    # `GPU Engine(*)` 有几百个实例（每个进程 × 每块卡 × 每种引擎），
    # 每次调用都全量取再筛会拖慢采样，所以先按 PID 过滤。
    $out = @{}
    try {
        $samples = (Get-Counter -Counter '\GPU Engine(*)\Utilization Percentage' -ErrorAction Stop).CounterSamples
    } catch {
        return $out
    }
    foreach ($s in $samples) {
        # 实例名形如
        #   pid_1234_luid_0x00000000_0x0000ABCD_phys_0_eng_0_engtype_3D
        if ($s.InstanceName -notmatch "^pid_$($targetPid)") { continue }
        $v = [math]::Round($s.CookedValue, 1)
        if ($v -le 0) { continue }
        if ($s.InstanceName -match "engtype_(\w+)$") {
            $k = $Matches[1]
            $out[$k] = [math]::Round(($out[$k] + $v), 1)
        }
    }
    return $out
}

function Measure-One([string]$file) {
    $name = Split-Path -Leaf $file
    Write-Host "=== $name ==="

    # 先杀掉上一轮的残留。不杀的话 Get-Counter 的 PID 过滤会把
    # 两次运行的占用加在一起，数字翻倍 —— 这个坑踩过一次。
    Get-Process -Name "video-view" -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 800

    $target = Get-LoopedCopy $file
    $proc = Start-Process -FilePath $Exe -ArgumentList "`"$target`"" -PassThru
    try {
        # 等窗口出来
        $deadline = (Get-Date).AddSeconds(15)
        while ($proc.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 200
            $proc.Refresh()
        }
        if ($proc.MainWindowHandle -eq 0) {
            Write-Host "  窗口没出来，跳过"
            return
        }
        # 固定窗口大小 —— 3D 占用随面积走，尺寸不固定就没有可比性
        [VVWin]::MoveWindow($proc.MainWindowHandle, 100, 100, $WinW, $WinH, $true) | Out-Null
        [VVWin]::SetForegroundWindow($proc.MainWindowHandle) | Out-Null
        # 等它真的开始解码：2 秒是给 mpv 建轨道 + 起 hwdec 的余量
        Start-Sleep -Seconds 2

        $ws = @()
        $priv = @()
        $cpu = @()
        $gpu3d = @()
        $gpuDec = @()
        $gpuCopy = @()
        $ticks = [int]($SampleSec * 1000 / $IntervalMs)
        for ($i = 0; $i -lt $ticks; $i++) {
            $proc.Refresh()
            if ($proc.HasExited) { Write-Host "  进程提前退出"; return }
            $ws += $proc.WorkingSet64 / 1MB
            $priv += $proc.PrivateMemorySize64 / 1MB
            # CPU 是「累计秒数」，采样点之间求差得到这一段的占用。
            # 直接用累计值是不对的 —— 进程刚起时累计值本来就小。
            $cpu += $proc.TotalProcessorTime.TotalSeconds
            $g = Get-GpuEngines $proc.Id
            $gpu3d += $(if ($g.ContainsKey("3D")) { $g["3D"] } else { 0 })
            $gpuDec += $(if ($g.ContainsKey("VideoDecode")) { $g["VideoDecode"] } else { 0 })
            $gpuCopy += $(if ($g.ContainsKey("Copy")) { $g["Copy"] } else { 0 })
            Start-Sleep -Milliseconds $IntervalMs
        }

        # CPU 用**整窗口的累计差**算，不是逐区间差再取中位数。
        #
        # `Process.TotalProcessorTime` 的分辨率是 15.625 ms（一个系统时钟滴答）。
        # 逐区间算的话，400 ms 的区间只能分辨出 15.625/400 = 3.9% 的整数倍 ——
        # 实测 320×240 / 640×480 / 1080p 三个分辨率读出来是 7.8% / 11.7% / 15.6%，
        # 正好全是 3.9% 的整数倍，而且换个顺序跑就变 —— 那是量化噪声，
        # 不是「640×480 比 1080p 更费 CPU」这种荒谬结论。
        #
        # 整窗口相除把分辨率提到 0.78%（12 秒 / 15.625 ms），噪声被平均掉了。
        # 中位数那一套还给 GPU 用 —— GPU 计数器是浮点，没有这个量化问题。
        $wall = ($cpu.Count - 1) * ($IntervalMs / 1000)
        $cpuPct = ($cpu[$cpu.Count - 1] - $cpu[0]) / $wall * 100
        # GPU 那几列取中位数：一次播放里会有 seek、窗口缩放之类的尖峰
        $g3 = $gpu3d[1..($gpu3d.Count - 1)]
        $gd = $gpuDec[1..($gpuDec.Count - 1)]
        $gc = $gpuCopy[1..($gpuCopy.Count - 1)]

        [pscustomobject]@{
            File       = $name
            Win        = "$WinW`x$($WinH - 30)"
            WorkSetMB  = [math]::Round((Get-Median $ws), 0)
            PrivateMB  = [math]::Round((Get-Median $priv), 0)
            CPUpct     = [math]::Round($cpuPct, 1)
            Gpu3Dpct   = [math]::Round((Get-Median $g3), 1)
            GpuDecPct  = [math]::Round((Get-Median $gd), 1)
            GpuCopyPct = [math]::Round((Get-Median $gc), 1)
        }
    }
    finally {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
}

if ($param_Media) {
    $files = @(if (Test-Path $param_Media) { $param_Media }
              else { Join-Path $MediaDir $param_Media })
}
else {
    $files = @($DefaultMedia | ForEach-Object { Join-Path $MediaDir $_ } |
                  Where-Object { Test-Path $_ })
    if ($files.Count -lt $DefaultMedia.Count) {
        $miss = $DefaultMedia | Where-Object { -not (Test-Path (Join-Path $MediaDir $_)) }
        Write-Host "  缺少素材：$($miss -join ', ')（跑 scripts/make-test-media.ps1）"
    }
}

$rows = @()
foreach ($f in $files) {
    $r = Measure-One $f
    if ($r) {
        $rows += $r
        Write-Host ("  工作集 {0} MB / private {1} MB / CPU {2}% / 3D {3}% / 解码 {4}%" -f `
            $r.WorkSetMB, $r.PrivateMB, $r.CPUpct, $r.Gpu3Dpct, $r.GpuDecPct)
    }
}

Get-Process -Name "video-view" -ErrorAction SilentlyContinue | Stop-Process -Force

Write-Host ""
Write-Host "| 文件 | 窗口 | 工作集 | private | CPU（单核） | GPU 3D | GPU 解码 |"
Write-Host "| --- | --- | --- | --- | --- | --- | --- |"
foreach ($r in $rows) {
    Write-Host ("| `{0}` | {1} | {2} MB | {3} MB | {4}% | {5}% | {6}% |" -f `
        $r.File, $r.Win, $r.WorkSetMB, $r.PrivateMB, $r.CPUpct, $r.Gpu3Dpct, $r.GpuDecPct)
}