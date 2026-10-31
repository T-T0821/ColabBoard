//! ボード全体の文字を使う AI 機能(ファシリテーションのパネル)。
//!
//! - 議事録: サーバー(`/ai/minutes`)が作ったマークダウンを、マークダウンの取り込みと同じ方法で
//!   ボードの右側に並べる(見出し → フレーム、箇条書き → 付箋、アクション → 表)。1 回の Undo で戻せる
//! - マインドマップ: `/ai/mindmap` が返した木を、マインドマップモードと同じ部品(`board_core::mindmap`)で
//!   ボードの右側に置く。置いた後は Tab / Enter で枝を足せる
//! - 質問: `/ai/ask` に質問を送り、答えと根拠の要素をパネルに出す。根拠を押すとその要素へ移動する。
//!   続けて質問すると、前のやり取りも踏まえて答える
//!
//! どちらも選択中の要素(フレームを選べば中身も)、なければボード全体が対象。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use board_core::{Element, ElementId, Vec2};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlElement, HtmlInputElement};

use super::ai::{api_url, post_json};
use super::ink_ui::toast;
use super::{App, by_id, listen};

/// 議事録を既存の内容から離す距離。
const GAP: f32 = 240.0;
/// マインドマップの片側の広がりの見込み(置く場所と表示の範囲に使う)。
const MINDMAP_HALF_WIDTH: f32 = 700.0;
/// パネルに残すやり取りの数。
const KEEP_TURNS: usize = 6;

#[derive(Clone)]
struct Turn {
    q: String,
    a: String,
    refs: Vec<ElementId>,
}

