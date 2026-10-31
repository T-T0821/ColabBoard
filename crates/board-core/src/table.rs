//! 表。
//!
//! 表は「隙間なく格子に並んだ四角形のセル」をグループにしたもの。専用の要素を持たないので、
//! 同期・Undo・コピー&ペースト・移動は他の図形と同じように動く。
//! 行や列の追加・削除は、グループの並びから格子を読み取って行う。

use glam::Vec2;

use crate::board::Board;
use crate::color::Rgba8;
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind, TextAlign};

pub const CELL_SIZE: Vec2 = Vec2::new(160.0, 48.0);
const HEADER_FILL: Rgba8 = Rgba8::hex(0xf1f3f4);
const BORDER: Rgba8 = Rgba8::hex(0xb8bcc2);
/// 位置を同じとみなす誤差(ワールド単位)。
const EPS: f32 = 0.5;

pub(crate) fn cell(id: ElementId, pos: Vec2, size: Vec2, header: bool, group: u64) -> Shape {
    let fill = if header { HEADER_FILL } else { Rgba8::WHITE };
    let mut s = Shape::new(id, ShapeKind::Rect, pos, size, fill);
    s.stroke = BORDER;
    s.stroke_width = 1.0;
    s.corner_radius = 0.0;
    s.font_size = 16.0;
    s.align = TextAlign::Left;
    s.meta.group = group;
    s
}

/// 中心 `center` に `rows` 行 `cols` 列の表を作る。1 行目は見出しの色。
pub fn create(board: &mut Board, center: Vec2, rows: usize, cols: usize) -> Vec<ElementId> {
    let rows = rows.max(1);
    let cols = cols.max(1);
    let group = board.alloc_id().0;
    let origin = center - CELL_SIZE * Vec2::new(cols as f32, rows as f32) * 0.5;
    let mut cells = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        for c in 0..cols {
            let id = board.alloc_id();
            let pos = origin + CELL_SIZE * Vec2::new(c as f32, r as f32);
            cells.push(Element::from(cell(id, pos, CELL_SIZE, r == 0, group)));
        }
    }
    let ids = cells.iter().map(Element::id).collect();
    board.push_many(cells);
    ids
}

/// グループから読み取った格子。
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub group: u64,
    /// 列の左端 / 行の上端(昇順)。
    pub xs: Vec<f32>,
    pub ys: Vec<f32>,
    pub widths: Vec<f32>,
    pub heights: Vec<f32>,
    /// `cells[row][col]`。
    pub cells: Vec<Vec<ElementId>>,
}

impl Grid {
    pub fn rows(&self) -> usize {
        self.ys.len()
    }

    pub fn cols(&self) -> usize {
        self.xs.len()
    }

    /// ワールド座標の点がある行と列。
    pub fn locate(&self, p: Vec2) -> Option<(usize, usize)> {
        let col =
            (0..self.cols()).find(|&c| p.x >= self.xs[c] && p.x <= self.xs[c] + self.widths[c])?;
        let row =
            (0..self.rows()).find(|&r| p.y >= self.ys[r] && p.y <= self.ys[r] + self.heights[r])?;
        Some((row, col))
    }

    fn bottom(&self) -> f32 {
        self.ys.last().unwrap() + self.heights.last().unwrap()
    }

    fn right(&self) -> f32 {
        self.xs.last().unwrap() + self.widths.last().unwrap()
    }
}

fn dedup_sorted(mut v: Vec<f32>) -> Vec<f32> {
    v.sort_by(f32::total_cmp);
    v.dedup_by(|a, b| (*a - *b).abs() < EPS);
    v
}

