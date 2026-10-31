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

impl Shape {
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
        let font_size = if self.font_size > 0.0 {
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
        Rect::new(center - half, center + half)
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
        match self.kind {
            ShapeKind::Ellipse => {
                let half = self.size * 0.5;
                if half.x <= 0.0 || half.y <= 0.0 {
                    return false;
                }
                let d = (p - (self.pos + half)) / half;
                d.length_squared() <= 1.0
            }
            ShapeKind::Diamond => {
                let half = self.size * 0.5;
                if half.x <= 0.0 || half.y <= 0.0 {
                    return false;
                }
                let d = ((p - (self.pos + half)) / half).abs();
                d.x + d.y <= 1.0
            }
            ShapeKind::Triangle => {
                // 上の頂点と下の 2 頂点。
                let b = self.bounds();
                if !b.contains(p) || self.size.y <= 0.0 {
                    return false;
                }
                let t = (p.y - b.min.y) / self.size.y; // 0(頂点)..1(底辺)
                let half_w = self.size.x * 0.5 * t;
                (p.x - b.center().x).abs() <= half_w
            }
            // 星・六角形・平行四辺形は外接矩形で判定する(掴みやすさ優先)。
            _ => p.cmpge(self.min()).all() && p.cmple(self.max()).all(),
        }
    }

    /// フレームの見出し部分か(フレームは見出しを掴んで動かす)。
    pub fn frame_title_contains(&self, p: Vec2) -> bool {
        self.kind == ShapeKind::Frame && self.text_rect().expand(4.0).contains(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
