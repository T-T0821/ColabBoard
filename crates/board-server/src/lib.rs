//! グラボード の同期サーバー。
//!
//! - `GET /ws/{room}?key=`: WebSocket。メッセージ形式は [`board_core::protocol`]。
//! - `GET /p/{board}/{slug}`: Web に公開したボードのページと OGP 画像([`public`])。
//! - `/api/boards/{id}/media`: 動画メモの保存と配信([`media`])。
//! - `GET /api/rtc`: 音声通話の ICE サーバー。通話の接続手順は WebSocket で中継する。
//! - `/api/billing/...`: 料金プランと Stripe での支払い([`billing`])。
//! - `/api/v1/...`: 外部からボードを操作する REST API([`rest`])。API トークン([`tokens`])でも呼べる。
//! - `/api/errors`・`/api/admin/...`: エラー監視([`errors`])と定期バックアップ・復元([`backup`])。
//! - `/api/...`: アカウント・ボード一覧・共有設定([`api`])、AI による付箋の要約・クラスタリング([`ai`])・図の生成([`ai_diagram`])・手書きの整形([`ai_ink`])・議事録/マインドマップ/質問応答([`ai_board`])・会議の進行役([`ai_meeting`])、Slack 通知([`slack`])、メール・プッシュ通知([`notify`])、会議・カレンダーの紐づけ([`meeting_link`])、GitHub・Jira の課題カード([`issues`])、自作テンプレートのギャラリー([`templates`])。
//! - それ以外: 静的ファイル(ビルド済みの Web クライアント)。
//!
//! サーバーはルームごとに yrs ドキュメントを持ち、届いた更新を取り込んで同じルームの
//! 他の参加者へ配る。状態は定期的に redb へ保存する。
//! 接続時に権限([`boards::role_for`])を確かめ、編集権限のない人からの更新は捨てる。

pub mod ai;
pub mod ai_board;
pub mod ai_diagram;
pub mod ai_ink;
pub mod ai_meeting;
pub mod api;
pub mod audit;
pub mod auth;
pub mod backup;
pub mod billing;
pub mod boards;
pub mod comments;
pub mod e2e;
#[cfg(feature = "embed")]
mod embedded;
pub mod errors;
pub mod guard;
pub mod issues;
pub mod local_ai;
pub mod media;
pub mod meeting_link;
pub mod notify;
pub mod og;

pub mod oidc;
pub mod p2p;
pub mod plugins;
pub mod public;
pub mod rest;
pub mod room;
pub mod sealed;
pub mod search;
pub mod slack;
pub mod storage;
pub mod store;
pub mod teams;
pub mod templates;
pub mod tokens;
pub mod transfer;
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
    /// WASM プラグイン。
    pub plugins: plugins::PluginConfig,
    /// GitHub・Jira の課題カード。
    pub issues: issues::IssuesConfig,
    /// 音声通話の ICE サーバー(`RTCIceServer` の配列)。`GET /api/rtc` で配る。
    pub ice_servers: serde_json::Value,
    /// 会議の URL として受け付ける、Zoom・Meet・Teams 以外のドメイン(`MEETING_HOSTS`)。
    pub meeting_hosts: Vec<String>,
    /// `?embed=1` のページを iframe で埋め込んでよいサイト(CSP の `frame-ancestors` の値)。
    /// それ以外のページは同じサイトからしか埋め込めない。
    pub embed_origins: String,
    /// サーバー管理者のログイン名(全体の操作ログを見られる)。
    pub admins: Vec<String>,
    /// リバースプロキシの `X-Forwarded-For` を接続元として信じるか。
    pub trust_proxy: bool,
    /// OpenID Connect による SSO。`None` なら無効。
    pub oidc: Option<Arc<oidc::Oidc>>,
    /// パスワードでのログイン・アカウント作成を許すか(SSO だけにするなら false)。
    pub password_login: bool,
    /// 回数制限・利用上限・セキュリティヘッダー。
    pub limits: guard::Limits,
    /// ブラウザ内 AI(音声認識モデル)の取得元。
    pub local_ai: local_ai::LocalAiConfig,
    /// エンドツーエンド暗号化ボードを作れるか(既定値。サーバー管理者が画面から変えるとそちらが優先)。
    pub e2e_boards: bool,
    /// サーバーを通さない直接接続(P2P)を許すか。チームごとにも止められる。
    pub p2p: bool,
    /// エラー監視(集めたエラーの置き場)。
    pub monitor: Arc<errors::Monitor>,
    /// 定期バックアップ。`None` なら無効。
    pub backups: Option<Arc<backup::Backups>>,
    /// メール・プッシュ通知(既定はどちらも使わない)。
    pub notifier: Arc<notify::Notifier>,
    /// 料金プランと支払い(Stripe)。既定は使わない。
    pub billing: billing::BillingConfig,
}

