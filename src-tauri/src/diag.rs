//! 解码健康度：把 mpv 的观测项收成一份能下判断的数据。
//!
//! ## 为什么要有这个模块
//!
//! 0.3.0 加了 `hwdec-current`，但它只回答一个问题：**硬解有没有生效**。
//! 而「解码有没有问题」是另一个问题，画面能播但 CPU 跑满、机器发烫时，
//! 硬解生效也照样是有问题的。判断这件事需要的是丢帧、实际帧率、零拷贝
//! 这些数据，而不是一个 yes/no。
//!
//! 这也是本项目一贯的做法：把「静默发生的事」变成看得见的东西。
//!
//! ## 数据分两类，取法不同
//!
//! * **低频项**（编码名、分辨率、丢帧计数…）走 `OBSERVED_PROPERTIES` 事件，
//!   只在真的变化时才回调。
//! * **高频项**（实际帧率、显示器刷新率、缓存时长、速度）刻意**不观察**——
//!   它们每个视频帧都可能变，观察等于把每秒几十上百条事件转成界面刷新，
//!   和当初刻意不观察 `time-pos` 是同一个理由。改为 `refresh` 里按界面
//!   刷新节奏主动读一次，**而且只在面板可见时读**，隐藏时开销为零。
//!
//! ## 单位与口径
//!
//! mpv 的帧率是「每秒帧数」而不是「每帧秒数」，这里保持原样不做换算；
//! 报告里显示成 `25.0/25.0`（实际/容器）这种一眼能比较的形式。

use crate::lang;
use crate::mpv::{MpvPlayer, PropertyChange};

/// mpv 报 `property unavailable` 时的错误文本包含这个词。
///
/// mpv 把「属性存在但当前没有值」（比如解码器还没建）和「属性名不存在」
/// 分成不同错误码，但我们只能拿到文本。读取失败一律当「暂时没有值」处理，
/// 保留上一份数据——这与 `decode_property` 里对 null 数据的处理一致。
fn is_unavailable(err: &str) -> bool {
    err.contains("unavailable") || err.contains("not found") || err.contains("parameter")
}

/// 一条诊断结论的严重程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 一切正常
    Ok,
    /// 有值得注意但不影��播放的东西
    Warn,
    /// 确定有问题
    Bad,
}

/// 一次健康度判断的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    /// 最严重的一条
    pub level: Level,
    /// 每条问题一行，已经本地化
    pub notes: Vec<String>,
}

/// 当前解码状态。只在主线程读写。
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    // ---- 事件驱动（低频）----
    /// `hwdec-current`：`d3d11va` / `dxva2` / `no` / 空串（还没解码）
    pub hwdec: String,
    /// `video-format`：如 `h264`
    pub video_format: String,
    /// `video-params/pixelformat`：软解时 `yuv420p`，D3D11 硬解时 `d3d11`
    pub pixel_format: String,
    /// `video-params/w` / `h`：源分辨率
    pub src: Option<(i64, i64)>,
    /// `video-out-params/w` / `h`：输出分辨率
    pub out: Option<(i64, i64)>,
    /// `decoder-frame-drop-count`
    pub decoder_drops: Option<i64>,
    /// `frame-drop-count`。mpv 旧文档里的名字是 `vo-drop-frame-count`，
    /// 实测在这份 libmpv 上那个名字**不存在**，照抄会让程序起不来。
    pub vo_drops: Option<i64>,
    /// `current-vo`
    pub vo: String,
    /// `video-sync`
    pub video_sync: String,

    // ---- 主动读（高频，仅面板可见时）----
    /// `estimated-vf-fps`：实际送出去的帧率
    pub vf_fps: Option<f64>,
    /// `container-fps`：文件标称帧率
    pub container_fps: Option<f64>,
    /// `display-fps`：显示器刷新率，仅有真实 VO 时可读
    pub display_fps: Option<f64>,
    /// `speed`
    pub speed: Option<f64>,
    /// `demuxer-cache-duration`：已缓存的秒数
    pub cache_duration: Option<f64>,
    /// `demuxer-cache-idle`
    pub cache_idle: Option<bool>,
}