/// グループ `group` が表(隙間のない格子に並んだ四角形)なら、その格子を返す。
pub fn grid(board: &Board, group: u64) -> Option<Grid> {
    if group == 0 {
        return None;
    }
    let members: Vec<&Shape> = board
        .elements()
        .iter()
        .filter(|e| e.meta().group == group)
        .map(|e| e.as_shape().filter(|s| s.kind == ShapeKind::Rect))
        .collect::<Option<_>>()?;
    if members.len() < 2 {
        return None;
    }
    let xs = dedup_sorted(members.iter().map(|s| s.pos.x).collect());
    let ys = dedup_sorted(members.iter().map(|s| s.pos.y).collect());
    if xs.len() * ys.len() != members.len() {
        return None;
    }
    let find = |v: &[f32], x: f32| v.iter().position(|a| (a - x).abs() < EPS);
    let mut cells = vec![vec![ElementId(0); xs.len()]; ys.len()];
    let mut widths = vec![0.0; xs.len()];
    let mut heights = vec![0.0; ys.len()];
    for s in &members {
        let c = find(&xs, s.pos.x)?;
        let r = find(&ys, s.pos.y)?;
        if cells[r][c] != ElementId(0) {
            return None;
        }
        cells[r][c] = s.id;
        widths[c] = s.size.x;
        heights[r] = s.size.y;
    }
    // 列の幅・行の高さがそろい、隣と隙間なく並んでいるか。
    for s in &members {
        let c = find(&xs, s.pos.x)?;
        let r = find(&ys, s.pos.y)?;
        if (s.size.x - widths[c]).abs() > EPS || (s.size.y - heights[r]).abs() > EPS {
            return None;
        }
    }
    let contiguous = |starts: &[f32], sizes: &[f32]| {
        starts
            .windows(2)
            .zip(sizes)
            .all(|(w, size)| (w[0] + size - w[1]).abs() < EPS)
    };
    if !contiguous(&xs, &widths) || !contiguous(&ys, &heights) {
        return None;
    }
    Some(Grid {
        group,
        xs,
        ys,
        widths,
        heights,
        cells,
    })
}

/// 選択 `ids` のうちのどれかが属する表。
pub fn grid_of(board: &Board, ids: &[ElementId]) -> Option<Grid> {
    ids.iter()
        .filter_map(|id| board.get(*id))
        .map(|e| e.meta().group)
        .find_map(|g| grid(board, g))
}

/// 表のセル `id` の次(`forward` が false なら前)のセル。端では折り返す。
pub fn next_cell(board: &Board, id: ElementId, forward: bool) -> Option<ElementId> {
    let g = grid(board, board.get(id)?.meta().group)?;
    let flat: Vec<ElementId> = g.cells.into_iter().flatten().collect();
    let i = flat.iter().position(|c| *c == id)?;
    let n = flat.len();
    Some(
        flat[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }],
    )
}

/// 行・列の操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    RowAbove,
    RowBelow,
    ColLeft,
    ColRight,
    DeleteRow,
    DeleteCol,
}

fn shift(board: &mut Board, ids: impl IntoIterator<Item = ElementId>, d: Vec2) {
    for id in ids {
        board.update(id, |e| e.translate(d));
    }
}

/// 点 `at` のセルを基準に、行や列を足す・消す。新しくできたセルの ID を返す。
pub fn edit(board: &mut Board, g: &Grid, at: Vec2, how: Edit) -> Option<Vec<ElementId>> {
    let (row, col) = g.locate(at)?;
    let style_of = |board: &Board, id: ElementId| board.get(id).and_then(|e| e.as_shape().cloned());
    let mut created = Vec::new();
    match how {
        Edit::RowAbove | Edit::RowBelow => {
            let insert = if how == Edit::RowAbove { row } else { row + 1 };
            let h = g.heights[row];
            let y = if insert < g.rows() {
                g.ys[insert]
            } else {
                g.bottom()
            };
            let below: Vec<ElementId> = g.cells[insert..].iter().flatten().copied().collect();
            shift(board, below, Vec2::new(0.0, h));
            // 見出しの上に足すときは、見出しでない行の見た目にする。
            let template_row = if row == 0 && g.rows() > 1 { 1 } else { row };
            let mut new = Vec::new();
            for c in 0..g.cols() {
                let mut s = style_of(board, g.cells[template_row][c])?;
                s.id = board.alloc_id();
                s.pos = Vec2::new(g.xs[c], y);
                s.size = Vec2::new(g.widths[c], h);
                s.text.clear();
                created.push(s.id);
                new.push(Element::from(s));
            }
            board.push_many(new);
        }
        Edit::ColLeft | Edit::ColRight => {
            let insert = if how == Edit::ColLeft { col } else { col + 1 };
            let w = g.widths[col];
            let x = if insert < g.cols() {
                g.xs[insert]
            } else {
                g.right()
            };
            let right: Vec<ElementId> = g
                .cells
                .iter()
                .flat_map(|r| r[insert..].iter().copied())
                .collect();
            shift(board, right, Vec2::new(w, 0.0));
            let mut new = Vec::new();
            for r in 0..g.rows() {
                let mut s = style_of(board, g.cells[r][col])?;
                s.id = board.alloc_id();
                s.pos = Vec2::new(x, g.ys[r]);
                s.size = Vec2::new(w, g.heights[r]);
                s.text.clear();
                created.push(s.id);
                new.push(Element::from(s));
            }
            board.push_many(new);
        }
        Edit::DeleteRow => {
            if g.rows() <= 1 {
                return None;
            }
            board.remove(&g.cells[row]);
            let below: Vec<ElementId> = g.cells[row + 1..].iter().flatten().copied().collect();
            shift(board, below, Vec2::new(0.0, -g.heights[row]));
        }
        Edit::DeleteCol => {
            if g.cols() <= 1 {
                return None;
            }
            let ids: Vec<ElementId> = g.cells.iter().map(|r| r[col]).collect();
            board.remove(&ids);
            let right: Vec<ElementId> = g
                .cells
                .iter()
                .flat_map(|r| r[col + 1..].iter().copied())
                .collect();
            shift(board, right, Vec2::new(-g.widths[col], 0.0));
        }
    }
    Some(created)
}

