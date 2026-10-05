# 界面端到端验收
#
# 控制栏是 GDI 自绘的，`cargo test` 覆盖不到「像素真的贴对了地方」这件事——
# 单元测试只能钉住坐标约定，位图最终有没有落到屏幕上得看真实输出。
# 这里用截图对比来做：抓窗口像素，判断画面在动、控制栏在更新、影院模式与
# 全屏的几何是否正确。
#
# 这不是 CI 里跑的东西：它要抢前台窗口、发键盘鼠标事件，会干扰正在用电脑的人。
# 发布前手动跑一次。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts/verify-ui.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/verify-ui.ps1 -Exe <路径>
#
# 依赖 System.Drawing（Windows 自带的 .NET 组件）。

param(
    # 被测 exe，默认用 release 构建产物
    [string]$Exe,
    # 测试素材，默认用仓库里的 60 秒测试图（进度条检查需要足够长的素材）
    [string]$Media
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
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }

$Root = Split-Path -Parent $PSScriptRoot
if (-not $Exe) {
    $Exe = Join-Path $Root "src-tauri\target\release\video-view.exe"
}
if (-not (Test-Path -LiteralPath $Exe)) {
    throw "找不到 $Exe，先跑 cargo build --release"
}
if (-not $Media) {
    $Media = Join-Path $Root "src-tauri\tests\media\loop60s.mp4"
}
if (-not (Test-Path -LiteralPath $Media)) {
    throw "找不到测试素材 $Media，跑 scripts/make-test-media.ps1 生成"
}

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class VV {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern IntPtr GetThreadDpiAwarenessContext();
  [DllImport("user32.dll")] public static extern int GetAwarenessFromDpiAwarenessContext(IntPtr ctx);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int ht, uint f);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr p, EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern void keybd_event(byte k, byte s, uint f, IntPtr e);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, IntPtr e);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  public struct RECT { public int Left, Top, Right, Bottom; }
  public struct POINT { public int X, Y; }
}
"@

# 自己也要 per-monitor 感知，否则拿到的坐标全缩放过，读到的像素会偏。
#
# 必须用 V2 而不是老的老 API SetProcessDPIAware()：后者只是「系统 DPI 感知」，
# 在缩放比例不一致的多显示器机器上，只有主显示器那几块屏幕拿到的是设备像素。
# 下面所有偏移（客户区原点、控件位置）都建立在这条前提上，前提错了整套判定
# 全错，而且不会报错、只会莫名其妙地 FAIL。
#
# 静默失败是这个脚本最难查的一类问题，所以返回值一定要看。但**不能**把
# 「设不上」直接当成错误：
#
#   * `SetProcessDpiAwarenessContext` 在「已经有窗口」或「已经被设成别的档位」
#     时返回 FALSE。宿主 pwsh.exe 自带 PerMonitorV2 清单、走 `-File` 起本脚本时
#     就常是这个状态——那恰恰是我们想要的结果，不是故障。
#   * `SetProcessDPIAware()` 更糟：它在**已经是 DPI 感知**时也返回 FALSE。
#     所以「两个调用都返回 FALSE」既可能是「设置失败」，也可能是
#     「早就设好了」，两者返回值一模一样。
#
# 正确做法是先问现在是什么档位，只有「既不是 PerMonitor 也不是 PerMonitorV2」
# 才当故障处理。
# DPI_AWARENESS: 0=INVALID 1=UNAWARE 2=SYSTEM 3=PER_MONITOR
$DPI_AWARENESS_PER_MONITOR = 3
# -4 = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
$PER_MONITOR_AWARE_V2 = [IntPtr](-4)

function Get-DpiAwareness {
    [VV]::GetAwarenessFromDpiAwarenessContext([VV]::GetThreadDpiAwarenessContext())
}

if (-not [VV]::SetProcessDpiAwarenessContext($PER_MONITOR_AWARE_V2)) {
    $now = Get-DpiAwareness
    if ($now -lt $DPI_AWARENESS_PER_MONITOR) {
        # 确实没到 per-monitor，退一步试系统 DPI 感知（单屏或全同缩放时够用）
        if (-not [VV]::SetProcessDPIAware()) {
            $now = Get-DpiAwareness
            if ($now -lt $DPI_AWARENESS_PER_MONITOR) {
                throw ("设置 DPI 感知失败（当前 awareness={0}）：本脚本拿到的坐标会与实际屏幕像素不符，所有判定都不可信。" -f $now)
            }
        }
    }
    $now = Get-DpiAwareness
    if ($now -lt $DPI_AWARENESS_PER_MONITOR) {
        Write-Warning ("只退到了 DPI awareness={0}（3=PerMonitor, 2=System）。多屏不同缩放时读到的像素会偏移。" -f $now)
    }
}

