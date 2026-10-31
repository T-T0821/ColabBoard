//! マインドマップ。
//!
//! ノードは普通の図形(`Shape`)で、`Meta::mind` に親・兄弟の中での順番・折りたたみを持つ。
//! 親子をつなぐ枝はコネクタ(曲線)で、ID は子ノードの ID から決まる(`branch_id`)。
//! 位置・大きさ・枝・折りたたみによる非表示は、木の形から決まる純粋な結果なので
//! `relayout` が計算し直して Undo の対象外として書く。どの参加者が計算しても同じ値になるので、
//! 共同編集で同時に枝を足しても、全員の画面が同じ配置に収束する。
//!
//! Undo の対象になるのは、ノードの追加・削除・文字・親子関係・折りたたみ・並べ方だけ。

use std::collections::{HashMap, HashSet};
use std::f32::consts::{FRAC_PI_2, TAU};

use glam::Vec2;
use serde::{Deserialize, Serialize};

use crate::board::Board;
use crate::color::Rgba8;
use crate::connector::{Cap, Connector, Endpoint, PathStyle, boundary_toward};
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, TextAlign, estimate_text_height, estimate_text_width};

/// 要素がマインドマップの何か。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mind {
    Node(MindLink),
    /// 親子をつなぐ枝(コネクタ)。
    Branch,
}

/// ノードの木の中での位置。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MindLink {
    /// 親ノード(`None` なら中心のノード)。
    pub parent: Option<ElementId>,
    /// 兄弟の中での並び(小さいほど先)。
    pub order: f64,
    /// 子を隠しているか。
    pub collapsed: bool,
    /// 並べ方(中心のノードのものだけを使う)。
    pub layout: MindLayout,
}

/// 並べ方。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MindLayout {
    /// 右に広げる。
    Right,
    /// 左右に振り分ける。
    #[default]
    Both,
    /// 中心から放射状に広げる。
    Radial,
}

impl MindLayout {
    pub const ALL: [MindLayout; 3] = [MindLayout::Both, MindLayout::Right, MindLayout::Radial];

    pub fn name(self) -> &'static str {
        match self {
            MindLayout::Right => "right",
            MindLayout::Both => "both",
            MindLayout::Radial => "radial",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s {
            "right" => MindLayout::Right,
            "radial" => MindLayout::Radial,
            _ => MindLayout::Both,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MindLayout::Right => "右へ",
            MindLayout::Both => "左右",
            MindLayout::Radial => "放射",
        }
    }
}

/// 見出しの木(AI の生成結果やマークダウンの見出しから作るときの入力)。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MindTree {
    pub text: String,
    #[serde(default)]
    pub children: Vec<MindTree>,
}

impl MindTree {
    pub fn new(text: impl Into<String>, children: Vec<MindTree>) -> Self {
        Self {
            text: text.into(),
            children,
        }
    }
}

/// 枝ごとの色(中心から出る枝に順に割り当て、その先は同じ色を受け継ぐ)。
pub const BRANCH_COLORS: [Rgba8; 8] = [
    Rgba8::hex(0xe8590c),
    Rgba8::hex(0x1c7ed6),
    Rgba8::hex(0x2f9e44),
    Rgba8::hex(0xae3ec9),
    Rgba8::hex(0xf59f00),
    Rgba8::hex(0x0ca678),
    Rgba8::hex(0xd6336c),
    Rgba8::hex(0x4263eb),
];

/// 中心のノードの色。
pub const ROOT_FILL: Rgba8 = Rgba8::hex(0x343a56);

/// 親と子の列の間隔。
const H_GAP: f32 = 64.0;
/// 兄弟の部分木どうしの間隔。
const V_GAP: f32 = 14.0;
/// 文字の左右の余白(図形の文字領域の余白 12 を含む)。
const PAD_X: f32 = 22.0;
/// 文字の上下の余白。
const PAD_Y: f32 = 16.0;
const MIN_W: f32 = 96.0;
const MAX_W: f32 = 320.0;

/// 枝のコネクタの ID(子ノードの ID から決める。誰が作っても同じ ID になる)。
pub fn branch_id(child: ElementId) -> ElementId {
    ElementId(child.0 ^ (1 << 63))
}

/// ノードなら木の中での位置を返す。
pub fn link_of(e: &Element) -> Option<MindLink> {
    match (e, e.meta().mind) {
        (Element::Shape(_), Some(Mind::Node(l))) => Some(l),
        _ => None,
    }
}

pub fn is_node(board: &Board, id: ElementId) -> bool {
    board.get(id).and_then(link_of).is_some()
}

/// 木の形(壊れた親子関係は直したもの)。
struct Forest {
    /// ノードの ID → 実際の親(親が消えた・循環しているノードは `None` で中心扱い)。
    parent: HashMap<ElementId, Option<ElementId>>,
    /// 子(並び順)。
    children: HashMap<ElementId, Vec<ElementId>>,
    links: HashMap<ElementId, MindLink>,
    roots: Vec<ElementId>,
}

