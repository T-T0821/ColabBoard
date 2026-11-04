//! 自作テンプレート(テンプレートギャラリー)。
//!
//! 選んだ要素をクリップボードと同じ形式([`arrange::copy`])で保存し、自分だけで使うか、
//! チーム(組織)に公開する。置くときは [`arrange::paste`] と同じく新しい ID で貼る。
//! ギャラリーに出す縮小図は [`preview_svg`] でサーバーが作る。

use std::collections::HashMap;
use std::fmt::Write as _;

use glam::Vec2;

use crate::arrange;
use crate::color::Rgba8;
use crate::element::{Element, ElementId, Endpoint};
use crate::geom::Rect;
use crate::shape::ShapeKind;

/// 名前の上限(文字数)。
pub const MAX_TITLE_CHARS: usize = 60;
/// 説明の上限(文字数)。
pub const MAX_DESCRIPTION_CHARS: usize = 200;
/// 分類の上限(文字数)。
pub const MAX_CATEGORY_CHARS: usize = 30;
/// 1 つのテンプレートに入れられる要素の数。
pub const MAX_ELEMENTS: usize = 3000;
/// 中身(クリップボード形式の文字列)の上限。画像を含めても会議の型として十分な大きさ。
pub const MAX_DATA_BYTES: usize = 8 * 1024 * 1024;
/// 縮小図に描く要素の上限(多すぎると一覧が重くなる)。
const MAX_PREVIEW_ELEMENTS: usize = 600;

/// 1 行の文字を整える(制御文字を落とし、前後の空白を取り、長さを切る)。
pub fn clean_line(s: &str, max: usize) -> String {
    s.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// 保存された中身を読む。読めない・空・多すぎるなら理由を返す。
pub fn parse(data: &str) -> Result<Vec<Element>, String> {
    if data.len() > MAX_DATA_BYTES {
        return Err(format!(
            "テンプレートが大きすぎます({} MB まで)",
            MAX_DATA_BYTES / 1024 / 1024
        ));
    }
    let elements = arrange::parse_clipboard(data).ok_or("テンプレートの中身を読めません")?;
    if elements.is_empty() {
        return Err("テンプレートに要素がありません".into());
    }
    if elements.len() > MAX_ELEMENTS {
        return Err(format!(
            "要素が多すぎます({MAX_ELEMENTS} 個まで。いまは {} 個)",
            elements.len()
        ));
    }
    Ok(elements)
}

/// 要素の外接矩形(コネクタの経路は保存されないので、図形と手書き線で測る)。
pub fn bounds(elements: &[Element]) -> Option<Rect> {
    elements
        .iter()
        .filter(|e| !matches!(e, Element::Connector(_)))
        .map(Element::bounds)
        .reduce(|a, b| a.union(&b))
}

fn css(c: Rgba8) -> Option<String> {
    let [r, g, b, a] = c.0;
    if a == 0 {
        return None;
    }
    Some(if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("rgba({r},{g},{b},{:.2})", f32::from(a) / 255.0)
    })
}

