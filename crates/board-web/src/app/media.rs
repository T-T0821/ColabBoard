//! 画像・PDF の挿入(貼り付け・ドロップ・ファイル選択)。
//!
//! 画像は image クレート(Pure Rust)でデコードし、大きすぎれば縮めて保存し直す。
//! 保存した画像はボードの要素として同期される。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::{ImageData, Shape, Vec2};
use web_sys::{DragEvent, File, HtmlInputElement, MouseEvent};

use super::{App, by_id, listen};

/// 保存する画像の一辺の上限(ピクセル)。
const MAX_PIXELS: u32 = 1600;
/// これより小さい PNG / JPEG はそのまま保存する(バイト)。
const KEEP_BYTES: usize = 600 * 1024;
/// 置いたときの一辺の上限(ワールド単位)。
const MAX_WORLD: f32 = 480.0;

/// 保存用のバイト列と、画像の大きさ(ピクセル)。
pub fn prepare_image(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let format = image::guess_format(bytes).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("画像の大きさが 0 です".into());
    }
    let small = w.max(h) <= MAX_PIXELS && bytes.len() <= KEEP_BYTES;
    if small && matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Ok((bytes.to_vec(), w, h));
    }
    let img = if w.max(h) > MAX_PIXELS {
        img.resize(
            MAX_PIXELS,
            MAX_PIXELS,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let (w, h) = (img.width(), img.height());
    let mut out = std::io::Cursor::new(Vec::new());
    if img.color().has_alpha() {
        img.to_rgba8()
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
    } else {
        let rgb = img.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
            .encode_image(&rgb)
            .map_err(|e| e.to_string())?;
    }
    Ok((out.into_inner(), w, h))
}

/// PDF で取り込む最大ページ数。
#[cfg(feature = "pdf")]
const MAX_PDF_PAGES: usize = 20;
/// PDF のページを画像にするときの長辺(ピクセル)。
#[cfg(feature = "pdf")]
const PDF_PIXELS: f32 = 1400.0;

/// PDF の各ページを JPEG にする(hayro、Pure Rust のラスタライザ)。
#[cfg(not(feature = "pdf"))]
pub fn render_pdf(_bytes: Vec<u8>) -> Result<Vec<Vec<u8>>, String> {
    Err("この版は PDF の取り込みに対応していません".into())
}

/// PDF の各ページを JPEG にする(hayro、Pure Rust のラスタライザ)。
#[cfg(feature = "pdf")]
pub fn render_pdf(bytes: Vec<u8>) -> Result<Vec<Vec<u8>>, String> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::Pdf;
    use hayro::vello_cpu::color::palette::css::WHITE;
    let pdf = Pdf::new(bytes).map_err(|e| format!("PDF を読めません: {e:?}"))?;
    let settings = InterpreterSettings::default();
    let cache = hayro::RenderCache::new();
    let mut out = Vec::new();
    for page in pdf.pages().iter().take(MAX_PDF_PAGES) {
        let (w, h) = page.render_dimensions();
        let scale = (PDF_PIXELS / w.max(h).max(1.0)).min(4.0);
        let pixmap = hayro::render(
            page,
            &cache,
            &settings,
            &hayro::RenderSettings::default(),
            &hayro::PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );
        let (pw, ph) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        let rgba = pixmap.take_rgba8(hayro::vello_cpu::peniko::ImageAlphaType::Alpha);
        let img = image::RgbaImage::from_raw(pw, ph, rgba).ok_or("ページを画像にできません")?;
        let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
        let mut buf = std::io::Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 85)
            .encode_image(&rgb)
            .map_err(|e| e.to_string())?;
        out.push(buf.into_inner());
    }
    if out.is_empty() {
        return Err("PDF にページがありません".into());
    }
    Ok(out)
}

