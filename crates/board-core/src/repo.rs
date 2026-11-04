//! Git で管理するためのボードのファイル形式(`.graboard.json`)と、3 方向マージ。
//!
//! ボードを GitHub などのリポジトリに置き、差分を読み・レビューし・マージできるようにする。
//!
//! ```text
//! {
//!   "graboard": 1,
//!   "title": "11 月の振り返り",
//!   "elements": [
//! {"id":"1a2b","type":"shape","align":"center","fill":"#fff3a3",…,"text":"よかったこと","x":0,"y":0,"z":1},
//! {"id":"1a2c","type":"connector","from":{"element":"1a2b"},"to":{"x":300,"y":40},…}
//!   ]
//! }
//! ```
//!
//! - 1 要素 = 1 行。重なり順(奥から)に並べるので、付箋を 1 枚動かすと差分も 1 行になる。
//!   別々の要素を同時に変えても Git の行単位のマージがぶつかりにくい。
//! - 要素の中のキーは名前順。既定値の項目(リンクなし・ロックなしなど)は書かない。
//! - 数値は f32 の最短表記(`0.1` は `0.1`、`100.0` は `100`)。
//! - 画像は中身のハッシュを名前にした別ファイル(`assets/<hash>.png`。ボードのファイルからの
//!   相対パス)に出し、要素には `"image": "assets/…"` だけを書く。同じ画像は 1 つにまとまる。
//! - 要素の ID はボードの ID と同じ 16 進の文字列で、読み込み直しても変わらない。
//!
//! [`merge3`] は「最後に同期した時点」「ボードの今」「リポジトリの今」から要素・項目単位で
//! 3 方向マージする。両方で同じ項目を違う値に変えたときはボード側を残し、衝突として返す。

use std::collections::{BTreeMap, BTreeSet};

use glam::Vec2;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::board::Board;
use crate::color::Rgba8;
use crate::element::{Cap, Connector, Element, ElementId, Endpoint, Meta, PathStyle, Stroke};
use crate::mindmap::{Mind, MindLayout, MindLink};
use crate::ops::{color_string, id_string, parse_color, parse_id};
use crate::shape::{ImageData, Shape, ShapeKind, TextAlign, VAlign};

/// ファイル形式の版。
pub const FORMAT_VERSION: u64 = 1;
/// 勧める拡張子。
pub const EXTENSION: &str = ".graboard.json";
/// 画像を置くディレクトリ(ボードのファイルからの相対)。
pub const ASSET_DIR: &str = "assets";
/// 読み込める要素の数の上限。
pub const MAX_ELEMENTS: usize = 200_000;

/// ボードのファイルの中身。要素は ID → 項目(`id` 以外)。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoDoc {
    pub title: String,
    pub elements: BTreeMap<String, Map<String, Value>>,
}

/// 画像ファイル(ボードのファイルからの相対パスと中身)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// 2 つのドキュメントの違いの数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.added + self.updated + self.removed == 0
    }

    pub fn total(&self) -> usize {
        self.added + self.updated + self.removed
    }
}

/// マージの結果。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Merged {
    pub doc: RepoDoc,
    /// 両方で違う変更があった要素の ID(ボード側を残した)。
    pub conflicts: Vec<String>,
}

// ---- 数値・色 ----

/// f32 を最短表記の JSON の数にする(整数なら小数点なし)。
fn num(v: f32) -> Value {
    if !v.is_finite() {
        return Value::from(0);
    }
    if v.fract() == 0.0 && v.abs() < 1.0e15 {
        return Value::from(v as i64);
    }
    // f32 の最短表記を f64 として読み直すと、JSON にも同じ表記で出る。
    format!("{v}")
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map_or(Value::from(0), Value::Number)
}

fn num64(v: f64) -> Value {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1.0e15 {
        Value::from(v as i64)
    } else {
        serde_json::Number::from_f64(v).map_or(Value::from(0), Value::Number)
    }
}

fn color(c: Rgba8) -> Value {
    Value::String(color_string(c))
}

fn put_if(m: &mut Map<String, Value>, k: &str, cond: bool, v: impl FnOnce() -> Value) {
    if cond {
        m.insert(k.into(), v());
    }
}

/// 画像の種類から拡張子を決める。
fn image_ext(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "png"
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        "jpg"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else {
        "bin"
    }
}

