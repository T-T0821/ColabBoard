//! 公開ページの OGP 画像(1200×630 の PNG)を CPU で描く。
//!
//! サーバーには GPU がないので、図形・付箋・線を簡単な塗りつぶしで描き、
//! 文字は灰色の線(読めない大きさでも「文字がある」と分かる表現)で表す。
//! 2 倍の大きさで描いてから縮小し、輪郭を滑らかにする。
//! 下の帯にボード名を描く(フォントがあれば)。

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use board_core::geom::Rect;
use board_core::{Board, Element, Rgba8, ShapeKind, Vec2};
use image::{ImageEncoder, Rgba, RgbaImage};

pub const WIDTH: u32 = 1200;
pub const HEIGHT: u32 = 630;
/// 下の帯(ボード名)の高さ。
const FOOTER: u32 = 96;
const SS: u32 = 2;

const BG: Rgba8 = Rgba8::hex(0xf6f7f9);
const FOOTER_BG: Rgba8 = Rgba8::hex(0xffffff);
const INK: Rgba8 = Rgba8::hex(0x1b1d24);
const MUTED: Rgba8 = Rgba8::hex(0x6b7080);
const ACCENT: Rgba8 = Rgba8::hex(0x4c5cf5);

struct Canvas {
    img: RgbaImage,
}

impl Canvas {
    fn new(w: u32, h: u32, bg: Rgba8) -> Self {
        Self {
            img: RgbaImage::from_pixel(w, h, Rgba(bg.0)),
        }
    }

    fn blend(&mut self, x: i32, y: i32, c: Rgba8, coverage: f32) {
        if x < 0 || y < 0 || x >= self.img.width() as i32 || y >= self.img.height() as i32 {
            return;
        }
        let a = c.0[3] as f32 / 255.0 * coverage.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let p = self.img.get_pixel_mut(x as u32, y as u32);
        for i in 0..3 {
            p.0[i] = (p.0[i] as f32 * (1.0 - a) + c.0[i] as f32 * a).round() as u8;
        }
        p.0[3] = 255;
    }

    /// `inside(x, y)` が真の画素を `rect` の範囲で塗る。
    fn fill_where(&mut self, r: Rect, c: Rgba8, inside: impl Fn(f32, f32) -> bool) {
        let (w, h) = (self.img.width() as f32, self.img.height() as f32);
        let x0 = r.min.x.max(0.0).floor() as i32;
        let y0 = r.min.y.max(0.0).floor() as i32;
        let x1 = r.max.x.min(w).ceil() as i32;
        let y1 = r.max.y.min(h).ceil() as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                if inside(x as f32 + 0.5, y as f32 + 0.5) {
                    self.blend(x, y, c, 1.0);
                }
            }
        }
    }

    fn rounded(&mut self, r: Rect, radius: f32, c: Rgba8) {
        let rad = radius.min(r.size().min_element() * 0.5).max(0.0);
        self.fill_where(r, c, |x, y| {
            let cx = x.clamp(r.min.x + rad, r.max.x - rad);
            let cy = y.clamp(r.min.y + rad, r.max.y - rad);
            (x - cx).powi(2) + (y - cy).powi(2) <= rad * rad + 1e-3
                && x >= r.min.x
                && x <= r.max.x
                && y >= r.min.y
                && y <= r.max.y
        });
    }

    fn ellipse(&mut self, r: Rect, c: Rgba8) {
        let ctr = r.center();
        let half = (r.size() * 0.5).max(Vec2::splat(0.5));
        self.fill_where(r, c, |x, y| {
            ((x - ctr.x) / half.x).powi(2) + ((y - ctr.y) / half.y).powi(2) <= 1.0
        });
    }

    fn line(&mut self, a: Vec2, b: Vec2, width: f32, c: Rgba8) {
        let hw = (width * 0.5).max(0.6);
        let r = Rect::from_points(a, b).expand(hw + 1.0);
        self.fill_where(r, c, |x, y| {
            board_core::geom::distance_to_segment(Vec2::new(x, y), a, b) <= hw
        });
    }

    fn outline_rounded(&mut self, r: Rect, radius: f32, width: f32, c: Rgba8) {
        let w = width.max(1.0);
        let rad = radius.min(r.size().min_element() * 0.5).max(0.0);
        let inner = r.expand(-w);
        let irad = (rad - w).max(0.0);
        let inside = |r: &Rect, rad: f32, x: f32, y: f32| {
            let cx = x.clamp(r.min.x + rad, r.max.x - rad);
            let cy = y.clamp(r.min.y + rad, r.max.y - rad);
            x >= r.min.x
                && x <= r.max.x
                && y >= r.min.y
                && y <= r.max.y
                && (x - cx).powi(2) + (y - cy).powi(2) <= rad * rad + 1e-3
        };
        self.fill_where(r, c, |x, y| {
            inside(&r, rad, x, y)
                && !(inner.size().min_element() > 0.0 && inside(&inner, irad, x, y))
        });
    }
}