impl Diagnostics {
    /// 换文件时清空。
    ///
    /// 不清的话上一段视频的分辨率与丢帧数会留在面板上，看起来像新文件也
    /// 丢了 10 帧——那是误导。丢帧计数在新文件加载后 mpv 自己会重置，
    /// 但**解码器建好之前**这段时间里读到的是旧文件的值，所以这里主动清。
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// 处理一条观测事件。
    pub fn apply(&mut self, change: &PropertyChange) {
        match change.name {
            "hwdec-current" => self.hwdec = text(change),
            "video-format" => self.video_format = text(change),
            "video-params/pixelformat" => self.pixel_format = text(change),
            "video-params/w" => self.set_src_w(int(change)),
            "video-params/h" => self.set_src_h(int(change)),
            "video-out-params/w" => self.set_out_w(int(change)),
            "video-out-params/h" => self.set_out_h(int(change)),
            "decoder-frame-drop-count" => self.decoder_drops = int(change),
            "frame-drop-count" => self.vo_drops = int(change),
            "current-vo" => self.vo = text(change),
            "video-sync" => self.video_sync = text(change),
            _ => {}
        }
    }

    /// 主动读一遍高频项。
    ///
    /// 只在面板可见时调用。读失败**保留上一份值**而不是清零：属性在解码器
    /// 建立之前是不可用的，那属于「暂时没有」而不是「变成 0」。
    pub fn refresh(&mut self, player: &MpvPlayer) {
        if let Some(v) = read_f64(player, "estimated-vf-fps") {
            self.vf_fps = Some(v);
        }
        if let Some(v) = read_f64(player, "container-fps") {
            self.container_fps = Some(v);
        }
        if let Some(v) = read_f64(player, "display-fps") {
            self.display_fps = Some(v);
        }
        if let Some(v) = read_f64(player, "speed") {
            self.speed = Some(v);
        }
        if let Some(v) = read_f64(player, "demuxer-cache-duration") {
            self.cache_duration = Some(v);
        }
        if let Ok(v) = player.get_flag_property("demuxer-cache-idle") {
            self.cache_idle = Some(v);
        }
    }

    fn set_src_w(&mut self, v: Option<i64>) {
        self.src = Some((v.unwrap_or(0), self.src.map(|s| s.1).unwrap_or(0)));
    }
    fn set_src_h(&mut self, v: Option<i64>) {
        self.src = Some((self.src.map(|s| s.0).unwrap_or(0), v.unwrap_or(0)));
    }
    fn set_out_w(&mut self, v: Option<i64>) {
        self.out = Some((v.unwrap_or(0), self.out.map(|s| s.1).unwrap_or(0)));
    }
    fn set_out_h(&mut self, v: Option<i64>) {
        self.out = Some((self.out.map(|s| s.0).unwrap_or(0), v.unwrap_or(0)));
    }

    /// 硬解是否真的生效。
    ///
    /// `no` 是**静默降级**：D3D11VA 格式探测失败时 mpv 不发 error、不发
    /// warning，只是把 hwdec 从白名单里拿掉继续用软解。这是低端机上最难
    /// 排查的性能问题——画面能播，但 CPU 跑满、机器发烫，用户只会觉得
    /// 「这个播放器慢」。
    pub fn hwdec_active(&self) -> bool {
        !self.hwdec.is_empty() && self.hwdec != "no"
    }

    /// 解码帧是否走了零拷贝（没有 CPU 往返）。
    pub fn zero_copy(&self) -> bool {
        self.pixel_format == "d3d11"
    }

