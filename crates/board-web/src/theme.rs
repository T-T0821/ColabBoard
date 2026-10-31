//! ライト / ダークの切り替え。
//!
//! `<html data-theme="light|dark">` を CSS が見て色を変え、キャンバスは [`is_dark`] を
//! 毎フレーム見て描画を合わせる。選んだテーマは localStorage に覚え、未選択なら OS の設定に従う。

use std::cell::Cell;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Document, Window};

const STORAGE_KEY: &str = "colabboard.theme";

thread_local! {
    static DARK: Cell<bool> = const { Cell::new(false) };
}

/// いまダークモードか。
pub fn is_dark() -> bool {
    DARK.with(Cell::get)
}

fn stored(window: &Window) -> Option<bool> {
    let s = window.local_storage().ok().flatten()?;
    match s.get_item(STORAGE_KEY).ok().flatten()?.as_str() {
        "dark" => Some(true),
        "light" => Some(false),
        _ => None,
    }
}

fn system_dark(window: &Window) -> bool {
    window
        .match_media("(prefers-color-scheme: dark)")
        .ok()
        .flatten()
        .is_some_and(|m| m.matches())
}

fn apply(doc: &Document, dark: bool) {
    DARK.with(|d| d.set(dark));
    if let Some(root) = doc.document_element() {
        let _ = root.set_attribute("data-theme", if dark { "dark" } else { "light" });
    }
    if let Some(b) = doc.get_element_by_id("theme-toggle") {
        let _ = b.set_attribute("aria-pressed", if dark { "true" } else { "false" });
        let _ = b.set_attribute(
            "title",
            if dark {
                "ライトモードにする"
            } else {
                "ダークモードにする"
            },
        );
    }
}

/// 起動時に一度だけ呼ぶ。テーマを決めて、切り替えボタンと OS の設定変更を見張る。
pub fn init(window: &Window, doc: &Document) {
    apply(doc, stored(window).unwrap_or_else(|| system_dark(window)));

    // OS の設定が変わったら、自分で選んでいない限り追従する。
    if let Ok(Some(mq)) = window.match_media("(prefers-color-scheme: dark)") {
        let (w, d) = (window.clone(), doc.clone());
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if stored(&w).is_none() {
                apply(&d, system_dark(&w));
            }
        });
        let _ = mq.add_event_listener_with_callback("change", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    if let Some(button) = doc.get_element_by_id("theme-toggle") {
        let (w, d) = (window.clone(), doc.clone());
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let dark = !is_dark();
            if let Ok(Some(s)) = w.local_storage() {
                let _ = s.set_item(STORAGE_KEY, if dark { "dark" } else { "light" });
            }
            apply(&d, dark);
        });
        let _ = button.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref());
        cb.forget();
    }
}
