//! PNG / PDF の書き出し。GPU で白背景にオフスクリーン描画して読み戻し、
//! PNG は image クレート、PDF は JPEG を埋め込んだ 1 ページの PDF にする。

use wasm_bindgen::JsCast;

use super::App;

thread_local! {
    /// 読み戻しを待っている書き出しの数。
    static PENDING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// 書き出しの読み戻しを進める。毎フレーム呼ぶ。
pub(super) fn pump(app: &mut App) {
    if PENDING.with(|p| p.get()) > 0 {
        app.renderer.poll();
        app.dirty = true;
    }
}

/// 書き出す画像の長辺の上限(ピクセル)。
const MAX_PX: u32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    Pdf,
}

/// バイト列をファイルとして保存させる。
fn download(name: &str, mime: &str, bytes: &[u8]) -> Result<(), String> {
    let window = web_sys::window().ok_or("window がありません")?;
    let doc = window.document().ok_or("document がありません")?;
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::of1(&array);
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts)
        .map_err(super::js_err)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(super::js_err)?;
    let a: web_sys::HtmlAnchorElement = doc
        .create_element("a")
        .map_err(super::js_err)?
        .dyn_into()
        .map_err(|_| "a 要素を作れません".to_string())?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    // すぐに消すと保存が始まらないことがあるので少し待つ。
    let revoke = wasm_bindgen::closure::Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    let _ = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000);
    Ok(())
}

fn encode(format: ExportFormat, rgba: Vec<u8>, w: u32, h: u32) -> Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("画像の大きさが合いません")?;
    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        ExportFormat::Png => {
            // 背景は白で不透明。
            image::DynamicImage::ImageRgba8(img)
                .to_rgb8()
                .write_to(&mut out, image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            Ok(out.into_inner())
        }
        ExportFormat::Pdf => {
            let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92)
                .encode_image(&rgb)
                .map_err(|e| e.to_string())?;
            Ok(board_core::export::jpeg_pdf(&out.into_inner(), w, h))
        }
    }
}

impl App {
    /// 選択中の要素(なければボード全体)を書き出す。
    pub(super) fn export(&mut self, format: ExportFormat) {
        let sel = self.editor.selection().to_vec();
        let area = if sel.is_empty() {
            self.board.bounds().map(|r| r.expand(48.0))
        } else {
            // フレームの見出しが切れないよう上を広めに取る。
            self.board.bounds_of(&sel).map(|r| {
                board_core::Rect::new(r.min - board_core::Vec2::new(24.0, 48.0), r.max + 24.0)
            })
        };
        let Some(area) = area else {
            super::show_error("書き出すものがありません");
            return;
        };
        let capture = self.renderer.capture(&self.board, area, MAX_PX);
        PENDING.with(|p| p.set(p.get() + 1));
        self.dirty = true;
        let (w, h) = (capture.width, capture.height);
        capture.read(move |rgba| {
            PENDING.with(|p| p.set(p.get().saturating_sub(1)));
            let result = rgba
                .ok_or_else(|| "画像を読み戻せませんでした".to_string())
                .and_then(|rgba| encode(format, rgba, w, h))
                .and_then(|bytes| match format {
                    ExportFormat::Png => download("graboard.png", "image/png", &bytes),
                    ExportFormat::Pdf => download("graboard.pdf", "application/pdf", &bytes),
                });
            if let Err(e) = result {
                log::warn!("書き出しに失敗しました: {e}");
                super::show_error(&format!("書き出しに失敗しました: {e}"));
            }
        });
    }
}
