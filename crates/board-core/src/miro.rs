//! Miro のボードの書き出し(REST API の JSON)の取り込み。
//!
//! Miro の REST API(`GET /v2/boards/{id}/items` と `/connectors`)が返す JSON を読み、
//! 付箋・図形・文字・カード・フレームと、それらをつなぐ線を位置そのままでボードの要素にする。
//! 受け付ける形: `{"data": [...]}`、配列そのもの、`{"items": ..., "connectors": ...}`。
//! (Miro の付箋の CSV 書き出しは CSV の取り込みで読める。)

use std::collections::HashMap;

use glam::Vec2;
use serde_json::Value;

use crate::board::Board;
use crate::color::Rgba8;
use crate::connector::{Cap, Connector, Endpoint, PathStyle, Target};
use crate::drawio::{color, plain_text};
use crate::element::{Element, ElementId};
use crate::shape::{Shape, ShapeKind, TextAlign};

/// Miro の JSON らしい文字列か。
pub fn looks_like_miro(text: &str) -> bool {
    let t = text.trim_start();
    (t.starts_with('{') || t.starts_with('['))
        && (t.contains("\"sticky_note\"")
            || t.contains("\"startItem\"")
            || (t.contains("\"position\"") && t.contains("\"geometry\"") && t.contains("\"type\"")))
}

/// 付箋の色の名前 → 色。
fn sticky_color(name: &str) -> Rgba8 {
    Rgba8::hex(match name {
        "gray" => 0xe6e6e6,
        "light_yellow" => 0xfff9b1,
        "yellow" => 0xf5d128,
        "orange" => 0xff9d48,
        "light_green" => 0xd5f692,
        "green" => 0xc9df56,
        "dark_green" => 0x93d275,
        "cyan" => 0x67c6c0,
        "light_pink" => 0xffcee0,
        "pink" => 0xea94bb,
        "violet" => 0xc6a2d2,
        "red" => 0xf0939d,
        "light_blue" => 0xa6ccf5,
        "blue" => 0x6cd8fa,
        "dark_blue" => 0x9ea9ff,
        "black" => 0x000000,
        _ => 0xfff9b1,
    })
}

fn shape_kind(name: &str) -> ShapeKind {
    match name {
        "round_rectangle" => ShapeKind::RoundRect,
        "circle" | "flow_chart_connector" => ShapeKind::Ellipse,
        "triangle" => ShapeKind::Triangle,
        "rhombus" | "flow_chart_decision" => ShapeKind::Diamond,
        "parallelogram" | "flow_chart_input_output" => ShapeKind::Parallelogram,
        "trapezoid" => ShapeKind::Trapezoid,
        "pentagon" => ShapeKind::Pentagon,
        "hexagon" | "flow_chart_preparation" => ShapeKind::Hexagon,
        "octagon" => ShapeKind::Octagon,
        "star" => ShapeKind::Star,
        "cloud" => ShapeKind::Cloud,
        "cross" => ShapeKind::Cross,
        "can" | "flow_chart_magnetic_disk" => ShapeKind::Cylinder,
        "right_arrow" => ShapeKind::ArrowRight,
        "left_arrow" => ShapeKind::ArrowLeft,
        "left_right_arrow" => ShapeKind::ArrowBoth,
        "wedge_round_rectangle_callout" => ShapeKind::Speech,
        "flow_chart_terminator" => ShapeKind::Pill,
        "flow_chart_predefined_process" => ShapeKind::PredefinedProcess,
        "flow_chart_document" => ShapeKind::Document,
        "flow_chart_manual_input" => ShapeKind::ManualInput,
        "flow_chart_delay" => ShapeKind::Delay,
        "flow_chart_offpage_connector" => ShapeKind::OffPage,
        "flow_chart_internal_storage" => ShapeKind::InternalStorage,
        "flow_chart_punched_card" => ShapeKind::Card,
        _ => ShapeKind::Rect,
    }
}

fn str_of<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_str()
}

fn num_of(v: &Value, path: &[&str]) -> Option<f32> {
    let v = path.iter().try_fold(v, |v, k| v.get(k))?;
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .map(|f| f as f32)
}

fn cap(name: Option<&str>) -> Cap {
    match name {
        Some("none") | None => Cap::None,
        Some("stealth") | Some("arrow") | Some("open_arrow") => Cap::Arrow,
        Some("filled_triangle") => Cap::TriangleFilled,
        Some("triangle") => Cap::Triangle,
        Some("filled_diamond") => Cap::DiamondFilled,
        Some("diamond") => Cap::Diamond,
        Some("filled_oval") | Some("oval") => Cap::Circle,
        Some("erd_one") | Some("erd_only_one") => Cap::One,
        Some("erd_many") => Cap::Many,
        Some("erd_zero_or_one") => Cap::ZeroOrOne,
        Some("erd_one_or_many") => Cap::OneOrMany,
        Some("erd_zero_or_many") => Cap::ZeroOrMany,
        Some(_) => Cap::Arrow,
    }
}

