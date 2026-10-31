//! 構成図・フローチャートの中間表現と、ボードの要素への変換。
//!
//! 図は「グループ(入れ子の枠)」「ノード(部品)」「矢印」の一覧で表す(`DiagramSpec`)。
//! 位置は持たず、`build` が自動でレイアウトして既存の図形・線に変換する。
//! 生成 AI の出力も、サンプルも、この JSON 形式でやりとりする。

pub mod catalog;
mod layout;
pub mod samples;

use glam::Vec2;
use serde::{Deserialize, Serialize};

use crate::color::Rgba8;
use crate::connector::{Connector, Endpoint, PathStyle};
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind};

pub use catalog::{Category, GROUP_KINDS, GroupKind, NODE_KINDS, NodeKind, NodeStyle};
pub use layout::Layout;

/// 図全体の定義。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagramSpec {
    pub title: String,
    pub direction: Direction,
    pub groups: Vec<GroupSpec>,
    pub nodes: Vec<NodeSpec>,
    pub edges: Vec<EdgeSpec>,
}

/// 矢印の基本の流れ。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// 左から右へ(構成図向け)。
    #[default]
    Right,
    /// 上から下へ(フローチャート向け)。
    Down,
}

/// 入れ子の枠(AWS Cloud / VPC / サブネットなど)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupSpec {
    pub id: String,
    /// `catalog::GROUP_KINDS` の名前。
    pub kind: String,
    pub label: String,
    /// 親グループの ID。空ならいちばん外側。
    pub parent: Option<String>,
}

/// 部品 1 つ。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeSpec {
    pub id: String,
    /// `catalog::NODE_KINDS` の名前。
    pub kind: String,
    pub label: String,
    /// 属するグループの ID。空ならいちばん外側。
    pub group: Option<String>,
}

/// 矢印。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EdgeSpec {
    pub from: String,
    pub to: String,
    pub label: String,
}