    /// 下健康度判断。
    ///
    /// 这是这个模块存在的意义：不是把 mpv 的数字抄到屏幕上，而是**给一个
    /// 能照着改的结论**。每一条都对应一个具体可查的原因。
    pub fn health(&self, s: &lang::Strings) -> Health {
        let mut notes: Vec<(Level, String)> = Vec::new();

        // 1. 硬解静默降级
        if self.hwdec == "no" {
            notes.push((Level::Bad, s.diag_software_decode.to_string()));
        } else if self.hwdec.is_empty() {
            notes.push((Level::Warn, s.diag_no_decoder.to_string()));
        }

        // 2. 硬解生效但没走零拷贝 = 每帧仍在 CPU 与 GPU 之间往返
        if self.hwdec_active() && !self.pixel_format.is_empty() && !self.zero_copy() {
            notes.push((
                Level::Warn,
                format!("{} ({})", s.diag_no_zero_copy, self.pixel_format),
            ));
        }

        // 3. 丢帧。两个计数都要看：解码器丢和 VO 丢是不同的问题
        if let Some(n) = self.decoder_drops {
            if n > 0 {
                notes.push((Level::Bad, format!("{}: {n}", s.diag_decoder_drops)));
            }
        }
        if let Some(n) = self.vo_drops {
            if n > 0 {
                notes.push((Level::Warn, format!("{}: {n}", s.diag_vo_drops)));
            }
        }

        // 4. 实际帧率跟不上标称帧率 —— 解码吃力的直接信号。
        //    阈值 2% 是为了不把浮点抖动当成问题：mpv 的 fps 是从 PTS 算的，
        //    `estimated-vf-fps` 稳态下会在标称值附近小幅抖动。
        if let (Some(vf), Some(cf)) = (self.vf_fps, self.container_fps) {
            if cf > 1.0 && vf < cf * 0.98 {
                notes.push((
                    Level::Warn,
                    format!("{} {vf:.1}/{cf:.1}", s.diag_fps_behind),
                ));
            }
        }

        let level = notes.iter().map(|(l, _)| *l).max().unwrap_or(Level::Ok);
        Health {
            level,
            notes: notes.into_iter().map(|(_, t)| t).collect(),
        }
    }

    /// 面板上的「标签 / 值」行。
    ///
    /// 返回值是**成对**的：绘制时要按标签列对齐，而中英文标签宽度差很多，
    /// 所以两列的宽度必须分开量（见 `ui::Metrics::diag_label_w`）。
    ///
    /// 值里的 `—` 是「拿不到」的意思，和 0 有本质区别：没有数据不是零。
    pub fn rows(&self, s: &lang::Strings) -> Vec<(String, String)> {
        let dash = s.diag_unknown;
        let health = self.health(s);

        let mut rows: Vec<(String, String)> = Vec::with_capacity(7);

        // 硬件解码。`no` 单独用一句人话，不用光秃秃的 "no"
        let hwdec = if self.hwdec.is_empty() {
            dash.to_string()
        } else if self.hwdec == "no" {
            s.diag_software_short.to_string()
        } else if self.zero_copy() {
            format!("{} + {}", self.hwdec, s.diag_zero_copy)
        } else {
            self.hwdec.clone()
        };
        rows.push((s.diag_hwdec.to_string(), hwdec));

        // 编码 + 源分辨率
        let mut video = if self.video_format.is_empty() {
            dash.to_string()
        } else {
            self.video_format.clone()
        };
        if let Some((w, h)) = self.src {
            if w > 0 && h > 0 {
                video.push_str(&format!("  {w}×{h}"));
                if let Some((ow, oh)) = self.out {
                    // 输出分辨率与源不同 = 被缩放过，值得单独指出来
                    if ow > 0 && oh > 0 && (ow, oh) != (w, h) {
                        video.push_str(&format!(" → {ow}×{oh}"));
                    }
                }
            }
        }
        rows.push((s.diag_video.to_string(), video));

        // 帧率：实际/容器，够用且一眼能比。显示器刷新率没有诊断价值，不显示。
        let fps = match (self.vf_fps, self.container_fps) {
            (Some(v), Some(c)) => format!("{v:.1}/{c:.1}"),
            (Some(v), None) => format!("{v:.1}"),
            _ => dash.to_string(),
        };
        rows.push((s.diag_fps.to_string(), fps));

        // 丢帧：只显示非零的，全 0 时写「0」而不是藏起来——「确认过没丢帧」
        // 和「没看」是两件事
        let drops = match (self.decoder_drops, self.vo_drops) {
            (Some(d), Some(v)) => format!("{d} / {v}"),
            (Some(d), None) => format!("{d}"),
            (None, Some(v)) => format!("{v}"),
            (None, None) => dash.to_string(),
        };
        rows.push((s.diag_dropped.to_string(), drops));

        // 缓存：本地文件播放时缓存其实不分配（见 mpv/mod.rs 里 demuxer-* 的
        // 注释），所以这里显示的是一个诊断上无意义但能证明链路正常的读数。
        let cache = match self.cache_duration {
            Some(d) => format!("{d:.1}s"),
            None => dash.to_string(),
        };
        rows.push((s.diag_cache.to_string(), cache));

        // 倍速
        let speed = match self.speed {
            Some(v) => format!("{v:.2}×"),
            None => dash.to_string(),
        };
        rows.push((s.diag_speed.to_string(), speed));

        // 结论行。健康时给一句明确的「正常」——诊断工具只报问题不报正常，
        // 用户会怀疑是不是没在测。
        let verdict = if health.notes.is_empty() {
            s.diag_healthy.to_string()
        } else {
            health.notes.join("; ")
        };
        rows.push((s.diag_verdict.to_string(), verdict));

        rows
    }