/// 画像のファイル名(中身の SHA-256 の先頭 16 バイト)。
pub fn asset_path(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let hex: String = hash[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("{ASSET_DIR}/{hex}.{}", image_ext(bytes))
}

// ---- 要素 → JSON ----

fn endpoint(e: &Endpoint) -> Value {
    let mut m = Map::new();
    match e {
        Endpoint::Free(p) => {
            m.insert("x".into(), num(p.x));
            m.insert("y".into(), num(p.y));
        }
        Endpoint::Attached { id, anchor } => {
            m.insert("element".into(), Value::String(id_string(*id)));
            if let Some(a) = anchor {
                m.insert("anchor".into(), Value::Array(vec![num(a.x), num(a.y)]));
            }
        }
    }
    Value::Object(m)
}

fn meta_fields(m: &mut Map<String, Value>, meta: &Meta) {
    put_if(m, "group", meta.group != 0, || Value::from(meta.group));
    put_if(m, "locked", meta.locked, || Value::Bool(true));
    put_if(m, "link", !meta.link.is_empty(), || {
        Value::String(meta.link.clone())
    });
    put_if(m, "private", !meta.private.is_empty(), || {
        Value::String(meta.private.clone())
    });
    put_if(m, "author", !meta.author.is_empty(), || {
        Value::String(meta.author.clone())
    });
    if let Some(mind) = &meta.mind {
        let v = match mind {
            Mind::Branch => Value::String("branch".into()),
            Mind::Node(n) => {
                let mut o = Map::new();
                if let Some(p) = n.parent {
                    o.insert("parent".into(), Value::String(id_string(p)));
                }
                o.insert("order".into(), num64(n.order));
                if n.collapsed {
                    o.insert("collapsed".into(), Value::Bool(true));
                }
                o.insert("layout".into(), Value::String(n.layout.name().into()));
                Value::Object(o)
            }
        };
        m.insert("mind".into(), v);
        put_if(m, "hidden", meta.hidden, || Value::Bool(true));
    }
}

/// 1 要素を項目の一覧にする(`id` は含めない)。画像があれば `assets` に足す。
pub fn element_fields(e: &Element, z: f64, assets: &mut Vec<Asset>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("z".into(), num64(z));
    meta_fields(&mut m, e.meta());
    match e {
        Element::Shape(s) => {
            m.insert("type".into(), Value::String("shape".into()));
            m.insert("kind".into(), Value::String(s.kind.name().into()));
            m.insert("x".into(), num(s.pos.x));
            m.insert("y".into(), num(s.pos.y));
            m.insert("w".into(), num(s.size.x));
            m.insert("h".into(), num(s.size.y));
            m.insert("text".into(), Value::String(s.text.clone()));
            m.insert("fill".into(), color(s.fill));
            m.insert("stroke".into(), color(s.stroke));
            m.insert("stroke_width".into(), num(s.stroke_width));
            m.insert("corner_radius".into(), num(s.corner_radius));
            m.insert("text_color".into(), color(s.text_color));
            m.insert("font_size".into(), num(s.font_size));
            m.insert("align".into(), Value::String(s.align.name().into()));
            m.insert("valign".into(), Value::String(s.valign.name().into()));
            put_if(&mut m, "vertical", s.vertical, || Value::Bool(true));
            put_if(&mut m, "icon", !s.icon.is_empty(), || {
                Value::String(s.icon.clone())
            });
            put_if(&mut m, "media", !s.media.is_empty(), || {
                Value::String(s.media.clone())
            });
            if let Some(img) = &s.image {
                let path = asset_path(img.bytes());
                if !assets.iter().any(|a| a.path == path) {
                    assets.push(Asset {
                        path: path.clone(),
                        bytes: img.bytes().to_vec(),
                    });
                }
                m.insert("image".into(), Value::String(path));
            }
        }
        Element::Connector(c) => {
            m.insert("type".into(), Value::String("connector".into()));
            m.insert("from".into(), endpoint(&c.from));
            m.insert("to".into(), endpoint(&c.to));
            m.insert("text".into(), Value::String(c.text.clone()));
            m.insert("color".into(), color(c.color));
            m.insert("width".into(), num(c.width));
            m.insert("style".into(), Value::String(c.style.name().into()));
            m.insert("start_cap".into(), Value::String(c.start_cap.name().into()));
            m.insert("end_cap".into(), Value::String(c.end_cap.name().into()));
            put_if(&mut m, "dashed", c.dashed, || Value::Bool(true));
        }
        Element::Stroke(s) => {
            m.insert("type".into(), Value::String("stroke".into()));
            m.insert(
                "points".into(),
                Value::Array(
                    s.points
                        .iter()
                        .map(|p| Value::Array(vec![num(p.x), num(p.y)]))
                        .collect(),
                ),
            );
            m.insert("color".into(), color(s.color));
            m.insert("width".into(), num(s.width));
            put_if(&mut m, "highlighter", s.highlighter, || Value::Bool(true));
        }
    }
    m
}

impl RepoDoc {
    /// ボードの今の中身から作る。画像ファイルも返す。
    pub fn from_board(board: &Board, title: &str) -> (Self, Vec<Asset>) {
        let mut assets = Vec::new();
        let elements = board
            .elements()
            .iter()
            .map(|e| {
                let z = board.z_of(e.id()).unwrap_or(0.0);
                (id_string(e.id()), element_fields(e, z, &mut assets))
            })
            .collect();
        (
            Self {
                title: title.to_string(),
                elements,
            },
            assets,
        )
    }

    /// 要素の ID を重なり順(奥から。同じなら ID 順)に。
    fn ordered_ids(&self) -> Vec<&String> {
        let mut ids: Vec<&String> = self.elements.keys().collect();
        let z = |id: &String| self.elements[id].get("z").and_then(Value::as_f64);
        ids.sort_by(|a, b| {
            z(a).unwrap_or(0.0)
                .total_cmp(&z(b).unwrap_or(0.0))
                .then_with(|| a.len().cmp(&b.len()))
                .then_with(|| a.cmp(b))
        });
        ids
    }

    /// ファイルの文字列にする(1 要素 1 行、末尾に改行)。
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        out.push_str(&format!("  \"graboard\": {FORMAT_VERSION},\n"));
        out.push_str(&format!(
            "  \"title\": {},\n",
            Value::String(self.title.clone())
        ));
        let ids = self.ordered_ids();
        if ids.is_empty() {
            out.push_str("  \"elements\": []\n}\n");
            return out;
        }
        out.push_str("  \"elements\": [\n");
        for (i, id) in ids.iter().enumerate() {
            let fields = &self.elements[*id];
            // `id` と `type` を先頭に、残りは名前順。
            let mut line = format!("{{\"id\":{}", Value::String((*id).clone()));
            if let Some(t) = fields.get("type") {
                line.push_str(&format!(",\"type\":{t}"));
            }
            for (k, v) in fields {
                if k == "type" {
                    continue;
                }
                line.push_str(&format!(",{}:{v}", Value::String(k.clone())));
            }
            line.push('}');
            out.push_str(&line);
            out.push_str(if i + 1 < ids.len() { ",\n" } else { "\n" });
        }
        out.push_str("  ]\n}\n");
        out
    }

    /// ファイルの文字列を読む(要素の中身はまだ確かめない。[`RepoDoc::to_elements`] で確かめる)。
    pub fn parse(text: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|e| format!("JSON として読めません({e})"))?;
        let Value::Object(mut root) = v else {
            return Err("グラボードのファイルではありません(全体が {…} ではない)".into());
        };
        match root.get("graboard").and_then(Value::as_u64) {
            Some(v) if v <= FORMAT_VERSION => {}
            Some(v) => {
                return Err(format!(
                    "新しい版({v})のファイルです。サーバーを新しくしてください"
                ));
            }
            None => return Err("グラボードのファイルではありません(\"graboard\" がない)".into()),
        }
        let title = root
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let list = match root.remove("elements") {
            Some(Value::Array(a)) => a,
            None => Vec::new(),
            Some(_) => return Err("\"elements\" は配列にしてください".into()),
        };
        if list.len() > MAX_ELEMENTS {
            return Err(format!("要素が多すぎます(上限 {MAX_ELEMENTS})"));
        }
        let mut elements = BTreeMap::new();
        for (i, item) in list.into_iter().enumerate() {
            let Value::Object(mut fields) = item else {
                return Err(format!("{} 番目の要素が {{…}} ではありません", i + 1));
            };
            let id = match fields.remove("id") {
                Some(Value::String(s)) if parse_id(&s).is_some() => s,
                _ => {
                    return Err(format!(
                        "{} 番目の要素の \"id\" がありません(16 進の文字列)",
                        i + 1
                    ));
                }
            };
            if elements.insert(id.clone(), fields).is_some() {
                return Err(format!("要素の ID「{id}」が重複しています"));
            }
        }
        Ok(Self { title, elements })
    }

    /// 要素にする。画像は `asset` で読む(見つからなければ画像なしの図形にする)。
    pub fn to_elements(
        &self,
        mut asset: impl FnMut(&str) -> Option<Vec<u8>>,
    ) -> Result<Vec<(Element, f64)>, String> {
        self.ordered_ids()
            .into_iter()
            .map(|id| {
                let fields = &self.elements[id];
                parse_element(id, fields, &mut asset).map_err(|e| format!("要素「{id}」: {e}"))
            })
            .collect()
    }

    /// 要素が参照している画像ファイル。
    pub fn asset_paths(&self) -> BTreeSet<String> {
        self.elements
            .values()
            .filter_map(|f| f.get("image").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    /// `self` から `other` への違いの数。
    pub fn changes_to(&self, other: &RepoDoc) -> Changes {
        let mut c = Changes::default();
        for (id, f) in &other.elements {
            match self.elements.get(id) {
                None => c.added += 1,
                Some(g) if g != f => c.updated += 1,
                Some(_) => {}
            }
        }
        c.removed = self
            .elements
            .keys()
            .filter(|id| !other.elements.contains_key(*id))
            .count();
        c
    }

    /// 要素の短い説明(衝突の表示用)。
    pub fn describe(&self, id: &str) -> String {
        let Some(f) = self.elements.get(id) else {
            return id.to_string();
        };
        let text: String = f
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(30)
            .collect();
        if text.is_empty() {
            let kind = f
                .get("kind")
                .or_else(|| f.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("要素");
            format!("{kind}({id})")
        } else {
            format!("「{text}」")
        }
    }
}

// ---- JSON → 要素 ----

type R<T> = Result<T, String>;

fn get_f32(m: &Map<String, Value>, k: &str) -> R<Option<f32>> {
    match m.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let v = n.as_f64().unwrap_or(0.0) as f32;
            if v.is_finite() && v.abs() <= crate::ops::MAX_COORD {
                Ok(Some(v))
            } else {
                Err(format!("\"{k}\" が範囲外です"))
            }
        }
        Some(_) => Err(format!("\"{k}\" は数にしてください")),
    }
}

