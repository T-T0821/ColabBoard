//! CPU で塗る小さなラスタライザ。
//!
//! 塗りは 1 画素を 4 点で調べて覆い率を出す(アンチエイリアス)。内側だと分かっている矩形
//! (`solid`)は調べずに塗り、穴だと分かっている矩形(`hole`)は飛ばすので、大きな図形でも速い。
//! 線は線分までの距離から覆い率を出し、折れ線全体を 1 枚のマスクにしてから重ねる
//! (半透明の蛍光ペンでも継ぎ目が濃くならない)。

use board_core::geom::{Rect, distance_to_segment};
use board_core::{Rgba8, Vec2};
use image::{Rgba, RgbaImage};

/// 1 画素の中で調べる点(4 点)。
const SAMPLES: [(f32, f32); 4] = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];

pub struct Canvas {
    pub img: RgbaImage,
}

impl Canvas {
    /// `bg` が `None` なら透明。
    pub fn new(w: u32, h: u32, bg: Option<Rgba8>) -> Self {
        let px = bg.map(|c| c.0).unwrap_or([0, 0, 0, 0]);
        Self {
            img: RgbaImage::from_pixel(w, h, Rgba(px)),
        }
    }

    pub fn width(&self) -> u32 {
        self.img.width()
    }

    pub fn height(&self) -> u32 {
        self.img.height()
    }

    /// 画面全体の矩形。
    pub fn bounds(&self) -> Rect {
        Rect::new(
            Vec2::ZERO,
            Vec2::new(self.img.width() as f32, self.img.height() as f32),
        )
    }

    /// 色 `c` を覆い率 `coverage` で重ねる(ソースオーバー)。
    pub fn blend(&mut self, x: i32, y: i32, c: Rgba8, coverage: f32) {
        if x < 0 || y < 0 || x >= self.img.width() as i32 || y >= self.img.height() as i32 {
            return;
        }
        let a = c.0[3] as f32 / 255.0 * coverage.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let p = self.img.get_pixel_mut(x as u32, y as u32);
        let da = p.0[3] as f32 / 255.0;
        let out_a = a + da * (1.0 - a);
        if out_a <= 0.0 {
            return;
        }
        for i in 0..3 {
            let v = (c.0[i] as f32 * a + p.0[i] as f32 * da * (1.0 - a)) / out_a;
            p.0[i] = v.round().clamp(0.0, 255.0) as u8;
        }
        p.0[3] = (out_a * 255.0).round() as u8;
    }

