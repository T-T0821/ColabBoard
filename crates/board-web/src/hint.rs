//! 長い説明文を「?」の吹き出しにしまう。
//!
//! パネルに説明文を並べると読みにくく場所も取るので、見出しの横に小さな「?」を置き、
//! マウスを乗せる・キーボードでフォーカスする・タップすると吹き出しで読めるようにする。
//!
//! ```html
//! <span class="hint"><button type="button" class="hint-btn" aria-label="説明" aria-expanded="false">?</button><span class="hint-body">説明文</span></span>
//! ```
//!
//! 説明文(`.hint-body`)は見た目だけ隠して元の場所に置いたままにする。英語表示では自動で訳され、
//! 画面読み上げにもそのまま読まれる。開いたときは、その中身を `body` 直下の吹き出し 1 つに写して
//! 画面に収まる位置へ出す(パネルの `backdrop-filter` やスクロールで切れないように)。

use std::cell::RefCell;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Document, Element, Event, HtmlElement, KeyboardEvent, Node, PointerEvent};

/// 「?」と説明文の HTML。`text` はそのまま HTML として入れる(呼び出し側で決めた固定の文だけを渡す)。
pub fn html(text: &str) -> String {
    format!(
        r#"<span class="hint"><button type="button" class="hint-btn" aria-label="説明" aria-expanded="false">?</button><span class="hint-body">{text}</span></span>"#
    )
}

thread_local! {
    /// いま吹き出しを出している「?」(`.hint`)。
    static OPEN: RefCell<Option<Element>> = const { RefCell::new(None) };
}

/// 起動時に一度だけ呼ぶ。ページ全体で「?」の操作を受け付ける(後から作られた「?」にも効く)。
pub fn install(doc: &Document) {
    // マウスを乗せたら開き、離れたら閉じる(クリックで固定したものは残す)。
    listen(doc, "pointerover", false, |e| {
        if is_mouse(&e)
            && let Some(h) = hint_of(&e)
        {
            open(&h);
        }
    });
    listen(doc, "pointerout", false, |e| {
        if !is_mouse(&e) {
            return;
        }
        let Some(h) = hint_of(&e) else { return };
        let to = e
            .dyn_ref::<PointerEvent>()
            .and_then(|p| p.related_target())
            .and_then(|t| t.dyn_into::<Node>().ok());
        if to.is_some_and(|n| h.contains(Some(&n))) || is_pinned(&h) || has_focus(&h) {
            return;
        }
        close();
    });
    // キーボードで「?」に来たら開き、離れたら閉じる。
    listen(doc, "focusin", false, |e| {
        if let Some(h) = hint_of(&e) {
            open(&h);
        }
    });
    listen(doc, "focusout", false, |e| {
        if let Some(h) = hint_of(&e) {
            unpin(&h);
            close();
        }
    });
    // クリック・タップで開いたままにする。もう一度押すか、ほかの場所を押すと閉じる。
    // ラベルや summary の中に置いても、そちらのクリック動作(チェックの切り替えなど)は起こさない。
    listen(doc, "click", true, |e| {
        let btn = e
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|t| t.closest(".hint-btn").ok().flatten());
        let Some(btn) = btn else {
            close();
            return;
        };
        e.prevent_default();
        e.stop_propagation();
        let Some(h) = btn.closest(".hint").ok().flatten() else {
            return;
        };
        if is_pinned(&h) {
            unpin(&h);
            close();
        } else {
            open(&h);
            let _ = btn.set_attribute("aria-expanded", "true");
        }
    });
    listen(doc, "keydown", false, |e| {
        if e.dyn_ref::<KeyboardEvent>().map(|k| k.key()).as_deref() == Some("Escape") {
            close();
        }
    });
    // パネルがスクロールすると吹き出しの位置がずれるので閉じる。
    listen(doc, "scroll", true, |_| close());
    if let Some(w) = web_sys::window() {
        let cb = Closure::<dyn Fn()>::new(close);
        let _ = w.add_event_listener_with_callback("resize", cb.as_ref().unchecked_ref());
        cb.forget();
    }
}

fn listen(doc: &Document, name: &str, capture: bool, f: impl Fn(Event) + 'static) {
    let cb = Closure::<dyn Fn(Event)>::new(f);
    let _ =
        doc.add_event_listener_with_callback_and_bool(name, cb.as_ref().unchecked_ref(), capture);
    cb.forget();
}

