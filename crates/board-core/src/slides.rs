//! スライド表示: ボード上のフレームを 1 枚ずつ全画面で見せる。
//!
//! - 並び順は「読む順」(上の段から、同じ段は左から)。
//!   ただし全部のフレーム名が数字で始まっていれば(「1. はじめに」など)その番号順。
//! - スライドの切り替えはカメラを補間して動かす([`tween`])。

use glam::Vec2;

use crate::board::Board;
use crate::camera::Camera;
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::ShapeKind;

/// スライド 1 枚(= フレーム 1 つ)。
#[derive(Clone, Debug, PartialEq)]
pub struct Slide {
    pub id: ElementId,
    pub title: String,
    pub rect: Rect,
}

/// ボード上のフレームをスライドの順に並べる。
pub fn slides(board: &Board) -> Vec<Slide> {
    let frames: Vec<Slide> = board
        .elements()
        .iter()
        .filter_map(|e| match e {
            Element::Shape(s) if s.kind == ShapeKind::Frame => Some(Slide {
                id: s.id,
                title: s.text.trim().to_string(),
                rect: s.bounds(),
            }),
            _ => None,
        })
        .filter(|s| s.rect.size().min_element() > 1.0)
        .collect();
    order(frames)
}

fn leading_number(title: &str) -> Option<u64> {
    let digits: String = title
        .chars()
        .map(|c| match c {
            // 全角数字も数える。
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            _ => c,
        })
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn order(mut frames: Vec<Slide>) -> Vec<Slide> {
    let key = |s: &Slide| (s.rect.min.y, s.rect.min.x, s.id);
    if !frames.is_empty() && frames.iter().all(|s| leading_number(&s.title).is_some()) {
        frames.sort_by(|a, b| {
            leading_number(&a.title)
                .cmp(&leading_number(&b.title))
                .then(
                    key(a)
                        .partial_cmp(&key(b))
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        return frames;
    }
    // 上端の順に見て、前の段の 1 枚目の縦の中央より上から始まるものは同じ段とみなす。
    frames.sort_by(|a, b| {
        key(a)
            .partial_cmp(&key(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out = Vec::with_capacity(frames.len());
    let mut row: Vec<Slide> = Vec::new();
    for f in frames {
        if let Some(first) = row.first()
            && f.rect.min.y >= first.rect.center().y
        {
            flush_row(&mut row, &mut out);
        }
        row.push(f);
    }
    flush_row(&mut row, &mut out);
    out
}

fn flush_row(row: &mut Vec<Slide>, out: &mut Vec<Slide>) {
    row.sort_by(|a, b| {
        (a.rect.min.x, a.rect.min.y)
            .partial_cmp(&(b.rect.min.x, b.rect.min.y))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.append(row);
}

/// スライドを映すカメラ(余白は短い辺の `margin` 割合)。
pub fn camera_for(slide: &Rect, viewport: Vec2, margin: f32) -> Camera {
    let mut cam = Camera::new(viewport);
    let pad = viewport.min_element() * margin;
    cam.fit(slide.min, slide.max, pad);
    cam
}

/// ゆっくり始まりゆっくり終わる(0..=1)。
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// 2 つのカメラの間。ズームは対数で補間するので、拡大・縮小の速さが一定に見える。
/// `t` はイージング前の進み具合(0..=1)。
pub fn tween(from: &Camera, to: &Camera, t: f32) -> Camera {
    let e = ease_in_out(t);
    let viewport = to.viewport;
    let center = |c: &Camera| c.origin + c.viewport * 0.5 / c.zoom;
    let c = center(from).lerp(center(to), e);
    let zoom = (from.zoom.ln() + (to.zoom.ln() - from.zoom.ln()) * e)
        .exp()
        .clamp(Camera::MIN_ZOOM, Camera::MAX_ZOOM);
    Camera {
        origin: c - viewport * 0.5 / zoom,
        zoom,
        viewport,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::Shape;

    fn frame(board: &mut Board, x: f32, y: f32, title: &str) -> ElementId {
        let id = board.alloc_id();
        let mut f = Shape::frame(id, Vec2::new(x, y), Vec2::new(400.0, 300.0));
        f.text = title.into();
        board.push(f);
        id
    }

    fn titles(board: &Board) -> Vec<String> {
        slides(board).into_iter().map(|s| s.title).collect()
    }

    #[test]
    fn reading_order() {
        let mut b = Board::local();
        frame(&mut b, 500.0, 20.0, "B");
        frame(&mut b, 0.0, 400.0, "C");
        frame(&mut b, 0.0, 0.0, "A");
        frame(&mut b, 520.0, 380.0, "D");
        // フレームでない図形は入らない。
        let id = b.alloc_id();
        b.push(Shape::rect(
            id,
            Vec2::ZERO,
            Vec2::splat(50.0),
            crate::Rgba8([0, 0, 0, 255]),
        ));
        assert_eq!(titles(&b), ["A", "B", "C", "D"]);
    }

    #[test]
    fn numbered_titles_win() {
        let mut b = Board::local();
        frame(&mut b, 0.0, 0.0, "2. 課題");
        frame(&mut b, 500.0, 0.0, "1. はじめに");
        frame(&mut b, 0.0, 400.0, "１０ まとめ");
        frame(&mut b, 500.0, 400.0, "3 案");
        assert_eq!(
            titles(&b),
            ["1. はじめに", "2. 課題", "3 案", "１０ まとめ"]
        );
        // 1 つでも番号がなければ読む順。
        frame(&mut b, 1000.0, 0.0, "付録");
        assert_eq!(titles(&b)[0], "2. 課題");
    }

    #[test]
    fn camera_fits_slide() {
        let r = Rect::new(Vec2::new(100.0, 100.0), Vec2::new(500.0, 400.0));
        let cam = camera_for(&r, Vec2::new(1600.0, 900.0), 0.05);
        let (min, max) = cam.visible_world_rect();
        assert!(min.x <= 100.0 && min.y <= 100.0 && max.x >= 500.0 && max.y >= 400.0);
    }

    #[test]
    fn tween_ends() {
        let vp = Vec2::new(800.0, 600.0);
        let a = camera_for(&Rect::new(Vec2::ZERO, Vec2::splat(100.0)), vp, 0.0);
        let b = camera_for(
            &Rect::new(Vec2::splat(1000.0), Vec2::splat(3000.0)),
            vp,
            0.0,
        );
        let s = tween(&a, &b, 0.0);
        assert!((s.zoom - a.zoom).abs() < 1e-4 && (s.origin - a.origin).length() < 1e-2);
        let e = tween(&a, &b, 1.0);
        assert!((e.zoom - b.zoom).abs() < 1e-4 && (e.origin - b.origin).length() < 1e-2);
        let m = tween(&a, &b, 0.5);
        assert!(m.zoom < a.zoom && m.zoom > b.zoom);
        assert_eq!(ease_in_out(0.5), 0.5);
    }
}
