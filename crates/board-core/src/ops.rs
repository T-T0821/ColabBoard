//! 外部からボードを読み書きするための JSON モデルと操作。
//!
//! REST API(`/api/v1/...`)と WASM プラグインの両方がこの形でボードを扱う。
//! 要素の ID は 16 進の文字列(`"1a2b3c"`)、色は `"#rrggbb"` か `"#rrggbbaa"`。
//!
//! ```json
//! {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 0, "y": 0, "text": "メモ"}}
//! {"op": "update", "id": "1a2b", "set": {"x": 100, "fill": "#b5e3ff"}}
//! {"op": "delete", "id": "1a2b"}
//! ```
//!
//! 同じ一括操作の中で作った要素は `"$0"`, `"$1"`, …(作った順)で参照できる。
//! コネクタで新しい付箋どうしをつなぐときに使う。

use glam::Vec2;
use serde::{Deserialize, Serialize};

use crate::board::Board;
use crate::color::{Rgba8, STICKY_COLORS};
use crate::element::{Cap, Connector, Element, ElementId, Endpoint, PathStyle, Stroke};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind, TextAlign};

/// 1 回の一括操作で受け付ける操作の数。
pub const MAX_OPS: usize = 2000;
/// 文字の長さの上限(文字数)。
pub const MAX_TEXT_CHARS: usize = 20_000;
/// 手書きの線の点の数の上限。
pub const MAX_POINTS: usize = 10_000;
/// 座標の絶対値の上限。
pub const MAX_COORD: f32 = 1.0e7;

// ---- 読み出し ----

/// 要素の JSON 表現。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ElementData {
    pub id: String,
    #[serde(flatten)]
    pub body: ElementBody,
    /// 重なり順(大きいほど手前)。
    pub z: f64,
    /// グループ(0 ならなし)。
    #[serde(default)]
    pub group: u64,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub link: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ElementBody {
    Shape {
        /// 図形の種類(`sticky`、`rect`、`ellipse`、`text`、`frame` など)。
        kind: String,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        text: String,
        fill: String,
        stroke: String,
        stroke_width: f32,
        text_color: String,
        /// 0 なら種類ごとの既定値。
        font_size: f32,
        align: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        icon: String,
        /// 画像を持つか(画像そのものは返さない)。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        has_image: bool,
    },
    Connector {
        from: EndpointData,
        to: EndpointData,
        text: String,
        color: String,
        width: f32,
        style: String,
        start_cap: String,
        end_cap: String,
    },
    Stroke {
        points: Vec<[f32; 2]>,
        color: String,
        width: f32,
        highlighter: bool,
    },
}

/// コネクタの端。点か、要素につながった端。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EndpointData {
    /// 要素につながった端。`anchor` は相手の外接矩形内の位置(0〜1)。省略すると自動。
    Element {
        element: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor: Option<[f32; 2]>,
    },
    Point {
        x: f32,
        y: f32,
    },
}

impl ElementData {
    pub fn kind_name(&self) -> &str {
        match &self.body {
            ElementBody::Shape { kind, .. } => kind,
            ElementBody::Connector { .. } => "connector",
            ElementBody::Stroke { .. } => "stroke",
        }
    }
}

pub fn id_string(id: ElementId) -> String {
    format!("{:x}", id.0)
}

pub fn parse_id(s: &str) -> Option<ElementId> {
    if s.is_empty() || s.len() > 16 {
        return None;
    }
    u64::from_str_radix(s, 16).ok().map(ElementId)
}