function Get-Cls($h) {
    $sb = New-Object System.Text.StringBuilder 256
    [VV]::GetClassName($h, $sb, 256) | Out-Null
    $sb.ToString()
}

function Find-Child($parent, $cls) {
    $script:found = [IntPtr]::Zero
    $cb = [VV+EnumProc] {
        param($h, $l)
        if ((Get-Cls $h) -eq $cls) { $script:found = $h; return $false }
        return $true
    }
    [VV]::EnumChildWindows($parent, $cb, [IntPtr]::Zero) | Out-Null
    return $script:found
}

function Force-Foreground($h) {
    $me = [VV]::GetCurrentThreadId()
    # 不能叫 $pid：PowerShell 里那是只读的内置变量，赋值会直接报错
    $fgPid = 0
    $fg = [VV]::GetForegroundWindow()
    $ft = [VV]::GetWindowThreadProcessId($fg, [ref]$fgPid)
    [VV]::AttachThreadInput($me, $ft, $true) | Out-Null
    [VV]::ShowWindow($h, 9) | Out-Null
    [VV]::BringWindowToTop($h) | Out-Null
    [VV]::SetForegroundWindow($h) | Out-Null
    [VV]::AttachThreadInput($me, $ft, $false) | Out-Null
}

function Get-Rect($h) { $r = New-Object VV+RECT; [VV]::GetWindowRect($h, [ref]$r) | Out-Null; return $r }

function Grab($h) {
    $r = Get-Rect $h
    $b = New-Object System.Drawing.Bitmap ($r.Right - $r.Left), ($r.Bottom - $r.Top)
    $g = [System.Drawing.Graphics]::FromImage($b)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size($b.Width, $b.Height)))
    $g.Dispose()
    return $b
}

function Send-Key($vk) {
    [VV]::keybd_event($vk, 0, 0, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [VV]::keybd_event($vk, 0, 2, [IntPtr]::Zero)
}

function Click($x, $y) {
    [VV]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 250
    [VV]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [VV]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)
}

function PixDiff($a, $b, $x0, $y0, $w, $h, $step) {
    # 退化输入直接报出来，而不是让它一路算到除零。
    # 这些值全部来自窗口几何：窗口被压到最小尺寸、或者最小化到客户区 0x0 时
    # w/h 就会 <= 0；step 来自调用点，是常量但仍不该让它变成死循环。
    if ($w -le 0 -or $h -le 0) { throw "PixDiff 区域无像素：w=$w h=$h（窗口几何不对？）" }
    if ($step -le 0) { throw "PixDiff 步长非法：$step" }
    $d = 0; $n = 0
    for ($y = 0; $y -lt $h; $y += $step) {
        for ($x = 0; $x -lt $w; $x += $step) {
            $n++
            $p1 = $a.GetPixel($x0 + $x, $y0 + $y)
            $p2 = $b.GetPixel($x0 + $x, $y0 + $y)
            if ([math]::Abs($p1.R - $p2.R) + [math]::Abs($p1.G - $p2.G) + [math]::Abs($p1.B - $p2.B) -gt 30) { $d++ }
        }
    }
    # $n >= 1 已由上面的 h>0 保证；这一步只是不把除零留给运行时
    if ($n -eq 0) { return 0.0 }
    return [math]::Round(100.0 * $d / $n, 1)
}

# ---- 起进程 ------------------------------------------------------------------
$script:Results = [ordered]@{}

function Add-Result($name, $ok, $note) {
    $script:Results[$name] = [bool]$ok
    $tag = if ($ok) { "PASS" } else { "FAIL" }
    Write-Host ("  {0,-16} {1,-5} {2}" -f $name, $tag, $note)
}

