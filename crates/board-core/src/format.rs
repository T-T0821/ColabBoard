//! 選択中の要素の書式(書式バー)。
//!
//! 選択の共通の書式を [`Summary`] にまとめ、[`Change`] を選択中の全要素に当てる。
//! 当てはまらない要素(手書き線に文字の大きさなど)は変えない。

use crate::board::Board;
use crate::connector::{Cap, PathStyle};
use crate::element::{Element, ElementId};
use crate::shape::{Shape, ShapeKind, TextAlign};

/// 書式バーで選べる文字の大きさ。
pub const FONT_SIZES: [f32; 7] = [12.0, 16.0, 20.0, 24.0, 32.0, 48.0, 64.0];
/// 書式バーで選べる線の太さ。
pub const WIDTHS: [f32; 3] = [2.0, 4.0, 6.0];

/// 書式を 1 つ変える操作。
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    FontSize(f32),
    /// 文字を一段大きく / 小さくする(`FONT_SIZES` の隣の値へ)。
    FontStep(i32),
    Align(TextAlign),
    /// 図形の種類を変える(付箋・文字・フレームは対象外)。
    Kind(ShapeKind),
    Path(PathStyle),
    StartCap(Cap),
    EndCap(Cap),
    /// 線の太さ(コネクタ・手書き線・図形の枠線)。
    Width(f32),
    Link(String),
}

/// 選択の共通の書式。値が要素ごとに違えば `None`。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub count: usize,
    /// 文字の書式を変えられる図形があるか。
    pub has_text: bool,
    pub font_size: Option<f32>,
    pub align: Option<TextAlign>,
    /// 種類を変えられる図形があるか、その共通の種類。
    pub has_kind: bool,
    pub kind: Option<ShapeKind>,
    pub has_connector: bool,
    pub path: Option<PathStyle>,
    pub start_cap: Option<Cap>,
    pub end_cap: Option<Cap>,
    pub has_width: bool,
    pub width: Option<f32>,
    pub link: Option<String>,
    pub locked: bool,
}

/// 値がすべて同じなら `Some`。
fn merge<T: PartialEq + Clone>(acc: &mut Option<Option<T>>, v: T) {
    match acc {
        None => *acc = Some(Some(v)),
        Some(Some(a)) if *a != v => *acc = Some(None),
        _ => {}
    }
}

fn convertible(kind: ShapeKind) -> bool {
    !matches!(
        kind,
        ShapeKind::Sticky
            | ShapeKind::Text
            | ShapeKind::Frame
            | ShapeKind::Stamp
            | ShapeKind::Image
    )
}

/// 図形の枠線の太さ(枠線が無い図形は 0)。
fn shape_width(s: &Shape) -> f32 {
    s.stroke_width
}

pub fn summarize(board: &Board, ids: &[ElementId]) -> Summary {
    let mut out = Summary::default();
    let (mut fs, mut al, mut kind, mut path, mut sc, mut ec, mut w, mut link) =
        (None, None, None, None, None, None, None, None);
    for e in ids.iter().filter_map(|&id| board.get(id)) {
        out.count += 1;
        out.locked |= e.is_locked();
        merge(&mut link, e.meta().link.clone());
        match e {
            Element::Shape(s) if matches!(s.kind, ShapeKind::Stamp | ShapeKind::Image) => {}
            Element::Shape(s) => {
                out.has_text = true;
                let st = s.text_style();
                merge(&mut fs, st.font_size);
                merge(&mut al, st.align);
                if convertible(s.kind) {
                    out.has_kind = true;
                    merge(&mut kind, s.kind);
                    out.has_width = true;
                    merge(&mut w, shape_width(s));
                }
            }
            Element::Connector(c) => {
                out.has_connector = true;
                merge(&mut path, c.style);
                merge(&mut sc, c.start_cap);
                merge(&mut ec, c.end_cap);
                out.has_width = true;
                merge(&mut w, c.width);
            }
            Element::Stroke(s) => {
                out.has_width = true;
                merge(&mut w, s.width);
            }
        }
    }
    out.font_size = fs.flatten();
    out.align = al.flatten();
    out.kind = kind.flatten();
    out.path = path.flatten();
    out.start_cap = sc.flatten();
    out.end_cap = ec.flatten();
    out.width = w.flatten();
    out.link = link.flatten();
    out
}

/// `FONT_SIZES` で `size` の隣の値。
pub fn step_font(size: f32, dir: i32) -> f32 {
    if dir > 0 {
        FONT_SIZES
            .iter()
            .copied()
            .find(|&s| s > size + 0.5)
            .unwrap_or(FONT_SIZES[FONT_SIZES.len() - 1])
    } else {
        FONT_SIZES
            .iter()
            .rev()
            .copied()
            .find(|&s| s < size - 0.5)
            .unwrap_or(FONT_SIZES[0])
    }
}

fn apply_one(e: &mut Element, change: &Change) {
    match (e, change) {
        (Element::Shape(s), Change::FontSize(f)) => {
            s.font_size = *f;
            s.fit_text_height();
        }
        (Element::Shape(s), Change::FontStep(d)) => {
            s.font_size = step_font(s.text_style().font_size, *d);
            s.fit_text_height();
        }
        (Element::Shape(s), Change::Align(a)) => s.align = *a,
        (Element::Shape(s), Change::Kind(k)) if convertible(s.kind) && convertible(*k) => {
            s.kind = *k;
        }
        (Element::Shape(s), Change::Width(w)) if convertible(s.kind) => s.stroke_width = *w,
        (Element::Connector(c), Change::Path(p)) => c.style = *p,
        (Element::Connector(c), Change::StartCap(cap)) => c.start_cap = *cap,
        (Element::Connector(c), Change::EndCap(cap)) => c.end_cap = *cap,
        (Element::Connector(c), Change::Width(w)) => c.width = *w,
        (Element::Stroke(s), Change::Width(w)) => s.width = *w,
        (e, Change::Link(l)) => e.meta_mut().link = l.trim().to_string(),
        _ => {}
    }
}