/// 文字を灰色の横線で表す(行の長さは文字数から決める)。
fn greek_text(cv: &mut Canvas, text: &str, area: Rect, font_px: f32, color: Rgba8, center: bool) {
    if text.trim().is_empty() || font_px < 1.5 {
        return;
    }
    let line_h = font_px * 1.4;
    let bar_h = (font_px * 0.5).max(1.0);
    let max_w = area.size().x;
    let char_w = font_px * 0.9;
    let mut y = area.min.y + (line_h - bar_h) * 0.5;
    let mut lines: Vec<f32> = Vec::new();
    for l in text.lines() {
        let n = l.chars().count() as f32;
        let mut w = n * char_w;
        if w == 0.0 {
            lines.push(0.0);
        }
        while w > 0.0 {
            lines.push(w.min(max_w));
            w -= max_w;
        }
    }
    let total = lines.len() as f32 * line_h;
    if center {
        y += ((area.size().y - total) * 0.5).max(0.0);
    }
    let c = Rgba8([
        color.0[0],
        color.0[1],
        color.0[2],
        (color.0[3] as f32 * 0.55) as u8,
    ]);
    for w in lines {
        if y + bar_h > area.max.y {
            break;
        }
        if w > 0.0 {
            let x = if center {
                area.center().x - w * 0.5
            } else {
                area.min.x
            };
            cv.rounded(
                Rect::new(Vec2::new(x, y), Vec2::new(x + w, y + bar_h)),
                bar_h * 0.5,
                c,
            );
        }
        y += line_h;
    }
}

