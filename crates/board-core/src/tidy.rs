//! 付箋の自動整理: 格子に並べる、色ごとにまとめる、グループごとに並べる。
//!
//! 位置の計算(純粋な関数)と、ボードへの反映(`arrange`)に分けてある。
//! 反映は普通の編集と同じく Undo できる。

use std::collections::HashMap;

use glam::Vec2;

use crate::board::Board;
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::ShapeKind;

/// 付箋どうしの間隔(ワールド単位)。
pub const GAP: f32 = 24.0;
/// グループどうしの間隔。
pub const GROUP_GAP: f32 = 96.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    /// 今の並び(上から下、左から右)のまま格子にそろえる。
    Grid,
    /// 色ごとにまとめて、色ごとに格子にする。
    ByColor,
}

/// 並べる対象: 選択中の付箋が 2 枚以上ならそれ、そうでなければボード上の全付箋。
pub fn targets(board: &Board, selection: &[ElementId]) -> Vec<ElementId> {
    let is_sticky = |e: &Element| e.as_shape().is_some_and(|s| s.kind == ShapeKind::Sticky);
    let selected: Vec<ElementId> = selection
        .iter()
        .copied()
        .filter(|id| board.get(*id).is_some_and(is_sticky))
        .collect();
    if selected.len() >= 2 {
        return selected;
    }
    board
        .elements()
        .iter()
        .filter(|e| is_sticky(e))
        .map(Element::id)
        .collect()
}

/// 要素を読む順(上の行から、行の中は左から)に並べ替える。
/// 行の判定は高さの半分までのずれを同じ行とみなす。
pub fn reading_order(items: &mut [(ElementId, Rect)]) {
    let row = items
        .iter()
        .map(|(_, r)| r.size().y)
        .fold(0.0f32, f32::max)
        .max(1.0)
        * 0.5;
    items.sort_by(|(ia, a), (ib, b)| {
        let ra = (a.min.y / row).round();
        let rb = (b.min.y / row).round();
        ra.total_cmp(&rb)
            .then(a.min.x.total_cmp(&b.min.x))
            .then(ia.cmp(ib))
    });
}

/// `items` を `origin` を左上にして格子に並べる。列数は正方形に近くなるように選ぶ。
/// 戻り値は各要素の新しい左上と、全体を囲む矩形。
pub fn grid(items: &[(ElementId, Vec2)], origin: Vec2, gap: f32) -> (Vec<(ElementId, Vec2)>, Rect) {
    if items.is_empty() {
        return (Vec::new(), Rect::new(origin, origin));
    }
    let cell = items.iter().fold(Vec2::ZERO, |m, (_, size)| m.max(*size));
    let cols = (items.len() as f32).sqrt().ceil().max(1.0) as usize;
    let rows = items.len().div_ceil(cols);
    let step = cell + Vec2::splat(gap);
    let out = items
        .iter()
        .enumerate()
        .map(|(i, (id, _))| {
            let c = Vec2::new((i % cols) as f32, (i / cols) as f32);
            (*id, origin + c * step)
        })
        .collect();
    let size = Vec2::new(cols as f32, rows as f32) * step - Vec2::splat(gap);
    (out, Rect::new(origin, origin + size))
}

/// グループごとに格子を作り、グループを左から右へ並べる。
/// グループの上に見出しを置く余白 `header` を空ける。戻り値の矩形は見出しを含まない格子部分。
pub fn groups(
    groups: &[Vec<(ElementId, Vec2)>],
    origin: Vec2,
    header: f32,
) -> (Vec<(ElementId, Vec2)>, Vec<Rect>) {
    let mut out = Vec::new();
    let mut rects = Vec::new();
    let mut x = origin.x;
    for g in groups.iter().filter(|g| !g.is_empty()) {
        let (pos, rect) = grid(g, Vec2::new(x, origin.y + header), GAP);
        x = rect.max.x + GROUP_GAP;
        out.extend(pos);
        rects.push(rect);
    }
    (out, rects)
}

/// 要素を指定の左上へ動かす(Undo では 1 回の操作)。
pub fn move_to(board: &mut Board, moves: &[(ElementId, Vec2)]) {
    board.end_gesture();
    for (id, pos) in moves {
        board.update(*id, |e| {
            let d = *pos - e.bounds().min;
            if d != Vec2::ZERO {
                e.translate(d);
            }
        });
    }
    board.end_gesture();
}

fn items_of(board: &Board, ids: &[ElementId]) -> Vec<(ElementId, Rect)> {
    ids.iter()
        .filter_map(|id| board.get(*id).map(|e| (*id, e.bounds())))
        .collect()
}

