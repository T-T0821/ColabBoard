//! 図の自動レイアウト。
//!
//! 1. グループの中身(ノードと子グループ)を内側から順に並べる。兄弟の項目どうしは、
//!    中身のノードの間の矢印を「項目 → 項目」の矢印とみなして段(最長経路、循環は無視)
//!    を決め、段ごとの列に定義順で積む。グループの大きさは中身で決まる。
//! 2. 外側から位置を確定する。矢印はノードにつないだコネクタなので経路はボードが引く。

use std::collections::HashMap;

use glam::Vec2;

use super::catalog::{self, GroupKind};
use super::{DiagramSpec, Direction, EdgeLine, GROUP_FONT, layout_shape, text_width};
use crate::geom::Rect;
use crate::shape::{FRAME_TITLE_HEIGHT, Shape};

/// 列と列の間隔(矢印とラベルが入る分)。
const COL_GAP: f32 = 110.0;
/// 同じ列の中の間隔(下の枠の見出しが入る分)。
const ROW_GAP: f32 = FRAME_TITLE_HEIGHT + 24.0;
/// 列をまたぐ矢印のために空ける通り道の最小の幅。
const LANE: f32 = 40.0;
/// グループの内側の余白。
const GROUP_PAD: f32 = 32.0;
/// グループ上端の余白(中の枠の見出しが入る分)。
const GROUP_HEADER: f32 = FRAME_TITLE_HEIGHT + 20.0;

pub struct PlacedGroup {
    pub kind: &'static GroupKind,
    pub label: String,
    pub rect: Rect,
    pub depth: usize,
}

pub struct PlacedNode {
    pub id: String,
    /// 位置確定済みの図形(要素 ID は仮)。
    pub shape: Shape,
}

pub struct PlacedEdge {
    /// `Layout::nodes` の添字。
    pub from: usize,
    pub to: usize,
    pub label: String,
    pub line: Option<EdgeLine>,
}

/// レイアウト結果。グループは外側から順に並ぶ。
pub struct Layout {
    pub groups: Vec<PlacedGroup>,
    pub nodes: Vec<PlacedNode>,
    pub edges: Vec<PlacedEdge>,
    pub bounds: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Node(usize),
    Group(usize),
}

struct Ctx<'a> {
    dir: Direction,
    spec: &'a DiagramSpec,
    /// 矢印(ノードの添字の組)。
    edges: Vec<(usize, usize)>,
    node_size: Vec<Vec2>,
    group_children: Vec<Vec<Item>>,
    /// グループの大きさと、中身の(グループ左上からの)位置。
    group_size: Vec<Vec2>,
    group_child_pos: Vec<Vec<Vec2>>,
}

