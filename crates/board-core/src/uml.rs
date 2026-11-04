//! UML・ER 図・シーケンス図の部品。
//!
//! 専用の要素は持たず、普通の図形・手書き線・コネクタをグループにして組み立てる。
//! そのため同期・Undo・コピー&ペースト・書式の変更は他の要素と同じように動く。
//! クラスやエンティティは「外枠 + 区画(名前・属性・操作)」のグループで、
//! 線は外枠につなぐ。自動整列はグループを 1 つの塊として動かす。

use glam::Vec2;

use crate::color::Rgba8;
use crate::connector::{Cap, Connector, Endpoint, PathStyle};
use crate::element::{Element, ElementId, Stroke};
use crate::shape::{Shape, ShapeKind, TextAlign, VAlign, estimate_text_height};

/// 線と枠の色。
pub const INK: Rgba8 = Rgba8::hex(0x3c4043);
/// クラス名の区画の色。
pub const HEADER: Rgba8 = Rgba8::hex(0xe8f0fe);
/// エンティティ名の区画の色。
pub const ENTITY_HEADER: Rgba8 = Rgba8::hex(0xe6f4ea);
/// ライフラインの頭の色。
pub const PARTICIPANT: Rgba8 = Rgba8::hex(0xfef7e0);
/// インターフェースの名前の区画の 1 行目。
pub const INTERFACE_MARK: &str = "«interface»";
/// 区画の文字の大きさ。
const FONT: f32 = 16.0;
/// 区画の上下の余白の合計(図形の内側の余白 12 の 2 倍)。
const PAD: f32 = 24.0;

/// パネルから置ける部品。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Class,
    Interface,
    Entity,
    Lifeline,
    Activation,
    Fragment,
    Actor,
    UseCase,
    State,
    InitialState,
    FinalState,
    Note,
}

impl Part {
    pub const ALL: [Part; 12] = [
        Part::Class,
        Part::Interface,
        Part::Entity,
        Part::Lifeline,
        Part::Activation,
        Part::Fragment,
        Part::Actor,
        Part::UseCase,
        Part::State,
        Part::InitialState,
        Part::FinalState,
        Part::Note,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Part::Class => "class",
            Part::Interface => "interface",
            Part::Entity => "entity",
            Part::Lifeline => "lifeline",
            Part::Activation => "activation",
            Part::Fragment => "fragment",
            Part::Actor => "actor",
            Part::UseCase => "use-case",
            Part::State => "state",
            Part::InitialState => "initial-state",
            Part::FinalState => "final-state",
            Part::Note => "note",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Part::Class => "クラス",
            Part::Interface => "インターフェース",
            Part::Entity => "エンティティ(ER)",
            Part::Lifeline => "ライフライン",
            Part::Activation => "実行中(活性区間)",
            Part::Fragment => "複合フラグメント(alt / loop)",
            Part::Actor => "アクター",
            Part::UseCase => "ユースケース",
            Part::State => "状態",
            Part::InitialState => "開始状態",
            Part::FinalState => "終了状態",
            Part::Note => "ノート",
        }
    }

    /// 部品を作る。左上が `pos`。
    pub fn build(self, pos: Vec2, alloc: &mut dyn FnMut() -> ElementId) -> Vec<Element> {
        match self {
            Part::Class => {
                class_box(
                    alloc,
                    pos,
                    "クラス名",
                    &["- id: int\n- name: String", "+ save(): void"],
                )
                .0
            }
            Part::Interface => {
                class_box(
                    alloc,
                    pos,
                    "«interface»\nインターフェース名",
                    &["+ run(): void"],
                )
                .0
            }
            Part::Entity => {
                entity_box(
                    alloc,
                    pos,
                    "テーブル名",
                    "PK  id : int\nFK  user_id : int\n     name : text",
                )
                .0
            }
            Part::Lifeline => lifeline(alloc, pos, ":オブジェクト", 360.0).0,
            Part::Activation => vec![activation(alloc(), pos, 120.0).into()],
            Part::Fragment => {
                vec![fragment(alloc(), pos, Vec2::new(360.0, 200.0), "alt [条件]").into()]
            }
            Part::Actor => actor(alloc, pos, "利用者").0,
            Part::UseCase => vec![use_case(alloc(), pos, "ユースケース").into()],
            Part::State => vec![state(alloc(), pos, "状態").into()],
            Part::InitialState => vec![initial_state(alloc(), pos).into()],
            Part::FinalState => final_state(alloc, pos).0,
            Part::Note => vec![note(alloc(), pos, "メモ").into()],
        }
    }
}

