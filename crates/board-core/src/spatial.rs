//! 要素の空間インデックス(一様グリッド)。
//!
//! ボードを一辺 `CELL` のマスに区切り、要素の外接矩形が重なるマスに要素 ID を登録する。
//! 点の下の要素(ヒットテスト)や、画面内・範囲内の要素を、全要素を見ずに探せる。
//! 数十万要素のボードでも、ポインタ移動ごとの探索や文字の準備が要素数に比例しない。
//!
//! とても大きい要素(多数のマスにまたがるもの)はマスに入れず、別の一覧で持つ。

use std::collections::{HashMap, HashSet};

use crate::element::ElementId;
use crate::geom::Rect;

/// マスの一辺(ワールド単位)。付箋(200)より少し大きい。
pub const CELL: f32 = 256.0;
/// これより多くのマスにまたがる要素は「大きい要素」として別に持つ。
const MAX_CELLS_PER_ELEMENT: i64 = 64;

type Cell = (i32, i32);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Slot {
    /// マスの範囲(両端を含む)。
    Cells {
        lo: Cell,
        hi: Cell,
    },
    Big,
}

#[derive(Default, Debug)]
pub struct SpatialIndex {
    cells: HashMap<Cell, Vec<ElementId>>,
    big: HashSet<ElementId>,
    slots: HashMap<ElementId, Slot>,
}

fn cell_of(v: f32) -> i32 {
    // `as` は飽和変換なので、巨大な座標や NaN でもパニックしない。
    (v / CELL).floor() as i32
}

fn cell_range(r: &Rect) -> (Cell, Cell) {
    (
        (cell_of(r.min.x), cell_of(r.min.y)),
        (cell_of(r.max.x), cell_of(r.max.y)),
    )
}

fn cell_count(lo: Cell, hi: Cell) -> i64 {
    let w = (hi.0 as i64 - lo.0 as i64 + 1).max(0);
    let h = (hi.1 as i64 - lo.1 as i64 + 1).max(0);
    w * h
}