thread_local! {
    static MINUTES_BUSY: Cell<bool> = const { Cell::new(false) };
    static MINDMAP_BUSY: Cell<bool> = const { Cell::new(false) };
    static ASK_BUSY: Cell<bool> = const { Cell::new(false) };
    static TURNS: RefCell<Vec<Turn>> = const { RefCell::new(Vec::new()) };
    /// 会話を続けているボード(ボードを移ったら会話を捨てる)。
    static TURNS_ROOM: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn ids_json(ids: &[ElementId]) -> Vec<String> {
    ids.iter().map(|id| format!("{:x}", id.0)).collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn to_text(v: &wasm_bindgen::JsValue) -> String {
    js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .unwrap_or_default()
}

/// 今日の日付(YYYY-MM-DD、端末の時刻)。
fn today() -> String {
    let d = js_sys::Date::new_0();
    format!(
        "{:04}-{:02}-{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date()
    )
}

impl App {
    /// 根拠の要素の短い名前(ボタンに出す)。
    fn ref_label(&self, id: ElementId) -> Option<String> {
        let e = self.board.get(id)?;
        let text: String = e
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(18)
            .collect();
        let kind = match e {
            Element::Shape(s) => match s.kind {
                board_core::ShapeKind::Sticky => "付箋",
                board_core::ShapeKind::Frame => "フレーム",
                board_core::ShapeKind::Text => "文字",
                k => k.label(),
            },
            _ => "要素",
        };
        Some(if text.is_empty() {
            kind.to_string()
        } else {
            format!("{kind}「{text}」")
        })
    }

    /// 要素がすべて見えるように移動して選択する。
    fn focus_refs(&mut self, ids: &[ElementId]) {
        let ids: Vec<ElementId> = ids
            .iter()
            .copied()
            .filter(|id| self.board.get(*id).is_some())
            .collect();
        let Some(b) = self.board.bounds_of(&ids) else {
            toast(&self.document, "その要素はもうボードにありません", "error");
            return;
        };
        let pad = b.size().max_element().max(200.0) * 0.4;
        self.camera
            .fit(b.min - Vec2::splat(pad), b.max + Vec2::splat(pad), 48.0);
        self.editor.set_selection(ids);
        self.dirty = true;
    }

    /// 議事録のマークダウンをボードの右側に並べる。置いた要素の数を返す。
    fn place_minutes(&mut self, md: &str) -> usize {
        let before = self.board.bounds();
        let center = self.camera.screen_to_world(self.camera.viewport * 0.5);
        let sticky = self.editor.sticky_color;
        self.board.end_gesture();
        let Some(r) = board_core::markdown::insert(&mut self.board, md, center, sticky) else {
            return 0;
        };
        // 既存の内容と重ならないよう、右に並べる(取り込みと同じ Undo の区切りの中で)。
        if let (Some(all), Some(new)) = (before, self.board.bounds_of(&r.ids)) {
            let d = Vec2::new(all.max.x + GAP, all.min.y) - new.min;
            for id in &r.ids {
                self.board.update(*id, |e| e.translate(d));
            }
        }
        self.board.end_gesture();
        let n = r.ids.len();
        self.focus_refs(&r.ids);
        n
    }

    /// マインドマップの木をボードの右側に置き、中心を選ぶ。
    fn place_mindmap(&mut self, tree: &board_core::mindmap::MindTree) {
        let center = match self.board.bounds() {
            // 枝の広がりを見込んで、既存の内容の右に離して置く。
            Some(all) => Vec2::new(all.max.x + GAP + MINDMAP_HALF_WIDTH, all.center().y),
            None => self.camera.screen_to_world(self.camera.viewport * 0.5),
        };
        self.board.end_gesture();
        let root = board_core::mindmap::insert_tree(
            &mut self.board,
            tree,
            center,
            board_core::mindmap::MindLayout::default(),
        );
        self.board.end_gesture();
        // 自動レイアウトは次のフレームで走るので、広がりを見込んで中心の周りを映す。
        let half = Vec2::new(MINDMAP_HALF_WIDTH, MINDMAP_HALF_WIDTH * 0.6);
        self.camera.fit(center - half, center + half, 48.0);
        self.editor.set_selection(vec![root]);
        self.dirty = true;
    }

    /// パネルの答えの欄を作り直す。
    fn render_answers(&self) {
        let Some(el) = self
            .document
            .get_element_by_id("ai-answer")
            .and_then(|e| e.dyn_into::<HtmlElement>().ok())
        else {
            return;
        };
        let turns = TURNS.with(|t| t.borrow().clone());
        if turns.is_empty() {
            el.set_hidden(true);
            el.set_inner_html("");
            return;
        }
        let mut h = String::new();
        for (i, t) in turns.iter().enumerate() {
            h.push_str(&format!(
                r#"<p class="q" translate="no">Q. {}</p><p class="a" translate="no">{}</p>"#,
                esc(&t.q),
                esc(&t.a)
            ));
            let refs: Vec<(ElementId, String)> = t
                .refs
                .iter()
                .filter_map(|id| self.ref_label(*id).map(|l| (*id, l)))
                .collect();
            if !refs.is_empty() {
                h.push_str(r#"<div class="refs">"#);
                if refs.len() > 1 {
                    h.push_str(&format!(
                        r#"<button type="button" data-turn="{i}" title="根拠をすべて表示">根拠 {} 件</button>"#,
                        refs.len()
                    ));
                }
                for (id, label) in refs {
                    h.push_str(&format!(
                        r#"<button type="button" data-ref="{:x}" title="{}" translate="no">{}</button>"#,
                        id.0,
                        esc(&label),
                        esc(&label)
                    ));
                }
                h.push_str("</div>");
            }
        }
        h.push_str(r#"<div class="tools"><button type="button" data-act="clear">会話を消す</button></div>"#);
        el.set_inner_html(&h);
        el.set_hidden(false);
        el.set_scroll_top(el.scroll_height());
    }
}

fn start_minutes(app: &Rc<RefCell<App>>) {
    if MINUTES_BUSY.with(Cell::get) {
        return;
    }
    let (window, doc, url, body) = {
        let a = app.borrow();
        if !a.can_edit() {
            return;
        }
        let Some(room) = a.room.clone() else {
            toast(
                &a.document,
                "AI 機能は共同編集のボードでだけ使えます",
                "error",
            );
            return;
        };
        let Some(url) = api_url(&a.window, &room, "minutes") else {
            return;
        };
        let body = json!({ "ids": ids_json(a.editor.selection()), "date": today() }).to_string();
        (a.window.clone(), a.document.clone(), url, body)
    };
    MINUTES_BUSY.with(|b| b.set(true));
    toast(
        &doc,
        "AI が議事録を書いています…(数十秒かかることがあります)",
        "busy",
    );
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = post_json(&window, &url, &body).await;
        MINUTES_BUSY.with(|b| b.set(false));
        let v: Value = match result {
            Ok(v) => serde_json::from_str(&to_text(&v)).unwrap_or_default(),
            Err(e) => {
                toast(&doc, &e, "error");
                return;
            }
        };
        let md = v["markdown"].as_str().unwrap_or_default();
        let n = app.borrow_mut().place_minutes(md);
        if n == 0 {
            toast(&doc, "議事録を作れませんでした", "error");
        } else {
            toast(
                &doc,
                "議事録をボードの右に置きました。Ctrl/⌘+Z で戻せます",
                "done",
            );
        }
    });
}

fn start_mindmap(app: &Rc<RefCell<App>>) {
    if MINDMAP_BUSY.with(Cell::get) {
        return;
    }
    let (window, doc, url, body) = {
        let a = app.borrow();
        if !a.can_edit() {
            return;
        }
        let Some(room) = a.room.clone() else {
            toast(
                &a.document,
                "AI 機能は共同編集のボードでだけ使えます",
                "error",
            );
            return;
        };
        let Some(url) = api_url(&a.window, &room, "mindmap") else {
            return;
        };
        let body = json!({ "ids": ids_json(a.editor.selection()) }).to_string();
        (a.window.clone(), a.document.clone(), url, body)
    };
    MINDMAP_BUSY.with(|b| b.set(true));
    toast(&doc, "AI がマインドマップを作っています…", "busy");
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = post_json(&window, &url, &body).await;
        MINDMAP_BUSY.with(|b| b.set(false));
        let tree: Option<board_core::mindmap::MindTree> = match result {
            Ok(v) => serde_json::from_str(&to_text(&v)).ok(),
            Err(e) => {
                toast(&doc, &e, "error");
                return;
            }
        };
        match tree {
            Some(t) if !t.children.is_empty() => {
                app.borrow_mut().place_mindmap(&t);
                toast(
                    &doc,
                    "マインドマップをボードの右に置きました。Ctrl/⌘+Z で戻せます",
                    "done",
                );
            }
            _ => toast(&doc, "マインドマップを作れませんでした", "error"),
        }
    });
}

