//! アクセシビリティ: キーボードだけでの操作と、画面読み上げ用の説明文。
//!
//! GPU で描いたキャンバスの中身は支援技術から見えないので、ここで要素を
//! 「読む順番」に並べ、1 つずつ日本語の説明文にする。矢印キーでの
//! フォーカス移動(空間ナビゲーション)、キーボードでの配置・接続もここに置く。

use glam::Vec2;

use crate::arrange;
use crate::board::Board;
use crate::connector::{Connector, Endpoint, PathStyle};
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind};

/// 矢印キーの向き。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    /// `ArrowLeft` などのキー名から。
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "ArrowLeft" => Dir::Left,
            "ArrowRight" => Dir::Right,
            "ArrowUp" => Dir::Up,
            "ArrowDown" => Dir::Down,
            _ => return None,
        })
    }

    /// 単位ベクトル(y 下向き)。
    pub fn vector(self) -> Vec2 {
        match self {
            Dir::Left => Vec2::new(-1.0, 0.0),
            Dir::Right => Vec2::new(1.0, 0.0),
            Dir::Up => Vec2::new(0.0, -1.0),
            Dir::Down => Vec2::new(0.0, 1.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Dir::Left => "左",
            Dir::Right => "右",
            Dir::Up => "上",
            Dir::Down => "下",
        }
    }
}

/// 説明文に入れる本文の最大文字数。
const TEXT_LIMIT: usize = 80;

/// 要素の種類の名前(「付箋」「四角形」「コネクタ」など)。
pub fn kind_label(e: &Element) -> &'static str {
    match e {
        Element::Shape(s) => match s.kind {
            ShapeKind::Text => "テキスト",
            ShapeKind::Frame => "フレーム",
            ShapeKind::Stamp => "スタンプ",
            ShapeKind::Image => "画像",
            _ if !s.icon.is_empty() => "構成図の部品",
            k => k.label(),
        },
        Element::Stroke(s) if s.highlighter => "蛍光ペンの線",
        Element::Stroke(_) => "手書きの線",
        Element::Connector(_) => "コネクタ",
    }
}

/// 長い文字を縮めて 1 行にする。
fn short(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > TEXT_LIMIT {
        let mut s: String = one_line.chars().take(TEXT_LIMIT).collect();
        s.push('…');
        s
    } else {
        one_line
    }
}

/// コネクタの端の相手の呼び名。
fn end_name(board: &Board, end: &Endpoint) -> String {
    match end.attached_to().and_then(|id| board.get(id)) {
        Some(e) => {
            let text = short(e.text());
            if text.is_empty() {
                kind_label(e).to_string()
            } else {
                format!("{}「{text}」", kind_label(e))
            }
        }
        None => "どこにもつながっていない点".to_string(),
    }
}

/// 要素の説明文。例: `付箋「売上を伸ばす」`、`コネクタ 四角形「A」から 楕円「B」へ`。
pub fn describe(board: &Board, e: &Element) -> String {
    let mut out = match e {
        Element::Connector(c) => {
            let label = short(&c.text);
            let mut s = format!(
                "コネクタ {} から {} へ",
                end_name(board, &c.from),
                end_name(board, &c.to)
            );
            if !label.is_empty() {
                s.push_str(&format!("、ラベル「{label}」"));
            }
            s
        }
        Element::Shape(s) if s.kind == ShapeKind::Stamp => {
            let name = crate::stamp::STAMPS
                .iter()
                .find(|st| st.glyph == s.text)
                .map_or(s.text.as_str(), |st| st.title);
            format!("スタンプ「{name}」")
        }
        _ => {
            let text = short(e.text());
            let kind = kind_label(e);
            if text.is_empty() {
                format!("{kind}(文字なし)")
            } else {
                format!("{kind}「{text}」")
            }
        }
    };
    let n = connections(board, e.id());
    if n > 0 && !matches!(e, Element::Connector(_)) {
        out.push_str(&format!("、つながり {n} 本"));
    }
    if e.is_locked() {
        out.push_str("、ロック中");
    }
    if !e.meta().link.is_empty() {
        out.push_str("、リンクあり");
    }
    out
}

