//! スタンプ(ボードに押す丸いバッジ)。
//!
//! スタンプは `ShapeKind::Stamp` の図形で、色付きの丸に白い記号を 1 つ載せる。
//! 記号は図形の文字として保存するので、同期や保存は他の図形と同じ。

use glam::Vec2;

use crate::color::Rgba8;
use crate::element::ElementId;
use crate::shape::{Shape, ShapeKind};

/// スタンプの大きさ(ワールド単位、直径)。
pub const STAMP_SIZE: f32 = 56.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// 識別名(UI 用)。
    pub name: &'static str,
    /// 丸の上に載せる記号。
    pub glyph: &'static str,
    pub color: Rgba8,
    pub title: &'static str,
}

pub const STAMPS: [Stamp; 6] = [
    Stamp {
        name: "plus1",
        glyph: "+1",
        color: Rgba8::hex(0x2f6fed),
        title: "賛成",
    },
    Stamp {
        name: "heart",
        glyph: "♥",
        color: Rgba8::hex(0xe5484d),
        title: "いいね",
    },
    Stamp {
        name: "star",
        glyph: "★",
        color: Rgba8::hex(0xf5a524),
        title: "注目",
    },
    Stamp {
        name: "check",
        glyph: "✓",
        color: Rgba8::hex(0x30a46c),
        title: "完了",
    },
    Stamp {
        name: "question",
        glyph: "?",
        color: Rgba8::hex(0x8e4ec6),
        title: "質問",
    },
    Stamp {
        name: "exclaim",
        glyph: "!",
        color: Rgba8::hex(0xf76b15),
        title: "重要",
    },
];

impl Stamp {
    pub fn by_name(name: &str) -> Option<Stamp> {
        STAMPS.iter().copied().find(|s| s.name == name)
    }

    /// 中心 `center` に置くスタンプの図形を作る。
    pub fn shape(&self, id: ElementId, center: Vec2) -> Shape {
        let size = Vec2::splat(STAMP_SIZE);
        let mut s = Shape::new(id, ShapeKind::Stamp, center - size * 0.5, size, self.color);
        s.stroke = Rgba8::WHITE;
        s.stroke_width = 3.0;
        s.text = self.glyph.to_string();
        s.text_color = Rgba8::WHITE;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_shape() {
        let s = STAMPS[0].shape(ElementId(1), Vec2::new(100.0, 100.0));
        assert_eq!(s.kind, ShapeKind::Stamp);
        assert_eq!(s.bounds().center(), Vec2::new(100.0, 100.0));
        assert_eq!(s.text_style().color, Rgba8::WHITE);
        assert!(s.text_style().font_size > 20.0);
        assert!(s.contains(Vec2::new(100.0, 100.0)));
        assert!(!s.contains(Vec2::new(73.0, 73.0)), "丸の外の角");
        assert_eq!(Stamp::by_name("heart").unwrap().glyph, "♥");
    }
}
