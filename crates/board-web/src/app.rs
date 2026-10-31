use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use board_core::{
    Board, Camera, Editor, Element as BoardElement, ElementId, Modifiers, Navigator, Tool, Vec2,
};
use board_render::{Renderer, wgpu};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Document, Element, HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement,
    HtmlTextAreaElement, KeyboardEvent, PointerEvent, WheelEvent, Window,
};

use crate::{format_count, format_zoom, parse_hex};

struct App {
    window: Window,
    document: Document,
    canvas: HtmlCanvasElement,
    text_editor: HtmlTextAreaElement,
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
    last_history: Option<(bool, bool)>,
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
        text_editor: by_id(&document, "text-editor")?,
        renderer,
        board,
        camera,
        editor: Editor::new(),
        nav: Navigator::new(),
        pointers: HashMap::new(),
        navigating: false,
        space_down: false,
        last_tool: None,
        last_history: None,
        last_count: None,
        dirty: true,
        continuous: false,
        frame_times: VecDeque::with_capacity(120),
        last_frame: None,
        hud_last_update: 0.0,
        hud,
    }));

    install_pointer(&app, &canvas)?;
    install_text_editor(&app)?;
    load_font(app.clone());
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

    fn undo(&mut self) {
        if self.editor.is_dragging() {
            return;
        }
        self.board.end_gesture();
        self.board.undo();
        self.editor.prune_selection(&self.board);
        self.dirty = true;
    }

    fn redo(&mut self) {
        if self.editor.is_dragging() {
            return;
        }
        self.board.end_gesture();
        self.board.redo();
        self.editor.prune_selection(&self.board);
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

    fn sync_history_buttons(&mut self) {
        let state = (self.board.can_undo(), self.board.can_redo());
        if self.last_history == Some(state) {
            return;
        }
        self.last_history = Some(state);
        for (id, enabled) in [("undo", state.0), ("redo", state.1)] {
            if let Some(el) = self.document.get_element_by_id(id) {
                if enabled {
                    let _ = el.remove_attribute("disabled");
                } else {
                    let _ = el.set_attribute("disabled", "");
                }
            }
        }
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
        self.sync_history_buttons();
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

        self.layout_text_editor();
        let overlay = self.editor.overlay(&self.board, &self.camera);
        let editing = self.editor.editing();
        if let Err(e) = self
            .renderer
            .render(&self.board, &self.camera, &overlay, editing)
        {
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
        // ドラッグ 1 回を 1 つの Undo 単位にする。
        self.board.end_gesture();
        if let Some(id) = self.editor.take_pending_edit() {
            self.begin_text_edit(id);
        }
        self.update_cursor(Some(p));
    }

    // ---- 文字編集 ----

    fn begin_text_edit(&mut self, id: ElementId) {
        if !self.editor.begin_edit(&self.board, id) {
            return;
        }
        let text = self.board.get(id).map(BoardElement::text).unwrap_or("");
        self.text_editor.set_value(text);
        let _ = self.text_editor.set_attribute("data-visible", "true");
        self.layout_text_editor();
        let _ = self.text_editor.focus();
        let len = self.text_editor.value().encode_utf16().count() as u32;
        let _ = self.text_editor.set_selection_range(len, len);
        self.dirty = true;
    }

    /// 編集中の文字を確定する(`commit` が false なら破棄)。
    fn end_text_edit(&mut self, commit: bool) {
        if self.editor.editing().is_none() {
            return;
        }
        let text = commit.then(|| self.text_editor.value());
        self.board.end_gesture();
        self.editor.end_edit(&mut self.board, text);
        self.board.end_gesture();
        let _ = self.text_editor.remove_attribute("data-visible");
        self.dirty = true;
    }

    /// 入力欄を編集中の図形の上に重ねる。カメラが動いても追従する。
    fn layout_text_editor(&self) {
        let Some(id) = self.editor.editing() else {
            return;
        };
        let Some(shape) = self.board.get(id).and_then(BoardElement::as_shape) else {
            return;
        };
        let dpr = self.window.device_pixel_ratio() as f32;
        let zoom = self.camera.zoom;
        let rect = shape.text_rect();
        let style = shape.text_style();
        let min = self.camera.world_to_screen(rect.min) / dpr;
        let size = rect.size() * zoom / dpr;
        let canvas_rect = self.canvas.get_bounding_client_rect();
        let el: &HtmlElement = self.text_editor.as_ref();
        let css = el.style();
        let px = |v: f32| format!("{v}px");
        let _ = css.set_property("left", &px(canvas_rect.left() as f32 + min.x));
        let _ = css.set_property("top", &px(canvas_rect.top() as f32 + min.y));
        let _ = css.set_property("width", &px(size.x));
        let _ = css.set_property("height", &px(size.y));
        let _ = css.set_property("font-size", &px(style.font_size * zoom / dpr));
        let _ = css.set_property("line-height", &px(style.line_height * zoom / dpr));
        // 縦方向の中央寄せ: 中身の高さを測って上の余白を決める。
        let _ = css.set_property("padding-top", "0px");
        let _ = css.set_property("height", "0px");
        let content = self.text_editor.scroll_height() as f32;
        let _ = css.set_property("height", &px(size.y));
        let pad = ((size.y - content) * 0.5).max(0.0);
        let _ = css.set_property("padding-top", &px(pad));
        let _ = css.set_property("box-sizing", "border-box");
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
            "Enter" => {
                if let [id] = self.editor.selection() {
                    let id = *id;
                    self.begin_text_edit(id);
                } else {
                    return false;
                }
            }
            "Delete" | "Backspace" => {
                self.editor.delete_selection(&mut self.board);
                self.board.end_gesture();
                self.dirty = true;
            }
            "z" | "Z" if ctrl => {
                if e.shift_key() {
                    self.redo();
                } else {
                    self.undo();
                }
            }
            "y" | "Y" if ctrl => self.redo(),
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
        listen(target, "dblclick", false, move |e: web_sys::MouseEvent| {
            let mut a = app.borrow_mut();
            let p = a.css_to_physical(e.client_x() as f64, e.client_y() as f64);
            if a.editor.tool() != Tool::Select {
                return;
            }
            if let Some(id) = a.editor.editable_at(&a.board, &a.camera, p) {
                e.prevent_default();
                a.begin_text_edit(id);
            }
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

fn install_text_editor(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let el = app.borrow().text_editor.clone();
    let target: &web_sys::EventTarget = el.as_ref();
    {
        let app = app.clone();
        listen(target, "blur", false, move |_: web_sys::FocusEvent| {
            app.borrow_mut().end_text_edit(true);
        })?;
    }
    {
        let app = app.clone();
        listen(target, "input", false, move |_: web_sys::Event| {
            app.borrow().layout_text_editor();
        })?;
    }
    {
        let el2 = el.clone();
        listen(target, "keydown", false, move |e: KeyboardEvent| {
            if e.is_composing() {
                return;
            }
            // Esc か Ctrl/⌘+Enter で確定(blur で反映される)。
            if e.key() == "Escape" || (e.key() == "Enter" && (e.ctrl_key() || e.meta_key())) {
                e.prevent_default();
                let _ = el2.blur();
            }
        })?;
    }
    Ok(())
}

/// 日本語フォントを後から読み込む。読み込み前は文字以外を描画しておく。
fn load_font(app: Rc<RefCell<App>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let window = app.borrow().window.clone();
        let result: Result<Vec<u8>, String> = async {
            let resp = wasm_bindgen_futures::JsFuture::from(
                window.fetch_with_str("fonts/NotoSansJP-Regular.ttf"),
            )
            .await
            .map_err(js_err)?
            .dyn_into::<web_sys::Response>()
            .map_err(|_| "fetch の結果が Response ではありません".to_string())?;
            if !resp.ok() {
                return Err(format!("HTTP {}", resp.status()));
            }
            let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(js_err)?)
                .await
                .map_err(js_err)?;
            Ok(js_sys::Uint8Array::new(&buf).to_vec())
        }
        .await;
        match result {
            Ok(bytes) => {
                let mut a = app.borrow_mut();
                a.renderer.load_font(bytes);
                a.dirty = true;
            }
            Err(e) => log::warn!("フォントを読み込めませんでした: {e}"),
        }
    });
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
                board.end_gesture();
                editor.apply_color(board, color);
                board.end_gesture();
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
    on_click(doc, "undo", app, |a| a.undo())?;
    on_click(doc, "redo", app, |a| a.redo())?;
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