impl Config {
    pub fn is_admin(&self, login: &str) -> bool {
        self.admins.iter().any(|a| a == login)
    }
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
            plugins: plugins::PluginConfig::default(),
            issues: issues::IssuesConfig::default(),
            ice_servers: default_ice_servers(),
            meeting_hosts: Vec::new(),
            embed_origins: "*".into(),
            admins: Vec::new(),
            trust_proxy: false,
            oidc: None,
            password_login: true,
            limits: guard::Limits::default(),
            local_ai: local_ai::LocalAiConfig::default(),
            e2e_boards: true,
            p2p: true,
            monitor: Arc::new(errors::Monitor::new()),
            backups: None,
            notifier: Arc::new(notify::Notifier::default()),
            billing: billing::BillingConfig::default(),
        }
    }
}

/// ハンドラーから使う共有の状態。
pub struct AppState {
    pub hub: Arc<Hub>,
    pub config: Config,
    /// 公開ページの OGP 画像のキャッシュ。
    pub og: public::OgCache,
    /// OGP 画像の文字に使うフォント(Web クライアントと同じもの)。
    pub font: Option<ab_glyph::FontArc>,
    pub guard: Arc<guard::Guard>,
    /// 暗号化ボードへの参加の記録を間引く。
    pub e2e_opens: e2e::OpenLog,
    /// テスト決済の申し込み(`BILLING=mock` のとき)。
    pub billing_mock: billing::MockSessions,
}

impl AppState {
    pub fn store(&self) -> &Arc<Store> {
        self.hub.store()
    }
}