/// 付箋を整理する。動かした数を返す。
pub fn arrange(board: &mut Board, ids: &[ElementId], mode: Arrange) -> usize {
    let mut items = items_of(board, ids);
    if items.len() < 2 {
        return 0;
    }
    let origin = items
        .iter()
        .map(|(_, r)| r.min)
        .fold(Vec2::splat(f32::INFINITY), Vec2::min);
    reading_order(&mut items);
    let moves = match mode {
        Arrange::Grid => {
            let sized: Vec<_> = items.iter().map(|(id, r)| (*id, r.size())).collect();
            grid(&sized, origin, GAP).0
        }
        Arrange::ByColor => {
            // 色の登場順(読む順で最初に出てきた順)にグループを並べる。
            let mut order: Vec<u32> = Vec::new();
            let mut by_color: HashMap<u32, Vec<(ElementId, Vec2)>> = HashMap::new();
            for (id, r) in &items {
                let c = board
                    .get(*id)
                    .and_then(Element::as_shape)
                    .map(|s| s.fill.to_u32())
                    .unwrap_or(0);
                if !by_color.contains_key(&c) {
                    order.push(c);
                }
                by_color.entry(c).or_default().push((*id, r.size()));
            }
            let gs: Vec<_> = order.iter().filter_map(|c| by_color.remove(c)).collect();
            groups(&gs, origin, 0.0).0
        }
    };
    let n = moves.len();
    move_to(board, &moves);
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{Rgba8, STICKY_COLORS};
    use crate::shape::Shape;

    fn add(b: &mut Board, x: f32, y: f32, c: Rgba8) -> ElementId {
        let id = b.alloc_id();
        b.push(Shape::sticky(id, Vec2::new(x, y), c));
        id
    }

    #[test]
    fn grid_is_square_ish() {
        let items: Vec<_> = (0..5).map(|i| (ElementId(i), Vec2::splat(100.0))).collect();
        let (pos, rect) = grid(&items, Vec2::ZERO, 10.0);
        // 5 個 → 3 列 2 行
        assert_eq!(pos[2].1, Vec2::new(220.0, 0.0));
        assert_eq!(pos[3].1, Vec2::new(0.0, 110.0));
        assert_eq!(rect.max, Vec2::new(320.0, 210.0));
    }

    #[test]
    fn reading_order_rows_then_columns() {
        let r = |x: f32, y: f32| Rect::new(Vec2::new(x, y), Vec2::new(x + 100.0, y + 100.0));
        let mut items = vec![
            (ElementId(1), r(500.0, 10.0)),
            (ElementId(2), r(0.0, 300.0)),
            (ElementId(3), r(0.0, 0.0)),
        ];
        reading_order(&mut items);
        let ids: Vec<_> = items.iter().map(|(id, _)| id.0).collect();
        assert_eq!(ids, vec![3, 1, 2]);
    }

    #[test]
    fn arrange_grid_and_undo() {
        let mut b = Board::new();
        let ids: Vec<_> = (0..4)
            .map(|i| {
                add(
                    &mut b,
                    i as f32 * 37.0,
                    (i * i) as f32 * 91.0,
                    STICKY_COLORS[0],
                )
            })
            .collect();
        b.end_gesture();
        let rect = b.alloc_id();
        b.push(Shape::rect(
            rect,
            Vec2::splat(-500.0),
            Vec2::ONE,
            Rgba8::WHITE,
        ));
        b.end_gesture();

        let t = targets(&b, &[]);
        assert_eq!(t.len(), 4, "四角形は対象外");
        assert_eq!(arrange(&mut b, &t, Arrange::Grid), 4);
        let pos: Vec<_> = ids
            .iter()
            .map(|id| b.get(*id).unwrap().bounds().min)
            .collect();
        assert_eq!(pos[0], Vec2::ZERO);
        assert_eq!(pos[1], Vec2::new(224.0, 0.0));
        assert_eq!(pos[2], Vec2::new(0.0, 224.0));
        // 1 回の Undo で全部戻る。
        assert!(b.undo());
        assert_eq!(b.get(ids[3]).unwrap().bounds().min, Vec2::new(111.0, 819.0));
    }

    #[test]
    fn arrange_by_color_groups() {
        let mut b = Board::new();
        let y1 = add(&mut b, 0.0, 0.0, STICKY_COLORS[0]);
        let p1 = add(&mut b, 250.0, 0.0, STICKY_COLORS[2]);
        let y2 = add(&mut b, 500.0, 0.0, STICKY_COLORS[0]);
        let p2 = add(&mut b, 750.0, 0.0, STICKY_COLORS[2]);
        b.end_gesture();
        arrange(&mut b, &[y1, p1, y2, p2], Arrange::ByColor);
        let x = |id| b.get(id).unwrap().bounds().min.x;
        // 黄色 2 枚(2 列)が左、ピンクがその右。
        assert_eq!(x(y1), 0.0);
        assert_eq!(x(y2), 224.0);
        assert!(x(p1) > x(y2) + 200.0);
        assert!(x(p2) > x(p1));
    }

    #[test]
    fn selection_limits_targets() {
        let mut b = Board::new();
        let a = add(&mut b, 0.0, 0.0, STICKY_COLORS[0]);
        let c = add(&mut b, 300.0, 0.0, STICKY_COLORS[0]);
        let _ = add(&mut b, 600.0, 0.0, STICKY_COLORS[0]);
        assert_eq!(targets(&b, &[a, c]), vec![a, c]);
        assert_eq!(targets(&b, &[a]).len(), 3);
    }
}