/// 配列か、`data` に配列を持つオブジェクトの中身。
fn take(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => match v.get("data") {
            Some(Value::Array(a)) => a.iter().collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// JSON から項目と線の一覧を取り出す。
fn lists(root: &Value) -> (Vec<&Value>, Vec<&Value>) {
    let mut items = Vec::new();
    let mut connectors = Vec::new();
    let mut all = Vec::new();
    if let Some(i) = root.get("items") {
        all.extend(take(i));
    }
    if let Some(c) = root.get("connectors") {
        all.extend(take(c));
    }
    if all.is_empty() {
        all = take(root);
    }
    for v in all {
        if v.get("startItem").is_some()
            || v.get("type").and_then(Value::as_str) == Some("connector")
        {
            connectors.push(v);
        } else {
            items.push(v);
        }
    }
    (items, connectors)
}

/// Miro の JSON を読み、中心 `center` に置く。置いた要素の ID を返す。
pub fn import(board: &mut Board, json: &str, center: Vec2) -> Result<Vec<ElementId>, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|e| format!("Miro の JSON を読めません: {e}"))?;
    let (items, connectors) = lists(&root);
    let by_id: HashMap<&str, &Value> = items
        .iter()
        .filter_map(|v| Some((v.get("id")?.as_str()?, *v)))
        .collect();
    // 項目の中心(絶対座標)。親(フレーム)からの位置は親の左上に足す。
    fn center_of(v: &Value, by_id: &HashMap<&str, &Value>, depth: usize) -> Vec2 {
        let p = Vec2::new(
            num_of(v, &["position", "x"]).unwrap_or(0.0),
            num_of(v, &["position", "y"]).unwrap_or(0.0),
        );
        let relative = str_of(v, &["position", "relativeTo"]) == Some("parent_top_left");
        let parent = str_of(v, &["parent", "id"]).and_then(|id| by_id.get(id));
        match parent {
            Some(par) if relative && depth < 16 => {
                let size = Vec2::new(
                    num_of(par, &["geometry", "width"]).unwrap_or(0.0),
                    num_of(par, &["geometry", "height"]).unwrap_or(0.0),
                );
                center_of(par, by_id, depth + 1) - size * 0.5 + p
            }
            _ => p,
        }
    }
    let mut frames: Vec<Element> = Vec::new();
    let mut content: Vec<Element> = Vec::new();
    let mut ids: HashMap<&str, ElementId> = HashMap::new();
    for v in &items {
        let kind = str_of(v, &["type"]).unwrap_or("");
        let text = plain_text(
            str_of(v, &["data", "content"])
                .or_else(|| str_of(v, &["data", "title"]))
                .unwrap_or(""),
            true,
        );
        let w = num_of(v, &["geometry", "width"]).unwrap_or(0.0);
        let h = num_of(v, &["geometry", "height"]).unwrap_or(0.0);
        let mid = center_of(v, &by_id, 0);
        let fill_name = str_of(v, &["style", "fillColor"]).map(str::to_string);
        let fill_hex = fill_name.as_ref().filter(|c| c.starts_with('#'));
        let id = board.alloc_id();
        let mut s = match kind {
            "sticky_note" => {
                let side = if w > 0.0 { w } else { 200.0 };
                let height = if h > 0.0 { h } else { side };
                let fill = fill_hex
                    .map(|c| color(Some(c), Rgba8::hex(0xfff9b1)))
                    .unwrap_or_else(|| sticky_color(fill_name.as_deref().unwrap_or("")));
                let mut s = Shape::sticky(id, Vec2::ZERO, fill);
                s.size = Vec2::new(side, height);
                s
            }
            "shape" => {
                let k = shape_kind(str_of(v, &["data", "shape"]).unwrap_or("rectangle"));
                let mut s = Shape::new(
                    id,
                    k,
                    Vec2::ZERO,
                    Vec2::new(w.max(1.0), h.max(1.0)),
                    color(fill_hex, Rgba8::WHITE),
                );
                s.stroke = color(
                    str_of(v, &["style", "borderColor"])
                        .map(str::to_string)
                        .as_ref(),
                    Rgba8::hex(0x1a1a1a),
                );
                s.stroke_width = num_of(v, &["style", "borderWidth"]).unwrap_or(2.0);
                if str_of(v, &["style", "borderOpacity"]) == Some("0.0") {
                    s.stroke = Rgba8::TRANSPARENT;
                }
                s
            }
            "text" => {
                let mut s = Shape::text(id, Vec2::ZERO, Vec2::new(w.max(40.0), h.max(24.0)));
                s.fill = color(fill_hex, Rgba8::TRANSPARENT);
                s
            }
            "card" | "app_card" => {
                let mut s = Shape::rect(
                    id,
                    Vec2::ZERO,
                    Vec2::new(w.max(120.0), h.max(60.0)),
                    Rgba8::WHITE,
                );
                s.stroke = Rgba8::hex(0x2d9bf0);
                s.align = TextAlign::Left;
                let desc = plain_text(str_of(v, &["data", "description"]).unwrap_or(""), true);
                let text = [text.clone(), desc]
                    .into_iter()
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                s.text = text.clone();
                s
            }
            "frame" => {
                let mut s = Shape::frame(id, Vec2::ZERO, Vec2::new(w.max(1.0), h.max(1.0)));
                s.fill = color(fill_hex, Rgba8::WHITE);
                s.text = text.clone();
                s
            }
            // 画像・埋め込み・文書などは場所だけ分かる四角形にする。
            "image" | "embed" | "document" | "preview" => {
                let mut s = Shape::rect(
                    id,
                    Vec2::ZERO,
                    Vec2::new(w.max(80.0), h.max(60.0)),
                    Rgba8::hex(0xf1f3f4),
                );
                s.stroke = Rgba8::hex(0xb8bcc2);
                s.stroke_width = 1.0;
                s.corner_radius = 0.0;
                if let Some(url) = str_of(v, &["data", "imageUrl"])
                    .or_else(|| str_of(v, &["data", "url"]))
                    .or_else(|| str_of(v, &["data", "documentUrl"]))
                {
                    s.meta.link = url.to_string();
                }
                s.text = if text.is_empty() {
                    kind.to_string()
                } else {
                    text.clone()
                };
                s
            }
            _ => continue,
        };
        if s.text.is_empty() {
            s.text = text;
        }
        if let Some(c) = str_of(v, &["style", "color"]) {
            s.text_color = color(Some(&c.to_string()), Rgba8::BLACK);
        }
        if let Some(f) = num_of(v, &["style", "fontSize"]) {
            s.font_size = f;
        }
        match str_of(v, &["style", "textAlign"]) {
            Some("left") => s.align = TextAlign::Left,
            Some("right") => s.align = TextAlign::Right,
            Some("center") => s.align = TextAlign::Center,
            _ => {}
        }
        s.pos = mid - s.size * 0.5;
        if let Some(i) = v.get("id").and_then(Value::as_str) {
            ids.insert(i, s.id);
        }
        if s.kind == ShapeKind::Frame {
            frames.push(s.into());
        } else {
            content.push(s.into());
        }
    }
    let mut elements = frames;
    elements.extend(content);
    for c in connectors {
        let end = |key: &str| -> Option<Endpoint> {
            let id = str_of(c, &[key, "id"])?;
            let target = ids.get(id)?;
            let anchor = match (
                num_of(c, &[key, "position", "x"]),
                num_of(c, &[key, "position", "y"]),
            ) {
                (Some(x), Some(y)) => {
                    // "50%" のような書き方。
                    Some(Vec2::new(x, y) / if x > 1.0 || y > 1.0 { 100.0 } else { 1.0 })
                }
                _ => percent_anchor(c, key),
            };
            Some(Endpoint::Attached {
                id: *target,
                anchor,
            })
        };
        let (Some(from), Some(to)) = (end("startItem"), end("endItem")) else {
            continue;
        };
        let mut k = Connector::new(board.alloc_id(), from, to);
        k.style = match str_of(c, &["shape"]) {
            Some("straight") => PathStyle::Straight,
            Some("curved") => PathStyle::Curved,
            _ => PathStyle::Elbow,
        };
        k.color = color(
            str_of(c, &["style", "strokeColor"])
                .map(str::to_string)
                .as_ref(),
            Rgba8::hex(0x1a1a1a),
        );
        k.width = num_of(c, &["style", "strokeWidth"]).unwrap_or(2.0);
        k.start_cap = cap(str_of(c, &["style", "startStrokeCap"]));
        k.end_cap = match str_of(c, &["style", "endStrokeCap"]) {
            None => Cap::Arrow,
            other => cap(other),
        };
        k.dashed = matches!(
            str_of(c, &["style", "strokeStyle"]),
            Some("dashed") | Some("dotted")
        );
        k.text = c
            .get("captions")
            .and_then(Value::as_array)
            .map(|caps| {
                caps.iter()
                    .filter_map(|cap| cap.get("content").and_then(Value::as_str))
                    .map(|t| plain_text(t, true))
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        elements.push(k.into());
    }
    let bounds = elements
        .iter()
        .filter(|e| !matches!(e, Element::Connector(_)))
        .map(Element::bounds)
        .reduce(|a, b| a.union(&b))
        .ok_or("Miro の JSON に置けるものがありません")?;
    let d = center - bounds.center();
    for e in &mut elements {
        e.translate(d);
    }
    let snapshot = elements.clone();
    for e in &mut elements {
        if let Element::Connector(c) = e {
            c.route(|id| snapshot.iter().find(|s| s.id() == id).and_then(Target::of));
        }
    }
    let ids = elements.iter().map(Element::id).collect();
    board.push_many(elements);
    Ok(ids)
}

/// `{"position": {"x": "50%", "y": "0%"}}` の形の接続位置。
fn percent_anchor(c: &Value, key: &str) -> Option<Vec2> {
    let p = |axis: &str| -> Option<f32> {
        let s = str_of(c, &[key, "position", axis])?;
        Some(s.trim_end_matches('%').parse::<f32>().ok()? / 100.0)
    };
    Some(Vec2::new(p("x")?, p("y")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"{
  "items": {"data": [
    {"id": "f1", "type": "frame", "data": {"title": "アイデア"},
     "position": {"x": 0, "y": 0, "origin": "center", "relativeTo": "canvas_center"},
     "geometry": {"width": 800, "height": 600}},
    {"id": "s1", "type": "sticky_note", "data": {"content": "<p>朝会を短く</p>", "shape": "square"},
     "style": {"fillColor": "light_pink", "textAlign": "center"},
     "position": {"x": 150, "y": 150, "origin": "center", "relativeTo": "parent_top_left"},
     "geometry": {"width": 199}, "parent": {"id": "f1"}},
    {"id": "r1", "type": "shape", "data": {"content": "<p>DB</p>", "shape": "can"},
     "style": {"fillColor": "#ffffff", "borderColor": "#1a1a1a"},
     "position": {"x": 1000, "y": 0, "origin": "center", "relativeTo": "canvas_center"},
     "geometry": {"width": 120, "height": 160}},
    {"id": "t1", "type": "text", "data": {"content": "メモ"},
     "position": {"x": 0, "y": -400}, "geometry": {"width": 200}}
  ]},
  "connectors": {"data": [
    {"id": "c1", "startItem": {"id": "s1"}, "endItem": {"id": "r1", "position": {"x": "0%", "y": "50%"}},
     "shape": "curved", "style": {"endStrokeCap": "filled_triangle", "strokeStyle": "dashed"},
     "captions": [{"content": "保存"}]}
  ]}
}"##;

    #[test]
    fn imports_items_and_connectors() {
        assert!(looks_like_miro(SAMPLE));
        let mut board = Board::new();
        let ids = import(&mut board, SAMPLE, Vec2::ZERO).unwrap();
        assert_eq!(ids.len(), 5);
        let shapes: Vec<&Shape> = board
            .elements()
            .iter()
            .filter_map(Element::as_shape)
            .collect();
        assert_eq!(shapes[0].kind, ShapeKind::Frame);
        assert_eq!(shapes[0].text, "アイデア");
        let note = shapes.iter().find(|s| s.kind == ShapeKind::Sticky).unwrap();
        assert_eq!(note.text, "朝会を短く");
        assert_eq!(note.fill, Rgba8::hex(0xffcee0));
        // フレームの左上から (150, 150) が付箋の中心。
        assert_eq!(
            note.bounds().center() - shapes[0].pos,
            Vec2::new(150.0, 150.0)
        );
        let db = shapes.iter().find(|s| s.text == "DB").unwrap();
        assert_eq!(db.kind, ShapeKind::Cylinder);
        let Some(Element::Connector(c)) = board.elements().last() else {
            panic!("線がない");
        };
        assert_eq!(c.style, PathStyle::Curved);
        assert_eq!(c.end_cap, Cap::TriangleFilled);
        assert!(c.dashed);
        assert_eq!(c.text, "保存");
        assert!(
            matches!(c.to, Endpoint::Attached { anchor: Some(a), .. } if a == Vec2::new(0.0, 0.5))
        );
    }

    #[test]
    fn plain_array_and_errors() {
        let mut board = Board::new();
        let json = r#"[{"id":"1","type":"sticky_note","data":{"content":"a"},"position":{"x":0,"y":0},"geometry":{"width":100}}]"#;
        assert_eq!(import(&mut board, json, Vec2::ZERO).unwrap().len(), 1);
        assert!(import(&mut board, "[]", Vec2::ZERO).is_err());
        assert!(import(&mut board, "{", Vec2::ZERO).is_err());
        assert!(!looks_like_miro("{\"a\": 1}"));
    }
}
