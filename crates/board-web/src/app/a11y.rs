//! キーボードだけでの操作と、画面読み上げ(スクリーンリーダー)への対応。
//!
//! キャンバスは GPU で描くので支援技術からは中身が見えない。そこで
//! - 要素を読む順番に並べた不可視の一覧(`#sr-board`)を DOM に置き、
//! - 選択やツールが変わったら読み上げ用のライブリージョンで知らせる。
//!
//! キーボード操作(キャンバスにフォーカスがあるとき):
//! - Tab / Shift+Tab: 要素を読む順に選ぶ(端まで来たらキャンバスの外へ)
//! - Alt+矢印: その方向で一番近い要素へ移る
//! - 矢印: 選択中の要素を動かす(Shift で 10 倍)。何も選んでいなければ画面を動かす
//! - Alt+Shift+矢印: その方向の要素とコネクタでつなぐ(なければ新しく置いてつなぐ)
//! - Enter: 付箋・図形などのツール中なら画面の中央に置く。選択中なら文字を入力
//! - ?: キーボード操作の一覧

use std::cell::RefCell;
use std::rc::Rc;

use board_core::a11y::{self, Dir};
use board_core::{ElementId, Tool, Vec2};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, KeyboardEvent, MouseEvent};

use super::{App, by_id, listen, now_ms};

/// 一覧に出す要素の上限(多すぎると読み上げも DOM も重くなる)。
const LIST_LIMIT: usize = 300;
/// 一覧を作り直す最短間隔(ミリ秒)。
const LIST_INTERVAL_MS: f64 = 700.0;
/// 読む順番を計算して位置(何番目か)を伝える上限。
const ORDER_LIMIT: usize = 20_000;

#[derive(Default)]
pub struct A11y {
    last_selection: Vec<ElementId>,
    last_tool: Option<Tool>,
    /// 交互に使うライブリージョン(同じ文でも読み直させるため)。
    flip: bool,
    /// 少し待ってから読む知らせ(矢印で動かしている間など)。
    pending: Option<(String, f64)>,
    list_rev: Option<u64>,
    list_view: Option<(Vec2, f32)>,
    list_built_at: f64,
    /// 読む順番のキャッシュ(ボードの版ごと)。
    order: Option<(u64, Vec<ElementId>)>,
}

fn tool_label(t: Tool) -> &'static str {
    match t {
        Tool::Select => "選択",
        Tool::Hand => "手のひら",
        Tool::Sticky => "付箋",
        Tool::Shape => "図形",
        Tool::Text => "テキスト",
        Tool::Connector => "コネクタ",
        Tool::Pen => "ペン",
        Tool::Highlighter => "蛍光ペン",
        Tool::Eraser => "消しゴム",
        Tool::Frame => "フレーム",
        Tool::Stamp => "スタンプ",
    }
}

fn creates_with_enter(t: Tool) -> bool {
    matches!(
        t,
        Tool::Sticky | Tool::Shape | Tool::Text | Tool::Frame | Tool::Stamp
    )
}

/// キー操作の対象がキャンバス(か、どこにもフォーカスがない状態)か。
fn on_canvas(e: &KeyboardEvent) -> bool {
    e.target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .is_none_or(|el| {
            let tag = el.tag_name();
            el.id() == "board" || tag == "BODY" || tag == "HTML"
        })
}

impl App {
    /// 画面読み上げで知らせる。
    pub(super) fn announce(&mut self, msg: &str) {
        self.a11y.pending = None;
        self.a11y.flip = !self.a11y.flip;
        let (on, off) = if self.a11y.flip {
            ("sr-live-a", "sr-live-b")
        } else {
            ("sr-live-b", "sr-live-a")
        };
        if let Ok(el) = by_id::<Element>(&self.document, off) {
            el.set_text_content(None);
        }
        if let Ok(el) = by_id::<Element>(&self.document, on) {
            el.set_text_content(Some(msg));
        }
    }

    /// `delay_ms` 後に知らせる(その前に別の知らせが来たら上書き)。
    fn announce_later(&mut self, msg: String, delay_ms: f64) {
        let at = now_ms(&self.window) + delay_ms;
        self.a11y.pending = Some((msg, at));
    }

    fn reading_order(&mut self) -> &[ElementId] {
        let rev = self.board.revision();
        if self.a11y.order.as_ref().is_none_or(|(r, _)| *r != rev) {
            self.a11y.order = Some((rev, a11y::reading_order(&self.board)));
        }
        &self.a11y.order.as_ref().expect("直前に入れた").1
    }

