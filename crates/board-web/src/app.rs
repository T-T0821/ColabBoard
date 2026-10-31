use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use board_core::protocol::{Message, Presence};
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

mod actions;
mod ai;
mod collab;
mod follow;
mod format_bar;
mod reactions;
mod toolbar;

use crate::net::{Net, NetEvent, Status};
use crate::presence::{Cursors, Identity};
use crate::session::{self, Access, BoardChrome, SharedChrome};
use crate::{format_count, format_zoom, parse_hex};

/// カーソル位置を送る最短間隔(ミリ秒)。
const PRESENCE_INTERVAL_MS: f64 = 40.0;

mod diagram_ui;

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
    /// 共同編集のルーム名(ダミー表示中は `None`)。
    room: Option<String>,
    net: Option<Net>,
    identity: Identity,
    cursors: Cursors,
    cursors_dirty: bool,
    /// 最初の表示位置を決めたか(共同編集では初回同期後に決める)。
    view_initialized: bool,
    /// 自分のカーソルのワールド座標(canvas の外なら `None`)。
    cursor_world: Option<Vec2>,
    presence_sent: Option<Option<Vec2>>,
    presence_sent_at: f64,
    last_status: Option<(Status, usize)>,
    collab: collab::Collab,
    ai: ai::AiUi,
    follow: follow::Follow,
    format_bar: format_bar::FormatBar,
    /// ボード名・権限・共有パネル(サーバーの API が使えるときだけ)。
    chrome: Option<SharedChrome>,
    /// 権限の確かめ直しの結果(非同期で届く)。
    access_slot: Rc<RefCell<Option<Access>>>,
    /// 初回同期の回数(2 回目以降はつなぎ直し)。
    syncs: u32,
}

struct Hud {
    backend: HtmlElement,
    count: HtmlElement,
    zoom: HtmlElement,
    fps: HtmlElement,
    net: HtmlElement,
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

    let params = window
        .location()
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .ok_or("URL を読めません")?;
    // `?n=100000` で性能確認用のダミーボードを開く。
    let demo_count = params
        .get("n")
        .and_then(|n| n.parse::<usize>().ok())
        .map(|n| n.min(1_000_000));
    // `?offline` でサーバーにつながず、このタブだけで動かす。
    let offline = params.has("offline");
    let collaborative = demo_count.is_none() && !offline;

    // `?room=` がなければボード一覧(サーバーに API がなければ従来どおり新しいボード)。
    if collaborative
        && params.get("room").filter(|r| valid_room(r)).is_none()
        && let crate::home::Outcome::Shown = crate::home::run(&window, &document).await?
    {
        return Ok(());
    }
    // 共有リンクの鍵(`&key=`)。
    let key = params.get("key").filter(|k| valid_room(k));
    let mut me = None;
    let mut info = None;
    if collaborative {
        let room = ensure_room(&window, &params);
        match session::check_access(&room, key.as_deref()).await {
            Access::Denied => {
                let me = session::fetch_me().await;
                session::show_denied(&document, me.is_some(), false);
                return Ok(());
            }
            Access::Granted(i) => {
                info = Some(i);
                me = session::fetch_me().await;
            }
            Access::Unavailable => {}
        }
    }

    let canvas: HtmlCanvasElement = by_id(&document, "board")?;
    let (canvas, renderer) = create_renderer(&window, canvas).await?;

    let (w, h) = renderer.size();
    let camera = Camera::new(Vec2::new(w as f32, h as f32));
    let (board, room, net) = match demo_count {
        Some(n) => (Board::demo(n), None, None),
        None if offline => (Board::welcome(), None, None),
        None => {
            let room = ensure_room(&window, &params);
            let mut net = Net::new(server_url(&window, &params, &room, key.as_deref())?);
            net.connect();
            // 共同編集のボードはサーバーから中身を受け取るまで空。
            (Board::new(), Some(room), Some(net))
        }
    };

