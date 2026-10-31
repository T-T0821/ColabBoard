use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use board_core::{Board, Camera, Editor, Modifiers, Navigator, Tool, Vec2};
use board_render::{Renderer, wgpu};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Document, Element, HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement,
    KeyboardEvent, PointerEvent, WheelEvent, Window,
};

use crate::{format_count, format_zoom, parse_hex};

struct App {
    window: Window,
    document: Document,
    canvas: HtmlCanvasElement,
    renderer: Renderer,
    board: Board,
    camera: Camera,
    editor: Editor,
    nav: Navigator,
    /// 押されているポインタの現在位置(物理ピクセル)。
    pointers: HashMap<i32, Vec2>,
    /// 今のジェスチャーをパン/ピンチとして扱っているか。
    navigating: bool,
    space_down: bool,
    last_tool: Option<Tool>,
    last_count: Option<usize>,
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

    // `?n=100000` で性能確認用のダミーボードを開く。
    let demo_count = window
        .location()
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("n"))
        .and_then(|n| n.parse::<usize>().ok())
        .map(|n| n.min(1_000_000));

    let (w, h) = renderer.size();
    let mut camera = Camera::new(Vec2::new(w as f32, h as f32));
    let board = match demo_count {
        Some(n) => Board::demo(n),
        None => Board::welcome(),
    };
    if let Some(r) = board.bounds() {
        camera.fit(r.min, r.max, 120.0);
        if demo_count.is_none() {
            // 最初は等倍に近い見た目にする。
            let center = camera.viewport * 0.5;
            camera.zoom_at(center, 1.0 / camera.zoom.max(1.0));
        }
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
        document: document.clone(),
        canvas: canvas.clone(),
        renderer,
        board,
        camera,
        editor: Editor::new(),
        nav: Navigator::new(),
        pointers: HashMap::new(),
        navigating: false,
        space_down: false,
        last_tool: None,
        last_count: None,
        dirty: true,
        continuous: false,
        frame_times: VecDeque::with_capacity(120),
        last_frame: None,
        hud_last_update: 0.0,
        hud,
    }));

    install_pointer(&app, &canvas)?;
    install_keyboard(&app, &window)?;
    install_controls(&app, &document)?;
    start_loop(app);
    Ok(())
}

impl App {
    fn set_board(&mut self, board: Board) {
        self.board = board;
        self.editor.clear_selection();
        self.fit();
    }

    fn fit(&mut self) {
        if let Some(r) = self.board.bounds() {
            self.camera.fit(r.min, r.max, 48.0);
            self.dirty = true;
        }
    }

    fn zoom_center(&mut self, factor: f32) {
        let center = self.camera.viewport * 0.5;
        self.camera.zoom_at(center, factor);
        self.dirty = true;
    }

