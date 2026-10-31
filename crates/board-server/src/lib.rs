//! ColabBoard の同期サーバー。
//!
//! - `GET /ws/{room}`: WebSocket。メッセージ形式は [`board_core::protocol`]。
//! - それ以外: 静的ファイル(ビルド済みの Web クライアント)。
//!
//! サーバーはルームごとに yrs ドキュメントを持ち、届いた更新を取り込んで同じルームの
//! 他の参加者へ配る。状態は定期的に redb へ保存する。

pub mod room;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use board_core::protocol::Message;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;
use tower_http::services::ServeDir;

pub use room::Hub;
pub use store::Store;

/// 1 通の上限(大きなボードの初回同期を考えて余裕を持たせる)。
const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// 保存の間隔。
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(2);

pub struct Config {
    pub static_dir: Option<PathBuf>,
}

pub fn router(hub: Arc<Hub>, config: Config) -> Router {
    let app = Router::new()
        .route("/ws/{room}", get(ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(hub);
    match config.static_dir {
        Some(dir) => app.fallback_service(ServeDir::new(dir)),
        None => app,
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

async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(room): Path<String>,
    State(hub): State<Arc<Hub>>,
) -> Response {
    if !valid_room_name(&room) {
        return (StatusCode::BAD_REQUEST, "invalid room name").into_response();
    }
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| session(socket, hub, room))
}

async fn session(socket: WebSocket, hub: Arc<Hub>, room_name: String) {
    let member = hub.join(&room_name);
    let room = member.room.clone();
    let me = member.peer;
    tracing::info!(room = %room_name, peer = me, peers = room.peer_count(), "参加");

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
                    Ok(other) => tracing::warn!(peer = me, "想定外のメッセージ: {other:?}"),
                    Err(e) => tracing::warn!(peer = me, "読めないメッセージ: {e}"),
                }
            }
            out = rx.recv() => match out {
                Ok(o) if o.from == me => {}
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
}