impl Ctx<'_> {
    fn main(&self, v: Vec2) -> f32 {
        match self.dir {
            Direction::Right => v.x,
            Direction::Down => v.y,
        }
    }

    fn cross(&self, v: Vec2) -> f32 {
        match self.dir {
            Direction::Right => v.y,
            Direction::Down => v.x,
        }
    }

    fn vec(&self, main: f32, cross: f32) -> Vec2 {
        match self.dir {
            Direction::Right => Vec2::new(main, cross),
            Direction::Down => Vec2::new(cross, main),
        }
    }

    fn size(&self, item: Item) -> Vec2 {
        match item {
            Item::Node(i) => self.node_size[i],
            Item::Group(g) => self.group_size[g],
        }
    }

    /// 項目に含まれるノード。
    fn collect_nodes(&self, item: Item, out: &mut Vec<usize>) {
        match item {
            Item::Node(i) => out.push(i),
            Item::Group(g) => {
                for c in &self.group_children[g] {
                    self.collect_nodes(*c, out);
                }
            }
        }
    }

    /// 兄弟の項目どうしの段。中身のノードの間に矢印があれば「項目 → 項目」の矢印とみなし、
    /// 最長経路で決める(グループの外から入ってくる矢印は外側の段で考慮済み)。
    fn sibling_ranks(&self, items: &[Item]) -> (Vec<usize>, Vec<(usize, usize)>) {
        let mut owner: HashMap<usize, usize> = HashMap::new();
        for (k, item) in items.iter().enumerate() {
            let mut nodes = Vec::new();
            self.collect_nodes(*item, &mut nodes);
            for n in nodes {
                owner.insert(n, k);
            }
        }
        let mut edges: Vec<(usize, usize)> = self
            .edges
            .iter()
            .filter_map(|(a, b)| {
                let (ia, ib) = (*owner.get(a)?, *owner.get(b)?);
                (ia != ib).then_some((ia, ib))
            })
            .collect();
        edges.sort_unstable();
        edges.dedup();
        (ranks(items.len(), &edges), edges)
    }

    /// 項目を列に並べる。返り値は各項目の左上位置と全体の大きさ。
    fn arrange(&self, items: &[Item]) -> (Vec<Vec2>, Vec2) {
        let (mut ranks, edges) = self.sibling_ranks(items);
        let mut cols: Vec<usize> = ranks.clone();
        cols.sort_unstable();
        cols.dedup();
        for r in &mut ranks {
            *r = cols.binary_search(r).unwrap_or(0);
        }
        let ncols = cols.len();
        let mut col_main = vec![0.0f32; ncols];
        let mut col_cross = vec![0.0f32; ncols];
        let mut col_count = vec![0usize; ncols];
        for (item, &c) in items.iter().zip(&ranks) {
            let s = self.size(*item);
            col_main[c] = col_main[c].max(self.main(s));
            col_cross[c] += self.cross(s);
            col_count[c] += 1;
        }
        // 列を 2 つ以上またぐ矢印は、間の列に通り道を空ける(ノードの上を通らないように)。
        // 通り道は列の後ろ側に取るので、間の列のノードは脇にずれる。
        let mut lanes = vec![0usize; ncols];
        for &(a, b) in &edges {
            let (ca, cb) = (ranks[a], ranks[b]);
            for lane in lanes.iter_mut().take(cb.max(ca)).skip(ca.min(cb) + 1) {
                *lane += 1;
            }
        }
        // 通り道の幅は列のいちばん大きい項目と同じにする(ノードが矢印から完全に外れるように)。
        let mut col_widest = vec![LANE; ncols];
        for (item, &c) in items.iter().zip(&ranks) {
            col_widest[c] = col_widest[c].max(self.cross(self.size(*item)));
        }
        for c in 0..ncols {
            let slots = col_count[c] + lanes[c];
            col_cross[c] +=
                col_widest[c] * lanes[c] as f32 + ROW_GAP * slots.saturating_sub(1) as f32;
        }
        let total_cross = col_cross.iter().copied().fold(0.0, f32::max);
        let mut col_start = vec![0.0f32; ncols];
        let mut acc = 0.0;
        for c in 0..ncols {
            col_start[c] = acc;
            acc += col_main[c] + COL_GAP;
        }
        let total_main = (acc - COL_GAP).max(0.0);

        // 列ごとに中央揃えで積む。
        let mut cursor: Vec<f32> = (0..ncols)
            .map(|c| (total_cross - col_cross[c]) * 0.5)
            .collect();
        let pos = items
            .iter()
            .zip(&ranks)
            .map(|(item, &c)| {
                let s = self.size(*item);
                let m = col_start[c] + (col_main[c] - self.main(s)) * 0.5;
                let x = cursor[c];
                cursor[c] += self.cross(s) + ROW_GAP;
                self.vec(m, x)
            })
            .collect();
        (pos, self.vec(total_main, total_cross))
    }

    /// グループの大きさを内側から決める。
    fn measure(&mut self, g: usize) {
        for c in self.group_children[g].clone() {
            if let Item::Group(cg) = c {
                self.measure(cg);
            }
        }
        let (pos, content) = self.arrange(&self.group_children[g].clone());
        let offset = Vec2::new(GROUP_PAD, GROUP_HEADER);
        self.group_child_pos[g] = pos.into_iter().map(|p| p + offset).collect();
        let kind = catalog::group_kind(&self.spec.groups[g].kind);
        let title = super::group_title(kind, &self.spec.groups[g].label);
        let min_w = text_width(&title, GROUP_FONT) + 40.0;
        let size = content + Vec2::new(GROUP_PAD * 2.0, GROUP_HEADER + GROUP_PAD);
        self.group_size[g] = Vec2::new(size.x.max(min_w).max(160.0), size.y.max(120.0));
        // 幅を広げた分、中身を中央へ寄せる。
        let dx = (self.group_size[g].x - (content.x + GROUP_PAD * 2.0)) * 0.5;
        if dx > 0.0 {
            for p in &mut self.group_child_pos[g] {
                p.x += dx;
            }
        }
    }
}

