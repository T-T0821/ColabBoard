use glam::Vec2;

use crate::color::Rgba8;

use crate::element::{ElementId, Meta};
use crate::geom::Rect;

/// 図形の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShapeKind {
    Rect,
    Ellipse,
    Sticky,
    Triangle,
    Diamond,
    Star,
    Hexagon,
    Parallelogram,
    /// 枠なしの文字だけの要素。
    Text,
    /// フレーム(セクション)。`text` は見出し。中に置いた要素ごと動かせる。
    Frame,
    /// スタンプ(色付きの丸に白い記号)。`text` が記号。
    Stamp,
    /// 画像。中身は `Shape::image`(PNG / JPEG のバイト列)。
    Image,
    // ---- 図形ライブラリで増やした種類 ----
    /// 角丸四角形(角の丸みは大きさに合わせる)。
    RoundRect,
    /// 両端が半円のカプセル形(フローチャートの端子)。
    Pill,
    TriangleDown,
    RightTriangle,
    ParallelogramLeft,
    Trapezoid,
    Pentagon,
    Octagon,
    Star4,
    Cross,
    Heart,
    Cloud,
    Lightning,
    /// 吹き出し(左下にしっぽ)。
    Speech,
    ArrowRight,
    ArrowLeft,
    ArrowUp,
    ArrowDown,
    ArrowBoth,
    Chevron,
    /// 右がとがった五角形(工程・ホームベース)。
    HomePlate,
    /// 定義済み処理(左右に縦線)。
    PredefinedProcess,
    /// 書類(下辺が波)。
    Document,
    /// 円柱(データベース)。
    Cylinder,
    /// 手操作入力(上辺が斜め)。
    ManualInput,
    /// 手作業(上が広い台形)。
    ManualOperation,
    /// 遅延(右が丸い D 形)。
    Delay,
    /// 他ページ結合子(下がとがる)。
    OffPage,
    /// 内部記憶(左と上に線)。
    InternalStorage,
    /// カード(左上の角が欠ける)。
    Card,
}

/// 図形ライブラリの分類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeCategory {
    Basic,
    Arrow,
    Flowchart,
}

impl ShapeCategory {
    pub const ALL: [ShapeCategory; 3] = [
        ShapeCategory::Basic,
        ShapeCategory::Arrow,
        ShapeCategory::Flowchart,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ShapeCategory::Basic => "基本図形",
            ShapeCategory::Arrow => "矢印",
            ShapeCategory::Flowchart => "フローチャート",
        }
    }
}

/// (種類, 保存名, 表示名)。保存名は同期・保存に使うので変えない。
const KINDS: &[(ShapeKind, &str, &str)] = &[
    (ShapeKind::Rect, "rect", "四角形"),
    (ShapeKind::Ellipse, "ellipse", "楕円"),
    (ShapeKind::Sticky, "sticky", "付箋"),
    (ShapeKind::Triangle, "triangle", "三角形"),
    (ShapeKind::Diamond, "diamond", "ひし形"),
    (ShapeKind::Star, "star", "星"),
    (ShapeKind::Hexagon, "hexagon", "六角形"),
    (ShapeKind::Parallelogram, "parallelogram", "平行四辺形"),
    (ShapeKind::Text, "text", "テキスト"),
    (ShapeKind::Frame, "frame", "フレーム"),
    (ShapeKind::Stamp, "stamp", "スタンプ"),
    (ShapeKind::Image, "image", "画像"),
    (ShapeKind::RoundRect, "round-rect", "角丸四角形"),
    (ShapeKind::Pill, "pill", "カプセル(端子)"),
    (ShapeKind::TriangleDown, "triangle-down", "逆三角形"),
    (ShapeKind::RightTriangle, "right-triangle", "直角三角形"),
    (
        ShapeKind::ParallelogramLeft,
        "parallelogram-left",
        "平行四辺形(左)",
    ),
    (ShapeKind::Trapezoid, "trapezoid", "台形"),
    (ShapeKind::Pentagon, "pentagon", "五角形"),
    (ShapeKind::Octagon, "octagon", "八角形"),
    (ShapeKind::Star4, "star4", "四芒星"),
    (ShapeKind::Cross, "cross", "十字"),
    (ShapeKind::Heart, "heart", "ハート"),
    (ShapeKind::Cloud, "cloud", "雲"),
    (ShapeKind::Lightning, "lightning", "稲妻"),
    (ShapeKind::Speech, "speech", "吹き出し"),
    (ShapeKind::ArrowRight, "arrow-right", "右矢印"),
    (ShapeKind::ArrowLeft, "arrow-left", "左矢印"),
    (ShapeKind::ArrowUp, "arrow-up", "上矢印"),
    (ShapeKind::ArrowDown, "arrow-down", "下矢印"),
    (ShapeKind::ArrowBoth, "arrow-both", "左右矢印"),
    (ShapeKind::Chevron, "chevron", "シェブロン"),
    (ShapeKind::HomePlate, "home-plate", "工程(ホームベース)"),
    (
        ShapeKind::PredefinedProcess,
        "predefined-process",
        "定義済み処理",
    ),
    (ShapeKind::Document, "document", "書類"),
    (ShapeKind::Cylinder, "cylinder", "データベース(円柱)"),
    (ShapeKind::ManualInput, "manual-input", "手操作入力"),
    (ShapeKind::ManualOperation, "manual-operation", "手作業"),
    (ShapeKind::Delay, "delay", "遅延"),
    (ShapeKind::OffPage, "off-page", "他ページ結合子"),
    (ShapeKind::InternalStorage, "internal-storage", "内部記憶"),
    (ShapeKind::Card, "card", "カード"),
];