pub fn color_string(c: Rgba8) -> String {
    let [r, g, b, a] = c.0;
    if a == 0xff {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// `#rgb` / `#rrggbb` / `#rrggbbaa`(`#` は省略可)。
pub fn parse_color(s: &str) -> Option<Rgba8> {
    let h = s.trim().trim_start_matches('#');
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        3 => {
            let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
            Some(Rgba8([d(0)?, d(1)?, d(2)?, 0xff]))
        }
        6 => Some(Rgba8([byte(0)?, byte(2)?, byte(4)?, 0xff])),
        8 => Some(Rgba8([byte(0)?, byte(2)?, byte(4)?, byte(6)?])),
        _ => None,
    }
}

fn endpoint_data(e: &Endpoint) -> EndpointData {
    match e {
        Endpoint::Free(p) => EndpointData::Point { x: p.x, y: p.y },
        Endpoint::Attached { id, anchor } => EndpointData::Element {
            element: id_string(*id),
            anchor: anchor.map(|a| [a.x, a.y]),
        },
    }
}

/// 要素を JSON 表現にする。
pub fn to_data(e: &Element, z: f64) -> ElementData {
    let meta = e.meta();
    let body = match e {
        Element::Shape(s) => ElementBody::Shape {
            kind: s.kind.name().to_string(),
            x: s.pos.x,
            y: s.pos.y,
            w: s.size.x,
            h: s.size.y,
            text: s.text.clone(),
            fill: color_string(s.fill),
            stroke: color_string(s.stroke),
            stroke_width: s.stroke_width,
            text_color: color_string(s.text_color),
            font_size: s.font_size,
            align: s.align.name().to_string(),
            icon: s.icon.clone(),
            has_image: s.image.is_some(),
        },
        Element::Connector(c) => ElementBody::Connector {
            from: endpoint_data(&c.from),
            to: endpoint_data(&c.to),
            text: c.text.clone(),
            color: color_string(c.color),
            width: c.width,
            style: c.style.name().to_string(),
            start_cap: c.start_cap.name().to_string(),
            end_cap: c.end_cap.name().to_string(),
        },
        Element::Stroke(s) => ElementBody::Stroke {
            points: s.points.iter().map(|p| [p.x, p.y]).collect(),
            color: color_string(s.color),
            width: s.width,
            highlighter: s.highlighter,
        },
    };
    ElementData {
        id: id_string(e.id()),
        body,
        z,
        group: meta.group,
        locked: meta.locked,
        link: meta.link.clone(),
    }
}

/// 要素の絞り込み。どれも省略できる(全部省略すれば全要素)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ElementQuery {
    /// `shape` / `connector` / `stroke`。
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub element_type: Option<String>,
    /// 図形の種類(`sticky` など)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// この範囲 `[x0, y0, x1, y1]` に重なる要素。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f32; 4]>,
    /// 文字にこの語を含む要素(大文字小文字は区別しない)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// ID を指定して取る。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    /// 最大の件数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

fn type_name(e: &Element) -> &'static str {
    match e {
        Element::Shape(_) => "shape",
        Element::Connector(_) => "connector",
        Element::Stroke(_) => "stroke",
    }
}

/// 条件に合う要素を、重なり順(奥から)で返す。
pub fn query(board: &Board, q: &ElementQuery) -> Vec<ElementData> {
    let needle = q.text.as_ref().map(|t| t.to_lowercase());
    let ids: Option<Vec<ElementId>> = q
        .ids
        .as_ref()
        .map(|v| v.iter().filter_map(|s| parse_id(s)).collect());
    let bbox = q
        .bbox
        .map(|[x0, y0, x1, y1]| Rect::from_points(Vec2::new(x0, y0), Vec2::new(x1, y1)));
    let limit = q.limit.unwrap_or(usize::MAX);
    board
        .elements()
        .iter()
        .filter(|e| q.element_type.as_deref().is_none_or(|t| t == type_name(e)))
        .filter(|e| {
            q.kind.as_deref().is_none_or(|k| match e {
                Element::Shape(s) => s.kind.name() == k,
                other => type_name(other) == k,
            })
        })
        .filter(|e| ids.as_ref().is_none_or(|ids| ids.contains(&e.id())))
        .filter(|e| bbox.as_ref().is_none_or(|r| r.intersects(&e.bounds())))
        .filter(|e| {
            needle
                .as_ref()
                .is_none_or(|n| e.text().to_lowercase().contains(n.as_str()))
        })
        .take(limit)
        .map(|e| to_data(e, board.z_of(e.id()).unwrap_or(0.0)))
        .collect()
}

