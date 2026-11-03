//! 生成 AI で図を作る(サーバーの `/api/ai/diagram` 経由で Claude API を呼ぶ)。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::diagram::DiagramSpec;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Document, HtmlButtonElement, HtmlElement, HtmlSelectElement, HtmlTextAreaElement,
    KeyboardEvent, RequestInit, Response, Window,
};

use super::{App, by_id, js_err, listen};

/// 画面の部品。
#[derive(Clone)]
struct Ui {
    prompt: HtmlTextAreaElement,
    mode: HtmlSelectElement,
    button: HtmlButtonElement,
    status: HtmlElement,
}

impl Ui {
    fn set_status(&self, msg: &str, error: bool) {
        self.status.set_text_content(Some(msg));
        if error {
            let _ = self.status.set_attribute("data-kind", "error");
        } else {
            let _ = self.status.remove_attribute("data-kind");
        }
    }

    fn set_busy(&self, busy: bool) {
        self.button.set_disabled(busy);
        self.button
            .set_text_content(Some(if busy { "生成中…" } else { "生成" }));
    }
}

/// API の URL。`?server=ws://host:port` で別ホストにつないでいるときはそちらへ。
fn api_url(window: &Window, path: &str) -> String {
    let server = window
        .location()
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("server"));
    match server {
        Some(s) => {
            let base = s
                .trim_end_matches('/')
                .replacen("wss://", "https://", 1)
                .replacen("ws://", "http://", 1);
            format!("{base}{path}")
        }
        None => path.to_string(),
    }
}

/// URL の `?key=`(共有リンクの鍵)。
fn share_key(window: &Window) -> Option<String> {
    window
        .location()
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("key"))
}

/// JSON を送って、(HTTP ステータス, 本文) を受け取る。
async fn fetch_json(
    window: &Window,
    method: &str,
    url: &str,
    body: Option<String>,
) -> Result<(u16, String), String> {
    let init = RequestInit::new();
    init.set_method(method);
    if let Some(b) = body {
        init.set_body(&JsValue::from_str(&b));
        let headers = web_sys::Headers::new().map_err(js_err)?;
        headers
            .set("Content-Type", "application/json")
            .map_err(js_err)?;
        init.set_headers(&headers);
    }
    let resp = JsFuture::from(window.fetch_with_str_and_init(url, &init))
        .await
        .map_err(|_| "サーバーに接続できません".to_string())?;
    let resp: Response = resp.dyn_into().map_err(js_err)?;
    let text = JsFuture::from(resp.text().map_err(js_err)?)
        .await
        .map_err(js_err)?
        .as_string()
        .unwrap_or_default();
    Ok((resp.status(), text))
}

/// エラー応答の本文から表示用のメッセージを取り出す。
fn error_message(status: u16, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("生成に失敗しました(HTTP {status})"))
}

async fn generate(app: Rc<RefCell<App>>, ui: Ui) {
    let prompt = ui.prompt.value();
    if prompt.trim().is_empty() {
        ui.set_status("作りたい図の説明を入力してください", true);
        let _ = ui.prompt.focus();
        return;
    }
    let (window, room) = {
        let a = app.borrow();
        (a.window.clone(), a.room.clone())
    };
    let Some(room) = room else {
        ui.set_status("生成 AI は共同編集のボード(?room=…)で使えます", true);
        return;
    };
    if crate::local_ai::cloud_off() {
        ui.set_status(crate::local_ai::CLOUD_OFF_MESSAGE, true);
        return;
    }
    ui.set_busy(true);
    ui.set_status(
        "Claude が図を考えています…(数十秒かかることがあります)",
        false,
    );
    let body = json!({ "prompt": prompt, "mode": ui.mode.value() }).to_string();
    let mut path = format!("/api/boards/{room}/ai/diagram");
    if let Some(key) = share_key(&window) {
        let key = String::from(js_sys::encode_uri_component(&key));
        path.push_str(&format!("?key={key}"));
    }
    let result = fetch_json(&window, "POST", &api_url(&window, &path), Some(body)).await;
    ui.set_busy(false);
    match result {
        Ok((200, text)) => match DiagramSpec::from_json(&text) {
            Ok(spec) => {
                let n = app.borrow_mut().insert_diagram(&spec);
                let title = if spec.title.trim().is_empty() {
                    "図".to_string()
                } else {
                    format!("「{}」", spec.title.trim())
                };
                ui.set_status(
                    &format!("{title}を追加しました({n} 要素)。Ctrl/⌘+Z で取り消せます"),
                    false,
                );
            }
            Err(e) => ui.set_status(&e, true),
        },
        Ok((status, text)) => ui.set_status(&error_message(status, &text), true),
        Err(e) => ui.set_status(&e, true),
    }
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    let ui = Ui {
        prompt: by_id(doc, "ai-prompt")?,
        mode: by_id(doc, "ai-mode")?,
        button: by_id(doc, "ai-generate")?,
        status: by_id(doc, "ai-diagram-status")?,
    };
    {
        let app = app.clone();
        let ui2 = ui.clone();
        listen(
            ui.button.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                spawn_local(generate(app.clone(), ui2.clone()));
            },
        )?;
    }
    {
        // Ctrl/⌘+Enter で生成。
        let app = app.clone();
        let ui2 = ui.clone();
        listen(
            ui.prompt.as_ref(),
            "keydown",
            false,
            move |e: KeyboardEvent| {
                if e.key() == "Enter" && (e.ctrl_key() || e.meta_key()) && !e.is_composing() {
                    e.prevent_default();
                    if !ui2.button.disabled() {
                        spawn_local(generate(app.clone(), ui2.clone()));
                    }
                }
            },
        )?;
    }

    // サーバーで有効かどうかを先に確かめて案内する。
    let window = app.borrow().window.clone();
    spawn_local(async move {
        match fetch_json(&window, "GET", &api_url(&window, "/api/ai/status"), None).await {
            Ok((200, text)) => {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                // ブラウザ内 AI のモデルの取得元(社内に置いたモデルなど)。
                if let Some(url) = v["local_model_url"].as_str() {
                    crate::local_ai::set_model_url(url);
                }
                if let Some(url) = v["local_encoder_url"].as_str() {
                    crate::local_ai::set_encoder_url(url);
                }
                if v["enabled"] == true {
                    ui.set_status("説明を書いて「生成」(Ctrl/⌘+Enter)", false);
                } else {
                    ui.set_status(
                        "サーバーで生成 AI が無効です(ANTHROPIC_API_KEY を設定すると使えます)",
                        true,
                    );
                    ui.button.set_disabled(true);
                }
            }
            _ => {
                ui.set_status("サーバーにつながっていないため生成 AI は使えません", true);
                ui.button.set_disabled(true);
            }
        }
    });
    Ok(())
}
