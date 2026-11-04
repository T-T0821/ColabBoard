//! ボードの要素を読んで図([`Diagram`])に戻す。Mermaid・PlantUML への書き出しの元になる。
//!
//! - ライフライン(頭の箱 + 縦の破線のグループ)が 2 本以上あればシーケンス図。
//!   ライフラインの間の水平な線をメッセージとして上から順に読む。
//! - それ以外はノードと矢印の図。図形をノード、両端がつながった線を矢印、フレームを枠にする。
//!   UML のクラス・エンティティ(区画を積んだグループ)、アクター(棒人間のグループ)、
//!   開始・終了状態(黒丸・二重丸)は部品ごとに 1 つのノードとして読む。
//!   図形の形からノードの種類(処理・判断・データ・端子・メモ)を決める。

use std::collections::{HashMap, HashSet};

use glam::Vec2;

use super::catalog;
use super::sequence::SELF_SIZE;
use super::{Diagram, DiagramSpec, Direction, EdgeLine, EdgeSpec, GroupSpec, NodeSpec};
use super::{SequenceSpec, node_text};
use crate::board::Board;
use crate::connector::{Connector, Endpoint};
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind};
use crate::uml;

/// 書き出す要素。`ids` が空ならボード全体(隠れた要素を除く)。
/// 選んだ要素と同じグループの要素、選んだフレームの中の要素、
/// 両端が対象につながった線も含める。
pub fn scope(board: &Board, ids: &[ElementId]) -> Vec<Element> {
    let all = board.elements();
    if ids.is_empty() {
        return all.iter().filter(|e| !e.meta().hidden).cloned().collect();
    }
    let mut set: HashSet<ElementId> = ids.iter().copied().collect();
    let groups: HashSet<u64> = all
        .iter()
        .filter(|e| set.contains(&e.id()) && e.meta().group != 0)
        .map(|e| e.meta().group)
        .collect();
    let frames: Vec<Rect> = all
        .iter()
        .filter_map(Element::as_shape)
        .filter(|s| s.kind == ShapeKind::Frame && set.contains(&s.id))
        .map(Shape::bounds)
        .collect();
    for e in all {
        if groups.contains(&e.meta().group)
            || frames.iter().any(|f| f.contains(e.bounds().center()))
        {
            set.insert(e.id());
        }
    }
    for e in all {
        if let Element::Connector(c) = e
            && let (Some(a), Some(b)) = (c.from.attached_to(), c.to.attached_to())
            && set.contains(&a)
            && set.contains(&b)
        {
            set.insert(c.id);
        }
    }
    all.iter()
        .filter(|e| set.contains(&e.id()) && !e.meta().hidden)
        .cloned()
        .collect()
}

/// ボードの要素 `ids`(空なら全体)を図にする。
pub fn extract(board: &Board, ids: &[ElementId]) -> Result<Diagram, String> {
    from_elements(&scope(board, ids))
}

/// 要素の並びを図にする。
pub fn from_elements(elements: &[Element]) -> Result<Diagram, String> {
    let mut groups: HashMap<u64, Vec<&Element>> = HashMap::new();
    for e in elements {
        if e.meta().group != 0 {
            groups.entry(e.meta().group).or_default().push(e);
        }
    }
    if let Some(seq) = sequence(elements, &groups) {
        return Ok(Diagram::Sequence(seq));
    }
    flow(elements, &groups).map(Diagram::Flow)
}

// ---- シーケンス図 ----

struct Lifeline {
    label: String,
    x: f32,
    top: f32,
    group: u64,
}

fn free_points(c: &Connector) -> Option<(Vec2, Vec2)> {
    match (&c.from, &c.to) {
        (Endpoint::Free(a), Endpoint::Free(b)) => Some((*a, *b)),
        _ => None,
    }
}