/// 1 要素を取る。
pub fn get(board: &Board, id: &str) -> Option<ElementData> {
    let id = parse_id(id)?;
    let e = board.get(id)?;
    Some(to_data(e, board.z_of(id).unwrap_or(0.0)))
}

// ---- 書き込み ----

/// 新しい要素。省略したところは種類ごとの既定値になる。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NewElement {
    Shape {
        /// 省略すると `rect`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default)]
        x: f32,
        #[serde(default)]
        y: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        w: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        h: Option<f32>,
        #[serde(default, flatten)]
        style: ShapeStyle,
        #[serde(default, flatten)]
        meta: MetaPatch,
    },
    Connector {
        from: EndpointData,
        to: EndpointData,
        #[serde(default, flatten)]
        style: ConnectorStyle,
        #[serde(default, flatten)]
        meta: MetaPatch,
    },
    Stroke {
        points: Vec<[f32; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        highlighter: Option<bool>,
        #[serde(default, flatten)]
        meta: MetaPatch,
    },
}

impl NewElement {
    /// 付箋を作る操作(よく使うので近道)。
    pub fn sticky(x: f32, y: f32, text: impl Into<String>) -> Self {
        NewElement::Shape {
            kind: Some("sticky".into()),
            x,
            y,
            w: None,
            h: None,
            style: ShapeStyle {
                text: Some(text.into()),
                ..Default::default()
            },
            meta: MetaPatch::default(),
        }
    }

    /// 図形を作る操作。
    pub fn shape(kind: &str, x: f32, y: f32, w: f32, h: f32) -> Self {
        NewElement::Shape {
            kind: Some(kind.into()),
            x,
            y,
            w: Some(w),
            h: Some(h),
            style: ShapeStyle::default(),
            meta: MetaPatch::default(),
        }
    }

    /// 2 つの端をつなぐコネクタ(矢印)。
    pub fn connector(from: EndpointData, to: EndpointData) -> Self {
        NewElement::Connector {
            from,
            to,
            style: ConnectorStyle::default(),
            meta: MetaPatch::default(),
        }
    }
}

impl EndpointData {
    pub fn element(id: impl Into<String>) -> Self {
        EndpointData::Element {
            element: id.into(),
            anchor: None,
        }
    }

    pub fn point(x: f32, y: f32) -> Self {
        EndpointData::Point { x, y }
    }
}

/// 図形の見た目(作るときと変えるときの両方で使う)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShapeStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    /// `left` / `center` / `right`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// コネクタの見た目。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectorStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    /// `straight` / `elbow` / `curved`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// `none` / `arrow`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_cap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_cap: Option<String>,
}

/// どの要素にもある属性。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetaPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// 既存の要素の変更。指定したところだけ変わる。要素の種類に合わないフィールドはエラー。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ElementPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f32>,
    /// 図形の種類を変える。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// 重なり順。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<f64>,
    #[serde(default, flatten)]
    pub shape: ShapeStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<EndpointData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<EndpointData>,
    /// コネクタ・手書きの線の色。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// コネクタ・手書きの線の太さ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_cap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_cap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f32; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlighter: Option<bool>,
    #[serde(default, flatten)]
    pub meta: MetaPatch,
}

/// 1 つの操作。
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Create { element: NewElement },
    Update { id: String, set: ElementPatch },
    Delete { id: String },
}

/// 一括操作のリクエスト。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpsRequest {
    pub ops: Vec<Op>,
}

impl From<Vec<Op>> for OpsRequest {
    fn from(ops: Vec<Op>) -> Self {
        OpsRequest { ops }
    }
}

/// 一括操作の結果。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpsResult {
    /// 作った要素の ID(作った順。`$0`, `$1`, … に対応する)。
    pub created: Vec<String>,
    /// 作った・変えた要素の今の状態(操作の順、重複なし)。
    pub elements: Vec<ElementData>,
    /// 消した要素の ID。
    pub deleted: Vec<String>,
}

