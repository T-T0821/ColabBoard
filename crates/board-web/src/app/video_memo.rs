//! 動画メモ: カメラとマイクで短い動画を録り、サーバーに保存してボードに置く。
//!
//! - ツールバーの「動画メモ」で録画パネルを開く。最長 [`MAX_SECONDS`] 秒。録り直しもできる。
//! - 「ボードに置く」で動画をサーバー(`POST /api/boards/{id}/media`)に送り、
//!   表紙(最初のコマに再生ボタンと長さを描いた JPEG)の要素を置く。表紙はボードに同期され、
//!   動画そのものは URL で参照する。
//! - ボード上の動画メモをダブルクリック(選択して Enter でも)すると、その場で再生する。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::{Element, ElementId, ImageData, Shape, ShapeKind, Vec2};
use serde::Deserialize;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Blob, BlobEvent, BlobPropertyBag, CanvasRenderingContext2d, Document, HtmlCanvasElement,
    HtmlElement, HtmlVideoElement, MediaRecorder, MediaRecorderOptions, MediaStream,
    MediaStreamConstraints, MediaStreamTrack,
};

use super::{App, by_id, listen, on_click};

/// 録画の最長(秒)。
pub const MAX_SECONDS: f64 = 60.0;
/// 置いたときの幅(ワールド単位)。
const PLACE_WIDTH: f32 = 360.0;
/// 表紙の幅の上限(ピクセル)。
const POSTER_WIDTH: u32 = 640;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Closed,
    /// カメラを準備中。
    Opening,
    Ready,
    Recording,
    /// 録り終えて確認中。
    Review,
    Uploading,
}

#[derive(Deserialize)]
struct Uploaded {
    url: String,
}

struct Recording {
    recorder: MediaRecorder,
    chunks: Rc<RefCell<Vec<Blob>>>,
    started: f64,
    _on_data: Closure<dyn FnMut(BlobEvent)>,
    _on_stop: Closure<dyn FnMut(web_sys::Event)>,
}

pub struct VideoMemo {
    panel: HtmlElement,
    preview: HtmlVideoElement,
    record: HtmlElement,
    place: HtmlElement,
    retake: HtmlElement,
    time: HtmlElement,
    status: HtmlElement,
    player: HtmlElement,
    player_video: HtmlVideoElement,
    stage: Stage,
    stream: Option<MediaStream>,
    recording: Option<Recording>,
    /// 録り終えた動画と、その種類・長さ(秒)。
    result: Option<(Blob, String, f64)>,
    result_url: Option<String>,
    /// 録画を始めたときのコマ(表紙にする)。
    first_frame: Option<HtmlCanvasElement>,
    timer: Option<(i32, Closure<dyn FnMut()>)>,
    /// 再生中の動画メモ。
    playing: Option<ElementId>,
}

impl VideoMemo {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            panel: by_id(document, "video-recorder")?,
            preview: by_id(document, "video-preview")?,
            record: by_id(document, "video-record")?,
            place: by_id(document, "video-place")?,
            retake: by_id(document, "video-retake")?,
            time: by_id(document, "video-time")?,
            status: by_id(document, "video-status")?,
            player: by_id(document, "video-player")?,
            player_video: by_id(document, "video-player-media")?,
            stage: Stage::Closed,
            stream: None,
            recording: None,
            result: None,
            result_url: None,
            first_frame: None,
            timer: None,
            playing: None,
        })
    }

    fn render(&self) {
        let s = self.stage;
        self.panel.set_hidden(s == Stage::Closed);
        let _ = self.panel.set_attribute(
            "data-stage",
            match s {
                Stage::Closed => "closed",
                Stage::Opening => "opening",
                Stage::Ready => "ready",
                Stage::Recording => "recording",
                Stage::Review => "review",
                Stage::Uploading => "uploading",
            },
        );
        self.record
            .set_hidden(!matches!(s, Stage::Ready | Stage::Recording));
        self.record.set_text_content(Some(if s == Stage::Recording {
            "停止"
        } else {
            "録画"
        }));
        let _ = self.record.set_attribute(
            "aria-pressed",
            if s == Stage::Recording {
                "true"
            } else {
                "false"
            },
        );
        self.place
            .set_hidden(!matches!(s, Stage::Review | Stage::Uploading));
        self.retake.set_hidden(s != Stage::Review);
        let _ = self
            .place
            .toggle_attribute_with_force("disabled", s == Stage::Uploading);
        self.place.set_text_content(Some(if s == Stage::Uploading {
            "保存中…"
        } else {
            "ボードに置く"
        }));
    }

    fn set_status(&self, msg: &str, error: bool) {
        self.status.set_text_content(Some(msg));
        self.status.set_hidden(msg.is_empty());
        let _ = self
            .status
            .set_attribute("data-state", if error { "error" } else { "info" });
    }

    fn stop_timer(&mut self) {
        if let Some((id, _)) = self.timer.take()
            && let Some(w) = web_sys::window()
        {
            w.clear_interval_with_handle(id);
        }
    }

    fn release_camera(&mut self) {
        if let Some(s) = self.stream.take() {
            for t in s.get_tracks().iter() {
                if let Ok(t) = t.dyn_into::<MediaStreamTrack>() {
                    t.stop();
                }
            }
        }
        self.preview.set_src_object(None);
    }

    fn clear_result(&mut self) {
        self.result = None;
        if let Some(u) = self.result_url.take() {
            let _ = web_sys::Url::revoke_object_url(&u);
        }
        self.preview.set_src("");
        let _ = self.preview.remove_attribute("src");
        self.preview.set_controls(false);
    }
}

