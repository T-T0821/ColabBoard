use glam::Vec2;

use crate::color::Rgba8;
use crate::geom::{Rect, distance_to_segment};
use crate::shape::Shape;

/// ボード内で一意な要素 ID。共同編集では CRDT 側のキーになる。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementId(pub u64);

/// 手書きの線。点列をそのまま持ち、GPU では線分ごとのカプセルとして描く。
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub id: ElementId,
    pub points: Vec<Vec2>,
    pub color: Rgba8,
    pub width: f32,
}

impl Stroke {
    pub fn new(id: ElementId, start: Vec2, color: Rgba8, width: f32) -> Self {
        Self {
            id,
            points: vec![start],
            color,
            width,
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
}

impl Element {
    pub fn id(&self) -> ElementId {
        match self {
            Element::Shape(s) => s.id,
            Element::Stroke(s) => s.id,
        }
    }

    /// 見た目の外接矩形。
    pub fn bounds(&self) -> Rect {
        match self {
            Element::Shape(s) => s.bounds(),
            Element::Stroke(s) => s.bounds(),
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
        }
    }

    pub fn translate(&mut self, d: Vec2) {
        match self {
            Element::Shape(s) => s.pos += d,
            Element::Stroke(s) => s.points.iter_mut().for_each(|p| *p += d),
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
        }
    }

    pub fn set_color(&mut self, c: Rgba8) {
        match self {
            Element::Shape(s) => s.fill = c,
            Element::Stroke(s) => s.color = c,
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