impl ShapeKind {
    /// 図形ライブラリに並べる種類(文字・付箋・フレーム・スタンプ・画像以外)。
    pub const LIBRARY: [ShapeKind; 37] = [
        // 基本図形
        ShapeKind::Rect,
        ShapeKind::RoundRect,
        ShapeKind::Ellipse,
        ShapeKind::Triangle,
        ShapeKind::TriangleDown,
        ShapeKind::RightTriangle,
        ShapeKind::Diamond,
        ShapeKind::Parallelogram,
        ShapeKind::ParallelogramLeft,
        ShapeKind::Trapezoid,
        ShapeKind::Pentagon,
        ShapeKind::Hexagon,
        ShapeKind::Octagon,
        ShapeKind::Star,
        ShapeKind::Star4,
        ShapeKind::Cross,
        ShapeKind::Heart,
        ShapeKind::Cloud,
        ShapeKind::Lightning,
        ShapeKind::Speech,
        ShapeKind::Pill,
        // 矢印
        ShapeKind::ArrowRight,
        ShapeKind::ArrowLeft,
        ShapeKind::ArrowUp,
        ShapeKind::ArrowDown,
        ShapeKind::ArrowBoth,
        ShapeKind::Chevron,
        ShapeKind::HomePlate,
        // フローチャート
        ShapeKind::PredefinedProcess,
        ShapeKind::Document,
        ShapeKind::Cylinder,
        ShapeKind::ManualInput,
        ShapeKind::ManualOperation,
        ShapeKind::Delay,
        ShapeKind::OffPage,
        ShapeKind::InternalStorage,
        ShapeKind::Card,
    ];

    /// 保存・同期用の名前。
    pub fn name(self) -> &'static str {
        KINDS.iter().find(|k| k.0 == self).map_or("rect", |k| k.1)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        KINDS.iter().find(|k| k.1 == name).map(|k| k.0)
    }

    /// 画面に出す名前。
    pub fn label(self) -> &'static str {
        KINDS.iter().find(|k| k.0 == self).map_or("", |k| k.2)
    }

    /// クリックだけで置いたときの大きさ。
    pub fn default_size(self) -> Vec2 {
        use ShapeKind::*;
        match self {
            Frame => Vec2::new(800.0, 600.0),
            Text => Vec2::new(240.0, 40.0),
            Triangle | TriangleDown | RightTriangle | Star | Star4 | Heart | Pentagon | Octagon
            | Cross | Lightning => Vec2::splat(140.0),
            Cylinder => Vec2::new(130.0, 150.0),
            ArrowUp | ArrowDown => Vec2::new(100.0, 160.0),
            ArrowRight | ArrowLeft | ArrowBoth | Chevron | HomePlate => Vec2::new(180.0, 100.0),
            Pill => Vec2::new(180.0, 80.0),
            Speech => Vec2::new(180.0, 130.0),
            Cloud => Vec2::new(190.0, 130.0),
            _ => Vec2::new(160.0, 100.0),
        }
    }

    /// 図形ライブラリでの分類。
    pub fn category(self) -> ShapeCategory {
        use ShapeKind::*;
        match self {
            ArrowRight | ArrowLeft | ArrowUp | ArrowDown | ArrowBoth | Chevron | HomePlate => {
                ShapeCategory::Arrow
            }
            PredefinedProcess | Document | Cylinder | ManualInput | ManualOperation | Delay
            | OffPage | InternalStorage | Card => ShapeCategory::Flowchart,
            _ => ShapeCategory::Basic,
        }
    }

    /// 多角形の図形なら頂点(外接矩形を -1..1 にした座標、y 下向き)。
    ///
    /// 当たり判定と描画(シェーダーはこの表から生成する)の両方がこれに従う。
    pub fn polygon(self) -> Option<&'static [[f32; 2]]> {
        POLYGONS.iter().find(|p| p.0 == self).map(|p| p.1)
    }
}

