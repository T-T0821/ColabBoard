//! 外部向けの REST API(`/api/v1`)と API トークンを確かめる。

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use board_core::Board;
use board_core::protocol::Message;
use board_server::{Config, Hub, Store, router};
use futures_util::StreamExt;
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

enum Auth<'a> {
    None,
    Cookie(&'a str),
    Bearer(&'a str),
}

async fn start() -> Server {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let app = router(hub, Config::default());
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
        auth: Auth<'_>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(path);
        match auth {
            Auth::None => {}
            Auth::Cookie(c) => req = req.header(header::COOKIE, c),
            Auth::Bearer(t) => req = req.header(header::AUTHORIZATION, format!("Bearer {t}")),
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
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn signup(&self, login: &str) -> String {
        let req = Request::builder()
            .method("POST")
            .uri("/api/signup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"login": login, "name": login, "password": "password123"}).to_string(),
            ))
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        res.headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    async fn token(&self, cookie: &str, body: Value) -> String {
        let (status, v) = self
            .call("POST", "/api/tokens", Auth::Cookie(cookie), Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_string()
    }
}

#[tokio::test]
async fn tokens_are_managed_with_the_session_only() {
    let s = start().await;
    let alice = s.signup("alice").await;
    let (status, _) = s.call("GET", "/api/tokens", Auth::None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let t = s
        .token(&alice, json!({"name": "CI", "scope": "write"}))
        .await;
    let (status, list) = s
        .call("GET", "/api/tokens", Auth::Cookie(&alice), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], "CI");
    assert!(list[0].get("token").is_none() && list[0].get("hash").is_none());

    // トークンでトークンは作れない。
    let (status, _) = s
        .call(
            "POST",
            "/api/tokens",
            Auth::Bearer(&t),
            Some(json!({"name": "x", "scope": "write"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, me) = s.call("GET", "/api/v1/me", Auth::Bearer(&t), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["login"], "alice");
    assert_eq!(me["token"]["scope"], "write");

    // 無効にしたら使えない。
    let id = list[0]["id"].as_str().unwrap();
    let (status, _) = s
        .call(
            "DELETE",
            &format!("/api/tokens/{id}"),
            Auth::Cookie(&alice),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = s.call("GET", "/api/v1/me", Auth::Bearer(&t), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = s
        .call("GET", "/api/v1/me", Auth::Bearer("gbt_nope_x"), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn elements_crud_reaches_connected_clients() {
    let s = start().await;
    let alice = s.signup("alice").await;
    let t = s
        .token(&alice, json!({"name": "bot", "scope": "write"}))
        .await;

    let (status, board) = s
        .call(
            "POST",
            "/api/v1/boards",
            Auth::Bearer(&t),
            Some(json!({"title": "API"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{board}");
    assert_eq!(board["role"], "editor");
    let id = board["id"].as_str().unwrap().to_string();

    // ブラウザでつないでいる人がいる。
    let mut req = format!("{}/{id}", s.ws).into_client_request().unwrap();
    req.headers_mut()
        .insert(header::COOKIE, alice.parse().unwrap());
    let (mut ws, _) = connect_async(req).await.unwrap();
    let Some(Ok(WsMessage::Binary(b))) = ws.next().await else {
        panic!()
    };
    let Message::Sync { state, .. } = Message::decode(&b).unwrap() else {
        panic!()
    };
    let mut client = Board::new();
    client.apply_remote(&state).unwrap();
    let before = client.len();

    let (status, e) = s
        .call(
            "POST",
            &format!("/api/v1/boards/{id}/elements"),
            Auth::Bearer(&t),
            Some(json!({"type": "shape", "kind": "sticky", "x": 10, "y": 20, "text": "API から"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{e}");
    assert_eq!(e["text"], "API から");
    let eid = e["id"].as_str().unwrap().to_string();

    // 更新が WebSocket で届く。
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let WsMessage::Binary(b) = msg
            && let Message::Update(u) = Message::decode(&b).unwrap()
        {
            client.apply_remote(&u).unwrap();
            break;
        }
    }
    assert_eq!(client.len(), before + 1);

    let path = format!("/api/v1/boards/{id}/elements/{eid}");
    let (status, e) = s
        .call(
            "PATCH",
            &path,
            Auth::Bearer(&t),
            Some(json!({"x": 300, "fill": "#b5e3ff"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{e}");
    assert_eq!(e["x"], 300.0);
    assert_eq!(e["fill"], "#b5e3ff");

    let (status, list) = s
        .call(
            "GET",
            &format!("/api/v1/boards/{id}/elements?kind=sticky&text=api"),
            Auth::Bearer(&t),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);

    // まとめて作り、作ったものどうしをつなぐ。
    let (status, r) = s
        .call(
            "POST",
            &format!("/api/v1/boards/{id}/ops"),
            Auth::Bearer(&t),
            Some(json!({"ops": [
                {"op": "create", "element": {"type": "shape", "kind": "rect", "x": 0, "y": 400, "text": "A"}},
                {"op": "create", "element": {"type": "shape", "kind": "rect", "x": 400, "y": 400, "text": "B"}},
                {"op": "create", "element": {"type": "connector", "from": {"element": "$0"}, "to": {"element": "$1"}}},
                {"op": "delete", "id": eid},
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["created"].as_array().unwrap().len(), 3);
    assert_eq!(r["deleted"][0], eid.as_str());

    // 途中で失敗したら何も変わらない。
    let (_, all_before) = s
        .call(
            "GET",
            &format!("/api/v1/boards/{id}/elements"),
            Auth::Bearer(&t),
            None,
        )
        .await;
    let (status, err) = s
        .call(
            "POST",
            &format!("/api/v1/boards/{id}/ops"),
            Auth::Bearer(&t),
            Some(json!({"ops": [
                {"op": "create", "element": {"type": "shape"}},
                {"op": "create", "element": {"type": "shape", "kind": "unknown"}},
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("1 番目"), "{err}");
    let (_, all_after) = s
        .call(
            "GET",
            &format!("/api/v1/boards/{id}/elements"),
            Auth::Bearer(&t),
            None,
        )
        .await;
    assert_eq!(all_before, all_after);

    let (status, _) = s.call("DELETE", &path, Auth::Bearer(&t), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = s.call("GET", &path, Auth::Bearer(&t), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn token_scope_and_board_limits() {
    let s = start().await;
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let (_, b1) = s
        .call(
            "POST",
            "/api/boards",
            Auth::Cookie(&alice),
            Some(json!({"title": "1"})),
        )
        .await;
    let (_, b2) = s
        .call(
            "POST",
            "/api/boards",
            Auth::Cookie(&alice),
            Some(json!({"title": "2"})),
        )
        .await;
    let b1 = b1["id"].as_str().unwrap().to_string();
    let b2 = b2["id"].as_str().unwrap().to_string();
    let sticky = json!({"type": "shape", "kind": "sticky"});

    // 読むだけのトークン。
    let read = s.token(&alice, json!({"name": "r", "scope": "read"})).await;
    let (status, _) = s
        .call(
            "GET",
            &format!("/api/v1/boards/{b1}/elements"),
            Auth::Bearer(&read),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = s
        .call(
            "POST",
            &format!("/api/v1/boards/{b1}/elements"),
            Auth::Bearer(&read),
            Some(sticky.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = s
        .call(
            "POST",
            &format!("/api/v1/boards/{b1}/comments"),
            Auth::Bearer(&read),
            Some(json!({"body": "hi"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = s
        .call(
            "POST",
            "/api/v1/boards",
            Auth::Bearer(&read),
            Some(json!({"title": "x"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, info) = s
        .call(
            "GET",
            &format!("/api/v1/boards/{b1}"),
            Auth::Bearer(&read),
            None,
        )
        .await;
    assert_eq!(info["role"], "viewer");

    // ボードを限ったトークン。
    let only1 = s
        .token(
            &alice,
            json!({"name": "1", "scope": "write", "boards": [b1]}),
        )
        .await;
    let (status, _) = s
        .call(
            "POST",
            &format!("/api/v1/boards/{b1}/elements"),
            Auth::Bearer(&only1),
            Some(sticky.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = s
        .call(
            "GET",
            &format!("/api/v1/boards/{b2}/elements"),
            Auth::Bearer(&only1),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = s
        .call("GET", "/api/v1/boards", Auth::Bearer(&only1), None)
        .await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    // 書き込みトークンでも、持ち主が閲覧者なら書けない。
    s.call(
        "PUT",
        &format!("/api/boards/{b1}/members"),
        Auth::Cookie(&alice),
        Some(json!({"login": "bob", "role": "viewer"})),
    )
    .await;
    let bob_t = s.token(&bob, json!({"name": "b", "scope": "write"})).await;
    let (status, _) = s
        .call(
            "GET",
            &format!("/api/v1/boards/{b1}/elements"),
            Auth::Bearer(&bob_t),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = s
        .call(
            "POST",
            &format!("/api/v1/boards/{b1}/elements"),
            Auth::Bearer(&bob_t),
            Some(sticky.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = s
        .call(
            "GET",
            &format!("/api/v1/boards/{b2}/elements"),
            Auth::Bearer(&bob_t),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // トークンは Web 用の API(共有設定など)には使えない。
    let write = s
        .token(&alice, json!({"name": "w", "scope": "write"}))
        .await;
    let (status, _) = s
        .call(
            "GET",
            &format!("/api/boards/{b1}/sharing"),
            Auth::Bearer(&write),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // コメントは書き込みトークンで書ける。
    let (status, _) = s
        .call(
            "POST",
            &format!("/api/v1/boards/{b1}/comments"),
            Auth::Bearer(&write),
            Some(json!({"body": "API から"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, comments) = s
        .call(
            "GET",
            &format!("/api/v1/boards/{b1}/comments"),
            Auth::Bearer(&read),
            None,
        )
        .await;
    assert_eq!(comments.as_array().unwrap().len(), 1);

    // ログインなし: 誰でも編集できるボードなら使える。
    let (status, _) = s
        .call(
            "POST",
            "/api/v1/boards/open-room/elements",
            Auth::None,
            Some(sticky),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}
