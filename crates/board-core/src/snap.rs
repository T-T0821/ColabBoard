//! スマートガイドとスナップ。
//!
//! 動かしている要素(またはリサイズ中の辺)を、周りの要素の端・中心にそろう位置や、
//! 隣どうしの間隔が等しくなる位置に吸い付ける。そろった所にはガイド線を出す。
//!
//! 位置の計算は純粋な関数にしてあり、エディタはドラッグ中に呼ぶだけ。

use glam::Vec2;

use crate::board::Board;
use crate::element::{Element, ElementId};
use crate::geom::Rect;

/// 吸い付く距離(スクリーン px)。
pub const SNAP_PX: f32 = 6.0;
/// 周りの要素として調べる数の上限(画面の中心に近い順)。
const MAX_OTHERS: usize = 400;
/// 位置が同じとみなす誤差(ワールド単位)。
const EPS: f32 = 0.01;

/// 画面に出すガイド。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Guide {
    /// 端や中心がそろった線(ワールド座標の線分)。
    Line { a: Vec2, b: Vec2 },
    /// 等しい間隔(線分の両端に短い縦棒を付けて描く)。`vertical` は縦向きの間隔か。
    Gap { a: Vec2, b: Vec2, vertical: bool },
}

/// スナップの結果。`offset` を足すと吸い付いた位置になる。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snap {
    pub offset: Vec2,
    pub guides: Vec<Guide>,
}

/// 軸ごとに使う値(x 軸なら左・中心・右)。
fn lines(r: &Rect, axis: usize) -> [f32; 3] {
    [r.min[axis], r.center()[axis], r.max[axis]]
}

/// 1 つの軸で一番近い候補(ずらす量)。
#[derive(Clone, Copy)]
struct Best {
    delta: f32,
}

fn consider(best: &mut Option<Best>, delta: f32, threshold: f32) {
    if delta.abs() <= threshold && best.is_none_or(|b| delta.abs() < b.delta.abs()) {
        *best = Some(Best { delta });
    }
}

/// 2 つの区間が重なっているか。
fn overlaps(a0: f32, a1: f32, b0: f32, b1: f32) -> bool {
    a0 < b1 && b0 < a1
}

/// 間隔をそろえる候補を `axis` 方向について探す。`best` を更新する。
fn spacing_candidates(
    moving: &Rect,
    others: &[Rect],
    axis: usize,
    threshold: f32,
    best: &mut Option<Best>,
) {
    let cross = 1 - axis;
    // 動かす要素と(交差軸方向で)重なる要素だけを、主軸方向に並べる。
    let mut row: Vec<&Rect> = others
        .iter()
        .filter(|o| {
            overlaps(
                o.min[cross],
                o.max[cross],
                moving.min[cross],
                moving.max[cross],
            )
        })
        .collect();
    row.sort_by(|a, b| a.min[axis].total_cmp(&b.min[axis]));
    let size = moving.max[axis] - moving.min[axis];
    // 間にはさまる: 左の要素と右の要素から等距離。
    for (i, l) in row.iter().enumerate() {
        for r in &row[i + 1..] {
            let space = r.min[axis] - l.max[axis];
            if space > size {
                let target = l.max[axis] + (space - size) * 0.5;
                consider(best, target - moving.min[axis], threshold);
            }
        }
    }
    // 並んだ 2 つと同じ間隔で、その先に置く。
    for w in row.windows(2) {
        let (a, b) = (w[0], w[1]);
        let gap = b.min[axis] - a.max[axis];
        if gap <= 0.0 {
            continue;
        }
        consider(best, b.max[axis] + gap - moving.min[axis], threshold);
        consider(best, a.min[axis] - gap - moving.max[axis], threshold);
    }
}

/// 位置が決まった後で、そろっている線のガイドを作る。
fn line_guides(moving: &Rect, others: &[Rect], axis: usize, out: &mut Vec<Guide>) {
    let cross = 1 - axis;
    for v in lines(moving, axis) {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for o in others {
            if lines(o, axis).iter().any(|w| (w - v).abs() < EPS) {
                lo = lo.min(o.min[cross]);
                hi = hi.max(o.max[cross]);
            }
        }
        if lo <= hi {
            lo = lo.min(moving.min[cross]);
            hi = hi.max(moving.max[cross]);
            let mut a = Vec2::ZERO;
            let mut b = Vec2::ZERO;
            a[axis] = v;
            b[axis] = v;
            a[cross] = lo;
            b[cross] = hi;
            out.push(Guide::Line { a, b });
        }
    }
}