impl Layout {
    pub fn compute(spec: &DiagramSpec) -> Layout {
        let group_index: HashMap<&str, usize> = spec
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| (g.id.as_str(), i))
            .collect();
        let node_index: HashMap<&str, usize> = spec
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let lookup_group = |id: &Option<String>| -> Option<usize> {
            id.as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|s| group_index.get(s).copied())
        };

        // 親子関係(循環していたら外側に出す)。
        let ng = spec.groups.len();
        let mut parent: Vec<Option<usize>> = spec
            .groups
            .iter()
            .map(|g| lookup_group(&g.parent))
            .collect();
        for g in 0..ng {
            let mut cur = parent[g];
            let mut steps = 0;
            while let Some(p) = cur {
                if p == g || steps > ng {
                    parent[g] = None;
                    break;
                }
                cur = parent[p];
                steps += 1;
            }
        }
        let depth: Vec<usize> = (0..ng)
            .map(|g| {
                let mut d = 0;
                let mut cur = parent[g];
                while let Some(p) = cur {
                    d += 1;
                    cur = parent[p];
                }
                d
            })
            .collect();

        let mut group_children: Vec<Vec<Item>> = vec![Vec::new(); ng];
        let mut root: Vec<Item> = Vec::new();
        for (g, p) in parent.iter().enumerate() {
            match *p {
                Some(p) => group_children[p].push(Item::Group(g)),
                None => root.push(Item::Group(g)),
            }
        }
        // 同じ ID のノードは最後のものだけ使う。
        let mut nodes_used: Vec<usize> = Vec::new();
        for (i, n) in spec.nodes.iter().enumerate() {
            if node_index.get(n.id.as_str()) != Some(&i) && !n.id.is_empty() {
                continue;
            }
            nodes_used.push(i);
            match lookup_group(&n.group) {
                Some(g) => group_children[g].push(Item::Node(i)),
                None => root.push(Item::Node(i)),
            }
        }

        // 矢印(存在するノード間のみ)。
        let edges: Vec<(usize, usize, &super::EdgeSpec)> = spec
            .edges
            .iter()
            .filter_map(|e| {
                let a = *node_index.get(e.from.as_str())?;
                let b = *node_index.get(e.to.as_str())?;
                (a != b).then_some((a, b, e))
            })
            .collect();
        let node_shapes: Vec<Shape> = spec.nodes.iter().map(layout_shape).collect();

        let mut ctx = Ctx {
            dir: spec.direction,
            spec,
            edges: edges.iter().map(|&(a, b, _)| (a, b)).collect(),
            node_size: node_shapes.iter().map(|s| s.size).collect(),
            group_children,
            group_size: vec![Vec2::ZERO; ng],
            group_child_pos: vec![Vec::new(); ng],
        };
        for item in &root {
            if let Item::Group(g) = item {
                ctx.measure(*g);
            }
        }
        let (root_pos, root_size) = ctx.arrange(&root);

        // 外側から絶対位置を決める。
        let mut group_rect: Vec<Option<Rect>> = vec![None; ng];
        let mut node_pos: Vec<Option<Vec2>> = vec![None; spec.nodes.len()];
        let mut stack: Vec<(Item, Vec2)> = root.iter().copied().zip(root_pos).collect();
        while let Some((item, pos)) = stack.pop() {
            match item {
                Item::Node(i) => node_pos[i] = Some(pos),
                Item::Group(g) => {
                    group_rect[g] = Some(Rect::new(pos, pos + ctx.group_size[g]));
                    for (c, p) in ctx.group_children[g].iter().zip(&ctx.group_child_pos[g]) {
                        stack.push((*c, pos + *p));
                    }
                }
            }
        }

        let mut groups: Vec<PlacedGroup> = (0..ng)
            .filter_map(|g| {
                Some(PlacedGroup {
                    kind: catalog::group_kind(&spec.groups[g].kind),
                    label: spec.groups[g].label.clone(),
                    rect: group_rect[g]?,
                    depth: depth[g],
                })
            })
            .collect();
        groups.sort_by_key(|g| g.depth);

        let nodes: Vec<PlacedNode> = nodes_used
            .iter()
            .filter_map(|&i| {
                let mut shape = node_shapes[i].clone();
                shape.pos = node_pos[i]?;
                Some(PlacedNode {
                    id: spec.nodes[i].id.clone(),
                    shape,
                })
            })
            .collect();
        let placed_index: HashMap<usize, usize> = nodes_used
            .iter()
            .enumerate()
            .filter(|(_, i)| node_pos[**i].is_some())
            .map(|(k, &i)| (i, k))
            .collect();
        let edges = edges
            .iter()
            .filter_map(|&(a, b, e)| {
                Some(PlacedEdge {
                    from: *placed_index.get(&a)?,
                    to: *placed_index.get(&b)?,
                    label: e.label.clone(),
                    line: e.line,
                })
            })
            .collect();

        // いちばん外側の枠の見出しも含める。
        let top = if root.iter().any(|i| matches!(i, Item::Group(_))) {
            FRAME_TITLE_HEIGHT
        } else {
            0.0
        };
        Layout {
            groups,
            nodes,
            edges,
            bounds: Rect::new(Vec2::new(0.0, -top), root_size),
        }
    }

    /// ノード ID からレイアウト後の図形を引く(テスト・デバッグ用)。
    pub fn node(&self, id: &str) -> Option<&Shape> {
        self.nodes.iter().find(|n| n.id == id).map(|n| &n.shape)
    }
}