fn req_f32(m: &Map<String, Value>, k: &str) -> R<f32> {
    get_f32(m, k)?.ok_or_else(|| format!("\"{k}\" がありません"))
}

fn get_str<'a>(m: &'a Map<String, Value>, k: &str) -> R<&'a str> {
    match m.get(k) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(s)) => {
            if s.chars().count() > crate::ops::MAX_TEXT_CHARS {
                Err(format!("\"{k}\" が長すぎます"))
            } else {
                Ok(s)
            }
        }
        Some(_) => Err(format!("\"{k}\" は文字列にしてください")),
    }
}

fn get_bool(m: &Map<String, Value>, k: &str) -> R<bool> {
    match m.get(k) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("\"{k}\" は true / false にしてください")),
    }
}

fn get_color(m: &Map<String, Value>, k: &str, default: Rgba8) -> R<Rgba8> {
    match get_str(m, k)? {
        "" => Ok(default),
        s => parse_color(s).ok_or_else(|| format!("\"{k}\" の色「{s}」が読めません(#rrggbb)")),
    }
}

fn ref_id(s: &str, what: &str) -> R<ElementId> {
    parse_id(s).ok_or_else(|| format!("{what} の要素 ID「{s}」が読めません"))
}

fn pair(v: &Value, what: &str) -> R<Vec2> {
    match v.as_array().map(Vec::as_slice) {
        Some([x, y]) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => {
                Ok(Vec2::new(x as f32, y as f32))
            }
            _ => Err(format!("{what} は [x, y] の数にしてください")),
        },
        _ => Err(format!("{what} は [x, y] にしてください")),
    }
}