/// グループがライフライン(頭の箱 1 つ + その下の縦の破線)なら読む。
fn lifeline(group: u64, members: &[&Element]) -> Option<Lifeline> {
    let shapes: Vec<&Shape> = members.iter().filter_map(|e| e.as_shape()).collect();
    let lines: Vec<&Connector> = members
        .iter()
        .filter_map(|e| match e {
            Element::Connector(c) => Some(c),
            _ => None,
        })
        .collect();
    let ([head], [line]) = (shapes.as_slice(), lines.as_slice()) else {
        return None;
    };
    if members.len() != 2 || head.kind != ShapeKind::Rect || !line.dashed {
        return None;
    }
    let (a, b) = free_points(line)?;
    let hb = head.bounds();
    let vertical = (a.x - b.x).abs() < 2.0;
    let below = (a.y.min(b.y) - hb.max.y).abs() < 8.0;
    let centered = (a.x - hb.center().x).abs() < 8.0;
    (vertical && below && centered).then(|| Lifeline {
        label: head.text.trim().to_string(),
        x: a.x,
        top: hb.min.y,
        group,
    })
}

fn sequence(elements: &[Element], groups: &HashMap<u64, Vec<&Element>>) -> Option<SequenceSpec> {
    let mut lines: Vec<Lifeline> = groups.iter().filter_map(|(g, m)| lifeline(*g, m)).collect();
    if lines.len() < 2 {
        return None;
    }
    lines.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.top.total_cmp(&b.top)));
    let mut spec = SequenceSpec::default();
    let mut used = HashSet::new();
    let ids: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(k, l)| pick_id(&l.label, "p", k + 1, &mut used))
        .collect();
    for (l, id) in lines.iter().zip(&ids) {
        spec.participant(id, Some(&l.label));
    }
    let in_lifeline: HashSet<u64> = lines.iter().map(|l| l.group).collect();
    let nearest = |x: f32| -> Option<usize> {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i, (l.x - x).abs()))
            .filter(|(_, d)| *d < 40.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let mut msgs: Vec<(f32, usize, usize, &Connector)> = Vec::new();
    for e in elements {
        let Element::Connector(c) = e else { continue };
        if in_lifeline.contains(&c.meta.group) {
            continue;
        }
        let Some((a, b)) = free_points(c) else {
            continue;
        };
        let Some(from) = nearest(a.x) else { continue };
        let d = b - a;
        if d.y.abs() < 12.0 {
            if let Some(to) = nearest(b.x)
                && to != from
            {
                msgs.push((a.y, from, to, c));
            }
        } else if d.y > 0.0 && d.y <= SELF_SIZE.y + 8.0 && d.x >= 0.0 && d.x <= SELF_SIZE.x + 8.0 {
            msgs.push((a.y, from, from, c));
        }
    }
    msgs.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, from, to, c) in msgs {
        spec.message(&ids[from], &ids[to], c.text.trim(), c.dashed);
    }
    Some(spec)
}

// ---- ノードと矢印の図 ----

/// 部品として読んだノード(つながる要素の ID は複数ありうる)。
struct Part {
    node: NodeSpec,
    bounds: Rect,
    members: Vec<ElementId>,
}

/// 区画を縦に積んだグループ(クラス・エンティティ)なら読む。
fn stacked_box(members: &[&Element]) -> Option<(NodeSpec, Rect)> {
    let shapes: Vec<&Shape> = members.iter().filter_map(|e| e.as_shape()).collect();
    if shapes.len() != members.len()
        || shapes.len() < 3
        || shapes.iter().any(|s| s.kind != ShapeKind::Rect)
    {
        return None;
    }
    // いちばん大きい四角形が外枠で、ほかは外枠の中に縦に並ぶ。
    let outer = shapes
        .iter()
        .max_by(|a, b| (a.size.x * a.size.y).total_cmp(&(b.size.x * b.size.y)))?;
    let ob = outer.bounds().expand(1.0);
    let mut parts: Vec<&&Shape> = shapes
        .iter()
        .filter(|s| s.id != outer.id && ob.contains_rect(&s.bounds()))
        .collect();
    if parts.len() + 1 != shapes.len() {
        return None;
    }
    parts.sort_by(|a, b| a.pos.y.total_cmp(&b.pos.y));
    let header = parts[0];
    let title = header.text.trim();
    let (kind, label) = if header.fill == uml::ENTITY_HEADER {
        ("entity", title.to_string())
    } else if let Some(rest) = title.strip_prefix(uml::INTERFACE_MARK) {
        ("interface", rest.trim().to_string())
    } else {
        ("class", title.to_string())
    };
    let sections = parts[1..]
        .iter()
        .map(|s| s.text.trim_end().to_string())
        .collect();
    Some((
        NodeSpec {
            kind: kind.into(),
            label,
            sections,
            ..NodeSpec::default()
        },
        outer.bounds(),
    ))
}