impl Forest {
    fn build(board: &Board) -> Self {
        let mut links = HashMap::new();
        let mut order: Vec<ElementId> = Vec::new();
        for e in board.elements() {
            if let Some(l) = link_of(e) {
                links.insert(e.id(), l);
                order.push(e.id());
            }
        }
        let mut parent: HashMap<ElementId, Option<ElementId>> = HashMap::new();
        for id in &order {
            let p = links[id]
                .parent
                .filter(|p| links.contains_key(p) && p != id);
            parent.insert(*id, p);
        }
        // 同時編集で循環ができたら、循環の中で ID が最小のノードを中心扱いにして切る。
        for id in &order {
            let mut seen = Vec::new();
            let mut cur = *id;
            while let Some(Some(p)) = parent.get(&cur) {
                if seen.contains(&cur) {
                    let cut = *seen.iter().min().unwrap();
                    parent.insert(cut, None);
                    break;
                }
                seen.push(cur);
                cur = *p;
            }
        }
        let mut children: HashMap<ElementId, Vec<ElementId>> = HashMap::new();
        let mut roots = Vec::new();
        for id in &order {
            match parent[id] {
                Some(p) => children.entry(p).or_default().push(*id),
                None => roots.push(*id),
            }
        }
        for v in children.values_mut() {
            v.sort_by(|a, b| links[a].order.total_cmp(&links[b].order).then(a.cmp(b)));
        }
        roots.sort();
        Self {
            parent,
            children,
            links,
            roots,
        }
    }

    fn kids(&self, id: ElementId) -> &[ElementId] {
        self.children.get(&id).map_or(&[], Vec::as_slice)
    }

    /// 見えている子(折りたたんでいれば空)。
    fn visible_kids(&self, id: ElementId) -> &[ElementId] {
        if self.links[&id].collapsed {
            &[]
        } else {
            self.kids(id)
        }
    }

    fn root_of(&self, mut id: ElementId) -> ElementId {
        while let Some(Some(p)) = self.parent.get(&id) {
            id = *p;
        }
        id
    }

    fn depth(&self, mut id: ElementId) -> usize {
        let mut d = 0;
        while let Some(Some(p)) = self.parent.get(&id) {
            id = *p;
            d += 1;
        }
        d
    }

    /// `id` とその子孫(行きがけ順)。
    fn subtree(&self, id: ElementId) -> Vec<ElementId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.kids(n).iter().rev());
        }
        out
    }
}

/// 文字に合わせたノードの大きさ。
fn node_size(s: &Shape) -> Vec2 {
    let style = s.text_style();
    let text_w = s
        .text
        .split('\n')
        .map(|l| estimate_text_width(l, style.font_size))
        .fold(0.0, f32::max);
    let w = (text_w + 2.0 * PAD_X + 2.0).clamp(MIN_W, MAX_W).round();
    let h = estimate_text_height(&s.text, w - 2.0 * PAD_X, &style).max(style.line_height);
    Vec2::new(w, (h + 2.0 * PAD_Y).round())
}

/// 深さに合わせた見た目のノードを作る。`color` は枝の色。
fn make_node(id: ElementId, depth: usize, color: Rgba8, link: MindLink, text: &str) -> Shape {
    let mut s = Shape::rect(id, Vec2::ZERO, Vec2::new(MIN_W, 48.0), Rgba8::WHITE);
    s.align = TextAlign::Center;
    s.text = text.to_string();
    match depth {
        0 => {
            s.fill = ROOT_FILL;
            s.stroke = Rgba8::TRANSPARENT;
            s.stroke_width = 0.0;
            s.corner_radius = 20.0;
            s.font_size = 26.0;
            s.text_color = Rgba8::WHITE;
        }
        1 => {
            s.fill = tint(color);
            s.stroke = color;
            s.stroke_width = 2.0;
            s.corner_radius = 14.0;
            s.font_size = 20.0;
        }
        _ => {
            s.fill = Rgba8::WHITE;
            s.stroke = color;
            s.stroke_width = 1.5;
            s.corner_radius = 10.0;
            s.font_size = 17.0;
        }
    }
    s.meta.mind = Some(Mind::Node(link));
    s.size = node_size(&s);
    s
}

/// 枝の色を白に寄せた淡い色(1 段目のノードの塗り)。
fn tint(c: Rgba8) -> Rgba8 {
    let m = |v: u8| (v as f32 * 0.16 + 255.0 * 0.84).round() as u8;
    Rgba8([m(c.0[0]), m(c.0[1]), m(c.0[2]), 0xff])
}

// ---- 自動レイアウト ----

/// ノードの配置(中心)と、親から見た向き。
#[derive(Clone, Copy)]
struct Placed {
    center: Vec2,
    /// 横に広げる並べ方での向き(右 +1 / 左 -1)。放射なら中心から外への向き。
    dir: Vec2,
}

/// 横に広げる並べ方の部分木の高さ。
fn subtree_height(
    f: &Forest,
    sizes: &HashMap<ElementId, Vec2>,
    id: ElementId,
    memo: &mut HashMap<ElementId, f32>,
) -> f32 {
    if let Some(h) = memo.get(&id) {
        return *h;
    }
    let kids = f.visible_kids(id);
    let own = sizes[&id].y;
    let h = if kids.is_empty() {
        own
    } else {
        let sum: f32 = kids
            .iter()
            .map(|k| subtree_height(f, sizes, *k, memo))
            .sum::<f32>()
            + V_GAP * (kids.len() - 1) as f32;
        sum.max(own)
    };
    memo.insert(id, h);
    h
}