/// その要素につながっているコネクタの数。
pub fn connections(board: &Board, id: ElementId) -> usize {
    board
        .elements()
        .iter()
        .filter(|e| match e {
            Element::Connector(c) => {
                c.from.attached_to() == Some(id) || c.to.attached_to() == Some(id)
            }
            _ => false,
        })
        .count()
}

/// フォーカスを移せる要素か(コネクタも含む。消えかけの要素などは除かない)。
fn focusable(e: &Element) -> bool {
    !matches!(e, Element::Shape(s) if s.size == Vec2::ZERO)
}

/// 読む順番: 上の行から下へ、同じ行の中は左から右へ。
///
/// 縦方向に半分以上重なる要素を同じ行とみなす。フレームはその中身より先に来る。
pub fn reading_order(board: &Board) -> Vec<ElementId> {
    let items: Vec<(ElementId, Rect)> = board
        .elements()
        .iter()
        .filter(|e| focusable(e))
        .map(|e| (e.id(), e.bounds()))
        .collect();
    reading_order_of(items)
}

/// 指定した要素(と矩形)を読む順番に並べる。
pub fn reading_order_of(mut items: Vec<(ElementId, Rect)>) -> Vec<ElementId> {
    items.sort_by(|a, b| {
        a.1.min
            .y
            .total_cmp(&b.1.min.y)
            .then(a.1.min.x.total_cmp(&b.1.min.x))
    });
    // (行の下端, 行の要素)
    let mut rows: Vec<(f32, Vec<(ElementId, Rect)>)> = Vec::new();
    for (id, r) in items {
        let h = r.size().y.max(1.0);
        match rows.last_mut() {
            // 行の先頭の要素と縦に半分以上重なれば同じ行。
            Some((bottom, row)) if r.min.y < *bottom && (*bottom - r.min.y) >= h * 0.5 => {
                row.push((id, r));
            }
            _ => rows.push((r.max.y, vec![(id, r)])),
        }
    }
    rows.into_iter()
        .flat_map(|(_, mut row)| {
            row.sort_by(|a, b| {
                a.1.min
                    .x
                    .total_cmp(&b.1.min.x)
                    .then(a.1.min.y.total_cmp(&b.1.min.y))
            });
            row.into_iter().map(|(id, _)| id)
        })
        .collect()
}

/// 読む順番で `current` の次(`forward` が false なら前)。端まで来たら `None`。
/// `current` がなければ先頭(後ろ向きなら末尾)。
pub fn step(order: &[ElementId], current: Option<ElementId>, forward: bool) -> Option<ElementId> {
    let pos = current.and_then(|c| order.iter().position(|id| *id == c));
    match (pos, forward) {
        (None, true) => order.first().copied(),
        (None, false) => order.last().copied(),
        (Some(i), true) => order.get(i + 1).copied(),
        (Some(i), false) => i.checked_sub(1).and_then(|j| order.get(j)).copied(),
    }
}

/// 空間ナビゲーションの対象(コネクタと線は除く)。
fn nav_target(e: &Element) -> bool {
    matches!(e, Element::Shape(s) if s.size != Vec2::ZERO)
}

