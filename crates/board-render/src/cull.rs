//! ブロック単位のカリング(CPU 側)。
//!
//! インスタンス列を描画順のまま `BLOCK` 個ずつのブロックに分け、ブロックごとの外接矩形を持つ。
//! 毎フレーム、画面と重なるブロックだけを描画命令の範囲にする。
//!
//! 描画順(重なり順)を一切変えないので、見た目は全要素を描いたときと同じ。
//! 要素は作られた順に並び、近くに続けて置かれることが多いので、広いボードでは
//! 画面外の大半を頂点シェーダに渡す前に捨てられる(頂点シェーダ側のカリングは残す)。

use std::ops::Range;

use board_core::Camera;

use crate::{KIND_SEGMENT, ShapeInstance};

/// 1 ブロックのインスタンス数。
pub const BLOCK: usize = 256;
/// 描画命令の数の上限。超えるときは近い範囲どうしをつなげる。
pub const MAX_RANGES: usize = 64;
/// 枠線・付箋の影など、図形の外にはみ出して描く分(ワールド単位)。
const OUTSET: f32 = 24.0;

#[derive(Default, Debug)]
pub struct VisibleBlocks {
    /// ブロックごとの外接矩形 `[min_x, min_y, max_x, max_y]`。
    bounds: Vec<[f32; 4]>,
    len: u32,
    ranges: Vec<Range<u32>>,
    gaps: Vec<u32>,
}

fn instance_bounds(i: &ShapeInstance) -> [f32; 4] {
    if i.kind == KIND_SEGMENT {
        let r = i.radius;
        [
            i.pos[0].min(i.size[0]) - r,
            i.pos[1].min(i.size[1]) - r,
            i.pos[0].max(i.size[0]) + r,
            i.pos[1].max(i.size[1]) + r,
        ]
    } else {
        let sw = i.stroke_width.max(0.0);
        [
            i.pos[0] - sw,
            i.pos[1] - sw,
            i.pos[0] + i.size[0] + sw,
            i.pos[1] + i.size[1] + sw,
        ]
    }
}

fn block_bounds(chunk: &[ShapeInstance]) -> [f32; 4] {
    chunk.iter().map(instance_bounds).fold(
        [
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        ],
        |a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        },
    )
}

impl VisibleBlocks {
    /// インスタンス列全体からブロックを作り直す。
    pub fn rebuild(&mut self, instances: &[ShapeInstance]) {
        self.bounds.clear();
        self.bounds
            .extend(instances.chunks(BLOCK).map(block_bounds));
        self.len = instances.len() as u32;
    }

    /// `instances` の `range` だけが変わったときに、関係するブロックだけ計算し直す。
    /// `instances` は変更後の全体。長さが変わったときは `rebuild` を使う。
    pub fn update(&mut self, instances: &[ShapeInstance], range: Range<usize>) {
        if instances.len() as u32 != self.len {
            self.rebuild(instances);
            return;
        }
        let first = range.start / BLOCK;
        let last = range.end.div_ceil(BLOCK).min(self.bounds.len());
        for b in first..last {
            let end = ((b + 1) * BLOCK).min(instances.len());
            self.bounds[b] = block_bounds(&instances[b * BLOCK..end]);
        }
    }

    pub fn instance_count(&self) -> u32 {
        self.len
    }

    /// 画面と重なるブロックの範囲(インスタンス番号、描画順)。隣り合う範囲はつなげてある。
    pub fn visible_ranges(&mut self, camera: &Camera) -> &[Range<u32>] {
        self.ranges.clear();
        let (view_min, view_max) = camera.visible_world_rect();
        let m = OUTSET + 2.0 / camera.zoom.max(1e-6);
        let (lo_x, lo_y, hi_x, hi_y) = (
            view_min.x - m,
            view_min.y - m,
            view_max.x + m,
            view_max.y + m,
        );
        let visible = |b: &[f32; 4]| {
            // NaN を含むブロックは念のため描く(頂点シェーダが最終判断する)。
            !(b[2] < lo_x || b[3] < lo_y || b[0] > hi_x || b[1] > hi_y)
        };
        for (i, b) in self.bounds.iter().enumerate() {
            if !visible(b) {
                continue;
            }
            let start = (i * BLOCK) as u32;
            let end = (start + BLOCK as u32).min(self.len);
            match self.ranges.last_mut() {
                Some(r) if r.end == start => r.end = end,
                _ => self.ranges.push(start..end),
            }
        }
        self.limit_ranges();
        &self.ranges
    }