/// 棒人間(頭の丸 + 手書き線 + 名前)なら読む。
fn actor(members: &[&Element]) -> Option<(NodeSpec, Rect)> {
    let shapes: Vec<&Shape> = members.iter().filter_map(|e| e.as_shape()).collect();
    let strokes = members
        .iter()
        .filter(|e| matches!(e, Element::Stroke(_)))
        .count();
    let heads: Vec<&&Shape> = shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::Ellipse)
        .collect();
    let names: Vec<&&Shape> = shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::Text)
        .collect();
    if strokes == 0 || heads.len() != 1 || names.len() != 1 || shapes.len() != 2 {
        return None;
    }
    let bounds = members
        .iter()
        .map(|e| e.bounds())
        .reduce(|a, b| a.union(&b))?;
    Some((
        NodeSpec {
            kind: "actor".into(),
            label: names[0].text.trim().to_string(),
            ..NodeSpec::default()
        },
        bounds,
    ))
}

/// 終了状態(二重丸)なら読む。
fn final_state(members: &[&Element]) -> Option<(NodeSpec, Rect)> {
    let shapes: Vec<&Shape> = members.iter().filter_map(|e| e.as_shape()).collect();
    let [a, b] = shapes.as_slice() else {
        return None;
    };
    if members.len() != 2
        || a.kind != ShapeKind::Ellipse
        || b.kind != ShapeKind::Ellipse
        || !a.text.trim().is_empty()
        || !b.text.trim().is_empty()
    {
        return None;
    }
    let (outer, inner) = if a.size.x >= b.size.x { (a, b) } else { (b, a) };
    outer
        .bounds()
        .contains_rect(&inner.bounds())
        .then(|| (terminal("終了"), outer.bounds()))
}

fn terminal(label: &str) -> NodeSpec {
    NodeSpec {
        kind: "terminal".into(),
        label: label.into(),
        ..NodeSpec::default()
    }
}

/// 開始状態(小さな黒丸)か。
fn is_initial_state(s: &Shape) -> bool {
    let [r, g, b, a] = s.fill.0;
    let luma = (r as u32 * 3 + g as u32 * 6 + b as u32) / 10;
    s.kind == ShapeKind::Ellipse
        && s.size.max_element() <= 48.0
        && s.text.trim().is_empty()
        && a > 0x80
        && luma < 110
}

/// 1 つの図形をノードにする。
fn shape_node(s: &Shape) -> NodeSpec {
    if is_initial_state(s) {
        return terminal("開始");
    }
    let text = s.text.trim();
    // 構成図の部品(アイコン付き)。表示は「サービス名 + 名前」。
    if !s.icon.is_empty() && !s.icon.starts_with("group:") {
        let k = catalog::node_kind(&s.icon);
        if k.id == s.icon {
            let label = match text.split_once('\n') {
                Some((head, rest)) if head.trim() == k.title => rest.trim(),
                _ if text == k.title => "",
                _ => text,
            };
            if node_text(k, label) == text {
                return NodeSpec {
                    kind: k.id.into(),
                    label: label.into(),
                    ..NodeSpec::default()
                };
            }
        }
    }
    let min = s.size.min_element();
    let kind = match s.kind {
        ShapeKind::Diamond | ShapeKind::Hexagon => "decision",
        ShapeKind::Parallelogram
        | ShapeKind::ParallelogramLeft
        | ShapeKind::Cylinder
        | ShapeKind::Document
        | ShapeKind::InternalStorage
        | ShapeKind::ManualInput => "data",
        ShapeKind::Ellipse | ShapeKind::Pill => "terminal",
        ShapeKind::Rect | ShapeKind::RoundRect if s.corner_radius >= min * 0.4 => "terminal",
        ShapeKind::Sticky | ShapeKind::Card => "note",
        _ => "process",
    };
    NodeSpec {
        kind: kind.into(),
        label: text.into(),
        ..NodeSpec::default()
    }
}

/// 線の見た目。
fn edge_line(c: &Connector) -> Option<EdgeLine> {
    EdgeLine {
        start: c.start_cap,
        end: c.end_cap,
        dashed: c.dashed,
    }
    .normalized()
}

fn area(r: &Rect) -> f32 {
    r.size().x * r.size().y
}