/// 子を縦に並べて、`side` の側に置いていく。`edge` は親の外側の辺の x。
#[allow(clippy::too_many_arguments)]
fn place_column(
    f: &Forest,
    sizes: &HashMap<ElementId, Vec2>,
    memo: &mut HashMap<ElementId, f32>,
    kids: &[ElementId],
    edge: f32,
    center_y: f32,
    side: f32,
    out: &mut HashMap<ElementId, Placed>,
) {
    if kids.is_empty() {
        return;
    }
    let total: f32 = kids
        .iter()
        .map(|k| subtree_height(f, sizes, *k, memo))
        .sum::<f32>()
        + V_GAP * (kids.len() - 1) as f32;
    let mut y = center_y - total * 0.5;
    for k in kids {
        let h = subtree_height(f, sizes, *k, memo);
        let w = sizes[k].x;
        let c = Vec2::new(edge + side * (H_GAP + w * 0.5), y + h * 0.5);
        out.insert(
            *k,
            Placed {
                center: c,
                dir: Vec2::new(side, 0.0),
            },
        );
        place_column(
            f,
            sizes,
            memo,
            f.visible_kids(*k),
            c.x + side * w * 0.5,
            c.y,
            side,
            out,
        );
        y += h + V_GAP;
    }
}

/// 放射状の並べ方での葉の数(折りたたんだノードは 1)。
fn leaves(f: &Forest, id: ElementId, memo: &mut HashMap<ElementId, f32>) -> f32 {
    if let Some(v) = memo.get(&id) {
        return *v;
    }
    let kids = f.visible_kids(id);
    let v = if kids.is_empty() {
        1.0
    } else {
        kids.iter().map(|k| leaves(f, *k, memo)).sum()
    };
    memo.insert(id, v);
    v
}

/// 1 つの木を並べる。中心のノードは今の中心の位置のまま。
fn layout_tree(
    f: &Forest,
    sizes: &HashMap<ElementId, Vec2>,
    root: ElementId,
    root_center: Vec2,
) -> HashMap<ElementId, Placed> {
    let mut out = HashMap::new();
    out.insert(
        root,
        Placed {
            center: root_center,
            dir: Vec2::ZERO,
        },
    );
    let kids = f.visible_kids(root);
    let rs = sizes[&root];
    let mut memo = HashMap::new();
    match f.links[&root].layout {
        MindLayout::Right => place_column(
            f,
            sizes,
            &mut memo,
            kids,
            root_center.x + rs.x * 0.5,
            root_center.y,
            1.0,
            &mut out,
        ),
        MindLayout::Both => {
            // 左右の高さの差がいちばん小さくなる所で分ける(前半を右、後半を左へ。左は下から上へ、時計回り)。
            let heights: Vec<f32> = kids
                .iter()
                .map(|k| subtree_height(f, sizes, *k, &mut memo))
                .collect();
            let total: f32 = heights.iter().sum();
            let mut acc = 0.0;
            let mut split = kids.len().min(1);
            let mut best = f32::INFINITY;
            for (i, h) in heights.iter().enumerate() {
                acc += h;
                let diff = (2.0 * acc - total).abs();
                if diff < best - 0.5 {
                    best = diff;
                    split = i + 1;
                }
            }
            let (right, left) = kids.split_at(split);
            let left: Vec<ElementId> = left.iter().rev().copied().collect();
            place_column(
                f,
                sizes,
                &mut memo,
                right,
                root_center.x + rs.x * 0.5,
                root_center.y,
                1.0,
                &mut out,
            );
            place_column(
                f,
                sizes,
                &mut memo,
                &left,
                root_center.x - rs.x * 0.5,
                root_center.y,
                -1.0,
                &mut out,
            );
        }
        MindLayout::Radial => {
            let total = leaves(f, root, &mut memo);
            // 葉 1 枚あたり 70 の弧の長さが取れる半径にする。
            let r1 = (rs.x * 0.5 + 150.0).max(total * 70.0 / TAU);
            let mut stack: Vec<(ElementId, f32, f32, usize)> = Vec::new();
            let mut a = -FRAC_PI_2;
            for k in kids {
                let span = TAU * leaves(f, *k, &mut memo) / total;
                stack.push((*k, a, a + span, 1));
                a += span;
            }
            while let Some((id, a0, a1, d)) = stack.pop() {
                let mid = (a0 + a1) * 0.5;
                let dir = Vec2::new(mid.cos(), mid.sin());
                let r = r1 + (d - 1) as f32 * 190.0;
                out.insert(
                    id,
                    Placed {
                        center: root_center + dir * r,
                        dir,
                    },
                );
                let n = leaves(f, id, &mut memo);
                let mut a = a0;
                for k in f.visible_kids(id) {
                    let span = (a1 - a0) * leaves(f, *k, &mut memo) / n;
                    stack.push((*k, a, a + span, d + 1));
                    a += span;
                }
            }
        }
    }
    out
}

