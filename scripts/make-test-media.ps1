# 生成测试素材
#
# 前置：ffmpeg 在 PATH 里（`winget install Gyan.FFmpeg`，或手动解压后把
# 含 `ffmpeg.exe` 的 `bin` 目录加进 PATH）。
#
#   powershell -ExecutionPolicy Bypass -File scripts/make-test-media.ps1
#
# 需要 ffmpeg 的原因只有一个：0.6.1 要补上 1080p/4K 与多音轨的实测，
# 而仓库里原来的最大素材是 640×480 —— 那个尺寸下 Video Decode 的负载和
# 1080p 差一个数量级，拿它得出的资源占用数字对大分辨率没有参考价值。
# 多音轨同理：容器里只有一条音轨时，「在两条以上之间循环」这个分支
# 永远走不到，只能在单测里用纯函数覆盖，集成测试里是空的。
#
# 产物都进 git（`src-tauri/tests/media/**` 在 .gitattributes 里标了
# `binary`，按字节比较、不做换行转换）。大文件那几个刻意用**短时长**：
# 4K 素材只需要 3 秒 —— 测的是「解码一帧的负载与显存」，不是「播多久」。
# 30 秒的 4K 会让仓库白白多几百 MB，而测出来的数字一模一样。

$ErrorActionPreference = "Stop"

# PowerShell 7.3 起存在 `$PSNativeCommandUseErrorActionPreference`，7.4 里是**默认 $true**
# （原生命令返回非 0 就终止）。我们要的是「ffmpeg 失败 -> 报错 -> exit 1」而不是被这条
# 终止后给出毫无上下文的报错，所以显式关掉，并自己检查 $LASTEXITCODE。
# Windows PowerShell 5.1 里没有这个变量，赋值也无害。
$PSNativeCommandUseErrorActionPreference = $false

$Root = Split-Path -Parent $PSScriptRoot
$Dest = Join-Path $Root "src-tauri\tests\media"

$ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if (-not $ffmpeg) {
    $ffmpeg = Get-ChildItem "$env:LOCALAPPDATA\ffmpeg" -Recurse -Filter "ffmpeg.exe" -ErrorAction SilentlyContinue |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $ffmpeg) {
    Write-Error "找不到 ffmpeg。装一个：winget install Gyan.FFmpeg"
    exit 1
}
Write-Host "ffmpeg: $ffmpeg"

New-Item -ItemType Directory -Path $Dest -Force | Out-Null

# 造一段测试画面用的源。
#
# `testsrc2` 而不是 `testsrc`：前者画的是彩条 + 渐变 + 移动方块，**每帧像素
# 都有变化**，能暴露「静止画面时 GPU 空闲」那种假象。后者大部分区域是纯色，
# 编码器可能用 skip 宏块让某些帧几乎不耗解码时间。
#
# 时长 3 秒：够 mpv 建立轨道、解码若干帧、出 `track-list`，又不会让仓库大到
# 荒谬。逐帧/解码统计这类测量看的是「稳态每帧」，3 秒已经进入稳态。
$src = "testsrc2=size=1280x720:rate=25"

# 单个产物的体积上限（MB）。超过就报错并删掉。
#
# 这道守卫是**被一个真实事故逼出来的**：`-t` 只放在 `-i` 之后时（输入侧），
# ffmpeg 并没有把输出限制在 3 秒 —— 实测 multitrack.mp4 涨到了 **14.6 GB**
# 才被我掐掉。多个无限 lavfi 输入（`testsrc2` / `sine` 都是）的时间戳基准
# 不一致时，输入侧的 `-t` 拦不住输出。
#
# 所以现在两处都放：`-i` 之后的是输入侧（限制读多少），最后一个输出参数
# 位置的是输出侧（限制写多少）。守卫是第三道 —— 万一 ffmpeg 又改了行为，
# 至少不会把磁盘填满，也不会留下一个需要人手动删的巨型文件。
$MAX_MB = 200