fn flow(elements: &[Element], groups: &HashMap<u64, Vec<&Element>>) -> Result<DiagramSpec, String> {
    let attached: HashSet<ElementId> = elements
        .iter()
        .filter_map(|e| match e {
            Element::Connector(c) => Some([c.from.attached_to(), c.to.attached_to()]),
            _ => None,
        })
        .flatten()
        .flatten()
        .collect();

    // 部品(グループ)を先に読む。並びはボードでの最初の要素の順。
    let mut parts: Vec<(usize, Part)> = Vec::new();
    let mut consumed: HashSet<ElementId> = HashSet::new();
    let mut seen_groups: HashSet<u64> = HashSet::new();
    for (i, e) in elements.iter().enumerate() {
        let g = e.meta().group;
        if g == 0 || !seen_groups.insert(g) {
            continue;
        }
        let members = &groups[&g];
        let read = stacked_box(members)
            .or_else(|| actor(members))
            .or_else(|| final_state(members));
        if let Some((node, bounds)) = read {
            let ids: Vec<ElementId> = members.iter().map(|e| e.id()).collect();
            consumed.extend(&ids);
            parts.push((
                i,
                Part {
                    node,
                    bounds,
                    members: ids,
                },
            ));
        }
    }
    let mut frames: Vec<&Shape> = Vec::new();
    for (i, e) in elements.iter().enumerate() {
        let Element::Shape(s) = e else { continue };
        if consumed.contains(&s.id) {
            continue;
        }
        match s.kind {
            ShapeKind::Frame => {
                frames.push(s);
                continue;
            }
            ShapeKind::Image | ShapeKind::Video | ShapeKind::Stamp => continue,
            _ => {}
        }
        let connected = attached.contains(&s.id);
        let empty = s.text.trim().is_empty();
        if !connected && (s.kind == ShapeKind::Text || (empty && !is_initial_state(s))) {
            continue;
        }
        parts.push((
            i,
            Part {
                node: shape_node(s),
                bounds: s.bounds(),
                members: vec![s.id],
            },
        ));
    }
    parts.sort_by_key(|(i, _)| *i);
    if parts.is_empty() {
        return Err("書き出せる図形がありません".into());
    }

    let mut spec = DiagramSpec::default();
    // 枠: 中心を含むいちばん小さいフレームが親。
    let frame_ids: Vec<String> = (1..=frames.len()).map(|k| format!("g{k}")).collect();
    let smallest_frame = |r: &Rect, skip: Option<usize>| -> Option<usize> {
        frames
            .iter()
            .enumerate()
            .filter(|(k, f)| {
                Some(*k) != skip && f.bounds().contains(r.center()) && area(&f.bounds()) > area(r)
            })
            .min_by(|a, b| area(&a.1.bounds()).total_cmp(&area(&b.1.bounds())))
            .map(|(k, _)| k)
    };
    for (k, f) in frames.iter().enumerate() {
        let kind = f
            .icon
            .strip_prefix("group:")
            .filter(|id| catalog::group_kind(id).id == *id)
            .unwrap_or("generic");
        spec.groups.push(GroupSpec {
            id: frame_ids[k].clone(),
            kind: kind.into(),
            label: f.text.trim().to_string(),
            parent: smallest_frame(&f.bounds(), Some(k)).map(|p| frame_ids[p].clone()),
        });
    }
    let mut owner: HashMap<ElementId, usize> = HashMap::new();
    let mut centers: Vec<Vec2> = Vec::new();
    for (n, (_, p)) in parts.iter().enumerate() {
        let mut node = p.node.clone();
        node.id = format!("n{}", n + 1);
        node.group = smallest_frame(&p.bounds, None).map(|k| frame_ids[k].clone());
        for id in &p.members {
            owner.insert(*id, n);
        }
        centers.push(p.bounds.center());
        spec.nodes.push(node);
    }
    // 中身のない枠は落とす。
    let used: HashSet<String> = spec
        .nodes
        .iter()
        .filter_map(|n| n.group.clone())
        .chain(spec.groups.iter().filter_map(|g| g.parent.clone()))
        .collect();
    spec.groups.retain(|g| used.contains(&g.id));

    let (mut dx, mut dy) = (0.0, 0.0);
    for e in elements {
        let Element::Connector(c) = e else { continue };
        let (Some(a), Some(b)) = (c.from.attached_to(), c.to.attached_to()) else {
            continue;
        };
        let (Some(&a), Some(&b)) = (owner.get(&a), owner.get(&b)) else {
            continue;
        };
        if a == b {
            continue;
        }
        let d = centers[b] - centers[a];
        dx += d.x.abs();
        dy += d.y.abs();
        spec.edges.push(EdgeSpec {
            from: spec.nodes[a].id.clone(),
            to: spec.nodes[b].id.clone(),
            label: c.text.trim().to_string(),
            line: edge_line(c),
        });
    }
    spec.direction = if dy > dx {
        Direction::Down
    } else {
        Direction::Right
    };
    canonicalize(&mut spec);
    Ok(spec)
}

