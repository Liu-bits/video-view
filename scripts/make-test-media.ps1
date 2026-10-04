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

$Root = Split-Path -Parent $PSScriptRoot
$Dest = Join-Path $Root "src-tauri\tests\media"

if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) {
    throw "找不到 ffmpeg，请先安装并加入 PATH"
}

New-Item -ItemType Directory -Force -Path $Dest | Out-Null

# 5 秒测试图 + 正弦音，320x240
$Video = "testsrc=size=320x240:rate=15:duration=5"
$Audio = "sine=frequency=440:duration=5"

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
    @("theora.ogg", @("-c:v", "libtheora", "-c:a", "libvorbis"))
)

Push-Location $Dest
try {
    foreach ($job in $Jobs) {
        $name = $job[0]
        $enc = $job[1]

        $vSrc = if ($name -eq "dv.avi") { "testsrc=size=720x480:rate=10:duration=2" } else { $Video }
        $aSrc = if ($name -eq "dv.avi") { "sine=frequency=440:duration=2" } else { $Audio }

        $out = & ffmpeg -hide_banner -loglevel error -y `
            -f lavfi -i $vSrc -f lavfi -i $aSrc @enc $name 2>&1

        if (Test-Path $name) {
            $kb = [math]::Round((Get-Item $name).Length / 1KB, 1)
            Write-Host ("OK   {0,-15} {1,9} KB" -f $name, $kb) -ForegroundColor Green
        } else {
            Write-Host ("FAIL {0,-15} {1}" -f $name, ($out -join " ")) -ForegroundColor Red
        }
    }
}
finally {
    Pop-Location
}

Write-Host "`n完成，目录：$Dest"