    /// 画素の範囲(画面内に切り詰めた `[x0, x1) × [y0, y1)`)。
    fn pixel_range(&self, r: &Rect) -> Option<(i32, i32, i32, i32)> {
        let (w, h) = (self.img.width() as f32, self.img.height() as f32);
        let x0 = r.min.x.max(0.0).floor() as i32;
        let y0 = r.min.y.max(0.0).floor() as i32;
        let x1 = r.max.x.min(w).ceil() as i32;
        let y1 = r.max.y.min(h).ceil() as i32;
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    /// `inside(x, y)` が真の部分を `r` の範囲で塗る。
    ///
    /// `solid` は確実に内側の矩形、`hole` は確実に外側(穴)の矩形(どちらも任意)。
    pub fn fill_where(
        &mut self,
        r: Rect,
        c: Rgba8,
        solid: Option<Rect>,
        hole: Option<Rect>,
        inside: impl Fn(f32, f32) -> bool,
    ) {
        let Some((x0, y0, x1, y1)) = self.pixel_range(&r) else {
            return;
        };
        let covers = |q: &Option<Rect>, x: f32, y: f32| {
            q.is_some_and(|q| {
                x >= q.min.x && x + 1.0 <= q.max.x && y >= q.min.y && y + 1.0 <= q.max.y
            })
        };
        for y in y0..y1 {
            let fy = y as f32;
            for x in x0..x1 {
                let fx = x as f32;
                if covers(&solid, fx, fy) {
                    self.blend(x, y, c, 1.0);
                    continue;
                }
                if covers(&hole, fx, fy) {
                    continue;
                }
                let n = SAMPLES
                    .iter()
                    .filter(|(dx, dy)| inside(fx + dx, fy + dy))
                    .count();
                if n > 0 {
                    self.blend(x, y, c, n as f32 / SAMPLES.len() as f32);
                }
            }
        }
    }

    /// 矩形を塗る(角は画素の端数も考える)。
    pub fn rect(&mut self, r: Rect, c: Rgba8) {
        self.fill_where(r, c, Some(r), None, |x, y| r.contains(Vec2::new(x, y)));
    }

    /// 角丸の矩形を塗る。
    pub fn rounded(&mut self, r: Rect, radius: f32, c: Rgba8) {
        let rad = radius.min(r.size().min_element() * 0.5).max(0.0);
        let solid = r.expand(-rad * 0.3);
        self.fill_where(r, c, Some(solid), None, |x, y| in_rounded(&r, rad, x, y));
    }

    /// 角丸の矩形の縁(内側に幅 `width`)。
    pub fn outline_rounded(&mut self, r: Rect, radius: f32, width: f32, c: Rgba8) {
        let w = width.max(1.0);
        let rad = radius.min(r.size().min_element() * 0.5).max(0.0);
        let inner = r.expand(-w);
        let irad = (rad - w).max(0.0);
        let has_inner = inner.size().min_element() > 0.0;
        let hole = has_inner.then(|| inner.expand(-irad * 0.3));
        self.fill_where(r, c, None, hole, |x, y| {
            in_rounded(&r, rad, x, y) && !(has_inner && in_rounded(&inner, irad, x, y))
        });
    }

    pub fn ellipse(&mut self, r: Rect, c: Rgba8) {
        let ctr = r.center();
        let half = (r.size() * 0.5).max(Vec2::splat(0.5));
        let solid = Rect::new(ctr - half * 0.7, ctr + half * 0.7);
        self.fill_where(r, c, Some(solid), None, |x, y| {
            ((x - ctr.x) / half.x).powi(2) + ((y - ctr.y) / half.y).powi(2) <= 1.0
        });
    }

    /// 楕円の縁(内側に幅 `width`)。
    pub fn outline_ellipse(&mut self, r: Rect, width: f32, c: Rgba8) {
        let w = width.max(1.0);
        let ctr = r.center();
        let ho = (r.size() * 0.5).max(Vec2::splat(0.5));
        let hi = (ho - Vec2::splat(w)).max(Vec2::splat(0.01));
        let hole = Rect::new(ctr - hi * 0.7, ctr + hi * 0.7);
        self.fill_where(r, c, None, Some(hole), |x, y| {
            let o = ((x - ctr.x) / ho.x).powi(2) + ((y - ctr.y) / ho.y).powi(2);
            let i = ((x - ctr.x) / hi.x).powi(2) + ((y - ctr.y) / hi.y).powi(2);
            o <= 1.0 && i > 1.0
        });
    }

    /// 凸でなくてもよい多角形を塗る(偶奇規則)。
    pub fn polygon(&mut self, pts: &[Vec2], c: Rgba8) {
        if pts.len() < 3 {
            return;
        }
        let mut r = Rect::new(pts[0], pts[0]);
        for p in pts {
            r = r.union(&Rect::new(*p, *p));
        }
        self.fill_where(r, c, None, None, |x, y| point_in_polygon(pts, x, y));
    }

    /// 折れ線を引く。`dash` があれば(線の長さ, 間隔)の破線。
    pub fn polyline(&mut self, pts: &[Vec2], width: f32, c: Rgba8, dash: Option<(f32, f32)>) {
        if pts.is_empty() || c.0[3] == 0 {
            return;
        }
        let hw = (width * 0.5).max(0.5);
        let segs: Vec<(Vec2, Vec2)> = match dash {
            Some((on, off)) if on > 0.0 && off > 0.0 => dashed(pts, on, off),
            _ if pts.len() == 1 => vec![(pts[0], pts[0])],
            _ => pts.windows(2).map(|w| (w[0], w[1])).collect(),
        };
        if segs.is_empty() {
            return;
        }
        let mut bb = Rect::new(segs[0].0, segs[0].0);
        for (a, b) in &segs {
            bb = bb.union(&Rect::from_points(*a, *b));
        }
        let Some((x0, y0, x1, y1)) = self.pixel_range(&bb.expand(hw + 1.0)) else {
            return;
        };
        let (mw, mh) = ((x1 - x0) as usize, (y1 - y0) as usize);
        // 大きすぎる線(拡大しすぎ)はマスクを作らず、線分ごとに塗る。
        if mw.saturating_mul(mh) > 64 << 20 {
            for (a, b) in segs {
                self.segment_direct(a, b, hw, c);
            }
            return;
        }
        let mut mask = vec![0f32; mw * mh];
        for (a, b) in &segs {
            let r = Rect::from_points(*a, *b).expand(hw + 1.0);
            let Some((sx0, sy0, sx1, sy1)) = self.pixel_range(&r) else {
                continue;
            };
            for y in sy0..sy1 {
                for x in sx0..sx1 {
                    let d = distance_to_segment(Vec2::new(x as f32 + 0.5, y as f32 + 0.5), *a, *b);
                    let cov = (hw + 0.5 - d).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        let m = &mut mask[(y - y0) as usize * mw + (x - x0) as usize];
                        *m = m.max(cov);
                    }
                }
            }
        }
        for (i, cov) in mask.into_iter().enumerate() {
            if cov > 0.0 {
                self.blend(x0 + (i % mw) as i32, y0 + (i / mw) as i32, c, cov);
            }
        }
    }

