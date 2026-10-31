//! 選択した要素への操作(コピー&ペースト、グループ化、整列、重なり順、ロック)と、
//! そのショートカット・右クリックメニュー。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::arrange::{self, Align, Order};
use wasm_bindgen::JsCast;
use web_sys::{ClipboardEvent, Element, HtmlElement, KeyboardEvent, MouseEvent};

use super::{App, js_err, listen};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Copy,
    Cut,
    Paste,
    Duplicate,
    Delete,
    SelectAll,
    Group,
    Ungroup,
    Lock,
    UnlockAll,
    Order(Order),
    Align(Align),
    Distribute { horizontal: bool },
    Tidy,
}

thread_local! {
    /// システムのクリップボードが使えないとき(メニューからの貼り付け)用の控え。
    static CLIPBOARD: RefCell<Option<String>> = const { RefCell::new(None) };
    /// 右クリックした位置(ワールド座標)。メニューからの貼り付け先。
    static MENU_POS: RefCell<Option<board_core::Vec2>> = const { RefCell::new(None) };
}

/// ショートカットキーに対応する操作。
pub fn action_for_key(e: &KeyboardEvent) -> Option<Action> {
    let ctrl = e.ctrl_key() || e.meta_key();
    let shift = e.shift_key();
    let alt = e.alt_key();
    let code = e.code();
    Some(match (ctrl, shift, alt, code.as_str()) {
        (true, false, false, "KeyD") => Action::Duplicate,
        (true, false, false, "KeyG") => Action::Group,
        (true, true, false, "KeyG") => Action::Ungroup,
        (true, true, false, "KeyL") => Action::Lock,
        (true, true, false, "BracketRight") => Action::Order(Order::Front),
        (true, false, false, "BracketRight") => Action::Order(Order::Forward),
        (true, false, false, "BracketLeft") => Action::Order(Order::Backward),
        (true, true, false, "BracketLeft") => Action::Order(Order::Back),
        (false, false, true, "KeyA") => Action::Align(Align::Left),
        (false, false, true, "KeyH") => Action::Align(Align::CenterX),
        (false, false, true, "KeyD") => Action::Align(Align::Right),
        (false, false, true, "KeyW") => Action::Align(Align::Top),
        (false, false, true, "KeyV") => Action::Align(Align::CenterY),
        (false, false, true, "KeyS") => Action::Align(Align::Bottom),
        (false, true, true, "KeyH") => Action::Distribute { horizontal: true },
        (false, true, true, "KeyV") => Action::Distribute { horizontal: false },
        (false, true, true, "KeyT") => Action::Tidy,
        _ => return None,
    })
}

impl App {
    /// 選択の数などから、その操作が今できるか。
    fn can(&self, a: Action) -> bool {
        // 見るだけの人はコピーと全選択だけ。
        if !self.can_edit() && !matches!(a, Action::Copy | Action::SelectAll) {
            return false;
        }
        let n = self.editor.selection().len();
        match a {
            Action::Paste => CLIPBOARD.with(|c| c.borrow().is_some()),
            Action::SelectAll | Action::UnlockAll => !self.board.is_empty(),
            Action::Group | Action::Align(_) => n >= 2,
            Action::Distribute { .. } => n >= 3,
            Action::Tidy => n >= 2,
            Action::Ungroup => self
                .editor
                .selection()
                .iter()
                .any(|id| self.board.get(*id).is_some_and(|e| e.meta().group != 0)),
            _ => n > 0,
        }
    }

    pub(super) fn run_action(&mut self, a: Action) {
        if self.editor.is_dragging() || self.editor.editing().is_some() {
            return;
        }
        if !self.can_edit() && !matches!(a, Action::Copy | Action::SelectAll) {
            return;
        }
        let sel = self.editor.selection().to_vec();
        self.board.end_gesture();
        match a {
            Action::Copy => {
                self.copy_selection();
            }
            Action::Cut => {
                self.copy_selection();
                self.editor.delete_selection(&mut self.board);
            }
            Action::Paste => {
                if let Some(text) = CLIPBOARD.with(|c| c.borrow().clone()) {
                    let at = MENU_POS.with(|p| p.borrow_mut().take());
                    self.paste_text(&text, at);
                }
            }
            Action::Duplicate => {
                let ids = arrange::duplicate(&mut self.board, &sel);
                self.editor.set_selection(ids);
            }
            Action::Delete => {
                self.editor.delete_selection(&mut self.board);
            }
            Action::SelectAll => self.editor.select_all(&self.board),
            Action::Group => {
                arrange::group(&mut self.board, &sel);
            }
            Action::Ungroup => arrange::ungroup(&mut self.board, &sel),
            Action::Lock => {
                if arrange::toggle_lock(&mut self.board, &sel) {
                    self.editor.clear_selection();
                }
            }
            Action::UnlockAll => {
                let ids = arrange::unlock_all(&mut self.board);
                self.editor.set_selection(ids);
            }
            Action::Order(o) => arrange::reorder(&mut self.board, &sel, o),
            Action::Align(how) => arrange::align(&mut self.board, &sel, how),
            Action::Distribute { horizontal } => {
                arrange::distribute(&mut self.board, &sel, horizontal)
            }
            Action::Tidy => arrange::tidy(&mut self.board, &sel),
        }
        self.board.end_gesture();
        self.editor.prune_selection(&self.board);
        self.dirty = true;
    }