    /// 要素を 1 つ選び、画面に入っていなければそこへ表示を移す。
    pub(super) fn select_and_reveal(&mut self, id: ElementId) {
        self.editor.set_selection(vec![id]);
        self.reveal(id);
        self.dirty = true;
    }

    fn reveal(&mut self, id: ElementId) {
        let Some(r) = self.board.bounds_of(&[id]) else {
            return;
        };
        let (vmin, vmax) = self.camera.visible_world_rect();
        let view = board_core::Rect::new(vmin, vmax);
        if view.contains_rect(&r) {
            return;
        }
        let size = vmax - vmin;
        if r.size().x > size.x * 0.8 || r.size().y > size.y * 0.8 {
            self.camera.fit(r.min, r.max, 96.0);
        } else {
            self.camera.origin = r.center() - self.camera.viewport * 0.5 / self.camera.zoom;
        }
    }

    fn describe_selection(&mut self) -> String {
        let sel = self.editor.selection().to_vec();
        match sel.as_slice() {
            [] => "選択なし".to_string(),
            [id] => {
                let Some(e) = self.board.get(*id) else {
                    return String::new();
                };
                let mut s = a11y::describe(&self.board, e);
                if self.board.len() <= ORDER_LIMIT {
                    let id = *id;
                    let order = self.reading_order();
                    if let Some(i) = order.iter().position(|x| *x == id) {
                        s.push_str(&format!("。{} / {} 番目", i + 1, order.len()));
                    }
                }
                s
            }
            many => format!("{} 個を選択", many.len()),
        }
    }

    /// 毎フレーム: 選択・ツールの変化を知らせ、不可視の一覧を最新にする。
    pub(super) fn a11y_frame(&mut self, now: f64) {
        if self.editor.is_dragging() || self.editor.editing().is_some() {
            return;
        }
        let tool = self.editor.tool();
        if self.a11y.last_tool != Some(tool) {
            let first = self.a11y.last_tool.is_none();
            self.a11y.last_tool = Some(tool);
            if !first {
                let hint = if creates_with_enter(tool) {
                    "。Enter で画面の中央に置けます"
                } else {
                    ""
                };
                self.announce(&format!("{}ツール{hint}", tool_label(tool)));
            }
        }
        if self.editor.selection() != self.a11y.last_selection.as_slice() {
            let prev = std::mem::replace(
                &mut self.a11y.last_selection,
                self.editor.selection().to_vec(),
            );
            if !self.a11y.last_selection.is_empty() {
                let msg = self.describe_selection();
                self.announce(&msg);
            } else if !prev.is_empty() {
                let gone = prev.iter().all(|id| self.board.get(*id).is_none());
                self.announce(if gone {
                    "削除しました"
                } else {
                    "選択を解除"
                });
            }
        }
        if let Some((_, at)) = &self.a11y.pending
            && now >= *at
        {
            let (msg, _) = self.a11y.pending.take().expect("直前に確かめた");
            self.announce(&msg);
        }
        self.refresh_sr_list(now);
    }

    /// 不可視の要素一覧を作り直す(変わったときだけ、間隔をあけて)。
    fn refresh_sr_list(&mut self, now: f64) {
        let rev = self.board.revision();
        let big = self.board.len() > LIST_LIMIT;
        let view = (self.camera.origin, self.camera.zoom);
        let changed = self.a11y.list_rev != Some(rev) || (big && self.a11y.list_view != Some(view));
        if !changed || now - self.a11y.list_built_at < LIST_INTERVAL_MS {
            return;
        }
        let (Ok(list), Ok(heading)) = (
            by_id::<Element>(&self.document, "sr-board-list"),
            by_id::<Element>(&self.document, "sr-board-summary"),
        ) else {
            return;
        };
        self.a11y.list_rev = Some(rev);
        self.a11y.list_view = Some(view);
        self.a11y.list_built_at = now;

        let ids: Vec<ElementId> = if big {
            let (vmin, vmax) = self.camera.visible_world_rect();
            let r = board_core::Rect::new(vmin, vmax);
            let items = self
                .board
                .query_visible(&r)
                .into_iter()
                .take(LIST_LIMIT)
                .map(|e| (e.id(), e.bounds()))
                .collect();
            a11y::reading_order_of(items)
        } else {
            self.reading_order().to_vec()
        };
        let summary = a11y::summary(&self.board);
        let head = if big {
            format!("{summary}。画面に見えている {} 個を表示", ids.len())
        } else {
            summary
        };
        heading.set_text_content(Some(&head));
        list.set_inner_html("");
        for id in ids {
            let Some(e) = self.board.get(id) else {
                continue;
            };
            let (Ok(li), Ok(b)) = (
                self.document.create_element("li"),
                self.document.create_element("button"),
            ) else {
                continue;
            };
            let _ = b.set_attribute("type", "button");
            let _ = b.set_attribute("tabindex", "-1");
            let _ = b.set_attribute("data-id", &id.0.to_string());
            b.set_text_content(Some(&a11y::describe(&self.board, e)));
            let _ = li.append_child(&b);
            let _ = list.append_child(&li);
        }
    }

