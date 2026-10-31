//! ブラウザでの計測モード(`?bench`)。
//!
//! 決まった手順でカメラを動かしながら毎フレーム描画し、フレーム間隔と CPU 側の描画時間を
//! 区間ごとに集計する。結果は `window.__benchResult`(JSON 文字列)に置くので、
//! `tools/bench-web.mjs` などの自動計測から読み取れる。
//!
//! カメラの動きはボードの外接矩形だけから決まるので、同じ `?n=` なら毎回同じ条件になる。

use std::sync::{Arc, Mutex};

use board_core::{Camera, Rect, Vec2};

/// 計測の区間。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 開始直後の数フレーム(バッファ転送やシェーダの準備が入るので集計しない)。
    Warmup,
    /// 等倍で横方向に流し見る。
    Pan,
    /// 等倍から全体表示まで縮小する。
    ZoomOut,
    /// 全体表示のまま少し動かす(全要素が画面内)。
    PanOverview,
}

const PLAN: [(Phase, usize); 4] = [
    (Phase::Warmup, 30),
    (Phase::Pan, 240),
    (Phase::ZoomOut, 240),
    (Phase::PanOverview, 120),
];

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Warmup => "warmup",
            Phase::Pan => "pan_100",
            Phase::ZoomOut => "zoom_out",
            Phase::PanOverview => "pan_overview",
        }
    }
}

/// 全フレーム数。
pub fn total_frames() -> usize {
    PLAN.iter().map(|p| p.1).sum()
}

/// `frame` 番目(0 始まり)の区間と、区間内の進み具合(0..1)。
pub fn phase_at(frame: usize) -> Option<(Phase, f32)> {
    let mut start = 0;
    for (phase, len) in PLAN {
        if frame < start + len {
            return Some((phase, (frame - start) as f32 / len as f32));
        }
        start += len;
    }
    None
}

/// `frame` 番目のカメラを決める。`viewport` は物理ピクセル。
pub fn camera_for(frame: usize, extent: &Rect, viewport: Vec2) -> Option<Camera> {
    let (phase, t) = phase_at(frame)?;
    let mut cam = Camera::new(viewport);
    let mut fit = cam;
    fit.fit(extent.min, extent.max, 40.0);
    let fit_zoom = fit.zoom.min(1.0);
    let center = extent.center();
    // 等倍で画面 20 枚分を横切る(ボードが狭ければその幅)。
    let span = (viewport.x * 20.0).min(extent.size().x.max(viewport.x));
    let pan_start = center - Vec2::new(span * 0.5, 0.0);

    let (zoom, c) = match phase {
        Phase::Warmup => (1.0, pan_start),
        Phase::Pan => (1.0, pan_start + Vec2::new(span * t, 0.0)),
        Phase::ZoomOut => {
            // 倍率は等比で変える(見た目に一定の速さ)。
            let z = (fit_zoom.ln() * t).exp();
            let from = pan_start + Vec2::new(span, 0.0);
            (z, from + (center - from) * t)
        }
        Phase::PanOverview => {
            let a = t * std::f32::consts::TAU;
            let r = viewport / fit_zoom * 0.05;
            (fit_zoom, center + Vec2::new(a.cos() * r.x, a.sin() * r.y))
        }
    };
    cam.zoom = zoom;
    cam.origin = c - viewport / zoom * 0.5;
    Some(cam)
}

/// 区間ごとの集計。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub frames: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

pub fn stats(samples: &[f64]) -> Stats {
    if samples.is_empty() {
        return Stats::default();
    }
    let mut v = samples.to_vec();
    v.sort_by(f64::total_cmp);
    let pick = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    Stats {
        frames: v.len(),
        mean: v.iter().sum::<f64>() / v.len() as f64,
        p50: pick(0.5),
        p95: pick(0.95),
        max: *v.last().unwrap(),
    }
}

/// 計測の進行と記録。
pub struct Bench {
    frame: usize,
    extent: Rect,
    last_now: Option<f64>,
    /// (区間, フレーム間隔 ms, CPU 側の描画時間 ms, 描いたインスタンス数)
    samples: Vec<(Phase, f64, f64, u32)>,
    pending: Option<(Phase, f64)>,
    /// GPU が描き終わるまでの時間(提出からの ms)。GPU の完了通知から非同期に足される。
    gpu: Arc<Mutex<Vec<(Phase, f64)>>>,
    gpu_expected: usize,
    /// 最後に GPU 計測を仕掛けたフレーム(同じフレームで二重に数えない)。
    probed: usize,
}

/// GPU の完了通知を受け取る口。
pub type GpuSink = Arc<Mutex<Vec<(Phase, f64)>>>;

impl Bench {
    pub fn new(extent: Rect) -> Self {
        Self {
            frame: 0,
            extent,
            last_now: None,
            samples: Vec::with_capacity(total_frames()),
            pending: None,
            gpu: Arc::default(),
            gpu_expected: 0,
            probed: 0,
        }
    }

    /// 全フレームを描き終えたか。
    pub fn is_done(&self) -> bool {
        self.frame >= total_frames()
    }

    /// GPU の完了通知がすべて届いたか。
    pub fn gpu_settled(&self) -> bool {
        self.gpu
            .lock()
            .map_or(true, |g| g.len() >= self.gpu_expected)
    }

    /// 今のフレームの GPU 完了時間を記録する口(集計しないフレームなら `None`)。
    pub fn gpu_probe(&mut self) -> Option<(Phase, GpuSink)> {
        if self.probed == self.frame {
            return None;
        }
        self.probed = self.frame;
        let phase = phase_at(self.frame.checked_sub(1)?)?.0;
        if phase == Phase::Warmup {
            return None;
        }
        self.gpu_expected += 1;
        Some((phase, self.gpu.clone()))
    }

