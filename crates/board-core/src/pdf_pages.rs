//! PDF のページをボードに並べる(取り込みの描画は Worker 側、`board-files`)。
//!
//! 数百ページの PDF でもすぐ形が見えるよう、先にページの大きさの枠([`place_frames`])を
//! 1 回の操作として置き、描けたページから順に画像に差し替える([`fill_page`])。
//! 差し替えは Undo の単位を増やさないので、Undo 1 回で PDF ごと取り消せ、Redo で画像が戻る。

use glam::Vec2;

use crate::board::Board;
use crate::color::Rgba8;
use crate::element::ElementId;
use crate::shape::{ImageData, Shape};

/// ページの長辺(ワールド単位)。
pub const PAGE_WORLD: f32 = 600.0;
/// ページの間隔(ワールド単位)。
pub const PAGE_GAP: f32 = 40.0;
/// 描き終わる前のページの枠の色。
const PAGE_FILL: Rgba8 = Rgba8::hex(0xf1f3f5);
const PAGE_STROKE: Rgba8 = Rgba8::hex(0xced4da);
const PAGE_NUMBER: Rgba8 = Rgba8::hex(0x868e96);

/// PDF のページを並べる位置(左上)と大きさ。`at` を中心に、ページが多ければ何列かに折り返す。
pub fn layout(sizes: &[(f32, f32)], at: Vec2) -> Vec<(Vec2, Vec2)> {
    let n = sizes.len();
    if n == 0 {
        return Vec::new();
    }
    // 10 ページまでは横 1 列(これまでどおり)。それより多ければ、横長の長方形に近い格子にする。
    let cols = if n <= 10 {
        n
    } else {
        ((n as f32).sqrt() * 1.4).ceil() as usize
    };
    let sized: Vec<Vec2> = sizes
        .iter()
        .map(|&(w, h)| {
            let s = Vec2::new(w.max(1.0), h.max(1.0));
            s * (PAGE_WORLD / s.max_element())
        })
        .collect();
    let rows: Vec<&[Vec2]> = sized.chunks(cols).collect();
    let row_w = |r: &[Vec2]| {
        r.iter().map(|s| s.x).sum::<f32>() + PAGE_GAP * r.len().saturating_sub(1) as f32
    };
    let row_h = |r: &[Vec2]| r.iter().map(|s| s.y).fold(0.0, f32::max);
    let width = rows.iter().map(|r| row_w(r)).fold(0.0, f32::max);
    let height =
        rows.iter().map(|r| row_h(r)).sum::<f32>() + PAGE_GAP * rows.len().saturating_sub(1) as f32;
    let mut out = Vec::with_capacity(n);
    let mut y = at.y - height * 0.5;
    for r in rows {
        let mut x = at.x - width * 0.5;
        let h = row_h(r);
        for &size in r {
            // 行の中では上下中央にそろえる。
            out.push((Vec2::new(x, y + (h - size.y) * 0.5), size));
            x += size.x + PAGE_GAP;
        }
        y += h + PAGE_GAP;
    }
    out
}

/// ページの枠(ページ番号入りの灰色の四角形)を並べる。ページ順の ID を返す。
pub fn place_frames(board: &mut Board, sizes: &[(f32, f32)], at: Vec2) -> Vec<ElementId> {
    let mut ids = Vec::new();
    let mut shapes = Vec::new();
    for (i, (pos, size)) in layout(sizes, at).into_iter().enumerate() {
        let id = board.alloc_id();
        let mut frame = Shape::rect(id, pos, size, PAGE_FILL);
        frame.stroke = PAGE_STROKE;
        frame.corner_radius = 0.0;
        frame.text = (i + 1).to_string();
        frame.font_size = 64.0;
        frame.text_color = PAGE_NUMBER;
        shapes.push(frame.into());
        ids.push(id);
    }
    board.push_many(shapes);
    ids
}

/// ページの枠を描けたページの画像に差し替える(枠を動かしていればその位置・大きさで)。
/// 枠が消されていたり、もう画像になっていれば何もせず `false`。
pub fn fill_page(board: &mut Board, id: ElementId, image: ImageData) -> bool {
    let Some(frame) = board.get(id).and_then(|e| e.as_shape()) else {
        return false;
    };
    if frame.image.is_some() {
        return false;
    }
    let page = Shape::image(id, frame.pos, frame.size, image);
    board.write_untracked(vec![(page.into(), None)], &[]);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::ShapeKind;

    #[test]
    fn few_pages_line_up_and_many_pages_wrap_into_a_grid() {
        let row = layout(&[(400.0, 200.0), (200.0, 400.0)], Vec2::ZERO);
        assert_eq!(row[0].1, Vec2::new(PAGE_WORLD, PAGE_WORLD * 0.5));
        assert_eq!(row[1].0.x, row[0].0.x + PAGE_WORLD + PAGE_GAP);
        // 全体の中心が `at` になる。
        let right = row[1].0.x + row[1].1.x;
        assert!((row[0].0.x + right).abs() < 1e-3);

        let many = layout(&vec![(210.0, 297.0); 300], Vec2::new(100.0, 50.0));
        assert_eq!(many.len(), 300);
        let cols = many.iter().filter(|(p, _)| p.y == many[0].0.y).count();
        assert_eq!(cols, 25);
        // 重ならない。
        for w in many.windows(2) {
            let (a, b) = (w[0], w[1]);
            assert!(b.0.x >= a.0.x + a.1.x || b.0.y >= a.0.y + a.1.y);
        }
        assert!(layout(&[], Vec2::ZERO).is_empty());
    }

    #[test]
    fn pages_fill_in_without_extra_undo_steps() {
        let mut board = Board::with_client_id(1);
        board.end_gesture();
        let ids = place_frames(&mut board, &[(200.0, 300.0); 3], Vec2::ZERO);
        board.end_gesture();
        assert_eq!(ids.len(), 3);
        let jpeg = ImageData::new(vec![1, 2, 3]);
        assert!(fill_page(&mut board, ids[1], jpeg.clone()));
        assert!(!fill_page(&mut board, ids[1], jpeg.clone()));
        let kind = |b: &Board, id| b.get(id).and_then(|e| e.as_shape()).map(|s| s.kind);
        assert_eq!(kind(&board, ids[1]), Some(ShapeKind::Image));
        assert_eq!(kind(&board, ids[0]), Some(ShapeKind::Rect));
        // Undo 1 回で枠ごと消え、Redo で描けたページも戻る。
        assert!(board.undo());
        assert!(ids.iter().all(|id| board.get(*id).is_none()));
        assert!(!board.can_undo());
        assert!(!fill_page(&mut board, ids[2], jpeg));
        assert!(board.redo());
        assert_eq!(kind(&board, ids[1]), Some(ShapeKind::Image));
        assert_eq!(kind(&board, ids[2]), Some(ShapeKind::Rect));
    }
}