fn draw_board(cv: &mut Canvas, board: &Board, area: Rect) {
    let Some(bounds) = board.bounds() else {
        return;
    };
    let pad = 40.0 * SS as f32;
    let avail = (area.size() - Vec2::splat(pad * 2.0)).max(Vec2::ONE);
    let size = bounds.size().max(Vec2::ONE);
    // 小さなボードを拡大しすぎない。
    let scale = (avail / size).min_element().min(1.5 * SS as f32);
    let offset = area.center() - bounds.center() * scale;
    let to = |p: Vec2| p * scale + offset;
    let rect = |r: Rect| Rect::new(to(r.min), to(r.max));

    for e in board.elements() {
        match e {
            Element::Shape(s) => {
                let r = rect(s.bounds());
                let radius = s.corner_radius * scale;
                match s.kind {
                    ShapeKind::Frame => {
                        cv.rounded(r, 6.0, Rgba8::hex(0xffffff));
                        cv.outline_rounded(r, 6.0, 1.5 * SS as f32, Rgba8::hex(0xd5d8e0));
                        // 名前の位置。
                        let tr = rect(s.text_rect());
                        greek_text(cv, &s.text, tr, (16.0 * scale).max(3.0), INK, false);
                        continue;
                    }
                    ShapeKind::Sticky => {
                        let sh =
                            Rect::new(r.min + Vec2::new(0.0, 3.0), r.max + Vec2::new(0.0, 3.0));
                        cv.rounded(sh, radius, Rgba8([16, 20, 40, 22]));
                        cv.rounded(r, radius, s.fill);
                    }
                    ShapeKind::Ellipse => {
                        cv.ellipse(r, s.fill);
                        if s.stroke.0[3] > 0 && s.stroke_width > 0.0 {
                            // 細い輪郭は外側の楕円との差で描く。
                            let w = (s.stroke_width * scale).max(1.0);
                            let outer = r;
                            let inner = r.expand(-w);
                            let (co, ho) = (outer.center(), outer.size() * 0.5);
                            let hi = (inner.size() * 0.5).max(Vec2::splat(0.1));
                            cv.fill_where(outer, s.stroke, |x, y| {
                                let o = ((x - co.x) / ho.x).powi(2) + ((y - co.y) / ho.y).powi(2);
                                let i = ((x - co.x) / hi.x).powi(2) + ((y - co.y) / hi.y).powi(2);
                                o <= 1.0 && i > 1.0
                            });
                        }
                    }
                    ShapeKind::Image => {
                        cv.rounded(r, 2.0, Rgba8::hex(0xdfe2ea));
                    }
                    // 角丸の四角以外(ひし形・六角形・雲など)は、内外判定で塗る。
                    kind if !matches!(kind, ShapeKind::Rect | ShapeKind::Text) => {
                        let has_stroke = s.stroke.0[3] > 0 && s.stroke_width > 0.0;
                        let w = if has_stroke {
                            (s.stroke_width * scale).max(1.0)
                        } else {
                            0.0
                        };
                        if has_stroke {
                            cv.fill_where(r, s.stroke, |x, y| {
                                board_core::shape::kind_contains(kind, &r, Vec2::new(x, y))
                            });
                        }
                        let inner = r.expand(-w);
                        if s.fill.0[3] > 0 || has_stroke {
                            let fill = if s.fill.0[3] > 0 { s.fill } else { BG };
                            cv.fill_where(inner, fill, |x, y| {
                                board_core::shape::kind_contains(kind, &inner, Vec2::new(x, y))
                            });
                        }
                    }
                    _ => {
                        if s.fill.0[3] > 0 {
                            cv.rounded(r, radius, s.fill);
                        }
                        if s.stroke.0[3] > 0 && s.stroke_width > 0.0 {
                            cv.outline_rounded(r, radius, s.stroke_width * scale, s.stroke);
                        }
                    }
                }
                if let Some(tb) = e.text_box() {
                    let fr = rect(tb.rect).expand(-2.0);
                    greek_text(
                        cv,
                        &s.text,
                        fr,
                        tb.style.font_size * scale,
                        tb.style.color,
                        tb.valign == board_core::shape::VAlign::Middle,
                    );
                }
            }
            Element::Stroke(st) => {
                let w = st.width * scale;
                let pts: Vec<Vec2> = st.points.iter().map(|p| to(*p)).collect();
                match pts.as_slice() {
                    [a] => cv.line(*a, *a, w, st.color),
                    _ => {
                        for seg in pts.windows(2) {
                            cv.line(seg[0], seg[1], w, st.color);
                        }
                    }
                }
            }
            Element::Connector(c) => {
                let w = (c.width * scale).max(1.0);
                for seg in c.path.windows(2) {
                    cv.line(to(seg[0]), to(seg[1]), w, c.color);
                }
                for (at_end, cap) in [(false, c.start_cap), (true, c.end_cap)] {
                    if cap == board_core::Cap::None {
                        continue;
                    }
                    if let Some(lines) = c.arrow_lines(at_end) {
                        for (a, b) in lines {
                            cv.line(to(a), to(b), w, c.color);
                        }
                    }
                }
            }
        }
    }
}

/// 帯に文字を描く(フォントがなければ何もしない)。
fn draw_text(
    cv: &mut Canvas,
    font: &FontArc,
    text: &str,
    pos: Vec2,
    px: f32,
    max_w: f32,
    color: Rgba8,
) {
    let scaled = font.as_scaled(PxScale::from(px));
    let mut x = pos.x;
    let baseline = pos.y + scaled.ascent();
    let ellipsis_w = scaled.h_advance(font.glyph_id('…')) * 1.0;
    let chars: Vec<char> = text.chars().collect();
    let mut prev = None;
    for (i, ch) in chars.iter().enumerate() {
        let id = font.glyph_id(*ch);
        if let Some(p) = prev {
            x += scaled.kern(p, id);
        }
        let adv = scaled.h_advance(id);
        let rest_fits = {
            let rest: f32 = chars[i..]
                .iter()
                .map(|c| scaled.h_advance(font.glyph_id(*c)))
                .sum();
            x + rest <= pos.x + max_w
        };
        if !rest_fits && x + adv + ellipsis_w > pos.x + max_w {
            draw_glyph(cv, font, '…', px, Vec2::new(x, baseline), color);
            return;
        }
        draw_glyph(cv, font, *ch, px, Vec2::new(x, baseline), color);
        x += adv;
        prev = Some(id);
    }
}

