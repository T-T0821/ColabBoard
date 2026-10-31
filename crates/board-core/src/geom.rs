use glam::Vec2;

/// 軸に平行な矩形(ワールド座標)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    pub fn new(min: Vec2, max: Vec2) -> Self {
        Self { min, max }
    }

    /// 2 点から(順不同で)作る。
    pub fn from_points(a: Vec2, b: Vec2) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
        }
    }

    pub fn size(&self) -> Vec2 {
        self.max - self.min
    }

    pub fn center(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect::new(self.min.min(o.min), self.max.max(o.max))
    }

    pub fn expand(&self, d: f32) -> Rect {
        Rect::new(self.min - Vec2::splat(d), self.max + Vec2::splat(d))
    }

    pub fn contains(&self, p: Vec2) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }

    pub fn contains_rect(&self, o: &Rect) -> bool {
        self.contains(o.min) && self.contains(o.max)
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.min.cmple(o.max).all() && o.min.cmple(self.max).all()
    }

    /// `from` の中の点 `p` を、同じ相対位置の `to` の中の点に写す。
    pub fn map_point(from: &Rect, to: &Rect, p: Vec2) -> Vec2 {
        let s = from.size();
        let t = Vec2::new(
            if s.x.abs() > f32::EPSILON {
                (p.x - from.min.x) / s.x
            } else {
                0.0
            },
            if s.y.abs() > f32::EPSILON {
                (p.y - from.min.y) / s.y
            } else {
                0.0
            },
        );
        to.min + t * to.size()
    }
}

/// 点と線分の距離。
pub fn distance_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 <= f32::EPSILON {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_ops() {
        let r = Rect::from_points(Vec2::new(10.0, 0.0), Vec2::new(0.0, 10.0));
        assert_eq!(r.min, Vec2::ZERO);
        assert!(r.contains(Vec2::splat(5.0)));
        assert!(r.intersects(&Rect::new(Vec2::splat(9.0), Vec2::splat(20.0))));
        assert!(!r.intersects(&Rect::new(Vec2::splat(11.0), Vec2::splat(20.0))));
        let to = Rect::new(Vec2::splat(100.0), Vec2::splat(120.0));
        assert_eq!(
            Rect::map_point(&r, &to, Vec2::splat(5.0)),
            Vec2::splat(110.0)
        );
    }

    #[test]
    fn segment_distance() {
        let d = distance_to_segment(Vec2::new(5.0, 3.0), Vec2::ZERO, Vec2::new(10.0, 0.0));
        assert!((d - 3.0).abs() < 1e-6);
        let d = distance_to_segment(Vec2::new(-4.0, 3.0), Vec2::ZERO, Vec2::new(10.0, 0.0));
        assert!((d - 5.0).abs() < 1e-6);
    }
}