    fn set_tool(&mut self, tool: Tool) {
        self.editor.set_tool(tool);
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

    fn set_cursor(&self, cursor: &str) {
        let _ = self.canvas.style().set_property("cursor", cursor);
    }

    fn update_cursor(&self, pos: Option<Vec2>) {
        let cursor = if self.navigating {
            "grabbing"
        } else if self.space_down {
            "grab"
        } else if let Some(p) = pos {
            self.editor.cursor(&self.board, &self.camera, p)
        } else {
            return;
        };
        self.set_cursor(cursor);
    }

    /// ツールバーの表示を編集状態に合わせる。
    fn sync_toolbar(&mut self) {
        let tool = self.editor.tool();
        if self.last_tool == Some(tool) {
            return;
        }
        self.last_tool = Some(tool);
        for t in Tool::ALL {
            if let Some(el) = self
                .document
                .get_element_by_id(&format!("tool-{}", t.name()))
            {
                let _ = el.set_attribute("aria-pressed", if t == tool { "true" } else { "false" });
            }
        }
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

        self.sync_toolbar();
        if self.last_count != Some(self.board.len()) {
            self.last_count = Some(self.board.len());
            self.hud
                .count
                .set_text_content(Some(&format_count(self.board.len())));
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

        let overlay = self.editor.overlay(&self.board, &self.camera);
        if let Err(e) = self.renderer.render(&self.board, &self.camera, &overlay) {
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

    // ---- ポインタ ----

    fn on_pointer_down(&mut self, e: &PointerEvent) {
        let p = self.css_to_physical(e.client_x() as f64, e.client_y() as f64);
        let id = e.pointer_id();
        self.pointers.insert(id, p);

        let wants_nav = e.button() == 1
            || self.space_down
            || self.editor.tool() == Tool::Hand
            || self.pointers.len() > 1;

        if wants_nav && !self.navigating {
            // 編集中に 2 本目の指が来たら、編集を取り消してピンチに切り替える。
            if self.editor.is_dragging() {
                self.editor.cancel(&mut self.board);
            }
            self.navigating = true;
            for (pid, pos) in &self.pointers {
                self.nav.pointer_down(*pid, *pos);
            }
        } else if self.navigating {
            self.nav.pointer_down(id, p);
        } else if e.button() == 0 {
            let mods = Modifiers {
                shift: e.shift_key(),
            };
            self.editor
                .pointer_down(&mut self.board, &self.camera, p, mods);
        }
        self.dirty = true;
        self.update_cursor(Some(p));
    }

    fn on_pointer_move(&mut self, e: &PointerEvent) {
        let p = self.css_to_physical(e.client_x() as f64, e.client_y() as f64);
        let id = e.pointer_id();
        if let Some(slot) = self.pointers.get_mut(&id) {
            *slot = p;
        }
        if self.navigating {
            if self.nav.pointer_move(&mut self.camera, id, p) {
                self.dirty = true;
            }
        } else if self.editor.is_dragging() {
            let mods = Modifiers {
                shift: e.shift_key(),
            };
            if self
                .editor
                .pointer_move(&mut self.board, &self.camera, p, mods)
            {
                self.dirty = true;
            }
        }
        self.update_cursor(Some(p));
    }

    fn on_pointer_up(&mut self, e: &PointerEvent) {
        let p = self.css_to_physical(e.client_x() as f64, e.client_y() as f64);
        let id = e.pointer_id();
        self.pointers.remove(&id);
        if self.navigating {
            self.nav.pointer_up(id);
            if self.pointers.is_empty() {
                self.navigating = false;
            }
        } else if self.editor.pointer_up(&mut self.board, &self.camera, p) {
            self.dirty = true;
        }
        self.update_cursor(Some(p));
    }

    // ---- キーボード ----

    fn on_key_down(&mut self, e: &KeyboardEvent) -> bool {
        let key = e.key();
        let ctrl = e.ctrl_key() || e.meta_key();
        match key.as_str() {
            " " => {
                if !self.space_down {
                    self.space_down = true;
                    self.update_cursor(None);
                }
            }
            "Delete" | "Backspace" => {
                self.editor.delete_selection(&mut self.board);
                self.dirty = true;
            }
            "Escape" => {
                if self.editor.is_dragging() {
                    self.editor.cancel(&mut self.board);
                }
                self.editor.clear_selection();
                self.editor.set_tool(Tool::Select);
                self.dirty = true;
            }
            "a" | "A" if ctrl => {
                self.editor.select_all(&self.board);
                self.dirty = true;
            }
            "+" | "=" => self.zoom_center(1.25),
            "-" => self.zoom_center(0.8),
            "0" => {
                let z = self.camera.zoom;
                self.zoom_center(1.0 / z);
            }
            "1" if e.shift_key() || !ctrl => self.fit(),
            k if !ctrl => match Tool::from_key(k) {
                Some(t) => self.set_tool(t),
                None => return false,
            },
            _ => return false,
        }
        true
    }

    fn on_key_up(&mut self, e: &KeyboardEvent) {
        if e.key() == " " {
            self.space_down = false;
            self.set_cursor(if self.editor.tool() == Tool::Hand {
                "grab"
            } else {
                "default"
            });
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

fn install_pointer(app: &Rc<RefCell<App>>, canvas: &HtmlCanvasElement) -> Result<(), String> {
    let target: &web_sys::EventTarget = canvas.as_ref();
    {
        let app = app.clone();
        let canvas = canvas.clone();
        listen(target, "pointerdown", false, move |e: PointerEvent| {
            e.prevent_default();
            let _ = canvas.set_pointer_capture(e.pointer_id());
            let _ = canvas.focus();
            app.borrow_mut().on_pointer_down(&e);
        })?;
    }
    {
        let app = app.clone();
        listen(target, "pointermove", false, move |e: PointerEvent| {
            app.borrow_mut().on_pointer_move(&e);
        })?;
    }
    for name in ["pointerup", "pointercancel"] {
        let app = app.clone();
        listen(target, name, false, move |e: PointerEvent| {
            app.borrow_mut().on_pointer_up(&e);
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
    Ok(())
}

fn install_keyboard(app: &Rc<RefCell<App>>, window: &Window) -> Result<(), String> {
    let target: &web_sys::EventTarget = window.as_ref();
    let is_text_input = |e: &KeyboardEvent| {
        e.target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .map(|el| {
                let tag = el.tag_name();
                tag == "INPUT" || tag == "TEXTAREA" || el.has_attribute("contenteditable")
            })
            .unwrap_or(false)
    };
    {
        let app = app.clone();
        listen(target, "keydown", false, move |e: KeyboardEvent| {
            if is_text_input(&e) || e.is_composing() {
                return;
            }
            if app.borrow_mut().on_key_down(&e) {
                e.prevent_default();
            }
        })?;
    }
    {
        let app = app.clone();
        listen(target, "keyup", false, move |e: KeyboardEvent| {
            app.borrow_mut().on_key_up(&e);
        })?;
    }
    Ok(())
}

fn on_click(
    doc: &Document,
    id: &str,
    app: &Rc<RefCell<App>>,
    f: impl Fn(&mut App) + 'static,
) -> Result<(), String> {
    let button: HtmlButtonElement = by_id(doc, id)?;
    let app = app.clone();
    listen(
        button.as_ref(),
        "click",
        false,
        move |_: web_sys::MouseEvent| {
            f(&mut app.borrow_mut());
        },
    )
}

fn install_controls(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    for tool in Tool::ALL {
        on_click(doc, &format!("tool-{}", tool.name()), app, move |a| {
            a.set_tool(tool)
        })?;
    }

    // 色見本: data-color="#rrggbb"
    let swatches = doc.query_selector_all("[data-color]").map_err(js_err)?;
    for i in 0..swatches.length() {
        let Some(el) = swatches.item(i).and_then(|n| n.dyn_into::<Element>().ok()) else {
            continue;
        };
        let Some(color) = el.get_attribute("data-color").and_then(|c| parse_hex(&c)) else {
            continue;
        };
        let app = app.clone();
        listen(
            el.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                let mut a = app.borrow_mut();
                let App { editor, board, .. } = &mut *a;
                editor.apply_color(board, color);
                a.dirty = true;
            },
        )?;
    }

    for (id, count) in [
        ("demo-1k", 1_000),
        ("demo-10k", 10_000),
        ("demo-100k", 100_000),
        ("demo-1m", 1_000_000),
    ] {
        on_click(doc, id, app, move |a| a.set_board(Board::demo(count)))?;
    }
    on_click(doc, "demo-clear", app, |a| {
        a.set_board(Board::welcome());
    })?;
    on_click(doc, "zoom-in", app, |a| a.zoom_center(1.25))?;
    on_click(doc, "zoom-out", app, |a| a.zoom_center(0.8))?;
    on_click(doc, "zoom-fit", app, |a| a.fit())?;
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
