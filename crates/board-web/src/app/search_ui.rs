//! ボード内の検索(Ctrl/⌘+F)。見つかった要素を順に選び、そこへ表示を移す。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::ElementId;
use board_core::search;
use web_sys::{Element, HtmlElement, HtmlInputElement, KeyboardEvent, MouseEvent};

use super::{App, by_id, listen};

#[derive(Default)]
pub struct Search {
    hits: Vec<ElementId>,
    current: usize,
}

impl App {
    /// 検索に当たった要素(個人作業エリアで伏せているものは除く)。
    fn visible_hits(&self, q: &str) -> Vec<board_core::ElementId> {
        search::find(&self.board, q)
            .into_iter()
            .filter(|id| !self.hidden_elements().contains(id))
            .collect()
    }

    fn search_input(&self) -> Option<HtmlInputElement> {
        by_id(&self.document, "search-input").ok()
    }

    pub(super) fn open_search(&mut self) {
        let (Ok(panel), Some(input)) = (
            by_id::<HtmlElement>(&self.document, "search-box"),
            self.search_input(),
        ) else {
            return;
        };
        panel.set_hidden(false);
        let _ = input.focus();
        input.select();
        self.run_search();
    }

    fn close_search(&mut self) {
        if let Ok(panel) = by_id::<HtmlElement>(&self.document, "search-box") {
            panel.set_hidden(true);
        }
        self.search = Search::default();
        let _ = self.canvas.focus();
    }

    fn run_search(&mut self) {
        let q = self.search_input().map(|i| i.value()).unwrap_or_default();
        self.search.hits = self.visible_hits(&q);
        self.search.current = 0;
        self.show_hit();
    }

    fn step_search(&mut self, forward: bool) {
        // 編集で文字が変わっているかもしれないので探し直す(位置は保つ)。
        let q = self.search_input().map(|i| i.value()).unwrap_or_default();
        let hits = self.visible_hits(&q);
        let n = hits.len();
        if n == 0 {
            self.search.hits = hits;
            self.show_hit();
            return;
        }
        let cur = self
            .search
            .hits
            .get(self.search.current)
            .and_then(|id| hits.iter().position(|h| h == id));
        self.search.current = match (cur, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, _) => 0,
        };
        self.search.hits = hits;
        self.show_hit();
    }

    /// 今の候補を選んで表示し、件数を出す。
    fn show_hit(&mut self) {
        let count = by_id::<Element>(&self.document, "search-count").ok();
        let n = self.search.hits.len();
        let q_empty = self
            .search_input()
            .is_none_or(|i| i.value().trim().is_empty());
        if let Some(c) = &count {
            let text = match (n, q_empty) {
                (_, true) => String::new(),
                (0, false) => "見つかりません".to_string(),
                _ => format!("{} / {n}", self.search.current + 1),
            };
            c.set_text_content(Some(&text));
        }
        let Some(&id) = self.search.hits.get(self.search.current) else {
            return;
        };
        self.editor.set_selection(vec![id]);
        if let Some(r) = self.board.bounds_of(&[id]) {
            // 収まらなければ縮め、収まるなら倍率はそのままで中央へ。
            let (vmin, vmax) = self.camera.visible_world_rect();
            let view = vmax - vmin;
            if r.size().x > view.x * 0.8 || r.size().y > view.y * 0.8 {
                self.camera.fit(r.min, r.max, 96.0);
            } else {
                let c = r.center();
                self.camera.origin = c - self.camera.viewport * 0.5 / self.camera.zoom;
            }
        }
        self.dirty = true;
    }
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let input: HtmlInputElement = by_id(&doc, "search-input")?;
    {
        let app = app.clone();
        listen(input.as_ref(), "input", false, move |_: web_sys::Event| {
            app.borrow_mut().run_search();
        })?;
    }
    {
        let app = app.clone();
        listen(input.as_ref(), "keydown", false, move |e: KeyboardEvent| {
            if e.is_composing() {
                return;
            }
            match e.key().as_str() {
                "Enter" => {
                    e.prevent_default();
                    app.borrow_mut().step_search(!e.shift_key());
                }
                "Escape" => {
                    e.prevent_default();
                    app.borrow_mut().close_search();
                }
                _ => {}
            }
        })?;
    }
    for (id, forward) in [("search-prev", false), ("search-next", true)] {
        let app = app.clone();
        let el: Element = by_id(&doc, id)?;
        listen(el.as_ref(), "click", false, move |_: MouseEvent| {
            app.borrow_mut().step_search(forward);
        })?;
    }
    {
        let app = app.clone();
        let el: Element = by_id(&doc, "search-close")?;
        listen(el.as_ref(), "click", false, move |_: MouseEvent| {
            app.borrow_mut().close_search();
        })?;
    }
    Ok(())
}
