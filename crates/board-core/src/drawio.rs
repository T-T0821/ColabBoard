//! draw.io(diagrams.net)のファイルの取り込み。
//!
//! `.drawio` / `.xml`(`<mxfile>`、圧縮された中身も可)、`<mxGraphModel>` だけの XML、
//! `.drawio.svg`(SVG に埋め込まれた図)を読み、図形と線を位置そのままでボードの要素にする。
//!
//! - 図形: スタイルの形(`ellipse` `rhombus` `cylinder` など)を近い種類に、`text` は文字、
//!   `swimlane` やグループ・コンテナはフレームにする。塗り・線・文字の色と大きさ、揃えも写す。
//! - 線: つながった図形にはそのままつなぐ(出入りの位置 `exitX` `entryX` も)。
//!   直角・曲線・直線、矢じり(ER 図の記号も)、破線、ラベルを写す。
//! - ページが複数あれば、ページごとにフレームに入れて横に並べる。
//!
//! 書き出し([`export`])は逆向きで、図形・線・手書き線を位置と見た目のまま
//! 1 ページの `.drawio` にする。書き出したファイルを取り込むと同じ図に戻る。

use std::collections::HashMap;

use glam::Vec2;
use quick_xml::events::{BytesStart, Event};

use crate::board::Board;
use crate::color::Rgba8;
use crate::connector::{Cap, Connector, Endpoint, PathStyle, Target};
use crate::element::{Element, ElementId};
use crate::shape::{Shape, ShapeKind, TextAlign, VAlign};

/// ページどうしの隙間。
const PAGE_GAP: f32 = 160.0;
/// ページを入れるフレームの余白。
const PAGE_PAD: f32 = 40.0;

/// draw.io の図らしい文字列か。
pub fn looks_like_drawio(text: &str) -> bool {
    let head: String = text.chars().take(4000).collect();
    head.contains("<mxfile")
        || head.contains("<mxGraphModel")
        || head.contains("&lt;mxfile")
        || is_url_encoded(text)
}

/// draw.io から図形をコピーしたときのクリップボード(URL エンコードされた XML)か。
fn is_url_encoded(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("%3CmxGraphModel") || t.starts_with("%3Cmxfile")
}

#[derive(Clone, Debug, Default)]
struct Geometry {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Clone, Debug, Default)]
struct Cell {
    id: String,
    value: String,
    style: String,
    vertex: bool,
    edge: bool,
    parent: String,
    source: String,
    target: String,
    /// `UserObject` に付いたリンク。
    link: String,
    geometry: Option<Geometry>,
    source_point: Option<Vec2>,
    target_point: Option<Vec2>,
}

fn attrs(e: &BytesStart) -> HashMap<String, String> {
    e.attributes()
        .flatten()
        .filter_map(|a| {
            let key = a.key.as_ref().to_string();
            let value = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()?
                .into_owned();
            Some((key, value))
        })
        .collect()
}

fn num(map: &HashMap<String, String>, key: &str) -> f32 {
    map.get(key).and_then(|v| v.parse().ok()).unwrap_or(0.0)
}

/// 圧縮された図(base64 → deflate → URL エンコード)を XML に戻す。
fn inflate(data: &str) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|e| format!("圧縮された図を読めません: {e}"))?;
    let raw = miniz_oxide::inflate::decompress_to_vec(&bytes)
        .map_err(|e| format!("圧縮された図を展開できません: {e:?}"))?;
    let text = String::from_utf8_lossy(&raw);
    Ok(percent_encoding::percent_decode_str(&text)
        .decode_utf8_lossy()
        .into_owned())
}

