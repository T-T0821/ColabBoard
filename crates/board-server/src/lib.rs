//! グラボード の同期サーバー。
//!
//! - `GET /ws/{room}?key=`: WebSocket。メッセージ形式は [`board_core::protocol`]。
//! - `GET /api/rtc`: 音声通話の ICE サーバー。通話の接続手順は WebSocket で中継する。
//! - `/api/v1/...`: 外部からボードを操作する REST API([`rest`])。API トークン([`tokens`])でも呼べる。
//! - `/api/...`: アカウント・ボード一覧・共有設定([`api`])、AI による付箋の要約・クラスタリング([`ai`])と図の生成([`ai_diagram`])、Slack 通知([`slack`])、GitHub・Jira の課題カード([`issues`])。
//! - それ以外: 静的ファイル(ビルド済みの Web クライアント)。
//!
//! サーバーはルームごとに yrs ドキュメントを持ち、届いた更新を取り込んで同じルームの
//! 他の参加者へ配る。状態は定期的に redb へ保存する。
//! 接続時に権限([`boards::role_of`])を確かめ、編集権限のない人からの更新は捨てる。

pub mod ai;
pub mod ai_diagram;
pub mod api;
pub mod auth;
pub mod boards;
pub mod comments;
#[cfg(feature = "embed")]
mod embedded;
pub mod issues;
pub mod rest;
pub mod room;
pub mod slack;
pub mod store;
pub mod tokens;
pub mod versions;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use board_core::api::Role;
use board_core::protocol::Message;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;
use tower_http::services::ServeDir;

pub use room::Hub;
pub use store::Store;

/// 1 通の上限の既定値(大きなボードの初回同期を考えて余裕を持たせる。10 万要素で約 28 MB)。
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 128 * 1024 * 1024;
/// 保存の間隔。
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(2);

pub struct Config {
    /// 配信する Web クライアントのディレクトリ。`None` なら、バイナリに埋め込んだもの
    /// (`embed` 機能付きでビルドしたとき)を配る。
    pub static_dir: Option<PathBuf>,
    /// WebSocket で受け取る 1 通の上限(バイト)。
    pub max_message_bytes: usize,
    /// ログインせずに `?room=` で作る「誰でも編集できるボード」を許すか。
    pub anonymous_boards: bool,
    /// Cookie に `Secure` を付けるか(HTTPS で公開するとき)。
    pub secure_cookies: bool,
    /// AI 機能(付箋の要約・クラスタリング)。`None` なら無効。
    pub ai: Option<Arc<ai::Ai>>,
    /// Slack 通知(送ってよい Webhook URL とメッセージのリンク)。
    pub slack: slack::SlackConfig,
    /// GitHub・Jira の課題カード。
    pub issues: issues::IssuesConfig,
    /// 音声通話の ICE サーバー(`RTCIceServer` の配列)。`GET /api/rtc` で配る。
    pub ice_servers: serde_json::Value,
}

/// 既定の ICE サーバー(公開 STUN)。
pub fn default_ice_servers() -> serde_json::Value {
    serde_json::json!([{ "urls": "stun:stun.l.google.com:19302" }])
}

/// 環境変数 `ICE_SERVERS` の JSON 配列を読む。読めなければ既定値。
pub fn ice_servers_from_env(raw: Option<String>) -> serde_json::Value {
    match raw
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
    {
        Some(Ok(v)) if v.is_array() => v,
        Some(_) => {
            tracing::warn!("ICE_SERVERS は JSON の配列で指定してください。既定値を使います");
            default_ice_servers()
        }
        None => default_ice_servers(),
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            static_dir: None,
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            anonymous_boards: true,
            secure_cookies: false,
            ai: None,
            slack: slack::SlackConfig::default(),
            issues: issues::IssuesConfig::default(),
            ice_servers: default_ice_servers(),
        }
    }
}

/// ハンドラーから使う共有の状態。
pub struct AppState {
    pub hub: Arc<Hub>,
    pub config: Config,
}

impl AppState {
    pub fn store(&self) -> &Arc<Store> {
        self.hub.store()
    }
}

pub fn router(hub: Arc<Hub>, mut config: Config) -> Router {
    let static_dir = config.static_dir.take();
    let state = Arc::new(AppState { hub, config });
    let app = Router::new()
        .route("/ws/{room}", get(ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/boards/{id}/ai/summarize", post(ai::summarize))
        .route("/api/boards/{id}/ai/cluster", post(ai::cluster))
        .route("/api/boards/{id}/ai/diagram", post(ai_diagram::generate))
        .route("/api/ai/status", get(ai_diagram::status))
        .route("/api/boards/{id}/issues", post(issues::lookup))
        .route("/api/rtc", get(rtc_config))
        .nest("/api/v1", rest::routes())
        .nest("/api", api::routes().merge(rest::token_routes()))
        .with_state(state);
    match static_dir {
        Some(dir) => app.fallback_service(ServeDir::new(dir)),
        #[cfg(feature = "embed")]
        None if embedded::available() => app.fallback(embedded::serve),
        None => app,
    }
}

/// バイナリに Web クライアントが埋め込まれているか。
pub fn has_embedded_client() -> bool {
    #[cfg(feature = "embed")]
    {
        embedded::available()
    }
    #[cfg(not(feature = "embed"))]
    {
        false
    }
}

/// 定期的に保存する。
pub fn spawn_flusher(hub: Arc<Hub>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(FLUSH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let hub = hub.clone();
            let _ = tokio::task::spawn_blocking(move || hub.flush()).await;
        }
    })
}