/// 位置が決まった後で、等しい間隔のガイドを作る。
fn gap_guides(moving: &Rect, others: &[Rect], axis: usize, out: &mut Vec<Guide>) {
    let cross = 1 - axis;
    let mut row: Vec<Rect> = others
        .iter()
        .filter(|o| {
            overlaps(
                o.min[cross],
                o.max[cross],
                moving.min[cross],
                moving.max[cross],
            )
        })
        .copied()
        .collect();
    row.push(*moving);
    row.sort_by(|a, b| a.min[axis].total_cmp(&b.min[axis]));
    let Some(k) = row.iter().position(|r| r == moving) else {
        return;
    };
    // 隣り合う組の間隔。重なっている組は間隔なし。
    let gap = |i: usize| -> Option<f32> {
        let (a, b) = (row.get(i)?, row.get(i + 1)?);
        let g = b.min[axis] - a.max[axis];
        (g > 0.0).then_some(g)
    };
    let mut push = |i: usize| {
        let (a, b) = (&row[i], &row[i + 1]);
        let lo = a.min[cross].max(b.min[cross]);
        let hi = a.max[cross].min(b.max[cross]);
        let c = (lo + hi) * 0.5;
        let mut p = Vec2::ZERO;
        let mut q = Vec2::ZERO;
        p[axis] = a.max[axis];
        q[axis] = b.min[axis];
        p[cross] = c;
        q[cross] = c;
        out.push(Guide::Gap {
            a: p,
            b: q,
            vertical: axis == 1,
        });
    };
    let near = |a: Option<f32>, b: Option<f32>| matches!((a, b), (Some(a), Some(b)) if (a - b).abs() < 0.05);
    let before = k.checked_sub(1).and_then(gap);
    let after = gap(k);
    let before2 = k.checked_sub(2).and_then(gap);
    let after2 = gap(k + 1);
    if near(before, after) {
        push(k - 1);
        push(k);
    } else if near(before, before2) {
        push(k - 2);
        push(k - 1);
    } else if near(after, after2) {
        push(k);
        push(k + 1);
    }
}

/// `moving` を周りの `others` に吸い付ける。`threshold` はワールド単位。
pub fn snap_move(moving: Rect, others: &[Rect], threshold: f32) -> Snap {
    let mut offset = Vec2::ZERO;
    for axis in 0..2 {
        let mut best: Option<Best> = None;
        let mine = lines(&moving, axis);
        for o in others {
            for theirs in lines(o, axis) {
                for m in mine {
                    consider(&mut best, theirs - m, threshold);
                }
            }
        }
        spacing_candidates(&moving, others, axis, threshold, &mut best);
        if let Some(b) = best {
            offset[axis] = b.delta;
        }
    }
    let placed = Rect::new(moving.min + offset, moving.max + offset);
    let mut guides = Vec::new();
    for axis in 0..2 {
        line_guides(&placed, others, axis, &mut guides);
        gap_guides(&placed, others, axis, &mut guides);
    }
    Snap { offset, guides }
}

/// リサイズ中の辺を吸い付ける。`edges` は [左, 上, 右, 下] のうち動いている辺。
/// 戻り値は吸い付いた矩形とガイド。
pub fn snap_resize(
    target: Rect,
    edges: [bool; 4],
    others: &[Rect],
    threshold: f32,
) -> (Rect, Vec<Guide>) {
    let mut r = target;
    for (i, moving) in edges.iter().enumerate() {
        if !moving {
            continue;
        }
        let axis = i % 2;
        let max_side = i >= 2;
        let v = if max_side { r.max[axis] } else { r.min[axis] };
        let mut best: Option<Best> = None;
        for o in others {
            for theirs in lines(o, axis) {
                consider(&mut best, theirs - v, threshold);
            }
        }
        if let Some(b) = best {
            let nv = v + b.delta;
            // 反対側の辺を越えない。
            if max_side && nv > r.min[axis] + 1.0 {
                r.max[axis] = nv;
            } else if !max_side && nv < r.max[axis] - 1.0 {
                r.min[axis] = nv;
            }
        }
    }
    let mut guides = Vec::new();
    for axis in 0..2 {
        // 動いた辺だけのガイドを出す。
        let mut all = Vec::new();
        line_guides(&r, others, axis, &mut all);
        for g in all {
            if let Guide::Line { a, .. } = g {
                let v = a[axis];
                let lo_moved = edges[axis] && (v - r.min[axis]).abs() < EPS;
                let hi_moved = edges[axis + 2] && (v - r.max[axis]).abs() < EPS;
                if lo_moved || hi_moved {
                    guides.push(g);
                }
            }
        }
    }
    (r, guides)
}

