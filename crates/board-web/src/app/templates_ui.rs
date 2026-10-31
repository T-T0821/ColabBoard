//! テンプレートの選択と進め方のガイド。
//!
//! 選んだテンプレートは今あるものの右に置き、そこへ表示を移す。置いたあとは
//! ファシリテーションのパネルに「進め方」を出し、手順ごとのボタンからタイマー・投票・AI を始める。

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use board_core::Vec2;
use board_core::templates::{self, Category, StepAction, Template};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlInputElement, MouseEvent};

use super::ai::{self, Action};
use super::collab::now_ms;
use super::{App, by_id, listen};

impl App {
    fn insert_template(&mut self, t: Template) {
        if !self.can_edit() {
            return;
        }
        // 既存の要素と重ならないよう右側に置く(空なら画面の中央)。
        let center = match self.board.bounds() {
            Some(r) => Vec2::new(r.max.x + 200.0 + 800.0, r.center().y),
            None => self.camera.screen_to_world(self.camera.viewport * 0.5),
        };
        self.board.end_gesture();
        let ids = templates::insert(&mut self.board, t, center);
        self.board.end_gesture();
        if let Some(r) = self.board.bounds_of(&ids) {
            self.camera.fit(r.min, r.max, 96.0);
            // 右側にファシリテーションのパネル(進め方)が開くので、その分あけて合わせ直す。
            let panel = 340.0 / self.camera.zoom;
            self.camera
                .fit(r.min, Vec2::new(r.max.x + panel, r.max.y), 96.0);
        }
        self.editor.set_selection(ids);
        self.dirty = true;
        show_guide(&self.document, t);
    }
}

/// HTML に入れる文字を逃がす(テンプレートの文言は固定だが念のため)。
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 手順のボタンの文言。説明だけの手順にはボタンを付けない。
fn action_label(a: StepAction) -> Option<String> {
    Some(match a {
        StepAction::None => return None,
        StepAction::Timer(m) => format!("{m}分タイマー"),
        StepAction::Vote(n) => format!("投票開始({n}票)"),
        StepAction::CloseVote => "締め切る".into(),
        StepAction::AiCluster => "AI でまとめる".into(),
        StepAction::AiSummary => "AI で要約".into(),
    })
}

/// パネルに進め方を出して開く。
fn show_guide(doc: &web_sys::Document, t: Template) {
    let (Ok(guide), Ok(title), Ok(list)) = (
        by_id::<Element>(doc, "template-guide"),
        by_id::<Element>(doc, "template-guide-title"),
        by_id::<Element>(doc, "template-guide-steps"),
    ) else {
        return;
    };
    title.set_text_content(Some(&format!("進め方: {}", t.title())));
    let _ = list.set_attribute("data-template", t.name());
    let mut html = String::new();
    for (i, s) in t.steps().iter().enumerate() {
        let _ = write!(html, "<li data-index=\"{i}\"><span>{}</span>", esc(s.label));
        if let Some(label) = action_label(s.action) {
            let _ = write!(html, "<button data-step=\"{i}\">{}</button>", esc(&label));
        }
        html.push_str("</li>");
    }
    list.set_inner_html(&html);
    let _ = guide.remove_attribute("hidden");
    if let Ok(panel) = by_id::<Element>(doc, "collab") {
        let _ = panel.set_attribute("open", "");
    }
}

/// 手順 `i` のボタンが押された。
fn run_step(app: &Rc<RefCell<App>>, t: Template, i: usize) {
    let Some(step) = t.steps().get(i) else {
        return;
    };
    match step.action {
        StepAction::None => {}
        StepAction::Timer(m) => {
            app.borrow_mut()
                .board
                .start_timer(f64::from(m) * 60_000.0, now_ms());
        }
        StepAction::Vote(n) => {
            let mut a = app.borrow_mut();
            if let Ok(input) = by_id::<HtmlInputElement>(&a.document, "vote-limit") {
                input.set_value(&n.to_string());
            }
            a.board.start_vote(n);
            a.refresh_votes();
        }
        StepAction::CloseVote => {
            let mut a = app.borrow_mut();
            a.board.close_vote();
            a.refresh_votes();
        }
        StepAction::AiCluster => ai::start(app, Action::Cluster),
        StepAction::AiSummary => ai::start(app, Action::Summarize),
    }
}