/// 全部のマインドマップを並べ直す。変わった要素だけを Undo の対象外として書く。
/// 何か書いたら true。
pub fn relayout(board: &mut Board) -> bool {
    let f = Forest::build(board);
    if f.links.is_empty() && !board.elements().iter().any(is_branch) {
        return false;
    }
    let mut puts: Vec<(Element, Option<f64>)> = Vec::new();
    let mut wanted_branches: HashSet<ElementId> = HashSet::new();

    let mut sizes: HashMap<ElementId, Vec2> = HashMap::new();
    for id in f.links.keys() {
        if let Some(Element::Shape(s)) = board.get(*id) {
            sizes.insert(*id, node_size(s));
        }
    }

    for &root in &f.roots {
        let Some(Element::Shape(rs)) = board.get(root) else {
            continue;
        };
        let layout = f.links[&root].layout;
        let placed = layout_tree(&f, &sizes, root, rs.bounds().center());
        // 子孫を行きがけ順に見て、枝の色と非表示を決める。
        let mut stack: Vec<(ElementId, Option<Rgba8>, bool)> = vec![(root, None, false)];
        while let Some((id, color, hidden)) = stack.pop() {
            let Some(Element::Shape(s)) = board.get(id) else {
                continue;
            };
            let mut n = s.clone();
            if let Some(p) = placed.get(&id) {
                let size = sizes[&id];
                n.size = size;
                n.pos = p.center - size * 0.5;
            }
            n.meta.hidden = hidden;
            if n != *s {
                puts.push((Element::Shape(n), None));
            }
            // 1 段目のノードの枠の色がその枝の色になる。
            let my_color = color.unwrap_or(if s.stroke.0[3] > 0 {
                s.stroke
            } else {
                Rgba8::hex(0x868e96)
            });
            let collapsed = f.links[&id].collapsed;
            for k in f.kids(id) {
                stack.push((
                    *k,
                    Some(color.unwrap_or_else(|| child_color(board, *k))),
                    hidden || collapsed,
                ));
            }
            if let Some(Some(parent)) = f.parent.get(&id) {
                let bid = branch_id(id);
                wanted_branches.insert(bid);
                let depth = f.depth(id);
                let (pa, ca) = match (layout, placed.get(&id)) {
                    (MindLayout::Radial, _) | (_, None) => (None, None),
                    (_, Some(p)) => {
                        let s = p.dir.x.signum();
                        (
                            Some(Vec2::new(0.5 + 0.5 * s, 0.5)),
                            Some(Vec2::new(0.5 - 0.5 * s, 0.5)),
                        )
                    }
                };
                let existing = match board.get(bid) {
                    Some(Element::Connector(c)) => Some(c),
                    _ => None,
                };
                let mut c = existing.cloned().unwrap_or_else(|| {
                    Connector::new(bid, Endpoint::Free(Vec2::ZERO), Endpoint::Free(Vec2::ZERO))
                });
                c.from = Endpoint::Attached {
                    id: *parent,
                    anchor: pa,
                };
                c.to = Endpoint::Attached { id, anchor: ca };
                c.style = PathStyle::Curved;
                c.start_cap = Cap::None;
                c.end_cap = Cap::None;
                c.color = my_color;
                c.width = if depth <= 1 { 4.0 } else { 2.5 };
                c.meta.mind = Some(Mind::Branch);
                c.meta.hidden = hidden;
                c.meta.locked = false;
                if existing != Some(&c) {
                    // 枝はノードの後ろに描く。
                    let z = board.z_of(id).map(|z| z - 0.001);
                    puts.push((Element::Connector(c), z));
                }
            }
        }
    }
    let removes: Vec<ElementId> = board
        .elements()
        .iter()
        .filter(|e| is_branch(e) && !wanted_branches.contains(&e.id()))
        .map(Element::id)
        .collect();
    let changed = !puts.is_empty() || !removes.is_empty();
    if changed {
        board.write_untracked(puts, &removes);
    }
    changed
}

fn is_branch(e: &Element) -> bool {
    matches!(e.meta().mind, Some(Mind::Branch))
}

/// 1 段目のノードなら、その枠の色(枝の色)。
fn child_color(board: &Board, id: ElementId) -> Rgba8 {
    match board.get(id) {
        Some(Element::Shape(s)) if s.stroke.0[3] > 0 => s.stroke,
        _ => Rgba8::hex(0x868e96),
    }
}

// ---- 編集操作(どれも Undo の対象。区切りは呼び出し側で `end_gesture`) ----

/// 新しいマインドマップを `center` に作る。中心のノードの ID を返す。
pub fn create(board: &mut Board, center: Vec2, text: &str, layout: MindLayout) -> ElementId {
    let id = board.alloc_id();
    let link = MindLink {
        layout,
        ..MindLink::default()
    };
    let mut s = make_node(id, 0, ROOT_FILL, link, text);
    s.pos = center - s.size * 0.5;
    board.push(s);
    id
}

/// 子ノードを足す(折りたたんでいれば開く)。新しいノードの ID を返す。
pub fn add_child(board: &mut Board, parent: ElementId) -> Option<ElementId> {
    let f = Forest::build(board);
    let plink = *f.links.get(&parent)?;
    if plink.collapsed {
        set_link(board, parent, |l| l.collapsed = false);
    }
    let kids = f.kids(parent);
    let order = kids
        .iter()
        .map(|k| f.links[k].order)
        .fold(f64::NEG_INFINITY, f64::max);
    let order = if order.is_finite() { order + 1.0 } else { 0.0 };
    let depth = f.depth(parent) + 1;
    let color = if depth == 1 {
        BRANCH_COLORS[kids.len() % BRANCH_COLORS.len()]
    } else {
        child_color(board, parent)
    };
    Some(insert_node(board, parent, depth, color, order, ""))
}

