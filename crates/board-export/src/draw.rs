//! 要素を [`Canvas`] に描く。見た目は Web クライアントの GPU 描画に近づけてあるが、
//! アイコン画像や影の細かいところは省く。

use ab_glyph::FontArc;
use board_core::geom::Rect;
use board_core::shape::{Shape, kind_contains};
use board_core::{Cap, Connector, Element, Rgba8, ShapeKind, Vec2};

use crate::canvas::Canvas;
use crate::text;

/// フレームの地の色。
pub const FRAME_FILL: Rgba8 = Rgba8::hex(0xffffff);
const FRAME_BORDER: Rgba8 = Rgba8::hex(0xd5d8e0);
const IMAGE_PLACEHOLDER: Rgba8 = Rgba8::hex(0xdfe2ea);
const VIDEO_FILL: Rgba8 = Rgba8::hex(0x23252d);

/// ワールド座標から画素への変換(`画素 = ワールド × scale + offset`)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub scale: f32,
    pub offset: Vec2,
}

impl View {
    /// ワールドの矩形 `area` が、画素の (0, 0) から `scale` 倍で描かれる変換。
    pub fn of_area(area: Rect, scale: f32) -> Self {
        Self {
            scale,
            offset: -area.min * scale,
        }
    }

    pub fn point(&self, p: Vec2) -> Vec2 {
        p * self.scale + self.offset
    }

    pub fn rect(&self, r: Rect) -> Rect {
        Rect::new(self.point(r.min), self.point(r.max))
    }
}

/// 描き方の設定。
#[derive(Clone, Copy)]
pub struct Style<'a> {
    pub font: Option<&'a FontArc>,
    /// 白抜きの記号などを塗る「紙」の色。
    pub paper: Rgba8,
}

/// 要素を奥から順に描く。画面の外の要素は飛ばす。
pub fn draw_elements(cv: &mut Canvas, elements: &[Element], view: View, style: Style) {
    let screen = cv.bounds().expand(2.0);
    for e in elements {
        let b = view.rect(element_bounds(e));
        if !b.intersects(&screen) {
            continue;
        }
        match e {
            Element::Shape(s) => draw_shape(cv, s, view, style),
            Element::Stroke(st) => {
                let pts: Vec<Vec2> = st.points.iter().map(|p| view.point(*p)).collect();
                cv.polyline(&pts, st.width * view.scale, st.color, None);
            }
            Element::Connector(c) => draw_connector(cv, c, view, style),
        }
    }
}

/// 描画で使う外接矩形(フレームの見出しと線の太さを含む)。
pub fn element_bounds(e: &Element) -> Rect {
    match e {
        Element::Shape(s) if s.kind == ShapeKind::Frame => s.bounds().union(&s.text_rect()),
        Element::Connector(c) => {
            let mut r = e.bounds();
            if let Some(tb) = c.label_box() {
                r = r.union(&tb.rect);
            }
            r.expand(c.arrow_size() + c.width)
        }
        Element::Stroke(st) => e.bounds().expand(st.width),
        _ => e.bounds(),
    }
}

fn has_stroke(s: &Shape) -> bool {
    s.stroke.0[3] > 0 && s.stroke_width > 0.0
}

