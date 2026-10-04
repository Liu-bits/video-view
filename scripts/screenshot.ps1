# 截取程序窗口，用于 Release 说明
#
# 用法:
#   powershell -ExecutionPolicy Bypass -File scripts/screenshot.ps1 -Exe <path> [-Video <path>] [-Out <path>]

param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Video = "",
    [string]$Out = "screenshot.png",
    [int]$WarmupSeconds = 9
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Shot {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

$HWND_TOPMOST = [IntPtr](-1)
$HWND_NOTOPMOST = [IntPtr](-2)
$SWP_NOMOVE = 0x0002
$SWP_NOSIZE = 0x0001

$args = @()
if ($Video) { $args = @("`"$Video`"") }

$proc = Start-Process -FilePath $Exe -ArgumentList $args -PassThru
try {
    Write-Host "已启动 PID=$($proc.Id)，等待 ${WarmupSeconds}s..."
    Start-Sleep -Seconds $WarmupSeconds
    $proc.Refresh()
    if ($proc.HasExited) { throw "进程已退出，退出码 $($proc.ExitCode)" }

    $h = $proc.MainWindowHandle
    if ($h -eq [IntPtr]::Zero) { throw "没有找到主窗口" }

    # 截图期间置顶，否则会被发起截图的终端窗口挡住。
    # 视频是 D3D 硬件呈现的，PrintWindow 只能拿到黑屏，只能抓屏幕。
    [Shot]::SetWindowPos($h, $HWND_TOPMOST, 0, 0, 0, 0, $SWP_NOMOVE -bor $SWP_NOSIZE) | Out-Null
    [Shot]::SetForegroundWindow($h) | Out-Null
    Start-Sleep -Milliseconds 1200

    $r = New-Object Shot+RECT
    [Shot]::GetWindowRect($h, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left
    $ht = $r.Bottom - $r.Top

    $bmp = New-Object System.Drawing.Bitmap($w, $ht)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size($w, $ht)))
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()
    $bmp.Dispose()

    [Shot]::SetWindowPos($h, $HWND_NOTOPMOST, 0, 0, 0, 0, $SWP_NOMOVE -bor $SWP_NOSIZE) | Out-Null

    Write-Host "已保存 $Out ($w x $ht)"
}
finally {
    if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}
