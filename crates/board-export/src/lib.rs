//! GPU なしでボードを画像・PDF に描く。
//!
//! Web クライアントは GPU(wgpu)で描くが、CLI やサーバーには GPU がないので、同じ
//! [`board_core`] のデータを CPU で塗る。CLI(`graboard`)の書き出し、サーバーの
//! 書き出し API(`/api/v1/boards/{id}/export`)、公開ページの OGP 画像で共用する。
//!
//! ```no_run
//! use board_export::{Format, Options, Target};
//! let board = board_core::Board::welcome();
//! let png = board_export::export(&board, Format::Png, &Target::Board, &Options::default(), None, "企画")?;
//! # Ok::<(), board_export::Error>(())
//! ```

pub mod canvas;
pub mod draw;
pub mod pdf;
pub mod text;

use std::path::{Path, PathBuf};

pub use ab_glyph::FontArc;
use board_core::geom::Rect;
use board_core::{Board, Element, Rgba8, Vec2};
use image::{ImageEncoder, RgbaImage};

pub use canvas::Canvas;
pub use draw::{Style, View, draw_elements, element_bounds};

/// 書き出す形式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Pdf,
}

impl Format {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Format::Png),
            "pdf" => Some(Format::Pdf),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Pdf => "pdf",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Pdf => "application/pdf",
        }
    }
}

/// どこを書き出すか。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Target {
    /// ボード全体を 1 枚に。
    #[default]
    Board,
    /// フレーム 1 つ(名前・スライドの番号(1 始まり)・要素 ID のどれかで指定)。
    Frame(String),
    /// フレームを 1 つ 1 ページに(スライドの順)。PDF 向け。
    Frames,
}

/// 書き出しの設定。
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// 1 ワールド単位を何画素にするか(2 なら Retina 相当)。
    pub scale: f32,
    /// 周りの余白(ワールド単位)。
    pub padding: f32,
    /// 背景色。`None` なら透明(PNG のみ。PDF では白)。
    pub background: Option<Rgba8>,
    /// 1 枚の画素数の上限。超えるときは `scale` を下げて収める。
    pub max_pixels: u64,
    /// 個人作業エリアの中身を伏せる(サーバーで書き出すときなど)。
    pub hide_private: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scale: 2.0,
            padding: 48.0,
            background: Some(Rgba8::WHITE),
            max_pixels: 40_000_000,
            hide_private: false,
        }
    }
}

/// 1 辺の画素数の上限。
pub const MAX_SIDE: u32 = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 描くものがない。
    Empty,
    /// 指定したフレームがない。
    FrameNotFound(String),
    /// フレームが 1 つもない。
    NoFrames,
    /// PNG は 1 枚にしかできない。
    PngNeedsOnePage(usize),
    /// 画像を作れなかった。
    Encode(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Empty => write!(f, "書き出すものがありません"),
            Error::FrameNotFound(name) => write!(f, "フレーム「{name}」が見つかりません"),
            Error::NoFrames => write!(f, "フレームがありません"),
            Error::PngNeedsOnePage(n) => write!(
                f,
                "PNG は 1 枚だけです(フレームが {n} 個あります)。PDF にするか、フレームを 1 つ指定してください"
            ),
            Error::Encode(e) => write!(f, "画像を作れませんでした: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// 書き出す 1 ページ(ワールド座標の範囲と見出し)。
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub title: String,
    pub area: Rect,
}

/// 描く要素(重なり順)。`hide_private` なら個人作業エリアの中身を伏せたカードにする。
pub fn visible_elements(board: &Board, hide_private: bool) -> Vec<Element> {
    let mask = hide_private
        .then(|| board_core::privacy::Mask::new(board, ""))
        .flatten();
    board
        .elements()
        .iter()
        .filter_map(|e| match &mask {
            Some(m) if m.hides(e) => board_core::privacy::face_down(e),
            _ => Some(e.clone()),
        })
        .collect()
}

/// 要素全体の外接矩形(フレームの見出しや線の太さを含む)。
pub fn content_bounds(elements: &[Element]) -> Option<Rect> {
    elements
        .iter()
        .map(element_bounds)
        .reduce(|a, b| a.union(&b))
}

/// フレームの見出しが切れないよう、上を広めに取ったフレームの範囲。
fn frame_area(r: Rect, padding: f32) -> Rect {
    let pad = padding.clamp(0.0, 24.0);
    Rect::new(
        r.min - Vec2::new(pad, board_core::shape::FRAME_TITLE_HEIGHT + pad),
        r.max + Vec2::splat(pad),
    )
}

/// 書き出すページを決める。
pub fn pages(
    board: &Board,
    elements: &[Element],
    target: &Target,
    padding: f32,
) -> Result<Vec<Page>, Error> {
    match target {
        Target::Board => {
            let area = content_bounds(elements).ok_or(Error::Empty)?;
            Ok(vec![Page {
                title: String::new(),
                area: area.expand(padding.max(0.0)),
            }])
        }
        Target::Frame(name) => {
            let slides = board_core::slides::slides(board);
            let key = name.trim();
            let found = key
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| slides.get(i))
                .or_else(|| {
                    board_core::ops::parse_id(key).and_then(|id| slides.iter().find(|s| s.id == id))
                })
                .or_else(|| slides.iter().find(|s| s.title == key))
                .or_else(|| {
                    let k = key.to_lowercase();
                    slides.iter().find(|s| s.title.to_lowercase().contains(&k))
                })
                .ok_or_else(|| Error::FrameNotFound(name.clone()))?;
            Ok(vec![Page {
                title: found.title.clone(),
                area: frame_area(found.rect, padding),
            }])
        }
        Target::Frames => {
            let slides = board_core::slides::slides(board);
            if slides.is_empty() {
                return Err(Error::NoFrames);
            }
            Ok(slides
                .into_iter()
                .map(|s| Page {
                    title: s.title,
                    area: frame_area(s.rect, padding),
                })
                .collect())
        }
    }
}