# 参数**不能叫 `$args`**。
#
# `$args` 是 PowerShell 的自动变量（函数未声明参数时的兜底参数数组）。
# 拿它当形参名，函数体里的 `@args` splat 到的不是传进来的那批参数，
# 于是 ffmpeg 收到的是**空参数** —— 表现为「同一串参数手动跑 exit=0、
# 包在函数里跑 exit=-22」，报错还完全指不到真正的原因。
#
# 顺带：`Write-Error` 在 `$ErrorActionPreference = "Stop"` 下会**抛异常**，
# 所以下面那句 `exit 1` 其实到不了。留着是为了让 `$ErrorActionPreference`
# 被改成别的值时行为仍然正确。
function Run-FFmpeg([string[]]$ffArgs, [string]$what) {
    Write-Host "  $what"
    # 输出文件名固定是最后一个参数 —— 守卫要靠它找到产物
    $out = $ffArgs[-1]
    if (Test-Path $out) { Remove-Item $out -Force }

    & $ffmpeg -hide_banner -loglevel error -y @ffArgs
    if ($LASTEXITCODE -ne 0) {
        Remove-Item $out -Force -ErrorAction SilentlyContinue
        Write-Error "$what 失败（ffmpeg 退出码 $LASTEXITCODE）"
        exit 1
    }
    if (-not (Test-Path $out)) {
        Write-Error "$what 失败：ffmpeg 返回 0 但没有产出文件"
        exit 1
    }
    $mb = (Get-Item $out).Length / 1MB
    if ($mb -gt $MAX_MB) {
        Remove-Item $out -Force
        Write-Error ("{0} 产出了 {1:N1} MB，超过上限 {2} MB。" -f $what, $mb, $MAX_MB) +
        "`n这通常意味着时长限制没生效（`-t` 必须同时出现在输入侧和输出侧）。已删除该文件。"
        exit 1
    }
    Write-Host ("    -> {0:N1} MB" -f $mb)
}

# ---------------------------------------------------------------- 1080p / 4K
#
# 目的是让 README 里那张资源占用表有 1080p / 4K 两行真实数字，而不是
# 「本表用的是 640×480，不代表 1080p/4K」这句免责声明。
#
# H.264 + yuv420p，preset 用 `medium`。
#
# 两个实测踩到的点：
#   * **`-preset default` 不存在**。libx264 的 preset 是
#     ultrafast/superfast/veryfast/faster/fast/medium/slow/slower/veryslow/placebo，
#     `default` 是 NVENC 的名字。填错报的是
#     `invalid preset 'default'` + `Error setting preset/tune default/(null)`，
#     看着像编码器坏了而不是名字打错。
#   * `testsrc2` 的 `duration=` 选项在 ffmpeg 8+ 里也没了，改用 `-t`。
#     实测 ffmpeg 9.0.2 上 `testsrc2=...:duration=3` 直接
#     `Error opening input: Invalid argument`。
#
# 不用 `ultrafast`：码率与画质对 Video Decode 的负载没有影响（解码负载取决于
# 分辨率与帧率，不取决于码率），而 ultrafast 的文件会大一截。
#
# 注意每个调用里 `-t 3` 出现**两次**：`-i` 之后那次是输入侧（限制从该输入读
# 多少），编码参数之后、输出文件之前那次是输出侧（限制写多少）。只放输入侧
# 时实测产出了一个 14.6 GB 的文件，见上面 `$MAX_MB` 那段注释。
Write-Host "[1080p / 4K]"
Run-FFmpeg @("-f", "lavfi", "-i", $src, "-t", "3", "-c:v", "libx264", "-pix_fmt", "yuv420p",
    "-preset", "medium", "-t", "3", (Join-Path $Dest "h264-1080p.mp4")) "h264-1080p.mp4"

Run-FFmpeg @("-f", "lavfi", "-i", "testsrc2=size=3840x2160:rate=25", "-t", "3", "-c:v", "libx264",
    "-pix_fmt", "yuv420p", "-preset", "medium", "-t", "3", (Join-Path $Dest "h264-4k.mp4")) "h264-4k.mp4"

# HEVC 4K：H.264 之外还要看 HEVC 的硬解负载 —— 同一个 `h264` 标签掩盖了
# 两种解码器完全不同的 GPU 占用。
Run-FFmpeg @("-f", "lavfi", "-i", "testsrc2=size=3840x2160:rate=25", "-t", "3", "-c:v", "libx265",
    "-pix_fmt", "yuv420p", "-preset", "medium", "-t", "3", (Join-Path $Dest "hevc-4k.mp4")) "hevc-4k.mp4"