fn picker_html() -> String {
    let mut html = String::from(
        r#"<input class="template-search" type="search" placeholder="テンプレートを検索(例: 振り返り、KPT)" aria-label="テンプレートを検索" />"#,
    );
    html.push_str(r#"<div class="template-list">"#);
    for c in Category::ALL {
        let _ = write!(
            html,
            r#"<section data-category><h4>{}</h4><div class="template-grid">"#,
            esc(c.title())
        );
        for t in Template::ALL.iter().filter(|t| t.category() == c) {
            let keys = format!(
                "{} {} {} {}",
                t.name(),
                t.title(),
                t.description(),
                c.title()
            )
            .to_lowercase();
            let _ = write!(
                html,
                r#"<button data-template="{}" data-keys="{}"><strong>{}</strong><span>{}</span></button>"#,
                t.name(),
                esc(&keys),
                esc(t.title()),
                esc(t.description())
            );
        }
        html.push_str("</div></section>");
    }
    html.push_str(r#"<p class="template-empty" hidden>見つかりません</p></div>"#);
    html
}

/// 検索語で絞り込む(空白区切りの語をすべて含むものを残す)。
fn filter(picker: &Element, query: &str) {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut any = false;
    let Ok(sections) = picker.query_selector_all("[data-category]") else {
        return;
    };
    for i in 0..sections.length() {
        let Some(sec) = sections.item(i).and_then(|n| n.dyn_into::<Element>().ok()) else {
            continue;
        };
        let mut shown = 0;
        if let Ok(buttons) = sec.query_selector_all("[data-template]") {
            for j in 0..buttons.length() {
                let Some(b) = buttons.item(j).and_then(|n| n.dyn_into::<Element>().ok()) else {
                    continue;
                };
                let keys = b.get_attribute("data-keys").unwrap_or_default();
                if words.iter().all(|w| keys.contains(w.as_str())) {
                    let _ = b.remove_attribute("hidden");
                    shown += 1;
                } else {
                    let _ = b.set_attribute("hidden", "");
                }
            }
        }
        if shown == 0 {
            let _ = sec.set_attribute("hidden", "");
        } else {
            let _ = sec.remove_attribute("hidden");
            any = true;
        }
    }
    if let Ok(Some(empty)) = picker.query_selector(".template-empty") {
        if any {
            let _ = empty.set_attribute("hidden", "");
        } else {
            let _ = empty.remove_attribute("hidden");
        }
    }
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let picker: Element = by_id(&doc, "template-picker")?;
    picker.set_inner_html(&picker_html());
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
    if let Ok(Some(input)) = picker.query_selector(".template-search") {
        let picker2 = picker.clone();
        let input2: HtmlInputElement = input.clone().unchecked_into();
        listen(input.as_ref(), "input", false, move |_: web_sys::Event| {
            filter(&picker2, &input2.value());
        })?;
        // 文字入力中のキーをボードのショートカットに渡さない。
        listen(
            input.as_ref(),
            "keydown",
            false,
            move |e: web_sys::KeyboardEvent| e.stop_propagation(),
        )?;
    }
    {
        let list: Element = by_id(&doc, "template-guide-steps")?;
        let list2 = list.clone();
        let app = app.clone();
        listen(list.as_ref(), "click", false, move |e: MouseEvent| {
            let Some(btn) = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|t| t.closest("[data-step]").ok().flatten())
            else {
                return;
            };
            let Some(i) = btn
                .get_attribute("data-step")
                .and_then(|s| s.parse::<usize>().ok())
            else {
                return;
            };
            let Some(t) = list2
                .get_attribute("data-template")
                .and_then(|n| Template::from_name(&n))
            else {
                return;
            };
            // 押した手順に印を付ける(どこまで進んだかわかるように)。
            if let Ok(Some(li)) = btn.closest("li") {
                let _ = li.set_attribute("data-done", "");
            }
            let app = app.clone();
            // ほかのハンドラが App を借りていても困らないよう、借用が終わってから動かす。
            wasm_bindgen_futures::spawn_local(async move { run_step(&app, t, i) });
        })?;
    }
    {
        let close: Element = by_id(&doc, "template-guide-close")?;
        let guide: Element = by_id(&doc, "template-guide")?;
        listen(close.as_ref(), "click", false, move |_: MouseEvent| {
            let _ = guide.set_attribute("hidden", "");
        })?;
    }
    super::toolbar::popover(app, "templates-button", picker)
}
