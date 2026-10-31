//! コネクタ(図形どうしをつなぐ線・矢印)。
//!
//! 端は「自由な点」か「要素につながった点」。つながった端は相手が動くと追従する。
//! 実際の経路(`path`)は保存せず、ボードが相手の位置から計算し直す。

use glam::Vec2;

use crate::color::Rgba8;
use crate::element::{ElementId, Meta};
use crate::geom::{Rect, distance_to_segment};

/// コネクタの端。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Endpoint {
    /// ワールド座標の点。
    Free(Vec2),
    /// 要素につながった端。`anchor` は相手の外接矩形内の位置(0..1)。
    /// `None` なら相手の輪郭上で、もう一方の端に一番近い所に自動で付く。
    Attached { id: ElementId, anchor: Option<Vec2> },
}

impl Endpoint {
    pub fn attached_to(&self) -> Option<ElementId> {
        match self {
            Endpoint::Attached { id, .. } => Some(*id),
            Endpoint::Free(_) => None,
        }
    }
}

/// 線の引き方。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PathStyle {
    Straight,
    /// 縦横だけで折れ曲がる線。
    #[default]
    Elbow,
    Curved,
}

impl PathStyle {
    pub fn name(self) -> &'static str {
        match self {
            PathStyle::Straight => "straight",
            PathStyle::Elbow => "elbow",
            PathStyle::Curved => "curved",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s {
            "straight" => PathStyle::Straight,
            "curved" => PathStyle::Curved,
            _ => PathStyle::Elbow,
        }
    }
}

/// 端の飾り。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cap {
    #[default]
    None,
    Arrow,
}