    /// 次のフレームのカメラ。終わっていれば `None`。
    pub fn next_camera(&mut self, viewport: Vec2, now: f64) -> Option<Camera> {
        let cam = camera_for(self.frame, &self.extent, viewport)?;
        let phase = phase_at(self.frame).map(|p| p.0)?;
        let interval = self.last_now.map(|prev| now - prev);
        self.last_now = Some(now);
        self.pending = interval.map(|dt| (phase, dt));
        self.frame += 1;
        Some(cam)
    }

    /// 描画にかかった CPU 時間と、描いたインスタンス数を記録する。
    pub fn record(&mut self, cpu_ms: f64, drawn: u32) {
        if let Some((phase, dt)) = self.pending.take() {
            self.samples.push((phase, dt, cpu_ms, drawn));
        }
    }

    /// 結果の JSON。
    pub fn result_json(&self, meta: &[(&str, String)], timings: &[(&str, f64)]) -> String {
        let mut out = String::from("{");
        for (k, v) in meta {
            out.push_str(&format!("{k:?}:{v:?},"));
        }
        out.push_str("\"timings_ms\":{");
        let t: Vec<String> = timings
            .iter()
            .map(|(k, v)| format!("{k:?}:{v:.1}"))
            .collect();
        out.push_str(&t.join(","));
        out.push_str("},\"phases\":{");
        let mut phases = Vec::new();
        for (phase, _) in PLAN.iter().skip(1) {
            let rows: Vec<_> = self.samples.iter().filter(|s| s.0 == *phase).collect();
            let interval = stats(&rows.iter().map(|s| s.1).collect::<Vec<_>>());
            let cpu = stats(&rows.iter().map(|s| s.2).collect::<Vec<_>>());
            let drawn = rows.iter().map(|s| s.3 as f64).sum::<f64>() / rows.len().max(1) as f64;
            let gpu = self.gpu.lock().map_or_else(
                |_| Stats::default(),
                |g| {
                    stats(
                        &g.iter()
                            .filter(|s| s.0 == *phase)
                            .map(|s| s.1)
                            .collect::<Vec<_>>(),
                    )
                },
            );
            phases.push(format!(
                "{:?}:{{\"frames\":{},\"fps\":{:.1},\"frame_ms\":{{\"mean\":{:.2},\"p50\":{:.2},\"p95\":{:.2},\"max\":{:.2}}},\"cpu_ms\":{{\"mean\":{:.2},\"p95\":{:.2}}},\"gpu_done_ms\":{{\"mean\":{:.2},\"p50\":{:.2},\"p95\":{:.2}}},\"drawn_instances\":{:.0}}}",
                phase.name(),
                interval.frames,
                1000.0 / interval.mean.max(1e-6),
                interval.mean,
                interval.p50,
                interval.p95,
                interval.max,
                cpu.mean,
                cpu.p95,
                gpu.mean,
                gpu.p50,
                gpu.p95,
                drawn
            ));
        }
        out.push_str(&phases.join(","));
        out.push_str("}}");
        out
    }

    /// HUD に出す 1 行の要約。
    pub fn summary(&self) -> String {
        let parts: Vec<String> = PLAN
            .iter()
            .skip(1)
            .map(|(phase, _)| {
                let v: Vec<f64> = self
                    .samples
                    .iter()
                    .filter(|s| s.0 == *phase)
                    .map(|s| s.1)
                    .collect();
                let s = stats(&v);
                format!("{} {:.0}fps", phase.name(), 1000.0 / s.mean.max(1e-6))
            })
            .collect();
        parts.join(" / ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_covers_all_frames() {
        assert_eq!(phase_at(0).unwrap().0, Phase::Warmup);
        assert_eq!(phase_at(30).unwrap().0, Phase::Pan);
        assert!(phase_at(total_frames()).is_none());
    }

    #[test]
    fn camera_path_is_deterministic_and_ends_at_overview() {
        let extent = Rect::new(Vec2::ZERO, Vec2::splat(26_000.0));
        let vp = Vec2::new(1920.0, 1080.0);
        let a = camera_for(100, &extent, vp).unwrap();
        let b = camera_for(100, &extent, vp).unwrap();
        assert_eq!(a.origin, b.origin);
        assert_eq!(a.zoom, 1.0);
        let last = camera_for(total_frames() - 1, &extent, vp).unwrap();
        assert!(last.zoom < 0.05);
        // 全体表示ではボード全体がほぼ画面に入る。
        let (lo, hi) = last.visible_world_rect();
        assert!(hi.x - lo.x >= extent.size().x * 0.9);
    }

    #[test]
    fn records_and_reports() {
        let extent = Rect::new(Vec2::ZERO, Vec2::splat(1000.0));
        let mut b = Bench::new(extent);
        let mut now = 0.0;
        while b.next_camera(Vec2::new(800.0, 600.0), now).is_some() {
            b.record(1.0, 10);
            now += 16.0;
        }
        assert!(b.is_done());
        let json = b.result_json(&[("backend", "WebGL2".into())], &[("first_frame", 12.0)]);
        assert!(json.contains("\"pan_100\":{\"frames\":240,\"fps\":62.5"));
        assert!(json.contains("\"first_frame\":12.0"));
        assert_eq!(stats(&[3.0, 1.0, 2.0]).p50, 2.0);
    }
}