# ---------------------------------------------------------------- 多音轨 + 内嵌字幕
#
# 这是 0.6.1 最重要的一件：原来所有素材都只有一条音轨，于是
# `J` / `L` / `A` 的「在两条以上之间循环」分支在集成测试里**永远走不到**。
#
# 三条音轨，各不相同以便在菜单里认出来：
#   - 44100 Hz 单声道  -> 和 loop60s.mp4 的形状一致，作为对照
#   - 48000 Hz 立体声  -> 最常见的组合
#   - 22050 Hz 单声道  -> 明显不同的采样率，肉眼（听不出，但看菜单能看出）
#
# 音频用 `sine` 生成不同频率：频率不同 -> 波形不同 -> 编码出的数据不同，
# 这样即使三条轨的元数据一样，`track-list` 里的 id 也不同，仍可区分。
#
# 字幕用两种形态各来一份：
#   - 内嵌（`-c:s mov_text`）进 MP4 容器
#   - 内嵌（`-c:s srt`）进 MKV 容器（WebVTT 在 MKV 里支持不如 srt 好）
# 两种都要，因为 `track-list` 里 subtitle 轨的 `codec` 字段不一样
# （`mov_text` vs `subrip`），而 0.6.1 的 `describe()` 会把它显示出来。
Write-Host "[多音轨 + 内嵌字幕]"

$multiaudio = Join-Path $Dest "multitrack.mp4"
$srtPath = Join-Path $env:TEMP "vv-multi.srt"
# 简体中文 + 一行英文，测「标题优先于语言码」那条逻辑（`describe()` 里
# `title > language > codec`）。ASS 格式才支持样式，mov_text 不支持，
# 所以只写纯文本。
@"
1
00:00:00,500 --> 00:00:01,500
第一条字幕（中文标题）

2
00:00:01,600 --> 00:00:02,600
第二条字幕（另一个标题）
"@ | Set-Content -Path $srtPath -Encoding UTF8

Run-FFmpeg @(
    "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25", "-t", "3",
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100", "-t", "3",
    "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000", "-t", "3",
    "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=22050", "-t", "3",
    "-i", $srtPath,
    "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:a", "-map", "4:s",
    "-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast",
    "-c:a", "aac",
    # 字幕编码器**必须显式指定**。MP4 容器里 ffmpeg 不会为 `subrip` 自动挑
    # 编码器（默认只有文本渲染器），报的是
    # `Automatic encoder selection failed ... (codec none)` —— 看起来像
    # 「这个字幕格式不支持」，实际是「没说要转成什么」。
    # `mov_text` 是 MP4 的标准字幕轨，mpv 读到的 codec 名就是 `mov_text`。
    "-c:s", "mov_text",
    # 三条音轨的 disposition 全是 default 的话 mpv 会只认第一条，
    # 后两条的 `selected` 永远是 false，测「循环切到第 3 条」就没意义了
    "-disposition:a:0", "default", "-disposition:a:1", "0", "-disposition:a:2", "0",
    "-disposition:s:0", "default",
    # 每条音轨一个语言码，测 `describe()` 的语言分支
    "-metadata:s:a:0", "language=chi",
    "-metadata:s:a:1", "language=eng",
    "-metadata:s:a:2", "language=jpn",
    "-metadata:s:0", "language=chi",
    # 输出侧时长限制。少了这一句实测会写出 14.6 GB 的文件
    "-t", "3",
    $multiaudio
) "multitrack.mp4（3 音轨 + 1 内嵌字幕轨）"

# MKV 版本：字幕 codec 是 subrip 而不是 mov_text
$multiaudioMkv = Join-Path $Dest "multitrack.mkv"
Run-FFmpeg @(
    "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25", "-t", "3",
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100", "-t", "3",
    "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000", "-t", "3",
    "-i", $srtPath,
    "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s",
    "-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast",
    "-c:a", "aac", "-c:s", "srt",
    "-metadata:s:a:0", "language=chi",
    "-metadata:s:a:1", "language=eng",
    # 输出侧时长限制，见上面 multitrack.mp4 那段的注释
    "-t", "3",
    $multiaudioMkv
) "multitrack.mkv（2 音轨 + 1 内嵌字幕轨，srt）"