/// 操作の誤り。どの操作で失敗したか(0 始まり)と理由。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpError {
    pub index: usize,
    pub message: String,
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} 番目の操作: {}", self.index, self.message)
    }
}

impl std::error::Error for OpError {}

type R<T> = Result<T, String>;

fn coord(v: f32, what: &str) -> R<f32> {
    if v.is_finite() && v.abs() <= MAX_COORD {
        Ok(v)
    } else {
        Err(format!("{what} が範囲外です"))
    }
}

fn size(v: f32, what: &str) -> R<f32> {
    if v.is_finite() && (1.0..=MAX_COORD).contains(&v) {
        Ok(v)
    } else {
        Err(format!("{what} は 1 以上にしてください"))
    }
}

fn non_negative(v: f32, what: &str) -> R<f32> {
    if v.is_finite() && (0.0..=10_000.0).contains(&v) {
        Ok(v)
    } else {
        Err(format!("{what} が範囲外です"))
    }
}

fn color(s: &str) -> R<Rgba8> {
    parse_color(s).ok_or_else(|| format!("色「{s}」は #rrggbb か #rrggbbaa で指定してください"))
}

fn text(s: &str) -> R<String> {
    if s.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("文字は {MAX_TEXT_CHARS} 文字までです"));
    }
    Ok(s.to_string())
}

fn kind(s: &str) -> R<ShapeKind> {
    match ShapeKind::from_name(s) {
        Some(ShapeKind::Image) => Err("画像はこの API では作れません".into()),
        Some(k) => Ok(k),
        None => Err(format!("図形の種類「{s}」はありません")),
    }
}

fn align(s: &str) -> R<TextAlign> {
    match s {
        "left" | "center" | "right" => Ok(TextAlign::from_name(s)),
        _ => Err(format!("揃え「{s}」は left / center / right のどれかです")),
    }
}

fn path_style(s: &str) -> R<PathStyle> {
    match s {
        "straight" | "elbow" | "curved" => Ok(PathStyle::from_name(s)),
        _ => Err(format!(
            "線の引き方「{s}」は straight / elbow / curved のどれかです"
        )),
    }
}

fn cap(s: &str) -> R<Cap> {
    match s {
        "none" | "arrow" => Ok(Cap::from_name(s)),
        _ => Err(format!("端の飾り「{s}」は none / arrow のどれかです")),
    }
}

fn points(pts: &[[f32; 2]]) -> R<Vec<Vec2>> {
    if pts.is_empty() || pts.len() > MAX_POINTS {
        return Err(format!("点は 1〜{MAX_POINTS} 個にしてください"));
    }
    pts.iter()
        .map(|[x, y]| Ok(Vec2::new(coord(*x, "x")?, coord(*y, "y")?)))
        .collect()
}

/// 操作を順に適用する中での状態(`$N` の解決に使う)。
struct Ctx<'a> {
    board: &'a mut Board,
    created: Vec<ElementId>,
}

impl Ctx<'_> {
    fn resolve(&self, s: &str) -> R<ElementId> {
        if let Some(n) = s.strip_prefix('$') {
            let i: usize = n.parse().map_err(|_| format!("「{s}」は参照できません"))?;
            return self
                .created
                .get(i)
                .copied()
                .ok_or_else(|| format!("「{s}」はまだ作られていません"));
        }
        let id = parse_id(s).ok_or_else(|| format!("要素 ID「{s}」が正しくありません"))?;
        if self.board.get(id).is_none() {
            return Err(format!("要素「{s}」はありません"));
        }
        Ok(id)
    }

    fn endpoint(&self, e: &EndpointData) -> R<Endpoint> {
        Ok(match e {
            EndpointData::Point { x, y } => {
                Endpoint::Free(Vec2::new(coord(*x, "x")?, coord(*y, "y")?))
            }
            EndpointData::Element { element, anchor } => Endpoint::Attached {
                id: self.resolve(element)?,
                anchor: match anchor {
                    Some([ax, ay]) if (0.0..=1.0).contains(ax) && (0.0..=1.0).contains(ay) => {
                        Some(Vec2::new(*ax, *ay))
                    }
                    Some(_) => return Err("anchor は 0〜1 で指定してください".into()),
                    None => None,
                },
            },
        })
    }
}