pub fn router(hub: Arc<Hub>, mut config: Config) -> Router {
    let static_dir = config.static_dir.take();
    let models_dir = config.local_ai.models_dir.clone();
    let font = load_font(static_dir.as_deref());
    let guard = Arc::new(guard::Guard::new(config.limits.clone()));
    let state = Arc::new(AppState {
        hub,
        config,
        og: public::OgCache::default(),
        font,
        guard,
        e2e_opens: e2e::OpenLog::default(),
        billing_mock: billing::MockSessions::default(),
    });
    let app = Router::new()
        .route("/ws/{room}", get(ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/boards/{id}/ai/summarize", post(ai::summarize))
        .route("/api/boards/{id}/ai/cluster", post(ai::cluster))
        .route("/api/boards/{id}/ai/diagram", post(ai_diagram::generate))
        .route("/api/boards/{id}/ai/ink", post(ai_ink::recognize))
        .route("/api/boards/{id}/ai/minutes", post(ai_board::minutes))
        .route("/api/boards/{id}/ai/ask", post(ai_board::ask))
        .route("/api/boards/{id}/ai/mindmap", post(ai_board::mindmap))
        .route(
            "/api/boards/{id}/ai/facilitate",
            post(ai_meeting::facilitate),
        )
        .route("/api/ai/status", get(ai_diagram::status))
        .route("/api/boards/{id}/issues", post(issues::lookup))
        .route("/api/rtc", get(rtc_config))
        .route("/api/boards/{id}/p2p", get(p2p::get_policy))
        .route("/api/boards/{id}/p2p/events", post(p2p::post_events))
        .route("/api/boards/{id}/p2p/report", post(p2p::post_report))
        .route(
            "/api/teams/{tid}/p2p",
            axum::routing::put(p2p::set_team_p2p),
        )
        .route("/p/{id}/{slug}", get(public::page))
        .route("/p/{id}/{slug}/og.png", get(public::image))
        .route(
            "/api/boards/{id}/media",
            post(media::upload).layer(axum::extract::DefaultBodyLimit::max(media::MAX_MEDIA_BYTES)),
        )
        .route("/api/boards/{id}/media/{mid}", get(media::download))
        .nest("/api/v1", rest::routes().merge(plugins::routes()))
        .nest(
            "/api",
            api::routes()
                .merge(rest::token_routes())
                .merge(teams::routes())
                .merge(templates::routes())
                .merge(audit::routes())
                .merge(errors::routes())
                .merge(backup::routes())
                .merge(oidc::routes())
                .merge(e2e::routes())
                .merge(notify::routes())
                .merge(billing::routes())
                .merge(search::routes()),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            audit::client_ip,
        ))
        .with_state(state.clone());
    // ブラウザ内 AI のモデル(大きいので必要な人だけが取りにくる)。
    let app = match models_dir {
        Some(dir) => app.nest_service(local_ai::MODELS_PATH, ServeDir::new(dir)),
        None => app,
    };
    let app = match static_dir {
        // `.br` / `.gz` があれば、ブラウザが受け取れる形式をそのまま返す(`tools/precompress`)。
        Some(dir) => {
            app.fallback_service(ServeDir::new(dir).precompressed_br().precompressed_gzip())
        }
        #[cfg(feature = "embed")]
        None if embedded::available() => app.fallback(embedded::serve),
        None => app,
    };
    app.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        guard::rate_limit,
    ))
    .layer(axum::middleware::from_fn_with_state(
        state,
        guard::security_headers,
    ))
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

/// OGP 画像用のフォント。配信するファイル(埋め込みを含む)の中か、なければリポジトリの中から探す。
fn load_font(static_dir: Option<&std::path::Path>) -> Option<ab_glyph::FontArc> {
    const FILE: &str = "fonts/NotoSansJP-Regular.ttf";
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../board-web/assets");
    let candidates = static_dir
        .map(|d| d.join(FILE))
        .into_iter()
        .chain(std::iter::once(dev.join(FILE)));
    #[cfg(feature = "embed")]
    if static_dir.is_none()
        && let Some(f) = embedded::file(FILE).and_then(|b| ab_glyph::FontArc::try_from_vec(b).ok())
    {
        return Some(f);
    }
    for path in candidates {
        if let Ok(bytes) = std::fs::read(&path) {
            match ab_glyph::FontArc::try_from_vec(bytes) {
                Ok(f) => return Some(f),
                Err(e) => tracing::warn!("フォントを読めません({}): {e}", path.display()),
            }
        }
    }
    None
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
    guard::ClientIp(ip): guard::ClientIp,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !valid_room_name(&room) {
        return (StatusCode::BAD_REQUEST, "invalid room name").into_response();
    }
    // ほかのサイトのページからは、ログイン Cookie を使わせない(共有リンクの鍵や誰でも開けるボードは使える)。
    let user = if guard::same_origin(&headers, &state.config.limits.allowed_origins) {
        match api::current_user(&state, &headers) {
            Ok(u) => u,
            Err(e) => return e.into_response(),
        }
    } else {
        tracing::warn!(room = %room, "ほかのオリジンからの接続なので、ログインなしとして扱います");
        None
    };
    let role = match api::board_access(
        &state,
        &room,
        user.as_ref().map(|u| u.login.as_str()),
        q.key.as_deref(),
    ) {
        Ok((meta, role)) => {
            // 暗号化ボードは、誰がいつ参加したかだけを操作ログに残す(中身は残せない)。
            if let Some(meta) = meta.as_ref().filter(|m| m.e2e.is_some()) {
                e2e::log_open(
                    &state,
                    &headers,
                    meta,
                    user.as_ref().map(|u| u.login.as_str()),
                    role,
                );
            }
            role
        }
        Err(e) => return e.into_response(),
    };
    let limits = &state.config.limits;
    if limits.peers_per_room > 0
        && state
            .hub
            .get(&room)
            .is_some_and(|r| r.peer_count() >= limits.peers_per_room)
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "このボードは参加人数の上限に達しています",
        )
            .into_response();
    }
    let conn = match ip {
        Some(_) => match state.guard.connect(ip) {
            Some(c) => Some(c),
            None => {
                tracing::warn!(?ip, "同時接続が多すぎるので断りました");
                return (StatusCode::TOO_MANY_REQUESTS, "同時接続が多すぎます").into_response();
            }
        },
        None => None,
    };
    let hub = state.hub.clone();
    let rate = limits.messages_per_second;
    ws.max_message_size(state.config.max_message_bytes)
        .on_upgrade(move |socket| async move {
            session(socket, hub, room, role, rate).await;
            drop(conn);
        })
}