Remove-Item $srtPath -Force -ErrorAction SilentlyContinue

# ---------------------------------------------------- 带字幕轨的长素材
#
# 这是 `verify-ui.ps1` 的默认素材。两个条件都必需，缺一个整轮检查会垮：
#
#   * **够长**：整轮端到端要 30 秒以上（progress 段 7 项、transport 段 7 项，
#     各项之间还有等待）。`multitrack.mp4` 只有 3 秒，播完之后 mpv 发
#     `EndFile`、本地进入 idle、画面被 `set_video_visible(false)` 收掉 ——
#     实测后半轮直接失败 13 项（baseline / seekFwd / theatreOn / rightMenuOpens …），
#     看起来像程序崩了一大片，其实只是素材太短。
#   * **有字幕轨**：右键菜单里「字幕轨」那一项必须**不是置灰的**，否则
#     方向键跳不过去（Win32 的 `MF_GRAYED` 项被方向键导航跳过），
#     按右方向键展不开子菜单。
#
# 做法是**流拷贝** `loop60s.mp4` 再挂两条字幕轨，不重新编码：
# 60 秒的素材 0.1 秒造完，体积 1.8 MB → 1.8 MB（字幕轨总共十几 KB）。
# 重新编码的话会得到一个和 `loop60s.mp4` 编码参数不同的近似文件，
# 而 `verify-ui.ps1` 的若干像素比对基线是照着 `loop60s.mp4` 调的。
Write-Host "[带字幕轨的长素材]"
$subsDir = Join-Path $env:TEMP "vv-subsrc"
New-Item -ItemType Directory -Path $subsDir -Force | Out-Null
$zh = Join-Path $subsDir "zh.srt"
$en = Join-Path $subsDir "en.srt"
# 时间轴刻意铺满前 10 秒：`verify-ui` 在 transport 段会来回 seek，
# 字幕全在开头的话后半轮画面上就没有字幕了，像素比对的基线会飘。
@"
1
00:00:00,500 --> 00:00:09,000
第一条字幕（简体中文）

2
00:00:11,000 --> 00:00:19,000
第二条字幕（简体中文）

3
00:00:21,000 --> 00:00:29,000
第三条字幕

4
00:00:31,000 --> 00:00:39,000
第四条字幕

5
00:00:41,000 --> 00:00:49,000
第五条字幕

6
00:00:51,000 --> 00:00:59,000
第六条字幕
"@ | Set-Content -Path $zh -Encoding UTF8
@"
1
00:00:00,500 --> 00:00:09,000
First subtitle (English)

2
00:00:11,000 --> 00:00:19,000
Second subtitle (English)

3
00:00:21,000 --> 00:00:29,000
Third subtitle

4
00:00:31,000 --> 00:00:39,000
Fourth subtitle

5
00:00:41,000 --> 00:00:49,000
Fifth subtitle

6
00:00:51,000 --> 00:00:59,000
Sixth subtitle
"@ | Set-Content -Path $en -Encoding UTF8

Run-FFmpeg @(
    # `-map 0` 把 loop60s.mp4 的视频与音频**原样**搬过来（配 `-c copy` 不重编码）
    "-i", (Join-Path $Dest "loop60s.mp4"),
    "-i", $zh, "-i", $en,
    "-map", "0", "-map", "1:s", "-map", "2:s",
    "-c", "copy",
    # 字幕轨必须显式转成 mov_text：`-c copy` 覆盖不到它（源是文本 srt，
    # MP4 里放不了裸 srt），而 MP4 容器也不会自动挑编码器
    "-c:s", "mov_text",
    "-metadata:s:s:0", "language=chi",
    "-metadata:s:s:1", "language=eng",
    (Join-Path $Dest "loop60s-subs.mp4")
) "loop60s-subs.mp4（loop60s.mp4 + 2 条内嵌字幕轨）"
Remove-Item $subsDir -Recurse -Force -ErrorAction SilentlyContinue

# ---------------------------------------------------------------- 汇总
Write-Host ""
Write-Host "产物："
Get-ChildItem $Dest -File | Sort-Object Length -Descending | ForEach-Object {
    Write-Host ("  {0,-24} {1,8:N1} MB" -f $_.Name, ($_.Length / 1MB))
}