fn draw_glyph(cv: &mut Canvas, font: &FontArc, ch: char, px: f32, at: Vec2, color: Rgba8) {
    let glyph = font
        .glyph_id(ch)
        .with_scale_and_position(PxScale::from(px), ab_glyph::point(at.x, at.y));
    if let Some(outlined) = font.outline_glyph(glyph) {
        let b = outlined.px_bounds();
        outlined.draw(|gx, gy, cov| {
            cv.blend(
                b.min.x as i32 + gx as i32,
                b.min.y as i32 + gy as i32,
                color,
                cov,
            );
        });
    }
}

/// OGP 画像を描いて PNG にする。
pub fn render(board: &Board, title: &str, font: Option<&FontArc>) -> Vec<u8> {
    let (w, h) = (WIDTH * SS, HEIGHT * SS);
    let mut cv = Canvas::new(w, h, BG);
    let footer_top = (HEIGHT - FOOTER) * SS;
    let area = Rect::new(Vec2::ZERO, Vec2::new(w as f32, footer_top as f32));
    draw_board(&mut cv, board, area);
    if board.is_empty() {
        greek_text(
            &mut cv,
            "まだ何もありません",
            area.expand(-(200.0 * SS as f32)),
            40.0,
            MUTED,
            true,
        );
    }
    let mut img = image::imageops::resize(
        &cv.img,
        WIDTH,
        HEIGHT,
        image::imageops::FilterType::Triangle,
    );

    // 下の帯は縮小後に描く(文字をくっきりさせる)。
    let mut cv = Canvas {
        img: std::mem::take(&mut img),
    };
    let ft = (HEIGHT - FOOTER) as f32;
    cv.fill_where(
        Rect::new(Vec2::new(0.0, ft), Vec2::new(WIDTH as f32, HEIGHT as f32)),
        FOOTER_BG,
        |_, _| true,
    );
    cv.line(
        Vec2::new(0.0, ft),
        Vec2::new(WIDTH as f32, ft),
        1.0,
        Rgba8::hex(0xe3e6ec),
    );
    // ロゴ(グラデーションの代わりにアクセント色の角丸)。
    let logo = Rect::new(Vec2::new(40.0, ft + 28.0), Vec2::new(80.0, ft + 68.0));
    cv.rounded(logo, 12.0, ACCENT);
    cv.ellipse(
        Rect::new(
            logo.min + Vec2::new(8.0, 8.0),
            logo.min + Vec2::new(20.0, 20.0),
        ),
        Rgba8([255, 255, 255, 140]),
    );
    if let Some(font) = font {
        draw_text(
            &mut cv,
            font,
            title,
            Vec2::new(100.0, ft + 18.0),
            34.0,
            WIDTH as f32 - 360.0,
            INK,
        );
        draw_text(
            &mut cv,
            font,
            "グラボードで公開中",
            Vec2::new(100.0, ft + 58.0),
            18.0,
            400.0,
            MUTED,
        );
    }

    let mut out = Vec::new();
    let _ = image::codecs::png::PngEncoder::new(&mut out).write_image(
        cv.img.as_raw(),
        WIDTH,
        HEIGHT,
        image::ExtendedColorType::Rgba8,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_png() {
        let png = render(&Board::welcome(), "企画", None);
        assert_eq!(&png[1..4], b"PNG");
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (WIDTH, HEIGHT));
        // 何か描かれている(背景だけではない)。
        let rgba = img.to_rgba8();
        assert!(
            rgba.pixels()
                .any(|p| p.0[..3] != BG.0[..3] && p.0[..3] != FOOTER_BG.0[..3])
        );
        // 空のボードでも描ける。
        assert!(!render(&Board::new(), "", None).is_empty());
    }
}