fn format_time(sec: f64) -> String {
    let s = sec.max(0.0).round() as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// 使える録画形式(WebM を優先。Safari は MP4)。
fn pick_mime() -> Option<&'static str> {
    [
        "video/webm;codecs=vp9,opus",
        "video/webm;codecs=vp8,opus",
        "video/webm",
        "video/mp4",
    ]
    .into_iter()
    .find(|m| MediaRecorder::is_type_supported(m))
}

/// 動画の URL に共有リンクの鍵を付ける(`<video>` は Cookie しか送らないので)。
fn playable_url(media: &str) -> String {
    let key = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("key"));
    let base = crate::http::base();
    match key {
        Some(k) => format!("{base}{media}?key={}", crate::http::encode(&k)),
        None => format!("{base}{media}"),
    }
}

/// 表紙を描く: 最初のコマ + 暗めの帯 + 再生ボタン + 長さ。JPEG のバイト列と大きさを返す。
fn draw_poster(
    document: &Document,
    frame: Option<&HtmlCanvasElement>,
    seconds: f64,
) -> Option<(Vec<u8>, u32, u32)> {
    let (w, h) = frame.map_or((640, 360), |f| (f.width().max(1), f.height().max(1)));
    let canvas: HtmlCanvasElement = document.create_element("canvas").ok()?.dyn_into().ok()?;
    canvas.set_width(w);
    canvas.set_height(h);
    let ctx: CanvasRenderingContext2d = canvas.get_context("2d").ok()??.dyn_into().ok()?;
    let (wf, hf) = (w as f64, h as f64);
    ctx.set_fill_style_str("#1b1d24");
    ctx.fill_rect(0.0, 0.0, wf, hf);
    if let Some(f) = frame {
        let _ = ctx.draw_image_with_html_canvas_element(f, 0.0, 0.0);
    }
    // 下の帯(長さを読みやすく)。
    let band = (hf * 0.2).max(36.0);
    let grad = ctx.create_linear_gradient(0.0, hf - band, 0.0, hf);
    let _ = grad.add_color_stop(0.0, "rgba(0,0,0,0)");
    let _ = grad.add_color_stop(1.0, "rgba(0,0,0,0.55)");
    ctx.set_fill_style_canvas_gradient(&grad);
    ctx.fill_rect(0.0, hf - band, wf, band);
    // 再生ボタン。
    let r = wf.min(hf) * 0.13;
    let (cx, cy) = (wf * 0.5, hf * 0.5);
    ctx.begin_path();
    let _ = ctx.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
    ctx.set_fill_style_str("rgba(0,0,0,0.5)");
    ctx.fill();
    ctx.set_line_width((r * 0.08).max(2.0));
    ctx.set_stroke_style_str("rgba(255,255,255,0.9)");
    ctx.stroke();
    ctx.begin_path();
    ctx.move_to(cx - r * 0.32, cy - r * 0.45);
    ctx.line_to(cx + r * 0.5, cy);
    ctx.line_to(cx - r * 0.32, cy + r * 0.45);
    ctx.close_path();
    ctx.set_fill_style_str("#ffffff");
    ctx.fill();
    // 長さ(右下)と見出し(左下)。
    let font = (hf * 0.065).clamp(14.0, 28.0);
    ctx.set_font(&format!(
        "600 {font}px Inter, 'Hiragino Sans', 'Noto Sans JP', sans-serif"
    ));
    ctx.set_text_baseline("bottom");
    ctx.set_fill_style_str("#ffffff");
    ctx.set_text_align("right");
    let _ = ctx.fill_text(&format_time(seconds), wf - font * 0.7, hf - font * 0.5);
    ctx.set_text_align("left");
    let _ = ctx.fill_text(&crate::i18n::tr("動画メモ"), font * 0.7, hf - font * 0.5);

    let url = canvas.to_data_url_with_type("image/jpeg").ok()?;
    let b64 = url.split_once(',')?.1;
    let bin = web_sys::window()?.atob(b64).ok()?;
    let bytes: Vec<u8> = bin.chars().map(|c| c as u32 as u8).collect();
    Some((bytes, w, h))
}