    /// 1 本の線分(折れ線の継ぎ目は気にしない)。
    pub fn line(&mut self, a: Vec2, b: Vec2, width: f32, c: Rgba8) {
        self.segment_direct(a, b, (width * 0.5).max(0.5), c);
    }

    fn segment_direct(&mut self, a: Vec2, b: Vec2, hw: f32, c: Rgba8) {
        let r = Rect::from_points(a, b).expand(hw + 1.0);
        let Some((x0, y0, x1, y1)) = self.pixel_range(&r) else {
            return;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let d = distance_to_segment(Vec2::new(x as f32 + 0.5, y as f32 + 0.5), a, b);
                let cov = (hw + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, c, cov);
                }
            }
        }
    }

    /// 画像を矩形 `dst` に引き伸ばして重ねる(`clip` の外は描かない)。
    pub fn image(&mut self, src: &RgbaImage, dst: Rect, clip: Rect) {
        let (sw, sh) = (src.width(), src.height());
        if sw == 0 || sh == 0 || dst.size().min_element() <= 0.0 {
            return;
        }
        // 大きく縮めるときは先に縮めておく(ぎざぎざを防ぐ)。
        let (tw, th) = (dst.size().x.ceil() as u32, dst.size().y.ceil() as u32);
        let shrunk;
        let src = if tw.max(1) * 2 < sw || th.max(1) * 2 < sh {
            shrunk = image::imageops::resize(
                src,
                tw.clamp(1, sw),
                th.clamp(1, sh),
                image::imageops::FilterType::Triangle,
            );
            &shrunk
        } else {
            src
        };
        let (sw, sh) = (src.width() as f32, src.height() as f32);
        let vis = Rect::new(dst.min.max(clip.min), dst.max.min(clip.max));
        let Some((x0, y0, x1, y1)) = self.pixel_range(&vis) else {
            return;
        };
        let size = dst.size();
        for y in y0..y1 {
            for x in x0..x1 {
                let u = ((x as f32 + 0.5 - dst.min.x) / size.x * sw - 0.5).clamp(0.0, sw - 1.0);
                let v = ((y as f32 + 0.5 - dst.min.y) / size.y * sh - 0.5).clamp(0.0, sh - 1.0);
                let px = bilinear(src, u, v);
                self.blend(
                    x,
                    y,
                    Rgba8([px[0], px[1], px[2], 255]),
                    px[3] as f32 / 255.0,
                );
            }
        }
    }
}