# 播放/暂停判定靠「两帧之间像素有没有变」，天然对时序敏感：点击落在 mpv
# 还没真正开播的窗口上、或截图恰好抓到重绘中间态，都会误判成没暂停。
#
# 重试**只重采样判断，不重做动作**。这一点很要紧：这些动作大半是「切换」
# （点一下暂停 / 按一下空格），原来的实现整段重跑，于是第一次判定失败
# （比如点击落在了窗口还没准备好的时候）之后，第二次重跑又点了一下——
# 状态被翻回「播放」，而这一次要判的恰恰是「应该是暂停」。
# 于是「暂停」这项永远第一次假通过、「恢复」这项永远假失败，
# 或者反过来：连点两次恰好让结果对上，测出来的是「点两次等于暂停」这种
# 恰好成立的事实。
#
# 所以拆成两段：$act 只执行一次，$judge 可以重跑。判定失败只说明「还没看到
# 想要的状态」，重采样就是正确的补救；动作没生效的话重采样多少次都没用，
# 那种情况应该老实报 FAIL，而不是靠反复点击把它撞成 PASS。
# 判定本身也可能抛（`PixDiff` 会在区域非法时 throw）。异常必须就地吃掉并
# 记成 FAIL：让它冒出去会跳过后面所有项，脚本最后只剩一句 PowerShell
# 堆栈，13 项判定一个都看不到——那还不如不测。
# 这里只隔离「判定」，动作阶段不隔离：动作挂了说明环境本身有问题，
# 让它冒到顶层的 try/catch 去，那里有专门的说明。
function Test-Transport([string]$name, [string]$note, [scriptblock]$act, [scriptblock]$judge, [int]$Tries = 3) {
    & $act
    Start-Sleep -Milliseconds 1500
    for ($i = 1; $i -le $Tries; $i++) {
        $ok = $false
        try {
            $ok = [bool](& $judge)
        } catch {
            Add-Result $name $false ("$note（判定报错：{0}）" -f $_.Exception.Message)
            return
        }
        if ($ok) {
            Add-Result $name $true $(if ($i -gt 1) { "$note（第 $i 次采样通过）" } else { $note })
            return
        }
        if ($i -lt $Tries) { Start-Sleep -Milliseconds 1500 }
    }
    Add-Result $name $false "$note（采样 $Tries 次都没看到预期状态）"
}

Get-Process video-view -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400