/// すぐ下に兄弟ノードを足す。中心のノードなら子を足す。
pub fn add_sibling(board: &mut Board, node: ElementId) -> Option<ElementId> {
    let f = Forest::build(board);
    let parent = match f.parent.get(&node)? {
        Some(p) => *p,
        None => return add_child(board, node),
    };
    let kids = f.kids(parent);
    let i = kids.iter().position(|k| *k == node)?;
    let here = f.links[&node].order;
    let order = match kids.get(i + 1) {
        Some(next) => (here + f.links[next].order) * 0.5,
        None => here + 1.0,
    };
    let depth = f.depth(node);
    let color = if depth == 1 {
        // 中心から出る枝は、使われていない色を選ぶ。
        let used: Vec<Rgba8> = kids.iter().map(|k| child_color(board, *k)).collect();
        BRANCH_COLORS
            .iter()
            .copied()
            .find(|c| !used.contains(c))
            .unwrap_or(BRANCH_COLORS[kids.len() % BRANCH_COLORS.len()])
    } else {
        child_color(board, node)
    };
    Some(insert_node(board, parent, depth, color, order, ""))
}

fn insert_node(
    board: &mut Board,
    parent: ElementId,
    depth: usize,
    color: Rgba8,
    order: f64,
    text: &str,
) -> ElementId {
    let id = board.alloc_id();
    let link = MindLink {
        parent: Some(parent),
        order,
        ..MindLink::default()
    };
    let mut s = make_node(id, depth, color, link, text);
    // 並べ直すまでの仮の位置(親の上)。
    if let Some(p) = board.get(parent) {
        s.pos = p.bounds().center() - s.size * 0.5;
    }
    board.push(s);
    relayout(board);
    id
}

fn set_link(board: &mut Board, id: ElementId, f: impl FnOnce(&mut MindLink)) {
    board.update(id, |e| {
        if let Some(Mind::Node(l)) = &mut e.meta_mut().mind {
            f(l);
        }
    });
}

/// 子を持つノードか。
pub fn has_children(board: &Board, id: ElementId) -> bool {
    !Forest::build(board).kids(id).is_empty()
}

/// 折りたたむ・開く。子がなければ何もしない。変えたら true。
pub fn toggle_collapse(board: &mut Board, id: ElementId) -> bool {
    if !has_children(board, id) {
        return false;
    }
    set_link(board, id, |l| l.collapsed = !l.collapsed);
    relayout(board);
    true
}

/// ノードの子孫を全部開く・全部たたむ(中心のノードはたたまない)。
pub fn set_all_collapsed(board: &mut Board, any: ElementId, collapsed: bool) {
    let f = Forest::build(board);
    if !f.links.contains_key(&any) {
        return;
    }
    let root = f.root_of(any);
    for id in f.subtree(root) {
        let want = collapsed && id != root && !f.kids(id).is_empty();
        if f.links[&id].collapsed != want {
            set_link(board, id, |l| l.collapsed = want);
        }
    }
    relayout(board);
}

/// ノードが入っている木の並べ方。
pub fn layout_of(board: &Board, any: ElementId) -> Option<MindLayout> {
    let f = Forest::build(board);
    f.links
        .contains_key(&any)
        .then(|| f.links[&f.root_of(any)].layout)
}

/// 木の中心のノード。
pub fn root_of(board: &Board, any: ElementId) -> Option<ElementId> {
    let f = Forest::build(board);
    f.links.contains_key(&any).then(|| f.root_of(any))
}

/// ノードが入っている木の並べ方を変える。
pub fn set_layout(board: &mut Board, any: ElementId, layout: MindLayout) {
    let Some(root) = root_of(board, any) else {
        return;
    };
    set_link(board, root, |l| l.layout = layout);
    relayout(board);
}

/// 消すときに一緒に消す要素(選んだノードの子孫と枝)を足した一覧。
pub fn expand_for_delete(board: &Board, ids: &[ElementId]) -> Vec<ElementId> {
    let mut out: Vec<ElementId> = ids.to_vec();
    if !ids.iter().any(|id| is_node(board, *id)) {
        return out;
    }
    let f = Forest::build(board);
    for id in ids {
        if !f.links.contains_key(id) {
            continue;
        }
        for n in f.subtree(*id) {
            for x in [n, branch_id(n)] {
                if !out.contains(&x) && board.get(x).is_some() {
                    out.push(x);
                }
            }
        }
    }
    out
}