/// `from` から `dir` の方向で一番近い要素(フレームの中から外へも移れる)。
///
/// 向きの軸の距離に、横ずれの距離を 2 倍にして足したものが小さいものを選ぶ。
pub fn nearest_in_direction(board: &Board, from: ElementId, dir: Dir) -> Option<ElementId> {
    let origin = board.get(from)?.bounds();
    let c = origin.center();
    let v = dir.vector();
    board
        .elements()
        .iter()
        .filter(|e| e.id() != from && nav_target(e))
        .filter_map(|e| {
            let r = e.bounds();
            // 同じ要素を囲むフレームなどへは移らない(中身から外枠に飛ぶと迷う)。
            if r.contains_rect(&origin) || origin.contains_rect(&r) {
                return None;
            }
            let d = r.center() - c;
            let along = d.dot(v);
            if along <= 1.0 {
                return None;
            }
            let across = (d - v * along).length();
            Some((e.id(), along + across * 2.0))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

/// 画面の中心に一番近い要素(何も選んでいないときの最初のフォーカス先)。
pub fn nearest_to(board: &Board, p: Vec2) -> Option<ElementId> {
    board
        .elements()
        .iter()
        .filter(|e| nav_target(e))
        .min_by(|a, b| {
            a.bounds()
                .center()
                .distance_squared(p)
                .total_cmp(&b.bounds().center().distance_squared(p))
        })
        .map(Element::id)
}

/// 選んだ要素(フレームの中身ごと)を `d` だけ動かす。動かした数を返す。
pub fn nudge(board: &mut Board, selection: &[ElementId], d: Vec2) -> usize {
    let ids = arrange::move_set(board, selection);
    for id in &ids {
        board.update(*id, |e| e.translate(d));
    }
    ids.len()
}

/// 2 つの要素をコネクタでつなぐ(すでにつながっていれば何もしない)。
pub fn connect(
    board: &mut Board,
    from: ElementId,
    to: ElementId,
    style: PathStyle,
) -> Option<ElementId> {
    if from == to || board.get(from).is_none() || board.get(to).is_none() {
        return None;
    }
    let exists = board.elements().iter().any(|e| match e {
        Element::Connector(c) => {
            let ends = (c.from.attached_to(), c.to.attached_to());
            ends == (Some(from), Some(to)) || ends == (Some(to), Some(from))
        }
        _ => false,
    });
    if exists {
        return None;
    }
    let id = board.alloc_id();
    let mut c = Connector::new(
        id,
        Endpoint::Attached {
            id: from,
            anchor: None,
        },
        Endpoint::Attached {
            id: to,
            anchor: None,
        },
    );
    c.style = style;
    board.push(c);
    Some(id)
}

/// `from` の `dir` 側に、同じ種類・色・大きさの空の要素を置く(つなぐ相手がいないとき用)。
pub fn place_sibling(board: &mut Board, from: ElementId, dir: Dir) -> Option<ElementId> {
    let Some(Element::Shape(src)) = board.get(from).cloned() else {
        return None;
    };
    if matches!(src.kind, ShapeKind::Frame | ShapeKind::Image) {
        return None;
    }
    const GAP: f32 = 80.0;
    let step = (src.size + Vec2::splat(GAP)) * dir.vector();
    let id = board.alloc_id();
    let mut s = Shape {
        id,
        text: String::new(),
        meta: Default::default(),
        ..src
    };
    s.pos = free_spot(
        board,
        Rect::new(src.pos + step, src.pos + step + src.size),
        step,
    );
    board.push(s);
    Some(id)
}

/// `rect` が他の要素(フレーム以外)と重ならない位置になるまで `step` ずつずらす。
pub fn free_spot(board: &Board, rect: Rect, step: Vec2) -> Vec2 {
    let step = if step == Vec2::ZERO {
        Vec2::new(rect.size().x + 40.0, 0.0)
    } else {
        step
    };
    let mut r = rect;
    for _ in 0..50 {
        let hit = board.query_visible(&r).iter().any(|e| {
            !matches!(e, Element::Connector(_))
                && !matches!(e, Element::Shape(s) if s.kind == ShapeKind::Frame)
                && e.bounds().intersects(&r)
        });
        if !hit {
            break;
        }
        r = Rect::new(r.min + step, r.max + step);
    }
    r.min
}

/// ボード全体のあらまし。例: `要素 12 個(付箋 5、四角形 4、コネクタ 3)`。
pub fn summary(board: &Board) -> String {
    if board.is_empty() {
        return "ボードは空です".to_string();
    }
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for e in board.elements() {
        let k = kind_label(e);
        match counts.iter_mut().find(|(name, _)| *name == k) {
            Some((_, n)) => *n += 1,
            None => counts.push((k, 1)),
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    let parts: Vec<String> = counts
        .iter()
        .take(6)
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    let more = if counts.len() > 6 { "、ほか" } else { "" };
    format!("要素 {} 個({}{more})", board.len(), parts.join("、"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgba8;

    fn sticky(board: &mut Board, x: f32, y: f32, text: &str) -> ElementId {
        let id = board.alloc_id();
        let mut s = Shape::sticky(id, Vec2::new(x, y), Rgba8::WHITE);
        s.text = text.into();
        board.push(s);
        id
    }

    #[test]
    fn reading_order_is_rows_then_columns() {
        let mut b = Board::new();
        let c = sticky(&mut b, 0.0, 300.0, "C");
        let bb = sticky(&mut b, 300.0, 10.0, "B");
        let a = sticky(&mut b, 0.0, 0.0, "A");
        assert_eq!(reading_order(&b), vec![a, bb, c]);
    }

    #[test]
    fn step_stops_at_ends() {
        let order = [ElementId(1), ElementId(2)];
        assert_eq!(step(&order, None, true), Some(ElementId(1)));
        assert_eq!(step(&order, None, false), Some(ElementId(2)));
        assert_eq!(step(&order, Some(ElementId(1)), true), Some(ElementId(2)));
        assert_eq!(step(&order, Some(ElementId(2)), true), None);
        assert_eq!(step(&order, Some(ElementId(1)), false), None);
    }

    #[test]
    fn nearest_in_direction_prefers_aligned() {
        let mut b = Board::new();
        let a = sticky(&mut b, 0.0, 0.0, "A");
        let right = sticky(&mut b, 400.0, 0.0, "R");
        let _diag = sticky(&mut b, 300.0, 400.0, "D");
        let below = sticky(&mut b, 0.0, 400.0, "B");
        assert_eq!(nearest_in_direction(&b, a, Dir::Right), Some(right));
        assert_eq!(nearest_in_direction(&b, a, Dir::Down), Some(below));
        assert_eq!(nearest_in_direction(&b, a, Dir::Left), None);
    }

    #[test]
    fn describe_sticky_and_connector() {
        let mut b = Board::new();
        let a = sticky(&mut b, 0.0, 0.0, "売上\nを伸ばす");
        let c = sticky(&mut b, 400.0, 0.0, "");
        let k = connect(&mut b, a, c, PathStyle::Straight).unwrap();
        assert_eq!(
            describe(&b, b.get(a).unwrap()),
            "付箋「売上 を伸ばす」、つながり 1 本"
        );
        assert_eq!(
            describe(&b, b.get(k).unwrap()),
            "コネクタ 付箋「売上 を伸ばす」 から 付箋 へ"
        );
        // 2 回目はつながない。
        assert!(connect(&mut b, c, a, PathStyle::Straight).is_none());
    }

    #[test]
    fn nudge_moves_frame_contents() {
        let mut b = Board::new();
        let f = b.alloc_id();
        b.push(Shape::frame(f, Vec2::ZERO, Vec2::splat(1000.0)));
        let a = sticky(&mut b, 100.0, 100.0, "A");
        assert_eq!(nudge(&mut b, &[f], Vec2::new(10.0, 0.0)), 2);
        assert_eq!(b.get(a).unwrap().bounds().min, Vec2::new(110.0, 100.0));
    }

    #[test]
    fn sibling_avoids_overlap() {
        let mut b = Board::new();
        let a = sticky(&mut b, 0.0, 0.0, "A");
        let _blocker = sticky(&mut b, 280.0, 0.0, "X");
        let s = place_sibling(&mut b, a, Dir::Right).unwrap();
        let r = b.get(s).unwrap().bounds();
        assert_eq!(r.min, Vec2::new(560.0, 0.0));
        assert_eq!(b.get(s).unwrap().text(), "");
    }

    #[test]
    fn editor_create_at_places_without_overlap() {
        let mut b = Board::new();
        let mut ed = crate::Editor::new();
        ed.set_tool(crate::Tool::Sticky);
        let a = ed.create_at(&mut b, Vec2::ZERO).unwrap();
        assert_eq!(ed.tool(), crate::Tool::Select);
        assert_eq!(ed.selection(), &[a]);
        ed.set_tool(crate::Tool::Sticky);
        let c = ed.create_at(&mut b, Vec2::ZERO).unwrap();
        assert!(
            !b.get(a)
                .unwrap()
                .bounds()
                .intersects(&b.get(c).unwrap().bounds())
        );
        ed.set_tool(crate::Tool::Pen);
        assert!(ed.create_at(&mut b, Vec2::ZERO).is_none());
    }

    #[test]
    fn summary_counts_kinds() {
        let mut b = Board::new();
        assert_eq!(summary(&b), "ボードは空です");
        sticky(&mut b, 0.0, 0.0, "A");
        sticky(&mut b, 300.0, 0.0, "B");
        assert_eq!(summary(&b), "要素 2 個(付箋 2)");
    }
}
