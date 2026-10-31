use std::collections::HashMap;

use glam::Vec2;

use crate::color::{Rgba8, STICKY_COLORS};
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::Shape;

/// ボード全体。並び順がそのまま描画順(後ろほど手前)。
///
/// 共同編集の段階でこの中身は CRDT ドキュメントのビューに置き換わる。
#[derive(Clone, Debug, Default)]
pub struct Board {
    elements: Vec<Element>,
    index: HashMap<ElementId, usize>,
    /// 内容が変わるたびに増える。描画側がバッファ再転送の要否を判断する。
    revision: u64,
    next_id: u64,
}

impl Board {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn elements(&self) -> &[Element] {
        &self.elements
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    pub fn alloc_id(&mut self) -> ElementId {
        self.next_id += 1;
        ElementId(self.next_id)
    }

    pub fn get(&self, id: ElementId) -> Option<&Element> {
        self.index.get(&id).map(|&i| &self.elements[i])
    }

    /// 要素を変更する。変更したら revision が進む。
    pub fn update<R>(&mut self, id: ElementId, f: impl FnOnce(&mut Element) -> R) -> Option<R> {
        let i = *self.index.get(&id)?;
        self.revision += 1;
        Some(f(&mut self.elements[i]))
    }

    /// 最前面に追加する。
    pub fn push(&mut self, element: impl Into<Element>) -> ElementId {
        let element = element.into();
        let id = element.id();
        self.next_id = self.next_id.max(id.0);
        if let Some(&i) = self.index.get(&id) {
            self.elements[i] = element;
        } else {
            self.index.insert(id, self.elements.len());
            self.elements.push(element);
        }
        self.revision += 1;
        id
    }

    pub fn remove(&mut self, ids: &[ElementId]) -> usize {
        let before = self.elements.len();
        self.elements.retain(|e| !ids.contains(&e.id()));
        let removed = before - self.elements.len();
        if removed > 0 {
            self.reindex();
            self.revision += 1;
        }
        removed
    }

    pub fn clear(&mut self) {
        self.elements.clear();
        self.index.clear();
        self.revision += 1;
    }