fn parse_endpoint(v: Option<&Value>, what: &str) -> R<Endpoint> {
    let Some(Value::Object(m)) = v else {
        return Err(format!(
            "\"{what}\" がありません({{\"x\",\"y\"}} か {{\"element\"}})"
        ));
    };
    if let Some(e) = m.get("element") {
        let id = ref_id(e.as_str().unwrap_or_default(), what)?;
        let anchor = match m.get("anchor") {
            None | Some(Value::Null) => None,
            Some(a) => Some(pair(a, &format!("\"{what}.anchor\""))?),
        };
        return Ok(Endpoint::Attached { id, anchor });
    }
    Ok(Endpoint::Free(Vec2::new(
        req_f32(m, "x")?,
        req_f32(m, "y")?,
    )))
}

fn parse_meta(m: &Map<String, Value>) -> R<Meta> {
    let mind = match m.get("mind") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "branch" => Some(Mind::Branch),
        Some(Value::Object(o)) => Some(Mind::Node(MindLink {
            parent: match o.get("parent").and_then(Value::as_str) {
                Some(p) => Some(ref_id(p, "\"mind.parent\"")?),
                None => None,
            },
            order: o.get("order").and_then(Value::as_f64).unwrap_or(0.0),
            collapsed: o.get("collapsed").and_then(Value::as_bool).unwrap_or(false),
            layout: MindLayout::from_name(
                o.get("layout").and_then(Value::as_str).unwrap_or_default(),
            ),
        })),
        Some(_) => return Err("\"mind\" が読めません".into()),
    };
    Ok(Meta {
        group: m.get("group").and_then(Value::as_u64).unwrap_or(0),
        locked: get_bool(m, "locked")?,
        link: get_str(m, "link")?.to_string(),
        private: get_str(m, "private")?.to_string(),
        author: get_str(m, "author")?.to_string(),
        hidden: mind.is_some() && get_bool(m, "hidden")?,
        mind,
    })
}