    /// 矢印で選択中の要素を動かす。
    fn nudge(&mut self, dir: Dir, big: bool) {
        let d = dir.vector() * if big { 10.0 } else { 1.0 };
        let sel = self.editor.selection().to_vec();
        self.board.end_gesture();
        if a11y::nudge(&mut self.board, &sel, d) == 0 {
            self.announce("ロック中のため動かせません");
            return;
        }
        self.board.end_gesture();
        self.dirty = true;
        if let Some(r) = self.board.bounds_of(&sel) {
            self.announce_later(
                format!(
                    "{}へ移動。位置 横 {:.0}、縦 {:.0}",
                    dir.label(),
                    r.min.x,
                    r.min.y
                ),
                500.0,
            );
        }
    }

    /// Alt+Shift+矢印: 選択中の要素を、その方向の要素とつなぐ。
    fn connect_toward(&mut self, dir: Dir) {
        let [from] = *self.editor.selection() else {
            self.announce("つなぐには要素を 1 つだけ選んでください");
            return;
        };
        self.board.end_gesture();
        let (to, created) = match a11y::nearest_in_direction(&self.board, from, dir) {
            Some(to) => (to, false),
            None => match a11y::place_sibling(&mut self.board, from, dir) {
                Some(to) => (to, true),
                None => {
                    self.announce(&format!("{}に つなげる要素がありません", dir.label()));
                    return;
                }
            },
        };
        let style = self.editor.connector_style();
        let linked = a11y::connect(&mut self.board, from, to, style).is_some();
        self.board.end_gesture();
        self.select_and_reveal(to);
        // 選択の変化の知らせより先に結果を伝える。
        self.a11y.last_selection = vec![to];
        let target = self
            .board
            .get(to)
            .map(|e| a11y::describe(&self.board, e))
            .unwrap_or_default();
        let msg = match (created, linked) {
            (true, _) => format!("{}に新しく置いてつなぎました。{target}", dir.label()),
            (false, true) => format!("つなぎました。{target}"),
            (false, false) => format!("すでにつながっています。{target}"),
        };
        self.announce(&msg);
        if created {
            self.begin_text_edit(to);
        }
    }

    /// Enter: 今のツールで画面の中央(選択中ならその右)に要素を置く。
    fn create_with_keyboard(&mut self) -> bool {
        let tool = self.editor.tool();
        if !creates_with_enter(tool) {
            return false;
        }
        let center = match self.board.bounds_of(self.editor.selection()) {
            Some(r) => Vec2::new(r.max.x + 140.0, r.center().y),
            None => {
                let (a, b) = self.camera.visible_world_rect();
                (a + b) * 0.5
            }
        };
        self.board.end_gesture();
        let Some(id) = self.editor.create_at(&mut self.board, center) else {
            return false;
        };
        self.board.end_gesture();
        self.select_and_reveal(id);
        self.a11y.last_selection = vec![id];
        self.announce(&format!(
            "{}を置きました",
            self.board.get(id).map_or("要素", |e| a11y::kind_label(e))
        ));
        if tool != Tool::Stamp {
            self.begin_text_edit(id);
        }
        true
    }

    fn pan_with_keyboard(&mut self, dir: Dir, big: bool) {
        let dpr = self.window.device_pixel_ratio() as f32;
        let step = if big { 320.0 } else { 80.0 } * dpr;
        self.camera.pan_by_screen(-dir.vector() * step);
        self.dirty = true;
    }