/// 最長経路で段を決める。循環を作る矢印(深さ優先で見つかる後退辺)は無視する。
pub(crate) fn ranks(n: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut out_edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        out_edges[a].push(b);
    }
    // 0: 未訪問, 1: 訪問中, 2: 完了
    let mut state = vec![0u8; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut keep: Vec<Vec<usize>> = vec![Vec::new(); n];
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        // 再帰を使わない深さ優先探索。
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        state[start] = 1;
        while let Some(&mut (v, ref mut next)) = stack.last_mut() {
            if let Some(&w) = out_edges[v].get(*next) {
                *next += 1;
                match state[w] {
                    0 => {
                        keep[v].push(w);
                        state[w] = 1;
                        stack.push((w, 0));
                    }
                    2 => keep[v].push(w),
                    _ => {} // 後退辺(循環)
                }
            } else {
                state[v] = 2;
                order.push(v);
                stack.pop();
            }
        }
    }
    // `order` は帰りがけ順なので、逆順がトポロジカル順。
    let mut rank = vec![0usize; n];
    for &v in order.iter().rev() {
        for &w in &keep[v] {
            rank[w] = rank[w].max(rank[v] + 1);
        }
    }
    rank
}

#[cfg(test)]
mod tests {
    use super::super::{EdgeSpec, GroupSpec, NodeSpec, samples};
    use super::*;

    fn node(id: &str, group: Option<&str>) -> NodeSpec {
        NodeSpec {
            id: id.into(),
            kind: "process".into(),
            label: id.into(),
            group: group.map(Into::into),
            ..Default::default()
        }
    }