impl Cap {
    pub fn name(self) -> &'static str {
        match self {
            Cap::None => "none",
            Cap::Arrow => "arrow",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s {
            "arrow" => Cap::Arrow,
            _ => Cap::None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Connector {
    pub id: ElementId,
    pub from: Endpoint,
    pub to: Endpoint,
    pub color: Rgba8,
    pub width: f32,
    pub style: PathStyle,
    pub start_cap: Cap,
    pub end_cap: Cap,
    /// 線の中ほどに出す文字。
    pub text: String,
    pub meta: Meta,
    /// 計算済みの経路(ワールド座標)。保存しない。
    pub path: Vec<Vec2>,
}

/// 曲線を折れ線にするときの分割数。
const CURVE_SEGMENTS: usize = 24;

impl Connector {
    pub fn new(id: ElementId, from: Endpoint, to: Endpoint) -> Self {
        let mut c = Self {
            id,
            from,
            to,
            color: Rgba8::hex(0x1f1f1f),
            width: 2.0,
            style: PathStyle::default(),
            start_cap: Cap::None,
            end_cap: Cap::Arrow,
            text: String::new(),
            meta: Meta::default(),
            path: Vec::new(),
        };
        c.route(|_| None);
        c
    }

    /// 矢じりの長さ。
    pub fn arrow_size(&self) -> f32 {
        8.0 + self.width * 3.0
    }

    pub fn start(&self) -> Vec2 {
        self.path.first().copied().unwrap_or(Vec2::ZERO)
    }

    pub fn end(&self) -> Vec2 {
        self.path.last().copied().unwrap_or(Vec2::ZERO)
    }

    /// 経路を計算し直す。`lookup` はつながった相手の外接矩形と図形の種類を返す。
    /// 戻り値は経路が変わったか。
    pub fn route(&mut self, lookup: impl Fn(ElementId) -> Option<Target>) -> bool {
        let target_of = |e: &Endpoint| match e {
            Endpoint::Attached { id, .. } => lookup(*id),
            Endpoint::Free(_) => None,
        };
        let from_t = target_of(&self.from);
        let to_t = target_of(&self.to);
        let from_rect = from_t.map(|t| t.rect);
        let to_rect = to_t.map(|t| t.rect);
        // 相手が見つからない(削除された)端は、前回の位置に残す。
        let prev = (self.start(), self.end());
        let rough = |e: &Endpoint, r: Option<Rect>, fallback: Vec2| match (e, r) {
            (Endpoint::Free(p), _) => *p,
            (
                Endpoint::Attached {
                    anchor: Some(a), ..
                },
                Some(r),
            ) => r.min + r.size() * *a,
            (Endpoint::Attached { anchor: None, .. }, Some(r)) => r.center(),
            (_, None) => fallback,
        };
        let a0 = rough(&self.from, from_rect, prev.0);
        let b0 = rough(&self.to, to_rect, prev.1);
        let elbow_style = self.style == PathStyle::Elbow;
        // 端の位置と、そこから出ていく向き。
        let resolve = |e: &Endpoint, t: Option<Target>, rough_self: Vec2, toward: Vec2| {
            let r = t.map(|t| t.rect);
            match (e, t) {
                (Endpoint::Attached { anchor: None, .. }, Some(t)) => {
                    let on_rect = boundary_toward(&t.rect, toward);
                    let dir = exit_dir(on_rect, r, toward);
                    // カギ線は辺の中央から出す。
                    let p = if elbow_style {
                        side_mid(&t.rect, dir)
                    } else {
                        on_rect
                    };
                    let p = match t.kind {
                        Some(k) => crate::shape::outline_point(k, &t.rect, p),
                        None => p,
                    };
                    (p, dir)
                }
                _ => (rough_self, exit_dir(rough_self, r, toward)),
            }
        };
        let (a, da) = resolve(&self.from, from_t, a0, b0);
        let (b, db) = resolve(&self.to, to_t, b0, a0);

        let path = match self.style {
            PathStyle::Straight => vec![a, b],
            PathStyle::Elbow => elbow(a, b, da, db),
            PathStyle::Curved => curve(a, b, da, db),
        };
        if path != self.path {
            self.path = path;
            true
        } else {
            false
        }
    }

    pub fn bounds(&self) -> Rect {
        let first = self.start();
        let r = self.path.iter().fold(Rect::new(first, first), |r, p| {
            Rect::new(r.min.min(*p), r.max.max(*p))
        });
        let pad = self.width * 0.5
            + if self.start_cap == Cap::Arrow || self.end_cap == Cap::Arrow {
                self.arrow_size()
            } else {
                0.0
            };
        r.expand(pad)
    }

    pub fn distance(&self, p: Vec2) -> f32 {
        match self.path.as_slice() {
            [] => f32::INFINITY,
            [a] => p.distance(*a),
            pts => pts
                .windows(2)
                .map(|w| distance_to_segment(p, w[0], w[1]))
                .fold(f32::INFINITY, f32::min),
        }
    }

    /// 自由な端だけを動かす(つながった端は相手に追従する)。
    pub fn translate(&mut self, d: Vec2) {
        for end in [&mut self.from, &mut self.to] {
            if let Endpoint::Free(p) = end {
                *p += d;
            }
        }
        if self.from.attached_to().is_none() && self.to.attached_to().is_none() {
            self.path.iter_mut().for_each(|p| *p += d);
        }
    }

    /// 経路の真ん中あたり(文字を置く位置)。
    pub fn midpoint(&self) -> Vec2 {
        let total: f32 = self.path.windows(2).map(|w| w[0].distance(w[1])).sum();
        let mut left = total * 0.5;
        for w in self.path.windows(2) {
            let len = w[0].distance(w[1]);
            if len >= left && len > 0.0 {
                return w[0].lerp(w[1], left / len);
            }
            left -= len;
        }
        self.start()
    }

    /// 線の上に出す文字の場所と書式(文字がなければ `None`)。
    pub fn label_box(&self) -> Option<crate::shape::TextBox> {
        if self.text.is_empty() {
            return None;
        }
        let style = crate::shape::TextStyle {
            font_size: 16.0,
            line_height: 22.0,
            color: Rgba8::BLACK,
            align: crate::shape::TextAlign::Center,
        };
        let w = self
            .text
            .split('\n')
            .map(|l| crate::shape::estimate_text_width(l, style.font_size))
            .fold(0.0, f32::max)
            .clamp(24.0, 240.0)
            + 8.0;
        let h = crate::shape::estimate_text_height(&self.text, w - 8.0, &style);
        let c = self.midpoint();
        let rect = Rect::new(c - Vec2::new(w, h) * 0.5, c + Vec2::new(w, h) * 0.5);
        Some(crate::shape::TextBox {
            rect,
            clip: rect.expand(2.0),
            style,
            valign: crate::shape::VAlign::Middle,
            vertical: false,
        })
    }

    /// 矢じりの 2 本の線(先端, 根元)。`at_end` が false なら始点側。
    pub fn arrow_lines(&self, at_end: bool) -> Option<[(Vec2, Vec2); 2]> {
        let pts = &self.path;
        if pts.len() < 2 {
            return None;
        }
        let (tip, prev) = if at_end {
            let tip = pts[pts.len() - 1];
            let prev = pts[..pts.len() - 1]
                .iter()
                .rev()
                .find(|p| p.distance(tip) > 1e-3)?;
            (tip, *prev)
        } else {
            let tip = pts[0];
            let prev = pts[1..].iter().find(|p| p.distance(tip) > 1e-3)?;
            (tip, *prev)
        };
        let dir = (tip - prev).normalize_or_zero();
        let n = Vec2::new(-dir.y, dir.x);
        let s = self.arrow_size();
        let base = tip - dir * s;
        Some([(tip, base + n * s * 0.5), (tip, base - n * s * 0.5)])
    }
}

/// つながる相手の形。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub rect: Rect,
    /// 図形なら種類(輪郭で線を止める)。手書き線などは `None`。
    pub kind: Option<crate::shape::ShapeKind>,
}

impl Target {
    /// 要素をつなぐ相手として見たときの形(コネクタにはつながない)。
    pub fn of(e: &crate::element::Element) -> Option<Self> {
        use crate::element::Element;
        match e {
            Element::Connector(_) => None,
            Element::Shape(s) => Some(Self {
                rect: s.bounds(),
                kind: Some(s.kind),
            }),
            e => Some(e.bounds().into()),
        }
    }
}

impl From<Rect> for Target {
    fn from(rect: Rect) -> Self {
        Self { rect, kind: None }
    }
}

/// 矩形の `dir` 側の辺の中央。
fn side_mid(r: &Rect, dir: Vec2) -> Vec2 {
    let c = r.center();
    let h = r.size() * 0.5;
    c + dir * h
}

/// 矩形の中心から `toward` へ向かう線が輪郭と交わる点。
pub fn boundary_toward(r: &Rect, toward: Vec2) -> Vec2 {
    let c = r.center();
    let h = r.size() * 0.5;
    let d = toward - c;
    if d.length_squared() < 1e-9 || h.x <= 0.0 || h.y <= 0.0 {
        return c;
    }
    let t = (h.x / d.x.abs()).min(h.y / d.y.abs());
    c + d * t.min(1.0)
}

/// 端が矩形のどの辺にあるか(出ていく向き)。
fn exit_dir(p: Vec2, r: Option<Rect>, toward: Vec2) -> Vec2 {
    let Some(r) = r else {
        let d = toward - p;
        return if d.x.abs() >= d.y.abs() {
            Vec2::new(d.x.signum(), 0.0)
        } else {
            Vec2::new(0.0, d.y.signum())
        };
    };
    let dl = (p.x - r.min.x).abs();
    let dr = (p.x - r.max.x).abs();
    let dt = (p.y - r.min.y).abs();
    let db = (p.y - r.max.y).abs();
    let m = dl.min(dr).min(dt).min(db);
    if m == dl {
        Vec2::NEG_X
    } else if m == dr {
        Vec2::X
    } else if m == dt {
        Vec2::NEG_Y
    } else {
        Vec2::Y
    }
}

fn elbow(a: Vec2, b: Vec2, da: Vec2, db: Vec2) -> Vec2s {
    let horizontal_a = da.x != 0.0;
    let horizontal_b = db.x != 0.0;
    let pts = match (horizontal_a, horizontal_b) {
        (true, true) => {
            let mx = (a.x + b.x) * 0.5;
            vec![a, Vec2::new(mx, a.y), Vec2::new(mx, b.y), b]
        }
        (false, false) => {
            let my = (a.y + b.y) * 0.5;
            vec![a, Vec2::new(a.x, my), Vec2::new(b.x, my), b]
        }
        (true, false) => vec![a, Vec2::new(b.x, a.y), b],
        (false, true) => vec![a, Vec2::new(a.x, b.y), b],
    };
    dedup(pts)
}

type Vec2s = Vec<Vec2>;

fn dedup(mut pts: Vec2s) -> Vec2s {
    pts.dedup_by(|x, y| x.distance(*y) < 1e-4);
    if pts.len() == 1 {
        pts.push(pts[0]);
    }
    pts
}

fn curve(a: Vec2, b: Vec2, da: Vec2, db: Vec2) -> Vec2s {
    let k = (a.distance(b) * 0.4).max(20.0);
    let c1 = a + da * k;
    let c2 = b + db * k;
    (0..=CURVE_SEGMENTS)
        .map(|i| {
            let t = i as f32 / CURVE_SEGMENTS as f32;
            let u = 1.0 - t;
            a * (u * u * u) + c1 * (3.0 * u * u * t) + c2 * (3.0 * u * t * t) + b * (t * t * t)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32) -> Rect {
        Rect::new(Vec2::new(x, y), Vec2::new(x + 100.0, y + 100.0))
    }

    #[test]
    fn attached_ends_land_on_boundaries() {
        let mut c = Connector::new(
            ElementId(9),
            Endpoint::Attached {
                id: ElementId(1),
                anchor: None,
            },
            Endpoint::Attached {
                id: ElementId(2),
                anchor: None,
            },
        );
        c.style = PathStyle::Straight;
        let lookup = |id: ElementId| match id.0 {
            1 => Some(rect(0.0, 0.0).into()),
            2 => Some(rect(300.0, 0.0).into()),
            _ => None,
        };
        assert!(c.route(lookup));
        assert_eq!(c.start(), Vec2::new(100.0, 50.0));
        assert_eq!(c.end(), Vec2::new(300.0, 50.0));
        assert!(!c.route(lookup), "同じ配置なら変わらない");
    }

    #[test]
    fn elbow_is_axis_aligned() {
        let mut c = Connector::new(
            ElementId(9),
            Endpoint::Attached {
                id: ElementId(1),
                anchor: None,
            },
            Endpoint::Free(Vec2::new(400.0, 300.0)),
        );
        c.route(|id| (id.0 == 1).then(|| rect(0.0, 0.0).into()));
        for w in c.path.windows(2) {
            assert!(w[0].x == w[1].x || w[0].y == w[1].y, "{:?}", c.path);
        }
        assert_eq!(c.end(), Vec2::new(400.0, 300.0));
        assert!(c.arrow_lines(true).is_some());
    }

    #[test]
    fn missing_target_keeps_last_position() {
        let mut c = Connector::new(
            ElementId(9),
            Endpoint::Free(Vec2::ZERO),
            Endpoint::Attached {
                id: ElementId(2),
                anchor: Some(Vec2::new(0.0, 0.5)),
            },
        );
        c.style = PathStyle::Straight;
        c.route(|_| Some(rect(300.0, 0.0).into()));
        assert_eq!(c.end(), Vec2::new(300.0, 50.0));
        c.route(|_| None);
        assert_eq!(c.end(), Vec2::new(300.0, 50.0));
    }

    #[test]
    fn curve_and_hit() {
        let mut c = Connector::new(
            ElementId(9),
            Endpoint::Free(Vec2::ZERO),
            Endpoint::Free(Vec2::new(200.0, 0.0)),
        );
        c.style = PathStyle::Curved;
        c.route(|_| None);
        assert_eq!(c.path.len(), CURVE_SEGMENTS + 1);
        assert!(c.distance(Vec2::new(100.0, 0.0)) < 1.0);
        assert!((c.midpoint() - Vec2::new(100.0, 0.0)).length() < 1.0);
    }
}