impl SpatialIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn clear(&mut self) {
        self.cells.clear();
        self.big.clear();
        self.slots.clear();
    }

    pub fn reserve(&mut self, n: usize) {
        self.slots.reserve(n);
        self.cells.reserve(n);
    }

    /// 要素を登録する(登録済みなら位置を更新する)。
    pub fn insert(&mut self, id: ElementId, bounds: Rect) {
        let (lo, hi) = cell_range(&bounds);
        let slot = if bounds.min.is_finite()
            && bounds.max.is_finite()
            && cell_count(lo, hi) <= MAX_CELLS_PER_ELEMENT
        {
            Slot::Cells { lo, hi }
        } else {
            Slot::Big
        };
        if let Some(old) = self.slots.get(&id).copied() {
            if old == slot {
                return;
            }
            self.unlink(id, old);
        }
        match slot {
            Slot::Big => {
                self.big.insert(id);
            }
            Slot::Cells { lo, hi } => {
                for y in lo.1..=hi.1 {
                    for x in lo.0..=hi.0 {
                        self.cells.entry((x, y)).or_default().push(id);
                    }
                }
            }
        }
        self.slots.insert(id, slot);
    }

    pub fn remove(&mut self, id: ElementId) {
        if let Some(slot) = self.slots.remove(&id) {
            self.unlink(id, slot);
        }
    }

    fn unlink(&mut self, id: ElementId, slot: Slot) {
        match slot {
            Slot::Big => {
                self.big.remove(&id);
            }
            Slot::Cells { lo, hi } => {
                for y in lo.1..=hi.1 {
                    for x in lo.0..=hi.0 {
                        if let Some(v) = self.cells.get_mut(&(x, y)) {
                            if let Some(i) = v.iter().position(|e| *e == id) {
                                v.swap_remove(i);
                            }
                            if v.is_empty() {
                                self.cells.remove(&(x, y));
                            }
                        }
                    }
                }
            }
        }
    }

    /// `r` と重なるかもしれない要素を `f` に渡す(同じ要素が複数回来ることがある)。
    ///
    /// 調べるマスが登録要素数より多くなる(範囲が広すぎる)ときは何もせず `false` を返す。
    /// 呼び出し側は全要素を見る方法に切り替える。
    pub fn query(&self, r: &Rect, mut f: impl FnMut(ElementId)) -> bool {
        let (lo, hi) = cell_range(r);
        let n = cell_count(lo, hi);
        if self.cells.is_empty() {
            // マスに何もなければ大きい要素だけ見ればよい。
            self.big.iter().copied().for_each(f);
            return true;
        }
        // マスを 1 つずつ引くより全要素を見るほうが速いなら、そちらに任せる。
        let limit = (self.slots.len() as i64).max(1024);
        if n > limit || !r.min.is_finite() || !r.max.is_finite() {
            return false;
        }
        for y in lo.1..=hi.1 {
            for x in lo.0..=hi.0 {
                if let Some(v) = self.cells.get(&(x, y)) {
                    v.iter().copied().for_each(&mut f);
                }
            }
        }
        self.big.iter().copied().for_each(f);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(Vec2::new(x, y), Vec2::new(x + w, y + h))
    }

    fn found(ix: &SpatialIndex, r: Rect) -> Vec<u64> {
        let mut v = Vec::new();
        assert!(ix.query(&r, |id| v.push(id.0)));
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn insert_move_remove() {
        let mut ix = SpatialIndex::new();
        ix.insert(ElementId(1), rect(0.0, 0.0, 100.0, 100.0));
        ix.insert(ElementId(2), rect(1000.0, 1000.0, 100.0, 100.0));
        // 複数のマスにまたがる要素。
        ix.insert(ElementId(3), rect(300.0, 300.0, 400.0, 400.0));
        assert_eq!(found(&ix, rect(10.0, 10.0, 1.0, 1.0)), vec![1]);
        assert_eq!(found(&ix, rect(500.0, 500.0, 1.0, 1.0)), vec![3]);
        assert_eq!(
            found(&ix, rect(-50.0, -50.0, 2000.0, 2000.0)),
            vec![1, 2, 3]
        );

        ix.insert(ElementId(1), rect(1000.0, 1000.0, 10.0, 10.0));
        assert!(found(&ix, rect(10.0, 10.0, 1.0, 1.0)).is_empty());
        assert_eq!(found(&ix, rect(1005.0, 1005.0, 1.0, 1.0)), vec![1, 2]);

        ix.remove(ElementId(2));
        ix.remove(ElementId(3));
        assert_eq!(found(&ix, rect(-2000.0, -2000.0, 4000.0, 4000.0)), vec![1]);
        assert_eq!(ix.len(), 1);
    }

    #[test]
    fn big_elements_always_returned() {
        let mut ix = SpatialIndex::new();
        ix.insert(ElementId(1), rect(-1e6, -1e6, 2e6, 2e6));
        ix.insert(ElementId(2), rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(found(&ix, rect(5.0, 5.0, 1.0, 1.0)), vec![1, 2]);
        assert_eq!(found(&ix, rect(5000.0, 5000.0, 1.0, 1.0)), vec![1]);
        // NaN の外接矩形でもパニックしない。
        ix.insert(ElementId(3), Rect::new(Vec2::NAN, Vec2::NAN));
        assert_eq!(found(&ix, rect(5000.0, 5000.0, 1.0, 1.0)), vec![1, 3]);
    }

    #[test]
    fn huge_query_falls_back() {
        let mut ix = SpatialIndex::new();
        ix.insert(ElementId(1), rect(0.0, 0.0, 10.0, 10.0));
        assert!(!ix.query(&rect(-1e7, -1e7, 2e7, 2e7), |_| {}));
    }
}