/// 動かすときに一緒に動かす要素: 中心のノードを動かすなら木全体。
pub fn expand_for_move(board: &Board, ids: &[ElementId]) -> Vec<ElementId> {
    let mut out: Vec<ElementId> = ids.to_vec();
    if !ids.iter().any(|id| is_node(board, *id)) {
        return out;
    }
    let f = Forest::build(board);
    for id in ids {
        if f.parent.get(id) == Some(&None) {
            for n in f.subtree(*id) {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
    out
}

/// ノード `dragged` を点 `world` で離したときの新しい親の候補(自分の子孫は除く)。
pub fn drop_target(board: &Board, dragged: ElementId, world: Vec2) -> Option<ElementId> {
    let f = Forest::build(board);
    // 中心のノードは付け替えない。
    f.parent.get(&dragged).copied().flatten()?;
    let own: HashSet<ElementId> = f.subtree(dragged).into_iter().collect();
    board
        .elements()
        .iter()
        .rev()
        .filter(|e| link_of(e).is_some() && !own.contains(&e.id()) && !e.meta().hidden)
        .find(|e| e.bounds().contains(world))
        .map(Element::id)
}

/// 親を付け替える(新しい親の最後の子にする)。付け替えたら true。
pub fn reparent(board: &mut Board, id: ElementId, new_parent: ElementId) -> bool {
    let f = Forest::build(board);
    if !f.links.contains_key(&new_parent)
        || f.subtree(id).contains(&new_parent)
        || f.parent.get(&id).copied().flatten() == Some(new_parent)
    {
        return false;
    }
    let order = f
        .kids(new_parent)
        .iter()
        .map(|k| f.links[k].order)
        .fold(-1.0, f64::max)
        + 1.0;
    if f.links[&new_parent].collapsed {
        set_link(board, new_parent, |l| l.collapsed = false);
    }
    set_link(board, id, |l| {
        l.parent = Some(new_parent);
        l.order = order;
    });
    relayout(board);
    true
}

/// 矢印キーでの移動先。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Left,
    Right,
    Up,
    Down,
}

/// ノードから矢印キーの向きにある隣のノード。
pub fn navigate(board: &Board, id: ElementId, nav: Nav) -> Option<ElementId> {
    let f = Forest::build(board);
    f.links.get(&id)?;
    let center = |n: ElementId| board.get(n).map(|e| e.bounds().center());
    let me = center(id)?;
    let root = f.root_of(id);
    let rc = center(root)?;
    let parent = f.parent.get(&id).copied().flatten();
    // 中心より右にあるノードは右が子、左が親。左側はその逆。
    let right_side = me.x >= rc.x;
    let nearest = |cands: &mut dyn Iterator<Item = ElementId>| {
        cands
            .filter_map(|k| Some((k, center(k)?)))
            .min_by(|a, b| (a.1.y - me.y).abs().total_cmp(&(b.1.y - me.y).abs()))
            .map(|(k, _)| k)
    };
    let toward_kids = |want_right: bool| {
        let mut it = f
            .visible_kids(id)
            .iter()
            .copied()
            .filter(|k| center(*k).is_some_and(|c| (c.x >= me.x) == want_right));
        nearest(&mut it)
    };
    match nav {
        Nav::Right if parent.is_none() => toward_kids(true),
        Nav::Left if parent.is_none() => toward_kids(false),
        Nav::Right if right_side => toward_kids(true),
        Nav::Left if !right_side => toward_kids(false),
        Nav::Left | Nav::Right => parent,
        Nav::Up | Nav::Down => {
            let p = parent?;
            let sibs: Vec<(ElementId, Vec2)> = f
                .kids(p)
                .iter()
                .filter(|k| **k != id)
                .filter_map(|k| Some((*k, center(*k)?)))
                .filter(|(_, c)| (c.x >= rc.x) == right_side)
                .collect();
            let up = nav == Nav::Up;
            sibs.into_iter()
                .filter(|(_, c)| if up { c.y < me.y } else { c.y > me.y })
                .min_by(|a, b| (a.1.y - me.y).abs().total_cmp(&(b.1.y - me.y).abs()))
                .map(|(k, _)| k)
        }
    }
}

/// 折りたたみのつまみ。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Toggle {
    pub id: ElementId,
    pub center: Vec2,
    pub collapsed: bool,
    pub color: Rgba8,
    /// 隠れている子孫の数(開いていれば 0)。
    pub hidden: usize,
}

/// 子を持つ(中心以外の)見えているノードの、折りたたみのつまみ。`r` はつまみの半径。
pub fn toggles(board: &Board, view: &Rect, r: f32) -> Vec<Toggle> {
    let f = Forest::build(board);
    let mut out = Vec::new();
    for (id, link) in &f.links {
        let Some(Some(_)) = f.parent.get(id) else {
            continue;
        };
        if f.kids(*id).is_empty() {
            continue;
        }
        let Some(Element::Shape(s)) = board.get(*id) else {
            continue;
        };
        if s.meta.hidden || !s.bounds().expand(r * 3.0).intersects(view) {
            continue;
        }
        let root = f.root_of(*id);
        let Some(rc) = board.get(root).map(|e| e.bounds().center()) else {
            continue;
        };
        let b = s.bounds();
        let c = b.center();
        let center = match f.links[&root].layout {
            MindLayout::Radial => {
                let dir = (c - rc).normalize_or(Vec2::X);
                boundary_toward(&b, c + dir * 10_000.0) + dir * (r + 2.0)
            }
            _ => {
                let side = if c.x >= rc.x { 1.0 } else { -1.0 };
                Vec2::new(c.x + side * (b.size().x * 0.5 + r + 2.0), c.y)
            }
        };
        let hidden = if link.collapsed {
            f.subtree(*id).len() - 1
        } else {
            0
        };
        out.push(Toggle {
            id: *id,
            center,
            collapsed: link.collapsed,
            color: if s.stroke.0[3] > 0 {
                s.stroke
            } else {
                Rgba8::hex(0x868e96)
            },
            hidden,
        });
    }
    out.sort_by_key(|t| t.id);
    out
}

// ---- 木から作る ----

/// 見出しの木からマインドマップを作り、`center` を中心に並べる。中心のノードの ID を返す。
///
/// AI の生成結果やマークダウンの見出しから作るときに使う。追加は 1 回の操作として Undo できる
/// (区切りは呼び出し側で `end_gesture`)。
pub fn insert_tree(
    board: &mut Board,
    tree: &MindTree,
    center: Vec2,
    layout: MindLayout,
) -> ElementId {
    let root = create(board, center, &tree.text, layout);
    let mut nodes: Vec<Element> = Vec::new();
    // (木, 親, 深さ, 色)
    let mut stack: Vec<(&MindTree, ElementId, usize, Rgba8)> = Vec::new();
    for (i, c) in tree.children.iter().enumerate().rev() {
        stack.push((c, root, 1, BRANCH_COLORS[i % BRANCH_COLORS.len()]));
    }
    let mut order = 0.0;
    while let Some((t, parent, depth, color)) = stack.pop() {
        let id = board.alloc_id();
        let link = MindLink {
            parent: Some(parent),
            order,
            ..MindLink::default()
        };
        order += 1.0;
        let mut s = make_node(id, depth, color, link, &t.text);
        s.pos = center - s.size * 0.5;
        nodes.push(s.into());
        for c in t.children.iter().rev() {
            stack.push((c, id, depth + 1, color));
        }
    }
    board.push_many(nodes);
    relayout(board);
    root
}

/// 木を見出しの木として読み出す(マークダウンや AI への入力用)。
pub fn to_tree(board: &Board, any: ElementId) -> Option<MindTree> {
    let f = Forest::build(board);
    if !f.links.contains_key(&any) {
        return None;
    }
    fn build(board: &Board, f: &Forest, id: ElementId) -> MindTree {
        MindTree {
            text: board
                .get(id)
                .map(|e| e.text().to_string())
                .unwrap_or_default(),
            children: f.kids(id).iter().map(|k| build(board, f, *k)).collect(),
        }
    }
    Some(build(board, &f, f.root_of(any)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MindTree {
        MindTree::new(
            "テーマ",
            vec![
                MindTree::new(
                    "A",
                    vec![MindTree::new("A1", vec![]), MindTree::new("A2", vec![])],
                ),
                MindTree::new("B", vec![MindTree::new("B1", vec![])]),
                MindTree::new("C", vec![]),
                MindTree::new("D", vec![]),
            ],
        )
    }

    fn shape(b: &Board, id: ElementId) -> &Shape {
        b.get(id).unwrap().as_shape().unwrap()
    }

    fn branches(b: &Board) -> usize {
        b.elements().iter().filter(|e| is_branch(e)).count()
    }

    fn find(b: &Board, text: &str) -> ElementId {
        b.elements()
            .iter()
            .find(|e| link_of(e).is_some() && e.text() == text)
            .unwrap()
            .id()
    }

    #[test]
    fn insert_tree_lays_out_both_sides_with_branches() {
        let mut b = Board::new();
        let root = insert_tree(&mut b, &sample(), Vec2::new(500.0, 300.0), MindLayout::Both);
        assert_eq!(
            b.elements().iter().filter(|e| link_of(e).is_some()).count(),
            8
        );
        assert_eq!(branches(&b), 7);
        let rc = shape(&b, root).bounds().center();
        assert!((rc - Vec2::new(500.0, 300.0)).length() < 1.0);
        // A と B は右、C と D は左。
        assert!(shape(&b, find(&b, "A")).pos.x > rc.x);
        assert!(shape(&b, find(&b, "D")).bounds().max.x < rc.x);
        // 孫は子よりさらに外側。
        assert!(shape(&b, find(&b, "A1")).pos.x > shape(&b, find(&b, "A")).bounds().max.x);
        // ノードどうしが重ならない。
        let nodes: Vec<Rect> = b
            .elements()
            .iter()
            .filter(|e| link_of(e).is_some())
            .map(|e| e.bounds())
            .collect();
        for (i, a) in nodes.iter().enumerate() {
            for c in &nodes[i + 1..] {
                assert!(!a.expand(-1.0).intersects(&c.expand(-1.0)), "{a:?} {c:?}");
            }
        }
        // 2 回目は何も変えない(全員が同じ配置に収束する)。
        assert!(!relayout(&mut b));
        // 枝の色は 1 段目のノードの枠の色。
        let a1 = find(&b, "A1");
        let Some(Element::Connector(c)) = b.get(branch_id(a1)) else {
            panic!()
        };
        assert_eq!(c.color, shape(&b, find(&b, "A")).stroke);
        assert_eq!(c.style, PathStyle::Curved);
    }

    #[test]
    fn tab_and_enter_add_nodes_in_order() {
        let mut b = Board::new();
        let root = create(&mut b, Vec2::ZERO, "中心", MindLayout::Right);
        let a = add_child(&mut b, root).unwrap();
        let c = add_sibling(&mut b, a).unwrap();
        // a と c の間に入る。
        let mid = add_sibling(&mut b, a).unwrap();
        let ys: Vec<f32> = [a, mid, c].iter().map(|id| shape(&b, *id).pos.y).collect();
        assert!(ys[0] < ys[1] && ys[1] < ys[2], "{ys:?}");
        // 1 段目は違う色。
        assert_ne!(shape(&b, a).stroke, shape(&b, c).stroke);
        let g = add_child(&mut b, a).unwrap();
        assert_eq!(shape(&b, g).stroke, shape(&b, a).stroke);
        assert_eq!(navigate(&b, a, Nav::Right), Some(g));
        assert_eq!(navigate(&b, g, Nav::Left), Some(a));
        assert_eq!(navigate(&b, a, Nav::Down), Some(mid));
        assert!(navigate(&b, root, Nav::Right).is_some());
        assert_eq!(
            add_sibling(&mut b, root).map(|n| is_node(&b, n)),
            Some(true)
        );
    }

    #[test]
    fn collapse_hides_descendants_and_undo_restores() {
        let mut b = Board::new();
        let root = insert_tree(&mut b, &sample(), Vec2::ZERO, MindLayout::Right);
        b.end_gesture();
        let a = find(&b, "A");
        let a1 = find(&b, "A1");
        assert!(toggle_collapse(&mut b, a));
        b.end_gesture();
        assert!(b.get(a1).unwrap().meta().hidden);
        assert!(b.get(branch_id(a1)).unwrap().meta().hidden);
        assert!(
            !b.get(a1)
                .unwrap()
                .hit(b.get(a1).unwrap().bounds().center(), 0.0)
        );
        let t = toggles(&b, &Rect::new(Vec2::splat(-1e5), Vec2::splat(1e5)), 8.0);
        assert!(t.iter().any(|t| t.id == a && t.collapsed && t.hidden == 2));
        // たたむと B が上に詰まる。
        let b_y = shape(&b, find(&b, "B")).pos.y;
        b.undo();
        relayout(&mut b);
        assert!(!b.get(a1).unwrap().meta().hidden);
        assert!(shape(&b, find(&b, "B")).pos.y > b_y);
        let _ = root;
    }

    #[test]
    fn delete_takes_subtree_and_branches() {
        let mut b = Board::new();
        insert_tree(&mut b, &sample(), Vec2::ZERO, MindLayout::Both);
        b.end_gesture();
        let a = find(&b, "A");
        let ids = expand_for_delete(&b, &[a]);
        assert_eq!(ids.len(), 6);
        b.remove(&ids);
        relayout(&mut b);
        b.end_gesture();
        assert_eq!(branches(&b), 4);
        b.undo();
        relayout(&mut b);
        assert_eq!(branches(&b), 7);
        assert!(!relayout(&mut b));
    }

    #[test]
    fn reparent_by_drop_and_no_cycles() {
        let mut b = Board::new();
        insert_tree(&mut b, &sample(), Vec2::ZERO, MindLayout::Right);
        let a = find(&b, "A");
        let a1 = find(&b, "A1");
        let c = find(&b, "C");
        assert!(!reparent(&mut b, a, a1));
        let target = shape(&b, c).bounds().center();
        assert_eq!(drop_target(&b, a, target), Some(c));
        assert!(reparent(&mut b, a, c));
        assert!(shape(&b, a).pos.x > shape(&b, c).bounds().max.x);
        let t = to_tree(&b, a).unwrap();
        assert_eq!(t.text, "テーマ");
        assert_eq!(t.children.len(), 3);
    }

    #[test]
    fn radial_layout_spreads_around_root() {
        let mut b = Board::new();
        let root = insert_tree(&mut b, &sample(), Vec2::ZERO, MindLayout::Radial);
        let rc = shape(&b, root).bounds().center();
        let quadrants: HashSet<(bool, bool)> = ["A", "B", "C", "D"]
            .iter()
            .map(|t| {
                let c = shape(&b, find(&b, t)).bounds().center() - rc;
                (c.x >= 0.0, c.y >= 0.0)
            })
            .collect();
        assert!(quadrants.len() >= 3);
        set_layout(&mut b, root, MindLayout::Right);
        assert_eq!(layout_of(&b, find(&b, "A1")), Some(MindLayout::Right));
    }

    #[test]
    fn concurrent_children_converge() {
        let mut x = Board::with_client_id(1);
        let root = create(&mut x, Vec2::ZERO, "中心", MindLayout::Both);
        let mut y = Board::with_client_id(2);
        y.apply_remote(&x.encode_diff(None).unwrap()).unwrap();
        add_child(&mut x, root);
        add_child(&mut y, root);
        add_child(&mut y, root);
        let ux = x.encode_diff(None).unwrap();
        let uy = y.encode_diff(None).unwrap();
        x.apply_remote(&uy).unwrap();
        y.apply_remote(&ux).unwrap();
        // 両方が並べ直して、互いの結果を受け取る。
        for _ in 0..3 {
            relayout(&mut x);
            relayout(&mut y);
            let ux = x.encode_diff(Some(&y.state_vector())).unwrap();
            let uy = y.encode_diff(Some(&x.state_vector())).unwrap();
            x.apply_remote(&uy).unwrap();
            y.apply_remote(&ux).unwrap();
        }
        assert!(!relayout(&mut x));
        let pos = |b: &Board| {
            let mut v: Vec<_> = b
                .elements()
                .iter()
                .map(|e| (e.id(), e.bounds().min.to_array()))
                .collect();
            v.sort_by_key(|p| p.0);
            v
        };
        assert_eq!(pos(&x), pos(&y));
        assert_eq!(branches(&x), 3);
    }

    #[test]
    fn tree_json_round_trip() {
        let json = r#"{"text":"テーマ","children":[{"text":"A"},{"text":"B","children":[{"text":"B1"}]}]}"#;
        let t: MindTree = serde_json::from_str(json).unwrap();
        assert_eq!(t.children[1].children[0].text, "B1");
        let mut b = Board::new();
        let root = insert_tree(&mut b, &t, Vec2::ZERO, MindLayout::default());
        assert_eq!(to_tree(&b, root), Some(t));
    }
}