async fn session(socket: WebSocket, hub: Arc<Hub>, room_name: String, role: Role, rate: u32) {
    let mut bucket = guard::Bucket::new(rate);
    let member = hub.join(&room_name);
    let room = member.room.clone();
    let me = member.peer;
    tracing::info!(room = %room_name, peer = me, peers = room.peer_count(), ?role, "参加");

    // 取りこぼさないよう、状態を写す前に購読を始める(重複は yrs が吸収する)。
    let mut rx = room.tx.subscribe();
    let (mut sink, mut stream) = socket.split();

    // 暗号化ボードはログ全体(暗号文)を、そうでなければボード全体を送る。
    let hello = room.sealed_sync(me).unwrap_or_else(|| {
        Message::Sync {
            you: me,
            state: room.snapshot(),
        }
        .encode()
    });
    if sink.send(binary(hello)).await.is_err() {
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
                // 送りすぎる相手は切る(つなぎ直せば全体を同期し直すので、ずれは残らない)。
                if bucket.as_mut().is_some_and(|b| !b.take()) {
                    tracing::warn!(room = %room_name, peer = me, "メッセージが多すぎるので切断します");
                    break;
                }
                let bytes = match msg {
                    WsMessage::Binary(b) => b,
                    WsMessage::Close(_) => break,
                    _ => continue,
                };
                match Message::decode(&bytes) {
                    Ok(Message::Update(_) | Message::Sealed { .. } | Message::Compact { .. })
                        if !role.can_edit() =>
                    {
                        tracing::debug!(peer = me, "編集権限のない更新を捨てました");
                    }
                    // 暗号化ボード: 中身は読まずに、ログに足して配るだけ。
                    Ok(Message::Sealed { blob, .. }) => {
                        if let Err(e) = room.append_sealed(me, blob) {
                            tracing::warn!(peer = me, "暗号文を受け付けません: {e}");
                        }
                    }
                    Ok(Message::Compact { base, upto, blob }) => {
                        if let Err(e) = room.compact_sealed(base, upto, blob) {
                            tracing::warn!(peer = me, "ログをまとめられません: {e}");
                        }
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
                    let full = room
                        .sealed_sync(me)
                        .unwrap_or_else(|| Message::Update(room.snapshot()).encode());
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

    #[test]
    fn embed_query() {
        assert!(guard::is_embed_request(Some("room=a&key=b&embed=1")));
        assert!(!guard::is_embed_request(Some("room=a&embed=0")));
        assert!(!guard::is_embed_request(Some("room=a&notembed=1")));
        assert!(!guard::is_embed_request(None));
    }
}
