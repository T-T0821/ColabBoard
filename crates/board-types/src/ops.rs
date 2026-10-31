//! 外部からボードを読み書きするための JSON モデル。
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

use serde::{Deserialize, Serialize};

/// 1 回の一括操作で受け付ける操作の数。
pub const MAX_OPS: usize = 2000;
/// 文字の長さの上限(文字数)。
pub const MAX_TEXT_CHARS: usize = 20_000;
/// 手書きの線の点の数の上限。
pub const MAX_POINTS: usize = 10_000;
/// 座標の絶対値の上限。
pub const MAX_COORD: f32 = 1.0e7;

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
        /// 破線。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        dashed: bool,
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
    /// `none` / `arrow` / `triangle` / `triangle-filled` / `diamond` / `diamond-filled` /
    /// `circle` / `one` / `many` / `zero-or-one` / `one-or-many` / `zero-or-many`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_cap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_cap: Option<String>,
    /// 破線にするか。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashed: Option<bool>,
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
    pub dashed: Option<bool>,
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
