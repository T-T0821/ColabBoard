//! 選択中の要素の書式バー(選択の上に浮かぶ)。
//!
//! 図形の種類・文字の大きさ・揃え・コネクタの線種と矢印・線の太さ・リンクを変える。
//! 色は下のツールバーの色見本で変える。

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use board_core::format::{self, Change, FONT_SIZES, Summary, WIDTHS};
use board_core::{Cap, PathStyle, ShapeKind, TextAlign};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, HtmlInputElement, HtmlSelectElement};

use super::{App, actions::Action, by_id, listen};

/// 書式バーの状態(作り直しを減らすため、前回の内容を覚えておく)。
#[derive(Default)]
pub struct FormatBar {
    shown: Option<Summary>,
    /// リンクの入力欄を開いているか。
    link_open: bool,
    /// 前回の位置(CSS ピクセル)。
    pos: Option<(i32, i32)>,
}

fn path_label(p: PathStyle) -> &'static str {
    match p {
        PathStyle::Straight => "直線",
        PathStyle::Elbow => "カギ線",
        PathStyle::Curved => "曲線",
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn pressed(on: bool) -> &'static str {
    if on { "true" } else { "false" }
}

const SEP: &str = r#"<span class="fb-sep"></span>"#;

fn align_icon(a: TextAlign) -> &'static str {
    match a {
        TextAlign::Left => {
            r#"<svg viewBox="0 0 24 24"><path d="M4 6h16M4 10h10M4 14h16M4 18h10"/></svg>"#
        }
        TextAlign::Center => {
            r#"<svg viewBox="0 0 24 24"><path d="M4 6h16M7 10h10M4 14h16M7 18h10"/></svg>"#
        }
        TextAlign::Right => {
            r#"<svg viewBox="0 0 24 24"><path d="M4 6h16M10 10h10M4 14h16M10 18h10"/></svg>"#
        }
    }
}

