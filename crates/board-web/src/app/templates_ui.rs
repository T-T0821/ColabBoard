//! テンプレートの選択。選んだテンプレートは今あるものの右に置き、そこへ表示を移す。

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use board_core::Vec2;
use board_core::templates::{self, Template};
use wasm_bindgen::JsCast;
use web_sys::{Element, MouseEvent};

use super::{App, by_id, listen};

impl App {
    fn insert_template(&mut self, t: Template) {
        if !self.can_edit() {
            return;
        }
        // 既存の要素と重ならないよう右側に置く(空なら画面の中央)。
        let center = match self.board.bounds() {
            Some(r) => Vec2::new(r.max.x + 200.0 + 600.0, r.center().y),
            None => self.camera.screen_to_world(self.camera.viewport * 0.5),
        };
        self.board.end_gesture();
        let ids = templates::insert(&mut self.board, t, center);
        self.board.end_gesture();
        if let Some(r) = self.board.bounds_of(&ids) {
            self.camera.fit(r.min, r.max, 96.0);
        }
        self.editor.set_selection(ids);
        self.dirty = true;
    }
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let picker: Element = by_id(&doc, "template-picker")?;
    let mut html = String::new();
    for t in Template::ALL {
        let _ = write!(
            html,
            r#"<button data-template="{}"><strong>{}</strong><span>{}</span></button>"#,
            t.name(),
            t.title(),
            t.description()
        );
    }
    picker.set_inner_html(&html);
    {
        let app = app.clone();
        let picker2 = picker.clone();
        listen(picker.as_ref(), "click", false, move |e: MouseEvent| {
            let Some(t) = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|t| t.closest("[data-template]").ok().flatten())
                .and_then(|b| b.get_attribute("data-template"))
                .and_then(|n| Template::from_name(&n))
            else {
                return;
            };
            let _ = picker2.remove_attribute("data-open");
            app.borrow_mut().insert_template(t);
        })?;
    }
    super::toolbar::popover(app, "templates-button", picker)
}
