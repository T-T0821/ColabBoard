//! コネクタでつないだ図の自動整列。
//!
//! ボードに描いた図形とコネクタを、矢印の向きに沿って段に並べ直す(簡易な階層レイアウト)。
//!
//! 1. 段: 最長経路で決める(循環は無視)。構成図のレイアウトと同じ `ranks` を使う。
//! 2. 段の中の順番: 今の位置で並べてから、つながった相手の平均位置(重心)で何回か並べ替え、
//!    線の交差を減らす。
//! 3. 位置: 段の中で、つながった相手の中心にできるだけ合わせつつ、重ならないよう詰める。
//!
//! つながっていない固まりは別々に並べて、交差軸の方向に積む。結果は 1 回の Undo で戻せる。

use std::collections::HashMap;

use glam::Vec2;

use crate::board::Board;
use crate::diagram::Direction;
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::ShapeKind;

/// 段と段の間隔(矢印とラベルが入る分)。
pub const RANK_GAP: f32 = 120.0;
/// 同じ段の中の間隔。
pub const NODE_GAP: f32 = 48.0;
/// 固まりどうしの間隔。
const COMPONENT_GAP: f32 = 120.0;
/// 重心での並べ替えの回数(往復)。
const SWEEPS: usize = 4;

/// 並べる対象のノード。
fn is_node(e: &Element) -> bool {
    matches!(e, Element::Shape(s) if s.kind != ShapeKind::Frame) && !e.is_locked()
}

/// 選択から並べる対象(ノードと矢印)を決める。
///
/// 図形を 2 つ以上選んでいればそれらを、1 つ(またはコネクタだけ)なら、
/// それとコネクタでつながった図形全部を対象にする。
pub fn collect(
    board: &Board,
    selection: &[ElementId],
) -> (Vec<ElementId>, Vec<(ElementId, ElementId)>) {
    // コネクタの両端がつながっている組(向き付き)。
    let all_edges: Vec<(ElementId, ElementId)> = board
        .elements()
        .iter()
        .filter_map(|e| match e {
            Element::Connector(c) => {
                let (a, b) = (c.from.attached_to()?, c.to.attached_to()?);
                (a != b).then_some((a, b))
            }
            _ => None,
        })
        .collect();
    let node_ok = |id: &ElementId| board.get(*id).is_some_and(is_node);
    let mut nodes: Vec<ElementId> = selection.iter().copied().filter(node_ok).collect();
    if nodes.len() < 2 {
        // 選んだ図形やコネクタからたどれる図形全部。
        let mut seeds = nodes.clone();
        for id in selection {
            if let Some(Element::Connector(c)) = board.get(*id) {
                seeds.extend(c.from.attached_to());
                seeds.extend(c.to.attached_to());
            }
        }
        let mut seen: Vec<ElementId> = Vec::new();
        let mut stack = seeds;
        while let Some(v) = stack.pop() {
            if seen.contains(&v) || !node_ok(&v) {
                continue;
            }
            seen.push(v);
            for &(a, b) in &all_edges {
                if a == v {
                    stack.push(b);
                } else if b == v {
                    stack.push(a);
                }
            }
        }
        nodes = seen;
    }
    let edges = all_edges
        .into_iter()
        .filter(|(a, b)| nodes.contains(a) && nodes.contains(b))
        .collect();
    (nodes, edges)
}

/// 主軸・交差軸の取り出し。
#[derive(Clone, Copy)]
struct Axes(Direction);

impl Axes {
    fn main(self, v: Vec2) -> f32 {
        match self.0 {
            Direction::Right => v.x,
            Direction::Down => v.y,
        }
    }
    fn cross(self, v: Vec2) -> f32 {
        match self.0 {
            Direction::Right => v.y,
            Direction::Down => v.x,
        }
    }
    fn vec(self, main: f32, cross: f32) -> Vec2 {
        match self.0 {
            Direction::Right => Vec2::new(main, cross),
            Direction::Down => Vec2::new(cross, main),
        }
    }
}

