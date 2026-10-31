//! パン/ズーム操作の解釈。ブラウザのイベントから切り離してテストできるようにしている。
//!
//! - ポインタ 1 本のドラッグ: パン
//! - ポインタ 2 本(タッチ): ピンチズーム + パン
//! - ホイール/2 本指スクロール: パン、Ctrl/⌘ 付き(トラックパッドのピンチも含む): ズーム

use glam::Vec2;

use crate::camera::Camera;

#[derive(Clone, Debug, Default)]
pub struct Navigator {
    pointers: Vec<(i32, Vec2)>,
    /// ピンチ中の (2 点間距離, 中点)。
    pinch: Option<(f32, Vec2)>,
}

impl Navigator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_dragging(&self) -> bool {
        !self.pointers.is_empty()
    }

    pub fn pointer_down(&mut self, id: i32, pos: Vec2) {
        self.pointers.retain(|(p, _)| *p != id);
        self.pointers.push((id, pos));
        self.pinch = self.pinch_state();
    }

    /// カメラが動いたら `true`。
    pub fn pointer_move(&mut self, camera: &mut Camera, id: i32, pos: Vec2) -> bool {
        let Some(slot) = self.pointers.iter_mut().find(|(p, _)| *p == id) else {
            return false;
        };
        let prev = slot.1;
        slot.1 = pos;

        match self.pointers.len() {
            1 => {
                let delta = pos - prev;
                if delta == Vec2::ZERO {
                    return false;
                }
                camera.pan_by_screen(delta);
                true
            }
            _ => {
                let (Some((d0, m0)), Some((d1, m1))) = (self.pinch, self.pinch_state()) else {
                    return false;
                };
                self.pinch = Some((d1, m1));
                camera.pan_by_screen(m1 - m0);
                if d0 > 1.0 {
                    camera.zoom_at(m1, d1 / d0);
                }
                true
            }
        }
    }

    pub fn pointer_up(&mut self, id: i32) {
        self.pointers.retain(|(p, _)| *p != id);
        self.pinch = self.pinch_state();
    }

    /// ホイール。`delta` はピクセル単位に正規化済みの値、`pos` はカーソル位置。
    pub fn wheel(&mut self, camera: &mut Camera, delta: Vec2, pos: Vec2, zoom_modifier: bool) {
        if zoom_modifier {
            // トラックパッドのピンチは小さな値、マウスホイールは大きな値が来る。どちらも滑らかに。
            let factor = (-delta.y * 0.01).clamp(-0.5, 0.5).exp();
            camera.zoom_at(pos, factor);
        } else {
            camera.pan_by_screen(-delta);
        }
    }

    fn pinch_state(&self) -> Option<(f32, Vec2)> {
        match self.pointers.as_slice() {
            [(_, a), (_, b), ..] => Some((a.distance(*b), (*a + *b) * 0.5)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Camera {
        Camera::new(Vec2::new(800.0, 600.0))
    }

    #[test]
    fn single_pointer_pans() {
        let mut c = cam();
        let mut n = Navigator::new();
        n.pointer_down(1, Vec2::new(100.0, 100.0));
        assert!(n.pointer_move(&mut c, 1, Vec2::new(130.0, 90.0)));
        assert_eq!(c.origin, Vec2::new(-30.0, 10.0));
        n.pointer_up(1);
        assert!(!n.is_dragging());
        assert!(!n.pointer_move(&mut c, 1, Vec2::new(0.0, 0.0)));
    }

    #[test]
    fn pinch_zooms_around_midpoint() {
        let mut c = cam();
        let mut n = Navigator::new();
        n.pointer_down(1, Vec2::new(300.0, 300.0));
        n.pointer_down(2, Vec2::new(500.0, 300.0));
        let mid_world = c.screen_to_world(Vec2::new(400.0, 300.0));
        // 指を左右に広げる(距離 200 → 400)。
        n.pointer_move(&mut c, 1, Vec2::new(200.0, 300.0));
        n.pointer_move(&mut c, 2, Vec2::new(600.0, 300.0));
        assert!((c.zoom - 2.0).abs() < 1e-4);
        assert!((c.screen_to_world(Vec2::new(400.0, 300.0)) - mid_world).length() < 1e-3);
    }

    #[test]
    fn wheel_pans_and_ctrl_wheel_zooms() {
        let mut c = cam();
        let mut n = Navigator::new();
        n.wheel(&mut c, Vec2::new(0.0, 50.0), Vec2::ZERO, false);
        assert_eq!(c.origin, Vec2::new(0.0, 50.0));
        n.wheel(
            &mut c,
            Vec2::new(0.0, -100.0),
            Vec2::new(400.0, 300.0),
            true,
        );
        assert!(c.zoom > 1.0);
    }
}