    fn reindex(&mut self) {
        self.index = self
            .elements
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id(), i))
            .collect();
    }

    /// 全要素を囲む矩形。空なら `None`。
    pub fn bounds(&self) -> Option<Rect> {
        Self::union_bounds(self.elements.iter())
    }

    /// 指定要素を囲む矩形。
    pub fn bounds_of(&self, ids: &[ElementId]) -> Option<Rect> {
        Self::union_bounds(ids.iter().filter_map(|id| self.get(*id)))
    }

    fn union_bounds<'a>(mut it: impl Iterator<Item = &'a Element>) -> Option<Rect> {
        let first = it.next()?.bounds();
        Some(it.fold(first, |r, e| r.union(&e.bounds())))
    }

    /// 最前面から探して、点の下にある要素を返す。
    pub fn hit_test(&self, world: Vec2, tolerance: f32) -> Option<&Element> {
        self.elements.iter().rev().find(|e| e.hit(world, tolerance))
    }

    /// 矩形に完全に含まれる要素(範囲選択用)。
    pub fn query_inside(&self, r: &Rect) -> Vec<ElementId> {
        self.elements
            .iter()
            .filter(|e| r.contains_rect(&e.bounds()))
            .map(Element::id)
            .collect()
    }

    /// 初回表示用の小さなボード。
    pub fn welcome() -> Self {
        let mut b = Self::new();
        let notes = [
            (Vec2::new(0.0, 0.0), STICKY_COLORS[0]),
            (Vec2::new(240.0, 0.0), STICKY_COLORS[4]),
            (Vec2::new(480.0, 0.0), STICKY_COLORS[2]),
        ];
        let texts = [
            "ようこそ ColabBoard へ!\nダブルクリックで編集できます",
            "付箋・図形・ペンは下のツールバーから",
            "Ctrl/⌘ + スクロールでズーム",
        ];
        for ((pos, c), text) in notes.into_iter().zip(texts) {
            let id = b.alloc_id();
            let mut note = Shape::sticky(id, pos, c);
            note.text = text.to_string();
            b.push(note);
        }
        let id = b.alloc_id();
        b.push(Shape::rect(
            id,
            Vec2::new(0.0, 260.0),
            Vec2::new(300.0, 140.0),
            Rgba8::hex(0xe8f0fe),
        ));
        let id = b.alloc_id();
        b.push(Shape::ellipse(
            id,
            Vec2::new(380.0, 250.0),
            Vec2::new(160.0, 160.0),
            Rgba8::hex(0xe6f4ea),
        ));
        b
    }

    /// 性能確認用に `n` 個の要素を格子状に並べたボードを作る(決定的)。
    pub fn demo(n: usize) -> Self {
        let mut board = Self::new();
        board.elements.reserve(n);
        board.index.reserve(n);
        let cols = (n as f32).sqrt().ceil().max(1.0) as usize;
        let cell = 260.0;
        let mut rng = SplitMix64(0x0005_eedc_01ab_0a2d);
        let palette = [
            Rgba8::hex(0xffffff),
            Rgba8::hex(0xe8f0fe),
            Rgba8::hex(0xfde7e9),
            Rgba8::hex(0xe6f4ea),
        ];
        for i in 0..n {
            let id = board.alloc_id();
            let pos = Vec2::new((i % cols) as f32, (i / cols) as f32) * cell;
            let r = rng.next();
            let shape = match r % 3 {
                0 => Shape::sticky(
                    id,
                    pos,
                    STICKY_COLORS[(r >> 8) as usize % STICKY_COLORS.len()],
                ),
                1 => {
                    let size = Vec2::new(
                        120.0 + (r >> 16) as f32 % 100.0,
                        80.0 + (r >> 24) as f32 % 120.0,
                    );
                    Shape::rect(id, pos, size, palette[(r >> 32) as usize % palette.len()])
                }
                _ => {
                    let d = 100.0 + (r >> 16) as f32 % 100.0;
                    Shape::ellipse(
                        id,
                        pos,
                        Vec2::splat(d),
                        palette[(r >> 32) as usize % palette.len()],
                    )
                }
            };
            board.push(shape);
        }
        board
    }
}

/// 依存を増やさないための小さな擬似乱数。
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_is_deterministic_and_sized() {
        let a = Board::demo(1000);
        let b = Board::demo(1000);
        assert_eq!(a.len(), 1000);
        assert_eq!(a.elements(), b.elements());
        let r = a.bounds().unwrap();
        assert!(r.min.cmpge(Vec2::ZERO).all());
    }

    #[test]
    fn hit_test_picks_topmost() {
        let mut b = Board::new();
        let id1 = b.alloc_id();
        b.push(Shape::rect(
            id1,
            Vec2::ZERO,
            Vec2::splat(100.0),
            Rgba8::WHITE,
        ));
        let id2 = b.alloc_id();
        b.push(Shape::rect(
            id2,
            Vec2::splat(50.0),
            Vec2::splat(100.0),
            Rgba8::WHITE,
        ));
        assert_eq!(b.hit_test(Vec2::splat(75.0), 0.0).unwrap().id(), id2);
        assert_eq!(b.hit_test(Vec2::splat(10.0), 0.0).unwrap().id(), id1);
        assert!(b.hit_test(Vec2::splat(500.0), 0.0).is_none());
    }

    #[test]
    fn update_remove_and_index() {
        let mut b = Board::welcome();
        let ids: Vec<_> = b.elements().iter().map(Element::id).collect();
        let r0 = b.revision();
        b.update(ids[1], |e| e.translate(Vec2::new(5.0, 0.0)));
        assert!(b.revision() > r0);
        assert_eq!(b.remove(&[ids[0]]), 1);
        assert!(b.get(ids[0]).is_none());
        // 削除後も ID から引ける。
        assert_eq!(b.get(ids[2]).unwrap().id(), ids[2]);
        let inside = b.query_inside(&Rect::new(Vec2::splat(-1000.0), Vec2::splat(1000.0)));
        assert_eq!(inside.len(), b.len());
    }
}