fn apply_meta(e: &mut Element, m: &MetaPatch) -> R<()> {
    let meta = e.meta_mut();
    if let Some(g) = m.group {
        meta.group = g;
    }
    if let Some(l) = m.locked {
        meta.locked = l;
    }
    if let Some(link) = &m.link {
        if link.len() > 2048 {
            return Err("リンクが長すぎます".into());
        }
        meta.link = link.clone();
    }
    Ok(())
}

fn apply_shape_style(s: &mut Shape, st: &ShapeStyle) -> R<()> {
    if let Some(t) = &st.text {
        s.text = text(t)?;
    }
    if let Some(c) = &st.fill {
        s.fill = color(c)?;
    }
    if let Some(c) = &st.stroke {
        s.stroke = color(c)?;
    }
    if let Some(w) = st.stroke_width {
        s.stroke_width = non_negative(w, "stroke_width")?;
    }
    if let Some(c) = &st.text_color {
        s.text_color = color(c)?;
    }
    if let Some(f) = st.font_size {
        s.font_size = non_negative(f, "font_size")?;
    }
    if let Some(a) = &st.align {
        s.align = align(a)?;
    }
    if let Some(i) = &st.icon {
        if i.len() > 128 {
            return Err("icon が長すぎます".into());
        }
        s.icon = i.clone();
    }
    Ok(())
}

fn shape_style_is_empty(st: &ShapeStyle) -> bool {
    *st == ShapeStyle::default()
}

fn build(ctx: &mut Ctx<'_>, new: &NewElement) -> R<Element> {
    let id = ctx.board.alloc_id();
    let mut e = match new {
        NewElement::Shape {
            kind: k,
            x,
            y,
            w,
            h,
            style,
            meta: _,
        } => {
            let k = kind(k.as_deref().unwrap_or("rect"))?;
            let def = k.default_size();
            let pos = Vec2::new(coord(*x, "x")?, coord(*y, "y")?);
            let sz = Vec2::new(
                size(
                    w.unwrap_or(if k == ShapeKind::Sticky { 200.0 } else { def.x }),
                    "w",
                )?,
                size(
                    h.unwrap_or(if k == ShapeKind::Sticky { 200.0 } else { def.y }),
                    "h",
                )?,
            );
            let fill = if k == ShapeKind::Sticky {
                STICKY_COLORS[0]
            } else {
                Rgba8::WHITE
            };
            let mut s = Shape::new(id, k, pos, sz, fill);
            apply_shape_style(&mut s, style)?;
            Element::Shape(s)
        }
        NewElement::Connector {
            from, to, style, ..
        } => {
            let mut c = Connector::new(id, ctx.endpoint(from)?, ctx.endpoint(to)?);
            apply_connector_style(&mut c, style)?;
            Element::Connector(c)
        }
        NewElement::Stroke {
            points: pts,
            color: c,
            width,
            highlighter,
            ..
        } => {
            let pts = points(pts)?;
            let mut s = Stroke::new(id, pts[0], Rgba8::BLACK, 4.0);
            s.points = pts;
            if let Some(c) = c {
                s.color = color(c)?;
            }
            if let Some(w) = width {
                s.width = size(*w, "width")?;
            }
            if let Some(h) = highlighter {
                s.highlighter = *h;
            }
            Element::Stroke(s)
        }
    };
    let meta = match new {
        NewElement::Shape { meta, .. }
        | NewElement::Connector { meta, .. }
        | NewElement::Stroke { meta, .. } => meta,
    };
    apply_meta(&mut e, meta)?;
    Ok(e)
}