/// 範囲 `area` に合わせた倍率(画素数の上限に収める)。
pub fn fit_scale(area: Rect, scale: f32, max_pixels: u64) -> f32 {
    let size = area.size().max(Vec2::ONE);
    let mut s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let px = (size.x * s) as f64 * (size.y * s) as f64;
    if px > max_pixels as f64 {
        s *= (max_pixels as f64 / px).sqrt() as f32;
    }
    let side = size.max_element() * s;
    if side > MAX_SIDE as f32 {
        s *= MAX_SIDE as f32 / side;
    }
    s
}

/// 範囲 `area` を描いた画像。
pub fn render_area(
    elements: &[Element],
    area: Rect,
    opts: &Options,
    font: Option<&FontArc>,
) -> RgbaImage {
    let scale = fit_scale(area, opts.scale, opts.max_pixels);
    let w = ((area.size().x * scale).ceil() as u32).clamp(1, MAX_SIDE);
    let h = ((area.size().y * scale).ceil() as u32).clamp(1, MAX_SIDE);
    let mut cv = Canvas::new(w, h, opts.background);
    let paper = opts
        .background
        .filter(|c| c.0[3] == 255)
        .unwrap_or(Rgba8::WHITE);
    draw_elements(
        &mut cv,
        elements,
        View::of_area(area, scale),
        Style { font, paper },
    );
    cv.img
}

pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| Error::Encode(e.to_string()))?;
    Ok(out)
}

/// ボードを書き出す。`title` は PDF の文書名。
pub fn export(
    board: &Board,
    format: Format,
    target: &Target,
    opts: &Options,
    font: Option<&FontArc>,
    title: &str,
) -> Result<Vec<u8>, Error> {
    let elements = visible_elements(board, opts.hide_private);
    let pages = pages(board, &elements, target, opts.padding)?;
    match format {
        Format::Png => {
            let [page] = pages.as_slice() else {
                return Err(Error::PngNeedsOnePage(pages.len()));
            };
            encode_png(&render_area(&elements, page.area, opts, font))
        }
        Format::Pdf => {
            let images: Vec<RgbaImage> = pages
                .iter()
                .map(|p| render_area(&elements, p.area, opts, font))
                .collect();
            let list: Vec<pdf::Page> = images
                .iter()
                .zip(&pages)
                .map(|(img, p)| pdf::Page {
                    image: img,
                    // 1 ワールド単位 = CSS の 1px = 0.75pt。
                    width_pt: p.area.size().x * 0.75,
                    height_pt: p.area.size().y * 0.75,
                })
                .collect();
            Ok(pdf::build(&list, title))
        }
    }
}

/// 日本語を描けるフォントを探して読む。
///
/// 順に: 環境変数 `GRABOARD_FONT`、`extra` のパス、リポジトリ内の Noto Sans JP、
/// よくあるシステムフォントの場所。
pub fn find_font(extra: &[PathBuf]) -> Option<FontArc> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("GRABOARD_FONT") {
        candidates.push(p.into());
    }
    candidates.extend(extra.iter().cloned());
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../board-web/assets/fonts/NotoSansJP-Regular.ttf"),
    );
    for p in [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "C:\\Windows\\Fonts\\YuGothR.ttc",
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ] {
        candidates.push(p.into());
    }
    candidates.iter().find_map(|p| load_font(p).ok())
}