fn is_mouse(e: &Event) -> bool {
    e.dyn_ref::<PointerEvent>()
        .is_some_and(|p| p.pointer_type() == "mouse")
}

/// イベントの起きた場所を含む `.hint`。
fn hint_of(e: &Event) -> Option<Element> {
    e.target()?
        .dyn_into::<Element>()
        .ok()?
        .closest(".hint")
        .ok()
        .flatten()
}

fn button_of(h: &Element) -> Option<Element> {
    h.query_selector(".hint-btn").ok().flatten()
}

fn is_pinned(h: &Element) -> bool {
    button_of(h)
        .and_then(|b| b.get_attribute("aria-expanded"))
        .as_deref()
        == Some("true")
}

fn unpin(h: &Element) {
    if let Some(b) = button_of(h) {
        let _ = b.set_attribute("aria-expanded", "false");
    }
}

fn has_focus(h: &Element) -> bool {
    h.owner_document()
        .and_then(|d| d.active_element())
        .is_some_and(|a| h.contains(Some(&a)))
}

/// `body` 直下の吹き出し(なければ作る)。画面読み上げには元の説明文を読ませるので隠しておく。
fn bubble(doc: &Document) -> Option<HtmlElement> {
    if let Some(b) = doc.get_element_by_id("hint-bubble") {
        return b.dyn_into().ok();
    }
    let b: HtmlElement = doc.create_element("div").ok()?.dyn_into().ok()?;
    b.set_id("hint-bubble");
    let _ = b.set_attribute("aria-hidden", "true");
    // 中身は訳し終えた説明文を写すので、ここで訳し直さない。
    let _ = b.set_attribute("translate", "no");
    doc.body()?.append_child(&b).ok()?;
    Some(b)
}

/// 開いている吹き出しを閉じる。
fn close() {
    let Some(h) = OPEN.with(|o| o.borrow_mut().take()) else {
        return;
    };
    unpin(&h);
    let _ = h.remove_attribute("data-open");
    if let Some(b) = h
        .owner_document()
        .and_then(|d| d.get_element_by_id("hint-bubble"))
    {
        let _ = b.remove_attribute("data-open");
    }
}

/// 吹き出しを開き、「?」の下(入らなければ上)の画面内に収まる位置へ置く。
fn open(h: &Element) {
    if OPEN.with(|o| o.borrow().as_ref() == Some(h)) {
        return;
    }
    close();
    let (Some(doc), Some(btn), Ok(Some(body))) = (
        h.owner_document(),
        button_of(h),
        h.query_selector(".hint-body"),
    ) else {
        return;
    };
    let Some(bub) = bubble(&doc) else { return };
    bub.set_inner_html(&body.inner_html());
    let _ = h.set_attribute("data-open", "");
    let _ = bub.set_attribute("data-open", "");
    OPEN.with(|o| *o.borrow_mut() = Some(h.clone()));

    let Some(win) = web_sys::window() else { return };
    let size =
        |v: Result<wasm_bindgen::JsValue, _>, d: f64| v.ok().and_then(|v| v.as_f64()).unwrap_or(d);
    let vw = size(win.inner_width(), 1024.0);
    let vh = size(win.inner_height(), 768.0);
    const GAP: f64 = 8.0;
    const EDGE: f64 = 8.0;
    let r = btn.get_bounding_client_rect();
    let b = bub.get_bounding_client_rect();
    let (w, hgt) = (b.width(), b.height());
    let cx = r.left() + r.width() / 2.0;
    let left = (cx - w / 2.0).clamp(EDGE, (vw - w - EDGE).max(EDGE));
    let below = r.bottom() + GAP;
    let above = r.top() - GAP - hgt;
    let side = if below + hgt <= vh - EDGE || above < EDGE {
        "below"
    } else {
        "above"
    };
    let top = if side == "below" { below } else { above };
    let style = bub.style();
    let _ = style.set_property("left", &format!("{left:.0}px"));
    let _ = style.set_property("top", &format!("{top:.0}px"));
    // しっぽ(三角)は「?」の真上・真下に向ける。
    let tail = (cx - left).clamp(12.0, (w - 12.0).max(12.0));
    let _ = style.set_property("--tail-x", &format!("{tail:.0}px"));
    let _ = bub.set_attribute("data-side", side);
}