    /// 描画命令が多すぎるときは、隙間の小さい順に範囲をつなげて `MAX_RANGES` 以下にする。
    fn limit_ranges(&mut self) {
        let n = self.ranges.len();
        if n <= MAX_RANGES {
            return;
        }
        self.gaps.clear();
        self.gaps
            .extend(self.ranges.windows(2).map(|w| w[1].start - w[0].end));
        // つなげる隙間の数。小さい隙間から順につなげる。
        let merges = n - MAX_RANGES;
        let mut sorted = self.gaps.clone();
        sorted.sort_unstable();
        let cutoff = sorted[merges - 1];
        // `cutoff` より小さい隙間はすべてつなげ、`cutoff` と等しい隙間は数が足りるまでつなげる。
        let below = self.gaps.iter().filter(|&&g| g < cutoff).count();
        let mut equal_to_merge = merges - below;
        let mut out: Vec<Range<u32>> = Vec::with_capacity(MAX_RANGES);
        for (i, r) in self.ranges.iter().enumerate() {
            let merge = i > 0 && {
                let g = self.gaps[i - 1];
                if g < cutoff {
                    true
                } else if g == cutoff && equal_to_merge > 0 {
                    equal_to_merge -= 1;
                    true
                } else {
                    false
                }
            };
            match out.last_mut() {
                Some(last) if merge => last.end = r.end,
                _ => out.push(r.clone()),
            }
        }
        self.ranges = out;
    }
}

/// 昇順で重ならない `ranges` を `within` に切り詰める(空になったものは除く)。
pub fn clip_ranges(
    ranges: &[Range<u32>],
    within: Range<u32>,
) -> impl Iterator<Item = Range<u32>> + '_ {
    // `within` より前で終わる範囲は二分探索で飛ばす。
    let first = ranges.partition_point(|r| r.end <= within.start);
    ranges[first..]
        .iter()
        .take_while(move |r| r.start < within.end)
        .map(move |r| r.start.max(within.start)..r.end.min(within.end))
        .filter(|r| !r.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::{Board, Vec2};

    fn instances(board: &Board) -> Vec<ShapeInstance> {
        let mut v = Vec::new();
        for e in board.elements() {
            crate::push_instances(e, &mut v);
        }
        v
    }

    fn camera(center: Vec2, zoom: f32) -> Camera {
        let mut c = Camera::new(Vec2::new(1920.0, 1080.0));
        c.zoom = zoom;
        c.origin = center - c.viewport / zoom * 0.5;
        c
    }

    /// 画面に見えるインスタンスは必ずどれかの範囲に入っている。
    #[test]
    fn never_drops_visible_instances() {
        let board = Board::demo(50_000);
        let inst = instances(&board);
        let mut blocks = VisibleBlocks::default();
        blocks.rebuild(&inst);
        let extent = board.bounds().unwrap();
        for (k, zoom) in [1.0f32, 0.3, 0.05, 2.0].into_iter().enumerate() {
            let center = extent.min + (extent.max - extent.min) * (0.2 + 0.2 * k as f32);
            let cam = camera(center, zoom);
            let ranges = blocks.visible_ranges(&cam).to_vec();
            assert!(ranges.len() <= MAX_RANGES);
            let (lo, hi) = cam.visible_world_rect();
            let drawn: u32 = ranges.iter().map(|r| r.end - r.start).sum();
            for (i, s) in inst.iter().enumerate() {
                let b = instance_bounds(s);
                let on_screen = !(b[2] < lo.x || b[3] < lo.y || b[0] > hi.x || b[1] > hi.y);
                if on_screen {
                    let i = i as u32;
                    assert!(
                        ranges.iter().any(|r| r.contains(&i)),
                        "zoom {zoom}: {i} が漏れた"
                    );
                }
            }
            if zoom >= 1.0 {
                // 等倍なら全体のごく一部しか描かない。
                assert!(drawn < inst.len() as u32 / 10, "{drawn}");
            }
            // 範囲は昇順で重ならない(描画順が保たれる)。
            assert!(ranges.windows(2).all(|w| w[0].end < w[1].start));
        }
    }

    #[test]
    fn update_matches_rebuild() {
        let board = Board::demo(3000);
        let mut inst = instances(&board);
        let mut a = VisibleBlocks::default();
        a.rebuild(&inst);
        for s in &mut inst[700..710] {
            s.pos[0] += 50_000.0;
        }
        a.update(&inst, 700..710);
        let mut b = VisibleBlocks::default();
        b.rebuild(&inst);
        assert_eq!(a.bounds, b.bounds);
    }

    #[test]
    fn clip_to_layer() {
        let r = [0..10, 20..30, 40..50];
        let got: Vec<_> = clip_ranges(&r, 5..45).collect();
        assert_eq!(got, vec![5..10, 20..30, 40..45]);
        assert_eq!(clip_ranges(&r, 10..20).count(), 0);
        assert_eq!(clip_ranges(&r, 0..100).count(), 3);
    }

    #[test]
    fn empty_is_fine() {
        let mut b = VisibleBlocks::default();
        b.rebuild(&[]);
        assert!(b.visible_ranges(&camera(Vec2::ZERO, 1.0)).is_empty());
    }
}
