//! ツールバーの補助: 図形ライブラリのポップオーバー。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::ShapeKind;
use wasm_bindgen::JsCast;
use web_sys::{Element, MouseEvent};

use super::{App, by_id, js_err, listen};

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let button: Element = by_id(&doc, "tool-shape")?;
    let library: Element = by_id(&doc, "shape-library")?;

    // 図形ボタンを押すたびにライブラリを開閉する(ツールの切り替えは共通の処理で行う)。
    {
        let library = library.clone();
        listen(button.as_ref(), "click", false, move |_: MouseEvent| {
            if library.has_attribute("data-open") {
                let _ = library.remove_attribute("data-open");
            } else {
                let _ = library.set_attribute("data-open", "");
            }
        })?;
    }

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

    // 外を押したら閉じる。
    {
        let window = app.borrow().window.clone();
        listen(
            window.as_ref(),
            "pointerdown",
            false,
            move |e: web_sys::PointerEvent| {
                let inside = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                    .is_some_and(|n| library.contains(Some(&n)) || button.contains(Some(&n)));
                if !inside {
                    let _ = library.remove_attribute("data-open");
                }
            },
        )?;
    }
    Ok(())
}