    fn copy_selection(&mut self) -> Option<String> {
        let text = arrange::copy(&self.board, self.editor.selection())?;
        CLIPBOARD.with(|c| *c.borrow_mut() = Some(text.clone()));
        Some(text)
    }

    /// 貼り付ける。中身の中心をカーソル(なければ画面中央)に合わせる。
    fn paste_text(&mut self, text: &str, at: Option<board_core::Vec2>) -> bool {
        if !self.can_edit() {
            return false;
        }
        let Some(elements) = arrange::parse_clipboard(text) else {
            return false;
        };
        let Some(bounds) = elements
            .iter()
            .map(|e| e.bounds())
            .reduce(|a, b| a.union(&b))
        else {
            return false;
        };
        let target = at
            .or(self.cursor_world)
            .unwrap_or_else(|| self.camera.screen_to_world(self.camera.viewport * 0.5));
        let ids = arrange::paste(&mut self.board, elements, target - bounds.center());
        self.editor.set_selection(ids);
        self.board.end_gesture();
        self.dirty = true;
        true
    }
}

/// 入力欄にフォーカスがあるときはブラウザに任せる。
fn in_text_field(doc: &web_sys::Document) -> bool {
    doc.active_element().is_some_and(|el| {
        let tag = el.tag_name();
        tag == "INPUT" || tag == "TEXTAREA" || el.has_attribute("contenteditable")
    })
}

/// コピー・切り取り・貼り付け(システムのクリップボード)。
pub fn install_clipboard(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let target: &web_sys::EventTarget = doc.as_ref();
    for (name, cut) in [("copy", false), ("cut", true)] {
        let app = app.clone();
        let doc2 = doc.clone();
        listen(target, name, false, move |e: ClipboardEvent| {
            if in_text_field(&doc2) {
                return;
            }
            let mut a = app.borrow_mut();
            if a.editor.editing().is_some() {
                return;
            }
            let Some(text) = a.copy_selection() else {
                return;
            };
            if let Some(dt) = e.clipboard_data() {
                let _ = dt.set_data("text/plain", &text);
                e.prevent_default();
            }
            if cut {
                a.run_action(Action::Delete);
            }
        })?;
    }
    {
        let app = app.clone();
        let doc2 = doc.clone();
        listen(target, "paste", false, move |e: ClipboardEvent| {
            if in_text_field(&doc2) {
                return;
            }
            let Some(text) = e
                .clipboard_data()
                .and_then(|d| d.get_data("text/plain").ok())
            else {
                return;
            };
            let mut a = app.borrow_mut();
            if a.paste_text(&text, None) {
                e.prevent_default();
            } else if !text.trim().is_empty() {
                // ふつうの文字列は付箋にして貼る。
                e.prevent_default();
                a.paste_plain_text(text.trim());
            }
        })?;
    }
    Ok(())
}

impl App {
    fn paste_plain_text(&mut self, text: &str) {
        if !self.can_edit() {
            return;
        }
        let target = self
            .cursor_world
            .unwrap_or_else(|| self.camera.screen_to_world(self.camera.viewport * 0.5));
        let id = self.board.alloc_id();
        let mut note = board_core::Shape::sticky(id, target, self.editor.sticky_color);
        note.pos = target - note.size * 0.5;
        note.text = text.chars().take(2000).collect();
        self.board.end_gesture();
        self.board.push(note);
        self.board.end_gesture();
        self.editor.set_selection(vec![id]);
        self.dirty = true;
    }
}

// ---- 右クリックメニュー ----

enum Item {
    Action(&'static str, &'static str, Action),
    Sep,
}