impl App {
    fn video_render(&self) {
        self.video.render();
    }

    /// 録画パネルを開いてカメラを準備する。
    pub(super) fn open_video_recorder(app: &Rc<RefCell<App>>) {
        {
            let mut a = app.borrow_mut();
            if !a.can_edit() {
                return;
            }
            if a.room.is_none() || a.net.is_none() {
                super::show_error("動画メモはサーバーにつながっているボードで使えます");
                return;
            }
            if a.video.stage != Stage::Closed {
                return;
            }
            a.video.stage = Stage::Opening;
            a.video.set_status("カメラを準備しています…", false);
            a.video
                .time
                .set_text_content(Some(&format!("0:00 / {}", format_time(MAX_SECONDS))));
            a.video_render();
        }
        let app = app.clone();
        spawn_local(async move {
            let result = open_camera().await;
            let mut a = app.borrow_mut();
            if a.video.stage != Stage::Opening {
                // 待っている間に閉じられた。
                if let Ok(s) = result {
                    a.video.stream = Some(s);
                    a.video.release_camera();
                }
                return;
            }
            match result {
                Ok(stream) => {
                    a.video.preview.set_muted(true);
                    a.video.preview.set_src_object(Some(&stream));
                    let _ = a.video.preview.play();
                    a.video.stream = Some(stream);
                    a.video.stage = Stage::Ready;
                    a.video.set_status("", false);
                }
                Err(e) => {
                    a.video.stage = Stage::Ready;
                    a.video.set_status(&e, true);
                    a.video.record.set_hidden(true);
                    a.video_render();
                    a.video.record.set_hidden(true);
                    return;
                }
            }
            a.video_render();
        });
    }

    pub(super) fn close_video_recorder(&mut self) {
        if let Some(r) = self.video.recording.take() {
            r.recorder.set_onstop(None);
            r.recorder.set_ondataavailable(None);
            let _ = r.recorder.stop();
        }
        self.video.stop_timer();
        self.video.release_camera();
        self.video.clear_result();
        self.video.first_frame = None;
        self.video.stage = Stage::Closed;
        self.video.set_status("", false);
        self.video_render();
    }