fn apply_connector_style(c: &mut Connector, st: &ConnectorStyle) -> R<()> {
    if let Some(t) = &st.text {
        c.text = text(t)?;
    }
    if let Some(v) = &st.color {
        c.color = color(v)?;
    }
    if let Some(w) = st.width {
        c.width = size(w, "width")?;
    }
    if let Some(s) = &st.style {
        c.style = path_style(s)?;
    }
    if let Some(s) = &st.start_cap {
        c.start_cap = cap(s)?;
    }
    if let Some(s) = &st.end_cap {
        c.end_cap = cap(s)?;
    }
    Ok(())
}

fn patch(ctx: &Ctx<'_>, e: &mut Element, p: &ElementPatch) -> R<()> {
    let only = |ok: bool, field: &str, ty: &str| -> R<()> {
        if ok {
            Ok(())
        } else {
            Err(format!("{field} は {ty} には使えません"))
        }
    };
    let is_shape = matches!(e, Element::Shape(_));
    let is_conn = matches!(e, Element::Connector(_));
    let is_stroke = matches!(e, Element::Stroke(_));
    let ty = type_name(e);
    only(is_shape || p.kind.is_none(), "kind", ty)?;
    only(is_shape || p.w.is_none() && p.h.is_none(), "w / h", ty)?;
    only(
        is_shape || (shape_style_is_empty(&p.shape) || is_conn && only_text(&p.shape)),
        "図形の見た目",
        ty,
    )?;
    only(
        is_conn || p.from.is_none() && p.to.is_none(),
        "from / to",
        ty,
    )?;
    only(
        is_conn || p.style.is_none() && p.start_cap.is_none() && p.end_cap.is_none(),
        "style / start_cap / end_cap",
        ty,
    )?;
    only(
        !is_shape || p.color.is_none() && p.width.is_none(),
        "color / width",
        ty,
    )?;
    only(
        is_stroke || p.points.is_none() && p.highlighter.is_none(),
        "points / highlighter",
        ty,
    )?;
    only(
        !is_conn || p.x.is_none() && p.y.is_none(),
        "x / y(端は from / to で指定)",
        ty,
    )?;

    match e {
        Element::Shape(s) => {
            if let Some(k) = &p.kind {
                s.kind = kind(k)?;
            }
            if let Some(x) = p.x {
                s.pos.x = coord(x, "x")?;
            }
            if let Some(y) = p.y {
                s.pos.y = coord(y, "y")?;
            }
            if let Some(w) = p.w {
                s.size.x = size(w, "w")?;
            }
            if let Some(h) = p.h {
                s.size.y = size(h, "h")?;
            }
            apply_shape_style(s, &p.shape)?;
        }
        Element::Connector(c) => {
            if let Some(f) = &p.from {
                c.from = ctx.endpoint(f)?;
            }
            if let Some(t) = &p.to {
                c.to = ctx.endpoint(t)?;
            }
            apply_connector_style(
                c,
                &ConnectorStyle {
                    text: p.shape.text.clone(),
                    color: p.color.clone(),
                    width: p.width,
                    style: p.style.clone(),
                    start_cap: p.start_cap.clone(),
                    end_cap: p.end_cap.clone(),
                },
            )?;
        }
        Element::Stroke(s) => {
            if let Some(pts) = &p.points {
                s.points = points(pts)?;
            }
            // 位置の指定は外接矩形の左上を動かす。
            if p.x.is_some() || p.y.is_some() {
                let b = s.bounds();
                let d = Vec2::new(
                    p.x.map_or(0.0, |x| x - b.min.x),
                    p.y.map_or(0.0, |y| y - b.min.y),
                );
                coord(b.min.x + d.x, "x")?;
                coord(b.min.y + d.y, "y")?;
                s.points.iter_mut().for_each(|q| *q += d);
            }
            if let Some(c) = &p.color {
                let c = color(c)?;
                s.color = c;
            }
            if let Some(w) = p.width {
                s.width = size(w, "width")?;
            }
            if let Some(h) = p.highlighter {
                s.highlighter = h;
            }
        }
    }
    apply_meta(e, &p.meta)
}