fn bilinear(img: &RgbaImage, u: f32, v: f32) -> [u8; 4] {
    let (x0, y0) = (u.floor() as u32, v.floor() as u32);
    let x1 = (x0 + 1).min(img.width() - 1);
    let y1 = (y0 + 1).min(img.height() - 1);
    let (fx, fy) = (u - x0 as f32, v - y0 as f32);
    let p = |x, y| img.get_pixel(x, y).0;
    let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    let mut out = [0u8; 4];
    for i in 0..4 {
        let top = a[i] as f32 * (1.0 - fx) + b[i] as f32 * fx;
        let bottom = c[i] as f32 * (1.0 - fx) + d[i] as f32 * fx;
        out[i] = (top * (1.0 - fy) + bottom * fy).round() as u8;
    }
    out
}

pub fn in_rounded(r: &Rect, rad: f32, x: f32, y: f32) -> bool {
    if x < r.min.x || x > r.max.x || y < r.min.y || y > r.max.y {
        return false;
    }
    let cx = x.clamp(r.min.x + rad, r.max.x - rad);
    let cy = y.clamp(r.min.y + rad, r.max.y - rad);
    (x - cx).powi(2) + (y - cy).powi(2) <= rad * rad + 1e-3
}

pub fn point_in_polygon(pts: &[Vec2], x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[j]);
        if (a.y > y) != (b.y > y) && x < (b.x - a.x) * (y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// 折れ線を破線の線分に切る。
fn dashed(pts: &[Vec2], on: f32, off: f32) -> Vec<(Vec2, Vec2)> {
    let mut out = Vec::new();
    let period = on + off;
    let mut phase = 0.0f32; // 周期の中の位置
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = a.distance(b);
        if len <= 0.0 {
            continue;
        }
        let dir = (b - a) / len;
        let mut t = 0.0;
        while t < len {
            let left_in_phase = if phase < on {
                on - phase
            } else {
                period - phase
            };
            let step = left_in_phase.min(len - t);
            if phase < on {
                out.push((a + dir * t, a + dir * (t + step)));
            }
            t += step;
            phase = (phase + step) % period;
        }
        // 1 万本を超えるような破線は描かない(拡大しすぎ)。
        if out.len() > 100_000 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_onto_transparent_keeps_color() {
        let mut cv = Canvas::new(2, 2, None);
        cv.blend(0, 0, Rgba8([200, 100, 50, 255]), 0.5);
        let p = cv.img.get_pixel(0, 0).0;
        assert_eq!(&p[..3], &[200, 100, 50]);
        assert!((126..=129).contains(&p[3]));
    }

    #[test]
    fn rect_fills_inside_only() {
        let mut cv = Canvas::new(10, 10, Some(Rgba8::WHITE));
        cv.rect(
            Rect::new(Vec2::new(2.0, 2.0), Vec2::new(6.0, 6.0)),
            Rgba8([0, 0, 0, 255]),
        );
        assert_eq!(cv.img.get_pixel(3, 3).0, [0, 0, 0, 255]);
        assert_eq!(cv.img.get_pixel(8, 8).0, [255, 255, 255, 255]);
    }

    #[test]
    fn dashes_alternate() {
        let segs = dashed(&[Vec2::ZERO, Vec2::new(10.0, 0.0)], 2.0, 3.0);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1].0, Vec2::new(5.0, 0.0));
    }

    #[test]
    fn highlighter_does_not_double_at_joints() {
        let mut cv = Canvas::new(20, 20, Some(Rgba8::WHITE));
        let pts = [
            Vec2::new(2.0, 10.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(18.0, 10.0),
        ];
        cv.polyline(&pts, 4.0, Rgba8([255, 0, 0, 128]), None);
        assert_eq!(cv.img.get_pixel(10, 10).0, cv.img.get_pixel(5, 10).0);
    }
}