/// 多角形の図形の頂点。描画側はこの並び順の番号でシェーダーの種類を決める。
pub const POLYGONS: &[(ShapeKind, &[[f32; 2]])] = &[
    (
        ShapeKind::TriangleDown,
        &[[-1.0, -1.0], [1.0, -1.0], [0.0, 1.0]],
    ),
    (
        ShapeKind::RightTriangle,
        &[[-1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]],
    ),
    (
        ShapeKind::ParallelogramLeft,
        &[[-1.0, -1.0], [0.5, -1.0], [1.0, 1.0], [-0.5, 1.0]],
    ),
    (
        ShapeKind::Trapezoid,
        &[[-0.5, -1.0], [0.5, -1.0], [1.0, 1.0], [-1.0, 1.0]],
    ),
    (
        ShapeKind::Pentagon,
        &[
            [0.0, -1.0],
            [1.0, -0.236],
            [0.618, 1.0],
            [-0.618, 1.0],
            [-1.0, -0.236],
        ],
    ),
    (
        ShapeKind::Octagon,
        &[
            [-0.4142, -1.0],
            [0.4142, -1.0],
            [1.0, -0.4142],
            [1.0, 0.4142],
            [0.4142, 1.0],
            [-0.4142, 1.0],
            [-1.0, 0.4142],
            [-1.0, -0.4142],
        ],
    ),
    (
        ShapeKind::Star4,
        &[
            [0.0, -1.0],
            [0.22, -0.22],
            [1.0, 0.0],
            [0.22, 0.22],
            [0.0, 1.0],
            [-0.22, 0.22],
            [-1.0, 0.0],
            [-0.22, -0.22],
        ],
    ),
    (
        ShapeKind::Cross,
        &[
            [-0.33, -1.0],
            [0.33, -1.0],
            [0.33, -0.33],
            [1.0, -0.33],
            [1.0, 0.33],
            [0.33, 0.33],
            [0.33, 1.0],
            [-0.33, 1.0],
            [-0.33, 0.33],
            [-1.0, 0.33],
            [-1.0, -0.33],
            [-0.33, -0.33],
        ],
    ),
    (
        ShapeKind::Lightning,
        &[
            [-0.1, -1.0],
            [0.6, -1.0],
            [0.15, -0.25],
            [0.75, -0.25],
            [-0.45, 1.0],
            [-0.1, 0.1],
            [-0.65, 0.1],
        ],
    ),
    (
        ShapeKind::ArrowRight,
        &[
            [-1.0, -0.4],
            [0.2, -0.4],
            [0.2, -1.0],
            [1.0, 0.0],
            [0.2, 1.0],
            [0.2, 0.4],
            [-1.0, 0.4],
        ],
    ),
    (
        ShapeKind::ArrowLeft,
        &[
            [1.0, -0.4],
            [1.0, 0.4],
            [-0.2, 0.4],
            [-0.2, 1.0],
            [-1.0, 0.0],
            [-0.2, -1.0],
            [-0.2, -0.4],
        ],
    ),
    (
        ShapeKind::ArrowUp,
        &[
            [-0.4, 1.0],
            [-0.4, -0.2],
            [-1.0, -0.2],
            [0.0, -1.0],
            [1.0, -0.2],
            [0.4, -0.2],
            [0.4, 1.0],
        ],
    ),
    (
        ShapeKind::ArrowDown,
        &[
            [-0.4, -1.0],
            [0.4, -1.0],
            [0.4, 0.2],
            [1.0, 0.2],
            [0.0, 1.0],
            [-1.0, 0.2],
            [-0.4, 0.2],
        ],
    ),
    (
        ShapeKind::ArrowBoth,
        &[
            [-1.0, 0.0],
            [-0.5, -1.0],
            [-0.5, -0.4],
            [0.5, -0.4],
            [0.5, -1.0],
            [1.0, 0.0],
            [0.5, 1.0],
            [0.5, 0.4],
            [-0.5, 0.4],
            [-0.5, 1.0],
        ],
    ),
    (
        ShapeKind::Chevron,
        &[
            [-1.0, -1.0],
            [0.5, -1.0],
            [1.0, 0.0],
            [0.5, 1.0],
            [-1.0, 1.0],
            [-0.5, 0.0],
        ],
    ),
    (
        ShapeKind::HomePlate,
        &[
            [-1.0, -1.0],
            [0.5, -1.0],
            [1.0, 0.0],
            [0.5, 1.0],
            [-1.0, 1.0],
        ],
    ),
    (
        ShapeKind::ManualInput,
        &[[-1.0, -0.5], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]],
    ),
    (
        ShapeKind::ManualOperation,
        &[[-1.0, -1.0], [1.0, -1.0], [0.6, 1.0], [-0.6, 1.0]],
    ),
    (
        ShapeKind::OffPage,
        &[
            [-1.0, -1.0],
            [1.0, -1.0],
            [1.0, 0.4],
            [0.0, 1.0],
            [-1.0, 0.4],
        ],
    ),
    (
        ShapeKind::Card,
        &[
            [-0.6, -1.0],
            [1.0, -1.0],
            [1.0, 1.0],
            [-1.0, 1.0],
            [-1.0, -0.6],
        ],
    ),
];