    /// 可复制的完整诊断报告。
    ///
    /// 这个比面板重要：面板只能看当前这一秒，而用户报 bug 时贴出来的是
    /// **一段文字**。所以这里把机器、版本、mpv 能力、当前解码状态都带上，
    /// 并且只用换行分隔的 `键 = 值`，方便直接粘进 issue。
    pub fn report(&self, ctx: &ReportContext<'_>) -> String {
        let mut out = String::new();
        let mut line = |k: &str, v: &str| {
            out.push_str(k);
            out.push_str(" = ");
            out.push_str(v);
            out.push('\n');
        };

        line("app.version", ctx.app_version);
        line("mpv.api_version", &ctx.mpv_api_version.to_string());
        line("os", ctx.os);
        line("cpu.physical_cores", &ctx.physical_cores.to_string());
        line("gpu.adapter", ctx.gpu);
        // 没接入的显示设备数量单独一行：这本身是诊断信息（远程投屏 / 虚拟
        // 显示器装的机器上这个数会很大，而它们正是 D3D11VA 可能被选成默认
        // 适配器、从而静默退回软解的来源）
        line("gpu.inactive_devices", &ctx.gpu_inactive.to_string());
        line("display.dpi", &ctx.dpi.to_string());

        line("hwdec.current", non_empty(&self.hwdec));
        line("hwdec.zero_copy", &self.zero_copy().to_string());
        line("video.format", non_empty(&self.video_format));
        line("video.params_pixelformat", non_empty(&self.pixel_format));
        line("video.resolution", &resolution(self.src));
        line("video.out_resolution", &resolution(self.out));
        line("vo.current", non_empty(&self.vo));
        line("video.sync", non_empty(&self.video_sync));
        line("fps.container", &opt_f64(self.container_fps));
        line("fps.estimated_vf", &opt_f64(self.vf_fps));
        line("fps.display", &opt_f64(self.display_fps));
        line("drops.decoder", &opt_i64(self.decoder_drops));
        line("drops.vo", &opt_i64(self.vo_drops));
        line("cache.duration_s", &opt_f64(self.cache_duration));
        line(
            "cache.idle",
            &self.cache_idle.map(|b| b.to_string()).unwrap_or_default(),
        );
        line("speed", &opt_f64(self.speed));

        let health = self.health(ctx.strings);
        line(
            "verdict",
            match health.level {
                Level::Ok => "ok",
                Level::Warn => "warn",
                Level::Bad => "bad",
            },
        );
        out.push_str("verdict.notes = ");
        out.push_str(&if health.notes.is_empty() {
            "-".to_string()
        } else {
            health.notes.join("; ")
        });
        out.push('\n');
        out
    }
}

/// 报告里除诊断状态之外的那些机器信息。
pub struct ReportContext<'a> {
    pub app_version: &'a str,
    /// `mpv_client_api_version` 的返回值
    pub mpv_api_version: u32,
    /// `windows` crate 报的平台名
    pub os: &'a str,
    pub physical_cores: usize,
    /// 当前接在桌面上的显示适配器（`gpu::summary`）
    pub gpu: &'a str,
    /// 未接入的显示设备数量
    pub gpu_inactive: usize,
    pub dpi: u32,
    pub strings: &'a lang::Strings,
}