    fn start_recording(app: &Rc<RefCell<App>>) {
        let mut a = app.borrow_mut();
        let Some(stream) = a.video.stream.clone() else {
            return;
        };
        let Some(mime) = pick_mime() else {
            a.video
                .set_status("このブラウザは録画に対応していません", true);
            return;
        };
        let opts = MediaRecorderOptions::new();
        opts.set_mime_type(mime);
        opts.set_video_bits_per_second(1_500_000);
        let recorder =
            match MediaRecorder::new_with_media_stream_and_media_recorder_options(&stream, &opts) {
                Ok(r) => r,
                Err(_) => {
                    a.video.set_status("録画を始められません", true);
                    return;
                }
            };
        // 表紙にするコマを今のうちに写しておく。
        a.video.first_frame = capture_frame(&a.document, &a.video.preview);

        let chunks: Rc<RefCell<Vec<Blob>>> = Rc::default();
        let chunks2 = chunks.clone();
        let on_data = Closure::<dyn FnMut(BlobEvent)>::new(move |e: BlobEvent| {
            if let Some(b) = e.data()
                && b.size() > 0.0
            {
                chunks2.borrow_mut().push(b);
            }
        });
        let app2 = app.clone();
        let on_stop = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            // 記録の後片付けは描画ループの外で(このコールバックを持つ値を捨てるため)。
            let app = app2.clone();
            spawn_local(async move { App::finish_recording(&app) });
        });
        recorder.set_ondataavailable(Some(on_data.as_ref().unchecked_ref()));
        recorder.set_onstop(Some(on_stop.as_ref().unchecked_ref()));
        if recorder.start_with_time_slice(1000).is_err() {
            a.video.set_status("録画を始められません", true);
            return;
        }
        let started = js_sys::Date::now();
        a.video.recording = Some(Recording {
            recorder,
            chunks,
            started,
            _on_data: on_data,
            _on_stop: on_stop,
        });
        a.video.stage = Stage::Recording;
        a.video.set_status("", false);

        // 経過時間の表示と、最長での自動停止。
        let app3 = app.clone();
        let tick = Closure::<dyn FnMut()>::new(move || {
            let Ok(a) = app3.try_borrow() else { return };
            let Some(r) = &a.video.recording else { return };
            let sec = (js_sys::Date::now() - r.started) / 1000.0;
            a.video.time.set_text_content(Some(&format!(
                "{} / {}",
                format_time(sec),
                format_time(MAX_SECONDS)
            )));
            if sec >= MAX_SECONDS {
                let _ = r.recorder.stop();
            }
        });
        if let Some(w) = web_sys::window()
            && let Ok(id) = w.set_interval_with_callback_and_timeout_and_arguments_0(
                tick.as_ref().unchecked_ref(),
                250,
            )
        {
            a.video.timer = Some((id, tick));
        }
        a.video_render();
    }

    fn stop_recording(&mut self) {
        if let Some(r) = &self.video.recording {
            let _ = r.recorder.stop();
        }
    }

    fn finish_recording(app: &Rc<RefCell<App>>) {
        let mut a = app.borrow_mut();
        let Some(r) = a.video.recording.take() else {
            return;
        };
        a.video.stop_timer();
        let seconds = ((js_sys::Date::now() - r.started) / 1000.0).min(MAX_SECONDS);
        let mime = r.recorder.mime_type();
        let mime = if mime.is_empty() {
            "video/webm".to_string()
        } else {
            mime
        };
        let parts = js_sys::Array::new();
        for b in r.chunks.borrow().iter() {
            parts.push(b);
        }
        let bag = BlobPropertyBag::new();
        bag.set_type(&mime);
        let Ok(blob) = Blob::new_with_blob_sequence_and_options(&parts, &bag) else {
            a.video.set_status("録画を保存できません", true);
            a.video.stage = Stage::Ready;
            a.video_render();
            return;
        };
        // 録り終えたらカメラは止め、録ったものを確認できるようにする。
        a.video.release_camera();
        if let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) {
            a.video.preview.set_muted(false);
            a.video.preview.set_src(&url);
            a.video.preview.set_controls(true);
            a.video.result_url = Some(url);
        }
        a.video.time.set_text_content(Some(&format_time(seconds)));
        a.video.result = Some((blob, mime, seconds));
        a.video.stage = Stage::Review;
        a.video_render();
    }

    fn retake_video(app: &Rc<RefCell<App>>) {
        {
            let mut a = app.borrow_mut();
            a.video.clear_result();
            a.video.first_frame = None;
            a.video.stage = Stage::Closed;
        }
        App::open_video_recorder(app);
    }

    /// 録った動画をサーバーに送り、ボードに置く。
    fn place_video(app: &Rc<RefCell<App>>) {
        let (blob, mime, seconds, path, at) = {
            let mut a = app.borrow_mut();
            let Some((blob, mime, seconds)) = a.video.result.clone() else {
                return;
            };
            let Some(room) = a.room.clone() else { return };
            let key = web_sys::window()
                .and_then(|w| w.location().search().ok())
                .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
                .and_then(|p| p.get("key"));
            let path = crate::http::board_path(&room, key.as_deref(), "/media");
            a.video.stage = Stage::Uploading;
            a.video.set_status("", false);
            a.video_render();
            let at = a.camera.screen_to_world(a.camera.viewport * 0.5);
            (blob, mime, seconds, path, at)
        };
        let app = app.clone();
        spawn_local(async move {
            let bytes = match JsFuture::from(blob.array_buffer()).await {
                Ok(buf) => js_sys::Uint8Array::new(&buf).to_vec(),
                Err(_) => Vec::new(),
            };
            let uploaded = if bytes.is_empty() {
                Err("録画が空です".to_string())
            } else {
                crate::http::post_bytes::<Uploaded>(&path, &mime, &bytes)
                    .await
                    .map_err(|e| e.message())
            };
            let mut a = app.borrow_mut();
            match uploaded {
                Ok(up) => {
                    let poster = draw_poster(&a.document, a.video.first_frame.as_ref(), seconds);
                    a.insert_video(up.url, poster, at);
                    a.close_video_recorder();
                }
                Err(e) => {
                    a.video.stage = Stage::Review;
                    a.video
                        .set_status(&format!("{}: {e}", crate::i18n::tr("保存できません")), true);
                    a.video_render();
                }
            }
        });
    }

    fn insert_video(&mut self, url: String, poster: Option<(Vec<u8>, u32, u32)>, at: Vec2) {
        let (bytes, w, h) = poster.unwrap_or_default();
        let aspect = if w > 0 {
            h as f32 / w as f32
        } else {
            9.0 / 16.0
        };
        let size = Vec2::new(PLACE_WIDTH, PLACE_WIDTH * aspect);
        let id = self.board.alloc_id();
        self.board.end_gesture();
        self.board.push(Shape::video(
            id,
            at - size * 0.5,
            size,
            ImageData::new(bytes),
            url,
        ));
        self.board.end_gesture();
        self.editor.set_selection(vec![id]);
        self.dirty = true;
    }

    // ---- 再生 ----

    /// 画面の点 `p` に動画メモがあれば再生する。
    pub(super) fn play_video_at(&mut self, p: Vec2) -> bool {
        let world = self.camera.screen_to_world(p);
        let id = match self.board.hit_test(world, 4.0 / self.camera.zoom) {
            Some(Element::Shape(s)) if s.kind == ShapeKind::Video => s.id,
            _ => return false,
        };
        self.play_video(id)
    }

    /// 選択中の動画メモを再生する(Enter)。
    pub(super) fn play_selected_video(&mut self) -> bool {
        let sel = self.editor.selection();
        let [id] = sel else { return false };
        let id = *id;
        self.play_video(id)
    }

    fn play_video(&mut self, id: ElementId) -> bool {
        let Some(Element::Shape(s)) = self.board.get(id) else {
            return false;
        };
        if s.kind != ShapeKind::Video || s.media.is_empty() {
            return false;
        }
        let url = playable_url(&s.media);
        self.video.playing = Some(id);
        self.video.player_video.set_src(&url);
        self.video.player.set_hidden(false);
        let _ = self.video.player_video.play();
        self.layout_video_player();
        true
    }

    pub(super) fn close_video_player(&mut self) {
        if self.video.playing.take().is_none() {
            return;
        }
        let _ = self.video.player_video.pause();
        let _ = self.video.player_video.remove_attribute("src");
        self.video.player_video.load();
        self.video.player.set_hidden(true);
    }

    /// 再生中の動画を要素の位置に合わせる(パン・ズームに追従)。
    pub(super) fn layout_video_player(&mut self) {
        let Some(id) = self.video.playing else { return };
        let Some(Element::Shape(s)) = self.board.get(id) else {
            // 消された。
            self.close_video_player();
            return;
        };
        let b = s.bounds();
        let dpr = self.window.device_pixel_ratio() as f32;
        let rect = self.canvas.get_bounding_client_rect();
        let origin = Vec2::new(rect.left() as f32, rect.top() as f32);
        let a = self.camera.world_to_screen(b.min) / dpr + origin;
        let c = self.camera.world_to_screen(b.max) / dpr + origin;
        // 小さすぎると操作できないので、最低限の大きさは確保する。
        let w = (c.x - a.x).max(240.0);
        let h = (c.y - a.y).max(135.0);
        let style = self.video.player.style();
        let _ = style.set_property("left", &format!("{}px", a.x));
        let _ = style.set_property("top", &format!("{}px", a.y));
        let _ = style.set_property("width", &format!("{w}px"));
        let _ = style.set_property("height", &format!("{h}px"));
    }
}