fn parse_element(
    id: &str,
    m: &Map<String, Value>,
    asset: &mut impl FnMut(&str) -> Option<Vec<u8>>,
) -> R<(Element, f64)> {
    let eid = ref_id(id, "\"id\"")?;
    let z = m.get("z").and_then(Value::as_f64).unwrap_or(0.0);
    let meta = parse_meta(m)?;
    let e = match get_str(m, "type")? {
        "shape" => {
            let kind_name = get_str(m, "kind")?;
            let kind = ShapeKind::from_name(kind_name)
                .ok_or_else(|| format!("図形の種類「{kind_name}」を知りません"))?;
            let pos = Vec2::new(req_f32(m, "x")?, req_f32(m, "y")?);
            let def = kind.default_size();
            let size = Vec2::new(
                get_f32(m, "w")?.unwrap_or(def.x).max(1.0),
                get_f32(m, "h")?.unwrap_or(def.y).max(1.0),
            );
            let fill = if kind == ShapeKind::Sticky {
                crate::color::STICKY_COLORS[0]
            } else {
                Rgba8::WHITE
            };
            let mut s = Shape::new(eid, kind, pos, size, fill);
            s.fill = get_color(m, "fill", s.fill)?;
            s.stroke = get_color(m, "stroke", s.stroke)?;
            s.text_color = get_color(m, "text_color", s.text_color)?;
            if let Some(v) = get_f32(m, "stroke_width")? {
                s.stroke_width = v;
            }
            if let Some(r) = get_f32(m, "corner_radius")? {
                s.corner_radius = r;
            }
            s.text = get_str(m, "text")?.to_string();
            s.font_size = get_f32(m, "font_size")?.unwrap_or(0.0);
            if m.contains_key("align") {
                s.align = TextAlign::from_name(get_str(m, "align")?);
            }
            if m.contains_key("valign") {
                s.valign = VAlign::from_name(get_str(m, "valign")?);
            }
            s.vertical = get_bool(m, "vertical")?;
            s.icon = get_str(m, "icon")?.to_string();
            s.media = get_str(m, "media")?.to_string();
            s.image = match get_str(m, "image")? {
                "" => None,
                path => asset(path).map(ImageData::new),
            };
            s.meta = meta;
            Element::Shape(s)
        }
        "connector" => {
            let mut c = Connector::new(
                eid,
                parse_endpoint(m.get("from"), "from")?,
                parse_endpoint(m.get("to"), "to")?,
            );
            c.text = get_str(m, "text")?.to_string();
            c.color = get_color(m, "color", c.color)?;
            if let Some(w) = get_f32(m, "width")? {
                c.width = w;
            }
            if m.contains_key("style") {
                c.style = PathStyle::from_name(get_str(m, "style")?);
            }
            if m.contains_key("start_cap") {
                c.start_cap = Cap::from_name(get_str(m, "start_cap")?);
            }
            if m.contains_key("end_cap") {
                c.end_cap = Cap::from_name(get_str(m, "end_cap")?);
            }
            c.dashed = get_bool(m, "dashed")?;
            c.meta = meta;
            Element::Connector(c)
        }
        "stroke" => {
            let pts = match m.get("points") {
                Some(Value::Array(a)) if !a.is_empty() => a
                    .iter()
                    .map(|p| pair(p, "\"points\" の点"))
                    .collect::<R<Vec<_>>>()?,
                _ => return Err("\"points\" がありません".into()),
            };
            if pts.len() > crate::ops::MAX_POINTS {
                return Err("\"points\" が多すぎます".into());
            }
            let mut s = Stroke::new(eid, pts[0], Rgba8::BLACK, 4.0);
            s.points = pts;
            s.color = get_color(m, "color", s.color)?;
            if let Some(w) = get_f32(m, "width")? {
                s.width = w;
            }
            s.highlighter = get_bool(m, "highlighter")?;
            s.meta = meta;
            Element::Stroke(s)
        }
        "" => return Err("\"type\" がありません(shape / connector / stroke)".into()),
        t => return Err(format!("要素の種類「{t}」を知りません")),
    };
    Ok((e, z))
}

// ---- ボードへの反映 ----