impl App {
    /// PDF の各ページを横に並べて置く。
    pub(super) fn insert_pdf(&mut self, bytes: Vec<u8>, at: Vec2) {
        if !self.can_edit() {
            return;
        }
        let pages = match render_pdf(bytes) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("{e}");
                super::show_error(&e);
                return;
            }
        };
        let mut items = Vec::new();
        for page in pages {
            let Ok((data, w, h)) = prepare_image(&page) else {
                continue;
            };
            let mut size = Vec2::new(w as f32, h as f32);
            size *= MAX_WORLD * 1.25 / size.max_element();
            items.push((data, size));
        }
        const GAP: f32 = 40.0;
        let total: f32 = items.iter().map(|(_, s)| s.x).sum::<f32>()
            + GAP * items.len().saturating_sub(1) as f32;
        let mut x = at.x - total * 0.5;
        let mut ids = Vec::new();
        let mut shapes = Vec::new();
        for (data, size) in items {
            let id = self.board.alloc_id();
            let pos = Vec2::new(x, at.y - size.y * 0.5);
            shapes.push(Shape::image(id, pos, size, ImageData::new(data)).into());
            ids.push(id);
            x += size.x + GAP;
        }
        self.board.end_gesture();
        self.board.push_many(shapes);
        self.board.end_gesture();
        self.editor.set_selection(ids);
        self.dirty = true;
    }

    /// 画像をワールド座標 `at` を中心に置く。
    pub(super) fn insert_image(&mut self, bytes: &[u8], at: Vec2) {
        if !self.can_edit() {
            return;
        }
        let (data, w, h) = match prepare_image(bytes) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("画像を読めません: {e}");
                super::show_error(&format!("画像を読めません: {e}"));
                return;
            }
        };
        let mut size = Vec2::new(w as f32, h as f32);
        let m = size.max_element();
        if m > MAX_WORLD {
            size *= MAX_WORLD / m;
        }
        let id = self.board.alloc_id();
        self.board.end_gesture();
        self.board.push(Shape::image(
            id,
            at - size * 0.5,
            size,
            ImageData::new(data),
        ));
        self.board.end_gesture();
        self.editor.set_selection(vec![id]);
        self.dirty = true;
    }

    /// 画面の中央(ワールド座標)。
    fn media_center(&self) -> Vec2 {
        self.camera.screen_to_world(self.camera.viewport * 0.5)
    }
}

/// ファイルを読み込んで画像として置く(複数なら少しずつずらす)。
pub fn insert_files(app: &Rc<RefCell<App>>, files: Vec<File>, at: Option<Vec2>) {
    let files: Vec<File> = files.into_iter().filter(is_supported).collect();
    if files.is_empty() {
        return;
    }
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        for (i, f) in files.into_iter().enumerate() {
            let Ok(buf) = wasm_bindgen_futures::JsFuture::from(f.array_buffer()).await else {
                continue;
            };
            let bytes = js_sys::Uint8Array::new(&buf).to_vec();
            let mut a = app.borrow_mut();
            let base = at.unwrap_or_else(|| a.cursor_world.unwrap_or_else(|| a.media_center()));
            let base = base + Vec2::splat(32.0 * i as f32);
            if is_pdf(&f) {
                a.insert_pdf(bytes, base);
            } else {
                a.insert_image(&bytes, base);
            }
        }
    });
}

fn is_pdf(f: &File) -> bool {
    f.type_() == "application/pdf" || f.name().to_ascii_lowercase().ends_with(".pdf")
}

fn is_supported(f: &File) -> bool {
    f.type_().starts_with("image/") || is_pdf(f)
}

fn file_list(list: Option<web_sys::FileList>) -> Vec<File> {
    let Some(list) = list else {
        return Vec::new();
    };
    (0..list.length()).filter_map(|i| list.get(i)).collect()
}

/// 貼り付け・ドロップされたデータに画像・PDF のファイルがあれば取り出す。
pub fn image_files(dt: Option<web_sys::DataTransfer>) -> Vec<File> {
    file_list(dt.and_then(|d| d.files()))
        .into_iter()
        .filter(is_supported)
        .collect()
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let (doc, canvas) = {
        let a = app.borrow();
        (a.document.clone(), a.canvas.clone())
    };

    // ファイルを選んで挿入。
    let input: HtmlInputElement = by_id(&doc, "image-file")?;
    {
        let input2 = input.clone();
        let button = by_id::<web_sys::Element>(&doc, "insert-image")?;
        listen(button.as_ref(), "click", false, move |_: MouseEvent| {
            input2.set_value("");
            input2.click();
        })?;
    }
    {
        let app = app.clone();
        let input2 = input.clone();
        listen(input.as_ref(), "change", false, move |_: web_sys::Event| {
            let files = file_list(input2.files());
            let at = Some(app.borrow().media_center());
            insert_files(&app, files, at);
        })?;
    }

    // キャンバスへのドロップ。
    listen(canvas.as_ref(), "dragover", false, |e: DragEvent| {
        e.prevent_default();
    })?;
    {
        let app = app.clone();
        listen(canvas.as_ref(), "drop", false, move |e: DragEvent| {
            let files = image_files(e.data_transfer());
            if files.is_empty() {
                return;
            }
            e.prevent_default();
            let at = {
                let a = app.borrow();
                let p = a.css_to_physical(e.client_x() as f64, e.client_y() as f64);
                a.camera.screen_to_world(p)
            };
            insert_files(&app, files, Some(at));
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 128]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn small_images_are_kept_and_large_ones_shrunk() {
        let small = png(40, 20);
        let (bytes, w, h) = prepare_image(&small).unwrap();
        assert_eq!((bytes, w, h), (small, 40, 20));
        let (_, w, h) = prepare_image(&png(3200, 800)).unwrap();
        assert_eq!((w, h), (1600, 400));
        assert!(prepare_image(b"not an image").is_err());
    }
}