/// 区画 1 つ(左揃え・上揃えの文字の入った四角形)。高さは文字に合わせる。
fn compartment(
    id: ElementId,
    pos: Vec2,
    width: f32,
    text: &str,
    fill: Rgba8,
    header: bool,
) -> Shape {
    let mut s = Shape::rect(id, pos, Vec2::new(width, 40.0), fill);
    s.stroke = INK;
    s.stroke_width = 1.5;
    s.corner_radius = 0.0;
    s.text = text.into();
    s.font_size = FONT;
    if header {
        s.align = TextAlign::Center;
        s.valign = VAlign::Middle;
    } else {
        s.align = TextAlign::Left;
        s.valign = VAlign::Top;
    }
    let style = s.text_style();
    let inner = s.text_rect().size().x;
    let h = estimate_text_height(text, inner, &style) + PAD;
    s.size.y = h.max(if header { 44.0 } else { 32.0 });
    s
}

/// 区画を縦に積んだ箱(クラス・エンティティ)。返り値は要素と、線をつなぐ外枠の ID。
fn stacked_box(
    alloc: &mut dyn FnMut() -> ElementId,
    pos: Vec2,
    title: &str,
    header_fill: Rgba8,
    sections: &[&str],
) -> (Vec<Element>, ElementId) {
    let width = box_width(title, sections);
    let group = alloc().0;
    let outer_id = alloc();
    let mut parts = Vec::new();
    let mut y = pos.y;
    let mut head = compartment(
        alloc(),
        Vec2::new(pos.x, y),
        width,
        title,
        header_fill,
        true,
    );
    head.meta.group = group;
    y += head.size.y;
    parts.push(head);
    for text in sections {
        let mut s = compartment(
            alloc(),
            Vec2::new(pos.x, y),
            width,
            text,
            Rgba8::WHITE,
            false,
        );
        s.meta.group = group;
        y += s.size.y;
        parts.push(s);
    }
    // 外枠: 区画の後ろに置き、線はここにつなぐ。
    let mut outer = Shape::rect(outer_id, pos, Vec2::new(width, y - pos.y), Rgba8::WHITE);
    outer.stroke = INK;
    outer.stroke_width = 2.0;
    outer.corner_radius = 0.0;
    outer.meta.group = group;
    let mut out: Vec<Element> = vec![outer.into()];
    out.extend(parts.into_iter().map(Element::from));
    (out, outer_id)
}

/// 文字の量に合わせた箱の幅。
fn box_width(title: &str, sections: &[&str]) -> f32 {
    let widest = std::iter::once(title)
        .chain(sections.iter().copied())
        .flat_map(str::lines)
        .map(|l| crate::shape::estimate_text_width(l, FONT))
        .fold(0.0, f32::max);
    (widest + 40.0).clamp(200.0, 420.0)
}

/// UML のクラス(名前・属性・操作の区画)。
pub fn class_box(
    alloc: &mut dyn FnMut() -> ElementId,
    pos: Vec2,
    name: &str,
    sections: &[&str],
) -> (Vec<Element>, ElementId) {
    stacked_box(alloc, pos, name, HEADER, sections)
}

/// ER 図のエンティティ(名前と列の一覧)。
pub fn entity_box(
    alloc: &mut dyn FnMut() -> ElementId,
    pos: Vec2,
    name: &str,
    columns: &str,
) -> (Vec<Element>, ElementId) {
    stacked_box(alloc, pos, name, ENTITY_HEADER, &[columns])
}

/// シーケンス図のライフライン(頭の箱 + 破線)。返り値は要素と、頭の箱の ID。
pub fn lifeline(
    alloc: &mut dyn FnMut() -> ElementId,
    pos: Vec2,
    name: &str,
    length: f32,
) -> (Vec<Element>, ElementId) {
    let group = alloc().0;
    let id = alloc();
    let width = (crate::shape::estimate_text_width(name, FONT) + 40.0).max(160.0);
    let mut head = Shape::rect(id, pos, Vec2::new(width, 56.0), PARTICIPANT);
    head.stroke = INK;
    head.corner_radius = 4.0;
    head.text = name.into();
    head.font_size = FONT;
    head.meta.group = group;
    let x = pos.x + width * 0.5;
    let mut line = Connector::new(
        alloc(),
        Endpoint::Free(Vec2::new(x, pos.y + 56.0)),
        Endpoint::Free(Vec2::new(x, pos.y + 56.0 + length)),
    );
    line.style = PathStyle::Straight;
    line.end_cap = Cap::None;
    line.dashed = true;
    line.color = INK;
    line.width = 1.5;
    line.meta.group = group;
    (vec![head.into(), line.into()], id)
}

/// ライフラインの上の実行中の区間(細長い四角)。`pos` は左上。
pub fn activation(id: ElementId, pos: Vec2, height: f32) -> Shape {
    let mut s = Shape::rect(id, pos, Vec2::new(16.0, height), Rgba8::WHITE);
    s.stroke = INK;
    s.stroke_width = 1.5;
    s.corner_radius = 0.0;
    s
}

