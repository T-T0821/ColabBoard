//! 画像・PDF・マークダウンの挿入(貼り付け・ドロップ・ファイル選択)。
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

/// PDF の取り込み部品(別の wasm、`crates/board-pdf`)を Web Worker で動かす。
/// 本体の wasm に入れると約 3 MB 重くなるので、PDF が貼られたときに初めて読み込む。
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function render_pdf_in_worker(url, bytes) {
  // bytes は wasm のメモリを指すので、先に写しておく。
  const copy = bytes.slice();
  return new Promise((resolve, reject) => {
    let w;
    try { w = new Worker(url); } catch (e) { reject(String(e)); return; }
    const done = (f, v) => { w.terminate(); f(v); };
    w.onmessage = (e) => {
      const d = e.data || {};
      if (d.ready) { w.postMessage(copy, [copy.buffer]); return; }
      if (d.error) done(reject, d.error);
      else done(resolve, d.pages || []);
    };
    w.onerror = (e) => { e.preventDefault(); done(reject, 'PDF の読み込み部品を起動できません'); };
  });
}
"#)]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(catch)]
    fn render_pdf_in_worker(
        url: &str,
        bytes: &[u8],
    ) -> Result<js_sys::Promise, wasm_bindgen::JsValue>;
}

/// PDF の各ページを JPEG にする(Web Worker で)。
async fn render_pdf(bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let err = |e: wasm_bindgen::JsValue| {
        e.as_string()
            .unwrap_or_else(|| "PDF を読めません".to_string())
    };
    let promise = render_pdf_in_worker("board-pdf_loader.js", bytes).map_err(err)?;
    let pages = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(err)?;
    Ok(js_sys::Array::from(&pages)
        .iter()
        .map(|p| js_sys::Uint8Array::new(&p).to_vec())
        .collect())
}

/// PDF を読み込み、各ページを横に並べて置く。
async fn insert_pdf(app: &Rc<RefCell<App>>, bytes: &[u8], at: Vec2) {
    if !app.borrow().can_edit() {
        return;
    }
    match render_pdf(bytes).await {
        Ok(pages) => app.borrow_mut().insert_pdf_pages(pages, at),
        Err(e) => {
            log::warn!("{e}");
            super::show_error(&e);
        }
    }
}

impl App {
    /// PDF の各ページ(JPEG)を横に並べて置く。
    fn insert_pdf_pages(&mut self, pages: Vec<Vec<u8>>, at: Vec2) {
        if !self.can_edit() {
            return;
        }
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

    /// 取り込んだマークダウンの画像の仮置きを、読み込んだ画像に差し替える。
    fn replace_placeholder(&mut self, placeholder: board_core::ElementId, bytes: &[u8]) {
        let Some(rect) = self.board.get(placeholder).map(|e| e.bounds()) else {
            return;
        };
        let Ok((data, w, h)) = prepare_image(bytes) else {
            return;
        };
        let width = rect.size().x;
        let size = Vec2::new(width, width * h as f32 / w as f32);
        let id = self.board.alloc_id();
        self.board.end_gesture();
        self.board.remove(&[placeholder]);
        self.board
            .push(Shape::image(id, rect.min, size, ImageData::new(data)));
        self.board.end_gesture();
        let mut sel = self.editor.selection().to_vec();
        if let Some(p) = sel.iter_mut().find(|p| **p == placeholder) {
            *p = id;
            self.editor.set_selection(sel);
        }
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
            let base = {
                let a = app.borrow();
                at.unwrap_or_else(|| a.cursor_world.unwrap_or_else(|| a.media_center()))
            };
            let base = base + Vec2::splat(32.0 * i as f32);
            if super::imports::is_diagram_file(&f.name()) {
                let text = super::imports::decode_text(&bytes);
                if !app.borrow_mut().insert_diagram_text(&text) {
                    super::show_error(
                        "Mermaid / PlantUML / draw.io / Miro の図として読めませんでした",
                    );
                }
            } else if super::imports::is_csv(&f.name(), &f.type_()) {
                app.borrow_mut()
                    .insert_csv(&super::imports::decode_text(&bytes), base);
            } else if super::imports::is_spreadsheet(&f.name()) {
                match super::imports::read_spreadsheet(bytes) {
                    Ok(sheets) => {
                        if !app.borrow_mut().insert_sheets(sheets, base) {
                            super::show_error("シートが空です");
                        }
                    }
                    Err(e) => super::show_error(&e),
                }
            } else if is_markdown(&f) {
                insert_markdown(&app, &String::from_utf8_lossy(&bytes), Some(base));
            } else if is_pdf(&f) {
                insert_pdf(&app, &bytes, base).await;
            } else {
                app.borrow_mut().insert_image(&bytes, base);
            }
        }
    });
}