fn draw_shape(cv: &mut Canvas, s: &Shape, view: View, style: Style) {
    let scale = view.scale;
    let r = view.rect(s.bounds());
    let radius = s.corner_radius * scale;
    match s.kind {
        ShapeKind::Frame => {
            let fill = if s.fill.0[3] > 0 { s.fill } else { FRAME_FILL };
            cv.rounded(r, 6.0 * scale.min(2.0), fill);
            cv.outline_rounded(
                r,
                6.0 * scale.min(2.0),
                (1.0 * scale).max(1.0),
                FRAME_BORDER,
            );
        }
        ShapeKind::Sticky => {
            let off = Vec2::new(0.0, 2.0 * scale);
            cv.rounded(
                Rect::new(r.min + off, r.max + off),
                radius,
                Rgba8([16, 20, 40, 26]),
            );
            cv.rounded(r, radius, s.fill);
        }
        ShapeKind::Image => {
            match s
                .image
                .as_ref()
                .and_then(|i| image::load_from_memory(i.bytes()).ok())
            {
                Some(img) => cv.image(&img.to_rgba8(), r, cv.bounds()),
                None => cv.rounded(r, 2.0, IMAGE_PLACEHOLDER),
            }
            return;
        }
        ShapeKind::Video => {
            // 表紙の画像があれば描き、なければ暗い四角。
            match s
                .image
                .as_ref()
                .and_then(|i| image::load_from_memory(i.bytes()).ok())
            {
                Some(img) => cv.image(&img.to_rgba8(), r, cv.bounds()),
                None => cv.rounded(r, 8.0 * scale.min(2.0), VIDEO_FILL),
            }
            // 再生ボタン。
            let c = r.center();
            let k = r.size().min_element() * 0.12;
            cv.polygon(
                &[
                    c + Vec2::new(-k * 0.8, -k),
                    c + Vec2::new(k, 0.0),
                    c + Vec2::new(-k * 0.8, k),
                ],
                Rgba8([255, 255, 255, 220]),
            );
            return;
        }
        ShapeKind::Ellipse => {
            if s.fill.0[3] > 0 {
                cv.ellipse(r, s.fill);
            }
            if has_stroke(s) {
                cv.outline_ellipse(r, s.stroke_width * scale, s.stroke);
            }
        }
        ShapeKind::Rect | ShapeKind::Text => {
            if s.fill.0[3] > 0 {
                cv.rounded(r, radius, s.fill);
            }
            if has_stroke(s) {
                cv.outline_rounded(r, radius, s.stroke_width * scale, s.stroke);
            }
        }
        // ひし形・六角形・雲など: 内外判定で塗る。
        kind => {
            let w = if has_stroke(s) {
                (s.stroke_width * scale).max(1.0)
            } else {
                0.0
            };
            let inner = r.expand(-w);
            if s.fill.0[3] > 0 {
                cv.fill_where(r, s.fill, None, None, |x, y| {
                    kind_contains(kind, &inner, Vec2::new(x, y))
                });
            }
            if w > 0.0 {
                let has_inner = inner.size().min_element() > 0.0;
                cv.fill_where(r, s.stroke, None, None, |x, y| {
                    let p = Vec2::new(x, y);
                    kind_contains(kind, &r, p) && !(has_inner && kind_contains(kind, &inner, p))
                });
            }
        }
    }
    let tb = s.text_box();
    text::draw_box(cv, style.font, &s.text, &tb, |p| view.point(p), scale);
}

/// 経路の端を `inset` だけ縮める(白抜きの記号の中に線を通さない)。
fn trim_path(path: &mut Vec<Vec2>, at_end: bool, inset: f32) {
    if inset <= 0.0 || path.len() < 2 {
        return;
    }
    if at_end {
        path.reverse();
    }
    let mut left = inset;
    while path.len() >= 2 {
        let len = path[0].distance(path[1]);
        if len > left {
            path[0] = path[0].lerp(path[1], left / len);
            break;
        }
        left -= len;
        path.remove(0);
    }
    if at_end {
        path.reverse();
    }
}

fn draw_connector(cv: &mut Canvas, c: &Connector, view: View, style: Style) {
    let scale = view.scale;
    let w = (c.width * scale).max(1.0);
    let caps: Vec<(bool, board_core::connector::CapShape)> = [false, true]
        .into_iter()
        .filter(|&at_end| (if at_end { c.end_cap } else { c.start_cap }) != Cap::None)
        .filter_map(|at_end| c.cap_shape(at_end).map(|s| (at_end, s)))
        .collect();
    let mut path = c.path.clone();
    for (at_end, cap) in &caps {
        trim_path(&mut path, *at_end, cap.inset);
    }
    let pts: Vec<Vec2> = path.iter().map(|p| view.point(*p)).collect();
    let dash = c.dashed.then_some((w * 3.0, w * 2.5));
    cv.polyline(&pts, w, c.color, dash);
    for (_, cap) in &caps {
        if cap.fill.len() >= 3 {
            let pts: Vec<Vec2> = cap.fill.iter().map(|p| view.point(*p)).collect();
            cv.polygon(&pts, c.color);
        } else if !cap.lines.is_empty() && cap.inset > 0.0 {
            // 白抜きの三角・ひし形は中を紙の色で塗る。
            let mut poly: Vec<Vec2> = cap.lines.iter().map(|(a, _)| view.point(*a)).collect();
            poly.dedup();
            cv.polygon(&poly, style.paper);
        }
        for (a, b) in &cap.lines {
            cv.polyline(&[view.point(*a), view.point(*b)], w, c.color, None);
        }
        for (ctr, rad) in &cap.circles {
            let ctr = view.point(*ctr);
            let rad = rad * scale;
            let r = Rect::new(ctr - Vec2::splat(rad), ctr + Vec2::splat(rad));
            cv.ellipse(r, style.paper);
            cv.outline_ellipse(r, w, c.color);
        }
    }
    if let Some(tb) = c.label_box() {
        // 線の上の文字は、読みやすいよう紙の色の下地を敷く。
        cv.rounded(view.rect(tb.rect), 4.0 * scale.min(2.0), style.paper);
        text::draw_box(cv, style.font, &c.text, &tb, |p| view.point(p), scale);
    }
}