async fn open_camera() -> Result<MediaStream, String> {
    let window = web_sys::window().ok_or("window がありません")?;
    let devices = window.navigator().media_devices().map_err(|_| {
        "このブラウザではカメラを使えません(HTTPS か localhost で開いてください)".to_string()
    })?;
    let video = js_sys::Object::new();
    let set = |o: &js_sys::Object, k: &str, v: JsValue| {
        let _ = js_sys::Reflect::set(o, &k.into(), &v);
    };
    set(&video, "width", JsValue::from(1280));
    set(&video, "height", JsValue::from(720));
    set(&video, "facingMode", JsValue::from("user"));
    let audio = js_sys::Object::new();
    set(&audio, "echoCancellation", JsValue::TRUE);
    set(&audio, "noiseSuppression", JsValue::TRUE);
    let c = MediaStreamConstraints::new();
    c.set_video(&video);
    c.set_audio(&audio);
    let promise = devices
        .get_user_media_with_constraints(&c)
        .map_err(|_| "カメラを使えません".to_string())?;
    JsFuture::from(promise)
        .await
        .map_err(|e| {
            let name = js_sys::Reflect::get(&e, &"name".into())
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            match name.as_str() {
                "NotAllowedError" | "SecurityError" => {
                    "カメラとマイクの使用が許可されていません(アドレスバーから許可できます)"
                        .to_string()
                }
                "NotFoundError" | "OverconstrainedError" => "カメラが見つかりません".into(),
                "NotReadableError" => "カメラを他のアプリが使っています".into(),
                _ => "カメラを使えません".into(),
            }
        })?
        .dyn_into()
        .map_err(|_| "カメラを使えません".to_string())
}