/// ギャラリーに出す縮小図(SVG)。色と形だけを描き、文字は灰色の線で表す。
///
/// 中身は数値と色だけで、利用者が書いた文字は入らない(そのまま HTML に入れてよい)。
pub fn preview_svg(elements: &[Element]) -> String {
    let Some(b) = bounds(elements) else {
        return String::new();
    };
    let pad = (b.size().x.max(b.size().y) * 0.03).max(8.0);
    let (x0, y0) = (b.min.x - pad, b.min.y - pad);
    let (w, h) = (b.size().x + pad * 2.0, b.size().y + pad * 2.0);
    // 線の太さは縮小しても見えるように、全体の大きさに合わせて太くする。
    let line = (w.max(h) / 240.0).max(1.0);
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{x0:.0} {y0:.0} {w:.0} {h:.0}" preserveAspectRatio="xMidYMid meet" aria-hidden="true">"#
    );
    let centers: HashMap<ElementId, Vec2> = elements
        .iter()
        .filter(|e| !matches!(e, Element::Connector(_)))
        .map(|e| (e.id(), e.bounds().center()))
        .collect();
    for e in elements.iter().take(MAX_PREVIEW_ELEMENTS) {
        match e {
            Element::Shape(s) => {
                let (x, y, sw, sh) = (s.pos.x, s.pos.y, s.size.x, s.size.y);
                let fill = match s.kind {
                    ShapeKind::Image | ShapeKind::Video => Some("#c9ced6".to_string()),
                    ShapeKind::Text => None,
                    _ => css(s.fill),
                };
                let stroke = if s.stroke_width > 0.0 {
                    css(s.stroke)
                } else {
                    None
                };
                let fill_attr = fill.as_deref().unwrap_or("none");
                let stroke_attr = match &stroke {
                    Some(c) => format!(r#" stroke="{c}" stroke-width="{line:.1}""#),
                    None => String::new(),
                };
                match s.kind {
                    ShapeKind::Ellipse | ShapeKind::Stamp => {
                        let _ = write!(
                            svg,
                            r#"<ellipse cx="{:.0}" cy="{:.0}" rx="{:.0}" ry="{:.0}" fill="{fill_attr}"{stroke_attr}/>"#,
                            x + sw / 2.0,
                            y + sh / 2.0,
                            sw / 2.0,
                            sh / 2.0
                        );
                    }
                    ShapeKind::Diamond => {
                        let _ = write!(
                            svg,
                            r#"<polygon points="{:.0},{:.0} {:.0},{:.0} {:.0},{:.0} {:.0},{:.0}" fill="{fill_attr}"{stroke_attr}/>"#,
                            x + sw / 2.0,
                            y,
                            x + sw,
                            y + sh / 2.0,
                            x + sw / 2.0,
                            y + sh,
                            x,
                            y + sh / 2.0
                        );
                    }
                    _ => {
                        if fill.is_some() || stroke.is_some() {
                            let r = s.corner_radius.min(sw / 2.0).min(sh / 2.0).max(0.0);
                            let _ = write!(
                                svg,
                                r#"<rect x="{x:.0}" y="{y:.0}" width="{sw:.0}" height="{sh:.0}" rx="{r:.0}" fill="{fill_attr}"{stroke_attr}/>"#
                            );
                        }
                    }
                }
                // 文字は灰色の線で(フレームは左上の見出し、それ以外は中ほど)。
                let lines = s.text.lines().filter(|l| !l.trim().is_empty()).count();
                if lines > 0 && sw > 0.0 && sh > 0.0 {
                    let fs = if s.font_size > 0.0 {
                        s.font_size
                    } else {
                        (sh / 6.0).clamp(10.0, 40.0)
                    };
                    let (tx, ty, max_lines) = if s.kind == ShapeKind::Frame {
                        (x + fs * 0.6, y - fs * 0.9, 1)
                    } else {
                        let n = lines.min(((sh * 0.7) / (fs * 1.4)).floor().max(1.0) as usize);
                        (x + sw * 0.15, y + sh / 2.0 - (n as f32 - 1.0) * fs * 0.7, n)
                    };
                    let lw = if s.kind == ShapeKind::Frame {
                        (sw * 0.3).min(fs * 8.0)
                    } else {
                        sw * 0.7
                    };
                    for i in 0..lines.min(max_lines) {
                        let len = if i + 1 == lines.min(max_lines) && i > 0 {
                            lw * 0.6
                        } else {
                            lw
                        };
                        let _ = write!(
                            svg,
                            r##"<rect x="{tx:.0}" y="{:.0}" width="{len:.0}" height="{:.0}" rx="{:.0}" fill="#5f6368" fill-opacity="0.45"/>"##,
                            ty + i as f32 * fs * 1.4 - fs * 0.25,
                            fs * 0.5,
                            fs * 0.25
                        );
                    }
                }
            }
            Element::Stroke(s) => {
                let Some(color) = css(s.color) else { continue };
                let mut pts = String::new();
                // 点が多い線は間引く。
                let step = (s.points.len() / 64).max(1);
                for p in s.points.iter().step_by(step) {
                    let _ = write!(pts, "{:.0},{:.0} ", p.x, p.y);
                }
                let _ = write!(
                    svg,
                    r#"<polyline points="{}" fill="none" stroke="{color}" stroke-width="{:.1}" stroke-linecap="round" stroke-linejoin="round"/>"#,
                    pts.trim_end(),
                    s.width.max(line)
                );
            }
            Element::Connector(c) => {
                let end = |p: &Endpoint| match p {
                    Endpoint::Free(v) => Some(*v),
                    Endpoint::Attached { id, .. } => centers.get(id).copied(),
                };
                let (Some(a), Some(z)) = (end(&c.from), end(&c.to)) else {
                    continue;
                };
                let color = css(c.color).unwrap_or_else(|| "#5f6368".into());
                let _ = write!(
                    svg,
                    r#"<line x1="{:.0}" y1="{:.0}" x2="{:.0}" y2="{:.0}" stroke="{color}" stroke-width="{:.1}"/>"#,
                    a.x,
                    a.y,
                    z.x,
                    z.y,
                    c.width.max(line)
                );
            }
        }
    }
    svg.push_str("</svg>");
    svg
}