/// 雲の形を作る楕円(中心と半径、外接矩形を -1..1 にした座標)。シェーダーと同じ値。
pub const CLOUD_LOBES: [([f32; 2], [f32; 2]); 7] = [
    ([-0.5, 0.35], [0.5, 0.62]),
    ([0.5, 0.35], [0.5, 0.62]),
    ([-0.3, -0.3], [0.5, 0.7]),
    ([0.3, -0.3], [0.5, 0.7]),
    ([0.0, 0.25], [0.55, 0.7]),
    ([-0.72, 0.05], [0.28, 0.45]),
    ([0.72, 0.05], [0.28, 0.45]),
];

/// 吹き出しの本体の下端(-1..1 の座標)。
pub const SPEECH_BODY: f32 = 0.6;
/// 吹き出しのしっぽ(-1..1 の座標)。
pub const SPEECH_TAIL: [[f32; 2]; 3] = [[-0.55, 0.5], [-0.1, 0.5], [-0.6, 1.0]];

/// ハートの内側か(-1..1 の座標)。Inigo Quilez のハートの距離関数(MIT)を外接矩形に合わせたもの。
fn heart_contains(d: Vec2) -> bool {
    let p = Vec2::new((d.x * 0.6036).abs(), (1.0 - d.y) * 0.5518);
    if p.x + p.y > 1.0 {
        return (p - Vec2::new(0.25, 0.75)).length() <= std::f32::consts::SQRT_2 / 4.0;
    }
    // 下の二辺の内側。
    p.x <= p.y
}

/// 点 `p`(-1..1 の座標)が多角形の内側か(偶奇規則)。
fn polygon_contains(v: &[[f32; 2]], p: Vec2) -> bool {
    let mut inside = false;
    let mut j = v.len() - 1;
    for i in 0..v.len() {
        let (a, b) = (Vec2::from(v[i]), Vec2::from(v[j]));
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// 文字の横方向の揃え。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

impl TextAlign {
    pub fn name(self) -> &'static str {
        match self {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "left" => TextAlign::Left,
            "right" => TextAlign::Right,
            _ => TextAlign::Center,
        }
    }
}

/// 文字の縦方向の揃え。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VAlign {
    Top,
    #[default]
    Middle,
    Bottom,
}

impl VAlign {
    pub const ALL: [VAlign; 3] = [VAlign::Top, VAlign::Middle, VAlign::Bottom];

    pub fn name(self) -> &'static str {
        match self {
            VAlign::Top => "top",
            VAlign::Middle => "middle",
            VAlign::Bottom => "bottom",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "top" => VAlign::Top,
            "bottom" => VAlign::Bottom,
            _ => VAlign::Middle,
        }
    }

    /// 高さ `avail` の枠に高さ `content` の中身を置くときの上の余白(はみ出すときは上詰め)。
    pub fn offset(self, avail: f32, content: f32) -> f32 {
        let free = (avail - content).max(0.0);
        match self {
            VAlign::Top => 0.0,
            VAlign::Middle => free * 0.5,
            VAlign::Bottom => free,
        }
    }
}

/// キャンバス上の 1 要素。座標はワールド座標(px 相当、y 下向き)。
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub id: ElementId,
    pub kind: ShapeKind,
    /// 左上の位置。
    pub pos: Vec2,
    pub size: Vec2,
    pub fill: Rgba8,
    pub stroke: Rgba8,
    pub stroke_width: f32,
    pub corner_radius: f32,
    /// 図形の中に表示する文字(付箋の本文など)。
    pub text: String,
    /// 文字の大きさ(ワールド単位)。0 なら種類ごとの既定値。
    pub font_size: f32,
    pub align: TextAlign,
    /// 文字の縦方向の揃え。
    pub valign: VAlign,
    /// 縦書き(右の列から左へ)。
    pub vertical: bool,
    pub text_color: Rgba8,
    pub meta: Meta,
    /// 画像のバイト列(`ShapeKind::Image` のときだけ)。
    pub image: Option<ImageData>,
    /// 図形の中に描くアイコンの名前(構成図の部品の種類。例: `ec2`、`group:vpc`)。
    /// 空ならアイコンなし。アイコンの画像は描画側が名前から探し、なければ描かない。
    pub icon: String,
}

/// 画像のバイト列(PNG / JPEG)。複製しても中身は共有する。
#[derive(Clone, PartialEq, Eq)]
pub struct ImageData(pub std::sync::Arc<[u8]>);

impl ImageData {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// 同じ中身かを素早く見分けるための値(描画のキャッシュ用)。
    pub fn key(&self) -> (usize, usize) {
        (self.0.as_ptr() as usize, self.0.len())
    }
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageData({} bytes)", self.0.len())
    }
}

/// 図形内の文字の書式(ワールド単位)。描画と編集用テキスト欄の両方がこれに従う。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub font_size: f32,
    pub line_height: f32,
    pub color: Rgba8,
    pub align: TextAlign,
}

/// 文字を置く場所と書式。描画と編集用の入力欄の両方がこれに従う。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextBox {
    pub rect: Rect,
    /// はみ出した文字を切り取る範囲。
    pub clip: Rect,
    pub style: TextStyle,
    /// 縦方向の揃え。
    pub valign: VAlign,
    /// 縦書き。
    pub vertical: bool,
}

