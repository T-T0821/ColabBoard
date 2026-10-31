//! DOM の文字を訳す部分(ブラウザでだけ動く)。

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{
    Document, Element, MutationObserver, MutationObserverInit, MutationRecord, Node, Window,
};

use super::{LANG, Lang, STORAGE_KEY, detect, lang, translate};

/// 訳す属性。
const ATTRS: [&str; 5] = ["title", "aria-label", "placeholder", "alt", "data-tip"];

/// 中を訳さない要素。
const SKIP_TAGS: [&str; 5] = ["SCRIPT", "STYLE", "TEXTAREA", "CANVAS", "svg"];

/// 起動時に一度だけ呼ぶ。言語を決め、英語なら画面を訳して見張りを始める。切り替えボタンも置く。
pub fn init(window: &Window, doc: &Document) {
    let storage = window.local_storage().ok().flatten();
    let stored = storage
        .as_ref()
        .and_then(|s| s.get_item(STORAGE_KEY).ok().flatten());
    let nav = window.navigator().language();
    let l = detect(stored.as_deref(), nav.as_deref());
    LANG.with(|c| c.set(l));
    if let Some(root) = doc.document_element() {
        let _ = root.set_attribute("lang", l.code());
    }
    install_toggles(window, doc);
    if l == Lang::En {
        if let Some(root) = doc.document_element() {
            translate_tree(&root);
        }
        observe(doc);
    }
}

/// 言語の切り替えボタン(ボード上部のバーと、ボード一覧の右上)。
fn install_toggles(window: &Window, doc: &Document) {
    let (label, title) = match lang() {
        Lang::Ja => ("EN", "Switch to English"),
        Lang::En => ("日本語", "日本語に切り替える"),
    };
    let make = |id: &str, class: &str| -> Option<Element> {
        let b = doc.create_element("button").ok()?;
        b.set_id(id);
        let _ = b.set_attribute("class", class);
        let _ = b.set_attribute("type", "button");
        // ボタンの文字は切り替え先の言語のまま出す。
        let _ = b.set_attribute("translate", "no");
        let _ = b.set_attribute("title", title);
        let _ = b.set_attribute("aria-label", title);
        b.set_text_content(Some(label));
        let w = window.clone();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| switch(&w));
        let _ = b.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref());
        cb.forget();
        Some(b)
    };
    if let Some(theme) = doc.get_element_by_id("theme-toggle")
        && let Some(parent) = theme.parent_node()
        && let Some(b) = make("lang-toggle", "lang-toggle")
    {
        let _ = parent.insert_before(&b, Some(&theme));
    }
    if let Some(body) = doc.body()
        && let Some(b) = make("lang-fab", "lang-fab panel")
    {
        let _ = body.append_child(&b);
    }
}

/// もう一方の言語にして読み直す。
fn switch(window: &Window) {
    let next = match lang() {
        Lang::Ja => Lang::En,
        Lang::En => Lang::Ja,
    };
    if let Ok(Some(s)) = window.local_storage() {
        let _ = s.set_item(STORAGE_KEY, next.code());
    }
    let _ = window.location().reload();
}

/// `node` か祖先が `translate="no"` か、訳さない要素の中か。
fn skipped(node: &Node) -> bool {
    let el = match node.dyn_ref::<Element>() {
        Some(e) => Some(e.clone()),
        None => node.parent_element(),
    };
    let Some(el) = el else {
        return false;
    };
    // 属性の変更は要素そのものを見る。中身の変更は親から見る。
    el.closest("[translate=\"no\"], script, style, svg")
        .ok()
        .flatten()
        .is_some()
}

/// テキストノード 1 つを訳す。
fn translate_text(node: &Node) {
    if let Some(v) = node.node_value()
        && let Some(en) = translate(&v)
    {
        node.set_node_value(Some(&en));
    }
}

/// 要素の属性を訳す。
fn translate_attrs(el: &Element) {
    for a in ATTRS {
        translate_attr(el, a);
    }
}

fn translate_attr(el: &Element, name: &str) {
    if let Some(v) = el.get_attribute(name)
        && let Some(en) = translate(&v)
    {
        let _ = el.set_attribute(name, &en);
    }
}

/// `node` 以下をすべて訳す(`translate="no"` の中は飛ばす)。
fn translate_tree(node: &Node) {
    match node.node_type() {
        Node::TEXT_NODE => translate_text(node),
        Node::ELEMENT_NODE => {
            let Some(el) = node.dyn_ref::<Element>() else {
                return;
            };
            if el.get_attribute("translate").as_deref() == Some("no") {
                return;
            }
            translate_attrs(el);
            // canvas・textarea などは属性(aria-label・placeholder)だけ訳し、中身は触らない。
            if SKIP_TAGS.contains(&el.tag_name().as_str()) {
                return;
            }
            let children = node.child_nodes();
            for i in 0..children.length() {
                if let Some(c) = children.item(i) {
                    translate_tree(&c);
                }
            }
        }
        _ => {}
    }
}

/// 後から増えた・変わった文字を訳し続ける。
fn observe(doc: &Document) {
    let cb = Closure::<dyn FnMut(js_sys::Array, MutationObserver)>::new(
        move |records: js_sys::Array, _obs: MutationObserver| {
            for r in records.iter() {
                let Ok(r) = r.dyn_into::<MutationRecord>() else {
                    continue;
                };
                let Some(target) = r.target() else {
                    continue;
                };
                match r.type_().as_str() {
                    "childList" => {
                        let added = r.added_nodes();
                        for i in 0..added.length() {
                            if let Some(n) = added.item(i)
                                && !skipped(&n)
                            {
                                translate_tree(&n);
                            }
                        }
                    }
                    "characterData" => {
                        if !skipped(&target) {
                            translate_text(&target);
                        }
                    }
                    "attributes" => {
                        if let (Some(el), Some(name)) =
                            (target.dyn_ref::<Element>(), r.attribute_name())
                            && !skipped(&target)
                        {
                            translate_attr(el, &name);
                        }
                    }
                    _ => {}
                }
            }
        },
    );
    let Ok(obs) = MutationObserver::new(cb.as_ref().unchecked_ref()) else {
        return;
    };
    cb.forget();
    let opts = MutationObserverInit::new();
    opts.set_child_list(true);
    opts.set_subtree(true);
    opts.set_character_data(true);
    opts.set_attributes(true);
    let filter = js_sys::Array::new();
    for a in ATTRS {
        filter.push(&a.into());
    }
    opts.set_attribute_filter(&filter);
    if let Some(root) = doc.document_element() {
        let _ = obs.observe_with_options(&root, &opts);
    }
    // 見張りは画面を閉じるまで続ける。
    std::mem::forget(obs);
}