/// 複合フラグメント(alt / loop / opt の枠)。見出しは左上。
pub fn fragment(id: ElementId, pos: Vec2, size: Vec2, label: &str) -> Shape {
    let mut s = Shape::rect(id, pos, size, Rgba8::TRANSPARENT);
    s.stroke = INK;
    s.stroke_width = 1.5;
    s.corner_radius = 0.0;
    s.text = label.into();
    s.font_size = 14.0;
    s.align = TextAlign::Left;
    s.valign = VAlign::Top;
    s
}

/// アクター(棒人間と名前)。返り値は要素と、線をつなぐ頭の ID。
pub fn actor(
    alloc: &mut dyn FnMut() -> ElementId,
    pos: Vec2,
    name: &str,
) -> (Vec<Element>, ElementId) {
    let group = alloc().0;
    let head_id = alloc();
    let cx = pos.x + 40.0;
    let mut head = Shape::ellipse(
        head_id,
        Vec2::new(cx - 16.0, pos.y),
        Vec2::splat(32.0),
        Rgba8::WHITE,
    );
    head.stroke = INK;
    head.meta.group = group;
    let mut out: Vec<Element> = vec![head.into()];
    let mut line = |pts: &[Vec2]| {
        let mut st = Stroke::new(alloc(), pts[0], INK, 2.5);
        for p in &pts[1..] {
            st.push_point(*p, 0.0);
        }
        st.meta.group = group;
        out.push(st.into());
    };
    let neck = Vec2::new(cx, pos.y + 32.0);
    let hip = Vec2::new(cx, pos.y + 72.0);
    line(&[neck, hip]);
    line(&[
        Vec2::new(cx - 26.0, pos.y + 46.0),
        Vec2::new(cx + 26.0, pos.y + 46.0),
    ]);
    line(&[
        Vec2::new(cx - 22.0, pos.y + 104.0),
        hip,
        Vec2::new(cx + 22.0, pos.y + 104.0),
    ]);
    let mut label = Shape::text(
        alloc(),
        Vec2::new(pos.x - 30.0, pos.y + 110.0),
        Vec2::new(140.0, 10.0),
    );
    label.text = name.into();
    label.font_size = FONT;
    label.align = TextAlign::Center;
    label.text_color = Rgba8::BLACK;
    label.fit_text_height();
    label.meta.group = group;
    out.push(label.into());
    (out, head_id)
}

/// ユースケース(楕円)。
pub fn use_case(id: ElementId, pos: Vec2, name: &str) -> Shape {
    let mut s = Shape::ellipse(id, pos, Vec2::new(220.0, 90.0), Rgba8::WHITE);
    s.stroke = INK;
    s.text = name.into();
    s.font_size = FONT;
    s
}

/// 状態(角丸の四角)。
pub fn state(id: ElementId, pos: Vec2, name: &str) -> Shape {
    let mut s = Shape::rect(id, pos, Vec2::new(180.0, 70.0), Rgba8::WHITE);
    s.stroke = INK;
    s.corner_radius = 20.0;
    s.text = name.into();
    s.font_size = FONT;
    s
}

/// 開始状態(黒丸)。
pub fn initial_state(id: ElementId, pos: Vec2) -> Shape {
    let mut s = Shape::ellipse(id, pos, Vec2::splat(32.0), INK);
    s.stroke = INK;
    s
}

/// 終了状態(二重丸)。返り値は要素と、線をつなぐ外側の丸の ID。
pub fn final_state(alloc: &mut dyn FnMut() -> ElementId, pos: Vec2) -> (Vec<Element>, ElementId) {
    let group = alloc().0;
    let id = alloc();
    let mut outer = Shape::ellipse(id, pos, Vec2::splat(40.0), Rgba8::WHITE);
    outer.stroke = INK;
    outer.meta.group = group;
    let mut inner = Shape::ellipse(alloc(), pos + Vec2::splat(9.0), Vec2::splat(22.0), INK);
    inner.stroke = INK;
    inner.stroke_width = 0.0;
    inner.meta.group = group;
    (vec![outer.into(), inner.into()], id)
}

/// ノート(角の欠けた紙)。
pub fn note(id: ElementId, pos: Vec2, text: &str) -> Shape {
    let mut s = Shape::new(
        id,
        ShapeKind::Card,
        pos,
        Vec2::new(200.0, 100.0),
        Rgba8::hex(0xfff8c5),
    );
    s.stroke = Rgba8::hex(0xb8a33a);
    s.stroke_width = 1.5;
    s.text = text.into();
    s.font_size = 14.0;
    s.align = TextAlign::Left;
    s
}

