use glam::{Mat4, Vec2, Vec3};

/// 無限キャンバスを覗くカメラ。
///
/// スクリーン座標は CSS ではなく物理ピクセル(左上原点、y 下向き)。
/// ワールド座標も y 下向きで、`zoom` は「ワールド 1 単位が何物理ピクセルか」。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// 画面左上に映るワールド座標。
    pub origin: Vec2,
    pub zoom: f32,
    /// ビューポートの大きさ(物理ピクセル)。
    pub viewport: Vec2,
}

impl Camera {
    pub const MIN_ZOOM: f32 = 0.02;
    pub const MAX_ZOOM: f32 = 64.0;

    pub fn new(viewport: Vec2) -> Self {
        Self {
            origin: Vec2::ZERO,
            zoom: 1.0,
            viewport,
        }
    }

    pub fn screen_to_world(&self, screen: Vec2) -> Vec2 {
        self.origin + screen / self.zoom
    }

    pub fn world_to_screen(&self, world: Vec2) -> Vec2 {
        (world - self.origin) * self.zoom
    }

    /// スクリーン上で `delta` ピクセルだけ内容を動かす(ドラッグ方向に内容が付いてくる)。
    pub fn pan_by_screen(&mut self, delta: Vec2) {
        self.origin -= delta / self.zoom;
    }

    /// `anchor`(スクリーン座標)の下にあるワールド点を固定したまま `factor` 倍ズームする。
    pub fn zoom_at(&mut self, anchor: Vec2, factor: f32) {
        let before = self.screen_to_world(anchor);
        self.zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.origin = before - anchor / self.zoom;
    }

    /// ワールド座標で見えている範囲 `(min, max)`。
    pub fn visible_world_rect(&self) -> (Vec2, Vec2) {
        (self.origin, self.screen_to_world(self.viewport))
    }

    /// 指定範囲が画面に収まるよう中央に合わせる。
    pub fn fit(&mut self, min: Vec2, max: Vec2, padding_px: f32) {
        let size = (max - min).max(Vec2::splat(1.0));
        let avail = (self.viewport - Vec2::splat(padding_px * 2.0)).max(Vec2::splat(1.0));
        self.zoom = (avail / size)
            .min_element()
            .clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        let center = (min + max) * 0.5;
        self.origin = center - self.viewport * 0.5 / self.zoom;
    }

    /// ワールド座標 → クリップ空間の行列(WGSL の列優先そのまま)。
    pub fn view_proj(&self) -> Mat4 {
        let sx = 2.0 * self.zoom / self.viewport.x.max(1.0);
        let sy = -2.0 * self.zoom / self.viewport.y.max(1.0);
        Mat4::from_translation(Vec3::new(-1.0, 1.0, 0.0))
            * Mat4::from_scale(Vec3::new(sx, sy, 1.0))
            * Mat4::from_translation(Vec3::new(-self.origin.x, -self.origin.y, 0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn roundtrip() {
        let mut c = Camera::new(Vec2::new(800.0, 600.0));
        c.origin = Vec2::new(-120.0, 40.0);
        c.zoom = 2.5;
        let w = Vec2::new(33.0, -7.0);
        assert!(approx(c.screen_to_world(c.world_to_screen(w)), w));
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut c = Camera::new(Vec2::new(800.0, 600.0));
        let anchor = Vec2::new(200.0, 150.0);
        let before = c.screen_to_world(anchor);
        c.zoom_at(anchor, 3.0);
        assert!(approx(c.screen_to_world(anchor), before));
        assert_eq!(c.zoom, 3.0);
        c.zoom_at(anchor, 1e9);
        assert_eq!(c.zoom, Camera::MAX_ZOOM);
    }

    #[test]
    fn pan_follows_pointer() {
        let mut c = Camera::new(Vec2::new(800.0, 600.0));
        c.zoom = 2.0;
        let w = Vec2::new(10.0, 10.0);
        let s0 = c.world_to_screen(w);
        c.pan_by_screen(Vec2::new(30.0, -20.0));
        assert!(approx(c.world_to_screen(w), s0 + Vec2::new(30.0, -20.0)));
    }

    #[test]
    fn view_proj_maps_corners() {
        let mut c = Camera::new(Vec2::new(800.0, 600.0));
        c.origin = Vec2::new(100.0, 50.0);
        c.zoom = 2.0;
        let m = c.view_proj();
        let tl = m.project_point3(c.screen_to_world(Vec2::ZERO).extend(0.0));
        let br = m.project_point3(c.screen_to_world(c.viewport).extend(0.0));
        assert!(approx(tl.truncate(), Vec2::new(-1.0, 1.0)));
        assert!(approx(br.truncate(), Vec2::new(1.0, -1.0)));
    }

    #[test]
    fn fit_contains_rect() {
        let mut c = Camera::new(Vec2::new(800.0, 600.0));
        c.fit(Vec2::new(0.0, 0.0), Vec2::new(4000.0, 1000.0), 0.0);
        let (min, max) = c.visible_world_rect();
        assert!(min.x <= 0.01 && max.x >= 3999.99);
        assert!(min.y <= 0.0 && max.y >= 1000.0);
    }
}