/// 書式バーの中身(HTML)。
fn render(s: &Summary, link_open: bool) -> String {
    let mut h = String::new();
    let mut groups: Vec<String> = Vec::new();

    if s.locked {
        groups.push(
            r#"<button data-act="unlock" title="ロックを解除 (Ctrl+Shift+L)">🔒 ロック中</button>"#
                .into(),
        );
    }

    if s.has_kind {
        let mut g = String::from(r#"<select data-act="kind" title="図形の種類">"#);
        if s.kind.is_none() {
            g.push_str(r#"<option value="" selected>—</option>"#);
        }
        for cat in board_core::ShapeCategory::ALL {
            let _ = write!(g, r#"<optgroup label="{}">"#, cat.label());
            for k in ShapeKind::LIBRARY
                .into_iter()
                .filter(|k| k.category() == cat)
            {
                let sel = if s.kind == Some(k) { " selected" } else { "" };
                let _ = write!(
                    g,
                    r#"<option value="{}"{sel}>{}</option>"#,
                    k.name(),
                    k.label()
                );
            }
            g.push_str("</optgroup>");
        }
        g.push_str("</select>");
        groups.push(g);
    }

    if s.has_text {
        let mut g = String::from(
            r#"<button data-act="font-down" title="文字を小さく">A<small>−</small></button><select data-act="font" title="文字の大きさ">"#,
        );
        let current = s.font_size.map(|f| f.round() as i32);
        if current.is_none_or(|c| !FONT_SIZES.iter().any(|&f| f as i32 == c)) {
            let label = current.map_or("—".to_string(), |c| c.to_string());
            let _ = write!(g, r#"<option value="" selected>{label}</option>"#);
        }
        for f in FONT_SIZES {
            let sel = if current == Some(f as i32) {
                " selected"
            } else {
                ""
            };
            let _ = write!(g, r#"<option value="{f}"{sel}>{f}</option>"#);
        }
        g.push_str(
            r#"</select><button data-act="font-up" title="文字を大きく">A<small>+</small></button>"#,
        );
        groups.push(g);

        let mut g = String::new();
        for (a, title) in [
            (TextAlign::Left, "左揃え"),
            (TextAlign::Center, "中央揃え"),
            (TextAlign::Right, "右揃え"),
        ] {
            let _ = write!(
                g,
                r#"<button data-act="align" data-value="{}" title="{title}" aria-pressed="{}">{}</button>"#,
                a.name(),
                pressed(s.align == Some(a)),
                align_icon(a)
            );
        }
        groups.push(g);
    }

    if s.has_connector {
        let mut g = String::from(r#"<select data-act="path" title="線の種類">"#);
        if s.path.is_none() {
            g.push_str(r#"<option value="" selected>—</option>"#);
        }
        for p in [PathStyle::Straight, PathStyle::Elbow, PathStyle::Curved] {
            let sel = if s.path == Some(p) { " selected" } else { "" };
            let _ = write!(
                g,
                r#"<option value="{}"{sel}>{}</option>"#,
                p.name(),
                path_label(p)
            );
        }
        g.push_str("</select>");
        let _ = write!(
            g,
            r#"<button data-act="start-cap" title="始点の矢印" aria-pressed="{}"><svg viewBox="0 0 24 24"><path d="M20 12H5M10 7l-5 5 5 5"/></svg></button>"#,
            pressed(s.start_cap == Some(Cap::Arrow))
        );
        let _ = write!(
            g,
            r#"<button data-act="end-cap" title="終点の矢印" aria-pressed="{}"><svg viewBox="0 0 24 24"><path d="M4 12h15M14 7l5 5-5 5"/></svg></button>"#,
            pressed(s.end_cap == Some(Cap::Arrow))
        );
        groups.push(g);
    }

    if s.has_width {
        let mut g = String::new();
        for (w, title) in WIDTHS.iter().zip(["細い線", "普通の線", "太い線"]) {
            let _ = write!(
                g,
                r#"<button data-act="width" data-value="{w}" title="{title}" aria-pressed="{}"><svg viewBox="0 0 24 24"><path d="M4 12h16" style="stroke-width:{}"/></svg></button>"#,
                pressed(s.width == Some(*w)),
                w * 0.75
            );
        }
        groups.push(g);
    }

    let link = s.link.clone().unwrap_or_default();
    let mut g = format!(
        r#"<button data-act="link" title="リンク" aria-pressed="{}"><svg viewBox="0 0 24 24"><path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/></svg></button>"#,
        pressed(!link.is_empty())
    );
    if let Some(url) = format::normalize_link(&link) {
        let _ = write!(
            g,
            r#"<a class="fb-open" href="{}" target="_blank" rel="noopener noreferrer" title="{}">開く ↗</a>"#,
            esc(&url),
            esc(&url)
        );
    }
    groups.push(g);

    h.push_str(&groups.join(SEP));
    if link_open {
        let _ = write!(
            h,
            r#"<div class="fb-link"><input data-act="link-input" type="url" placeholder="https://… (Enter で決定)" value="{}"><button data-act="link-apply">決定</button></div>"#,
            esc(&link)
        );
    }
    h
}

impl App {
    fn format_bar_el(&self) -> Option<HtmlElement> {
        self.document
            .get_element_by_id("format-bar")
            .and_then(|e| e.dyn_into().ok())
    }

    /// 選択や表示位置が変わったら書式バーを作り直し、置き直す。毎フレーム呼ぶ。
    pub(super) fn layout_format_bar(&mut self) {
        let Some(bar) = self.format_bar_el() else {
            return;
        };
        let sel = self.editor.selection();
        let visible = !sel.is_empty()
            && self.can_edit()
            && !self.editor.is_dragging()
            && !self.navigating
            && self.editor.editing().is_none();
        let bounds = if visible {
            self.board.bounds_of(sel)
        } else {
            None
        };
        let Some(bounds) = bounds else {
            if self.format_bar.shown.take().is_some() {
                bar.set_hidden(true);
                self.format_bar.link_open = false;
                self.format_bar.pos = None;
            }
            return;
        };
        let summary = format::summarize(&self.board, sel);
        if self.format_bar.shown.as_ref() != Some(&summary) {
            // 入力中のリンク欄は作り直さない。
            let typing = self
                .document
                .active_element()
                .is_some_and(|el| el.get_attribute("data-act").as_deref() == Some("link-input"));
            if !typing {
                bar.set_inner_html(&render(&summary, self.format_bar.link_open));
            }
            self.format_bar.shown = Some(summary);
            bar.set_hidden(false);
        }

        // 選択の上(入らなければ下)に、画面からはみ出さないように置く。
        let dpr = self.window.device_pixel_ratio() as f32;
        let canvas = self.canvas.get_bounding_client_rect();
        let top = self.camera.world_to_screen(bounds.min) / dpr;
        let bottom = self.camera.world_to_screen(bounds.max) / dpr;
        let w = bar.offset_width() as f32;
        let h = bar.offset_height() as f32;
        let vw = canvas.width() as f32;
        let vh = canvas.height() as f32;
        let cx = (top.x + bottom.x) * 0.5;
        let x = (cx - w * 0.5).clamp(8.0, (vw - w - 8.0).max(8.0));
        let mut y = top.y - h - 16.0;
        if y < 64.0 {
            y = bottom.y + 16.0;
        }
        let y = y.clamp(8.0, (vh - h - 80.0).max(8.0));
        let pos = (
            (canvas.left() as f32 + x).round() as i32,
            (canvas.top() as f32 + y).round() as i32,
        );
        if self.format_bar.pos != Some(pos) {
            self.format_bar.pos = Some(pos);
            let css = bar.style();
            let _ = css.set_property("left", &format!("{}px", pos.0));
            let _ = css.set_property("top", &format!("{}px", pos.1));
        }
    }

    fn apply_format(&mut self, change: Change) {
        if !self.can_edit() {
            return;
        }
        let ids = self.editor.selection().to_vec();
        self.board.end_gesture();
        format::apply(&mut self.board, &ids, &change);
        self.board.end_gesture();
        self.dirty = true;
    }

    fn open_link_editor(&mut self, open: bool) {
        self.format_bar.link_open = open;
        self.format_bar.shown = None; // 作り直す
        self.layout_format_bar();
        if open
            && let Some(input) = self
                .document
                .query_selector("#format-bar [data-act=link-input]")
                .ok()
                .flatten()
                .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
        {
            let _ = input.focus();
            input.select();
        }
    }

    fn commit_link(&mut self) {
        let value = self
            .document
            .query_selector("#format-bar [data-act=link-input]")
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
            .map(|i| i.value())
            .unwrap_or_default();
        if let Some(el) = self
            .document
            .active_element()
            .and_then(|e| e.dyn_into::<HtmlElement>().ok())
        {
            let _ = el.blur();
        }
        self.apply_format(Change::Link(value));
        self.open_link_editor(false);
    }
}

fn act_of(target: Option<web_sys::EventTarget>) -> Option<Element> {
    target
        .and_then(|t| t.dyn_into::<Element>().ok())
        .and_then(|e| e.closest("[data-act]").ok().flatten())
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let bar: Element = by_id(&doc, "format-bar")?;

    // キャンバスの操作(選択解除など)に渡さない。
    listen(
        bar.as_ref(),
        "pointerdown",
        false,
        |e: web_sys::PointerEvent| {
            e.stop_propagation();
        },
    )?;

    {
        let app = app.clone();
        listen(
            bar.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                let Some(el) = act_of(e.target()) else {
                    return;
                };
                if el.tag_name() == "SELECT" || el.tag_name() == "INPUT" || el.tag_name() == "A" {
                    return;
                }
                let act = el.get_attribute("data-act").unwrap_or_default();
                let value = el.get_attribute("data-value").unwrap_or_default();
                let mut a = app.borrow_mut();
                let s = a.format_bar.shown.clone().unwrap_or_default();
                match act.as_str() {
                    "unlock" => a.run_action(Action::Lock),
                    "font-down" => a.apply_format(Change::FontStep(-1)),
                    "font-up" => a.apply_format(Change::FontStep(1)),
                    "align" => a.apply_format(Change::Align(TextAlign::from_name(&value))),
                    "start-cap" => {
                        a.apply_format(Change::StartCap(if s.start_cap == Some(Cap::Arrow) {
                            Cap::None
                        } else {
                            Cap::Arrow
                        }))
                    }
                    "end-cap" => a.apply_format(Change::EndCap(if s.end_cap == Some(Cap::Arrow) {
                        Cap::None
                    } else {
                        Cap::Arrow
                    })),
                    "width" => {
                        if let Ok(w) = value.parse() {
                            a.apply_format(Change::Width(w));
                        }
                    }
                    "link" => {
                        let open = !a.format_bar.link_open;
                        a.open_link_editor(open);
                    }
                    "link-apply" => a.commit_link(),
                    _ => {}
                }
            },
        )?;
    }

    {
        let app = app.clone();
        listen(bar.as_ref(), "change", false, move |e: web_sys::Event| {
            let Some(sel) = e
                .target()
                .and_then(|t| t.dyn_into::<HtmlSelectElement>().ok())
            else {
                return;
            };
            let value = sel.value();
            if value.is_empty() {
                return;
            }
            let act = sel.get_attribute("data-act").unwrap_or_default();
            let change = match act.as_str() {
                "kind" => ShapeKind::from_name(&value).map(Change::Kind),
                "font" => value.parse().ok().map(Change::FontSize),
                "path" => Some(Change::Path(PathStyle::from_name(&value))),
                _ => None,
            };
            let _ = sel.blur();
            if let Some(c) = change {
                app.borrow_mut().apply_format(c);
            }
        })?;
    }

    {
        let app = app.clone();
        listen(
            bar.as_ref(),
            "keydown",
            false,
            move |e: web_sys::KeyboardEvent| {
                let is_input = act_of(e.target()).is_some_and(|el| {
                    el.get_attribute("data-act").as_deref() == Some("link-input")
                });
                if !is_input || e.is_composing() {
                    return;
                }
                match e.key().as_str() {
                    "Enter" => {
                        e.prevent_default();
                        app.borrow_mut().commit_link();
                    }
                    "Escape" => {
                        e.prevent_default();
                        app.borrow_mut().open_link_editor(false);
                    }
                    _ => {}
                }
            },
        )?;
    }
    Ok(())
}