/// 要素 `from` から `to` への線。
pub fn relation(id: ElementId, from: ElementId, to: ElementId, kind: Relation) -> Connector {
    let at = |id| Endpoint::Attached { id, anchor: None };
    let mut c = Connector::new(id, at(from), at(to));
    c.color = INK;
    c.width = 2.0;
    let (style, start, end, dashed) = kind.look();
    c.style = style;
    c.start_cap = start;
    c.end_cap = end;
    c.dashed = dashed;
    c
}

/// よく使う線の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    /// 関連(矢印つき)。
    Association,
    /// 継承(白三角)。
    Inheritance,
    /// 実現(破線 + 白三角)。
    Realization,
    /// 依存(破線 + 矢印)。
    Dependency,
    /// 集約(始点が白ひし形)。
    Aggregation,
    /// コンポジション(始点が黒ひし形)。
    Composition,
    /// ER: 1 対 多(始点 1、終点 0..多)。
    OneToMany,
    /// ER: 1 対 0..1。
    OneToZeroOrOne,
    /// ER: 多 対 多。
    ManyToMany,
    /// 線だけ(ユースケースとアクターなど)。
    Line,
    /// 状態遷移・流れ(曲線の矢印)。
    Transition,
}

impl Relation {
    fn look(self) -> (PathStyle, Cap, Cap, bool) {
        use PathStyle::{Curved, Elbow, Straight};
        match self {
            Relation::Association => (Straight, Cap::None, Cap::Arrow, false),
            Relation::Inheritance => (Elbow, Cap::None, Cap::Triangle, false),
            Relation::Realization => (Elbow, Cap::None, Cap::Triangle, true),
            Relation::Dependency => (Straight, Cap::None, Cap::Arrow, true),
            Relation::Aggregation => (Elbow, Cap::Diamond, Cap::None, false),
            Relation::Composition => (Elbow, Cap::DiamondFilled, Cap::None, false),
            Relation::OneToMany => (Elbow, Cap::One, Cap::ZeroOrMany, false),
            Relation::OneToZeroOrOne => (Elbow, Cap::One, Cap::ZeroOrOne, false),
            Relation::ManyToMany => (Elbow, Cap::OneOrMany, Cap::OneOrMany, false),
            Relation::Line => (Straight, Cap::None, Cap::None, false),
            Relation::Transition => (Curved, Cap::None, Cap::Arrow, false),
        }
    }
}

/// シーケンス図のメッセージ(水平の矢印)。`dashed` なら戻り。
pub fn message(
    id: ElementId,
    from_x: f32,
    to_x: f32,
    y: f32,
    text: &str,
    reply: bool,
) -> Connector {
    let mut c = Connector::new(
        id,
        Endpoint::Free(Vec2::new(from_x, y)),
        Endpoint::Free(Vec2::new(to_x, y)),
    );
    c.style = PathStyle::Straight;
    c.color = INK;
    c.width = 2.0;
    c.end_cap = if reply {
        Cap::Arrow
    } else {
        Cap::TriangleFilled
    };
    c.dashed = reply;
    c.text = text.into();
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> impl FnMut() -> ElementId {
        let mut n = 0;
        move || {
            n += 1;
            ElementId(n)
        }
    }

    #[test]
    fn every_part_builds_with_unique_ids() {
        for p in Part::ALL {
            let mut alloc = ids();
            let els = p.build(Vec2::new(10.0, 20.0), &mut alloc);
            assert!(!els.is_empty(), "{p:?}");
            let mut v: Vec<_> = els.iter().map(Element::id).collect();
            v.sort();
            v.dedup();
            assert_eq!(v.len(), els.len(), "{p:?}");
            assert_eq!(Part::from_name(p.name()), Some(p));
            // 複数の要素でできた部品は 1 つのグループ。
            if els.len() > 1 {
                let g = els[0].meta().group;
                assert!(g != 0 && els.iter().all(|e| e.meta().group == g), "{p:?}");
            }
        }
    }

    #[test]
    fn class_compartments_stack_inside_outer() {
        let mut alloc = ids();
        let (els, outer) = class_box(
            &mut alloc,
            Vec2::ZERO,
            "User",
            &["- id\n- name\n- email", "+ save()"],
        );
        assert_eq!(els.len(), 4);
        assert_eq!(els[0].id(), outer);
        let r = els[0].bounds();
        let mut y = 0.0;
        for e in &els[1..] {
            let b = e.bounds();
            assert_eq!(b.min.y, y);
            assert_eq!((b.min.x, b.max.x), (r.min.x, r.max.x));
            y = b.max.y;
        }
        assert_eq!(y, r.max.y);
        // 3 行の属性は 1 行の操作より高い。
        assert!(els[2].bounds().size().y > els[3].bounds().size().y);
    }
}