impl DiagramSpec {
    pub fn from_json(s: &str) -> Result<Self, String> {
        serde_json::from_str(s).map_err(|e| format!("図の定義を読めませんでした: {e}"))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

// ---- 見た目の定数(ワールド単位) ----

/// 矢印の色と太さ。
const EDGE_COLOR: Rgba8 = Rgba8::hex(0x545B64);
const EDGE_WIDTH: f32 = 2.0;
/// グループ枠の見出しの文字の大きさ。
const GROUP_FONT: f32 = 16.0;

/// 文字列のおおよその幅(全角 1、半角 0.6 文字分)。
pub(crate) fn text_width(s: &str, font: f32) -> f32 {
    s.chars()
        .map(|c| if c.is_ascii() { 0.6 } else { 1.0 })
        .sum::<f32>()
        * font
}

/// 幅 `inner` に折り返したときの行数。
fn line_count(text: &str, font: f32, inner: f32) -> usize {
    text.lines()
        .map(|l| ((text_width(l, font) / inner.max(1.0)).ceil() as usize).max(1))
        .sum::<usize>()
        .max(1)
}

/// ノードに表示する文字。AWS サービスは「サービス名 + 名前」の 2 行。
pub fn node_text(kind: &NodeKind, label: &str) -> String {
    let label = label.trim();
    match (kind.title, label) {
        ("", l) => l.to_string(),
        (t, "") => t.to_string(),
        (t, l) if t == l => t.to_string(),
        (t, l) => format!("{t}\n{l}"),
    }
}

/// ノード 1 つの図形を作る(位置は `pos`)。手動でパレットから置くときにも使う。
pub fn node_shape(id: ElementId, kind: &NodeKind, label: &str, pos: Vec2) -> Shape {
    let text = node_text(kind, label);
    let mut s = match kind.style {
        NodeStyle::Service => Shape {
            stroke: kind.category.color(),
            stroke_width: 3.0,
            corner_radius: 10.0,
            ..Shape::rect(id, pos, Vec2::new(180.0, 90.0), kind.category.fill())
        },
        NodeStyle::Process => Shape::rect(id, pos, Vec2::new(200.0, 90.0), Rgba8::WHITE),
        NodeStyle::Data => Shape::new(
            id,
            ShapeKind::Parallelogram,
            pos,
            Vec2::new(220.0, 90.0),
            Rgba8::hex(0xE6F2FF),
        ),
        NodeStyle::Decision => Shape::new(
            id,
            ShapeKind::Diamond,
            pos,
            Vec2::new(240.0, 140.0),
            Rgba8::hex(0xFFF6D6),
        ),
        NodeStyle::Terminal => Shape {
            // 角丸を高さの半分にした「端子」形。
            corner_radius: 38.0,
            ..Shape::rect(id, pos, Vec2::new(200.0, 76.0), Rgba8::hex(0xDFF5E1))
        },
        NodeStyle::Note => Shape {
            size: Vec2::new(200.0, 140.0),
            ..Shape::sticky(id, pos, crate::color::STICKY_COLORS[0])
        },
    };
    s.text = text;
    fit_height(&mut s);
    s
}

/// 文字が収まるように高さを伸ばす。
fn fit_height(s: &mut Shape) {
    if s.text.is_empty() {
        return;
    }
    let style = s.text_style();
    let inner_w = s.text_rect().size().x;
    let lines = line_count(&s.text, style.font_size, inner_w) as f32;
    let need = lines * style.line_height;
    let have = s.text_rect().size().y;
    if need > have {
        let extra = need - have;
        s.size.y += match s.kind {
            ShapeKind::Ellipse => extra * std::f32::consts::SQRT_2,
            ShapeKind::Diamond => extra * 2.0,
            _ => extra,
        };
    }
}

/// グループの枠(フレーム)。見出しは枠の上に出る。
pub fn group_shape(id: ElementId, kind: &GroupKind, label: &str, rect: Rect) -> Shape {
    let mut s = Shape::frame(id, rect.min, rect.size());
    s.stroke = kind.color();
    s.stroke_width = 2.0;
    s.corner_radius = 2.0;
    s.fill = kind.fill();
    s.text = group_title(kind, label);
    s.text_color = kind.color();
    s.font_size = GROUP_FONT;
    s
}

/// グループの表示名。
fn group_title(kind: &GroupKind, label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        kind.title.to_string()
    } else {
        label.to_string()
    }
}

/// 図をレイアウトして要素にする。全体の中心が `center` に来る。
/// 要素は重なり順(奥から)に並ぶ: 枠(外側から)→ ノード → 矢印(コネクタ)。
/// 矢印はノードにつながっているので、あとでノードを動かしても追従する。
pub fn build(
    spec: &DiagramSpec,
    center: Vec2,
    alloc: &mut dyn FnMut() -> ElementId,
) -> Vec<Element> {
    let layout = Layout::compute(spec);
    let offset = center - layout.bounds.center();
    let mut out: Vec<Element> = Vec::new();

    for g in &layout.groups {
        let rect = Rect::new(g.rect.min + offset, g.rect.max + offset);
        out.push(group_shape(alloc(), g.kind, &g.label, rect).into());
    }

    let mut ids = Vec::with_capacity(layout.nodes.len());
    for n in &layout.nodes {
        let mut s = n.shape.clone();
        s.id = alloc();
        s.pos += offset;
        ids.push(s.id);
        out.push(s.into());
    }

    for e in &layout.edges {
        let attach = |i: usize| Endpoint::Attached {
            id: ids[i],
            anchor: None,
        };
        let mut c = Connector::new(alloc(), attach(e.from), attach(e.to));
        c.color = EDGE_COLOR;
        c.width = EDGE_WIDTH;
        c.style = PathStyle::Elbow;
        c.text = e.label.trim().to_string();
        let rect_of = |id: ElementId| {
            out.iter()
                .find(|el| el.id() == id)
                .and_then(crate::connector::Target::of)
        };
        c.route(rect_of);
        out.push(c.into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> impl FnMut() -> ElementId {
        let mut n = 0;
        move || {
            n += 1;
            ElementId(n)
        }
    }

    #[test]
    fn json_round_trip_and_defaults() {
        let s = DiagramSpec::from_json(
            r#"{"nodes":[{"id":"a","kind":"ec2"},{"id":"b","label":"x"}],
                "edges":[{"from":"a","to":"b"}]}"#,
        )
        .unwrap();
        assert_eq!(s.direction, Direction::Right);
        assert_eq!(s.nodes[1].kind, "");
        let again = DiagramSpec::from_json(&s.to_json()).unwrap();
        assert_eq!(again, s);
        assert!(DiagramSpec::from_json("{").is_err());
    }

    #[test]
    fn node_text_combines_service_and_label() {
        let ec2 = catalog::node_kind("ec2");
        assert_eq!(node_text(ec2, "Web"), "EC2\nWeb");
        assert_eq!(node_text(ec2, ""), "EC2");
        assert_eq!(node_text(catalog::node_kind("process"), "処理"), "処理");
    }

    #[test]
    fn long_text_grows_node() {
        let k = catalog::node_kind("process");
        let short = node_shape(ElementId(1), k, "短い", Vec2::ZERO);
        let long = node_shape(ElementId(1), k, &"とても長い説明文".repeat(6), Vec2::ZERO);
        assert!(long.size.y > short.size.y);
    }

    #[test]
    fn build_orders_groups_nodes_connectors() {
        let spec = samples::aws_three_tier();
        let mut alloc = ids();
        let els = build(&spec, Vec2::new(500.0, 500.0), &mut alloc);
        let ng = spec.groups.len();
        let nn = spec.nodes.len();
        assert_eq!(els.len(), ng + nn + spec.edges.len());
        assert!(
            els[..ng]
                .iter()
                .all(|e| e.as_shape().is_some_and(|s| s.kind == ShapeKind::Frame))
        );
        assert!(els[ng..ng + nn].iter().all(|e| e.as_shape().is_some()));
        // 矢印は両端ともノードにつながる。
        let node_ids: Vec<_> = els[ng..ng + nn].iter().map(Element::id).collect();
        for e in &els[ng + nn..] {
            let Element::Connector(c) = e else {
                panic!("コネクタではない: {e:?}");
            };
            assert!(node_ids.contains(&c.from.attached_to().unwrap()));
            assert!(node_ids.contains(&c.to.attached_to().unwrap()));
            assert!(c.path.len() >= 2);
        }
        // ID は重複しない。
        let mut v: Vec<_> = els.iter().map(Element::id).collect();
        v.sort();
        v.dedup();
        assert_eq!(v.len(), els.len());
        // 全体の中心が指定位置。
        let b = els[..ng + nn]
            .iter()
            .map(Element::bounds)
            .reduce(|a, b| a.union(&b))
            .unwrap();
        assert!((b.center() - Vec2::new(500.0, 500.0)).length() < 40.0);
    }

    #[test]
    fn edges_become_board_connectors() {
        let spec = samples::aws_three_tier();
        let mut board = crate::Board::new();
        let els = build(&spec, Vec2::ZERO, &mut || board.alloc_id());
        for e in els {
            board.push(e);
        }
        let n = board
            .elements()
            .iter()
            .filter(|e| matches!(e, Element::Connector(_)))
            .count();
        assert_eq!(n, spec.edges.len());
    }
}
