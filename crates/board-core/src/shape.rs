use glam::Vec2;

use crate::color::Rgba8;

use crate::element::ElementId;
use crate::geom::Rect;

/// 図形の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShapeKind {
    Rect,
    Ellipse,
    Sticky,
}

/// キャンバス上の 1 要素。座標はワールド座標(px 相当、y 下向き)。
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub id: ElementId,
    pub kind: ShapeKind,
    /// 左上の位置。
    pub pos: Vec2,
    pub size: Vec2,
    pub fill: Rgba8,
    pub stroke: Rgba8,
    pub stroke_width: f32,
    pub corner_radius: f32,
}

impl Shape {
    pub fn rect(id: ElementId, pos: Vec2, size: Vec2, fill: Rgba8) -> Self {
        Self {
            id,
            kind: ShapeKind::Rect,
            pos,
            size,
            fill,
            stroke: Rgba8::BLACK,
            stroke_width: 2.0,
            corner_radius: 8.0,
        }
    }

    pub fn ellipse(id: ElementId, pos: Vec2, size: Vec2, fill: Rgba8) -> Self {
        Self {
            kind: ShapeKind::Ellipse,
            corner_radius: 0.0,
            ..Self::rect(id, pos, size, fill)
        }
    }

    pub fn sticky(id: ElementId, pos: Vec2, fill: Rgba8) -> Self {
        Self {
            id,
            kind: ShapeKind::Sticky,
            pos,
            size: Vec2::splat(200.0),
            fill,
            stroke: Rgba8::TRANSPARENT,
            stroke_width: 0.0,
            corner_radius: 4.0,
        }
    }

    pub fn min(&self) -> Vec2 {
        self.pos
    }

    pub fn max(&self) -> Vec2 {
        self.pos + self.size
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(self.min(), self.max())
    }

    pub fn set_bounds(&mut self, r: Rect) {
        self.pos = r.min;
        self.size = r.size();
    }

    /// ワールド座標の点が要素の内側か。
    pub fn contains(&self, p: Vec2) -> bool {
        match self.kind {
            ShapeKind::Rect | ShapeKind::Sticky => {
                p.cmpge(self.min()).all() && p.cmple(self.max()).all()
            }
            ShapeKind::Ellipse => {
                let half = self.size * 0.5;
                if half.x <= 0.0 || half.y <= 0.0 {
                    return false;
                }
                let d = (p - (self.pos + half)) / half;
                d.length_squared() <= 1.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_rect_and_ellipse() {
        let r = Shape::rect(
            ElementId(1),
            Vec2::ZERO,
            Vec2::new(100.0, 50.0),
            Rgba8::WHITE,
        );
        assert!(r.contains(Vec2::new(99.0, 49.0)));
        assert!(!r.contains(Vec2::new(101.0, 10.0)));

        let e = Shape::ellipse(
            ElementId(2),
            Vec2::ZERO,
            Vec2::new(100.0, 50.0),
            Rgba8::WHITE,
        );
        assert!(e.contains(Vec2::new(50.0, 25.0)));
        // 外接矩形の角は楕円の外。
        assert!(!e.contains(Vec2::new(2.0, 2.0)));
    }
}