    /// キーボード操作(キャンバス上)。扱ったら true。
    pub(super) fn a11y_key(&mut self, e: &KeyboardEvent) -> bool {
        let key = e.key();
        let ctrl = e.ctrl_key() || e.meta_key();
        if key == "?" && !ctrl && !e.alt_key() {
            self.toggle_shortcuts();
            return true;
        }
        if !on_canvas(e) || ctrl {
            return false;
        }
        if key == "Tab" && !e.alt_key() {
            let current = match self.editor.selection() {
                [id] => Some(*id),
                _ => None,
            };
            let next = {
                let order = self.reading_order();
                a11y::step(order, current, !e.shift_key())
            };
            return match next {
                Some(id) => {
                    self.select_and_reveal(id);
                    true
                }
                // 端まで来たらフォーカスをキャンバスの外へ出す(閉じ込めない)。
                None => {
                    if current.is_some() {
                        self.editor.clear_selection();
                        self.dirty = true;
                    }
                    false
                }
            };
        }
        if let Some(dir) = Dir::from_key(&key) {
            let has_sel = !self.editor.selection().is_empty();
            if e.alt_key() && e.shift_key() {
                if self.can_edit() {
                    self.connect_toward(dir);
                }
                return true;
            }
            if e.alt_key() {
                let next = match self.editor.selection() {
                    [] => {
                        let (a, b) = self.camera.visible_world_rect();
                        a11y::nearest_to(&self.board, (a + b) * 0.5)
                    }
                    sel => a11y::nearest_in_direction(&self.board, sel[0], dir),
                };
                match next {
                    Some(id) => self.select_and_reveal(id),
                    None => self.announce(&format!("{}には要素がありません", dir.label())),
                }
                return true;
            }
            if has_sel && self.can_edit() {
                self.nudge(dir, e.shift_key());
            } else {
                self.pan_with_keyboard(dir, e.shift_key());
            }
            return true;
        }
        if key == "ContextMenu" || (key == "F10" && e.shift_key()) {
            self.open_context_menu_by_keyboard();
            return true;
        }
        if !self.can_edit() {
            return false;
        }
        match key.as_str() {
            "Enter" if !e.shift_key() && !e.alt_key() => self.create_with_keyboard(),
            "F2" => {
                if let [id] = *self.editor.selection() {
                    self.begin_text_edit(id);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// 右クリックメニューを選択中の要素(なければ画面の中央)に開き、先頭の項目にフォーカスする。
    fn open_context_menu_by_keyboard(&mut self) {
        let dpr = self.window.device_pixel_ratio() as f32;
        let world = match self.board.bounds_of(self.editor.selection()) {
            Some(r) => r.center(),
            None => {
                let (a, b) = self.camera.visible_world_rect();
                (a + b) * 0.5
            }
        };
        let p = self.camera.world_to_screen(world) / dpr;
        let rect = self.canvas.get_bounding_client_rect();
        let init = web_sys::MouseEventInit::new();
        init.set_bubbles(true);
        init.set_cancelable(true);
        init.set_client_x((rect.left() as f32 + p.x) as i32);
        init.set_client_y((rect.top() as f32 + p.y) as i32);
        let Ok(ev) = MouseEvent::new_with_mouse_event_init_dict("contextmenu", &init) else {
            return;
        };
        let canvas = self.canvas.clone();
        let doc = self.document.clone();
        // メニューの組み立ては App を借りるので、今の借用が終わってから送る。
        wasm_bindgen_futures::spawn_local(async move {
            let _ = canvas.dispatch_event(&ev);
            focus_first_item(&doc, "#context-menu");
        });
    }

    fn toggle_shortcuts(&mut self) {
        let Ok(panel) = by_id::<HtmlElement>(&self.document, "shortcuts") else {
            return;
        };
        if panel.hidden() {
            panel.set_hidden(false);
            if let Ok(b) = by_id::<HtmlElement>(&self.document, "shortcuts-close") {
                let _ = b.focus();
            }
        } else {
            self.close_shortcuts();
        }
    }

    pub(super) fn close_shortcuts(&mut self) -> bool {
        let Ok(panel) = by_id::<HtmlElement>(&self.document, "shortcuts") else {
            return false;
        };
        if panel.hidden() {
            return false;
        }
        panel.set_hidden(true);
        let _ = self.canvas.focus();
        true
    }

    /// 文字入力欄の読み上げ名を、編集している要素に合わせる。
    pub(super) fn label_text_editor(&self, id: ElementId) {
        let kind = self.board.get(id).map_or("要素", a11y::kind_label);
        let _ = self.text_editor.set_attribute(
            "aria-label",
            &format!("{kind}の文字を入力。Esc か Ctrl+Enter で確定"),
        );
    }
}

/// メニュー内の、押せる最初の項目にフォーカスする。
fn focus_first_item(doc: &web_sys::Document, menu: &str) {
    let sel = format!("{menu} button:not([disabled]):not([hidden])");
    if let Ok(Some(b)) = doc.query_selector(&sel)
        && let Ok(b) = b.dyn_into::<HtmlElement>()
    {
        let _ = b.focus();
    }
}

/// メニュー(`role="menu"`)の中のキー操作: 上下で項目を移り、Esc で閉じて元の場所へ戻る。
fn menu_key(canvas: &HtmlElement, e: &KeyboardEvent) {
    let Some(target) = e.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
        return;
    };
    let Ok(Some(menu)) = target.closest("[role=menu]") else {
        return;
    };
    let key = e.key();
    match key.as_str() {
        "ArrowDown" | "ArrowUp" | "ArrowRight" | "ArrowLeft" | "Home" | "End" => {
            let Ok(list) = menu.query_selector_all("button:not([disabled]):not([hidden])") else {
                return;
            };
            let items: Vec<HtmlElement> = (0..list.length())
                .filter_map(|i| list.item(i)?.dyn_into::<HtmlElement>().ok())
                .filter(|b| b.offset_parent().is_some())
                .collect();
            if items.is_empty() {
                return;
            }
            let n = items.len();
            let cur = items
                .iter()
                .position(|b| {
                    let el: &Element = b.as_ref();
                    el == &target
                })
                .unwrap_or(0);
            let next = match key.as_str() {
                "ArrowDown" | "ArrowRight" => (cur + 1) % n,
                "ArrowUp" | "ArrowLeft" => (cur + n - 1) % n,
                "Home" => 0,
                _ => n - 1,
            };
            e.prevent_default();
            e.stop_propagation();
            let _ = items[next].focus();
        }
        "Escape" => {
            e.prevent_default();
            e.stop_propagation();
            let _ = menu.remove_attribute("data-open");
            // ポップオーバーなら開いたボタンへ、右クリックメニューならキャンバスへ戻る。
            let back = menu
                .parent_element()
                .filter(|g| g.class_list().contains("tool-group"))
                .and_then(|g| g.query_selector("button[aria-haspopup]").ok().flatten())
                .and_then(|b| b.dyn_into::<HtmlElement>().ok());
            match back {
                Some(b) => {
                    let _ = b.focus();
                }
                None => {
                    let _ = canvas.focus();
                }
            }
        }
        _ => {}
    }
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let canvas: HtmlElement = app.borrow().canvas.clone().into();
    {
        listen(doc.as_ref(), "keydown", true, move |e: KeyboardEvent| {
            menu_key(&canvas, &e);
        })?;
    }
    // ポップオーバーをキーボードで開いたら(click の detail が 0)、中の先頭へフォーカスする。
    for (button, menu) in [
        ("tool-stamp", "#stamp-picker"),
        ("templates-button", "#template-picker"),
    ] {
        let Ok(b) = by_id::<Element>(&doc, button) else {
            continue;
        };
        let doc2 = doc.clone();
        let b2 = b.clone();
        listen(b.as_ref(), "click", false, move |e: MouseEvent| {
            let open = doc2
                .query_selector(&format!("{menu}[data-open]"))
                .ok()
                .flatten()
                .is_some();
            let _ = b2.set_attribute("aria-expanded", if open { "true" } else { "false" });
            if open && e.detail() == 0 {
                focus_first_item(&doc2, menu);
            }
        })?;
    }
    // 不可視の一覧のボタン: 押すとその要素を選んでキャンバスに戻る。
    let list: Element = by_id(&doc, "sr-board-list")?;
    {
        let app = app.clone();
        listen(list.as_ref(), "click", false, move |e: MouseEvent| {
            let Some(id) = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|el| el.closest("button[data-id]").ok().flatten())
                .and_then(|b| b.get_attribute("data-id"))
                .and_then(|s| s.parse::<u64>().ok())
            else {
                return;
            };
            let mut a = app.borrow_mut();
            a.select_and_reveal(ElementId(id));
            let _ = a.canvas.focus();
        })?;
    }
    for id in ["shortcuts-close", "shortcuts-button"] {
        let Ok(el) = by_id::<Element>(&doc, id) else {
            continue;
        };
        let app = app.clone();
        let close = id == "shortcuts-close";
        listen(el.as_ref(), "click", false, move |_: MouseEvent| {
            let mut a = app.borrow_mut();
            if close {
                a.close_shortcuts();
            } else {
                a.toggle_shortcuts();
            }
        })?;
    }
    let panel: Element = by_id(&doc, "shortcuts")?;
    {
        let app = app.clone();
        listen(panel.as_ref(), "keydown", false, move |e: KeyboardEvent| {
            if e.key() == "Escape" {
                e.prevent_default();
                e.stop_propagation();
                app.borrow_mut().close_shortcuts();
            }
        })?;
    }
    Ok(())
}
