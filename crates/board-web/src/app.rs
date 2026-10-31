use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use board_core::{Board, Camera, Navigator, Vec2};
use board_render::{Renderer, wgpu};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Document, HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement, KeyboardEvent,
    PointerEvent, WheelEvent, Window,
};

use crate::{format_count, format_zoom};

const DEFAULT_COUNT: usize = 10_000;

struct App {
    window: Window,
    canvas: HtmlCanvasElement,
    renderer: Renderer,
    board: Board,
    camera: Camera,
    nav: Navigator,
    dirty: bool,
    continuous: bool,
    frame_times: VecDeque<f64>,
    last_frame: Option<f64>,
    hud_last_update: f64,
    hud: Hud,
}

struct Hud {
    backend: HtmlElement,
    count: HtmlElement,
    zoom: HtmlElement,
    fps: HtmlElement,
}

pub fn show_error(msg: &str) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    if let Some(el) = doc.get_element_by_id("error") {
        el.set_text_content(Some(msg));
        let _ = el.set_attribute("data-visible", "true");
    }
}

fn by_id<T: JsCast>(doc: &Document, id: &str) -> Result<T, String> {
    doc.get_element_by_id(id)
        .ok_or_else(|| format!("#{id} が見つかりません"))?
        .dyn_into::<T>()
        .map_err(|_| format!("#{id} の要素の種類が違います"))
}

fn js_err(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

fn canvas_physical_size(window: &Window, canvas: &HtmlCanvasElement) -> (u32, u32) {
    let dpr = window.device_pixel_ratio();
    let w = (canvas.client_width() as f64 * dpr).round().max(1.0) as u32;
    let h = (canvas.client_height() as f64 * dpr).round().max(1.0) as u32;
    (w, h)
}

/// WebGPU を試し、だめなら WebGL2 で作り直す。
/// 一度 WebGPU のコンテキストを取った canvas は WebGL2 に使えないので、canvas ごと差し替える。
async fn create_renderer(
    window: &Window,
    canvas: HtmlCanvasElement,
) -> Result<(HtmlCanvasElement, Renderer), String> {
    let mut last_err = String::new();
    let force_webgl = window
        .location()
        .search()
        .map(|s| s.contains("webgl"))
        .unwrap_or(false);
    let attempts: &[wgpu::Backends] = if force_webgl {
        &[wgpu::Backends::GL]
    } else {
        &[wgpu::Backends::BROWSER_WEBGPU, wgpu::Backends::GL]
    };

    let mut canvas = canvas;
    for (i, backends) in attempts.iter().enumerate() {
        if i > 0 {
            let fresh = canvas
                .clone_node()
                .map_err(js_err)?
                .dyn_into::<HtmlCanvasElement>()
                .map_err(|_| "canvas の複製に失敗しました".to_string())?;
            canvas.replace_with_with_node_1(&fresh).map_err(js_err)?;
            canvas = fresh;
        }
        let (w, h) = canvas_physical_size(window, &canvas);
        canvas.set_width(w);
        canvas.set_height(h);

        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = *backends;
        let instance = wgpu::util::new_instance_with_webgpu_detection(desc).await;
        let surface = match instance.create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone())) {
            Ok(s) => s,
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        match Renderer::new(&instance, surface, w, h).await {
            Ok(r) => return Ok((canvas, r)),
            Err(e) => {
                log::warn!("{backends:?} で初期化できませんでした: {e}");
                last_err = e.to_string();
            }
        }
    }
    Err(format!(
        "このブラウザでは GPU 描画を開始できませんでした({last_err})。WebGPU か WebGL2 に対応したブラウザで開いてください。"
    ))
}

pub async fn run() -> Result<(), String> {
    let window = web_sys::window().ok_or("window がありません")?;
    let document = window.document().ok_or("document がありません")?;
    let canvas: HtmlCanvasElement = by_id(&document, "board")?;

    let (canvas, renderer) = create_renderer(&window, canvas).await?;

    let count = window
        .location()
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("n"))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(DEFAULT_COUNT)
        .min(1_000_000);

    let (w, h) = renderer.size();
    let mut camera = Camera::new(Vec2::new(w as f32, h as f32));
    let board = Board::demo(count);
    if let Some((lo, hi)) = board.bounds() {
        camera.fit(lo, hi, 48.0);
    }

    let hud = Hud {
        backend: by_id(&document, "hud-backend")?,
        count: by_id(&document, "hud-count")?,
        zoom: by_id(&document, "hud-zoom")?,
        fps: by_id(&document, "hud-fps")?,
    };
    hud.backend.set_text_content(Some(&renderer.backend_name()));
    hud.backend.set_title(renderer.adapter_name());

    let app = Rc::new(RefCell::new(App {
        window: window.clone(),
        canvas: canvas.clone(),
        renderer,
        board,
        camera,
        nav: Navigator::new(),
        dirty: true,
        continuous: false,
        frame_times: VecDeque::with_capacity(120),
        last_frame: None,
        hud_last_update: 0.0,
        hud,
    }));
    app.borrow().update_hud_static();

    install_input(&app, &canvas)?;
    install_controls(&app, &document)?;
    start_loop(app);
    Ok(())
}

