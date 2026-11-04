//! ブラウザで起きたエラーをサーバーへ知らせる(`POST /api/errors`)。
//!
//! - wasm のパニック(コンソールにも出す)
//! - JavaScript の `error` と、処理されなかった Promise の失敗(`unhandledrejection`)
//!
//! `navigator.sendBeacon` で送るので、ページを閉じる間際やパニックの直後でも届く。
//! 同じ内容は 1 回だけ、1 ページあたり [`MAX_REPORTS`] 件までにする。

use std::cell::RefCell;

use board_core::api::ops::ClientErrorReport;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

const MAX_REPORTS: usize = 10;

thread_local! {
    static SENT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// パニックの記録と、ページ全体のエラーの受け取りを始める。
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        console_error_panic_hook::hook(info);
        let err = js_sys::Error::new("");
        let stack = js_sys::Reflect::get(&err, &JsValue::from_str("stack"))
            .ok()
            .and_then(|s| s.as_string());
        let source = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        // 場所は Rust のファイルなので、行番号は外さずにまとめる(版ごとに別のまとまりになってよい)。
        let message = match info.payload().downcast_ref::<&str>() {
            Some(s) => s.to_string(),
            None => info
                .payload()
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| info.to_string()),
        };
        report("panic", message, stack, source);
    }));
    let Some(window) = web_sys::window() else {
        return;
    };
    let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(|e: web_sys::Event| {
        let get = |name: &str| js_sys::Reflect::get(&e, &JsValue::from_str(name)).ok();
        let message = get("message")
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        // 画像の読み込み失敗などの「メッセージのないエラー」は拾わない。
        if message.is_empty() {
            return;
        }
        let file = get("filename")
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        let line = get("lineno").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let col = get("colno").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let stack = get("error").and_then(|err| {
            js_sys::Reflect::get(&err, &JsValue::from_str("stack"))
                .ok()?
                .as_string()
        });
        let source = (!file.is_empty()).then(|| format!("{file}:{line}:{col}"));
        report("error", message, stack, source);
    });
    let on_rejection = Closure::<dyn FnMut(web_sys::Event)>::new(|e: web_sys::Event| {
        let reason = js_sys::Reflect::get(&e, &JsValue::from_str("reason")).unwrap_or_default();
        let (message, stack) = match reason.dyn_ref::<js_sys::Error>() {
            Some(err) => (
                String::from(err.message()),
                js_sys::Reflect::get(err, &JsValue::from_str("stack"))
                    .ok()
                    .and_then(|s| s.as_string()),
            ),
            None => (
                reason
                    .as_string()
                    .or_else(|| js_sys::JSON::stringify(&reason).ok().map(String::from))
                    .unwrap_or_else(|| "(理由なし)".into()),
                None,
            ),
        };
        report("unhandledrejection", message, stack, None);
    });
    let _ = window.add_event_listener_with_callback("error", on_error.as_ref().unchecked_ref());
    let _ = window.add_event_listener_with_callback(
        "unhandledrejection",
        on_rejection.as_ref().unchecked_ref(),
    );
    on_error.forget();
    on_rejection.forget();
}

/// 1 件送る。
pub fn report(kind: &str, message: String, stack: Option<String>, source: Option<String>) {
    let key = format!("{kind}\n{message}\n{}", source.as_deref().unwrap_or(""));
    let fresh = SENT.with(|s| {
        let mut s = s.borrow_mut();
        if s.len() >= MAX_REPORTS || s.contains(&key) {
            return false;
        }
        s.push(key);
        true
    });
    if !fresh {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let location = window.location();
    let url = location.href().ok();
    let board = location
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("room"));
    // 共有リンクの鍵はサーバーの記録に残さない。
    let url = url.map(|u| strip_param(&u, "key"));
    let body = ClientErrorReport {
        kind: kind.into(),
        message: message.chars().take(1000).collect(),
        stack: stack.map(|s| s.chars().take(8000).collect()),
        source,
        url,
        board,
        release: Some(env!("CARGO_PKG_VERSION").into()),
    };
    let Ok(json) = serde_json::to_string(&body) else {
        return;
    };
    let endpoint = format!("{}/api/errors", crate::http::base());
    let _ = window
        .navigator()
        .send_beacon_with_opt_str(&endpoint, Some(&json));
}

/// URL から `name=` のクエリを外す。
fn strip_param(url: &str, name: &str) -> String {
    let Some((head, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let (query, hash) = match query.split_once('#') {
        Some((q, h)) => (q, Some(h)),
        None => (query, None),
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|kv| kv.split('=').next() != Some(name))
        .collect();
    let mut out = head.to_string();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(h) = hash {
        out.push('#');
        out.push_str(h);
    }
    out
}