/// プレビューの今のコマを写す(表紙用。長辺は [`POSTER_WIDTH`] まで)。
fn capture_frame(document: &Document, video: &HtmlVideoElement) -> Option<HtmlCanvasElement> {
    let (vw, vh) = (video.video_width(), video.video_height());
    if vw == 0 || vh == 0 {
        return None;
    }
    let scale = (POSTER_WIDTH as f64 / vw as f64).min(1.0);
    let (w, h) = (
        (vw as f64 * scale).round() as u32,
        (vh as f64 * scale).round() as u32,
    );
    let canvas: HtmlCanvasElement = document.create_element("canvas").ok()?.dyn_into().ok()?;
    canvas.set_width(w);
    canvas.set_height(h);
    let ctx: CanvasRenderingContext2d = canvas.get_context("2d").ok()??.dyn_into().ok()?;
    ctx.draw_image_with_html_video_element_and_dw_and_dh(video, 0.0, 0.0, w as f64, h as f64)
        .ok()?;
    Some(canvas)
}

pub fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    {
        let app2 = app.clone();
        let button: HtmlElement = by_id(doc, "insert-video")?;
        listen(
            button.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                App::open_video_recorder(&app2);
            },
        )?;
    }
    {
        let app2 = app.clone();
        let button: HtmlElement = by_id(doc, "video-record")?;
        listen(
            button.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                let recording = app2.borrow().video.stage == Stage::Recording;
                if recording {
                    app2.borrow_mut().stop_recording();
                } else {
                    App::start_recording(&app2);
                }
            },
        )?;
    }
    {
        let app2 = app.clone();
        let button: HtmlElement = by_id(doc, "video-place")?;
        listen(
            button.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                App::place_video(&app2);
            },
        )?;
    }
    {
        let app2 = app.clone();
        let button: HtmlElement = by_id(doc, "video-retake")?;
        listen(
            button.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                App::retake_video(&app2);
            },
        )?;
    }
    on_click(doc, "video-cancel", app, |a| a.close_video_recorder())?;
    on_click(doc, "video-player-close", app, |a| a.close_video_player())?;
    {
        // Esc で閉じる(録画パネル・再生)。
        let app2 = app.clone();
        listen(
            doc.as_ref(),
            "keydown",
            false,
            move |e: web_sys::KeyboardEvent| {
                if e.key() != "Escape" {
                    return;
                }
                let Ok(mut a) = app2.try_borrow_mut() else {
                    return;
                };
                if a.video.playing.is_some() {
                    a.close_video_player();
                } else if a.video.stage != Stage::Closed && a.video.stage != Stage::Uploading {
                    a.close_video_recorder();
                }
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(9.6), "0:10");
        assert_eq!(format_time(60.0), "1:00");
    }
}