/// 1 つの固まりを並べる。返り値は各ノードの左上(固まりの左上を原点とする)と全体の大きさ。
fn layout_component(rects: &[Rect], edges: &[(usize, usize)], ax: Axes) -> (Vec<Vec2>, Vec2) {
    let n = rects.len();
    let rank = crate::diagram::ranks(n, edges);
    let nranks = rank.iter().copied().max().map_or(0, |m| m + 1);
    let size: Vec<Vec2> = rects.iter().map(Rect::size).collect();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if rank[a] < rank[b] {
            preds[b].push(a);
            succs[a].push(b);
        } else if rank[b] < rank[a] {
            preds[a].push(b);
            succs[b].push(a);
        }
    }

    // 段ごとの並び。最初は今の位置(交差軸)の順。
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); nranks];
    for v in 0..n {
        layers[rank[v]].push(v);
    }
    for l in &mut layers {
        l.sort_by(|&a, &b| {
            ax.cross(rects[a].center())
                .total_cmp(&ax.cross(rects[b].center()))
        });
    }
    let mut order = vec![0usize; n];
    let reindex = |layers: &Vec<Vec<usize>>, order: &mut Vec<usize>| {
        for l in layers {
            for (i, &v) in l.iter().enumerate() {
                order[v] = i;
            }
        }
    };
    reindex(&layers, &mut order);
    for sweep in 0..SWEEPS {
        let down = sweep % 2 == 0;
        let ranks_iter: Vec<usize> = if down {
            (1..nranks).collect()
        } else {
            (0..nranks.saturating_sub(1)).rev().collect()
        };
        for r in ranks_iter {
            let mut keyed: Vec<(f32, usize)> = layers[r]
                .iter()
                .map(|&v| {
                    let nb = if down { &preds[v] } else { &succs[v] };
                    let key = if nb.is_empty() {
                        order[v] as f32
                    } else {
                        nb.iter().map(|&w| order[w] as f32).sum::<f32>() / nb.len() as f32
                    };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
            layers[r] = keyed.into_iter().map(|(_, v)| v).collect();
            reindex(&layers, &mut order);
        }
    }

    // 主軸の位置: 段ごとの最大の厚み。
    let mut rank_start = vec![0.0f32; nranks];
    let mut rank_thick = vec![0.0f32; nranks];
    for v in 0..n {
        rank_thick[rank[v]] = rank_thick[rank[v]].max(ax.main(size[v]));
    }
    let mut acc = 0.0;
    for r in 0..nranks {
        rank_start[r] = acc;
        acc += rank_thick[r] + RANK_GAP;
    }

    // 交差軸の位置(中心)。まず詰めて中央揃え、その後つながった相手に寄せる。
    let mut center = vec![0.0f32; n];
    for l in &layers {
        let total: f32 = l.iter().map(|&v| ax.cross(size[v])).sum::<f32>()
            + NODE_GAP * l.len().saturating_sub(1) as f32;
        let mut c = -total * 0.5;
        for &v in l {
            let s = ax.cross(size[v]);
            center[v] = c + s * 0.5;
            c += s + NODE_GAP;
        }
    }
    for sweep in 0..3 {
        let down = sweep % 2 == 0;
        let ranks_iter: Vec<usize> = if down {
            (1..nranks).collect()
        } else {
            (0..nranks.saturating_sub(1)).rev().collect()
        };
        for r in ranks_iter {
            let l = &layers[r];
            let want: Vec<f32> = l
                .iter()
                .map(|&v| {
                    let nb = if down { &preds[v] } else { &succs[v] };
                    if nb.is_empty() {
                        center[v]
                    } else {
                        nb.iter().map(|&w| center[w]).sum::<f32>() / nb.len() as f32
                    }
                })
                .collect();
            place_in_order(l, &want, &size, ax, &mut center);
        }
    }

    // 左上の位置に直して、全体を原点に寄せる。
    let mut pos: Vec<Vec2> = (0..n)
        .map(|v| {
            let m = rank_start[rank[v]] + (rank_thick[rank[v]] - ax.main(size[v])) * 0.5;
            ax.vec(m, center[v] - ax.cross(size[v]) * 0.5)
        })
        .collect();
    let min = pos
        .iter()
        .fold(Vec2::splat(f32::INFINITY), |m, p| m.min(*p));
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for (p, s) in pos.iter_mut().zip(&size) {
        *p -= min;
        max = max.max(*p + *s);
    }
    (pos, max.max(Vec2::ZERO))
}

/// 段の中の順番を保ったまま、できるだけ `want`(中心)に近く、重ならないよう置く。
fn place_in_order(layer: &[usize], want: &[f32], size: &[Vec2], ax: Axes, center: &mut [f32]) {
    if layer.is_empty() {
        return;
    }
    let half: Vec<f32> = layer.iter().map(|&v| ax.cross(size[v]) * 0.5).collect();
    let mut c: Vec<f32> = want.to_vec();
    // 前から押し出して重なりをなくす。
    for i in 1..c.len() {
        let min = c[i - 1] + half[i - 1] + NODE_GAP + half[i];
        if c[i] < min {
            c[i] = min;
        }
    }
    // 押し出した分のずれを、全体を戻して半分ずつにする。
    let shift = want.iter().zip(&c).map(|(w, c)| w - c).sum::<f32>() / c.len() as f32;
    for (k, &v) in layer.iter().enumerate() {
        center[v] = c[k] + shift;
    }
}

/// 位置の計算だけを行う(ボードは変えない)。返り値は動かす先の左上。
pub fn plan(
    board: &Board,
    nodes: &[ElementId],
    edges: &[(ElementId, ElementId)],
    dir: Direction,
) -> Vec<(ElementId, Vec2)> {
    let ax = Axes(dir);
    let index: HashMap<ElementId, usize> =
        nodes.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let rects: Vec<Rect> = nodes
        .iter()
        .map(|id| {
            board
                .get(*id)
                .map(Element::bounds)
                .unwrap_or_else(|| Rect::new(Vec2::ZERO, Vec2::ZERO))
        })
        .collect();
    let Some(all) = rects.iter().copied().reduce(|a, b| a.union(&b)) else {
        return Vec::new();
    };
    let e: Vec<(usize, usize)> = edges
        .iter()
        .filter_map(|(a, b)| Some((*index.get(a)?, *index.get(b)?)))
        .collect();

    // つながった固まりに分ける。
    let n = nodes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for &(a, b) in &e {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let mut comps: Vec<Vec<usize>> = Vec::new();
    let mut comp_of: HashMap<usize, usize> = HashMap::new();
    for v in 0..n {
        let r = find(&mut parent, v);
        let k = *comp_of.entry(r).or_insert_with(|| {
            comps.push(Vec::new());
            comps.len() - 1
        });
        comps[k].push(v);
    }
    // 固まりは今の位置(交差軸)の順に積む。
    let comp_bounds = |c: &Vec<usize>| {
        c.iter()
            .map(|&v| rects[v])
            .reduce(|a, b| a.union(&b))
            .unwrap_or(all)
    };
    comps.sort_by(|a, b| {
        ax.cross(comp_bounds(a).min)
            .total_cmp(&ax.cross(comp_bounds(b).min))
    });

    let mut out = Vec::with_capacity(n);
    let mut cross_cursor = ax.cross(all.min);
    for comp in &comps {
        let local: HashMap<usize, usize> = comp.iter().enumerate().map(|(i, &v)| (v, i)).collect();
        let ce: Vec<(usize, usize)> = e
            .iter()
            .filter_map(|(a, b)| Some((*local.get(a)?, *local.get(b)?)))
            .collect();
        let cr: Vec<Rect> = comp.iter().map(|&v| rects[v]).collect();
        let (pos, size) = layout_component(&cr, &ce, ax);
        let origin = ax.vec(ax.main(all.min), cross_cursor);
        for (k, &v) in comp.iter().enumerate() {
            out.push((nodes[v], origin + pos[k]));
        }
        cross_cursor += ax.cross(size) + COMPONENT_GAP;
    }
    out
}

/// 選択(または選択からつながった図)を自動で整列する。動かした図形の数を返す。
pub fn auto_layout(board: &mut Board, selection: &[ElementId], dir: Direction) -> usize {
    let (nodes, edges) = collect(board, selection);
    if nodes.len() < 2 {
        return 0;
    }
    let moves = plan(board, &nodes, &edges, dir);
    crate::tidy::move_to(board, &moves);
    moves.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgba8;
    use crate::connector::{Connector, Endpoint};
    use crate::shape::Shape;

    fn rect(b: &mut Board, x: f32, y: f32) -> ElementId {
        let id = b.alloc_id();
        b.push(Shape::rect(
            id,
            Vec2::new(x, y),
            Vec2::new(100.0, 60.0),
            Rgba8::WHITE,
        ))
    }

    fn link(b: &mut Board, a: ElementId, z: ElementId) -> ElementId {
        let id = b.alloc_id();
        let at = |id| Endpoint::Attached { id, anchor: None };
        b.push(Connector::new(id, at(a), at(z)))
    }

    fn pos(b: &Board, id: ElementId) -> Vec2 {
        b.get(id).unwrap().bounds().min
    }

    #[test]
    fn chain_goes_left_to_right_from_one_shape() {
        let mut b = Board::new();
        let a = rect(&mut b, 500.0, 500.0);
        let m = rect(&mut b, 0.0, 300.0);
        let z = rect(&mut b, 250.0, 0.0);
        link(&mut b, a, m);
        link(&mut b, m, z);
        // 1 つ選ぶだけで、つながった図全体が対象。
        assert_eq!(auto_layout(&mut b, &[m], Direction::Right), 3);
        assert!(pos(&b, a).x < pos(&b, m).x && pos(&b, m).x < pos(&b, z).x);
        assert_eq!(pos(&b, a).y, pos(&b, m).y);
        assert_eq!(pos(&b, m).y, pos(&b, z).y);
        assert_eq!(pos(&b, m).x - pos(&b, a).x, 100.0 + RANK_GAP);
        // 左上は元の範囲の左上。
        assert_eq!(pos(&b, a), Vec2::new(0.0, 0.0));
        // 1 回の Undo で戻る。
        b.undo();
        assert_eq!(pos(&b, a), Vec2::new(500.0, 500.0));
    }

    #[test]
    fn tree_downward_centers_parent_and_avoids_overlap() {
        let mut b = Board::new();
        let root = rect(&mut b, 0.0, 0.0);
        let kids: Vec<_> = (0..3)
            .map(|i| rect(&mut b, 40.0 * i as f32, 40.0))
            .collect();
        for k in &kids {
            link(&mut b, root, *k);
        }
        auto_layout(&mut b, &[root], Direction::Down);
        let ys: Vec<f32> = kids.iter().map(|k| pos(&b, *k).y).collect();
        assert!(ys.iter().all(|y| *y == ys[0] && *y > pos(&b, root).y));
        let mut xs: Vec<f32> = kids.iter().map(|k| pos(&b, *k).x).collect();
        xs.sort_by(f32::total_cmp);
        assert!(
            xs.windows(2)
                .all(|w| w[1] - w[0] >= 100.0 + NODE_GAP - 0.01)
        );
        // 親は子の真ん中。
        let mid = (xs[0] + xs[2]) * 0.5;
        assert!((pos(&b, root).x - mid).abs() < 0.01);
    }

    #[test]
    fn barycenter_removes_crossing() {
        // a1→b2, a2→b1 が交差する並びから始めても、整列後は交差しない。
        let mut b = Board::new();
        let a1 = rect(&mut b, 0.0, 0.0);
        let a2 = rect(&mut b, 0.0, 200.0);
        let b1 = rect(&mut b, 300.0, 0.0);
        let b2 = rect(&mut b, 300.0, 200.0);
        link(&mut b, a1, b2);
        link(&mut b, a2, b1);
        let c1 = rect(&mut b, 600.0, 0.0);
        link(&mut b, b1, c1);
        link(&mut b, b2, c1);
        auto_layout(&mut b, &[a1, a2, b1, b2, c1], Direction::Right);
        let (ya1, ya2) = (pos(&b, a1).y, pos(&b, a2).y);
        let (yb1, yb2) = (pos(&b, b1).y, pos(&b, b2).y);
        assert_eq!(ya1 < ya2, yb2 < yb1);
    }

    #[test]
    fn separate_components_stack_and_cycles_are_ok() {
        let mut b = Board::new();
        let a = rect(&mut b, 0.0, 0.0);
        let z = rect(&mut b, 0.0, 100.0);
        link(&mut b, a, z);
        link(&mut b, z, a);
        let lone = rect(&mut b, 0.0, 500.0);
        assert_eq!(auto_layout(&mut b, &[a, z, lone], Direction::Right), 3);
        let ra = b
            .get(a)
            .unwrap()
            .bounds()
            .union(&b.get(z).unwrap().bounds());
        assert!(pos(&b, lone).y >= ra.max.y + COMPONENT_GAP - 0.01);
    }

    #[test]
    fn nothing_to_do_for_single_unconnected_shape() {
        let mut b = Board::new();
        let a = rect(&mut b, 0.0, 0.0);
        assert_eq!(auto_layout(&mut b, &[a], Direction::Right), 0);
    }
}
