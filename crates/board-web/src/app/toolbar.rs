//! 図形ライブラリ(左のパネル)と、ツールバーのポップオーバー。
//!
//! 図形を押すとその図形のツールになり(キャンバスをドラッグして描く・クリックで置く)、
//! キャンバスへドラッグ&ドロップするとその場所に置ける。

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use board_core::shape::Shape;
use board_core::{ShapeCategory, ShapeKind, Tool, Vec2};
use wasm_bindgen::JsCast;
use web_sys::{DragEvent, Element, HtmlElement, HtmlInputElement, MouseEvent};

use super::{App, by_id, js_err, listen};

/// ドラッグ&ドロップで図形の種類を渡すときのデータ形式。
const DRAG_TYPE: &str = "application/x-colabboard-shape";

/// 図形ライブラリのアイコン(24×24 の SVG の中身)。
pub fn icon(kind: ShapeKind) -> String {
    // 多角形は頂点表から作る(描画・当たり判定と同じ形)。
    if let Some(v) = kind.polygon() {
        let mut d = String::new();
        for (i, p) in v.iter().enumerate() {
            let _ = write!(
                d,
                "{}{:.2} {:.2}",
                if i == 0 { "M" } else { "L" },
                12.0 + p[0] * 9.0,
                12.0 + p[1] * 8.0
            );
        }
        return format!(r#"<path d="{d}Z"/>"#);
    }
    let d = match kind {
        ShapeKind::Rect => return r#"<rect x="3" y="4" width="18" height="16" rx="1"/>"#.into(),
        ShapeKind::RoundRect => {
            return r#"<rect x="3" y="4" width="18" height="16" rx="5"/>"#.into();
        }
        ShapeKind::Pill => return r#"<rect x="2" y="7" width="20" height="10" rx="5"/>"#.into(),
        ShapeKind::Ellipse => return r#"<ellipse cx="12" cy="12" rx="9" ry="8"/>"#.into(),
        ShapeKind::Triangle => "M12 4L21 20H3z",
        ShapeKind::Diamond => "M12 4l9 8-9 8-9-8z",
        ShapeKind::Star => {
            "M12 3l2.6 5.6 6.1.7-4.5 4.2 1.2 6L12 16.6 6.6 19.5l1.2-6-4.5-4.2 6.1-.7z"
        }
        ShapeKind::Hexagon => "M7.5 4h9l4.5 8-4.5 8h-9L3 12z",
        ShapeKind::Parallelogram => "M7.5 4H21l-4.5 16H3z",
        ShapeKind::Heart => {
            "M12 20C5.5 15.5 3 12 3 8.7A4.6 4.6 0 0112 6.6a4.6 4.6 0 019 2.1c0 3.3-2.5 6.8-9 11.3z"
        }
        ShapeKind::Cloud => "M6.5 19a3.6 3.6 0 01-.7-7.1 5.2 5.2 0 0110-2.3A4.3 4.3 0 0117.8 19z",
        ShapeKind::Speech => {
            "M3 6a2 2 0 012-2h14a2 2 0 012 2v8a2 2 0 01-2 2h-9l-4.5 4v-4H5a2 2 0 01-2-2z"
        }
        ShapeKind::Cylinder => {
            "M4 7c0-1.7 3.6-3 8-3s8 1.3 8 3v10c0 1.7-3.6 3-8 3s-8-1.3-8-3zM4 7c0 1.7 3.6 3 8 3s8-1.3 8-3"
        }
        ShapeKind::Document => "M3 4h18v13c-3-2-6-.5-9 1S6 19.5 3 17.5z",
        ShapeKind::Delay => "M3 4h9a8 8 0 010 16H3z",
        ShapeKind::PredefinedProcess => "M3 4h18v16H3zM6 4v16M18 4v16",
        ShapeKind::InternalStorage => "M3 4h18v16H3zM7 4v16M3 8h18",
        _ => "M3 4h18v16H3z",
    };
    format!(r#"<path d="{d}"/>"#)
}

/// パネルの中身。
fn render() -> String {
    let mut h = String::from(
        r#"<header class="shape-panel-head"><h2>図形</h2><button class="icon-btn" data-close title="閉じる" aria-label="閉じる">×</button></header>
<input type="search" class="shape-search" placeholder="図形を検索(例: 矢印、円柱)" aria-label="図形を検索">
<p class="shape-hint">押して描く・キャンバスへドラッグして置く</p>"#,
    );
    for cat in ShapeCategory::ALL {
        let _ = write!(
            h,
            r#"<section class="shape-section"><h3>{}</h3><div class="shape-grid">"#,
            cat.label()
        );
        for k in ShapeKind::LIBRARY
            .into_iter()
            .filter(|k| k.category() == cat)
        {
            let _ = write!(
                h,
                r#"<button class="shape-item" data-shape="{name}" title="{label}" aria-label="{label}" draggable="true"><svg viewBox="0 0 24 24">{icon}</svg></button>"#,
                name = k.name(),
                label = k.label(),
                icon = icon(k),
            );
        }
        h.push_str("</div></section>");
    }
    h.push_str(
        r#"<section class="shape-section"><h3>その他</h3><div class="shape-links">
<button data-table><svg viewBox="0 0 24 24"><rect x="3" y="5" width="18" height="14" rx="1"/><path d="M3 10h18M3 14.5h18M9 5v14M15 5v14"/></svg>表(3×3)</button>
<button data-cloud><svg viewBox="0 0 24 24"><path d="M6.5 19a3.6 3.6 0 01-.7-7.1 5.2 5.2 0 0110-2.3A4.3 4.3 0 0117.8 19z"/></svg>AWS・クラウド構成図のアイコン<span aria-hidden="true">›</span></button>
</div></section>"#,
    );
    h
}

impl App {
    /// 図形をワールド座標 `at` を中心に既定の大きさで置く。
    fn drop_shape(&mut self, kind: ShapeKind, at: Vec2) {
        if !self.can_edit() {
            return;
        }
        let size = kind.default_size();
        let id = self.board.alloc_id();
        self.board.end_gesture();
        let shape = Shape::new(id, kind, at - size * 0.5, size, self.editor.shape_fill);
        self.board.push(shape);
        self.board.end_gesture();
        self.editor.set_tool(Tool::Select);
        self.editor.set_selection(vec![id]);
        self.dirty = true;
    }
}

fn set_open(doc: &web_sys::Document, panel: &HtmlElement, open: bool) {
    panel.set_hidden(!open);
    if let Some(b) = doc.get_element_by_id("tool-shape") {
        let _ = b.set_attribute("aria-expanded", if open { "true" } else { "false" });
    }
    if open {
        // 構成図パネルと同じ場所なので閉じる。
        if let Some(p) = doc
            .get_element_by_id("diagram-panel")
            .and_then(|p| p.dyn_into::<HtmlElement>().ok())
            && !p.hidden()
            && let Some(t) = doc
                .get_element_by_id("diagram-toggle")
                .and_then(|t| t.dyn_into::<HtmlElement>().ok())
        {
            t.click();
        }
    }
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let (doc, canvas) = {
        let a = app.borrow();
        (a.document.clone(), a.canvas.clone())
    };
    let panel: HtmlElement = by_id(&doc, "shape-panel")?;
    panel.set_inner_html(&render());

    // ツールバーの図形ボタンで開閉する(ツールも図形になる)。
    {
        let button: Element = by_id(&doc, "tool-shape")?;
        let panel = panel.clone();
        let doc2 = doc.clone();
        listen(button.as_ref(), "click", false, move |_: MouseEvent| {
            set_open(&doc2, &panel, panel.hidden());
        })?;
    }
    if let Some(close) = panel.query_selector("[data-close]").map_err(js_err)? {
        let panel = panel.clone();
        let doc2 = doc.clone();
        listen(close.as_ref(), "click", false, move |_: MouseEvent| {
            set_open(&doc2, &panel, false);
        })?;
    }
    // 構成図パネルを開いたらこちらは閉じる。
    if let Some(t) = doc.get_element_by_id("diagram-toggle") {
        let panel = panel.clone();
        let doc2 = doc.clone();
        listen(t.as_ref(), "click", false, move |_: MouseEvent| {
            if !panel.hidden() {
                set_open(&doc2, &panel, false);
            }
        })?;
    }

    let items = panel.query_selector_all("[data-shape]").map_err(js_err)?;
    for i in 0..items.length() {
        let Some(el) = items.item(i).and_then(|n| n.dyn_into::<Element>().ok()) else {
            continue;
        };
        let Some(kind) = el
            .get_attribute("data-shape")
            .and_then(|k| ShapeKind::from_name(&k))
        else {
            continue;
        };
        {
            let app = app.clone();
            let items = items.clone();
            let el2 = el.clone();
            listen(el.as_ref(), "click", false, move |e: MouseEvent| {
                e.stop_propagation();
                let mut a = app.borrow_mut();
                if !a.can_edit() {
                    return;
                }
                a.editor.set_shape_kind(kind);
                a.dirty = true;
                // 選んだ図形に印を付け、ツールバーのボタンの絵にする。
                for j in 0..items.length() {
                    if let Some(n) = items.item(j).and_then(|n| n.dyn_into::<Element>().ok()) {
                        let _ = n.remove_attribute("aria-pressed");
                    }
                }
                let _ = el2.set_attribute("aria-pressed", "true");
                if let Some(btn_svg) = a
                    .document
                    .get_element_by_id("tool-shape")
                    .and_then(|b| b.first_element_child())
                {
                    btn_svg.set_inner_html(&icon(kind));
                }
            })?;
        }
        listen(el.as_ref(), "dragstart", false, move |e: DragEvent| {
            if let Some(dt) = e.data_transfer() {
                let _ = dt.set_data(DRAG_TYPE, kind.name());
                dt.set_effect_allowed("copy");
            }
        })?;
    }

    // 検索(名前の一部で絞り込む)。
    if let Some(search) = panel
        .query_selector(".shape-search")
        .map_err(js_err)?
        .and_then(|s| s.dyn_into::<HtmlInputElement>().ok())
    {
        let panel = panel.clone();
        let search2 = search.clone();
        listen(search.as_ref(), "input", false, move |_: web_sys::Event| {
            let q = search2.value().trim().to_lowercase();
            let Ok(items) = panel.query_selector_all("[data-shape]") else {
                return;
            };
            for i in 0..items.length() {
                let Some(el) = items.item(i).and_then(|n| n.dyn_into::<HtmlElement>().ok()) else {
                    continue;
                };
                let kind = el
                    .get_attribute("data-shape")
                    .and_then(|k| ShapeKind::from_name(&k));
                let hit = q.is_empty()
                    || kind.is_some_and(|k| {
                        k.label().to_lowercase().contains(&q)
                            || k.name().contains(&q)
                            || k.category().label().contains(&q)
                    });
                el.set_hidden(!hit);
            }
            // 何も残らない見出しは隠す。
            if let Ok(sections) = panel.query_selector_all(".shape-section") {
                for i in 0..sections.length() {
                    let Some(sec) = sections
                        .item(i)
                        .and_then(|n| n.dyn_into::<HtmlElement>().ok())
                    else {
                        continue;
                    };
                    let links = sec.query_selector(".shape-links").ok().flatten().is_some();
                    let visible = if links {
                        q.is_empty()
                    } else {
                        sec.query_selector("[data-shape]:not([hidden])")
                            .ok()
                            .flatten()
                            .is_some()
                    };
                    sec.set_hidden(!visible);
                }
            }
        })?;
    }

    if let Some(btn) = panel.query_selector("[data-table]").map_err(js_err)? {
        let app = app.clone();
        listen(btn.as_ref(), "click", false, move |_: MouseEvent| {
            let mut a = app.borrow_mut();
            a.editor.set_tool(Tool::Select);
            a.run_action(super::actions::Action::InsertTable);
            a.dirty = true;
        })?;
    }
    if let Some(btn) = panel.query_selector("[data-cloud]").map_err(js_err)? {
        let doc2 = doc.clone();
        listen(btn.as_ref(), "click", false, move |_: MouseEvent| {
            // 構成図パネル(AWS などのアイコン)を開く。
            if let Some(t) = doc2
                .get_element_by_id("diagram-toggle")
                .and_then(|t| t.dyn_into::<HtmlElement>().ok())
            {
                t.click();
            }
        })?;
    }

    // キャンバスへのドロップ。
    {
        let app = app.clone();
        listen(canvas.as_ref(), "drop", false, move |e: DragEvent| {
            let Some(kind) = e
                .data_transfer()
                .and_then(|dt| dt.get_data(DRAG_TYPE).ok())
                .and_then(|k| ShapeKind::from_name(&k))
            else {
                return;
            };
            e.prevent_default();
            let mut a = app.borrow_mut();
            let p = a.css_to_physical(e.client_x() as f64, e.client_y() as f64);
            let at = a.camera.screen_to_world(p);
            a.drop_shape(kind, at);
        })?;
    }
    Ok(())
}

/// ボタン `button_id` で開閉するポップオーバー。外を押したら閉じる。
pub fn popover(app: &Rc<RefCell<App>>, button_id: &str, pop: Element) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let button: Element = by_id(&doc, button_id)?;
    {
        let pop = pop.clone();
        listen(button.as_ref(), "click", false, move |_: MouseEvent| {
            if pop.has_attribute("data-open") {
                let _ = pop.remove_attribute("data-open");
            } else {
                let _ = pop.set_attribute("data-open", "");
            }
        })?;
    }
    let window = app.borrow().window.clone();
    listen(
        window.as_ref(),
        "pointerdown",
        false,
        move |e: web_sys::PointerEvent| {
            let inside = e
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                .is_some_and(|n| pop.contains(Some(&n)) || button.contains(Some(&n)));
            if !inside {
                let _ = pop.remove_attribute("data-open");
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_library_shape_has_an_icon_and_a_section() {
        let html = render();
        for k in ShapeKind::LIBRARY {
            assert!(icon(k).starts_with('<'), "{k:?}");
            assert!(
                html.contains(&format!(r#"data-shape="{}""#, k.name())),
                "{k:?}"
            );
        }
        let polygons = ShapeKind::LIBRARY
            .iter()
            .filter(|k| k.polygon().is_some())
            .count();
        assert_eq!(board_core::shape::POLYGONS.len(), polygons);
    }
}