fn start_ask(app: &Rc<RefCell<App>>, input: &HtmlInputElement) {
    if ASK_BUSY.with(Cell::get) {
        return;
    }
    let question = input.value().trim().to_string();
    if question.is_empty() {
        let _ = input.focus();
        return;
    }
    let (window, doc, url, body) = {
        let a = app.borrow();
        let Some(room) = a.room.clone() else {
            toast(
                &a.document,
                "AI 機能は共同編集のボードでだけ使えます",
                "error",
            );
            return;
        };
        // ボードを移ったら前の会話は使わない。
        let same = TURNS_ROOM.with(|r| r.borrow().as_deref() == Some(room.as_str()));
        if !same {
            TURNS.with(|t| t.borrow_mut().clear());
            TURNS_ROOM.with(|r| *r.borrow_mut() = Some(room.clone()));
        }
        let Some(url) = api_url(&a.window, &room, "ask") else {
            return;
        };
        let history: Vec<Value> = TURNS.with(|t| {
            t.borrow()
                .iter()
                .map(|t| json!({ "q": t.q, "a": t.a }))
                .collect()
        });
        let body = json!({
            "question": question,
            "ids": ids_json(a.editor.selection()),
            "history": history,
        })
        .to_string();
        (a.window.clone(), a.document.clone(), url, body)
    };
    ASK_BUSY.with(|b| b.set(true));
    input.set_disabled(true);
    toast(&doc, "AI がボードを読んでいます…", "busy");
    let app = app.clone();
    let input = input.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = post_json(&window, &url, &body).await;
        ASK_BUSY.with(|b| b.set(false));
        input.set_disabled(false);
        let v: Value = match result {
            Ok(v) => serde_json::from_str(&to_text(&v)).unwrap_or_default(),
            Err(e) => {
                toast(&doc, &e, "error");
                return;
            }
        };
        let refs: Vec<ElementId> = v["refs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|h| u64::from_str_radix(h, 16).ok())
            .map(ElementId)
            .collect();
        TURNS.with(|t| {
            let mut t = t.borrow_mut();
            t.push(Turn {
                q: question,
                a: v["answer"].as_str().unwrap_or_default().to_string(),
                refs,
            });
            let extra = t.len().saturating_sub(KEEP_TURNS);
            t.drain(..extra);
        });
        input.set_value("");
        toast(&doc, "答えをパネルに出しました", "done");
        app.borrow().render_answers();
    });
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    let minutes: HtmlElement = by_id(doc, "ai-minutes")?;
    {
        let app = app.clone();
        listen(
            minutes.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                start_minutes(&app);
            },
        )?;
    }

    let mindmap: HtmlElement = by_id(doc, "ai-mindmap")?;
    {
        let app = app.clone();
        listen(
            mindmap.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                start_mindmap(&app);
            },
        )?;
    }

    let form: HtmlElement = by_id(doc, "ai-ask-form")?;
    let input: HtmlInputElement = by_id(doc, "ai-ask-input")?;
    {
        let app = app.clone();
        listen(form.as_ref(), "submit", false, move |e: web_sys::Event| {
            e.prevent_default();
            start_ask(&app, &input);
        })?;
    }

    let answer: HtmlElement = by_id(doc, "ai-answer")?;
    {
        let app = app.clone();
        listen(
            answer.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                let Some(el) = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    .and_then(|el| el.closest("button").ok().flatten())
                else {
                    return;
                };
                if el.get_attribute("data-act").as_deref() == Some("clear") {
                    TURNS.with(|t| t.borrow_mut().clear());
                    app.borrow().render_answers();
                    return;
                }
                let ids: Vec<ElementId> = if let Some(h) = el.get_attribute("data-ref") {
                    u64::from_str_radix(&h, 16)
                        .ok()
                        .map(ElementId)
                        .into_iter()
                        .collect()
                } else if let Some(i) = el
                    .get_attribute("data-turn")
                    .and_then(|i| i.parse::<usize>().ok())
                {
                    TURNS
                        .with(|t| t.borrow().get(i).map(|t| t.refs.clone()))
                        .unwrap_or_default()
                } else {
                    return;
                };
                app.borrow_mut().focus_refs(&ids);
            },
        )?;
    }
    Ok(())
}