/// 保存された中身を `center` を中心に新しい ID で置く。置いた要素の ID を返す。
pub fn insert(board: &mut crate::Board, elements: Vec<Element>, center: Vec2) -> Vec<ElementId> {
    let offset = match bounds(&elements) {
        Some(b) => center - b.center(),
        None => Vec2::ZERO,
    };
    arrange::paste(board, elements, offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Board;
    use crate::shape::Shape;
    use crate::templates::{self, Template};

    fn sample() -> (Board, Vec<ElementId>) {
        let mut b = Board::new();
        let ids = templates::insert(&mut b, Template::Kpt, Vec2::ZERO);
        (b, ids)
    }

    #[test]
    fn roundtrip_and_preview() {
        let (b, ids) = sample();
        let data = arrange::copy(&b, &ids).unwrap();
        let elements = parse(&data).unwrap();
        assert_eq!(elements.len(), ids.len());
        let svg = preview_svg(&elements);
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        assert!(svg.contains("<rect"));
        // 利用者の文字は縮小図に入らない。
        assert!(!svg.contains("Keep"));

        // 別のボードに置くと、新しい ID で同じ数だけ入り、中心が合う。
        let mut other = Board::new();
        let placed = insert(&mut other, elements, Vec2::new(5000.0, 0.0));
        assert_eq!(placed.len(), ids.len());
        assert!(placed.iter().all(|id| !ids.contains(id)));
        let r = other.bounds_of(&placed).unwrap();
        assert!((r.center().x - 5000.0).abs() < 400.0, "{:?}", r.center());
    }

    #[test]
    fn rejects_bad_data() {
        assert!(parse("hello").is_err());
        assert!(parse("colabboard/v1:").is_err());
        let mut b = Board::new();
        let mut ids = Vec::new();
        for i in 0..(MAX_ELEMENTS + 1) {
            let id = b.alloc_id();
            b.push(Shape::sticky(id, Vec2::new(i as f32, 0.0), Rgba8::WHITE));
            ids.push(id);
        }
        let data = arrange::copy(&b, &ids).unwrap();
        assert!(parse(&data).unwrap_err().contains("多すぎ"));
    }

    #[test]
    fn cleans_lines() {
        assert_eq!(clean_line("  振り返り\n用 ", 10), "振り返り用");
        assert_eq!(clean_line("あいうえお", 3), "あいう");
    }
}