/// フォントファイルを読む(.ttf / .otf / .ttc の最初のフォント)。
pub fn load_font(path: &Path) -> Result<FontArc, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    FontArc::try_from_vec(bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::{Shape, ShapeKind};

    fn sample() -> Board {
        let mut b = Board::new();
        let f = b.alloc_id();
        let mut frame = Shape::new(
            f,
            ShapeKind::Frame,
            Vec2::new(0.0, 0.0),
            Vec2::new(400.0, 300.0),
            Rgba8::TRANSPARENT,
        );
        frame.text = "1. はじめに".into();
        b.push(frame);
        let f2 = b.alloc_id();
        let mut frame2 = Shape::new(
            f2,
            ShapeKind::Frame,
            Vec2::new(600.0, 0.0),
            Vec2::new(400.0, 300.0),
            Rgba8::TRANSPARENT,
        );
        frame2.text = "2. まとめ".into();
        b.push(frame2);
        let s = b.alloc_id();
        let mut st = Shape::new(
            s,
            ShapeKind::Sticky,
            Vec2::new(40.0, 40.0),
            Vec2::new(160.0, 160.0),
            board_core::color::STICKY_COLORS[0],
        );
        st.text = "書き出し".into();
        b.push(st);
        b
    }

    #[test]
    fn png_of_whole_board() {
        let b = sample();
        let png = export(
            &b,
            Format::Png,
            &Target::Board,
            &Options::default(),
            None,
            "",
        )
        .unwrap();
        let img = image::load_from_memory(&png).unwrap();
        let bounds = content_bounds(&visible_elements(&b, false))
            .unwrap()
            .expand(48.0);
        assert_eq!(img.width(), (bounds.size().x * 2.0).ceil() as u32);
        // 付箋の色が描かれている。
        let rgba = img.to_rgba8();
        assert!(rgba.pixels().any(|p| p.0[..3] != [255, 255, 255]));
    }

    #[test]
    fn frames_become_pages() {
        let b = sample();
        let els = visible_elements(&b, false);
        let ps = pages(&b, &els, &Target::Frames, 24.0).unwrap();
        assert_eq!(
            ps.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(),
            ["1. はじめに", "2. まとめ"]
        );
        assert_eq!(
            pages(&b, &els, &Target::Frame("2".into()), 0.0).unwrap()[0].title,
            "2. まとめ"
        );
        assert_eq!(
            pages(&b, &els, &Target::Frame("まとめ".into()), 0.0).unwrap()[0].title,
            "2. まとめ"
        );
        assert!(matches!(
            pages(&b, &els, &Target::Frame("ない".into()), 0.0),
            Err(Error::FrameNotFound(_))
        ));
        let pdf = export(
            &b,
            Format::Pdf,
            &Target::Frames,
            &Options::default(),
            None,
            "t",
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&pdf).contains("/Count 2"));
        assert_eq!(
            export(
                &b,
                Format::Png,
                &Target::Frames,
                &Options::default(),
                None,
                ""
            ),
            Err(Error::PngNeedsOnePage(2))
        );
    }

    #[test]
    fn empty_board_is_an_error() {
        assert_eq!(
            export(
                &Board::new(),
                Format::Png,
                &Target::Board,
                &Options::default(),
                None,
                ""
            ),
            Err(Error::Empty)
        );
    }

    #[test]
    fn huge_area_is_scaled_down() {
        let area = Rect::new(Vec2::ZERO, Vec2::new(100_000.0, 50_000.0));
        let s = fit_scale(area, 2.0, 1_000_000);
        assert!((area.size().x * s) * (area.size().y * s) <= 1_000_001.0);
    }

    #[test]
    fn draws_real_glyphs_with_a_font() {
        let Some(font) = find_font(&[]) else {
            return;
        };
        let mut b = Board::new();
        let id = b.alloc_id();
        let mut t = Shape::new(
            id,
            ShapeKind::Text,
            Vec2::ZERO,
            Vec2::new(300.0, 60.0),
            Rgba8::TRANSPARENT,
        );
        t.text = "日本語のテキスト".into();
        t.font_size = 24.0;
        b.push(t);
        let opts = Options {
            padding: 0.0,
            ..Default::default()
        };
        let els = visible_elements(&b, false);
        let area = pages(&b, &els, &Target::Board, 0.0).unwrap()[0].area;
        let with = render_area(&els, area, &opts, Some(&font));
        let without = render_area(&els, area, &opts, None);
        assert_ne!(with.as_raw(), without.as_raw());
        assert!(with.pixels().filter(|p| p.0[0] < 128).count() > 50);
    }
}
