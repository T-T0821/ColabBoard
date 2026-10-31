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
}

impl ShapeKind {
    /// 図形ライブラリに並べる種類(文字・付箋・フレーム以外)。
    pub const LIBRARY: [ShapeKind; 7] = [
        ShapeKind::Rect,
        ShapeKind::Ellipse,
        ShapeKind::Triangle,
        ShapeKind::Diamond,
        ShapeKind::Star,
        ShapeKind::Hexagon,
        ShapeKind::Parallelogram,
    ];

    /// 保存・同期用の名前。
    pub fn name(self) -> &'static str {
        match self {
            ShapeKind::Rect => "rect",
            ShapeKind::Ellipse => "ellipse",
            ShapeKind::Sticky => "sticky",
            ShapeKind::Triangle => "triangle",
            ShapeKind::Diamond => "diamond",
            ShapeKind::Star => "star",
            ShapeKind::Hexagon => "hexagon",
            ShapeKind::Parallelogram => "parallelogram",
            ShapeKind::Text => "text",
            ShapeKind::Frame => "frame",
            ShapeKind::Stamp => "stamp",
            ShapeKind::Image => "image",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "rect" => ShapeKind::Rect,
            "ellipse" => ShapeKind::Ellipse,
            "sticky" => ShapeKind::Sticky,
            "triangle" => ShapeKind::Triangle,
            "diamond" => ShapeKind::Diamond,
            "star" => ShapeKind::Star,
            "hexagon" => ShapeKind::Hexagon,
            "parallelogram" => ShapeKind::Parallelogram,
            "text" => ShapeKind::Text,
            "frame" => ShapeKind::Frame,
            "stamp" => ShapeKind::Stamp,
            "image" => ShapeKind::Image,
            _ => return None,
        })
    }
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
    /// 縦方向も中央に寄せるか(文字だけの要素は上詰め)。
    pub center_vertically: bool,
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
        // 図形の内側に入る矩形の大きさ(外接矩形に対する比率)。
        let (fx, fy, dy) = match self.kind {
            ShapeKind::Ellipse => (
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
            ),
            ShapeKind::Diamond => (0.5, 0.5, 0.0),
            ShapeKind::Triangle => (0.5, 0.5, 0.25),
            ShapeKind::Star => (0.45, 0.4, 0.05),
            ShapeKind::Hexagon => (0.75, 1.0, 0.0),
            ShapeKind::Parallelogram => (0.7, 1.0, 0.0),
            _ => (1.0, 1.0, 0.0),
        };
        let half = b.size() * 0.5 * Vec2::new(fx, fy);
        let half = (half - glam::Vec2::splat(pad)).max(glam::Vec2::ZERO);
        let center = b.center() + Vec2::new(0.0, b.size().y * dy * 0.5);
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
            center_vertically: !matches!(self.kind, ShapeKind::Text | ShapeKind::Frame),
        }
    }

    /// 文字だけの要素の高さを中身に合わせる。
    pub fn fit_text_height(&mut self) {
        if self.kind != ShapeKind::Text {
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
    match kind {
        ShapeKind::Ellipse | ShapeKind::Stamp => d.length_squared() <= 1.0,
        ShapeKind::Diamond => d.x.abs() + d.y.abs() <= 1.0,
        // 上の頂点と下の 2 頂点。
        ShapeKind::Triangle => d.x.abs() <= (d.y + 1.0) * 0.5,
        // 上下が平らで、左右が頂点(上辺の幅は全体の半分)。
        ShapeKind::Hexagon => d.x.abs() <= 1.0 - 0.5 * d.y.abs(),
        // 上辺が右へ 1/4 ずれる。
        ShapeKind::Parallelogram => (d.x + 0.25 * d.y).abs() <= 0.75,
        // 星は外接矩形で判定する(掴みやすさ優先)。
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