/// 書き出しで ID に使える名前か(英数字と `_`、予約語でない)。
fn is_ident(s: &str) -> bool {
    const RESERVED: &[&str] = &[
        "end",
        "graph",
        "flowchart",
        "subgraph",
        "class",
        "classDef",
        "style",
        "click",
        "default",
        "direction",
        "participant",
        "actor",
        "note",
        "as",
        "title",
        "namespace",
        "interface",
        "entity",
        "abstract",
        "enum",
        "package",
        "rectangle",
        "database",
        "usecase",
        "hexagon",
        "left",
        "right",
        "up",
        "down",
        "start",
        "stop",
        "if",
        "else",
        "endif",
        "while",
        "repeat",
        "link",
        "loop",
        "alt",
        "opt",
        "par",
        "and",
        "rect",
        "activate",
        "deactivate",
        "box",
        "create",
        "destroy",
        "o",
        "x",
    ];
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.len() <= 40
        && !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(s))
}

/// 名前から ID を決める(使えない・重なる名前は `{prefix}{番号}`)。
fn pick_id(label: &str, prefix: &str, n: usize, used: &mut HashSet<String>) -> String {
    let id = if is_ident(label) && !used.contains(label) {
        label.to_string()
    } else {
        let mut k = n;
        loop {
            let id = format!("{prefix}{k}");
            if !used.contains(&id) {
                break id;
            }
            k += 1;
        }
    };
    used.insert(id.clone());
    id
}