/// 表全体の外接矩形。
pub fn bounds(g: &Grid) -> Rect {
    Rect::new(
        Vec2::new(g.xs[0], g.ys[0]),
        Vec2::new(g.right(), g.bottom()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: usize, cols: usize) -> (Board, Grid) {
        let mut b = Board::new();
        let ids = create(&mut b, Vec2::ZERO, rows, cols);
        assert_eq!(ids.len(), rows * cols);
        let g = grid_of(&b, &ids[..1]).expect("表として読める");
        (b, g)
    }

    #[test]
    fn create_and_read_grid() {
        let (_, g) = table(3, 4);
        assert_eq!((g.rows(), g.cols()), (3, 4));
        let r = bounds(&g);
        assert_eq!(r.center(), Vec2::ZERO);
        assert_eq!(g.locate(r.min + Vec2::splat(1.0)), Some((0, 0)));
        assert_eq!(g.locate(r.max - Vec2::splat(1.0)), Some((2, 3)));
    }

    #[test]
    fn insert_and_delete_rows_and_cols() {
        let (mut b, g) = table(2, 2);
        let first = g.cells[0][0];
        let at = bounds(&g).min + Vec2::splat(1.0); // 左上のセル
        let new = edit(&mut b, &g, at, Edit::RowBelow).unwrap();
        assert_eq!(new.len(), 2);
        let g = grid(&b, g.group).unwrap();
        assert_eq!(g.rows(), 3);
        // 新しい行は見出しの色ではない。
        let fill = b.get(g.cells[1][0]).unwrap().as_shape().unwrap().fill;
        assert_eq!(fill, Rgba8::WHITE);

        edit(&mut b, &g, at, Edit::ColLeft).unwrap();
        let g = grid(&b, g.group).unwrap();
        assert_eq!(g.cols(), 3);
        assert_eq!(g.cells[0][1], first, "元のセルは右へずれる");

        edit(&mut b, &g, at, Edit::DeleteCol).unwrap();
        let g = grid(&b, g.group).unwrap();
        assert_eq!(g.cols(), 2);
        assert_eq!(g.cells[0][0], first);

        edit(&mut b, &g, at, Edit::DeleteRow).unwrap();
        let g = grid(&b, g.group).unwrap();
        assert_eq!(g.rows(), 2);
        // 消し過ぎない。
        let (mut b1, g1) = table(1, 2);
        let at1 = bounds(&g1).min + Vec2::splat(1.0);
        assert!(edit(&mut b1, &g1, at1, Edit::DeleteRow).is_none());
    }

    #[test]
    fn tab_moves_between_cells() {
        let (b, g) = table(2, 2);
        assert_eq!(next_cell(&b, g.cells[0][1], true), Some(g.cells[1][0]));
        assert_eq!(next_cell(&b, g.cells[1][1], true), Some(g.cells[0][0]));
        assert_eq!(next_cell(&b, g.cells[0][0], false), Some(g.cells[1][1]));
    }

    #[test]
    fn broken_grid_is_not_a_table() {
        let (mut b, g) = table(2, 2);
        b.update(g.cells[1][1], |e| e.translate(Vec2::new(5.0, 0.0)));
        assert!(grid(&b, g.group).is_none());
    }
}
