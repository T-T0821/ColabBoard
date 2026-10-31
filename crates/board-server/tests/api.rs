//! HTTP API と、権限による WebSocket の制限を確かめる。

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use board_core::api::{BoardInfo, Me, Role, Sharing};
use board_core::protocol::Message;
use board_core::{Board, Rgba8, Shape, Vec2};
use board_server::guard::Limits;
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
    tokio::spawn(async move {
        axum::serve(
            listener,
            served.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
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

#[tokio::test]
async fn teams_share_boards_with_members() {
    let s = start(Config::default()).await;
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let carol = s.signup("carol").await;

    let (status, team, _) = s
        .call(
            "POST",
            "/api/teams",
            Some(&alice),
            Some(json!({"name": "開発部"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let tid = team["id"].as_str().unwrap().to_string();
    assert_eq!(team["role"], "owner");

    // bob をメンバーにする。いない人は足せない。
    let path = format!("/api/teams/{tid}/members");
    let (status, team, _) = s
        .call(
            "PUT",
            &path,
            Some(&alice),
            Some(json!({"login": "@Bob", "role": "member"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{team}");
    assert_eq!(team["members"]["bob"], "member");
    let (status, _, _) = s
        .call(
            "PUT",
            &path,
            Some(&alice),
            Some(json!({"login": "nobody", "role": "member"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // メンバーは人を足せない。外の人はチームを見られない。
    let (status, _, _) = s
        .call(
            "PUT",
            &path,
            Some(&bob),
            Some(json!({"login": "carol", "role": "member"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = s
        .call("GET", &format!("/api/teams/{tid}"), Some(&carol), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // メンバーでないチームにはボードを作れない。
    let (status, _, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&carol),
            Some(json!({"title": "x", "team": tid})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // チームのボードを作ると、bob の一覧に出て編集できる。
    let (status, b, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "計画", "team": tid})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let b: BoardInfo = serde_json::from_value(b).unwrap();
    assert_eq!(b.team.as_deref(), Some(tid.as_str()));
    let (_, list, _) = s.call("GET", "/api/boards", Some(&bob), None).await;
    let list: Vec<BoardInfo> = serde_json::from_value(list).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].role, Role::Editor);
    assert!(s.connect(&b.id, Some(&bob), None).await.is_some());
    assert!(s.connect(&b.id, Some(&carol), None).await.is_none());

    // チームの権限を閲覧のみにすると、bob の更新は捨てられる。
    let (status, _, _) = s
        .call(
            "PUT",
            &format!("/api/boards/{}/team", b.id),
            Some(&alice),
            Some(json!({"team": tid, "access": "viewer"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, info, _) = s
        .call("GET", &format!("/api/boards/{}", b.id), Some(&bob), None)
        .await;
    assert_eq!(info["role"], "viewer");

    // bob を管理者にすると、チームのボードのオーナー権限を持つ。
    let (status, _, _) = s
        .call(
            "PUT",
            &path,
            Some(&alice),
            Some(json!({"login": "bob", "role": "admin"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = s
        .call(
            "GET",
            &format!("/api/boards/{}/sharing", b.id),
            Some(&bob),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // bob が抜けると開けなくなる。
    let (status, _, _) = s
        .call(
            "PUT",
            &path,
            Some(&bob),
            Some(json!({"login": "bob", "role": null})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = s
        .call("GET", &format!("/api/boards/{}", b.id), Some(&bob), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, teams, _) = s.call("GET", "/api/teams", Some(&bob), None).await;
    assert_eq!(teams.as_array().unwrap().len(), 0);

    // チームを消すとボードは alice の個人ボードに戻る。
    let (status, _, _) = s
        .call("DELETE", &format!("/api/teams/{tid}"), Some(&alice), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, info, _) = s
        .call("GET", &format!("/api/boards/{}", b.id), Some(&alice), None)
        .await;
    assert_eq!(info["role"], "owner");
    assert!(info.get("team").is_none());
}

#[tokio::test]
async fn audit_log_records_and_exports() {
    let s = start(Config {
        admins: vec!["root".into()],
        trust_proxy: true,
        ..Config::default()
    })
    .await;
    let root = s.signup("root").await;
    let alice = s.signup("alice").await;
    let _bob = s.signup("bob").await;

    // ログイン失敗も残る。接続元はプロキシのヘッダーから取る。
    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        // 左側は利用者が偽れるので、直前のプロキシが付けた最後の値を使う。
        .header("x-forwarded-for", "6.6.6.6, 203.0.113.7")
        .header("x-graboard-client-ip", "6.6.6.6");
    let res = s
        .app
        .clone()
        .oneshot(
            req.body(Body::from(
                json!({"login": "alice", "password": "wrong-password"}).to_string(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let (_, team, _) = s
        .call(
            "POST",
            "/api/teams",
            Some(&alice),
            Some(json!({"name": "営業"})),
        )
        .await;
    let tid = team["id"].as_str().unwrap().to_string();
    s.call(
        "PUT",
        &format!("/api/teams/{tid}/members"),
        Some(&alice),
        Some(json!({"login": "bob", "role": "member"})),
    )
    .await;
    s.call(
        "POST",
        "/api/boards",
        Some(&alice),
        Some(json!({"title": "見込み客", "team": tid})),
    )
    .await;

    // チームのログはチームの管理者だけ。
    let path = format!("/api/audit?team={tid}");
    let (status, events, _) = s.call("GET", &path, Some(&alice), None).await;
    assert_eq!(status, StatusCode::OK);
    let actions: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, vec!["board.create", "team.member", "team.create"]);
    assert_eq!(events[1]["target"], "bob");
    let (status, _, _) = s.call("GET", &path, Some(&root), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 全体のログはサーバー管理者だけ。
    let (status, _, _) = s.call("GET", "/api/audit", Some(&alice), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, events, _) = s
        .call("GET", "/api/audit?action=auth", Some(&root), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let failed = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "auth.login_failed")
        .unwrap();
    assert_eq!(failed["target"], "alice");
    assert_eq!(failed["ip"], "203.0.113.7");
    let (_, me, _) = s.call("GET", "/api/me", Some(&root), None).await;
    assert_eq!(me["admin"], true);

    // 書き出し。
    let req = Request::builder()
        .uri(format!("/api/audit/export?team={tid}&format=csv"))
        .header(header::COOKIE, &alice)
        .body(Body::empty())
        .unwrap();
    let res = s.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains(".csv")
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(text.lines().count(), 4);
    assert!(text.contains("チーム作成"));
}

#[tokio::test]
async fn client_ip_header_cannot_be_spoofed() {
    let s = start(Config {
        admins: vec!["eve".into()],
        ..Config::default()
    })
    .await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/signup")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-graboard-client-ip", "6.6.6.6")
        .header("x-forwarded-for", "7.7.7.7")
        .body(Body::from(
            json!({"login": "eve", "name": "", "password": "password123"}).to_string(),
        ))
        .unwrap();
    let res = s.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    // 外から付けたヘッダーは消え、プロキシを信じない設定なので X-Forwarded-For も使わない。
    let (status, events, _) = s.call("GET", "/api/audit", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(events[0]["action"], "auth.signup");
    assert!(events[0].get("ip").is_none(), "{events}");
}

/// 生の応答(HTML や PNG を見るため)。
async fn raw(server: &Server, path: &str) -> (StatusCode, String, Vec<u8>) {
    let req = Request::builder()
        .uri(path)
        .header(header::HOST, "board.example")
        .header("x-forwarded-proto", "https")
        .body(Body::empty())
        .unwrap();
    let res = server.app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, ct, bytes)
}

#[tokio::test]
async fn public_pages_with_ogp() {
    let server = start(Config::default()).await;
    let alice = server.signup("alice").await;
    let bob = server.signup("bob").await;
    let (_, b, _) = server
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "<企画> 会議"})),
        )
        .await;
    let id = b["id"].as_str().unwrap().to_string();

    // オーナー以外は公開できない。
    let (status, _, _) = server
        .call(
            "PUT",
            &format!("/api/boards/{id}/public"),
            Some(&bob),
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, sharing, _) = server
        .call(
            "PUT",
            &format!("/api/boards/{id}/public"),
            Some(&alice),
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{sharing}");
    let sharing: Sharing = serde_json::from_value(sharing).unwrap();
    let slug = sharing.public.unwrap().slug;

    // 公開ページ: OGP と、閲覧用のボードへの移動。
    let (status, ct, body) = raw(&server, &format!("/p/{id}/{slug}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.starts_with("text/html"));
    let html = String::from_utf8(body).unwrap();
    assert!(
        html.contains(r#"<meta property="og:title" content="&lt;企画&gt; 会議">"#),
        "{html}"
    );
    assert!(html.contains(&format!(
        r#"<meta property="og:image" content="https://board.example/p/{id}/{slug}/og.png"#
    )));
    assert!(html.contains("summary_large_image"));
    assert!(html.contains(&format!("/?room={id}&key={slug}&public")));

    let (status, ct, png) = raw(&server, &format!("/p/{id}/{slug}/og.png")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct, "image/png");
    assert_eq!(&png[1..4], b"PNG");

    // 公開の鍵で、ログインなしで見られる(見るだけ)。
    let (status, info, _) = server
        .call("GET", &format!("/api/boards/{id}?key={slug}"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let info: BoardInfo = serde_json::from_value(info).unwrap();
    assert_eq!(info.role, Role::Viewer);
    assert!(server.connect(&id, None, Some(&slug)).await.is_some());

    // 鍵が違えば 404。
    assert_eq!(
        raw(&server, &format!("/p/{id}/wrong")).await.0,
        StatusCode::NOT_FOUND
    );

    // 公開をやめると見られなくなる。
    let (status, _, _) = server
        .call(
            "PUT",
            &format!("/api/boards/{id}/public"),
            Some(&alice),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        raw(&server, &format!("/p/{id}/{slug}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        raw(&server, &format!("/p/{id}/{slug}/og.png")).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(server.connect(&id, None, Some(&slug)).await.is_none());
}

#[tokio::test]
async fn login_attempts_are_limited() {
    let s = start(Config {
        limits: Limits {
            login_failures_per_account: 3,
            ..Limits::default()
        },
        ..Config::default()
    })
    .await;
    s.signup("alice").await;
    let try_login = |password: &'static str| {
        s.call(
            "POST",
            "/api/login",
            None,
            Some(json!({"login": "alice", "password": password})),
        )
    };
    for _ in 0..3 {
        assert_eq!(try_login("wrong-pass").await.0, StatusCode::BAD_REQUEST);
    }
    // 上限に達したら、正しいパスワードでもしばらく断る。
    let (status, body, _) = try_login("password123").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(body["error"].as_str().unwrap().contains("待って"), "{body}");
    // ほかのアカウントには影響しない。
    let bob = s.signup("bob").await;
    assert!(!bob.is_empty());
}

#[tokio::test]
async fn accounts_and_boards_have_limits() {
    let s = start(Config {
        limits: Limits {
            max_users: 1,
            max_boards_per_user: 2,
            ..Limits::default()
        },
        ..Config::default()
    })
    .await;
    let alice = s.signup("alice").await;
    let (status, body, _) = s
        .call(
            "POST",
            "/api/signup",
            None,
            Some(json!({"login": "bob", "name": "", "password": "password123"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let new_board = || {
        s.call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "b"})),
        )
    };
    let (status, first, _) = new_board().await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(new_board().await.0, StatusCode::CREATED);
    assert_eq!(new_board().await.0, StatusCode::FORBIDDEN);
    // 消せばまた作れる。
    let id = first["id"].as_str().unwrap();
    let (status, _, _) = s
        .call("DELETE", &format!("/api/boards/{id}"), Some(&alice), None)
        .await;
    assert!(status.is_success());
    assert_eq!(new_board().await.0, StatusCode::CREATED);
}

#[tokio::test]
async fn api_requests_are_rate_limited() {
    let s = start(Config {
        limits: Limits {
            requests_per_ip_per_minute: 5,
            ..Limits::default()
        },
        ..Config::default()
    })
    .await;
    for _ in 0..5 {
        assert_eq!(
            s.call("GET", "/api/me", None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        s.call("GET", "/api/me", None, None).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // 死活監視は数えない。
    assert_eq!(
        s.call("GET", "/healthz", None, None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn connections_per_board_and_ip_are_limited() {
    let s = start(Config {
        limits: Limits {
            peers_per_room: 2,
            connections_per_ip: 3,
            ..Limits::default()
        },
        ..Config::default()
    })
    .await;
    let _a = s.connect("room-a", None, None).await.unwrap();
    let _b = s.connect("room-a", None, None).await.unwrap();
    assert!(
        s.connect("room-a", None, None).await.is_none(),
        "人数の上限"
    );
    let c = s.connect("room-b", None, None).await.unwrap();
    assert!(
        s.connect("room-c", None, None).await.is_none(),
        "IP ごとの上限"
    );
    drop(c);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(s.connect("room-c", None, None).await.is_some());
}

#[tokio::test]
async fn flooding_websocket_is_disconnected() {
    let s = start(Config {
        limits: Limits {
            messages_per_second: 10,
            ..Limits::default()
        },
        ..Config::default()
    })
    .await;
    let (mut ws, mut board) = s.connect("flood", None, None).await.unwrap();
    for _ in 0..200 {
        for u in add_sticky(&mut board) {
            if ws
                .send(WsMessage::Binary(Message::Update(u).encode().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    }
    // サーバーが切るまで読む。
    let closed = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(_)) = ws.next().await {}
    })
    .await;
    assert!(closed.is_ok(), "切断されるはず");
}

#[tokio::test]
async fn cross_origin_websocket_does_not_use_login() {
    let s = start(Config::default()).await;
    let alice = s.signup("alice").await;
    let (_, info, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "secret"})),
        )
        .await;
    let id = info["id"].as_str().unwrap().to_string();
    let open = |origin: &'static str| {
        let mut req = format!("{}/{id}", s.ws).into_client_request().unwrap();
        req.headers_mut()
            .insert(header::COOKIE, alice.parse().unwrap());
        req.headers_mut()
            .insert(header::ORIGIN, origin.parse().unwrap());
        connect_async(req)
    };
    assert!(open("https://evil.example").await.is_err());
    let host =
        s.ws.trim_start_matches("ws://")
            .trim_end_matches("/ws")
            .to_string();
    let origin: &'static str = Box::leak(format!("http://{host}").into_boxed_str());
    assert!(open(origin).await.is_ok());
}

#[tokio::test]
async fn security_headers_and_csp() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("index.html"),
        "<html><script type=\"module\">boot()</script><script src=\"/a.js\"></script></html>",
    )
    .unwrap();
    let s = start(Config {
        static_dir: Some(dir.path().to_path_buf()),
        limits: Limits {
            hsts: true,
            ..Limits::default()
        },
        ..Config::default()
    })
    .await;
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let res = s.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let h = res.headers();
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "SAMEORIGIN");
    assert_eq!(h["referrer-policy"], "same-origin");
    assert!(h.contains_key("strict-transport-security"));
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(
        csp.contains("'wasm-unsafe-eval'") && csp.contains("'sha256-"),
        "{csp}"
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.starts_with(b"<html>"));

    // API にもヘッダーは付く(CSP は HTML だけ)。
    let req = Request::builder()
        .uri("/api/me")
        .body(Body::empty())
        .unwrap();
    let res = s.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    assert!(!res.headers().contains_key("content-security-policy"));
}
