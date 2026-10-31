//! HTTP API と、権限による WebSocket の制限を確かめる。

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use board_core::api::{BoardInfo, Me, Role, Sharing};
use board_core::protocol::Message;
use board_core::{Board, Rgba8, Shape, Vec2};
use board_server::{Config, Hub, Store, router};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

struct Server {
    app: Router,
    ws: String,
}

async fn start(config: Config) -> Server {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let app = router(hub, config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
    Server {
        app,
        ws: format!("ws://{addr}/ws"),
    }
}

impl Server {
    async fn call(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value, Option<String>) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        let req = match body {
            Some(b) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let cookie = res
            .headers()
            .get(header::SET_COOKIE)
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json, cookie)
    }

    async fn signup(&self, login: &str) -> String {
        let (status, body, cookie) = self
            .call(
                "POST",
                "/api/signup",
                None,
                Some(json!({"login": login, "name": login.to_uppercase(), "password": "password123"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        cookie.unwrap()
    }

    /// WebSocket でつなぐ。断られたら `None`。
    async fn connect(
        &self,
        room: &str,
        cookie: Option<&str>,
        key: Option<&str>,
    ) -> Option<(
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Board,
    )> {
        let url = match key {
            Some(k) => format!("{}/{room}?key={k}", self.ws),
            None => format!("{}/{room}", self.ws),
        };
        let mut req = url.into_client_request().unwrap();
        if let Some(c) = cookie {
            req.headers_mut().insert(header::COOKIE, c.parse().unwrap());
        }
        let (mut ws, _) = connect_async(req).await.ok()?;
        let msg = recv(&mut ws).await?;
        let Message::Sync { state, .. } = msg else {
            panic!("最初は Sync のはず")
        };
        let mut board = Board::new();
        board.apply_remote(&state).unwrap();
        Some((ws, board))
    }
}

async fn recv<S>(ws: &mut S) -> Option<Message>
where
    S: StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let msg = tokio::time::timeout(Duration::from_millis(500), ws.next())
            .await
            .ok()??
            .ok()?;
        match msg {
            WsMessage::Binary(b) => return Some(Message::decode(&b).unwrap()),
            WsMessage::Close(_) => return None,
            _ => {}
        }
    }
}

fn add_sticky(board: &mut Board) -> Vec<Vec<u8>> {
    let id = board.alloc_id();
    board.push(Shape::sticky(id, Vec2::ZERO, Rgba8::WHITE));
    board.take_outgoing()
}

#[tokio::test]
async fn accounts_and_sessions() {
    let s = start(Config::default()).await;
    let (status, _, _) = s.call("GET", "/api/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let alice = s.signup("alice").await;
    let (status, me, _) = s.call("GET", "/api/me", Some(&alice), None).await;
    assert_eq!(status, StatusCode::OK);
    let me: Me = serde_json::from_value(me).unwrap();
    assert_eq!((me.login.as_str(), me.name.as_str()), ("alice", "ALICE"));

    // 同じ名前では作れない。
    let (status, _, _) = s
        .call(
            "POST",
            "/api/signup",
            None,
            Some(json!({"login": "alice", "name": "", "password": "password123"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // パスワード違い → ログインできない。正しければ新しいセッション。
    let (status, _, _) = s
        .call(
            "POST",
            "/api/login",
            None,
            Some(json!({"login": "alice", "password": "nope-nope"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, second) = s
        .call(
            "POST",
            "/api/login",
            None,
            Some(json!({"login": "alice", "password": "password123"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let second = second.unwrap();

    // ログアウトしたセッションは使えない。もう一方は使える。
    let (status, _, _) = s.call("POST", "/api/logout", Some(&alice), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = s.call("GET", "/api/me", Some(&alice), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = s.call("GET", "/api/me", Some(&second), None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn boards_permissions_and_links() {
    let s = start(Config::default()).await;
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;

    let (status, b, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "企画会議"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let board: BoardInfo = serde_json::from_value(b).unwrap();
    assert_eq!(board.role, Role::Owner);
    let id = board.id.clone();
    let path = format!("/api/boards/{id}");

    // 本人の一覧に載る。bob と未ログインの人は開けない。
    let (_, list, _) = s.call("GET", "/api/boards", Some(&alice), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(
        s.call("GET", &path, Some(&bob), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.call("GET", &path, None, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(s.connect(&id, Some(&bob), None).await.is_none());
    assert!(s.connect(&id, None, None).await.is_none());

    // オーナーは編集できる。
    let (mut ws_a, mut board_a) = s.connect(&id, Some(&alice), None).await.unwrap();
    for u in add_sticky(&mut board_a) {
        ws_a.send(WsMessage::Binary(Message::Update(u).encode().into()))
            .await
            .unwrap();
    }

    // 閲覧リンクを作る。bob はリンクで見られるが、編集は反映されない。
    let (status, sh, _) = s
        .call(
            "PUT",
            &format!("{path}/link"),
            Some(&alice),
            Some(json!({"role": "viewer"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let sh: Sharing = serde_json::from_value(sh).unwrap();
    let key = sh.link.unwrap().key;
    // 共有設定を変えたので、つないでいた人はつなぎ直しになる。
    assert!(recv(&mut ws_a).await.is_none());

    // bob は共有設定を変えられない。
    let (status, _, _) = s
        .call(
            "PUT",
            &format!("{path}/link"),
            Some(&bob),
            Some(json!({"role": "editor"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, info, _) = s
        .call("GET", &format!("{path}?key={key}"), Some(&bob), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["role"], "viewer");
    let (_, list, _) = s.call("GET", "/api/boards", Some(&bob), None).await;
    assert_eq!(list[0]["key"], key.as_str());

    let (mut ws_a, board_a) = s.connect(&id, Some(&alice), None).await.unwrap();
    let before = board_a.len();
    let (mut ws_b, mut board_b) = s.connect(&id, Some(&bob), Some(&key)).await.unwrap();
    assert_eq!(board_b.len(), before);
    for u in add_sticky(&mut board_b) {
        ws_b.send(WsMessage::Binary(Message::Update(u).encode().into()))
            .await
            .unwrap();
    }
    // 閲覧のみの人の更新は誰にも届かず、サーバーにも残らない。
    assert!(recv(&mut ws_a).await.is_none());
    let (_, fresh) = s.connect(&id, Some(&alice), None).await.unwrap();
    assert_eq!(fresh.len(), before);

    // 名前の変更は編集者以上。
    let (status, _, _) = s
        .call(
            "PATCH",
            &format!("{path}?key={key}"),
            Some(&bob),
            Some(json!({"title": "x"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // bob を編集者として招待すると、鍵なしで編集できる。
    let (status, _, _) = s
        .call(
            "PUT",
            &format!("{path}/members"),
            Some(&alice),
            Some(json!({"login": "@Bob", "role": "editor"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = s
        .call(
            "PUT",
            &format!("{path}/members"),
            Some(&alice),
            Some(json!({"login": "nobody", "role": "editor"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, info, _) = s
        .call(
            "PATCH",
            &path,
            Some(&bob),
            Some(json!({"title": "新しい名前"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!(info["title"], "新しい名前");
    let (mut ws_b, mut board_b) = s.connect(&id, Some(&bob), None).await.unwrap();
    for u in add_sticky(&mut board_b) {
        ws_b.send(WsMessage::Binary(Message::Update(u).encode().into()))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, fresh) = s.connect(&id, Some(&alice), None).await.unwrap();
    assert_eq!(fresh.len(), before + 1);

    // bob は削除できない。alice が消すと、誰も開けなくなる(誰でも編集できるボードとしても復活しない)。
    assert_eq!(
        s.call("DELETE", &path, Some(&bob), None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.call("DELETE", &path, Some(&alice), None).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(recv(&mut ws_b).await.is_none());
    assert_eq!(
        s.call("GET", &path, Some(&alice), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(s.connect(&id, None, None).await.is_none());
    let (_, list, _) = s.call("GET", "/api/boards", Some(&bob), None).await;
    assert!(list.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn anonymous_boards_can_be_disabled() {
    let s = start(Config::default()).await;
    assert!(s.connect("free-room", None, None).await.is_some());
    let (status, info, _) = s.call("GET", "/api/boards/free-room", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["role"], "editor");

    let s = start(Config {
        anonymous_boards: false,
        ..Config::default()
    })
    .await;
    assert!(s.connect("free-room", None, None).await.is_none());
}

#[tokio::test]
async fn comments_permissions_and_live_refresh() {
    let s = start(Config::default()).await;
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let (_, b, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "レビュー"})),
        )
        .await;
    let id = b["id"].as_str().unwrap().to_string();
    let path = format!("/api/boards/{id}");
    s.call(
        "PUT",
        &format!("{path}/members"),
        Some(&alice),
        Some(json!({"login": "bob", "role": "viewer"})),
    )
    .await;

    // 閲覧のみの bob はコメントできない。未ログインでも書けない。
    let body = json!({"body": "@alice ここ確認して", "x": 10.0, "y": 20.0});
    let (status, _, _) = s
        .call(
            "POST",
            &format!("{path}/comments"),
            Some(&bob),
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = s
        .call(
            "POST",
            &format!("{path}/comments"),
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // コメント可にすると書ける。つないでいる人には Refresh が届く。
    s.call(
        "PUT",
        &format!("{path}/members"),
        Some(&alice),
        Some(json!({"login": "bob", "role": "commenter"})),
    )
    .await;
    let (mut ws_a, _) = s.connect(&id, Some(&alice), None).await.unwrap();
    let (status, posted, _) = s
        .call("POST", &format!("{path}/comments"), Some(&bob), Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{posted}");
    assert_eq!(posted["comment"]["mentions"], json!(["alice"]));
    assert_eq!(
        recv(&mut ws_a).await,
        Some(Message::Refresh(board_core::protocol::TOPIC_COMMENTS))
    );

    let (_, list, _) = s
        .call("GET", &format!("{path}/comments"), Some(&alice), None)
        .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, notes, _) = s
        .call("GET", "/api/notifications", Some(&alice), None)
        .await;
    assert_eq!(notes[0]["kind"], "mention");
    assert_eq!(notes[0]["board_title"], "レビュー");
    let (status, _, _) = s
        .call(
            "POST",
            "/api/notifications/read",
            Some(&alice),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, notes, _) = s
        .call("GET", "/api/notifications", Some(&alice), None)
        .await;
    assert_eq!(notes[0]["read"], true);

    // 開けない人には一覧も見せない。
    let carol = s.signup("carol").await;
    let (status, _, _) = s
        .call("GET", &format!("{path}/comments"), Some(&carol), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn versions_save_preview_and_restore() {
    let s = start(Config::default()).await;
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let (_, b, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "履歴"})),
        )
        .await;
    let id = b["id"].as_str().unwrap().to_string();
    let path = format!("/api/boards/{id}");
    s.call(
        "PUT",
        &format!("{path}/members"),
        Some(&alice),
        Some(json!({"login": "bob", "role": "viewer"})),
    )
    .await;

    let (mut ws, mut board) = s.connect(&id, Some(&alice), None).await.unwrap();
    let before = board.len();
    let (status, v1, _) = s
        .call(
            "POST",
            &format!("{path}/versions"),
            Some(&alice),
            Some(json!({"label": "初稿"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v1}");
    assert_eq!(v1["elements"], before);
    // 閲覧のみの人は保存も復元もできないが、一覧と中身は見られる。
    let (status, _, _) = s
        .call(
            "POST",
            &format!("{path}/versions"),
            Some(&bob),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 付箋を 2 枚足す。
    for _ in 0..2 {
        for u in add_sticky(&mut board) {
            ws.send(WsMessage::Binary(Message::Update(u).encode().into()))
                .await
                .unwrap();
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let vid = v1["id"].as_u64().unwrap();
    let (status, _, _) = s
        .call(
            "POST",
            &format!("{path}/versions/{vid}/restore"),
            Some(&bob),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, backup, _) = s
        .call(
            "POST",
            &format!("{path}/versions/{vid}/restore"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{backup}");
    assert_eq!(backup["label"], "「初稿」に戻す前");
    assert_eq!(backup["elements"], before + 2);

    // つないでいる人に戻した更新が届く。
    let mut got_update = false;
    while let Some(m) = recv(&mut ws).await {
        if let Message::Update(u) = m {
            board.apply_remote(&u).unwrap();
            got_update = true;
        }
    }
    assert!(got_update);
    assert_eq!(board.len(), before);
    let (_, fresh) = s.connect(&id, Some(&bob), None).await.unwrap();
    assert_eq!(fresh.len(), before);

    let (_, list, _) = s
        .call("GET", &format!("{path}/versions"), Some(&bob), None)
        .await;
    let list = list.as_array().unwrap();
    assert!(list.len() >= 2);
    assert_eq!(list[0]["label"], "「初稿」に戻す前", "新しい順");
}