// ---------------------------------------------------------------- 小工具

fn text(c: &PropertyChange) -> String {
    c.value.as_text().unwrap_or_default().to_string()
}

fn int(c: &PropertyChange) -> Option<i64> {
    c.value.as_integer()
}

fn read_f64(player: &MpvPlayer, name: &str) -> Option<f64> {
    match player.get_double_property(name) {
        Ok(v) if v.is_finite() => Some(v),
        // 非有限值（NaN / inf）说明 mpv 自己也不知道，照着显示会得到 "NaN"
        Ok(_) => None,
        Err(e) if is_unavailable(&e) => None,
        Err(_) => None,
    }
}

fn non_empty(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

fn opt_f64(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.3}"))
        .unwrap_or_else(|| "-".to_string())
}

fn opt_i64(v: Option<i64>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string())
}

fn resolution(r: Option<(i64, i64)>) -> String {
    match r {
        Some((w, h)) if w > 0 && h > 0 => format!("{w}x{h}"),
        _ => "-".to_string(),
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn strings() -> lang::Strings {
        lang::Strings::new(lang::Lang::ZhCn)
    }

    fn loaded() -> Diagnostics {
        Diagnostics {
            hwdec: "d3d11va".into(),
            video_format: "h264".into(),
            pixel_format: "d3d11".into(),
            src: Some((1920, 1080)),
            out: Some((1920, 1080)),
            decoder_drops: Some(0),
            vo_drops: Some(0),
            vo: "gpu".into(),
            video_sync: "audio".into(),
            vf_fps: Some(60.0),
            container_fps: Some(60.0),
            display_fps: Some(144.0),
            speed: Some(1.0),
            cache_duration: Some(2.0),
            cache_idle: Some(true),
        }
    }

    #[test]
    fn 健康时给出明确结论而不是留空() {
        let s = strings();
        let h = loaded().health(&s);
        assert_eq!(h.level, Level::Ok, "不该有问题：{:?}", h.notes);
        assert!(h.notes.is_empty());
        // 面板上要显示「正常」而不是空白
        let rows = loaded().rows(&s);
        let verdict = rows.last().expect("要有结论行");
        assert_eq!(verdict.1, s.diag_healthy);
    }

    #[test]
    fn 硬解静默降级被判为最严重() {
        let s = strings();
        let mut d = loaded();
        d.hwdec = "no".into();
        d.pixel_format = "yuv420p".into();
        let h = d.health(&s);
        assert_eq!(h.level, Level::Bad);
        assert!(
            h.notes.iter().any(|n| n == s.diag_software_decode),
            "{:?}",
            h.notes
        );
    }

    #[test]
    fn 硬解生效但没走零拷贝会被指出() {
        let s = strings();
        let mut d = loaded();
        d.pixel_format = "yuv420p".into();
        let h = d.health(&s);
        assert_eq!(h.level, Level::Warn);
        assert!(
            h.notes.iter().any(|n| n.contains(s.diag_no_zero_copy)),
            "{:?}",
            h.notes
        );
        // 且必须带上实际的 pixelformat，否则没法判断是哪条路径
        assert!(h.notes.iter().any(|n| n.contains("yuv420p")));
    }

    #[test]
    fn 丢帧判为严重并带上数字() {
        let s = strings();
        let mut d = loaded();
        d.decoder_drops = Some(37);
        d.vo_drops = Some(4);
        let h = d.health(&s);
        assert_eq!(h.level, Level::Bad);
        assert!(h.notes.iter().any(|n| n.contains("37")), "{:?}", h.notes);
        assert!(h.notes.iter().any(|n| n.contains('4')), "{:?}", h.notes);
    }

    #[test]
    fn 帧率略低不算问题避免浮点抖动误报() {
        let s = strings();
        let mut d = loaded();
        // 59.7 / 60 只差 0.5%，是 mpv 从 PTS 算 fps 的正常抖动
        d.vf_fps = Some(59.7);
        assert_eq!(d.health(&s).level, Level::Ok);
        // 明显跟不上就要报
        d.vf_fps = Some(40.0);
        let h = d.health(&s);
        assert_eq!(h.level, Level::Warn);
        assert!(h.notes.iter().any(|n| n.contains("40.0")), "{:?}", h.notes);
    }

    #[test]
    fn 没有解码器时不判成硬解降级() {
        let s = strings();
        let d = Diagnostics::default();
        let h = d.health(&s);
        // 空 hwdec = 还没开始解码，不能说成「静默降级」
        assert_ne!(h.level, Level::Bad);
        assert!(h.notes.iter().any(|n| n == s.diag_no_decoder));
        assert!(!h.notes.iter().any(|n| n == s.diag_software_decode));
    }

    #[test]
    fn 面板把拿不到的值显示成横线而不是零() {
        let s = strings();
        let rows = Diagnostics::default().rows(&s);
        // 「没有数据」和「真的是 0」必须能区分开
        assert_eq!(rows[0].1, s.diag_unknown, "hwdec 应为横线");
        assert_eq!(rows[3].1, s.diag_unknown, "丢帧应为横线");
    }

    #[test]
    fn 分辨率被缩放过时会指出来() {
        let s = strings();
        let mut d = loaded();
        d.src = Some((1920, 1080));
        d.out = Some((1280, 720));
        let rows = d.rows(&s);
        let video = &rows[1].1;
        assert!(video.contains("1920×1080"), "{video}");
        assert!(video.contains("1280×720"), "{video}");
    }

    #[test]
    fn 硬解且零拷贝时一行说清两件事() {
        let s = strings();
        let rows = loaded().rows(&s);
        let hwdec = &rows[0].1;
        assert!(hwdec.contains("d3d11va"), "{hwdec}");
        assert!(hwdec.contains(s.diag_zero_copy), "{hwdec}");
    }

    #[test]
    fn 换文件会清掉上一段的诊断数据() {
        let mut d = loaded();
        d.reset();
        assert_eq!(d.hwdec, "");
        assert_eq!(d.decoder_drops, None);
        assert_eq!(d.src, None);
    }

    #[test]
    fn 分辨率是分两次事件到的也能拼起来() {
        let mut d = Diagnostics::default();
        d.apply(&prop("video-params/w", 1920i64));
        // h 还没到，这时只有一个宽
        assert_eq!(d.src, Some((1920, 0)));
        d.apply(&prop("video-params/h", 1080i64));
        assert_eq!(d.src, Some((1920, 1080)));
    }

    #[test]
    fn 报告每行都有键且不含空值() {
        let s = strings();
        let ctx = ReportContext {
            app_version: "0.4.0",
            mpv_api_version: 2 * 65536 + 1,
            os: "windows",
            physical_cores: 4,
            gpu: "Intel(R) UHD Graphics",
            gpu_inactive: 0,
            dpi: 120,
            strings: &s,
        };
        let text = loaded().report(&ctx);
        for l in text.lines() {
            assert!(l.contains(" = "), "缺键值分隔的行：{l:?}");
            let (_, v) = l.split_once(" = ").expect("有分隔符");
            assert!(!v.is_empty(), "值为空：{l:?}");
        }
        assert!(text.contains("hwdec.current = d3d11va"));
        assert!(text.contains("video.resolution = 1920x1080"));
        assert!(text.contains("verdict = ok"));
    }

    #[test]
    fn 报告在降级时给出_bad_而不是_ok() {
        let s = strings();
        let mut d = loaded();
        d.hwdec = "no".into();
        let ctx = ReportContext {
            app_version: "0.4.0",
            mpv_api_version: 0,
            os: "windows",
            physical_cores: 2,
            gpu: "-",
            gpu_inactive: 0,
            dpi: 96,
            strings: &s,
        };
        let text = d.report(&ctx);
        assert!(text.contains("verdict = bad"), "{text}");
        assert!(text.contains("hwdec.current = no"));
    }

    fn prop(name: &'static str, v: i64) -> PropertyChange {
        PropertyChange {
            name,
            value: crate::mpv::PropertyValue::Integer(v),
        }
    }
}
