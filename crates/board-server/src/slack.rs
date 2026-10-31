//! Slack 通知(Incoming Webhook)。
//!
//! ボードのオーナーが共有パネルで Webhook URL を登録すると、コメント・メンション・
//! バージョンの保存をそのチャンネルに知らせる。
//!
//! - Webhook URL はボードの情報([`BoardMeta`](crate::boards::BoardMeta))に保存し、API では末尾だけ返す。
//! - 送り先は `https://hooks.slack.com/` で始まる URL だけ(サーバーから任意の URL へ送らせないため)。
//!   環境変数 `SLACK_WEBHOOK_PREFIXES`(カンマ区切り)で変えられる。
//! - 送信は裏で行い、失敗してもコメントなどの操作は失敗させない。
//! - メッセージのリンクには `PUBLIC_URL`(例 `https://board.example.com/`)を使う。なければリンクなし。

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use board_core::api::{Comment, SetSlack, SlackEvents, SlackSettings};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::api::{self, ApiResult, Error};
use crate::auth;
use crate::boards::{self, BoardMeta};

pub const DEFAULT_PREFIX: &str = "https://hooks.slack.com/";
const MAX_URL_LEN: usize = 512;
const EXCERPT_CHARS: usize = 200;

/// ボードに保存する Slack の設定。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlackHook {
    pub url: String,
    #[serde(default)]
    pub events: SlackEvents,
}

impl SlackHook {
    pub fn settings(hook: Option<&SlackHook>) -> SlackSettings {
        match hook {
            Some(h) => SlackSettings {
                enabled: true,
                hint: Some(hint(&h.url)),
                events: h.events,
            },
            None => SlackSettings::default(),
        }
    }
}

/// URL の末尾 4 文字だけ見せる。
fn hint(url: &str) -> String {
    let tail: String = url
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

/// サーバー全体の設定。
pub struct SlackConfig {
    /// 送ってよい Webhook URL の先頭。
    pub allowed_prefixes: Vec<String>,
    /// メッセージに載せるボードの URL の土台(末尾 `/` 付き)。
    pub public_url: Option<String>,
    http: reqwest::Client,
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self::new(vec![DEFAULT_PREFIX.to_string()], None)
    }
}

impl SlackConfig {
    pub fn new(allowed_prefixes: Vec<String>, public_url: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP クライアントを作れません");
        Self {
            allowed_prefixes,
            public_url: public_url.map(|u| {
                let u = u.trim().to_string();
                if u.ends_with('/') { u } else { format!("{u}/") }
            }),
            http,
        }
    }

    /// 環境変数 `SLACK_WEBHOOK_PREFIXES` と `PUBLIC_URL` から読む。
    pub fn from_env() -> Self {
        let prefixes = std::env::var("SLACK_WEBHOOK_PREFIXES")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec![DEFAULT_PREFIX.to_string()]);
        let public_url = std::env::var("PUBLIC_URL")
            .ok()
            .filter(|u| !u.trim().is_empty());
        Self::new(prefixes, public_url)
    }

    pub fn allows(&self, url: &str) -> bool {
        url.len() <= MAX_URL_LEN
            && !url.chars().any(|c| c.is_whitespace() || c.is_control())
            && self
                .allowed_prefixes
                .iter()
                .any(|p| url.starts_with(p.as_str()) && url.len() > p.len())
    }

    fn board_link(&self, meta: &BoardMeta) -> String {
        let title = escape(&meta.title);
        match &self.public_url {
            // メンバーだけが開ける URL(共有リンクの鍵は載せない)。
            Some(base) => format!("<{base}?room={}|{title}>", meta.id),
            None => format!("「{title}」"),
        }
    }

    /// 裏で送る。
    pub fn send(&self, url: &str, text: String) {
        let http = self.http.clone();
        let url = url.to_string();
        tokio::spawn(async move {
            if let Err(e) = post(&http, &url, &text).await {
                tracing::warn!("Slack に送れませんでした: {e}");
            }
        });
    }
}

