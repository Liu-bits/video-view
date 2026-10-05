# 生成测试用媒体
#
# 仓库里已经带了这些文件，日常跑测试不需要本脚本。
# 需要新增编码覆盖、或要重新生成时使用。
#
# 用法:
#   powershell -ExecutionPolicy Bypass -File scripts/make-test-media.ps1
#
# 依赖 ffmpeg（PATH 里能直接调用 ffmpeg 即可）。

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

$Root = Split-Path -Parent $PSScriptRoot
$Dest = Join-Path $Root "src-tauri\tests\media"

if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) {
    throw "找不到 ffmpeg，请先安装并加入 PATH"
}

New-Item -ItemType Directory -Force -Path $Dest | Out-Null

# 生成失败的素材名，最后决定退出码
$Failed = @()

# 默认时长，多数素材用 5 秒；个别素材在循环里单独指定
# 名称 -> ffmpeg 编码参数。目的是覆盖尽可能多的容器与编码组合。
$Jobs = @(
    # 容器：MP4 / MKV / MOV / WebM / AVI / FLV / WMV / OGG
    @("h264.mp4",   @("-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac"))
    @("hevc.mp4",   @("-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-tag:v", "hvc1"))
    @("h264.mkv",   @("-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "libopus"))
    @("h264.mov",   @("-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac"))
    @("vp9.webm",   @("-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "200k", "-c:a", "libopus"))
    @("av1.webm",   @("-c:v", "libaom-av1", "-cpu-used", "8", "-b:v", "200k", "-c:a", "libopus", "-strict", "experimental"))
    @("mpeg4.avi",  @("-c:v", "mpeg4", "-q:v", "5", "-c:a", "libmp3lame"))
    @("msmpeg4.avi", @("-c:v", "msmpeg4v2", "-c:a", "mp3"))
    # DV 是定长编码，单独用更短更小的参数
    @("dv.avi",     @("-c:v", "dvvideo", "-s", "720x480", "-pix_fmt", "yuv411p", "-c:a", "pcm_s16le"))
    @("flv.flv",    @("-c:v", "flv", "-c:a", "mp3"))
    @("wmv.wmv",    @("-c:v", "wmv2", "-b:v", "500k", "-c:a", "wmav2"))
    @("theora.ogg", @("-c:v", "libtheora", "-c:a", "libvorbis")),
    # 60 秒测试图。scripts/verify-ui.ps1 的「进度条与时间码推进」检查需要一段
    # 足够长的素材：仓库里其他素材只有 2~5 秒，播到一半就结束回到空闲态，
    # 采样窗口太窄、结果不稳。
    @("loop60s.mp4", @("-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac"))
)

Push-Location $Dest
try {
    foreach ($job in $Jobs) {
        $name = $job[0]
        $enc = $job[1]

        # 各素材的时长不一样：默认 5 秒，DV 编码 2 秒（体积小才够快），
        # loop60s 是 60 秒（供界面回归检查用）
        $secs = switch ($name) {
            "dv.avi"    { 2 }
            "loop60s.mp4" { 60 }
            default     { 5 }
        }
        $vSrc = "testsrc=size=320x240:rate=15:duration=$secs"
        $aSrc = "sine=frequency=440:duration=$secs"
        if ($name -eq "dv.avi") {
            # DV 只支持特定尺寸/采样率，换成 720x480
            $vSrc = "testsrc=size=720x480:rate=10:duration=$secs"
        }

        # ffmpeg 失败时可能已经建了目标文件（比如容器头写完了才报错），
        # 所以「文件存在」不能当成功判据——必须看退出码。
        # 之前的写法在这里判 Test-Path，于是编码器不认识、或者滤镜写错时
        # 会打印一行绿色 OK，脚本还整体 exit 0，坏素材就这么进了仓库，
        # 而且下一个跑集成测试的人只会看到一句莫名其妙的「文件打不开」。
        # 先删掉上一次的残留，避免旧文件把这次失败盖住。
        if (Test-Path -LiteralPath $name) {
            Remove-Item -LiteralPath $name -Force
        }

        $out = & ffmpeg -hide_banner -loglevel error -y `
            -f lavfi -i $vSrc -f lavfi -i $aSrc @enc $name 2>&1
        $code = $LASTEXITCODE

        # 退出码为准，再补一条「文件在不在」的独立检查：退出码 0 却没产出文件
        # 同样是不对劲的（路径权限、磁盘满），只是报错方式不同
        $exists = Test-Path -LiteralPath $name
        if ($code -eq 0 -and $exists) {
            $kb = [math]::Round((Get-Item $name).Length / 1KB, 1)
            Write-Host ("OK   {0,-15} {1,9} KB" -f $name, $kb) -ForegroundColor Green
        }
        else {
            $why = if ($code -ne 0) { "ffmpeg 退出码 $code" } else { "ffmpeg 退出码 0 但没有产出文件" }
            Write-Host ("FAIL {0,-15} {1}: {2}" -f $name, $why, ($out -join " ")) -ForegroundColor Red
            $Failed += $name
        }
    }
}
finally {
    Pop-Location
}

if ($Failed.Count -gt 0) {
    # 退出码非 0：调用方（CI 或人）才能发现这一轮素材是坏的。
    # 素材是仓库的一部分，坏素材会一路带进发布前的手工验收。
    Write-Host ""
    Write-Host ("失败 $($Failed.Count) 个：$($Failed -join ', ')") -ForegroundColor Red
    exit 1
}

Write-Host "`n完成，目录：$Dest"