/// ページ(名前と `mxGraphModel` の XML)を取り出す。
fn pages(xml: &str) -> Result<Vec<(String, String)>, String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    let mut current: Option<(String, usize)> = None;
    loop {
        let before = reader.buffer_position() as usize;
        let ev = reader
            .read_event()
            .map_err(|e| format!("draw.io のファイルを読めません: {e}"))?;
        match ev {
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == "svg" => {
                // SVG に埋め込まれた図(`content` 属性に mxfile がある)。
                if let Some(content) = attrs(&e).get("content") {
                    return pages(content);
                }
            }
            Event::Start(e) if e.name().as_ref() == "mxGraphModel" && current.is_none() => {
                // ページに包まれていない図。
                return Ok(vec![(String::new(), xml[before..].to_string())]);
            }
            Event::Start(e) if e.name().as_ref() == "diagram" => {
                let name = attrs(&e).get("name").cloned().unwrap_or_default();
                current = Some((name, reader.buffer_position() as usize));
            }
            Event::End(e) if e.name().as_ref() == "diagram" => {
                if let Some((name, start)) = current.take() {
                    let inner = xml[start..before].trim();
                    let model = if inner.starts_with('<') {
                        inner.to_string()
                    } else if inner.is_empty() {
                        continue;
                    } else {
                        inflate(inner)?
                    };
                    out.push((name, model));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if out.is_empty() {
        return Err("draw.io の図が見つかりません".into());
    }
    Ok(out)
}

/// `mxGraphModel` のセルを読む。
fn cells(model: &str) -> Result<Vec<Cell>, String> {
    let mut reader = quick_xml::Reader::from_str(model);
    let mut out: Vec<Cell> = Vec::new();
    // `<UserObject label=".." id="..">` の包み。
    let mut wrapper: Option<HashMap<String, String>> = None;
    let mut in_cell = false;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| format!("draw.io の図を読めません: {e}"))?;
        let (e, empty) = match &ev {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) => {
                match e.name().as_ref() {
                    "mxCell" => in_cell = false,
                    "UserObject" | "object" => wrapper = None,
                    _ => {}
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let a = attrs(e);
        match e.name().as_ref() {
            "UserObject" | "object" => wrapper = Some(a),
            "mxCell" => {
                let w = wrapper.clone().unwrap_or_default();
                let get = |k: &str| a.get(k).or_else(|| w.get(k)).cloned().unwrap_or_default();
                let value = a
                    .get("value")
                    .cloned()
                    .or_else(|| w.get("label").cloned())
                    .unwrap_or_default();
                out.push(Cell {
                    id: get("id"),
                    value,
                    style: get("style"),
                    vertex: a.get("vertex").is_some_and(|v| v == "1"),
                    edge: a.get("edge").is_some_and(|v| v == "1"),
                    parent: get("parent"),
                    source: get("source"),
                    target: get("target"),
                    link: w.get("link").cloned().unwrap_or_default(),
                    ..Cell::default()
                });
                in_cell = !empty;
            }
            "mxGeometry" if in_cell => {
                if let Some(c) = out.last_mut() {
                    c.geometry = Some(Geometry {
                        x: num(&a, "x"),
                        y: num(&a, "y"),
                        w: num(&a, "width"),
                        h: num(&a, "height"),
                    });
                }
            }
            "mxPoint" if in_cell => {
                let p = Vec2::new(num(&a, "x"), num(&a, "y"));
                if let Some(c) = out.last_mut() {
                    match a.get("as").map(String::as_str) {
                        Some("sourcePoint") => c.source_point = Some(p),
                        Some("targetPoint") => c.target_point = Some(p),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// スタイル文字列(`ellipse;whiteSpace=wrap;fillColor=#fff;`)。最初の `=` のない語は形の名前。
fn parse_style(style: &str) -> (String, HashMap<String, String>) {
    let mut base = String::new();
    let mut map = HashMap::new();
    for (i, part) in style.split(';').enumerate() {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            Some((k, v)) => {
                map.insert(k.to_string(), v.to_string());
            }
            None if i == 0 => base = part.to_string(),
            None => {
                map.insert(part.to_string(), "1".into());
            }
        }
    }
    (base, map)
}

pub(crate) fn color(v: Option<&String>, default: Rgba8) -> Rgba8 {
    let Some(v) = v else {
        return default;
    };
    let v = v.trim();
    if v == "none" {
        return Rgba8::TRANSPARENT;
    }
    let hex = v.trim_start_matches('#');
    let full = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => hex.to_string(),
        _ => return default,
    };
    u32::from_str_radix(&full, 16)
        .map(Rgba8::hex)
        .unwrap_or(default)
}

/// HTML のラベルを文字にする(`<br>` `<div>` は改行、ほかのタグは消す)。
pub(crate) fn plain_text(value: &str, html: bool) -> String {
    if !html {
        return value.trim().to_string();
    }
    let mut out = String::new();
    let mut rest = value;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[i + 1..i + j].trim().to_ascii_lowercase();
        let name = tag
            .trim_start_matches('/')
            .split([' ', '/'])
            .next()
            .unwrap_or("");
        let breaks = matches!(name, "br" | "p" | "div" | "li" | "tr")
            && (name == "br" || tag.starts_with('/'));
        if breaks && !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

/// 形の名前 → 図形の種類。
fn shape_kind(base: &str, style: &HashMap<String, String>) -> ShapeKind {
    let shape = style.get("shape").map(String::as_str).unwrap_or(base);
    let name = shape.rsplit('.').next().unwrap_or(shape);
    match name {
        "ellipse" | "doubleEllipse" | "umlActor" | "actor" => ShapeKind::Ellipse,
        "rhombus" => ShapeKind::Diamond,
        "triangle" => ShapeKind::Triangle,
        "hexagon" => ShapeKind::Hexagon,
        "parallelogram" => ShapeKind::Parallelogram,
        "trapezoid" => ShapeKind::Trapezoid,
        "cylinder" | "cylinder2" | "cylinder3" | "datastore" => ShapeKind::Cylinder,
        "document" => ShapeKind::Document,
        "process" => ShapeKind::PredefinedProcess,
        "cloud" => ShapeKind::Cloud,
        "step" => ShapeKind::Chevron,
        "card" => ShapeKind::Card,
        "note" | "note2" => ShapeKind::Sticky,
        "cross" => ShapeKind::Cross,
        "callout" => ShapeKind::Speech,
        "singleArrow" => ShapeKind::ArrowRight,
        "doubleArrow" => ShapeKind::ArrowBoth,
        "manualInput" => ShapeKind::ManualInput,
        "delay" => ShapeKind::Delay,
        "offPageConnector" => ShapeKind::OffPage,
        "internalStorage" => ShapeKind::InternalStorage,
        "text" | "label" => ShapeKind::Text,
        "swimlane" | "table" | "group" => ShapeKind::Frame,
        _ if style.get("container").is_some_and(|v| v == "1") => ShapeKind::Frame,
        _ if style.get("rounded").is_some_and(|v| v == "1") => ShapeKind::RoundRect,
        _ => ShapeKind::Rect,
    }
}

fn cap(name: Option<&String>, fill: Option<&String>, default: Cap) -> Cap {
    let filled = fill.is_none_or(|f| f != "0");
    match name.map(String::as_str) {
        None => default,
        Some("none") | Some("") => Cap::None,
        Some("block") | Some("classic") | Some("classicThin") | Some("blockThin") => {
            if filled {
                Cap::TriangleFilled
            } else {
                Cap::Triangle
            }
        }
        Some("open") | Some("openThin") | Some("openAsync") => Cap::Arrow,
        Some("diamond") | Some("diamondThin") => {
            if filled {
                Cap::DiamondFilled
            } else {
                Cap::Diamond
            }
        }
        Some("oval") | Some("circle") | Some("circlePlus") => Cap::Circle,
        Some("ERone") | Some("ERmandOne") => Cap::One,
        Some("ERmany") => Cap::Many,
        Some("ERzeroToOne") => Cap::ZeroOrOne,
        Some("ERoneToMany") => Cap::OneOrMany,
        Some("ERzeroToMany") => Cap::ZeroOrMany,
        Some(_) => Cap::Arrow,
    }
}

/// 1 ページ分を要素にする(座標は draw.io のまま)。
fn page_elements(cells: &[Cell], alloc: &mut dyn FnMut() -> ElementId) -> Vec<Element> {
    let by_id: HashMap<&str, &Cell> = cells.iter().map(|c| (c.id.as_str(), c)).collect();
    // セルの絶対位置(親が図形なら親の位置を足す)。
    fn origin(id: &str, by_id: &HashMap<&str, &Cell>, depth: usize) -> Vec2 {
        let Some(c) = by_id.get(id) else {
            return Vec2::ZERO;
        };
        if !c.vertex || depth > 32 {
            return Vec2::ZERO;
        }
        let g = c.geometry.clone().unwrap_or_default();
        origin(&c.parent, by_id, depth + 1) + Vec2::new(g.x, g.y)
    }
    let mut out: Vec<Element> = Vec::new();
    let mut ids: HashMap<&str, ElementId> = HashMap::new();
    // 線のラベル(線を親にした文字)。
    let mut edge_labels: HashMap<&str, Vec<String>> = HashMap::new();
    for c in cells.iter().filter(|c| c.vertex) {
        let (base, style) = parse_style(&c.style);
        let html = style.get("html").is_some_and(|v| v == "1");
        let text = plain_text(&c.value, html);
        if by_id.get(c.parent.as_str()).is_some_and(|p| p.edge) {
            if !text.is_empty() {
                edge_labels.entry(c.parent.as_str()).or_default().push(text);
            }
            continue;
        }
        let Some(g) = c.geometry.clone() else {
            continue;
        };
        if g.w <= 0.0 || g.h <= 0.0 {
            continue;
        }
        let pos = origin(&c.parent, &by_id, 0) + Vec2::new(g.x, g.y);
        let size = Vec2::new(g.w, g.h);
        let kind = shape_kind(&base, &style);
        let fill_default = match kind {
            ShapeKind::Text => Rgba8::TRANSPARENT,
            ShapeKind::Sticky => Rgba8::hex(0xfff2cc),
            _ => Rgba8::WHITE,
        };
        let mut s = Shape::new(
            alloc(),
            kind,
            pos,
            size,
            color(style.get("fillColor"), fill_default),
        );
        if kind != ShapeKind::Text {
            s.stroke = color(style.get("strokeColor"), Rgba8::BLACK);
            s.stroke_width = style
                .get("strokeWidth")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0);
        }
        if kind == ShapeKind::Rect {
            s.corner_radius = 0.0;
        }
        s.text = text;
        s.text_color = color(style.get("fontColor"), Rgba8::BLACK);
        s.font_size = style
            .get("fontSize")
            .and_then(|v| v.parse().ok())
            .unwrap_or(12.0);
        s.align = match style.get("align").map(String::as_str) {
            Some("left") => TextAlign::Left,
            Some("right") => TextAlign::Right,
            _ if kind == ShapeKind::Frame => TextAlign::Left,
            _ => TextAlign::Center,
        };
        s.valign = match style.get("verticalAlign").map(String::as_str) {
            // フレームの見出しは枠の上(draw.io では枠の外に出すため bottom にしてある)。
            _ if kind == ShapeKind::Frame => VAlign::Top,
            Some("top") => VAlign::Top,
            Some("bottom") => VAlign::Bottom,
            _ => VAlign::Middle,
        };
        if c.link.starts_with("http") {
            s.meta.link = c.link.clone();
        }
        ids.insert(c.id.as_str(), s.id);
        out.push(s.into());
    }
    let mut connectors = Vec::new();
    for c in cells.iter().filter(|c| c.edge) {
        let (base, style) = parse_style(&c.style);
        let anchor = |x: &str, y: &str| -> Option<Vec2> {
            let (x, y) = (style.get(x)?.parse().ok()?, style.get(y)?.parse().ok()?);
            Some(Vec2::new(x, y))
        };
        let offset = origin(&c.parent, &by_id, 0);
        let end = |cell: &str, point: Option<Vec2>, a: Option<Vec2>| -> Option<Endpoint> {
            match ids.get(cell) {
                Some(id) => Some(Endpoint::Attached { id: *id, anchor: a }),
                None => point.map(|p| Endpoint::Free(p + offset)),
            }
        };
        let (Some(from), Some(to)) = (
            end(&c.source, c.source_point, anchor("exitX", "exitY")),
            end(&c.target, c.target_point, anchor("entryX", "entryY")),
        ) else {
            continue;
        };
        let mut k = Connector::new(alloc(), from, to);
        k.color = color(style.get("strokeColor"), Rgba8::hex(0x1f1f1f));
        k.width = style
            .get("strokeWidth")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);
        k.style = if style.get("curved").is_some_and(|v| v == "1") {
            PathStyle::Curved
        } else if style.contains_key("edgeStyle") || base == "edgeStyle" {
            PathStyle::Elbow
        } else {
            PathStyle::Straight
        };
        k.end_cap = cap(
            style.get("endArrow"),
            style.get("endFill"),
            Cap::TriangleFilled,
        );
        k.start_cap = cap(style.get("startArrow"), style.get("startFill"), Cap::None);
        k.dashed = style.get("dashed").is_some_and(|v| v == "1");
        let html = style.get("html").is_some_and(|v| v == "1");
        let mut texts = vec![plain_text(&c.value, html)];
        texts.extend(edge_labels.remove(c.id.as_str()).unwrap_or_default());
        k.text = texts
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        connectors.push(k);
    }
    for mut k in connectors {
        k.route(|id| out.iter().find(|e| e.id() == id).and_then(Target::of));
        out.push(k.into());
    }
    out
}

/// draw.io の図を読み、中心 `center` に置く。置いた要素の ID を返す。
pub fn import(board: &mut Board, xml: &str, center: Vec2) -> Result<Vec<ElementId>, String> {
    let decoded;
    let xml = if is_url_encoded(xml) {
        decoded = percent_encoding::percent_decode_str(xml.trim())
            .decode_utf8_lossy()
            .into_owned();
        decoded.as_str()
    } else {
        xml
    };
    let pages = pages(xml)?;
    let framed = pages.len() > 1;
    let mut alloc = || board.alloc_id();
    let mut frames: Vec<Element> = Vec::new();
    let mut content: Vec<Element> = Vec::new();
    let mut x = 0.0;
    for (name, model) in &pages {
        let mut els = page_elements(&cells(model)?, &mut alloc);
        let Some(b) = els.iter().map(Element::bounds).reduce(|a, b| a.union(&b)) else {
            continue;
        };
        // ページを左上 (x, 0) にそろえる。
        let pad = if framed { PAGE_PAD } else { 0.0 };
        let d = Vec2::new(x + pad, pad) - b.min;
        els.iter_mut().for_each(|e| e.translate(d));
        if framed {
            let size = b.size() + pad * 2.0;
            let mut f = Shape::frame(alloc(), Vec2::new(x, 0.0), size);
            f.text = if name.is_empty() {
                "ページ".into()
            } else {
                name.clone()
            };
            frames.push(f.into());
            x += size.x + PAGE_GAP;
        }
        content.extend(els);
    }
    let mut elements = frames;
    elements.extend(content);
    let bounds = elements
        .iter()
        .map(Element::bounds)
        .reduce(|a, b| a.union(&b))
        .ok_or("draw.io の図に図形がありません")?;
    let d = center - bounds.center();
    for e in &mut elements {
        e.translate(d);
    }
    // 移動した図形に合わせて線を引き直す。
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

// ---- 書き出し ----

/// XML の属性値にする。
fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "&#xa;")
}

/// 図形の文字を HTML のラベルにする(改行は `<br>`)。
fn html_label(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>")
}

/// 色を `#rrggbb` にする(透明は `none`)。
fn color_value(c: Rgba8) -> String {
    let [r, g, b, a] = c.0;
    if a == 0 {
        "none".into()
    } else {
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

/// 図形の種類 → draw.io の形のスタイル。
fn shape_style(s: &Shape) -> &'static str {
    match s.kind {
        ShapeKind::Rect if s.corner_radius > 0.0 => "rounded=1;",
        ShapeKind::Rect => "rounded=0;",
        ShapeKind::RoundRect => "rounded=1;",
        ShapeKind::Pill => "rounded=1;arcSize=50;",
        ShapeKind::Ellipse => "ellipse;",
        ShapeKind::Diamond => "rhombus;",
        ShapeKind::Triangle => "triangle;direction=north;",
        ShapeKind::Hexagon => "shape=hexagon;perimeter=hexagonPerimeter2;",
        ShapeKind::Parallelogram => "shape=parallelogram;perimeter=parallelogramPerimeter;",
        ShapeKind::Trapezoid => "shape=trapezoid;perimeter=trapezoidPerimeter;",
        ShapeKind::Cylinder => "shape=cylinder3;boundedLbl=1;",
        ShapeKind::Document => "shape=document;boundedLbl=1;",
        ShapeKind::PredefinedProcess => "shape=process;",
        ShapeKind::Cloud => "ellipse;shape=cloud;",
        ShapeKind::Chevron => "shape=step;perimeter=stepPerimeter;",
        ShapeKind::Card => "shape=card;",
        ShapeKind::Sticky => "shape=note;",
        ShapeKind::Cross => "shape=cross;",
        ShapeKind::Speech => "shape=callout;",
        ShapeKind::ArrowRight => "shape=singleArrow;",
        ShapeKind::ArrowBoth => "shape=doubleArrow;",
        ShapeKind::ManualInput => "shape=manualInput;",
        ShapeKind::Delay => "shape=delay;",
        ShapeKind::OffPage => "shape=offPageConnector;",
        ShapeKind::InternalStorage => "shape=internalStorage;",
        ShapeKind::Text => "text;",
        ShapeKind::Frame => "container=1;collapsible=0;",
        _ => "rounded=0;",
    }
}

/// 端の飾り → `endArrow=..;endFill=..`(`side` は `start` か `end`)。
fn arrow_style(cap: Cap, side: &str) -> String {
    let (name, fill) = match cap {
        Cap::None => ("none", None),
        Cap::Arrow => ("open", None),
        Cap::Triangle => ("block", Some(0)),
        Cap::TriangleFilled => ("block", Some(1)),
        Cap::Diamond => ("diamond", Some(0)),
        Cap::DiamondFilled => ("diamond", Some(1)),
        Cap::Circle => ("oval", Some(0)),
        Cap::One => ("ERmandOne", None),
        Cap::Many => ("ERmany", None),
        Cap::ZeroOrOne => ("ERzeroToOne", None),
        Cap::OneOrMany => ("ERoneToMany", None),
        Cap::ZeroOrMany => ("ERzeroToMany", None),
    };
    match fill {
        Some(f) => format!("{side}Arrow={name};{side}Fill={f};"),
        None => format!("{side}Arrow={name};"),
    }
}

struct Writer {
    cells: String,
    next: usize,
}

impl Writer {
    fn id(&mut self) -> String {
        self.next += 1;
        format!("c{}", self.next)
    }

    fn vertex(&mut self, id: &str, value: &str, style: &str, r: (Vec2, Vec2), link: &str) {
        let (pos, size) = r;
        let geometry = format!(
            r#"<mxGeometry x="{}" y="{}" width="{}" height="{}" as="geometry"/>"#,
            pos.x, pos.y, size.x, size.y
        );
        if link.is_empty() {
            self.cells.push_str(&format!(
                "<mxCell id=\"{id}\" value=\"{}\" style=\"{}\" vertex=\"1\" parent=\"1\">{geometry}</mxCell>\n",
                attr(value),
                attr(style)
            ));
        } else {
            self.cells.push_str(&format!(
                "<UserObject label=\"{}\" link=\"{}\" id=\"{id}\"><mxCell style=\"{}\" vertex=\"1\" parent=\"1\">{geometry}</mxCell></UserObject>\n",
                attr(value),
                attr(link),
                attr(style)
            ));
        }
    }

    /// 線。端は図形の ID(書き出した図形につながっていれば)か点。
    #[allow(clippy::too_many_arguments)]
    fn edge(
        &mut self,
        value: &str,
        style: &str,
        source: Option<&str>,
        target: Option<&str>,
        from: Vec2,
        to: Vec2,
        waypoints: &[Vec2],
    ) {
        let id = self.id();
        let mut ends = String::new();
        if let Some(s) = source {
            ends.push_str(&format!(" source=\"{s}\""));
        }
        if let Some(t) = target {
            ends.push_str(&format!(" target=\"{t}\""));
        }
        let mut geo = String::new();
        if source.is_none() {
            geo.push_str(&format!(
                r#"<mxPoint x="{}" y="{}" as="sourcePoint"/>"#,
                from.x, from.y
            ));
        }
        if target.is_none() {
            geo.push_str(&format!(
                r#"<mxPoint x="{}" y="{}" as="targetPoint"/>"#,
                to.x, to.y
            ));
        }
        if !waypoints.is_empty() {
            geo.push_str(r#"<Array as="points">"#);
            for p in waypoints {
                geo.push_str(&format!(r#"<mxPoint x="{}" y="{}"/>"#, p.x, p.y));
            }
            geo.push_str("</Array>");
        }
        self.cells.push_str(&format!(
            "<mxCell id=\"{id}\" value=\"{}\" style=\"{}\" edge=\"1\" parent=\"1\"{ends}><mxGeometry relative=\"1\" as=\"geometry\">{geo}</mxGeometry></mxCell>\n",
            attr(value),
            attr(style)
        ));
    }
}

/// 手書き線をこの点数まではまっすぐな線のつながりにする(それより多ければ途中の点を経由する 1 本の線)。
const STROKE_SEGMENTS: usize = 8;

/// 要素 `ids`(空ならボード全体)を draw.io のファイル(`.drawio`)にする。
/// 選んだ要素と同じグループの要素、選んだフレームの中身、両端が対象につながった線も含める。
pub fn export(board: &Board, ids: &[ElementId]) -> Result<String, String> {
    let elements = crate::diagram::extract::scope(board, ids);
    let mut w = Writer {
        cells: String::new(),
        next: 1,
    };
    let mut cell_of: HashMap<ElementId, String> = HashMap::new();
    for e in &elements {
        let Element::Shape(s) = e else { continue };
        if matches!(
            s.kind,
            ShapeKind::Image | ShapeKind::Video | ShapeKind::Stamp
        ) {
            continue;
        }
        let id = w.id();
        let style = s.text_style();
        let mut st = String::from(shape_style(s));
        st.push_str("whiteSpace=wrap;html=1;");
        if s.kind != ShapeKind::Text {
            st.push_str(&format!(
                "fillColor={};strokeColor={};strokeWidth={};",
                color_value(s.fill),
                color_value(s.stroke),
                s.stroke_width
            ));
        } else {
            st.push_str("fillColor=none;strokeColor=none;");
        }
        st.push_str(&format!(
            "fontColor={};fontSize={};",
            color_value(s.text_color),
            style.font_size
        ));
        st.push_str(match s.align {
            TextAlign::Left => "align=left;",
            TextAlign::Right => "align=right;",
            _ => "align=center;",
        });
        if s.kind == ShapeKind::Frame {
            st.push_str("verticalAlign=bottom;labelPosition=center;verticalLabelPosition=top;");
        } else {
            st.push_str(match s.valign {
                VAlign::Top => "verticalAlign=top;",
                VAlign::Bottom => "verticalAlign=bottom;",
                VAlign::Middle => "verticalAlign=middle;",
            });
        }
        w.vertex(
            &id,
            &html_label(&s.text),
            &st,
            (s.pos, s.size),
            &s.meta.link,
        );
        cell_of.insert(s.id, id);
    }
    for e in &elements {
        match e {
            Element::Connector(c) => {
                let mut st = match c.style {
                    PathStyle::Elbow => String::from("edgeStyle=orthogonalEdgeStyle;rounded=0;"),
                    PathStyle::Curved => String::from("curved=1;"),
                    PathStyle::Straight => String::new(),
                };
                st.push_str("html=1;");
                st.push_str(&arrow_style(c.start_cap, "start"));
                st.push_str(&arrow_style(c.end_cap, "end"));
                if c.dashed {
                    st.push_str("dashed=1;");
                }
                st.push_str(&format!(
                    "strokeColor={};strokeWidth={};",
                    color_value(c.color),
                    c.width
                ));
                let end = |ep: &Endpoint, exit: bool, st: &mut String| -> Option<String> {
                    let Endpoint::Attached { id, anchor } = ep else {
                        return None;
                    };
                    let cell = cell_of.get(id)?.clone();
                    if let Some(a) = anchor {
                        let (x, y) = if exit {
                            ("exitX", "exitY")
                        } else {
                            ("entryX", "entryY")
                        };
                        st.push_str(&format!("{x}={};{y}={};", a.x, a.y));
                    }
                    Some(cell)
                };
                let source = end(&c.from, true, &mut st);
                let target = end(&c.to, false, &mut st);
                let point = |ep: &Endpoint, fallback: Option<&Vec2>| match ep {
                    Endpoint::Free(p) => *p,
                    Endpoint::Attached { .. } => fallback.copied().unwrap_or_default(),
                };
                let from = point(&c.from, c.path.first());
                let to = point(&c.to, c.path.last());
                w.edge(
                    &html_label(&c.text),
                    &st,
                    source.as_deref(),
                    target.as_deref(),
                    from,
                    to,
                    &[],
                );
            }
            Element::Stroke(k) if k.points.len() >= 2 => {
                let st = format!(
                    "html=1;endArrow=none;startArrow=none;strokeColor={};strokeWidth={};{}",
                    color_value(k.color),
                    k.width,
                    if k.highlighter { "opacity=40;" } else { "" }
                );
                if k.points.len() <= STROKE_SEGMENTS {
                    for pair in k.points.windows(2) {
                        w.edge("", &st, None, None, pair[0], pair[1], &[]);
                    }
                } else {
                    let n = k.points.len();
                    w.edge(
                        "",
                        &st,
                        None,
                        None,
                        k.points[0],
                        k.points[n - 1],
                        &k.points[1..n - 1],
                    );
                }
            }
            _ => {}
        }
    }
    if w.cells.is_empty() {
        return Err("書き出せる図形がありません".into());
    }
    Ok(format!(
        "<mxfile host=\"graboard\">\n<diagram name=\"グラボード\" id=\"graboard\">\n<mxGraphModel grid=\"1\" gridSize=\"10\" guides=\"1\" connect=\"1\" arrows=\"1\" page=\"0\">\n<root>\n<mxCell id=\"0\"/>\n<mxCell id=\"1\" parent=\"0\"/>\n{}</root>\n</mxGraphModel>\n</diagram>\n</mxfile>\n",
        w.cells
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &str = r##"<mxfile host="app.diagrams.net"><diagram name="ページ1" id="a">
<mxGraphModel><root>
<mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="s" value="開始" style="ellipse;whiteSpace=wrap;html=1;fillColor=#d5e8d4;strokeColor=#82b366;" vertex="1" parent="1">
  <mxGeometry x="40" y="40" width="120" height="60" as="geometry"/></mxCell>
<mxCell id="d" value="&lt;b&gt;在庫&lt;/b&gt;&lt;br&gt;ある?" style="rhombus;html=1;" vertex="1" parent="1">
  <mxGeometry x="40" y="160" width="120" height="80" as="geometry"/></mxCell>
<mxCell id="g" value="倉庫" style="swimlane;" vertex="1" parent="1">
  <mxGeometry x="300" y="120" width="200" height="200" as="geometry"/></mxCell>
<mxCell id="db" value="DB" style="shape=cylinder3;" vertex="1" parent="g">
  <mxGeometry x="40" y="60" width="80" height="80" as="geometry"/></mxCell>
<mxCell id="e1" style="edgeStyle=orthogonalEdgeStyle;endArrow=classic;html=1;" edge="1" parent="1" source="s" target="d">
  <mxGeometry relative="1" as="geometry"/></mxCell>
<mxCell id="e2" value="いいえ" style="endArrow=ERmany;dashed=1;exitX=1;exitY=0.5;" edge="1" parent="1" source="d" target="db">
  <mxGeometry relative="1" as="geometry"/></mxCell>
<mxCell id="l" value="ラベル" style="edgeLabel;" vertex="1" connectable="0" parent="e1">
  <mxGeometry x="-0.2" relative="1" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##;

    fn shapes(board: &Board) -> Vec<&Shape> {
        board
            .elements()
            .iter()
            .filter_map(Element::as_shape)
            .collect()
    }

    #[test]
    fn imports_shapes_and_edges() {
        let mut board = Board::new();
        let ids = import(&mut board, PLAIN, Vec2::ZERO).unwrap();
        assert_eq!(ids.len(), 6);
        let s = shapes(&board);
        assert_eq!(s[0].kind, ShapeKind::Ellipse);
        assert_eq!(s[0].fill, Rgba8::hex(0xd5e8d4));
        assert_eq!(s[1].kind, ShapeKind::Diamond);
        assert_eq!(s[1].text, "在庫\nある?");
        assert_eq!(s[2].kind, ShapeKind::Frame);
        assert_eq!(s[3].kind, ShapeKind::Cylinder);
        // 子は親の位置を足した場所(フレームの中)。
        assert!(s[2].bounds().contains(s[3].bounds().center()));
        assert_eq!(s[3].pos - s[2].pos, Vec2::new(40.0, 60.0));
        let conns: Vec<&Connector> = board
            .elements()
            .iter()
            .filter_map(|e| match e {
                Element::Connector(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(conns.len(), 2);
        assert_eq!(conns[0].style, PathStyle::Elbow);
        assert_eq!(conns[0].text, "ラベル");
        assert_eq!(conns[0].from.attached_to(), Some(s[0].id));
        assert_eq!(conns[1].end_cap, Cap::Many);
        assert!(conns[1].dashed);
        assert_eq!(conns[1].text, "いいえ");
        assert!(matches!(
            conns[1].from,
            Endpoint::Attached { anchor: Some(a), .. } if a == Vec2::new(1.0, 0.5)
        ));
        assert!(board.bounds().unwrap().center().length() < 1.0);
    }

    #[test]
    fn compressed_pages_become_frames() {
        use base64::Engine;
        let model = r#"<mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/><mxCell id="a" value="A 1" style="rounded=1;" vertex="1" parent="1"><mxGeometry x="0" y="0" width="100" height="50" as="geometry"/></mxCell></root></mxGraphModel>"#;
        let encoded: String =
            percent_encoding::utf8_percent_encode(model, percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        let deflated = miniz_oxide::deflate::compress_to_vec(encoded.as_bytes(), 6);
        let data = base64::engine::general_purpose::STANDARD.encode(deflated);
        let xml = format!(
            r#"<mxfile><diagram name="一枚目">{data}</diagram><diagram name="二枚目">{model}</diagram></mxfile>"#
        );
        let mut board = Board::new();
        import(&mut board, &xml, Vec2::ZERO).unwrap();
        let s = shapes(&board);
        let frames: Vec<_> = s.iter().filter(|s| s.kind == ShapeKind::Frame).collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].text, "一枚目");
        let boxes: Vec<_> = s
            .iter()
            .filter(|s| s.kind == ShapeKind::RoundRect)
            .collect();
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].text, "A 1");
        assert!(frames[0].bounds().contains(boxes[0].bounds().center()));
        assert!(frames[1].bounds().contains(boxes[1].bounds().center()));
    }

    #[test]
    fn svg_embedded_and_detection() {
        let escaped = PLAIN
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        let svg =
            format!(r#"<svg xmlns="http://www.w3.org/2000/svg" content="{escaped}"><g/></svg>"#);
        assert!(looks_like_drawio(&svg));
        let mut board = Board::new();
        assert_eq!(import(&mut board, &svg, Vec2::ZERO).unwrap().len(), 6);
        assert!(!looks_like_drawio("<html></html>"));
        // draw.io からコピーした図形(URL エンコード)。
        let model = &PLAIN[PLAIN.find("<mxGraphModel").unwrap()..PLAIN.find("</diagram>").unwrap()];
        let copied =
            percent_encoding::utf8_percent_encode(model, percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        assert!(looks_like_drawio(&copied));
        assert_eq!(
            import(&mut Board::new(), &copied, Vec2::ZERO)
                .unwrap()
                .len(),
            6
        );
        assert!(import(&mut Board::new(), "<mxfile></mxfile>", Vec2::ZERO).is_err());
    }

    #[test]
    fn export_then_import_keeps_shapes_and_lines() {
        use crate::templates::{self, Template};
        for t in [
            Template::UmlClass,
            Template::Er,
            Template::Sequence,
            Template::UseCase,
        ] {
            let mut board = Board::new();
            templates::insert(&mut board, t, Vec2::ZERO);
            let xml = export(&board, &[]).unwrap();
            assert!(looks_like_drawio(&xml));
            let mut back = Board::new();
            import(&mut back, &xml, Vec2::ZERO).unwrap();
            let a = shapes(&board);
            let b = shapes(&back);
            assert_eq!(a.len(), b.len(), "{t:?}");
            // 位置関係・大きさ・文字・色がそのまま。
            let d = b[0].pos - a[0].pos;
            for (x, y) in a.iter().zip(&b) {
                assert_eq!(x.text, y.text, "{t:?}");
                assert!((y.pos - x.pos - d).length() < 0.01, "{t:?} {}", x.text);
                assert!((y.size - x.size).length() < 0.01, "{t:?} {}", x.text);
                assert_eq!(x.fill, y.fill, "{t:?} {}", x.text);
                assert_eq!(x.valign, y.valign, "{t:?} {}", x.text);
            }
            let attached = |b: &Board| {
                b.elements()
                    .iter()
                    .filter_map(|e| match e {
                        Element::Connector(c) => Some((
                            c.from.attached_to().is_some(),
                            c.to.attached_to().is_some(),
                            c.start_cap,
                            c.end_cap,
                            c.dashed,
                            c.text.clone(),
                        )),
                        _ => None,
                    })
                    .filter(|c| c.0 || !c.5.is_empty() || c.4)
                    .collect::<Vec<_>>()
            };
            assert_eq!(attached(&board), attached(&back), "{t:?}");
        }
    }

    #[test]
    fn export_escapes_text_and_keeps_links() {
        let mut board = Board::new();
        let id = board.alloc_id();
        let mut s = Shape::rect(id, Vec2::ZERO, Vec2::new(100.0, 50.0), Rgba8::WHITE);
        s.text = "a < b & \"c\"\n2 行目".into();
        s.meta.link = "https://example.com/?a=1&b=2".into();
        board.push(Element::from(s));
        let xml = export(&board, &[]).unwrap();
        let mut back = Board::new();
        import(&mut back, &xml, Vec2::ZERO).unwrap();
        let t = shapes(&back)[0];
        assert_eq!(t.text, "a < b & \"c\"\n2 行目");
        assert_eq!(t.meta.link, "https://example.com/?a=1&b=2");
        assert!(export(&Board::new(), &[]).is_err());
    }
}