async fn post(http: &reqwest::Client, url: &str, text: &str) -> Result<(), String> {
    let resp = http
        .post(url)
        .json(&json!({ "text": text }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {}", resp.status()))
    }
}

/// Slack の mrkdwn で特別な意味を持つ文字を逃がす。
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn quote(body: &str) -> String {
    let mut s: String = body.chars().take(EXCERPT_CHARS).collect();
    if body.chars().count() > EXCERPT_CHARS {
        s.push('…');
    }
    escape(&s)
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// コメントを知らせる文面。送らない設定なら `None`。
pub fn comment_text(
    config: &SlackConfig,
    hook: &SlackHook,
    meta: &BoardMeta,
    c: &Comment,
) -> Option<String> {
    let mentioned = !c.mentions.is_empty();
    if !(hook.events.comments || hook.events.mentions && mentioned) {
        return None;
    }
    let verb = if c.thread.is_some() {
        "返信しました"
    } else {
        "コメントしました"
    };
    let mut text = format!(
        "*{}* さんが {} に{verb}\n{}",
        escape(&c.author_name),
        config.board_link(meta),
        quote(&c.body)
    );
    if mentioned {
        let names: Vec<String> = c.mentions.iter().map(|l| format!("@{l}")).collect();
        text.push_str(&format!("\nメンション: {}", names.join(" ")));
    }
    Some(text)
}

/// バージョンの保存・復元を知らせる文面。
pub fn version_text(
    config: &SlackConfig,
    hook: &SlackHook,
    meta: &BoardMeta,
    who: Option<&str>,
    label: &str,
    restored: bool,
) -> Option<String> {
    if !hook.events.versions {
        return None;
    }
    let who = who.map_or_else(|| "誰か".to_string(), |w| format!("*{}* さん", escape(w)));
    let what = if restored {
        "に戻しました"
    } else {
        "として保存しました"
    };
    Some(format!(
        "{who}が {} をバージョン「{}」{what}",
        config.board_link(meta),
        escape(label)
    ))
}

/// ボードに Slack の設定があれば送る。
pub fn notify(
    state: &AppState,
    meta: Option<&BoardMeta>,
    text: impl FnOnce(&SlackConfig, &SlackHook, &BoardMeta) -> Option<String>,
) {
    let Some(meta) = meta else { return };
    let Some(hook) = &meta.slack else { return };
    if !state.config.slack.allows(&hook.url) {
        return;
    }
    if let Some(t) = text(&state.config.slack, hook, meta) {
        state.config.slack.send(&hook.url, t);
    }
}

// ---- API ----

pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<SlackSettings>> {
    let meta = api::owner_board(&state, &headers, &id)?;
    Ok(Json(SlackHook::settings(meta.slack.as_ref())))
}

pub async fn set_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SetSlack>,
) -> ApiResult<Json<SlackSettings>> {
    let mut meta = api::owner_board(&state, &headers, &id)?;
    let url = match req.webhook.as_deref().map(str::trim) {
        Some("") => None,
        Some(u) => {
            if !state.config.slack.allows(u) {
                return Err(Error::bad_request(format!(
                    "Slack の Incoming Webhook URL({} で始まるもの)を入力してください",
                    state
                        .config
                        .slack
                        .allowed_prefixes
                        .first()
                        .map_or(DEFAULT_PREFIX, String::as_str)
                )));
            }
            Some(u.to_string())
        }
        None => meta.slack.as_ref().map(|h| h.url.clone()),
    };
    meta.slack = url.map(|url| SlackHook {
        url,
        events: req.events,
    });
    meta.updated = auth::now_ms();
    boards::put(state.store(), &meta)?;
    Ok(Json(SlackHook::settings(meta.slack.as_ref())))
}

pub async fn send_test(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let meta = api::owner_board(&state, &headers, &id)?;
    let hook = meta
        .slack
        .as_ref()
        .filter(|h| state.config.slack.allows(&h.url))
        .ok_or_else(|| Error::bad_request("Slack の Webhook URL が設定されていません"))?;
    let text = format!(
        "グラボード の {} からのテスト通知です。",
        state.config.slack.board_link(&meta)
    );
    post(&state.config.slack.http, &hook.url, &text)
        .await
        .map_err(|e| {
            Error::new(
                StatusCode::BAD_GATEWAY,
                format!("Slack に送れませんでした({e})"),
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_urls() {
        let c = SlackConfig::default();
        assert!(c.allows("https://hooks.slack.com/services/T0/B0/xyz"));
        assert!(!c.allows("https://hooks.slack.com/"));
        assert!(!c.allows("http://hooks.slack.com/services/x"));
        assert!(!c.allows("https://hooks.slack.com.evil.example/x"));
        assert!(!c.allows("https://example.com/hooks.slack.com/x"));
        assert!(!c.allows("https://hooks.slack.com/services/a b"));
    }

    #[test]
    fn escapes_and_hints() {
        assert_eq!(escape("<a> & b"), "&lt;a&gt; &amp; b");
        assert_eq!(hint("https://hooks.slack.com/services/abcdef"), "…cdef");
        assert_eq!(quote("1行目\n2行目"), "> 1行目\n> 2行目");
    }
}