fn only_text(st: &ShapeStyle) -> bool {
    ShapeStyle {
        text: None,
        ..st.clone()
    } == ShapeStyle::default()
}

/// 操作を順に適用する。途中で失敗したら、それまでの変更も含めて呼び出し側で捨てること
/// (サーバーやプラグインの実行環境は使い捨てのボードに適用してから全員に配る)。
pub fn apply(board: &mut Board, ops: &[Op]) -> Result<OpsResult, OpError> {
    if ops.len() > MAX_OPS {
        return Err(OpError {
            index: MAX_OPS,
            message: format!("1 回に送れる操作は {MAX_OPS} 個までです"),
        });
    }
    let mut ctx = Ctx {
        board,
        created: Vec::new(),
    };
    let mut touched: Vec<ElementId> = Vec::new();
    let mut deleted: Vec<ElementId> = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let err = |message: String| OpError { index, message };
        match op {
            Op::Create { element } => {
                let e = build(&mut ctx, element).map_err(err)?;
                let id = ctx.board.push(e);
                ctx.created.push(id);
                touched.push(id);
            }
            Op::Update { id, set } => {
                let id = ctx.resolve(id).map_err(err)?;
                let mut e = ctx
                    .board
                    .get(id)
                    .cloned()
                    .ok_or_else(|| err("要素がありません".into()))?;
                patch(&ctx, &mut e, set).map_err(err)?;
                if let Some(z) = set.z {
                    if !z.is_finite() {
                        return Err(err("z が範囲外です".into()));
                    }
                    ctx.board.set_z(&[(id, z)]);
                }
                ctx.board.update(id, |cur| *cur = e);
                touched.push(id);
            }
            Op::Delete { id } => {
                let id = ctx.resolve(id).map_err(err)?;
                ctx.board.remove(&[id]);
                touched.retain(|t| *t != id);
                deleted.push(id);
            }
        }
    }
    let created = ctx.created.iter().map(|id| id_string(*id)).collect();
    let board = ctx.board;
    let mut seen = std::collections::HashSet::new();
    let elements = touched
        .into_iter()
        .filter(|id| seen.insert(*id))
        .filter_map(|id| {
            board
                .get(id)
                .map(|e| to_data(e, board.z_of(id).unwrap_or(0.0)))
        })
        .collect();
    Ok(OpsResult {
        created,
        elements,
        deleted: deleted.into_iter().map(id_string).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ops(v: serde_json::Value) -> Vec<Op> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some(Rgba8::WHITE));
        assert_eq!(parse_color("112233"), Some(Rgba8::hex(0x112233)));
        assert_eq!(
            parse_color("#11223380"),
            Some(Rgba8([0x11, 0x22, 0x33, 0x80]))
        );
        assert_eq!(parse_color("#12"), None);
        assert_eq!(parse_color("#gggggg"), None);
        assert_eq!(color_string(Rgba8::hex(0xb5e3ff)), "#b5e3ff");
        assert_eq!(color_string(Rgba8::TRANSPARENT), "#00000000");
    }

    #[test]
    fn create_update_delete() {
        let mut b = Board::new();
        let r = apply(
            &mut b,
            &ops(json!([
                {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 10, "y": 20, "text": "A"}},
                {"op": "create", "element": {"type": "shape", "kind": "ellipse", "x": 300, "y": 20, "fill": "#b5e3ff"}},
                {"op": "create", "element": {"type": "connector", "from": {"element": "$0"}, "to": {"element": "$1"}, "text": "→"}},
            ])),
        )
        .unwrap();
        assert_eq!(r.created.len(), 3);
        assert_eq!(b.len(), 3);
        let sticky = get(&b, &r.created[0]).unwrap();
        match &sticky.body {
            ElementBody::Shape {
                kind,
                w,
                text,
                fill,
                ..
            } => {
                assert_eq!(kind, "sticky");
                assert_eq!(*w, 200.0);
                assert_eq!(text, "A");
                assert_eq!(fill, "#fff3a3");
            }
            other => panic!("{other:?}"),
        }
        let conn = get(&b, &r.created[2]).unwrap();
        assert!(matches!(
            &conn.body,
            ElementBody::Connector { from: EndpointData::Element { element, .. }, .. } if *element == r.created[0]
        ));

        let r2 = apply(
            &mut b,
            &ops(json!([
                {"op": "update", "id": r.created[0], "set": {"x": 50, "text": "B", "locked": true, "z": 100.0}},
                {"op": "delete", "id": r.created[1]},
            ])),
        )
        .unwrap();
        assert_eq!(r2.deleted, vec![r.created[1].clone()]);
        let s = get(&b, &r.created[0]).unwrap();
        assert!(s.locked);
        assert_eq!(s.z, 100.0);
        assert!(
            matches!(s.body, ElementBody::Shape { x, ref text, .. } if x == 50.0 && text == "B")
        );
        assert_eq!(b.len(), 2);

        // JSON の往復。
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["type"], "shape");
        assert_eq!(serde_json::from_value::<ElementData>(v).unwrap(), s);
    }

    #[test]
    fn errors_point_at_the_op() {
        let mut b = Board::new();
        let e = apply(
            &mut b,
            &ops(json!([
                {"op": "create", "element": {"type": "shape"}},
                {"op": "create", "element": {"type": "shape", "kind": "nope"}},
            ])),
        )
        .unwrap_err();
        assert_eq!(e.index, 1);
        let e = apply(&mut b, &ops(json!([{"op": "delete", "id": "ffff"}]))).unwrap_err();
        assert!(e.message.contains("ありません"));
        let id = b.elements()[0].id();
        let e = apply(
            &mut b,
            &ops(json!([{"op": "update", "id": id_string(id), "set": {"from": {"x": 0, "y": 0}}}])),
        )
        .unwrap_err();
        assert!(e.message.contains("from"), "{}", e.message);
        let e = apply(
            &mut b,
            &ops(json!([{"op": "create", "element": {"type": "shape", "fill": "red"}}])),
        )
        .unwrap_err();
        assert!(e.message.contains("色"));
    }

    #[test]
    fn queries() {
        let mut b = Board::new();
        apply(
            &mut b,
            &ops(json!([
                {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 0, "y": 0, "text": "Apple"}},
                {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 1000, "y": 0, "text": "banana"}},
                {"op": "create", "element": {"type": "shape", "kind": "rect", "x": 0, "y": 500}},
                {"op": "create", "element": {"type": "stroke", "points": [[0, 0], [10, 10]]}},
            ])),
        )
        .unwrap();
        let q = |v: serde_json::Value| query(&b, &serde_json::from_value(v).unwrap()).len();
        assert_eq!(q(json!({})), 4);
        assert_eq!(q(json!({"kind": "sticky"})), 2);
        assert_eq!(q(json!({"type": "stroke"})), 1);
        assert_eq!(q(json!({"text": "APPLE"})), 1);
        assert_eq!(q(json!({"bbox": [-10, -10, 300, 300]})), 2);
        assert_eq!(q(json!({"limit": 3})), 3);
    }

    #[test]
    fn stroke_move_and_sticky_helper() {
        let mut b = Board::new();
        let r = apply(
            &mut b,
            &[
                Op::Create {
                    element: NewElement::sticky(5.0, 5.0, "hi"),
                },
                Op::Create {
                    element: NewElement::Stroke {
                        points: vec![[0.0, 0.0], [10.0, 0.0]],
                        color: Some("#ff0000".into()),
                        width: Some(2.0),
                        highlighter: None,
                        meta: MetaPatch::default(),
                    },
                },
            ],
        )
        .unwrap();
        apply(
            &mut b,
            &[Op::Update {
                id: r.created[1].clone(),
                set: ElementPatch {
                    x: Some(99.0),
                    ..Default::default()
                },
            }],
        )
        .unwrap();
        let s = get(&b, &r.created[1]).unwrap();
        assert!(matches!(s.body, ElementBody::Stroke { ref points, .. } if points[0][0] == 100.0));
    }
}
