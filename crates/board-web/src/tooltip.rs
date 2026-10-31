//! ツールバーのツールチップ。
//!
//! ブラウザ標準の `title` は出るまで遅く見た目も揃わないので、起動時に
//! `title="選択 (V)"` を `data-tip="選択" data-key="V"` に移し、CSS で吹き出しを描く。

use wasm_bindgen::JsCast;
use web_sys::{Document, Element};

use crate::split_title;

pub fn install(doc: &Document) {
    let Ok(list) = doc.query_selector_all(".toolbar [title]") else {
        return;
    };
    for i in 0..list.length() {
        let Some(el) = list.item(i).and_then(|n| n.dyn_into::<Element>().ok()) else {
            continue;
        };
        let Some(title) = el.get_attribute("title") else {
            continue;
        };
        let (name, key) = split_title(&title);
        if el.get_attribute("aria-label").is_none() {
            let _ = el.set_attribute("aria-label", name);
        }
        let _ = el.set_attribute("data-tip", name);
        if !key.is_empty() {
            let _ = el.set_attribute("data-key", key);
        }
        let _ = el.remove_attribute("title");
    }
}
