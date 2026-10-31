use glam::Vec2;

use crate::color::Rgba8;
use crate::geom::{Rect, distance_to_segment};
use crate::shape::{Shape, ShapeKind};

pub use crate::connector::{Cap, Connector, Endpoint, PathStyle};

/// ボード内で一意な要素 ID。共同編集では CRDT 側のキーになる。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementId(pub u64);

/// どの種類の要素にもある属性。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    /// 所属するグループ(0 ならなし)。同じグループの要素はまとめて選択・移動する。
    pub group: u64,
    /// ロック中は選択・移動・削除できない(ロック解除はメニューから)。
    pub locked: bool,
    /// クリックで開くリンク(空ならなし)。
    pub link: String,
}

/// 手書きの線。点列をそのまま持ち、GPU では線分ごとのカプセルとして描く。
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub id: ElementId,
    pub points: Vec<Vec2>,
    pub color: Rgba8,
    pub width: f32,
    /// 蛍光ペン(半透明で、文字の下に引く想定)。
    pub highlighter: bool,
    pub meta: Meta,
}

impl Stroke {
    pub fn new(id: ElementId, start: Vec2, color: Rgba8, width: f32) -> Self {
        Self {
            id,
            points: vec![start],
            color,
            width,
            highlighter: false,
            meta: Meta::default(),
        }
    }

    /// 前の点から `min_dist` 以上離れていれば点を足す(点の数を抑える)。
    pub fn push_point(&mut self, p: Vec2, min_dist: f32) -> bool {
        match self.points.last() {
            Some(last) if last.distance(p) < min_dist => false,
            _ => {
                self.points.push(p);
                true
            }
        }
    }

    /// 線分の数(点 1 つなら長さ 0 の線分 1 本)。
    pub fn segment_count(&self) -> usize {
        self.points
            .len()
            .saturating_sub(1)
            .max(usize::from(!self.points.is_empty()))
    }

    pub fn bounds(&self) -> Rect {
        let first = self.points.first().copied().unwrap_or(Vec2::ZERO);
        let r = self.points.iter().fold(Rect::new(first, first), |r, p| {
            Rect::new(r.min.min(*p), r.max.max(*p))
        });
        r.expand(self.width * 0.5)
    }

    pub fn distance(&self, p: Vec2) -> f32 {
        match self.points.as_slice() {
            [] => f32::INFINITY,
            [a] => p.distance(*a),
            pts => pts
                .windows(2)
                .map(|w| distance_to_segment(p, w[0], w[1]))
                .fold(f32::INFINITY, f32::min),
        }
    }
}

/// ボード上の 1 要素。
#[derive(Clone, Debug, PartialEq)]
pub enum Element {
    Shape(Shape),
    Stroke(Stroke),
    Connector(Connector),
}

impl Element {
    pub fn id(&self) -> ElementId {
        match self {
            Element::Shape(s) => s.id,
            Element::Stroke(s) => s.id,
            Element::Connector(c) => c.id,
        }
    }

    pub fn meta(&self) -> &Meta {
        match self {
            Element::Shape(s) => &s.meta,
            Element::Stroke(s) => &s.meta,
            Element::Connector(c) => &c.meta,
        }
    }

    pub fn meta_mut(&mut self) -> &mut Meta {
        match self {
            Element::Shape(s) => &mut s.meta,
            Element::Stroke(s) => &mut s.meta,
            Element::Connector(c) => &mut c.meta,
        }
    }

    pub fn is_locked(&self) -> bool {
        self.meta().locked
    }

    /// 見た目の外接矩形。
    pub fn bounds(&self) -> Rect {
        match self {
            Element::Shape(s) => s.bounds(),
            Element::Stroke(s) => s.bounds(),
            Element::Connector(c) => c.bounds(),
        }
    }

    /// 点がこの要素に当たるか。`tolerance` はワールド単位の許容距離。
    pub fn hit(&self, p: Vec2, tolerance: f32) -> bool {
        match self {
            Element::Shape(s) if tolerance <= 0.0 => s.contains(p),
            Element::Shape(s) => {
                // 縁の少し外側でも掴めるようにする(細い図形の操作性のため)。
                let mut grown = s.clone();
                grown.set_bounds(s.bounds().expand(tolerance));
                grown.contains(p)
            }
            Element::Stroke(s) => s.distance(p) <= s.width * 0.5 + tolerance,
            Element::Connector(c) => c.distance(p) <= c.width * 0.5 + tolerance.max(4.0),
        }
    }

