//! 文字を描く。フォントがあれば字形で、なければ(または小さすぎて読めなければ)
//! 灰色の横線で「文字がある」ことだけを表す。

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use board_core::geom::Rect;
use board_core::shape::{TextAlign, TextBox, VAlign};
use board_core::{Rgba8, Vec2};

use crate::canvas::Canvas;

/// これより小さい文字(画素)は字形を描かずに横線にする。
const MIN_GLYPH_PX: f32 = 5.0;

/// 1 行(文字と幅)。
struct Line {
    text: String,
    width: f32,
}

/// 幅 `max_w` で折り返す。英単語の途中ではなるべく切らない。
fn wrap(font: &FontArc, px: f32, text: &str, max_w: f32) -> Vec<Line> {
    let scaled = font.as_scaled(PxScale::from(px));
    let adv = |c: char| scaled.h_advance(font.glyph_id(c));
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let mut width = 0.0f32;
        for ch in para.chars() {
            let w = adv(ch);
            if width + w > max_w && !line.is_empty() {
                // 直前の空白で切れるなら、そこで切る(英文)。
                let cut = if ch.is_ascii_alphanumeric() {
                    line.rfind(' ').filter(|&i| i > 0)
                } else {
                    None
                };
                match cut {
                    Some(i) => {
                        let rest = line[i + 1..].to_string();
                        line.truncate(i);
                        let lw = line.chars().map(adv).sum();
                        out.push(Line {
                            text: std::mem::take(&mut line),
                            width: lw,
                        });
                        width = rest.chars().map(adv).sum();
                        line = rest;
                    }
                    None => {
                        out.push(Line {
                            text: std::mem::take(&mut line),
                            width,
                        });
                        width = 0.0;
                    }
                }
                // 行頭の空白は捨てる。
                if ch == ' ' && line.is_empty() {
                    continue;
                }
            }
            line.push(ch);
            width += w;
        }
        out.push(Line { text: line, width });
    }
    out
}

/// 文字の箱を画面の座標(`to` で変換)に描く。`scale` は 1 ワールド単位の画素数。
pub fn draw_box(
    cv: &mut Canvas,
    font: Option<&FontArc>,
    text: &str,
    tb: &TextBox,
    to: impl Fn(Vec2) -> Vec2,
    scale: f32,
) {
    if text.trim().is_empty() {
        return;
    }
    let rect = Rect::new(to(tb.rect.min), to(tb.rect.max));
    let clip = Rect::new(to(tb.clip.min), to(tb.clip.max));
    let px = tb.style.font_size * scale;
    let line_h = (tb.style.line_height * scale).max(px);
    let font = match font {
        Some(f) if px >= MIN_GLYPH_PX => f,
        _ => {
            greek(
                cv,
                text,
                rect,
                px,
                line_h,
                tb.style.color,
                tb.style.align,
                tb.valign,
            );
            return;
        }
    };
    if tb.vertical {
        draw_vertical(
            cv,
            font,
            text,
            rect,
            clip,
            px,
            line_h,
            tb.style.color,
            tb.valign,
        );
        return;
    }
    let lines = wrap(font, px, text, rect.size().x.max(px));
    let total = lines.len() as f32 * line_h;
    let mut y = match tb.valign {
        VAlign::Top => rect.min.y,
        VAlign::Middle => rect.center().y - total * 0.5,
        VAlign::Bottom => rect.max.y - total,
    };
    let scaled = font.as_scaled(PxScale::from(px));
    let glyph_h = scaled.ascent() - scaled.descent();
    for line in &lines {
        if y > clip.max.y {
            break;
        }
        let x = match tb.style.align {
            TextAlign::Left => rect.min.x,
            TextAlign::Center => rect.center().x - line.width * 0.5,
            TextAlign::Right => rect.max.x - line.width,
        };
        let baseline = y + (line_h - glyph_h) * 0.5 + scaled.ascent();
        draw_run(
            cv,
            font,
            &line.text,
            Vec2::new(x, baseline),
            px,
            tb.style.color,
            clip,
        );
        y += line_h;
    }
}

