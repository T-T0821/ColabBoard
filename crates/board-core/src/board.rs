use glam::Vec2;

use crate::color::{Rgba8, STICKY_COLORS};
use crate::shape::{Shape, ShapeId};

/// ボード全体。並び順がそのまま描画順(後ろほど手前)。
///
/// 共同編集の段階でこの中身は CRDT ドキュメントのビューに置き換わる。
#[derive(Clone, Debug, Default)]
pub struct Board {
    shapes: Vec<Shape>,
    /// 内容が変わるたびに増える。描画側がバッファ再転送の要否を判断する。
    revision: u64,
    next_id: u64,
}

impl Board {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    pub fn alloc_id(&mut self) -> ShapeId {
        self.next_id += 1;
        ShapeId(self.next_id)
    }

    pub fn push(&mut self, shape: Shape) {
        self.next_id = self.next_id.max(shape.id.0);
        self.shapes.push(shape);
        self.revision += 1;
    }

    pub fn clear(&mut self) {
        self.shapes.clear();
        self.revision += 1;
    }

    /// 全要素を囲む矩形。空なら `None`。
    pub fn bounds(&self) -> Option<(Vec2, Vec2)> {
        let mut it = self.shapes.iter();
        let first = it.next()?;
        Some(it.fold((first.min(), first.max()), |(lo, hi), s| {
            (lo.min(s.min()), hi.max(s.max()))
        }))
    }

    /// 最前面から探して、点の下にある要素を返す。
    pub fn hit_test(&self, world: Vec2) -> Option<&Shape> {
        self.shapes.iter().rev().find(|s| s.contains(world))
    }

    /// 性能確認用に `n` 個の要素を格子状に並べたボードを作る(決定的)。
    pub fn demo(n: usize) -> Self {
        let mut board = Self::new();
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
        assert_eq!(a.shapes(), b.shapes());
        let (lo, hi) = a.bounds().unwrap();
        assert!(lo.cmpge(Vec2::ZERO).all());
        assert!(hi.x > 0.0 && hi.y > 0.0);
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
        assert_eq!(b.hit_test(Vec2::splat(75.0)).unwrap().id, id2);
        assert_eq!(b.hit_test(Vec2::splat(10.0)).unwrap().id, id1);
        assert!(b.hit_test(Vec2::splat(500.0)).is_none());
    }

    #[test]
    fn revision_increments() {
        let mut b = Board::new();
        let r0 = b.revision();
        let id = b.alloc_id();
        b.push(Shape::sticky(id, Vec2::ZERO, Rgba8::WHITE));
        assert!(b.revision() > r0);
    }
}
