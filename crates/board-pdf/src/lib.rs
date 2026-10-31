//! PDF の各ページを JPEG にする(hayro、Pure Rust のラスタライザ)。
//!
//! hayro はクライアントの wasm を約 3 MB 大きくするので、本体とは別の wasm にして Web Worker で動かす。
//! 本体は PDF が貼られたときに初めてこの Worker を起こす(初回表示では読み込まない)。
//! ラスタライズも本体のスレッドの外で走るので、大きな PDF でも画面が止まらない。
//!
//! Worker とのやりとり: 起動すると `{ ready: true }` を送る。`postMessage(Uint8Array)` で PDF を渡すと、
//! `{ pages: [Uint8Array(JPEG), ...] }` か `{ error: "..." }` が返る。

/// 取り込む最大ページ数。
pub const MAX_PAGES: usize = 20;
/// ページを画像にするときの長辺(ピクセル)。
const PIXELS: f32 = 1400.0;

/// PDF の各ページを JPEG にする。
pub fn render(bytes: Vec<u8>) -> Result<Vec<Vec<u8>>, String> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::Pdf;
    use hayro::vello_cpu::color::palette::css::WHITE;
    let pdf = Pdf::new(bytes).map_err(|e| format!("PDF を読めません: {e:?}"))?;
    let settings = InterpreterSettings::default();
    let cache = hayro::RenderCache::new();
    let mut out = Vec::new();
    for page in pdf.pages().iter().take(MAX_PAGES) {
        let (w, h) = page.render_dimensions();
        let scale = (PIXELS / w.max(h).max(1.0)).min(4.0);
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

#[cfg(target_arch = "wasm32")]
mod worker {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

    #[wasm_bindgen(start)]
    pub fn start() {
        let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
        let reply_to = scope.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let bytes = js_sys::Uint8Array::new(&e.data()).to_vec();
            let reply = js_sys::Object::new();
            match super::render(bytes) {
                Ok(pages) => {
                    let list = js_sys::Array::new();
                    for p in &pages {
                        list.push(&js_sys::Uint8Array::from(p.as_slice()));
                    }
                    let _ = js_sys::Reflect::set(&reply, &"pages".into(), &list);
                }
                Err(err) => {
                    let _ = js_sys::Reflect::set(&reply, &"error".into(), &err.into());
                }
            }
            let _ = reply_to.post_message(&reply);
        });
        scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        on_message.forget();
        // 受け取る準備ができたことを知らせる(wasm の初期化は非同期なので、それまでに来た
        // メッセージは取りこぼす)。本体はこれを待ってから PDF を送る。
        let ready = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&ready, &"ready".into(), &true.into());
        let _ = scope.post_message(&ready);
    }
}