/// 選択中の要素に書式を当てる。ロック中の要素は変えない。変えた数を返す。
pub fn apply(board: &mut Board, ids: &[ElementId], change: &Change) -> usize {
    let mut n = 0;
    for &id in ids {
        let changed = board.update(id, |e| {
            if e.is_locked() {
                return false;
            }
            let before = e.clone();
            apply_one(e, change);
            *e != before
        });
        n += usize::from(changed == Some(true));
    }
    n
}

/// リンクとして開いてよい URL に整える(スキームが無ければ https を補う)。
/// `javascript:` などは開かない。
pub fn normalize_link(link: &str) -> Option<String> {
    let l = link.trim();
    if l.is_empty() {
        return None;
    }
    let lower = l.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
    {
        return Some(l.to_string());
    }
    // ほかのスキーム(javascript: など)は開かない。
    if let Some((scheme, _)) = lower.split_once(':')
        && !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        && !scheme.contains('.')
    {
        return None;
    }
    Some(format!("https://{l}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rgba8;
    use crate::connector::{Connector, Endpoint};
    use crate::element::Stroke;
    use glam::Vec2;

    fn board() -> (Board, ElementId, ElementId, ElementId) {
        let mut b = Board::new();
        let r = b.alloc_id();
        b.push(Shape::new(
            r,
            ShapeKind::Rect,
            Vec2::ZERO,
            Vec2::splat(100.0),
            Rgba8::WHITE,
        ));
        let s = b.alloc_id();
        b.push(Shape::new(
            s,
            ShapeKind::Sticky,
            Vec2::new(200.0, 0.0),
            Vec2::splat(100.0),
            Rgba8::WHITE,
        ));
        let c = b.alloc_id();
        b.push(Connector::new(
            c,
            Endpoint::Free(Vec2::ZERO),
            Endpoint::Free(Vec2::new(50.0, 50.0)),
        ));
        (b, r, s, c)
    }

    #[test]
    fn summary_merges_values() {
        let (b, r, s, c) = board();
        let one = summarize(&b, &[r]);
        assert_eq!(one.font_size, Some(20.0));
        assert_eq!(one.kind, Some(ShapeKind::Rect));
        assert!(!one.has_connector);
        let two = summarize(&b, &[r, s]);
        assert_eq!(two.font_size, None); // 20 と 24
        assert_eq!(two.align, Some(TextAlign::Center));
        assert_eq!(two.kind, Some(ShapeKind::Rect)); // 付箋は種類の対象外
        let all = summarize(&b, &[r, s, c]);
        assert!(all.has_connector);
        assert_eq!(all.end_cap, Some(Cap::Arrow));
        assert_eq!(all.count, 3);
    }

    #[test]
    fn apply_changes_only_applicable() {
        let (mut b, r, s, c) = board();
        let ids = [r, s, c];
        apply(&mut b, &ids, &Change::FontSize(32.0));
        assert_eq!(summarize(&b, &ids).font_size, Some(32.0));
        apply(&mut b, &ids, &Change::Kind(ShapeKind::Ellipse));
        assert_eq!(
            b.get(r).unwrap().as_shape().unwrap().kind,
            ShapeKind::Ellipse
        );
        assert_eq!(
            b.get(s).unwrap().as_shape().unwrap().kind,
            ShapeKind::Sticky
        );
        apply(&mut b, &ids, &Change::EndCap(Cap::None));
        apply(&mut b, &ids, &Change::Path(PathStyle::Curved));
        let Some(Element::Connector(conn)) = b.get(c) else {
            panic!()
        };
        assert_eq!((conn.end_cap, conn.style), (Cap::None, PathStyle::Curved));
        b.end_gesture();
        apply(&mut b, &ids, &Change::Link(" example.com ".into()));
        assert_eq!(summarize(&b, &ids).link.as_deref(), Some("example.com"));
        // 元に戻せる。
        b.end_gesture();
        assert!(b.undo());
        assert_eq!(summarize(&b, &ids).link.as_deref(), Some(""));
    }

    #[test]
    fn locked_elements_are_kept() {
        let (mut b, r, _, _) = board();
        b.update(r, |e| e.meta_mut().locked = true);
        assert_eq!(apply(&mut b, &[r], &Change::FontSize(48.0)), 0);
    }

    #[test]
    fn font_steps_and_widths() {
        assert_eq!(step_font(20.0, 1), 24.0);
        assert_eq!(step_font(20.0, -1), 16.0);
        assert_eq!(step_font(64.0, 1), 64.0);
        assert_eq!(step_font(18.0, -1), 16.0);
        let mut b = Board::local();
        let id = b.alloc_id();
        b.push(Stroke::new(id, Vec2::ZERO, Rgba8::BLACK, 3.0));
        apply(&mut b, &[id], &Change::Width(6.0));
        assert_eq!(summarize(&b, &[id]).width, Some(6.0));
        assert!(!summarize(&b, &[id]).has_text);
    }

    #[test]
    fn links_are_sanitized() {
        assert_eq!(
            normalize_link("example.com/a").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(
            normalize_link("http://x.test").as_deref(),
            Some("http://x.test")
        );
        assert_eq!(normalize_link("javascript:alert(1)"), None);
        assert_eq!(normalize_link("  "), None);
    }
}