    pub fn translate(&mut self, d: Vec2) {
        match self {
            Element::Shape(s) => s.pos += d,
            Element::Stroke(s) => s.points.iter_mut().for_each(|p| *p += d),
            Element::Connector(c) => c.translate(d),
        }
    }

    /// 外接矩形 `from` を `to` に変形するように拡大縮小する。
    pub fn remap(&mut self, from: &Rect, to: &Rect) {
        match self {
            Element::Shape(s) => {
                let a = Rect::map_point(from, to, s.min());
                let b = Rect::map_point(from, to, s.max());
                s.set_bounds(Rect::from_points(a, b));
            }
            Element::Stroke(s) => {
                // 線の太さの分だけ内側の矩形で写すと、外接矩形がちょうど `to` に合う。
                let h = s.width * 0.5;
                let inner_from = from.expand(-h);
                let inner_to = Rect::from_points(to.min + h, to.max - h);
                s.points
                    .iter_mut()
                    .for_each(|p| *p = Rect::map_point(&inner_from, &inner_to, *p));
            }
            Element::Connector(c) => {
                for end in [&mut c.from, &mut c.to] {
                    if let Endpoint::Free(p) = end {
                        *p = Rect::map_point(from, to, *p);
                    }
                }
            }
        }
    }

    pub fn set_color(&mut self, c: Rgba8) {
        match self {
            // 文字だけの要素は文字色を変える。
            Element::Shape(s) if s.kind == ShapeKind::Text => s.text_color = c,
            Element::Shape(s) => s.fill = c,
            Element::Stroke(s) => {
                let a = s.color.0[3];
                s.color = Rgba8([c.0[0], c.0[1], c.0[2], a]);
            }
            Element::Connector(k) => k.color = c,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Element::Shape(s) => &s.text,
            Element::Stroke(_) => "",
            Element::Connector(c) => &c.text,
        }
    }

    /// 文字を置く場所(文字を持てない要素は `None`)。
    pub fn text_box(&self) -> Option<crate::shape::TextBox> {
        match self {
            Element::Shape(s) => Some(s.text_box()),
            Element::Connector(c) => c.label_box().or_else(|| {
                // 空のラベルを編集するとき用の場所。
                let mut tmp = c.clone();
                tmp.text = "  ".into();
                tmp.label_box()
            }),
            Element::Stroke(_) => None,
        }
    }

    /// 文字を設定する(文字を持てない要素では何もしない)。
    pub fn set_text(&mut self, text: String) {
        match self {
            Element::Shape(s) => {
                s.text = text;
                s.fit_text_height();
            }
            Element::Connector(c) => c.text = text,
            Element::Stroke(_) => {}
        }
    }

    pub fn as_shape(&self) -> Option<&Shape> {
        match self {
            Element::Shape(s) => Some(s),
            _ => None,
        }
    }
}

impl From<Shape> for Element {
    fn from(s: Shape) -> Self {
        Element::Shape(s)
    }
}

impl From<Connector> for Element {
    fn from(c: Connector) -> Self {
        Element::Connector(c)
    }
}

impl From<Stroke> for Element {
    fn from(s: Stroke) -> Self {
        Element::Stroke(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_hit_and_bounds() {
        let mut s = Stroke::new(ElementId(1), Vec2::ZERO, Rgba8::BLACK, 4.0);
        assert!(!s.push_point(Vec2::new(0.5, 0.0), 1.0));
        assert!(s.push_point(Vec2::new(10.0, 0.0), 1.0));
        assert_eq!(s.segment_count(), 1);
        let e = Element::Stroke(s);
        assert!(e.hit(Vec2::new(5.0, 1.5), 0.0));
        assert!(!e.hit(Vec2::new(5.0, 5.0), 0.0));
        assert!(e.hit(Vec2::new(5.0, 5.0), 4.0));
        let b = e.bounds();
        assert_eq!(b.min, Vec2::new(-2.0, -2.0));
        assert_eq!(b.max, Vec2::new(12.0, 2.0));
    }

    #[test]
    fn remap_stroke_fits_target() {
        let mut s = Stroke::new(ElementId(1), Vec2::ZERO, Rgba8::BLACK, 4.0);
        s.push_point(Vec2::new(10.0, 10.0), 0.0);
        let mut e = Element::Stroke(s);
        let from = e.bounds();
        let to = Rect::new(Vec2::ZERO, Vec2::splat(100.0));
        e.remap(&from, &to);
        let b = e.bounds();
        assert!((b.min - to.min).length() < 1e-4 && (b.max - to.max).length() < 1e-4);
    }
}