impl App {
    fn update_hud_static(&self) {
        self.hud
            .count
            .set_text_content(Some(&format_count(self.board.len())));
        self.hud
            .zoom
            .set_text_content(Some(&format_zoom(self.camera.zoom)));
    }

    fn set_board(&mut self, count: usize) {
        self.board = Board::demo(count);
        if let Some((lo, hi)) = self.board.bounds() {
            self.camera.fit(lo, hi, 48.0);
        }
        self.dirty = true;
        self.update_hud_static();
    }

    fn fit(&mut self) {
        if let Some((lo, hi)) = self.board.bounds() {
            self.camera.fit(lo, hi, 48.0);
            self.dirty = true;
        }
    }

    fn zoom_center(&mut self, factor: f32) {
        let center = self.camera.viewport * 0.5;
        self.camera.zoom_at(center, factor);
        self.dirty = true;
    }

    fn css_to_physical(&self, x: f64, y: f64) -> Vec2 {
        let rect = self.canvas.get_bounding_client_rect();
        let dpr = self.window.device_pixel_ratio();
        Vec2::new(
            ((x - rect.left()) * dpr) as f32,
            ((y - rect.top()) * dpr) as f32,
        )
    }

    fn frame(&mut self, now: f64) {
        // canvas の表示サイズが変わっていたら描画先を合わせる。
        let (w, h) = canvas_physical_size(&self.window, &self.canvas);
        if (w, h) != self.renderer.size() {
            self.canvas.set_width(w);
            self.canvas.set_height(h);
            self.renderer.resize(w, h);
            self.camera.viewport = Vec2::new(w as f32, h as f32);
            self.dirty = true;
        }

        if !(self.dirty || self.continuous) {
            self.last_frame = None;
            return;
        }
        self.dirty = false;

        if let Some(prev) = self.last_frame {
            if self.frame_times.len() == 120 {
                self.frame_times.pop_front();
            }
            self.frame_times.push_back(now - prev);
        }
        self.last_frame = Some(now);

        if let Err(e) = self.renderer.render(&self.board, &self.camera) {
            log::error!("{e}");
            show_error(&e.to_string());
        }

        self.hud
            .zoom
            .set_text_content(Some(&format_zoom(self.camera.zoom)));
        if now - self.hud_last_update > 250.0 && !self.frame_times.is_empty() {
            self.hud_last_update = now;
            let avg = self.frame_times.iter().sum::<f64>() / self.frame_times.len() as f64;
            let text = if self.continuous {
                format!("{:.0} fps ({avg:.1} ms)", 1000.0 / avg.max(0.001))
            } else {
                "操作時のみ描画".to_string()
            };
            self.hud.fps.set_text_content(Some(&text));
        }
    }
}

fn listen<E: JsCast + wasm_bindgen::convert::FromWasmAbi + 'static>(
    target: &web_sys::EventTarget,
    name: &str,
    passive: bool,
    f: impl FnMut(E) + 'static,
) -> Result<(), String> {
    let closure = Closure::<dyn FnMut(E)>::new(f);
    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_passive(passive);
    target
        .add_event_listener_with_callback_and_add_event_listener_options(
            name,
            closure.as_ref().unchecked_ref(),
            &opts,
        )
        .map_err(js_err)?;
    // ページが生きている間ずっと使うリスナーなので解放しない。
    closure.forget();
    Ok(())
}