$proc = Start-Process -FilePath $Exe -ArgumentList "`"$Media`"" -PassThru

# 进程一定要被收掉。
#
# 之前只在「拿不到主窗口」这一处显式 Stop-Process，后面任何一处 throw
# （找不到画面子窗口、PixDiff 区域非法、几何前置条件不满足）都会把播放器
# 留在后台继续播音频。更麻烦的是下一次跑这个脚本时，开头的
# `Stop-Process video-view` 是在**新**进程起来之前执行的，所以不是残留，
# 是上一次漏掉的那些——两次跑叠在一起时 Playback 控制和截图全都会错位。
function Remove-Proc($p) {
    # 整个函数体都包起来。这个函数是在 `finally` 里调的，
    # 它自己再抛就会**顶替掉真正的失败原因**——用户看到的是
    # 「清理时 ArgumentException」，而真正出错的那一项不见了。
    # `$p.HasExited` 会隐式 Refresh()，进程对象关联已失效时就会抛
    # ArgumentException，所以它不能裸在外面。
    try {
        if ($null -ne $p -and -not $p.HasExited) {
            Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
            # 等它真的退出，否则下面立刻重开会被上一次的实例抢前台
            try { $p.WaitForExit(5000) | Out-Null } catch { }
        }
    } catch {
        Write-Host ("  [warn] 收尾时没能确认进程已退出：{0}" -f $_.Exception.Message)
    }
}

# 汇总里要用，但它在 try 外面。必须先初始化：`$null.Count` 在 PowerShell 里
# 等于 0，也就是「一个失败都没有」——一旦漏初始化，脚本会把「根本没跑完」
# 报成「13 项全过」。
$failed = @()
# 中途抛出时的原因，先存着，汇总打完之后再报出来
$Fatal = $null

try {
    Start-Sleep -Seconds 6
    $main = (Get-Process -Id $proc.Id).MainWindowHandle
    if ($main -eq [IntPtr]::Zero) {
        throw "播放器进程起来了但没有主窗口"
    }

    # 固定成 1100x720 逻辑尺寸，让下面的坐标好算
    [VV]::SetWindowPos($main, [IntPtr](-1), 60, 40, 1100, 720, 0x0040) | Out-Null
    Start-Sleep -Milliseconds 500
    Force-Foreground $main
    Start-Sleep -Seconds 1

    $video = Find-Child $main "VideoViewVideoChild"
    if ($video -eq [IntPtr]::Zero) {
        throw "找不到 VideoViewVideoChild 画面子窗口"
    }

    # ---- 几何：客户区原点偏移与控制栏位置 -------------------------------------
    # 关键：截图抓的是**窗口矩形（含边框）**，而控制栏是按**客户区**布局的。
    # 少加这个偏移就会读到窗口边框附近的空白，看着像「界面没画」。
    $wr = Get-Rect $main
    $org = New-Object VV+POINT
    [VV]::ClientToScreen($main, [ref]$org) | Out-Null
    $ox = $org.X - $wr.Left
    $oy = $org.Y - $wr.Top
    $cr = New-Object VV+RECT
    [VV]::GetClientRect($main, [ref]$cr) | Out-Null
    $cw = $cr.Right - $cr.Left
    $ch = $cr.Bottom - $cr.Top
    $dpi = [double][VV]::GetDpiForWindow($main)

    function DipPx([double]$dip) { [int][math]::Round($dip * $dpi / 96.0) }

    # 与 src-tauri/src/ui.rs 的 Layout 常量保持一致
    $padX = DipPx 10
    $padTop = DipPx 6
    $trackH = DipPx 16
    $timeW = DipPx 52
    $gap = DipPx 8
    $ctrlH = DipPx 62
    $ctrlTop = $ch - $ctrlH
    $trackY = $ctrlTop + $padTop
    $trackCY = $trackY + [int]($trackH / 2)
    $seekX = $padX + $timeW + $gap
    $seekR = $cw - $padX - $timeW - $gap

    Write-Host "exe       : $Exe"
    Write-Host "media     : $Media"
    Write-Host ("geometry  : window {0}x{1} @({2},{3}); client origin offset ({4},{5}); client {6}x{7}; dpi {8}" -f `
            ($wr.Right - $wr.Left), ($wr.Bottom - $wr.Top), $wr.Left, $wr.Top, $ox, $oy, $cw, $ch, $dpi)
    Write-Host ("controls  : top={0}; seek track y={1} x={2}..{3}" -f $ctrlTop, $trackCY, $seekX, $seekR)
    Write-Host ""

    # 进度条判定全靠 [seekX, seekR) 这一段。窗口太窄时它会退化成空区间，
    # 后面 Get-ProgressRight 找不到橙色像素、百分比那行还会除以零，
    # 报出来的是「除数为零」而不是「窗口太窄」，方向完全指错。
    if ($seekR -le $seekX) {
        throw ("客户区太窄，进度条区间退化为空：seekX={0} seekR={1} client={2}x{3} dpi={4}" -f `
                $seekX, $seekR, $cw, $ch, $dpi)
    }

    function VideoRect {
        $r = Get-Rect $video
        return @{
            vx = $r.Left - $wr.Left
            vy = $r.Top - $wr.Top
            vw = $r.Right - $r.Left
            vh = $r.Bottom - $r.Top
            cx = [int](($r.Left + $r.Right) / 2)
            cy = [int](($r.Top + $r.Bottom) / 2)
        }
    }

    # 画面在动 或 控制栏在变，两者任一成立即视为「还在播放」
    function Test-Motion($tag) {
        $g = VideoRect
        $a = Grab $main; Start-Sleep -Seconds 2; $b = Grab $main
        $frame = PixDiff $a $b $g.vx $g.vy $g.vw $g.vh 5
        $ctlY = $g.vy + $g.vh + 4
        $ctlH = ($wr.Bottom - $wr.Top) - $ctlY - 6
        $ui = PixDiff $a $b ($padX + $ox) $ctlY ($timeW + $ox) $ctlH 2
        $a.Dispose(); $b.Dispose()
        $playing = ($frame -ge 1) -or ($ui -ge 0.5)
        Write-Host ("  {0,-34} frame={1,5}%  uiTime={2,5}%  => {3}" -f `
                $tag, $frame, $ui, $(if ($playing) { "PLAYING" } else { "PAUSED" }))
        return $playing
    }

    # 「还在动」本身也可能因为视频短、播完了而变成假阴性，所以也走重试
    Test-Transport "baseline" "启动后应自动播放" `
        { } `
        { Test-Motion "startup -> expect PLAYING" }

    # [progress] 排在 [transport] **前面**，是时间预算上的硬约束。
    #
    # 默认素材 60 秒。transport 有 7 项、每项约 4 秒（动作 + 1.5s 等它生效 +
    # 一次 2 秒双帧采样），顺利也要 30 秒上下；哪一项判定失败还会多烧两轮
    # 重采样。排在后面的话，进度条采样很可能落在 60 秒之后——那时 mpv 已经
    # EndFile、界面回到空闲态、画面子窗口被隐藏（`set_video_visible(false)`），
    # 橙色 accent 根本找不到，于是 `timecodeMoves` 和 `progressAdvances`
    # **两项一起假 FAIL**，而且报出来的原因和真正的问题完全无关。
    #
    # 放到最前面就没有这个问题：t≈8 秒开始采，四次采样落在 t=8..14 秒，
    # 既保证有播放进度可测（不是刚从 0 出发），又留足 46 秒余量。
    Write-Host ""
    Write-Host "[progress]"

    # 时间码那一列的逐列最亮值拼起来就是个「指纹」。
    # 指纹必须随播放变化——曾经有一次脏区重绘把控制栏贴到了窗口顶部被视频子窗口
    # 盖住，界面永远停在第一次全量绘制的画面上（时间码 00:00、thumb 钉在最左），
    # 而内部状态其实一直在推进。这两行就是那个 bug 的端到端防线。
    function Get-TimeSignature($bmp) {
        $sig = @()
        for ($x = $padX + $ox; $x -lt ($padX + $timeW + $ox); $x++) {
            $mx = 0
            for ($y = $trackY + $oy; $y -lt ($trackY + $trackH + $oy); $y++) {
                $c = $bmp.GetPixel($x, $y)
                $g = [int](0.299 * $c.R + 0.587 * $c.G + 0.114 * $c.B)
                if ($g -gt $mx) { $mx = $g }
            }
            $sig += $mx
        }
        return ($sig -join ',')
    }

    # 进度条上已播放段落的右边界（橙色 accent）
    function Get-ProgressRight($bmp) {
        $right = -1
        for ($x = $seekX + $ox; $x -lt ($seekR + $ox); $x++) {
            $c = $bmp.GetPixel($x, $trackCY + $oy)
            if ($c.R -ge 180 -and $c.G -ge 90 -and $c.G -le 220 -and $c.B -le 80) { $right = $x - $ox }
        }
        return $right
    }

    $sigs = @()
    $rights = @()
    # 显式初始化：第 1 次循环里 `$delta` 走的是 "-" 分支，$prev 用不到；
    # 但它一旦漏初始化，读者就得自己推一遍才知道这里为什么安全。
    $prev = 0
    for ($i = 1; $i -le 4; $i++) {
        $bmp = Grab $main
        $sigs += Get-TimeSignature $bmp
        $rights += Get-ProgressRight $bmp
        $bmp.Dispose()
        $pct = [math]::Round(100.0 * ($rights[-1] - $seekX) / ($seekR - $seekX), 1)
        # 同时打出与上次采样的增量：某次采样若出现反常跳变（比如 seek 落点不精确
        # 导致位置突变），光看百分比不容易分清是脚本问题还是播放行为问题。
        # 不猜「约多少秒」——素材时长是 -Media 决定的，这里假设不得。
        $delta = if ($i -eq 1) { "-" } else { "{0:+#;-#;0}" -f ($pct - $prev) }
        Write-Host ("  sample {0}: progressRight={1} ({2}%  Δ{3})" -f $i, $rights[-1], $pct, $delta)
        $prev = $pct
        if ($i -lt 4) { Start-Sleep -Seconds 2 }
    }

    $distinctSigs = @($sigs | Select-Object -Unique).Count
    Add-Result "timecodeMoves" ($distinctSigs -ge 3) `
        ("4 次采样里出现 {0} 种不同指纹（至少 3 种）" -f $distinctSigs)

    $monotonic = $true
    for ($i = 1; $i -lt $rights.Count; $i++) {
        if ($rights[$i] -lt $rights[$i - 1]) { $monotonic = $false }
    }
    Add-Result "progressAdvances" (($rights[0] -ge 0) -and $monotonic -and ($rights[-1] -gt $rights[0])) `
        ("已播放段落右边界 {0} -> {1}，单调递增={2}" -f $rights[0], $rights[-1], $monotonic)

    Write-Host ""
    Write-Host "[transport]"
    $g0 = VideoRect

    # 每个判定都是「点/按键 -> 等一下 -> 看画面还在不在动」。
    # Pause 类期望画面**停住**，所以取反。
    # 动作只做一次，重试只重采样（见 Test-Transport 的说明）。
    Test-Transport "clickPause" "单击画面暂停" `
        { Click $g0.cx $g0.cy } `
        { -not (Test-Motion "click picture -> expect PAUSED") }

    Test-Transport "clickResume" "再单击恢复" `
        { Click $g0.cx $g0.cy } `
        { Test-Motion "click picture -> expect PLAYING" }

    Test-Transport "spacePause" "空格暂停" `
        { Send-Key 0x20 } `
        { -not (Test-Motion "SPACE -> expect PAUSED") }

    Test-Transport "spaceResume" "空格恢复" `
        { Send-Key 0x20 } `
        { Test-Motion "SPACE -> expect PLAYING" }

    # 这两项是 seek 而非「切换」：连按两次右方向键只是多 seek 10 秒，
    # 不会把状态翻回去。所以对它们整段重试也安全，真遇到「按键丢了」
    # （窗口刚抢到焦点）还能顺手吃掉。Test-Transport 的统一语义
    # （动作只做一次）在这里只是多花一次重采样，没有正确性代价。
    Test-Transport "seekFwd" "右方向键快进" `
        { Send-Key 0x26 } `
        { Test-Motion "ArrowRight -> expect PLAYING" }

    Test-Transport "seekBack" "左方向键快退" `
        { Send-Key 0x25 } `
        { Test-Motion "ArrowLeft -> expect PLAYING" }

    Write-Host ""
    Write-Host "[theatre]"
    $g1 = VideoRect
    Click $g0.cx $g0.cy; Start-Sleep -Milliseconds 80
    Click $g0.cx $g0.cy
    Start-Sleep -Seconds 2
    $g2 = VideoRect
    Add-Result "theatreOn" ($g2.vh -gt $g1.vh) `
        ("画面 {0}x{1} -> {2}x{3}（控制栏收起）" -f $g1.vw, $g1.vh, $g2.vw, $g2.vh)

    Send-Key 0x1B; Start-Sleep -Seconds 2
    $g3 = VideoRect
    Add-Result "theatreOff" ($g3.vh -lt $g2.vh) `
        ("ESC 后回到 {0}x{1}" -f $g3.vw, $g3.vh)

    Write-Host ""
    Write-Host "[fullscreen]"
    Send-Key 0x7A; Start-Sleep -Seconds 2
    $wr = Get-Rect $main
    Add-Result "fsOn" ((($wr.Right - $wr.Left) -eq 1920) -and (($wr.Bottom - $wr.Top) -eq 1080)) `
        ("F11 -> {0}x{1}" -f ($wr.Right - $wr.Left), ($wr.Bottom - $wr.Top))
    Send-Key 0x7A; Start-Sleep -Seconds 2
    $wr = Get-Rect $main
    Add-Result "fsOff" (($wr.Right - $wr.Left) -eq 1100) `
        ("F11 -> {0}x{1}" -f ($wr.Right - $wr.Left), ($wr.Bottom - $wr.Top))

} catch {
    # 记下来，不直接抛。
    #
    # 直接抛的话 finally 会照跑（收掉进程），然后脚本带着堆栈退出，
    # 下面那段汇总一行都不执行——已经跑完的 5、6 项结果全白费，
    # 用户只看到一句 PowerShell 错误信息，得自己猜是哪一步挂的。
    # 存下来、汇总打完再报，用户至少知道「跑到哪儿为止」。
    $Fatal = $_
} finally {
    # 无论正常跑完还是在任何一处退出，播放器都必须被收掉。
    Remove-Proc $proc
}

# ---- 汇总 ------------------------------------------------------------------
# 放在 try/catch 外面：中途抛出也要能看到已完成的那几项。
Write-Host ""
Write-Host "[summary]"
$failed = @($script:Results.GetEnumerator() | Where-Object { -not $_.Value })
foreach ($kv in $script:Results.GetEnumerator()) {
    Write-Host ("  {0,-16} {1}" -f $kv.Key, $(if ($kv.Value) { "PASS" } else { "FAIL" }))
}

if ($null -ne $Fatal) {
    Write-Host ""
    Write-Host "中途出错，已完成的 $((($failed.Count + $script:Results.Count) - $failed.Count)) 项如上。原因：" -ForegroundColor Red
    Write-Host ("  {0}: {1}" -f $Fatal.Exception.GetType().Name, $Fatal.Exception.Message) -ForegroundColor Red
    Write-Host ("  {0}" -f ($Fatal.ScriptStackTrace -split "`n" | Select-Object -First 3)) -ForegroundColor DarkGray
    exit 2
}

Write-Host ""
if ($failed.Count -eq 0) {
    Write-Host "全部通过（$($script:Results.Count) 项）"
    exit 0
}
Write-Host "失败 $($failed.Count) 项：$($failed.Key -join ', ')"
exit 1
