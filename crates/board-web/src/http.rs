//! サーバーの HTTP API(`/api/...`)を呼ぶ。型は [`board_core::api`]。

use board_core::api::ApiError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestCredentials, RequestInit, Response};

/// API 呼び出しの失敗。
#[derive(Clone, Debug)]
pub enum HttpError {
    /// サーバーにつながらない、または API のないサーバー(静的配信だけ等)。
    Unavailable,
    /// サーバーが断った(ステータスと理由)。
    Status(u16, String),
}

impl HttpError {
    /// 利用者に見せる文。
    pub fn message(&self) -> String {
        match self {
            Self::Unavailable => "サーバーにつながりません".into(),
            Self::Status(_, m) => m.clone(),
        }
    }
}

/// API の基点。`?server=ws://host:port` のときはそのホストの HTTP にする。
pub fn base() -> String {
    let Some(window) = web_sys::window() else {
        return String::new();
    };
    let server = window
        .location()
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("server"));
    match server {
        Some(s) => s
            .trim_end_matches('/')
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1),
        None => String::new(),
    }
}

async fn call(method: &str, path: &str, body: Option<String>) -> Result<Option<String>, HttpError> {
    let window = web_sys::window().ok_or(HttpError::Unavailable)?;
    let init = RequestInit::new();
    init.set_method(method);
    init.set_credentials(RequestCredentials::Include);
    if let Some(b) = &body {
        init.set_body(&b.into());
    }
    let req = Request::new_with_str_and_init(&format!("{}/api{path}", base()), &init)
        .map_err(|_| HttpError::Unavailable)?;
    if body.is_some() {
        let _ = req.headers().set("Content-Type", "application/json");
    }
    let res = JsFuture::from(window.fetch_with_request(&req))
        .await
        .map_err(|_| HttpError::Unavailable)?;
    let res: Response = res.dyn_into().map_err(|_| HttpError::Unavailable)?;
    let status = res.status();
    let is_json = res
        .headers()
        .get("Content-Type")
        .ok()
        .flatten()
        .is_some_and(|t| t.contains("json"));
    let text = match res.text() {
        Ok(p) => JsFuture::from(p)
            .await
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    if (200..300).contains(&status) {
        if status == 204 {
            return Ok(None);
        }
        // API のない静的サーバーは index.html などを返してくる。
        if !is_json {
            return Err(HttpError::Unavailable);
        }
        return Ok(Some(text));
    }
    if !is_json {
        return Err(HttpError::Unavailable);
    }
    let msg = serde_json::from_str::<ApiError>(&text)
        .map(|e| e.error)
        .unwrap_or_else(|_| format!("エラー({status})"));
    Err(HttpError::Status(status, msg))
}

fn parse<T: DeserializeOwned>(text: Option<String>) -> Result<T, HttpError> {
    serde_json::from_str(text.as_deref().unwrap_or("null")).map_err(|e| {
        log::warn!("応答を読めません: {e}");
        HttpError::Unavailable
    })
}

pub async fn get<T: DeserializeOwned>(path: &str) -> Result<T, HttpError> {
    parse(call("GET", path, None).await?)
}

pub async fn send<B: Serialize, T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: &B,
) -> Result<T, HttpError> {
    let body = serde_json::to_string(body).map_err(|_| HttpError::Unavailable)?;
    parse(call(method, path, Some(body)).await?)
}

/// バイナリを受け取る(バージョンの中身など)。
pub async fn get_bytes(path: &str) -> Result<Vec<u8>, HttpError> {
    let window = web_sys::window().ok_or(HttpError::Unavailable)?;
    let init = RequestInit::new();
    init.set_method("GET");
    init.set_credentials(RequestCredentials::Include);
    let req = Request::new_with_str_and_init(&format!("{}/api{path}", base()), &init)
        .map_err(|_| HttpError::Unavailable)?;
    let res: Response = JsFuture::from(window.fetch_with_request(&req))
        .await
        .map_err(|_| HttpError::Unavailable)?
        .dyn_into()
        .map_err(|_| HttpError::Unavailable)?;
    if !res.ok() {
        return Err(HttpError::Status(
            res.status(),
            format!("エラー({})", res.status()),
        ));
    }
    let buf = JsFuture::from(res.array_buffer().map_err(|_| HttpError::Unavailable)?)
        .await
        .map_err(|_| HttpError::Unavailable)?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// バイナリを送って JSON を受け取る(動画メモの保存など)。
pub async fn post_bytes<T: DeserializeOwned>(
    path: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<T, HttpError> {
    let window = web_sys::window().ok_or(HttpError::Unavailable)?;
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_credentials(RequestCredentials::Include);
    let body = js_sys::Uint8Array::from(bytes);
    init.set_body(&body);
    let req = Request::new_with_str_and_init(&format!("{}/api{path}", base()), &init)
        .map_err(|_| HttpError::Unavailable)?;
    let _ = req.headers().set("Content-Type", content_type);
    let res: Response = JsFuture::from(window.fetch_with_request(&req))
        .await
        .map_err(|_| HttpError::Unavailable)?
        .dyn_into()
        .map_err(|_| HttpError::Unavailable)?;
    let status = res.status();
    let text = match res.text() {
        Ok(p) => JsFuture::from(p)
            .await
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    if !(200..300).contains(&status) {
        let msg = serde_json::from_str::<ApiError>(&text)
            .map(|e| e.error)
            .unwrap_or_else(|_| format!("エラー({status})"));
        return Err(HttpError::Status(status, msg));
    }
    parse(Some(text))
}

/// 応答の中身を使わない呼び出し。
pub async fn send_empty(method: &str, path: &str) -> Result<(), HttpError> {
    call(method, path, None).await.map(|_| ())
}

/// URL の部品として安全な形にする(ボード ID や鍵は英数字だけだが念のため)。
pub fn encode(s: &str) -> String {
    String::from(js_sys::encode_uri_component(s))
}

/// HTML に埋め込む文字列をエスケープする。
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `/api/boards/{id}` に鍵を付けたパス。
pub fn board_path(id: &str, key: Option<&str>, suffix: &str) -> String {
    match key {
        Some(k) => format!("/boards/{}{suffix}?key={}", encode(id), encode(k)),
        None => format!("/boards/{}{suffix}", encode(id)),
    }
}