fn install_input(app: &Rc<RefCell<App>>, canvas: &HtmlCanvasElement) -> Result<(), String> {
    let target: &web_sys::EventTarget = canvas.as_ref();

    {
        let app = app.clone();
        let canvas = canvas.clone();
        listen(target, "pointerdown", false, move |e: PointerEvent| {
            e.prevent_default();
            let _ = canvas.set_pointer_capture(e.pointer_id());
            let _ = canvas.focus();
            let mut a = app.borrow_mut();
            let p = a.css_to_physical(e.client_x() as f64, e.client_y() as f64);
            a.nav.pointer_down(e.pointer_id(), p);
            let _ = canvas.set_attribute("data-dragging", "true");
        })?;
    }
    {
        let app = app.clone();
        listen(target, "pointermove", false, move |e: PointerEvent| {
            let mut a = app.borrow_mut();
            if !a.nav.is_dragging() {
                return;
            }
            let p = a.css_to_physical(e.client_x() as f64, e.client_y() as f64);
            let App { nav, camera, .. } = &mut *a;
            if nav.pointer_move(camera, e.pointer_id(), p) {
                a.dirty = true;
            }
        })?;
    }
    for name in ["pointerup", "pointercancel"] {
        let app = app.clone();
        let canvas = canvas.clone();
        listen(target, name, false, move |e: PointerEvent| {
            let mut a = app.borrow_mut();
            a.nav.pointer_up(e.pointer_id());
            if !a.nav.is_dragging() {
                let _ = canvas.remove_attribute("data-dragging");
            }
        })?;
    }
    {
        let app = app.clone();
        listen(target, "wheel", false, move |e: WheelEvent| {
            e.prevent_default();
            let mut a = app.borrow_mut();
            let dpr = a.window.device_pixel_ratio() as f32;
            // deltaMode: 0=ピクセル, 1=行, 2=ページ
            let unit = match e.delta_mode() {
                1 => 16.0,
                2 => a.camera.viewport.y / dpr,
                _ => 1.0,
            };
            let delta = Vec2::new(e.delta_x() as f32, e.delta_y() as f32) * unit * dpr;
            let p = a.css_to_physical(e.client_x() as f64, e.client_y() as f64);
            let zoom = e.ctrl_key() || e.meta_key();
            let App { nav, camera, .. } = &mut *a;
            nav.wheel(camera, delta, p, zoom);
            a.dirty = true;
        })?;
    }
    {
        let app = app.clone();
        listen(target, "keydown", false, move |e: KeyboardEvent| {
            let mut a = app.borrow_mut();
            match e.key().as_str() {
                "+" | "=" => a.zoom_center(1.25),
                "-" => a.zoom_center(0.8),
                "0" => {
                    let z = a.camera.zoom;
                    a.zoom_center(1.0 / z);
                }
                "1" => a.fit(),
                _ => return,
            }
            e.prevent_default();
        })?;
    }
    Ok(())
}

fn install_controls(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    for (id, count) in [
        ("demo-1k", 1_000),
        ("demo-10k", 10_000),
        ("demo-100k", 100_000),
        ("demo-1m", 1_000_000),
    ] {
        let button: HtmlButtonElement = by_id(doc, id)?;
        let app = app.clone();
        listen(
            button.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                app.borrow_mut().set_board(count);
            },
        )?;
    }
    for (id, factor) in [("zoom-in", 1.25f32), ("zoom-out", 0.8)] {
        let button: HtmlButtonElement = by_id(doc, id)?;
        let app = app.clone();
        listen(
            button.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                app.borrow_mut().zoom_center(factor);
            },
        )?;
    }
    {
        let button: HtmlButtonElement = by_id(doc, "zoom-fit")?;
        let app = app.clone();
        listen(
            button.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                app.borrow_mut().fit();
            },
        )?;
    }
    {
        let input: HtmlInputElement = by_id(doc, "continuous")?;
        let app = app.clone();
        let input2 = input.clone();
        listen(input.as_ref(), "change", false, move |_: web_sys::Event| {
            let mut a = app.borrow_mut();
            a.continuous = input2.checked();
            a.frame_times.clear();
            a.dirty = true;
        })?;
    }
    Ok(())
}

fn start_loop(app: Rc<RefCell<App>>) {
    type Tick = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;
    let tick: Tick = Rc::new(RefCell::new(None));
    let tick2 = tick.clone();
    let window = app.borrow().window.clone();
    let window2 = window.clone();
    *tick2.borrow_mut() = Some(Closure::new(move |now: f64| {
        app.borrow_mut().frame(now);
        if let Some(cb) = tick.borrow().as_ref() {
            let _ = window2.request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }));
    if let Some(cb) = tick2.borrow().as_ref() {
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
    }
}