/// ルーム名は URL にそのまま載るので、英数字と `-` `_` だけにする。
pub fn valid_room_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 音声通話の設定(ICE サーバー)。
async fn rtc_config(State(state): State<Arc<AppState>>) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "iceServers": state.config.ice_servers }))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(room): Path<String>,
    Query(q): Query<api::KeyQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !valid_room_name(&room) {
        return (StatusCode::BAD_REQUEST, "invalid room name").into_response();
    }
    let user = match api::current_user(&state, &headers) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let role = match api::board_access(
        &state,
        &room,
        user.as_ref().map(|u| u.login.as_str()),
        q.key.as_deref(),
    ) {
        Ok((_, role)) => role,
        Err(e) => return e.into_response(),
    };
    let hub = state.hub.clone();
    ws.max_message_size(state.config.max_message_bytes)
        .on_upgrade(move |socket| session(socket, hub, room, role))
}

async fn session(socket: WebSocket, hub: Arc<Hub>, room_name: String, role: Role) {
    let member = hub.join(&room_name);
    let room = member.room.clone();
    let me = member.peer;
    tracing::info!(room = %room_name, peer = me, peers = room.peer_count(), ?role, "参加");

    // 取りこぼさないよう、状態を写す前に購読を始める(重複は yrs が吸収する)。
    let mut rx = room.tx.subscribe();
    let (mut sink, mut stream) = socket.split();

    let hello = Message::Sync {
        you: me,
        state: room.snapshot(),
    };
    if sink.send(binary(hello.encode())).await.is_err() {
        return;
    }
    for p in room.presences() {
        if sink.send(binary(p.to_vec())).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(Ok(msg)) = incoming else { break };
                let bytes = match msg {
                    WsMessage::Binary(b) => b,
                    WsMessage::Close(_) => break,
                    _ => continue,
                };
                match Message::decode(&bytes) {
                    Ok(Message::Update(_)) if !role.can_edit() => {
                        tracing::debug!(peer = me, "編集権限のない更新を捨てました");
                    }
                    Ok(Message::Update(u)) => match room.apply(&u) {
                        Ok(()) => room.broadcast(me, Arc::from(bytes.as_ref())),
                        Err(e) => tracing::warn!(peer = me, "不正な更新: {e}"),
                    },
                    Ok(Message::Presence(mut p)) => {
                        p.peer = me;
                        let encoded: Arc<[u8]> = Message::Presence(p).encode().into();
                        room.set_presence(me, encoded.clone());
                        room.broadcast(me, encoded);
                    }
                    Ok(Message::View(mut v)) if v.is_valid() => {
                        v.peer = me;
                        let encoded: Arc<[u8]> = Message::View(v).encode().into();
                        room.set_view(me, encoded.clone());
                        room.broadcast(me, encoded);
                    }
                    // 絵文字リアクションは保存せず、ほかの参加者に配るだけ。
                    Ok(Message::Reaction(mut r)) if r.is_valid() => {
                        r.peer = me;
                        room.broadcast(me, Message::Reaction(r).encode().into());
                    }
                    Ok(Message::Voice(mut v)) => {
                        v.peer = me;
                        let encoded: Arc<[u8]> = Message::Voice(v).encode().into();
                        room.set_voice(me, v.in_call, encoded.clone());
                        room.broadcast(me, encoded);
                    }
                    // 通話の接続手順は宛先の 1 人にだけ中継する(音声はサーバーを通らない)。
                    Ok(Message::Signal(mut s)) if s.to != me => {
                        s.from = me;
                        room.send_to(me, s.to, Message::Signal(s).encode().into());
                    }
                    Ok(other) => tracing::warn!(peer = me, "想定外のメッセージ: {other:?}"),
                    Err(e) => tracing::warn!(peer = me, "読めないメッセージ: {e}"),
                }
            }
            out = rx.recv() => match out {
                Ok(o) if o.is_kick() => break,
                Ok(o) if !o.is_for(me) => {}
                Ok(o) => {
                    if sink.send(binary(o.bytes.to_vec())).await.is_err() {
                        break;
                    }
                }
                // 配信が追いつかなかった。全体を送り直せば yrs 側で整合する。
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(peer = me, skipped = n, "配信が遅れたので全体を送り直します");
                    let full = Message::Update(room.snapshot()).encode();
                    if sink.send(binary(full)).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
        }
    }

    if room.remove_presence(me) {
        room.broadcast(me, Message::Leave(me).encode().into());
    }
    drop(member);
    tracing::info!(room = %room_name, peer = me, peers = room.peer_count(), "退出");
}

fn binary(bytes: Vec<u8>) -> WsMessage {
    WsMessage::Binary(bytes.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_names() {
        assert!(valid_room_name("abc-123_X"));
        assert!(!valid_room_name(""));
        assert!(!valid_room_name("a/b"));
        assert!(!valid_room_name("../etc"));
        assert!(!valid_room_name(&"a".repeat(65)));
    }

    #[test]
    fn ice_servers_config() {
        assert_eq!(ice_servers_from_env(None), default_ice_servers());
        assert_eq!(
            ice_servers_from_env(Some("{}".into())),
            default_ice_servers()
        );
        let turn = r#"[{"urls":"turn:example.com","username":"u","credential":"p"}]"#;
        assert_eq!(ice_servers_from_env(Some(turn.into()))[0]["username"], "u");
    }
}
