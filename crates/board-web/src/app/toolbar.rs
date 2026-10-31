//! ツールバーの補助: 図形ライブラリなどのポップオーバー。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::ShapeKind;
use wasm_bindgen::JsCast;
use web_sys::{Element, MouseEvent};

use super::{App, by_id, js_err, listen};

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let library: Element = by_id(&doc, "shape-library")?;

    let items = library.query_selector_all("[data-shape]").map_err(js_err)?;
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
        let app = app.clone();
        let library = library.clone();
        let el2 = el.clone();
        listen(el.as_ref(), "click", false, move |e: MouseEvent| {
            e.stop_propagation();
            let mut a = app.borrow_mut();
            a.editor.set_shape_kind(kind);
            a.dirty = true;
            let _ = library.remove_attribute("data-open");
            // 選んだ図形をボタンの絵にする。
            if let Some(svg) = el2.first_element_child()
                && let Some(btn_svg) = a
                    .document
                    .get_element_by_id("tool-shape")
                    .and_then(|b| b.first_element_child())
            {
                btn_svg.set_inner_html(&svg.inner_html());
            }
        })?;
    }

    popover(app, "tool-shape", library)?;
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
