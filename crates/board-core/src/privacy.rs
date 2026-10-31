//! 個人作業エリア: 持ち主以外には中身を見せないフレーム。
//!
//! フレームの `meta.private` に持ち主の参加者 ID を入れると、そのフレームの中(中心が入っている)
//! 要素は、ほかの参加者には中身のない「伏せたカード」として描かれ、選択も編集もできない。
//! 付箋をエリアの外へ出すか、持ち主がエリアを公開すると全員に見える。
//!
//! データ自体は全員に同期されるので、画面での目隠し(ワークショップで書いている途中を
//! 見せないため)であり、秘密の情報を守る仕組みではない。

use std::collections::HashSet;

use glam::Vec2;

use crate::board::Board;
use crate::color::Rgba8;
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind};

/// 伏せたカードの色。
pub const FACE_DOWN: Rgba8 = Rgba8::hex(0xd5d8dc);
/// 伏せたカードに書く文字。
pub const FACE_DOWN_TEXT: &str = "作業中";

/// 個人作業エリアの大きさ。
pub const AREA_SIZE: Vec2 = Vec2::new(720.0, 560.0);

/// ある参加者から見て隠す要素の判定。
pub struct Mask {
    viewer: String,
    /// ほかの人のエリア。
    areas: Vec<Rect>,
}

impl Mask {
    /// `viewer` から見た目隠し。ほかの人のエリアがなければ `None`(何も隠さない)。
    /// `viewer` が空なら、すべてのエリアの中身を隠す(サーバーで AI に渡すときなど)。
    pub fn new(board: &Board, viewer: &str) -> Option<Self> {
        let areas: Vec<Rect> = board
            .elements()
            .iter()
            .filter_map(Element::as_shape)
            .filter(|s| {
                s.kind == ShapeKind::Frame && !s.meta.private.is_empty() && s.meta.private != viewer
            })
            .map(Shape::bounds)
            .collect();
        (!areas.is_empty()).then(|| Self {
            viewer: viewer.to_string(),
            areas,
        })
    }

    /// 中身を隠すか。エリアのフレーム自体は隠さない。
    pub fn hides(&self, e: &Element) -> bool {
        if let Element::Shape(s) = e
            && s.kind == ShapeKind::Frame
            && !s.meta.private.is_empty()
        {
            return false;
        }
        let c = e.bounds().center();
        self.areas.iter().any(|r| r.contains(c))
    }

    pub fn viewer(&self) -> &str {
        &self.viewer
    }

    /// 隠す要素の ID。
    pub fn hidden_ids(&self, board: &Board) -> HashSet<ElementId> {
        board
            .elements()
            .iter()
            .filter(|e| self.hides(e))
            .map(Element::id)
            .collect()
    }
}

/// `viewer` から見て隠す要素(ほかの人のエリアがなければ空)。
pub fn hidden_ids(board: &Board, viewer: &str) -> HashSet<ElementId> {
    Mask::new(board, viewer)
        .map(|m| m.hidden_ids(board))
        .unwrap_or_default()
}

/// 伏せたカードとして描く形。描かないものは `None`(手書き線・コネクタ・画像)。
pub fn face_down(e: &Element) -> Option<Element> {
    let Element::Shape(s) = e else {
        return None;
    };
    if s.kind == ShapeKind::Image || s.kind == ShapeKind::Text || s.kind == ShapeKind::Stamp {
        return None;
    }
    let mut c = s.clone();
    if c.kind != ShapeKind::Sticky {
        c.kind = ShapeKind::Rect;
    }
    c.fill = FACE_DOWN;
    c.stroke = Rgba8::TRANSPARENT;
    c.text.clear();
    c.icon.clear();
    c.text_color = Rgba8::hex(0x5f6368);
    Some(Element::Shape(c))
}

impl Board {
    /// `owner` の個人作業エリアを `pos`(左上)に作る。
    pub fn add_personal_area(&mut self, owner: &str, name: &str, pos: Vec2) -> ElementId {
        let id = self.alloc_id();
        let mut f = Shape::frame(id, pos, AREA_SIZE);
        f.text = format!("{name} の個人エリア");
        f.meta.private = owner.to_string();
        self.push(f);
        id
    }

    /// 個人作業エリア(フレーム)の ID と持ち主。
    pub fn personal_areas(&self) -> Vec<(ElementId, String)> {
        self.elements()
            .iter()
            .filter_map(Element::as_shape)
            .filter(|s| s.kind == ShapeKind::Frame && !s.meta.private.is_empty())
            .map(|s| (s.id, s.meta.private.clone()))
            .collect()
    }

    /// エリアを公開する(`owner` が `None` なら全員分)。中身が全員に見えるようになる。
    /// 公開したエリアの数を返す。
    pub fn reveal_areas(&mut self, owner: Option<&str>) -> usize {
        let ids: Vec<ElementId> = self
            .personal_areas()
            .into_iter()
            .filter(|(_, o)| owner.is_none_or(|w| w == o))
            .map(|(id, _)| id)
            .collect();
        for id in &ids {
            self.update(*id, |e| {
                e.meta_mut().private.clear();
                if let Element::Shape(s) = e {
                    s.text = s.text.replace(" の個人エリア", " のエリア(公開)");
                }
            });
        }
        ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::STICKY_COLORS;

    fn note(b: &mut Board, p: Vec2, text: &str) -> ElementId {
        let id = b.alloc_id();
        let mut s = Shape::sticky(id, p, STICKY_COLORS[0]);
        s.text = text.into();
        b.push(s);
        id
    }

    #[test]
    fn hides_inside_others_area_until_revealed() {
        let mut b = Board::new();
        let area = b.add_personal_area("alice", "Alice", Vec2::ZERO);
        let inside = note(&mut b, Vec2::new(100.0, 100.0), "秘密");
        let outside = note(&mut b, Vec2::new(2000.0, 0.0), "公開");

        assert!(hidden_ids(&b, "alice").is_empty(), "持ち主には見える");
        let bob = hidden_ids(&b, "bob");
        assert!(bob.contains(&inside));
        assert!(!bob.contains(&outside));
        assert!(!bob.contains(&area), "エリアの枠は見える");
        assert!(hidden_ids(&b, "").contains(&inside));

        // 外へ出すと見える。
        b.update(inside, |e| e.translate(Vec2::new(3000.0, 0.0)));
        assert!(hidden_ids(&b, "bob").is_empty());
        b.update(inside, |e| e.translate(Vec2::new(-3000.0, 0.0)));

        assert_eq!(b.reveal_areas(Some("carol")), 0);
        assert_eq!(b.reveal_areas(Some("alice")), 1);
        assert!(hidden_ids(&b, "bob").is_empty());
        assert!(b.personal_areas().is_empty());
    }

    #[test]
    fn private_owner_survives_sync() {
        let mut a = Board::new();
        a.add_personal_area("alice", "Alice", Vec2::ZERO);
        note(&mut a, Vec2::new(50.0, 50.0), "x");
        let mut b = Board::new();
        for u in a.take_outgoing() {
            b.apply_remote(&u).unwrap();
        }
        assert_eq!(b.personal_areas().len(), 1);
        assert_eq!(hidden_ids(&b, "bob").len(), 1);
    }

    #[test]
    fn face_down_has_no_content() {
        let mut b = Board::new();
        let id = note(&mut b, Vec2::ZERO, "秘密");
        let f = face_down(b.get(id).unwrap()).unwrap();
        assert!(f.text().is_empty());
        assert_eq!(f.as_shape().unwrap().fill, FACE_DOWN);
    }
}
