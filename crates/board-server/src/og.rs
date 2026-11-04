//! 公開ページの OGP 画像(1200×630 の PNG)を CPU で描く。
//!
//! サーバーには GPU がないので、CLI・書き出し API と同じ [`board_export`] で CPU で描く
//! (フォントがあれば文字も描き、小さすぎる文字は灰色の線で表す)。
//! 2 倍の大きさで描いてから縮小し、輪郭を滑らかにする。
//! 下の帯にボード名を描く(フォントがあれば)。

use ab_glyph::FontArc;
use board_core::geom::Rect;
use board_core::shape::{TextAlign, VAlign};
use board_core::{Board, Rgba8, Vec2};
use board_export::{Canvas, Style, View};
use image::ImageEncoder;

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

/// ボードを `area` に収めて描く(描画は CLI・書き出し API と同じ [`board_export`])。
fn draw_board(cv: &mut Canvas, board: &Board, area: Rect, font: Option<&FontArc>) {
    // 公開ページでも個人作業エリアの中身は伏せる。
    let elements = board_export::visible_elements(board, true);
    let Some(bounds) = board_export::content_bounds(&elements) else {
        return;
    };
    let pad = 40.0 * SS as f32;
    let avail = (area.size() - Vec2::splat(pad * 2.0)).max(Vec2::ONE);
    let size = bounds.size().max(Vec2::ONE);
    // 小さなボードを拡大しすぎない。
    let scale = (avail / size).min_element().min(1.5 * SS as f32);
    let view = View {
        scale,
        offset: area.center() - bounds.center() * scale,
    };
    board_export::draw_elements(
        cv,
        &elements,
        view,
        Style {
            font,
            paper: Rgba8::WHITE,
        },
    );
}

/// OGP 画像を描いて PNG にする。
pub fn render(board: &Board, title: &str, font: Option<&FontArc>) -> Vec<u8> {
    let (w, h) = (WIDTH * SS, HEIGHT * SS);
    let mut cv = Canvas::new(w, h, Some(BG));
    let footer_top = (HEIGHT - FOOTER) * SS;
    let area = Rect::new(Vec2::ZERO, Vec2::new(w as f32, footer_top as f32));
    draw_board(&mut cv, board, area, font);
    if board.is_empty() {
        board_export::text::greek(
            &mut cv,
            "まだ何もありません",
            area.expand(-(200.0 * SS as f32)),
            40.0,
            56.0,
            MUTED,
            TextAlign::Center,
            VAlign::Middle,
        );
    }
    let img = image::imageops::resize(
        &cv.img,
        WIDTH,
        HEIGHT,
        image::imageops::FilterType::Triangle,
    );

    // 下の帯は縮小後に描く(文字をくっきりさせる)。
    let mut cv = Canvas { img };
    let ft = (HEIGHT - FOOTER) as f32;
    cv.rect(
        Rect::new(Vec2::new(0.0, ft), Vec2::new(WIDTH as f32, HEIGHT as f32)),
        FOOTER_BG,
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
        board_export::text::draw_ellipsized(
            &mut cv,
            font,
            title,
            Vec2::new(100.0, ft + 18.0),
            34.0,
            WIDTH as f32 - 360.0,
            INK,
        );
        board_export::text::draw_ellipsized(
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