/// 吸い付く相手として使う要素の外接矩形。`area` の中にあって `except` に含まれないもの。
/// コネクタと手書き線は相手にしない。多すぎるときは `area` の中心に近いものを残す。
pub fn targets(board: &Board, area: &Rect, except: &[ElementId]) -> Vec<Rect> {
    let mut out: Vec<Rect> = board
        .query_visible(area)
        .into_iter()
        .filter(|e| matches!(e, Element::Shape(_)))
        .filter(|e| !except.contains(&e.id()))
        .map(Element::bounds)
        .collect();
    if out.len() > MAX_OTHERS {
        let c = area.center();
        out.sort_by(|a, b| {
            a.center()
                .distance_squared(c)
                .total_cmp(&b.center().distance_squared(c))
        });
        out.truncate(MAX_OTHERS);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(Vec2::new(x, y), Vec2::new(x + w, y + h))
    }

    #[test]
    fn snaps_left_edges_and_shows_guide() {
        let others = [r(0.0, 0.0, 100.0, 50.0)];
        let s = snap_move(r(4.0, 200.0, 80.0, 40.0), &others, 6.0);
        assert_eq!(s.offset, Vec2::new(-4.0, 0.0));
        assert!(s.guides.iter().any(|g| matches!(g,
            Guide::Line { a, b } if a.x == 0.0 && b.x == 0.0 && a.y == 0.0 && b.y == 240.0)));
    }

    #[test]
    fn snaps_centers() {
        let others = [r(0.0, 0.0, 100.0, 100.0)];
        // 中心 y = 50 に、動かす要素の中心(y=53)を合わせる。
        let s = snap_move(r(300.0, 33.0, 40.0, 40.0), &others, 6.0);
        assert_eq!(s.offset.y, -3.0);
        assert_eq!(s.offset.x, 0.0);
    }

    #[test]
    fn far_away_does_not_snap() {
        let others = [r(0.0, 0.0, 100.0, 100.0)];
        let s = snap_move(r(300.0, 320.0, 40.0, 40.0), &others, 6.0);
        assert_eq!(s.offset, Vec2::ZERO);
        assert!(s.guides.is_empty());
    }

    #[test]
    fn equal_spacing_between_two() {
        // 0..100 と 300..400 の間に幅 100 の要素 → 100..200 の間隔 100 ずつ… ではなく中央の 150..250。
        let others = [r(0.0, 0.0, 100.0, 50.0), r(300.0, 0.0, 100.0, 50.0)];
        let s = snap_move(r(147.0, 0.0, 100.0, 50.0), &others, 6.0);
        assert_eq!(s.offset.x, 3.0);
        let gaps = s
            .guides
            .iter()
            .filter(|g| matches!(g, Guide::Gap { .. }))
            .count();
        assert_eq!(gaps, 2);
    }

    #[test]
    fn equal_spacing_continues_a_row() {
        // 間隔 50 で並んだ 2 つの右に、同じ間隔で置く(400 から)。
        let others = [r(0.0, 0.0, 100.0, 50.0), r(150.0, 0.0, 100.0, 50.0)];
        let s = snap_move(r(304.0, 0.0, 80.0, 50.0), &others, 6.0);
        assert_eq!(s.offset.x, -4.0);
        assert!(s.guides.iter().any(|g| matches!(g, Guide::Gap { .. })));
    }

    #[test]
    fn resize_snaps_only_moving_edge() {
        let others = [r(0.0, 0.0, 200.0, 50.0)];
        let (out, guides) = snap_resize(
            r(0.0, 100.0, 196.0, 50.0),
            [false, false, true, false],
            &others,
            6.0,
        );
        assert_eq!(out, r(0.0, 100.0, 200.0, 50.0));
        // 動いた右端のガイドだけ(左端はもともとそろっているが出さない)。
        assert_eq!(guides.len(), 1);
    }
}