/// 文字の幅の概算(全角 1em、半角 0.55em)。
pub fn estimate_text_width(text: &str, font_size: f32) -> f32 {
    text.chars()
        .map(|c| if c.is_ascii() { 0.55 } else { 1.0 })
        .sum::<f32>()
        * font_size
}

/// 幅 `width` で折り返したときの高さの概算。
pub fn estimate_text_height(text: &str, width: f32, style: &TextStyle) -> f32 {
    let lines: f32 = text
        .split('\n')
        .map(|line| {
            let w = estimate_text_width(line, style.font_size);
            (w / width.max(1.0)).ceil().max(1.0)
        })
        .sum();
    lines * style.line_height
}

/// フレームの見出しの高さ(ワールド単位)。
pub const FRAME_TITLE_HEIGHT: f32 = 32.0;
/// 図形の中のアイコンの一辺の上限。
pub const ICON_SIZE: f32 = 48.0;
/// フレームの左上に置くアイコンの一辺。
pub const FRAME_ICON_SIZE: f32 = 32.0;
const ICON_PAD: f32 = 14.0;
const ICON_GAP: f32 = 8.0;

impl Shape {
    /// 画像の図形を作る。
    pub fn image(id: ElementId, pos: Vec2, size: Vec2, data: ImageData) -> Self {
        Self {
            kind: ShapeKind::Image,
            stroke: Rgba8::TRANSPARENT,
            stroke_width: 0.0,
            corner_radius: 0.0,
            image: Some(data),
            ..Self::rect(id, pos, size, Rgba8::TRANSPARENT)
        }
    }

    /// 種類に合わせた既定の見た目で作る。
    pub fn new(id: ElementId, kind: ShapeKind, pos: Vec2, size: Vec2, fill: Rgba8) -> Self {
        let base = Self::rect(id, pos, size, fill);
        match kind {
            ShapeKind::Rect => base,
            ShapeKind::Ellipse => Self::ellipse(id, pos, size, fill),
            ShapeKind::Sticky => Self {
                size,
                ..Self::sticky(id, pos, fill)
            },
            ShapeKind::Text => Self::text(id, pos, size),
            ShapeKind::Frame => Self::frame(id, pos, size),
            _ => Self {
                kind,
                corner_radius: 0.0,
                ..base
            },
        }
    }

    pub fn rect(id: ElementId, pos: Vec2, size: Vec2, fill: Rgba8) -> Self {
        Self {
            id,
            kind: ShapeKind::Rect,
            pos,
            size,
            fill,
            stroke: Rgba8::BLACK,
            stroke_width: 2.0,
            corner_radius: 8.0,
            text: String::new(),
            font_size: 0.0,
            align: TextAlign::Center,
            valign: VAlign::Middle,
            vertical: false,
            text_color: Rgba8::BLACK,
            meta: Meta::default(),
            image: None,
            icon: String::new(),
        }
    }

    /// 枠なしの文字。
    pub fn text(id: ElementId, pos: Vec2, size: Vec2) -> Self {
        Self {
            kind: ShapeKind::Text,
            fill: Rgba8::TRANSPARENT,
            stroke: Rgba8::TRANSPARENT,
            stroke_width: 0.0,
            corner_radius: 0.0,
            align: TextAlign::Left,
            valign: VAlign::Top,
            ..Self::rect(id, pos, size, Rgba8::TRANSPARENT)
        }
    }

    /// フレーム(セクション)。
    pub fn frame(id: ElementId, pos: Vec2, size: Vec2) -> Self {
        Self {
            kind: ShapeKind::Frame,
            stroke: Rgba8::hex(0xd0d0cc),
            stroke_width: 1.0,
            corner_radius: 6.0,
            align: TextAlign::Left,
            valign: VAlign::Top,
            text: "フレーム".into(),
            ..Self::rect(id, pos, size, Rgba8::WHITE)
        }
    }

    pub fn ellipse(id: ElementId, pos: Vec2, size: Vec2, fill: Rgba8) -> Self {
        Self {
            kind: ShapeKind::Ellipse,
            corner_radius: 0.0,
            ..Self::rect(id, pos, size, fill)
        }
    }

    pub fn sticky(id: ElementId, pos: Vec2, fill: Rgba8) -> Self {
        Self {
            id,
            kind: ShapeKind::Sticky,
            pos,
            size: Vec2::splat(200.0),
            fill,
            stroke: Rgba8::TRANSPARENT,
            stroke_width: 0.0,
            corner_radius: 4.0,
            text: String::new(),
            font_size: 0.0,
            align: TextAlign::Center,
            valign: VAlign::Middle,
            vertical: false,
            text_color: Rgba8::BLACK,
            meta: Meta::default(),
            image: None,
            icon: String::new(),
        }
    }

    pub fn min(&self) -> Vec2 {
        self.pos
    }

    pub fn max(&self) -> Vec2 {
        self.pos + self.size
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(self.min(), self.max())
    }