/// 書き出したときの並び(枠ごとに中身、最後に枠の外)にそろえ、読みやすい ID を付け直す。
/// 書き出した文字を読み直したときに同じ順番・同じ ID になる。
fn canonicalize(spec: &mut DiagramSpec) {
    // 枠: 外側から、子は親の直後(深さ優先)。
    fn visit(spec: &DiagramSpec, parent: Option<&String>, out: &mut Vec<usize>) {
        for (i, g) in spec.groups.iter().enumerate() {
            if g.parent.as_ref() == parent {
                out.push(i);
                visit(spec, Some(&g.id), out);
            }
        }
    }
    let mut group_order = Vec::new();
    visit(spec, None, &mut group_order);
    let groups: Vec<GroupSpec> = group_order
        .iter()
        .map(|&i| spec.groups[i].clone())
        .collect();
    let mut node_order: Vec<usize> = Vec::new();
    for g in &groups {
        node_order
            .extend((0..spec.nodes.len()).filter(|&i| spec.nodes[i].group.as_ref() == Some(&g.id)));
    }
    node_order.extend((0..spec.nodes.len()).filter(|&i| spec.nodes[i].group.is_none()));
    let mut used = HashSet::new();
    let mut group_ids: HashMap<String, String> = HashMap::new();
    for (k, g) in groups.iter().enumerate() {
        group_ids.insert(g.id.clone(), pick_id(&g.label, "g", k + 1, &mut used));
    }
    let mut node_ids: HashMap<String, String> = HashMap::new();
    let mut nodes = Vec::with_capacity(spec.nodes.len());
    for (k, &i) in node_order.iter().enumerate() {
        let mut n = spec.nodes[i].clone();
        let id = pick_id(&n.label, "n", k + 1, &mut used);
        node_ids.insert(n.id.clone(), id.clone());
        n.id = id;
        n.group = n.group.and_then(|g| group_ids.get(&g).cloned());
        nodes.push(n);
    }
    spec.groups = groups
        .into_iter()
        .map(|mut g| {
            g.id = group_ids[&g.id].clone();
            g.parent = g.parent.and_then(|p| group_ids.get(&p).cloned());
            g
        })
        .collect();
    spec.nodes = nodes;
    for e in &mut spec.edges {
        e.from = node_ids[&e.from].clone();
        e.to = node_ids[&e.to].clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::{self, Template};

    fn board_of(t: Template) -> Board {
        let mut b = Board::new();
        templates::insert(&mut b, t, Vec2::ZERO);
        b
    }

    fn flow_of(t: Template) -> DiagramSpec {
        match extract(&board_of(t), &[]).unwrap() {
            Diagram::Flow(s) => s,
            d => panic!("ノードと矢印の図ではない: {d:?}"),
        }
    }

    #[test]
    fn sequence_template_reads_lifelines_and_messages() {
        let Diagram::Sequence(s) = extract(&board_of(Template::Sequence), &[]).unwrap() else {
            panic!("シーケンス図ではない");
        };
        let names: Vec<&str> = s.participants.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(names, [":利用者", ":画面", ":API", ":データベース"]);
        let msgs: Vec<(&str, &str, &str, bool)> = s
            .messages
            .iter()
            .map(|m| (m.from.as_str(), m.to.as_str(), m.label.as_str(), m.reply))
            .collect();
        assert_eq!(
            msgs,
            [
                ("p1", "p2", "ログインする", false),
                ("p2", "p3", "POST /login", false),
                ("p3", "p4", "SELECT users", false),
                ("p4", "p3", "行", true),
                ("p3", "p2", "トークン", true),
                ("p2", "p1", "マイページ", true),
            ]
        );
    }

    #[test]
    fn class_template_reads_boxes_and_relations() {
        let s = flow_of(Template::UmlClass);
        assert_eq!(s.nodes.len(), 7);
        let customer = &s.nodes[0];
        assert_eq!(customer.kind, "class");
        assert_eq!(customer.label, "顧客");
        assert_eq!(
            customer.sections,
            ["- id: int\n- name: String", "+ order(): 注文"]
        );
        let pay = s.nodes.iter().find(|n| n.label == "支払い方法").unwrap();
        assert_eq!(pay.kind, "interface");
        assert_eq!(s.edges.len(), 6);
        let realization = s.edges.iter().filter(|e| {
            e.line
                == Some(EdgeLine {
                    start: crate::Cap::None,
                    end: crate::Cap::Triangle,
                    dashed: true,
                })
        });
        assert_eq!(realization.count(), 2);
    }

    #[test]
    fn er_template_reads_entities() {
        let s = flow_of(Template::Er);
        assert!(s.nodes.iter().all(|n| n.kind == "entity"));
        assert_eq!(s.nodes[0].label, "users");
        assert!(s.nodes[0].sections[0].starts_with("PK  id : int"));
        assert_eq!(s.edges.len(), 4);
    }

    #[test]
    fn use_case_template_reads_actors_and_frame() {
        let s = flow_of(Template::UseCase);
        assert_eq!(s.nodes.iter().filter(|n| n.kind == "actor").count(), 2);
        assert_eq!(s.groups.len(), 1);
        assert_eq!(s.groups[0].label, "オンラインショップ");
        let cases = s
            .nodes
            .iter()
            .filter(|n| n.group.as_deref() == Some(s.groups[0].id.as_str()));
        assert_eq!(cases.count(), 4);
    }

    #[test]
    fn state_machine_reads_start_and_end() {
        let s = flow_of(Template::StateMachine);
        assert!(
            s.nodes
                .iter()
                .any(|n| n.kind == "terminal" && n.label == "開始")
        );
        assert!(
            s.nodes
                .iter()
                .any(|n| n.kind == "terminal" && n.label == "終了")
        );
    }

    #[test]
    fn built_diagram_reads_back_the_same() {
        let spec = super::super::samples::aws_three_tier();
        let mut board = Board::new();
        let els = super::super::build(&spec, Vec2::ZERO, &mut || board.alloc_id());
        board.push_many(els);
        let Diagram::Flow(back) = extract(&board, &[]).unwrap() else {
            panic!()
        };
        assert_eq!(back.nodes.len(), spec.nodes.len());
        assert_eq!(back.edges.len(), spec.edges.len());
        assert_eq!(back.groups.len(), spec.groups.len());
        for b in &spec.nodes {
            let kind = catalog::node_kind(&b.kind).id;
            assert!(
                back.nodes
                    .iter()
                    .any(|a| a.kind == kind && a.label == b.label.trim()),
                "{b:?}"
            );
        }
    }
}