/// ボードの中身を `doc` と同じにする(違う要素だけ書き換える)。画像は `asset` で読み、
/// 見つからなければボードにある同じ画像を使う。返り値はボードに加えた変更の数。
pub fn apply_to_board(
    board: &mut Board,
    doc: &RepoDoc,
    mut asset: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Result<Changes, String> {
    let (current, current_assets) = RepoDoc::from_board(board, &doc.title);
    let wanted = doc.to_elements(|path| {
        current_assets
            .iter()
            .find(|a| a.path == path)
            .map(|a| a.bytes.clone())
            .or_else(|| asset(path))
    })?;
    let changes = current.changes_to(doc);
    if changes.is_empty() {
        return Ok(changes);
    }
    let mut assets = Vec::new();
    let puts: Vec<(Element, f64)> = wanted
        .into_iter()
        .filter(|(e, z)| {
            let id = id_string(e.id());
            match current.elements.get(&id) {
                None => true,
                Some(f) => element_fields(e, *z, &mut assets) != *f,
            }
        })
        .collect();
    let removes: Vec<ElementId> = current
        .elements
        .keys()
        .filter(|id| !doc.elements.contains_key(*id))
        .filter_map(|id| parse_id(id))
        .collect();
    board.replace_elements(puts, &removes);
    Ok(changes)
}

// ---- 3 方向マージ ----

fn merge_value<T: Clone + PartialEq>(
    base: Option<&T>,
    ours: Option<&T>,
    theirs: Option<&T>,
) -> (Option<T>, bool) {
    if ours == theirs {
        return (ours.cloned(), false);
    }
    if ours == base {
        return (theirs.cloned(), false);
    }
    if theirs == base {
        return (ours.cloned(), false);
    }
    (None, true)
}

/// 「最後に同期した時点」`base`、「ボードの今」`ours`、「リポジトリの今」`theirs` をマージする。
///
/// - 片方だけが変えた要素・項目はその変更を取る(追加・削除も)。
/// - 両方が同じ要素の違う項目を変えたら両方の変更を合わせる。
/// - 両方が同じ項目を違う値にした・片方が消してもう片方が変えたときはボード側(変えた側)を残し、
///   `conflicts` に入れる。
/// - `base` がない(初めての同期)ときは、両方にある違う要素を衝突として扱う。
pub fn merge3(base: Option<&RepoDoc>, ours: &RepoDoc, theirs: &RepoDoc) -> Merged {
    let empty = RepoDoc::default();
    let base_doc = base.unwrap_or(&empty);
    let (title, _) = merge_value(
        base.map(|b| &b.title),
        Some(&ours.title),
        Some(&theirs.title),
    );
    let mut doc = RepoDoc {
        title: title.unwrap_or_else(|| ours.title.clone()),
        elements: BTreeMap::new(),
    };
    let mut conflicts = Vec::new();
    let ids: BTreeSet<&String> = ours
        .elements
        .keys()
        .chain(theirs.elements.keys())
        .chain(base_doc.elements.keys())
        .collect();
    for id in ids {
        let b = base_doc.elements.get(id);
        let o = ours.elements.get(id);
        let t = theirs.elements.get(id);
        let (v, conflict) = merge_value(b, o, t);
        if !conflict {
            if let Some(v) = v {
                doc.elements.insert(id.clone(), v);
            }
            continue;
        }
        match (o, t) {
            (Some(o), Some(t)) => {
                // 項目ごとに合わせる。
                let empty_map = Map::new();
                let b = b.unwrap_or(&empty_map);
                let keys: BTreeSet<&String> = o.keys().chain(t.keys()).chain(b.keys()).collect();
                let mut merged = Map::new();
                let mut clash = false;
                for k in keys {
                    let (v, c) = merge_value(b.get(k), o.get(k), t.get(k));
                    let v = if c {
                        clash = true;
                        o.get(k).cloned()
                    } else {
                        v
                    };
                    if let Some(v) = v {
                        merged.insert(k.clone(), v);
                    }
                }
                // 種類が変わるなど、合わせると壊れるときはボード側をそのまま使う。
                if merged.get("type") != o.get("type") {
                    merged = o.clone();
                    clash = true;
                }
                if clash {
                    conflicts.push(id.clone());
                }
                doc.elements.insert(id.clone(), merged);
            }
            // 片方が消し、もう片方が変えた: 変えた方を残す。
            (Some(o), None) => {
                conflicts.push(id.clone());
                doc.elements.insert(id.clone(), o.clone());
            }
            (None, Some(t)) => {
                conflicts.push(id.clone());
                doc.elements.insert(id.clone(), t.clone());
            }
            (None, None) => {}
        }
    }
    // つなぎ先の要素が消えたコネクタは、つなぎ先を点にする(マージで消えた要素を指さないように)。
    let present: BTreeSet<String> = doc.elements.keys().cloned().collect();
    for fields in doc.elements.values_mut() {
        for end in ["from", "to"] {
            let dangling = fields
                .get(end)
                .and_then(|v| v.get("element"))
                .and_then(Value::as_str)
                .is_some_and(|e| !present.contains(e));
            if dangling {
                let mut p = Map::new();
                p.insert("x".into(), Value::from(0));
                p.insert("y".into(), Value::from(0));
                fields.insert(end.into(), Value::Object(p));
            }
        }
    }
    Merged { doc, conflicts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::STICKY_COLORS;

    fn sticky(board: &mut Board, x: f32, text: &str) -> ElementId {
        let id = board.alloc_id();
        let mut s = Shape::new(
            id,
            ShapeKind::Sticky,
            Vec2::new(x, 0.0),
            Vec2::splat(200.0),
            STICKY_COLORS[0],
        );
        s.text = text.into();
        board.push(Element::Shape(s))
    }

    fn sample() -> Board {
        let mut b = Board::with_client_id(7);
        let a = sticky(&mut b, 0.0, "よかったこと");
        let c = sticky(&mut b, 300.5, "次にやること\n2 行目");
        let mut conn = Connector::new(
            b.alloc_id(),
            Endpoint::Attached {
                id: a,
                anchor: None,
            },
            Endpoint::Attached {
                id: c,
                anchor: Some(Vec2::new(0.0, 0.5)),
            },
        );
        conn.dashed = true;
        conn.text = "→".into();
        b.push(Element::Connector(conn));
        let mut st = Stroke::new(b.alloc_id(), Vec2::new(1.0, 2.0), Rgba8::BLACK, 3.0);
        st.points.push(Vec2::new(10.25, 20.1));
        b.push(Element::Stroke(st));
        let mut img = Shape::new(
            b.alloc_id(),
            ShapeKind::Image,
            Vec2::new(0.0, 400.0),
            Vec2::new(64.0, 64.0),
            Rgba8::WHITE,
        );
        img.image = Some(ImageData::new(b"\x89PNG fake image".to_vec()));
        img.meta.link = "https://example.com".into();
        img.meta.locked = true;
        b.push(Element::Shape(img));
        b
    }

    #[test]
    fn round_trip_is_lossless() {
        let b = sample();
        let (doc, assets) = RepoDoc::from_board(&b, "振り返り");
        assert_eq!(assets.len(), 1);
        assert!(assets[0].path.starts_with("assets/") && assets[0].path.ends_with(".png"));
        let text = doc.to_text();
        let back = RepoDoc::parse(&text).unwrap();
        assert_eq!(back, doc);
        assert_eq!(back.title, "振り返り");
        let elements = back
            .to_elements(|p| assets.iter().find(|a| a.path == p).map(|a| a.bytes.clone()))
            .unwrap();
        let mut restored = Board::with_client_id(8);
        restored.replace_elements(elements, &[]);
        let (doc2, assets2) = RepoDoc::from_board(&restored, "振り返り");
        assert_eq!(doc2, doc);
        assert_eq!(assets2, assets);
        assert_eq!(doc2.to_text(), text);
    }

    #[test]
    fn one_element_per_line_in_z_order() {
        let b = sample();
        let (doc, _) = RepoDoc::from_board(&b, "t");
        let text = doc.to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "  \"graboard\": 1,");
        let element_lines: Vec<&&str> = lines.iter().filter(|l| l.starts_with("{\"id\"")).collect();
        assert_eq!(element_lines.len(), b.len());
        assert!(element_lines[0].contains("よかったこと"));
        assert!(element_lines[1].contains("\"x\":300.5"));
        // 改行は JSON の中で \n になり、1 行に収まる。
        assert!(element_lines[1].contains("次にやること\\n2 行目"));
        assert!(element_lines[3].contains("[10.25,20.1]"));
        assert!(text.ends_with("]\n}\n"));
    }

    #[test]
    fn moving_one_sticky_changes_one_line() {
        let mut b = sample();
        let (before, _) = RepoDoc::from_board(&b, "t");
        let id = b.elements()[0].id();
        b.update(id, |e| {
            if let Element::Shape(s) = e {
                s.pos.x += 40.0;
            }
        });
        let (after, _) = RepoDoc::from_board(&b, "t");
        let (t1, t2) = (before.to_text(), after.to_text());
        let diff = t1.lines().zip(t2.lines()).filter(|(a, b)| a != b).count();
        assert_eq!(diff, 1);
        assert_eq!(
            before.changes_to(&after),
            Changes {
                added: 0,
                updated: 1,
                removed: 0
            }
        );
    }

    #[test]
    fn parse_errors_are_readable() {
        assert!(RepoDoc::parse("[]").unwrap_err().contains("{…}"));
        assert!(RepoDoc::parse("{}").unwrap_err().contains("graboard"));
        assert!(
            RepoDoc::parse(r#"{"graboard": 99}"#)
                .unwrap_err()
                .contains("新しい版")
        );
        let doc = RepoDoc::parse(
            r#"{"graboard":1,"elements":[{"id":"a","type":"shape","kind":"blob","x":0,"y":0}]}"#,
        )
        .unwrap();
        let err = doc.to_elements(|_| None).unwrap_err();
        assert!(err.contains("blob"), "{err}");
        let dup =
            r#"{"graboard":1,"elements":[{"id":"a","type":"stroke"},{"id":"a","type":"stroke"}]}"#;
        assert!(RepoDoc::parse(dup).unwrap_err().contains("重複"));
    }

    #[test]
    fn hand_written_file_with_defaults() {
        let doc = RepoDoc::parse(
            r##"{"graboard":1,"title":"手書き","elements":[
{"id":"1","type":"shape","kind":"sticky","x":0,"y":0,"text":"A","fill":"#fff59d"},
{"id":"2","type":"shape","kind":"rect","x":300,"y":0},
{"id":"4","type":"shape","kind":"sticky","x":0,"y":300},
{"id":"3","type":"connector","from":{"element":"1"},"to":{"element":"2"}}
]}"##,
        )
        .unwrap();
        let els = doc.to_elements(|_| None).unwrap();
        assert_eq!(els.len(), 4);
        let mut b = Board::new();
        let ch = apply_to_board(&mut b, &doc, |_| None).unwrap();
        assert_eq!(ch.added, 4);
        assert_eq!(b.len(), 4);
        assert!(matches!(b.get(ElementId(3)), Some(Element::Connector(_))));
        // 省略した色は種類ごとの既定値(付箋は黄色)。
        match b.get(ElementId(2)) {
            Some(Element::Shape(s)) => assert_eq!(s.fill, Rgba8::WHITE),
            other => panic!("{other:?}"),
        }
        let (doc, _) = RepoDoc::from_board(&b, "t");
        assert_eq!(doc.elements["1"]["fill"], "#fff59d");
        assert_eq!(
            doc.elements["4"]["fill"],
            color_string(crate::color::STICKY_COLORS[0]).as_str()
        );
    }

    #[test]
    fn apply_to_board_writes_only_differences() {
        let mut b = sample();
        let (mut doc, _) = RepoDoc::from_board(&b, "t");
        let first = doc.elements.keys().next().unwrap().clone();
        doc.elements
            .get_mut(&first)
            .unwrap()
            .insert("text".into(), Value::String("書き換え".into()));
        let last = doc.ordered_ids().last().map(|s| (*s).clone()).unwrap();
        doc.elements.remove(&last);
        // 画像ファイルが読めなくても、ボードにある画像を使う。
        let ch = apply_to_board(&mut b, &doc, |_| None).unwrap();
        assert_eq!(
            ch,
            Changes {
                added: 0,
                updated: 1,
                removed: 1
            }
        );
        let (now, _) = RepoDoc::from_board(&b, "t");
        assert_eq!(now, doc);
        // もう一度当てても何も変わらない。
        assert!(apply_to_board(&mut b, &doc, |_| None).unwrap().is_empty());
    }

    fn doc(items: &[(&str, &[(&str, Value)])]) -> RepoDoc {
        RepoDoc {
            title: "t".into(),
            elements: items
                .iter()
                .map(|(id, fields)| {
                    (
                        id.to_string(),
                        fields
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.clone()))
                            .collect(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn merge_takes_both_sides() {
        let base = doc(&[
            ("1", &[("text", "a".into()), ("x", 0.into())]),
            ("2", &[("text", "b".into())]),
            ("3", &[("text", "c".into())]),
        ]);
        // ボード: 1 の位置を変え、2 を消し、4 を足す。
        let ours = doc(&[
            ("1", &[("text", "a".into()), ("x", 50.into())]),
            ("3", &[("text", "c".into())]),
            ("4", &[("text", "d".into())]),
        ]);
        // リポジトリ: 1 の文字を変え、3 を変え、5 を足す。
        let theirs = doc(&[
            ("1", &[("text", "A".into()), ("x", 0.into())]),
            ("2", &[("text", "b".into())]),
            ("3", &[("text", "C".into())]),
            ("5", &[("text", "e".into())]),
        ]);
        let m = merge3(Some(&base), &ours, &theirs);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let want = doc(&[
            ("1", &[("text", "A".into()), ("x", 50.into())]),
            ("3", &[("text", "C".into())]),
            ("4", &[("text", "d".into())]),
            ("5", &[("text", "e".into())]),
        ]);
        assert_eq!(m.doc, want);
    }

    #[test]
    fn merge_conflicts_keep_board_side() {
        let base = doc(&[
            ("1", &[("text", "a".into())]),
            ("2", &[("text", "b".into())]),
        ]);
        let ours = doc(&[("1", &[("text", "ours".into())])]);
        let theirs = doc(&[
            ("1", &[("text", "theirs".into())]),
            ("2", &[("text", "B".into())]),
        ]);
        let m = merge3(Some(&base), &ours, &theirs);
        assert_eq!(m.conflicts, vec!["1".to_string(), "2".to_string()]);
        assert_eq!(m.doc.elements["1"]["text"], "ours");
        // 消した要素をリポジトリ側で変えていたら、変えた方を残す。
        assert_eq!(m.doc.elements["2"]["text"], "B");
    }

    #[test]
    fn merge_without_base_unions() {
        let ours = doc(&[("1", &[("text", "a".into())])]);
        let theirs = doc(&[
            ("1", &[("text", "a".into())]),
            ("2", &[("text", "b".into())]),
        ]);
        let m = merge3(None, &ours, &theirs);
        assert!(m.conflicts.is_empty());
        assert_eq!(m.doc.elements.len(), 2);
    }

    #[test]
    fn merge_detaches_connectors_to_removed_elements() {
        let base = doc(&[
            ("1", &[("type", "shape".into())]),
            (
                "2",
                &[
                    ("type", "connector".into()),
                    ("from", serde_json::json!({"x": 0, "y": 0})),
                ],
            ),
        ]);
        // ボード: コネクタを要素 1 につなぐ。リポジトリ: 要素 1 を消す。
        let ours = doc(&[
            ("1", &[("type", "shape".into())]),
            (
                "2",
                &[
                    ("type", "connector".into()),
                    ("from", serde_json::json!({"element": "1"})),
                ],
            ),
        ]);
        let theirs = doc(&[(
            "2",
            &[
                ("type", "connector".into()),
                ("from", serde_json::json!({"x": 0, "y": 0})),
            ],
        )]);
        let m = merge3(Some(&base), &ours, &theirs);
        // 要素 1 はボード側で変えていないので消え、コネクタは点につながる。
        assert!(!m.doc.elements.contains_key("1"));
        assert_eq!(
            m.doc.elements["2"]["from"],
            serde_json::json!({"x": 0, "y": 0})
        );
    }
}