    pub fn set_bounds(&mut self, r: Rect) {
        self.pos = r.min;
        self.size = r.size();
    }

    /// 種類ごとの既定の文字の大きさ。
    pub fn default_font_size(kind: ShapeKind) -> f32 {
        match kind {
            ShapeKind::Sticky => 24.0,
            ShapeKind::Frame => 16.0,
            ShapeKind::Text => 24.0,
            _ => 20.0,
        }
    }

    pub fn text_style(&self) -> TextStyle {
        let font_size = if self.kind == ShapeKind::Stamp {
            // スタンプの記号は丸の大きさに合わせる。
            (self.size.y * 0.46).max(1.0)
        } else if self.font_size > 0.0 {
            self.font_size
        } else {
            Self::default_font_size(self.kind)
        };
        TextStyle {
            font_size,
            line_height: font_size * 1.4,
            color: self.text_color,
            align: self.align,
        }
    }

    /// 文字を置く矩形。楕円などは内側に収まる矩形にする。フレームは上に出る見出し。
    pub fn text_rect(&self) -> Rect {
        let b = self.bounds();
        if self.kind == ShapeKind::Frame {
            return Rect::new(
                Vec2::new(b.min.x + 4.0, b.min.y - FRAME_TITLE_HEIGHT),
                Vec2::new(b.max.x, b.min.y - 4.0),
            );
        }
        let pad = match self.kind {
            ShapeKind::Sticky => 16.0,
            ShapeKind::Text => 4.0,
            ShapeKind::Stamp => 0.0,
            _ => 12.0,
        };
        // 図形の内側に入る矩形の大きさ(外接矩形に対する比率)と中心のずれ。
        use ShapeKind as K;
        let (fx, fy, dx, dy) = match self.kind {
            K::Ellipse => (
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                0.0,
            ),
            K::Diamond => (0.5, 0.5, 0.0, 0.0),
            K::Triangle => (0.5, 0.5, 0.0, 0.25),
            K::TriangleDown => (0.5, 0.5, 0.0, -0.25),
            K::RightTriangle => (0.5, 0.5, -0.25, 0.25),
            K::Star => (0.45, 0.4, 0.0, 0.05),
            K::Star4 => (0.35, 0.35, 0.0, 0.0),
            K::Hexagon => (0.75, 1.0, 0.0, 0.0),
            K::Parallelogram | K::ParallelogramLeft | K::Trapezoid => (0.7, 1.0, 0.0, 0.0),
            K::ManualOperation => (0.6, 1.0, 0.0, 0.0),
            K::Pill => (0.85, 1.0, 0.0, 0.0),
            K::Pentagon => (0.65, 0.6, 0.0, 0.1),
            K::Octagon => (0.85, 0.85, 0.0, 0.0),
            K::Cross => (1.0, 0.33, 0.0, 0.0),
            K::Heart => (0.6, 0.45, 0.0, -0.15),
            K::Cloud => (0.7, 0.6, 0.0, 0.05),
            K::Lightning => (0.35, 0.35, 0.0, 0.0),
            K::Speech => (1.0, 0.8, 0.0, -0.2),
            K::ArrowRight => (0.6, 0.4, -0.2, 0.0),
            K::ArrowLeft => (0.6, 0.4, 0.2, 0.0),
            K::ArrowUp => (0.4, 0.6, 0.0, 0.2),
            K::ArrowDown => (0.4, 0.6, 0.0, -0.2),
            K::ArrowBoth => (0.5, 0.4, 0.0, 0.0),
            K::Chevron => (0.5, 1.0, 0.0, 0.0),
            K::HomePlate => (0.75, 1.0, -0.25, 0.0),
            K::PredefinedProcess => (0.75, 1.0, 0.0, 0.0),
            K::Document => (1.0, 0.8, 0.0, -0.15),
            K::Cylinder => (1.0, 0.6, 0.0, 0.1),
            K::ManualInput => (1.0, 0.7, 0.0, 0.25),
            K::Delay => (0.8, 1.0, -0.1, 0.0),
            K::OffPage => (1.0, 0.7, 0.0, -0.2),
            K::InternalStorage => (0.85, 0.85, 0.15, 0.15),
            K::Card => (1.0, 0.8, 0.0, 0.1),
            _ => (1.0, 1.0, 0.0, 0.0),
        };
        let half = b.size() * 0.5 * Vec2::new(fx, fy);
        let half = (half - glam::Vec2::splat(pad)).max(glam::Vec2::ZERO);
        let center = b.center() + b.size() * Vec2::new(dx, dy) * 0.5;
        let mut r = Rect::new(center - half, center + half);
        // アイコンの右側に文字を置く。
        if let Some(icon) = self.icon_rect() {
            r.min.x = (icon.max.x + ICON_GAP).min(r.max.x);
        }
        r
    }