    fn edge(a: &str, b: &str) -> EdgeSpec {
        EdgeSpec {
            from: a.into(),
            to: b.into(),
            label: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn ranks_follow_edges_and_ignore_cycles() {
        let e = [(0, 1), (1, 2), (2, 0), (0, 2)];
        let r = ranks(3, &e);
        assert_eq!(r, vec![0, 1, 2]);
    }

    #[test]
    fn chain_flows_in_direction() {
        let mut spec = DiagramSpec {
            nodes: vec![node("a", None), node("b", None), node("c", None)],
            edges: vec![edge("a", "b"), edge("b", "c")],
            ..Default::default()
        };
        let l = Layout::compute(&spec);
        let (a, b, c) = (
            l.node("a").unwrap(),
            l.node("b").unwrap(),
            l.node("c").unwrap(),
        );
        assert!(a.pos.x < b.pos.x && b.pos.x < c.pos.x);
        assert_eq!(a.bounds().center().y, c.bounds().center().y);
        assert_eq!(l.edges.len(), 2);

        spec.direction = Direction::Down;
        let l = Layout::compute(&spec);
        assert!(l.node("a").unwrap().pos.y < l.node("c").unwrap().pos.y);
    }

    #[test]
    fn groups_contain_their_members() {
        let spec = samples::aws_three_tier();
        let l = Layout::compute(&spec);
        let rect_of = |id: &str| {
            let i = spec.groups.iter().position(|g| g.id == id).unwrap();
            let kind = catalog::group_kind(&spec.groups[i].kind);
            l.groups
                .iter()
                .find(|g| std::ptr::eq(g.kind, kind) && g.label == spec.groups[i].label)
                .unwrap()
                .rect
        };
        for n in &spec.nodes {
            if let Some(g) = &n.group {
                let r = rect_of(g);
                assert!(
                    r.contains_rect(&l.node(&n.id).unwrap().bounds()),
                    "{} in {g}",
                    n.id
                );
            }
        }
        for g in &spec.groups {
            if let Some(p) = &g.parent {
                assert!(rect_of(p).contains_rect(&rect_of(&g.id)), "{} in {p}", g.id);
            }
        }
        // 外側の枠から順に並ぶ。
        assert!(l.groups.windows(2).all(|w| w[0].depth <= w[1].depth));
    }

    #[test]
    fn siblings_do_not_overlap() {
        let spec = samples::aws_three_tier();
        let l = Layout::compute(&spec);
        for (i, a) in l.nodes.iter().enumerate() {
            for b in &l.nodes[i + 1..] {
                let (ra, rb) = (a.shape.bounds(), b.shape.bounds());
                let overlap = ra.min.cmplt(rb.max).all() && rb.min.cmplt(ra.max).all();
                assert!(!overlap, "{} / {}", a.id, b.id);
            }
        }
    }

    #[test]
    fn isolated_nodes_do_not_pull_groups_upstream() {
        // サブネットに矢印のない NAT Gateway があっても、VPC は利用者より右に来る。
        let spec = DiagramSpec {
            groups: vec![
                GroupSpec {
                    id: "vpc".into(),
                    kind: "vpc".into(),
                    ..Default::default()
                },
                GroupSpec {
                    id: "pub".into(),
                    kind: "public_subnet".into(),
                    parent: Some("vpc".into()),
                    ..Default::default()
                },
            ],
            nodes: vec![
                node("user", None),
                node("nat", Some("pub")),
                node("web", Some("vpc")),
            ],
            edges: vec![edge("user", "web")],
            ..Default::default()
        };
        let l = Layout::compute(&spec);
        let vpc = l.groups.iter().find(|g| g.depth == 0).unwrap().rect;
        assert!(l.node("user").unwrap().bounds().max.x < vpc.min.x);
    }

    #[test]
    fn broken_references_are_tolerated() {
        let spec = DiagramSpec {
            groups: vec![
                GroupSpec {
                    id: "g1".into(),
                    kind: "vpc".into(),
                    parent: Some("g2".into()),
                    ..Default::default()
                },
                GroupSpec {
                    id: "g2".into(),
                    kind: "region".into(),
                    parent: Some("g1".into()),
                    ..Default::default()
                },
            ],
            nodes: vec![
                node("a", Some("nope")),
                node("a", Some("g1")),
                node("b", Some("g2")),
            ],
            edges: vec![edge("a", "zzz"), edge("a", "a"), edge("a", "b")],
            ..Default::default()
        };
        let l = Layout::compute(&spec);
        assert_eq!(l.nodes.len(), 2);
        assert_eq!(l.groups.len(), 2);
        assert_eq!(l.edges.len(), 1);
    }
}