/// 縦書き: 右の列から左へ、上から下へ。
#[allow(clippy::too_many_arguments)]
fn draw_vertical(
    cv: &mut Canvas,
    font: &FontArc,
    text: &str,
    rect: Rect,
    clip: Rect,
    px: f32,
    col_w: f32,
    color: Rgba8,
    valign: VAlign,
) {
    let step = px * 1.05;
    let per_col = ((rect.size().y / step).floor() as usize).max(1);
    let mut cols: Vec<Vec<char>> = Vec::new();
    for para in text.split('\n') {
        let chars: Vec<char> = para.chars().collect();
        if chars.is_empty() {
            cols.push(Vec::new());
        }
        for chunk in chars.chunks(per_col) {
            cols.push(chunk.to_vec());
        }
    }
    let total_w = cols.len() as f32 * col_w;
    // 縦書きの「縦方向の揃え」は横の位置(上 = 右寄せ)。
    let right = match valign {
        VAlign::Top => rect.max.x,
        VAlign::Middle => rect.center().x + total_w * 0.5,
        VAlign::Bottom => rect.min.x + total_w,
    };
    let scaled = font.as_scaled(PxScale::from(px));
    for (i, col) in cols.iter().enumerate() {
        let cx = right - col_w * (i as f32 + 0.5);
        for (j, ch) in col.iter().enumerate() {
            let adv = scaled.h_advance(font.glyph_id(*ch));
            let top = rect.min.y + step * j as f32;
            let at = Vec2::new(cx - adv * 0.5, top + scaled.ascent());
            draw_glyph(cv, font, *ch, px, at, color, clip);
        }
    }
}

/// 1 行を描く(`at` は左端のベースライン)。
pub fn draw_run(
    cv: &mut Canvas,
    font: &FontArc,
    text: &str,
    at: Vec2,
    px: f32,
    color: Rgba8,
    clip: Rect,
) {
    let scaled = font.as_scaled(PxScale::from(px));
    let mut x = at.x;
    let mut prev = None;
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        if let Some(p) = prev {
            x += scaled.kern(p, id);
        }
        if x > clip.max.x {
            break;
        }
        draw_glyph(cv, font, ch, px, Vec2::new(x, at.y), color, clip);
        x += scaled.h_advance(id);
        prev = Some(id);
    }
}

/// 幅 `max_w` に収まらなければ「…」で切る 1 行(帯の見出しなど)。
pub fn draw_ellipsized(
    cv: &mut Canvas,
    font: &FontArc,
    text: &str,
    top_left: Vec2,
    px: f32,
    max_w: f32,
    color: Rgba8,
) {
    let scaled = font.as_scaled(PxScale::from(px));
    let adv = |c: char| scaled.h_advance(font.glyph_id(c));
    let full: f32 = text.chars().map(adv).sum();
    let mut shown = String::new();
    if full <= max_w {
        shown.push_str(text);
    } else {
        let limit = max_w - adv('…');
        let mut w = 0.0;
        for c in text.chars() {
            if w + adv(c) > limit {
                break;
            }
            w += adv(c);
            shown.push(c);
        }
        shown.push('…');
    }
    let clip = cv.bounds();
    draw_run(
        cv,
        font,
        &shown,
        Vec2::new(top_left.x, top_left.y + scaled.ascent()),
        px,
        color,
        clip,
    );
}

fn draw_glyph(
    cv: &mut Canvas,
    font: &FontArc,
    ch: char,
    px: f32,
    at: Vec2,
    color: Rgba8,
    clip: Rect,
) {
    let glyph = font
        .glyph_id(ch)
        .with_scale_and_position(PxScale::from(px), ab_glyph::point(at.x, at.y));
    if let Some(outlined) = font.outline_glyph(glyph) {
        let b = outlined.px_bounds();
        outlined.draw(|gx, gy, cov| {
            let x = b.min.x as i32 + gx as i32;
            let y = b.min.y as i32 + gy as i32;
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if fx >= clip.min.x && fx <= clip.max.x && fy >= clip.min.y && fy <= clip.max.y {
                cv.blend(x, y, color, cov);
            }
        });
    }
}

/// 文字を灰色の横線で表す(行の長さは文字数から決める)。
#[allow(clippy::too_many_arguments)]
pub fn greek(
    cv: &mut Canvas,
    text: &str,
    area: Rect,
    font_px: f32,
    line_h: f32,
    color: Rgba8,
    align: TextAlign,
    valign: VAlign,
) {
    if text.trim().is_empty() || font_px < 1.5 {
        return;
    }
    let bar_h = (font_px * 0.5).max(1.0);
    let max_w = area.size().x.max(1.0);
    let mut lines: Vec<f32> = Vec::new();
    for l in text.lines() {
        let mut w = board_core::shape::estimate_text_width(l, font_px);
        if w == 0.0 {
            lines.push(0.0);
        }
        while w > 0.0 {
            lines.push(w.min(max_w));
            w -= max_w;
        }
    }
    let total = lines.len() as f32 * line_h;
    let mut y = match valign {
        VAlign::Top => area.min.y,
        VAlign::Middle => area.center().y - total * 0.5,
        VAlign::Bottom => area.max.y - total,
    } + (line_h - bar_h) * 0.5;
    let c = Rgba8([
        color.0[0],
        color.0[1],
        color.0[2],
        (color.0[3] as f32 * 0.55) as u8,
    ]);
    for w in lines {
        if y + bar_h > area.max.y + line_h {
            break;
        }
        if w > 0.0 {
            let x = match align {
                TextAlign::Left => area.min.x,
                TextAlign::Center => area.center().x - w * 0.5,
                TextAlign::Right => area.max.x - w,
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