    /// アイコンを描く矩形(ワールド座標)。アイコンがなければ `None`。
    ///
    /// フレームは左上の角に、それ以外は左端に縦中央で置く。
    pub fn icon_rect(&self) -> Option<Rect> {
        if self.icon.is_empty() || self.kind == ShapeKind::Image {
            return None;
        }
        let b = self.bounds();
        if self.kind == ShapeKind::Frame {
            let side = FRAME_ICON_SIZE.min(b.size().x).min(b.size().y);
            return Some(Rect::new(b.min, b.min + Vec2::splat(side)));
        }
        let side = ICON_SIZE
            .min(b.size().y - 2.0 * ICON_PAD)
            .min(b.size().x * 0.4)
            .max(0.0);
        let min = Vec2::new(b.min.x + ICON_PAD, b.center().y - side * 0.5);
        Some(Rect::new(min, min + Vec2::splat(side)))
    }

    /// 文字の表示を切り取る範囲。
    pub fn text_clip(&self) -> Rect {
        match self.kind {
            ShapeKind::Frame => self.text_rect(),
            _ => self.bounds(),
        }
    }

    pub fn text_box(&self) -> TextBox {
        TextBox {
            rect: self.text_rect(),
            clip: self.text_clip(),
            style: self.text_style(),
            // フレームの見出しは 1 行の横書きのまま。
            valign: if self.kind == ShapeKind::Frame {
                VAlign::Top
            } else {
                self.valign
            },
            vertical: self.vertical && self.kind != ShapeKind::Frame,
        }
    }

    /// 文字だけの要素の高さを中身に合わせる。
    pub fn fit_text_height(&mut self) {
        // 縦書きは幅が中身で決まらないので、大きさはそのまま。
        if self.kind != ShapeKind::Text || self.vertical {
            return;
        }
        let style = self.text_style();
        let width = self.text_rect().size().x;
        let h = estimate_text_height(&self.text, width, &style) + 8.0;
        self.size.y = h.max(style.line_height + 8.0);
    }

    /// ワールド座標の点が要素の内側か。
    pub fn contains(&self, p: Vec2) -> bool {
        kind_contains(self.kind, &self.bounds(), p)
    }

    /// フレームの見出し部分か(フレームは見出しを掴んで動かす)。
    pub fn frame_title_contains(&self, p: Vec2) -> bool {
        self.kind == ShapeKind::Frame && self.text_rect().expand(4.0).contains(p)
    }
}

/// 種類 `kind` の図形を外接矩形 `r` に置いたとき、点 `p` が内側か。
pub fn kind_contains(kind: ShapeKind, r: &Rect, p: Vec2) -> bool {
    let half = r.size() * 0.5;
    if !r.contains(p) || half.x <= 0.0 || half.y <= 0.0 {
        return false;
    }
    let d = (p - r.center()) / half; // -1..1
    if let Some(v) = kind.polygon() {
        return polygon_contains(v, d);
    }
    let in_ellipse =
        |c: [f32; 2], r: [f32; 2]| ((d - Vec2::from(c)) / Vec2::from(r)).length_squared() <= 1.0;
    match kind {
        ShapeKind::Ellipse | ShapeKind::Stamp => d.length_squared() <= 1.0,
        ShapeKind::Diamond => d.x.abs() + d.y.abs() <= 1.0,
        // 上の頂点と下の 2 頂点。
        ShapeKind::Triangle => d.x.abs() <= (d.y + 1.0) * 0.5,
        // 上下が平らで、左右が頂点(上辺の幅は全体の半分)。
        ShapeKind::Hexagon => d.x.abs() <= 1.0 - 0.5 * d.y.abs(),
        // 上辺が右へ 1/4 ずれる。
        ShapeKind::Parallelogram => (d.x + 0.25 * d.y).abs() <= 0.75,
        ShapeKind::Heart => heart_contains(d),
        ShapeKind::Cloud => CLOUD_LOBES.iter().any(|&(c, r)| in_ellipse(c, r)),
        // 本体(上 8 割)と左下のしっぽ。
        ShapeKind::Speech => d.y <= SPEECH_BODY || polygon_contains(&SPEECH_TAIL, d),
        ShapeKind::Delay => d.x <= 0.0 || d.length_squared() <= 1.0,
        ShapeKind::Pill => {
            // 両端は半径が高さ(または幅)の半分の半円。
            let rad = half.min_element();
            let q = ((p - r.center()).abs() - (half - rad)).max(Vec2::ZERO);
            q.length() <= rad
        }
        _ => true,
    }
}