fn menu_items() -> Vec<Item> {
    use Item::{Action as A, Sep};
    vec![
        A("切り取り", "Ctrl+X", Action::Cut),
        A("コピー", "Ctrl+C", Action::Copy),
        A("貼り付け", "Ctrl+V", Action::Paste),
        A("複製", "Ctrl+D", Action::Duplicate),
        A("削除", "Delete", Action::Delete),
        Sep,
        A("グループ化", "Ctrl+G", Action::Group),
        A("グループ解除", "Ctrl+Shift+G", Action::Ungroup),
        A("ロック / 解除", "Ctrl+Shift+L", Action::Lock),
        A("すべてのロックを解除", "", Action::UnlockAll),
        Sep,
        A("最前面へ", "Ctrl+Shift+]", Action::Order(Order::Front)),
        A("前面へ", "Ctrl+]", Action::Order(Order::Forward)),
        A("背面へ", "Ctrl+[", Action::Order(Order::Backward)),
        A("最背面へ", "Ctrl+Shift+[", Action::Order(Order::Back)),
        Sep,
        A("左揃え", "Alt+A", Action::Align(Align::Left)),
        A("左右中央揃え", "Alt+H", Action::Align(Align::CenterX)),
        A("右揃え", "Alt+D", Action::Align(Align::Right)),
        A("上揃え", "Alt+W", Action::Align(Align::Top)),
        A("上下中央揃え", "Alt+V", Action::Align(Align::CenterY)),
        A("下揃え", "Alt+S", Action::Align(Align::Bottom)),
        A(
            "横に等間隔",
            "Alt+Shift+H",
            Action::Distribute { horizontal: true },
        ),
        A(
            "縦に等間隔",
            "Alt+Shift+V",
            Action::Distribute { horizontal: false },
        ),
        A("整頓(格子に並べる)", "Alt+Shift+T", Action::Tidy),
        Sep,
        A("すべて選択", "Ctrl+A", Action::SelectAll),
    ]
}

pub fn install_context_menu(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let (doc, canvas) = {
        let a = app.borrow();
        (a.document.clone(), a.canvas.clone())
    };
    let menu: HtmlElement = doc
        .create_element("div")
        .map_err(js_err)?
        .dyn_into()
        .map_err(|_| "メニューを作れません".to_string())?;
    menu.set_id("context-menu");
    menu.set_attribute("role", "menu").map_err(js_err)?;
    doc.body()
        .ok_or("body がありません")?
        .append_child(&menu)
        .map_err(js_err)?;

    let actions = Rc::new(RefCell::new(Vec::<(Element, Action)>::new()));
    for item in menu_items() {
        match item {
            Item::Sep => {
                let hr = doc.create_element("hr").map_err(js_err)?;
                menu.append_child(&hr).map_err(js_err)?;
            }
            Item::Action(label, keys, action) => {
                let b = doc.create_element("button").map_err(js_err)?;
                b.set_attribute("role", "menuitem").map_err(js_err)?;
                b.set_inner_html(&format!("<span>{label}</span><kbd>{keys}</kbd>"));
                menu.append_child(&b).map_err(js_err)?;
                actions.borrow_mut().push((b.clone(), action));
                let app = app.clone();
                let menu2 = menu.clone();
                listen(b.as_ref(), "click", false, move |_: MouseEvent| {
                    let _ = menu2.remove_attribute("data-open");
                    app.borrow_mut().run_action(action);
                })?;
            }
        }
    }

    {
        let app = app.clone();
        let menu2 = menu.clone();
        let actions = actions.clone();
        listen(
            canvas.as_ref(),
            "contextmenu",
            false,
            move |e: MouseEvent| {
                e.prevent_default();
                let mut a = app.borrow_mut();
                let p = a.css_to_physical(e.client_x() as f64, e.client_y() as f64);
                let App {
                    editor,
                    board,
                    camera,
                    ..
                } = &mut *a;
                editor.select_at(board, camera, p);
                let world = camera.screen_to_world(p);
                MENU_POS.with(|m| *m.borrow_mut() = Some(world));
                a.dirty = true;
                for (el, action) in actions.borrow().iter() {
                    if a.can(*action) {
                        let _ = el.remove_attribute("disabled");
                    } else {
                        let _ = el.set_attribute("disabled", "");
                    }
                }
                let style = menu2.style();
                let _ = menu2.set_attribute("data-open", "");
                // 画面からはみ出さないよう位置を調整する。
                let w = a
                    .window
                    .inner_width()
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let h = a
                    .window
                    .inner_height()
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let mw = menu2.offset_width() as f64;
                let mh = menu2.offset_height() as f64;
                let x = (e.client_x() as f64).min(w - mw - 8.0).max(8.0);
                let y = (e.client_y() as f64).min(h - mh - 8.0).max(8.0);
                let _ = style.set_property("left", &format!("{x}px"));
                let _ = style.set_property("top", &format!("{y}px"));
            },
        )?;
    }
    // メニューの外を押したら閉じる。
    {
        let menu2 = menu.clone();
        let window = app.borrow().window.clone();
        listen(
            window.as_ref(),
            "pointerdown",
            false,
            move |e: web_sys::PointerEvent| {
                let inside = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                    .is_some_and(|n| menu2.contains(Some(&n)));
                if !inside {
                    let _ = menu2.remove_attribute("data-open");
                }
            },
        )?;
    }
    {
        let menu2 = menu.clone();
        let window = app.borrow().window.clone();
        listen(
            window.as_ref(),
            "keydown",
            false,
            move |e: KeyboardEvent| {
                if e.key() == "Escape" {
                    let _ = menu2.remove_attribute("data-open");
                }
            },
        )?;
    }
    Ok(())
}