    let hud = Hud {
        backend: by_id(&document, "hud-backend")?,
        count: by_id(&document, "hud-count")?,
        zoom: by_id(&document, "hud-zoom")?,
        fps: by_id(&document, "hud-fps")?,
        net: by_id(&document, "hud-net")?,
    };
    hud.backend.set_text_content(Some(&renderer.backend_name()));
    hud.backend.set_title(renderer.adapter_name());

    let mut identity = Identity::load(&window);
    if let Some(me) = &me {
        identity.name = me.name.clone();
    }
    // サーバーの API が使えるとき(info がある)だけ、ボード名や共有パネルを出す。
    let chrome = match (&room, info) {
        (Some(room), Some(info)) => Some(BoardChrome::new(
            &window,
            &document,
            room.clone(),
            key.clone(),
            me,
            Some(info),
        )),
        _ => None,
    };

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
        room,
        net,
        identity,
        cursors: Cursors::new(document.clone(), by_id(&document, "cursors")?),
        cursors_dirty: false,
        view_initialized: false,
        cursor_world: None,
        presence_sent: None,
        presence_sent_at: 0.0,
        last_status: None,
        collab: collab::Collab::new(&document)?,
        ai: ai::AiUi::new(&document)?,
        follow: follow::Follow::new(&document)?,
        format_bar: format_bar::FormatBar::default(),
        chrome,
        access_slot: Rc::default(),
        syncs: 0,
    }));
    {
        let mut a = app.borrow_mut();
        if a.net.is_none() {
            a.initial_view();
            let text = if a.board.is_collaborative() {
                "オフライン(保存されません)"
            } else {
                "ダミー表示中(同期なし)"
            };
            a.hud.net.set_text_content(Some(text));
            let _ = a.hud.net.set_attribute("data-status", "offline");
        }
    }

    install_pointer(&app, &canvas)?;
    install_text_editor(&app)?;
    load_font(app.clone());
    install_keyboard(&app, &window)?;
    install_controls(&app, &document)?;
    actions::install_clipboard(&app)?;
    actions::install_context_menu(&app)?;
    toolbar::install(&app)?;
    format_bar::install(&app)?;
    reactions::install(&app)?;
    start_loop(app);
    Ok(())
}

/// `?room=` がなければ新しいルーム名を作って URL に載せる(そのまま共有できるように)。
fn ensure_room(window: &Window, params: &web_sys::UrlSearchParams) -> String {
    if let Some(room) = params.get("room").filter(|r| valid_room(r)) {
        return room;
    }
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let room: String = (0..10)
        .map(|_| {
            CHARS[(js_sys::Math::random() * CHARS.len() as f64) as usize % CHARS.len()] as char
        })
        .collect();
    params.set("room", &room);
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(
            &JsValue::NULL,
            "",
            Some(&format!("?{}", String::from(params.to_string()))),
        );
    }
    room
}