/// 外接矩形 `r` の中心から矩形上の点 `p` へ向かう線が、図形の輪郭と交わる点。
pub fn outline_point(kind: ShapeKind, r: &Rect, p: Vec2) -> Vec2 {
    let c = r.center();
    let inside = |q: Vec2| kind_contains(kind, &r.expand(0.01), q);
    if inside(p) {
        return p;
    }
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..20 {
        let mid = (lo + hi) * 0.5;
        if inside(c + (p - c) * mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    c + (p - c) * lo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_kinds_round_trip_and_hit_test() {
        let r = Rect::new(Vec2::ZERO, Vec2::new(200.0, 120.0));
        for k in ShapeKind::LIBRARY {
            assert_eq!(ShapeKind::from_name(k.name()), Some(k), "{k:?}");
            assert!(!k.label().is_empty(), "{k:?}");
            // 文字の置き場所の中心は図形の内側。
            let s = Shape::new(ElementId(1), k, r.min, r.size(), Rgba8::WHITE);
            let t = s.text_rect();
            assert!(s.contains(t.center()), "{k:?} {t:?}");
            assert!(r.contains_rect(&t), "{k:?}");
            // 外接矩形で判定する種類(四角に近い・掴みやすさ優先)は除く。
            let boxy = matches!(
                k,
                ShapeKind::Rect
                    | ShapeKind::RoundRect
                    | ShapeKind::Star
                    | ShapeKind::PredefinedProcess
                    | ShapeKind::Document
                    | ShapeKind::Cylinder
                    | ShapeKind::InternalStorage
            );
            if !boxy {
                // 角が欠けている図形は、少なくとも 1 つの角が外。
                let corners = [
                    r.min,
                    r.max,
                    Vec2::new(r.min.x, r.max.y),
                    Vec2::new(r.max.x, r.min.y),
                ];
                let near = |c: Vec2| c + (r.center() - c) * 0.02;
                assert!(corners.iter().any(|&c| !s.contains(near(c))), "{k:?}");
            }
        }
        // 多角形は頂点数 12 まで(シェーダーの配列の大きさ)。
        assert!(POLYGONS.iter().all(|p| (3..=12).contains(&p.1.len())));
    }

    #[test]
    fn outline_points_lie_on_shapes() {
        let r = Rect::new(Vec2::ZERO, Vec2::new(200.0, 100.0));
        // ひし形: 右上の角へ向かう線は辺の中ほどで止まる。
        let q = outline_point(ShapeKind::Diamond, &r, Vec2::new(200.0, 0.0));
        assert!((q - Vec2::new(150.0, 25.0)).length() < 0.1, "{q}");
        // 頂点はそのまま。
        let q = outline_point(ShapeKind::Diamond, &r, Vec2::new(200.0, 50.0));
        assert!((q - Vec2::new(200.0, 50.0)).length() < 0.1, "{q}");
        let q = outline_point(ShapeKind::Rect, &r, Vec2::new(200.0, 0.0));
        assert_eq!(q, Vec2::new(200.0, 0.0));
        let q = outline_point(ShapeKind::Ellipse, &r, Vec2::new(200.0, 0.0));
        let d = (q - r.center()) / Vec2::new(100.0, 50.0);
        assert!((d.length() - 1.0).abs() < 0.01);
    }

    #[test]
    fn contains_rect_and_ellipse() {
        let r = Shape::rect(
            ElementId(1),
            Vec2::ZERO,
            Vec2::new(100.0, 50.0),
            Rgba8::WHITE,
        );
        assert!(r.contains(Vec2::new(99.0, 49.0)));
        assert!(!r.contains(Vec2::new(101.0, 10.0)));

        let e = Shape::ellipse(
            ElementId(2),
            Vec2::ZERO,
            Vec2::new(100.0, 50.0),
            Rgba8::WHITE,
        );
        assert!(e.contains(Vec2::new(50.0, 25.0)));
        // 外接矩形の角は楕円の外。
        assert!(!e.contains(Vec2::new(2.0, 2.0)));
    }

    #[test]
    fn icon_sits_left_of_text() {
        let mut s = Shape::rect(
            ElementId(1),
            Vec2::ZERO,
            Vec2::new(200.0, 90.0),
            Rgba8::WHITE,
        );
        assert!(s.icon_rect().is_none());
        let plain = s.text_rect();
        s.icon = "ec2".into();
        let icon = s.icon_rect().unwrap();
        assert_eq!(icon.size(), Vec2::splat(ICON_SIZE));
        assert!(s.bounds().contains_rect(&icon));
        assert!(s.text_rect().min.x >= icon.max.x);
        assert_eq!(s.text_rect().max, plain.max);
        let mut f = Shape::frame(ElementId(2), Vec2::ZERO, Vec2::splat(300.0));
        f.icon = "group:vpc".into();
        assert_eq!(f.icon_rect().unwrap().min, Vec2::ZERO);
        // フレームの見出しは枠の上なので動かない。
        assert_eq!(
            f.text_rect(),
            Shape::frame(ElementId(2), Vec2::ZERO, Vec2::splat(300.0)).text_rect()
        );
    }

    #[test]
    fn text_rect_inside_shape() {
        let s = Shape::sticky(ElementId(1), Vec2::ZERO, Rgba8::WHITE);
        assert_eq!(
            s.text_rect(),
            Rect::new(Vec2::splat(16.0), Vec2::splat(184.0))
        );
        let e = Shape::ellipse(ElementId(2), Vec2::ZERO, Vec2::splat(200.0), Rgba8::WHITE);
        let r = e.text_rect();
        // 内接矩形の角は楕円の内側。
        assert!(e.contains(r.min) && e.contains(r.max));
    }
}