fn is_pdf(f: &File) -> bool {
    f.type_() == "application/pdf" || f.name().to_ascii_lowercase().ends_with(".pdf")
}

fn is_markdown(f: &File) -> bool {
    let name = f.name().to_ascii_lowercase();
    f.type_() == "text/markdown" || name.ends_with(".md") || name.ends_with(".markdown")
}

fn is_supported(f: &File) -> bool {
    f.type_().starts_with("image/")
        || is_pdf(f)
        || is_markdown(f)
        || super::imports::is_csv(&f.name(), &f.type_())
        || super::imports::is_spreadsheet(&f.name())
        || super::imports::is_diagram_file(&f.name())
}

/// マークダウンを読んで並べる(中心は `at`、なければカーソルか画面中央)。
/// 画像のリンクは仮の四角形で置き、読み込めたものから画像に差し替える。
pub fn insert_markdown(app: &Rc<RefCell<App>>, md: &str, at: Option<Vec2>) -> bool {
    let images = {
        let mut a = app.borrow_mut();
        if !a.can_edit() {
            return false;
        }
        let center = at.unwrap_or_else(|| a.cursor_world.unwrap_or_else(|| a.media_center()));
        let sticky = a.editor.sticky_color;
        a.board.end_gesture();
        let Some(r) = board_core::markdown::insert(&mut a.board, md, center, sticky) else {
            return false;
        };
        a.board.end_gesture();
        a.editor.set_selection(r.ids);
        a.dirty = true;
        r.images
    };
    for img in images {
        let app = app.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let Some(bytes) = fetch_bytes(&img.url).await else {
                return;
            };
            app.borrow_mut()
                .replace_placeholder(img.placeholder, &bytes);
        });
    }
    true
}

/// URL の中身を読む(http(s) と data: だけ。相対パスは元のファイルの場所が分からないので読まない)。
async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    use wasm_bindgen::JsCast;
    let lower = url.to_ascii_lowercase();
    if !["http://", "https://", "data:image/"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return None;
    }
    let window = web_sys::window()?;
    let res = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url))
        .await
        .ok()?;
    let res: web_sys::Response = res.dyn_into().ok()?;
    if !res.ok() {
        return None;
    }
    let buf = wasm_bindgen_futures::JsFuture::from(res.array_buffer().ok()?)
        .await
        .ok()?;
    Some(js_sys::Uint8Array::new(&buf).to_vec())
}

fn file_list(list: Option<web_sys::FileList>) -> Vec<File> {
    let Some(list) = list else {
        return Vec::new();
    };
    (0..list.length()).filter_map(|i| list.get(i)).collect()
}

/// 貼り付け・ドロップされたデータに画像・PDF・マークダウンのファイルがあれば取り出す。
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
            let at = {
                let a = app.borrow();
                let p = a.css_to_physical(e.client_x() as f64, e.client_y() as f64);
                a.camera.screen_to_world(p)
            };
            let files = image_files(e.data_transfer());
            if files.is_empty() {
                // ほかのアプリから引きずってきたマークダウンの文字列。
                let text = e
                    .data_transfer()
                    .and_then(|d| d.get_data("text/plain").ok())
                    .unwrap_or_default();
                if board_core::markdown::looks_like_markdown(&text) {
                    e.prevent_default();
                    insert_markdown(&app, &text, Some(at));
                }
                return;
            }
            e.prevent_default();
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