fn valid_room(r: &str) -> bool {
    (1..=64).contains(&r.len())
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 接続先。既定はページと同じホストの `/ws/{room}`。`?server=ws://host:port` で変えられる。
fn server_url(
    window: &Window,
    params: &web_sys::UrlSearchParams,
    room: &str,
    key: Option<&str>,
) -> Result<String, String> {
    let room = match key {
        Some(k) => format!("{room}?key={k}"),
        None => room.to_string(),
    };
    if let Some(base) = params.get("server") {
        return Ok(format!("{}/ws/{room}", base.trim_end_matches('/')));
    }
    let loc = window.location();
    let proto = if loc.protocol().map_err(js_err)? == "https:" {
        "wss:"
    } else {
        "ws:"
    };
    Ok(format!(
        "{proto}//{}/ws/{room}",
        loc.host().map_err(js_err)?
    ))
}

impl App {
    fn set_board(&mut self, board: Board) {
        // ダミー表示に切り替えたら共同編集はやめる。
        if let Some(net) = self.net.as_mut() {
            net.disable();
        }
        self.cursors.clear();
        self.board = board;
        self.editor.clear_selection();
        self.fit();
    }

    /// 最初の表示位置: 全体が入るようにしつつ、等倍より大きくはしない。
    fn initial_view(&mut self) {
        self.view_initialized = true;
        let Some(r) = self.board.bounds() else {
            return;
        };
        let demo = !self.board.is_collaborative();
        self.camera.fit(r.min, r.max, 120.0);
        if !demo {
            let center = self.camera.viewport * 0.5;
            self.camera.zoom_at(center, 1.0 / self.camera.zoom.max(1.0));
        }
        self.dirty = true;
    }

    // ---- 同期 ----

    /// 編集できるか(閲覧のみ・コメント可の人は見るだけ)。
    fn can_edit(&self) -> bool {
        self.chrome
            .as_ref()
            .is_none_or(|c| c.borrow().role().can_edit())
    }

    /// 権限を確かめ直す(つなぎ直したとき)。結果は `apply_access` で反映する。
    fn recheck_access(&self) {
        let Some(chrome) = &self.chrome else {
            return;
        };
        let (room, key) = {
            let c = chrome.borrow();
            (c.room.clone(), c.key.clone())
        };
        let slot = self.access_slot.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let access = session::check_access(&room, key.as_deref()).await;
            *slot.borrow_mut() = Some(access);
        });
    }

    fn apply_access(&mut self) {
        let Some(access) = self.access_slot.borrow_mut().take() else {
            return;
        };
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        match access {
            Access::Granted(info) => {
                chrome.borrow_mut().set_info(info);
                if !self.can_edit() {
                    // 編集できなくなった: 途中の操作をやめる。
                    self.end_text_edit(true);
                    if self.editor.is_dragging() {
                        self.editor.cancel(&mut self.board);
                    }
                    self.editor.clear_selection();
                    self.editor.set_tool(Tool::Select);
                    self.dirty = true;
                }
            }
            Access::Denied => {
                if let Some(net) = self.net.as_mut() {
                    net.disable();
                }
                let logged_in = chrome.borrow().me.is_some();
                session::show_denied(&self.document, logged_in, true);
            }
            Access::Unavailable => {}
        }
    }

    fn pump_net(&mut self, now: f64) {
        self.apply_access();
        let Some(net) = self.net.as_mut() else {
            return;
        };
        let events = net.poll(now);
        for e in events {
            match e {
                NetEvent::Open => {}
                NetEvent::Message(Message::Sync { you, state }) => {
                    log::info!("同期しました(参加者番号 {you})");
                    self.syncs += 1;
                    if self.syncs > 1 {
                        self.recheck_access();
                    }
                    if let Err(e) = self.board.apply_remote(&state) {
                        log::error!("初回同期に失敗: {e}");
                    }
                    // 切断中の変更も含めて手元の全体を送る(重複は CRDT が吸収する)。
                    let _ = self.board.take_outgoing();
                    if let (Some(net), Ok(full)) = (self.net.as_ref(), self.board.encode_diff(None))
                    {
                        net.send(&Message::Update(full));
                    }
                    self.cursors.clear();
                    self.presence_sent = None;
                    self.presence_sent_at = 0.0;
                    if !self.view_initialized {
                        self.initial_view();
                    }
                    self.editor.prune_selection(&self.board);
                    self.dirty = true;
                }
                NetEvent::Message(Message::Update(u)) => {
                    if let Err(e) = self.board.apply_remote(&u) {
                        log::warn!("更新を取り込めません: {e}");
                    }
                    self.editor.prune_selection(&self.board);
                    self.dirty = true;
                }
                NetEvent::Message(Message::Presence(p)) => {
                    self.cursors.upsert(p);
                    self.cursors_dirty = true;
                }
                NetEvent::Message(Message::Leave(peer)) => {
                    self.cursors.remove(peer);
                    self.on_peer_leave(peer);
                }
                NetEvent::Message(Message::View(v)) => self.on_view(v),
                NetEvent::Message(Message::Reaction(r)) => self.on_reaction(r),
                NetEvent::Closed => {
                    self.cursors.clear();
                    self.follow_reset();
                    if self.net.as_ref().map(|n| n.status) == Some(Status::Reconnecting) {
                        self.recheck_access();
                    }
                    let offline = self.net.as_ref().map(|n| n.status) == Some(Status::Offline);
                    if offline && self.board.is_empty() && !self.view_initialized {
                        log::warn!("サーバーにつながらないので、このタブだけで動かします");
                        self.board.seed_welcome();
                        self.initial_view();
                    }
                }
            }
        }

        let Some(net) = self.net.as_ref() else {
            return;
        };
        if net.status == Status::Online && net.is_open() {
            for u in self.board.take_outgoing() {
                net.send(&Message::Update(u));
            }
            if self.presence_sent != Some(self.cursor_world)
                && now - self.presence_sent_at >= PRESENCE_INTERVAL_MS
            {
                let (x, y) = self
                    .cursor_world
                    .map_or((f32::NAN, f32::NAN), |w| (w.x, w.y));
                net.send(&Message::Presence(Presence {
                    peer: 0,
                    x,
                    y,
                    color: self.identity.color,
                    name: self.identity.name.clone(),
                }));
                self.presence_sent = Some(self.cursor_world);
                self.presence_sent_at = now;
            }
        }

        let status = (net.status, self.cursors.len());
        if self.last_status != Some(status) {
            self.last_status = Some(status);
            let text = match status.0 {
                Status::Connecting => "接続中…".to_string(),
                Status::Online => format!("オンライン · {}人", status.1 + 1),
                Status::Reconnecting => "再接続中…".to_string(),
                Status::Offline => "オフライン(保存されません)".to_string(),
                Status::Disabled => "ダミー表示中(同期なし)".to_string(),
            };
            self.hud.net.set_text_content(Some(&text));
            let state = match status.0 {
                Status::Online => "online",
                Status::Connecting | Status::Reconnecting => "connecting",
                Status::Offline | Status::Disabled => "offline",
            };
            let _ = self.hud.net.set_attribute("data-status", state);
            self.hud.net.set_title(&format!(
                "ルーム: {}\nあなた: {}",
                self.room.as_deref().unwrap_or("-"),
                self.identity.name
            ));
        }
    }

    fn layout_cursors(&mut self) {
        self.cursors_dirty = false;
        let rect = self.canvas.get_bounding_client_rect();
        let dpr = self.window.device_pixel_ratio() as f32;
        self.cursors.layout(
            &self.camera,
            dpr,
            Vec2::new(rect.left() as f32, rect.top() as f32),
        );
    }

    /// 招待用に今の URL をコピーする。
    fn copy_link(&self) {
        let Ok(href) = self.window.location().href() else {
            return;
        };
        let _ = self.window.navigator().clipboard().write_text(&href);
        if let Some(el) = self.document.get_element_by_id("share") {
            el.set_text_content(Some("コピーしました"));
            let el2 = el.clone();
            let reset = Closure::once_into_js(move || el2.set_text_content(Some("リンクをコピー")));
            let _ = self
                .window
                .set_timeout_with_callback_and_timeout_and_arguments_0(reset.unchecked_ref(), 1500);
        }
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
        if self.editor.is_dragging() || !self.can_edit() {
            return;
        }
        self.board.end_gesture();
        self.board.undo();
        self.editor.prune_selection(&self.board);
        self.dirty = true;
    }

    fn redo(&mut self) {
        if self.editor.is_dragging() || !self.can_edit() {
            return;
        }
        self.board.end_gesture();
        self.board.redo();
        self.editor.prune_selection(&self.board);
        self.dirty = true;
    }

    fn set_tool(&mut self, tool: Tool) {
        if !self.can_edit() && tool != Tool::Hand {
            return;
        }
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
        self.pump_net(now);
        self.collab_frame();
        self.follow_frame(now);
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

        if self.cursors_dirty || self.dirty {
            self.layout_cursors();
        }
        if self.dirty {
            self.layout_collab();
            self.layout_format_bar();
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
            || !self.can_edit()
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
        } else if e.button() == 0 && (e.ctrl_key() || e.meta_key()) && self.open_link_at(p) {
            // Ctrl/⌘ + クリックでリンクを開いた。
        } else if e.button() == 0 && self.collab_pointer_down(p, e.shift_key()) {
            // 投票として扱った。
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

    /// 画面の点 `p` にある要素にリンクがあれば新しいタブで開く。
    fn open_link_at(&self, p: Vec2) -> bool {
        let world = self.camera.screen_to_world(p);
        let Some(url) = self
            .board
            .hit_test(world, 4.0 / self.camera.zoom)
            .and_then(|e| board_core::format::normalize_link(&e.meta().link))
        else {
            return false;
        };
        let _ = self
            .window
            .open_with_url_and_target_and_features(&url, "_blank", "noopener");
        true
    }

    fn on_pointer_move(&mut self, e: &PointerEvent) {
        let p = self.css_to_physical(e.client_x() as f64, e.client_y() as f64);
        let id = e.pointer_id();
        if let Some(slot) = self.pointers.get_mut(&id) {
            *slot = p;
        }
        self.cursor_world = Some(self.camera.screen_to_world(p));
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
        if !self.can_edit() || !self.editor.begin_edit(&self.board, id) {
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
        let Some(tb) = self.board.get(id).and_then(BoardElement::text_box) else {
            return;
        };
        let dpr = self.window.device_pixel_ratio() as f32;
        let zoom = self.camera.zoom;
        let rect = tb.rect;
        let style = tb.style;
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
        let _ = css.set_property("text-align", style.align.name());
        let [r, g, b, _] = style.color.0;
        let _ = css.set_property("color", &format!("rgb({r},{g},{b})"));
        // 縦方向の中央寄せ: 中身の高さを測って上の余白を決める。
        let _ = css.set_property("padding-top", "0px");
        let _ = css.set_property("height", "0px");
        let content = self.text_editor.scroll_height() as f32;
        let _ = css.set_property("height", &px(size.y));
        let pad = if tb.center_vertically {
            ((size.y - content) * 0.5).max(0.0)
        } else {
            0.0
        };
        let _ = css.set_property("padding-top", &px(pad));
        let _ = css.set_property("box-sizing", "border-box");
    }

    // ---- キーボード ----

    fn on_key_down(&mut self, e: &KeyboardEvent) -> bool {
        if let Some(a) = actions::action_for_key(e) {
            self.run_action(a);
            return true;
        }
        let key = e.key();
        let ctrl = e.ctrl_key() || e.meta_key();
        if key == "Escape"
            && let Some(c) = &self.chrome
            && c.borrow().share_open()
        {
            c.borrow().close_share();
            return true;
        }
        // 見るだけの人は表示の操作だけ。
        if !self.can_edit() && !matches!(key.as_str(), " " | "+" | "=" | "-" | "0" | "1") {
            return false;
        }
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
            k if !ctrl && !e.alt_key() => match Tool::from_key(k) {
                Some((t, kind)) => {
                    match kind {
                        Some(kind) => self.editor.set_shape_kind(kind),
                        None => self.editor.set_tool(t),
                    }
                    self.dirty = true;
                }
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
    {
        let app = app.clone();
        listen(target, "pointerleave", false, move |_: PointerEvent| {
            app.borrow_mut().cursor_world = None;
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
                if !a.can_edit() {
                    return;
                }
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
        if a.room.is_some() {
            // 共同編集のボードに戻る(ページを開き直す)。
            let room = a.room.clone().unwrap_or_default();
            let _ = a.window.location().set_search(&format!("?room={room}"));
        } else {
            a.set_board(Board::welcome());
        }
    })?;
    on_click(doc, "share", app, |a| {
        if let Some(c) = a.chrome.clone()
            && BoardChrome::on_share_click(&c)
        {
            return;
        }
        a.copy_link();
    })?;
    collab::install(app, doc)?;
    ai::install(app, doc)?;
    follow::install(app, doc)?;
    on_click(doc, "undo", app, |a| a.undo())?;
    on_click(doc, "redo", app, |a| a.redo())?;
    on_click(doc, "zoom-in", app, |a| a.zoom_center(1.25))?;
    on_click(doc, "zoom-out", app, |a| a.zoom_center(0.8))?;
    on_click(doc, "zoom-fit", app, |a| a.fit())?;
    diagram_ui::install(app, doc)?;
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
