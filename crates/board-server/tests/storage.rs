//! チームのデータの保存場所(S3 互換・ディレクトリ)を、テスト用の S3(このファイルの中の小さな HTTP サーバー)で確かめる。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::any;
use board_core::protocol::Message;
use board_core::{Board, Rgba8, Shape, Vec2};
use board_server::storage::{DirBlob, Location, S3Blob, S3Config};
use board_server::{Config, Hub, Store, router};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

type Objects = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// PUT・GET・DELETE だけの S3。署名の形と本文のハッシュを確かめる。
async fn s3(
    State(objects): State<Objects>,
    method: Method,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !auth.starts_with("AWS4-HMAC-SHA256 Credential=AKTEST/")
        || !auth.contains("/ap-northeast-1/s3/aws4_request")
        || !auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date")
    {
        return (
            StatusCode::FORBIDDEN,
            b"<Error><Code>AccessDenied</Code></Error>".to_vec(),
        );
    }
    let hash: String = Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if headers
        .get("x-amz-content-sha256")
        .and_then(|v| v.to_str().ok())
        != Some(hash.as_str())
    {
        return (
            StatusCode::BAD_REQUEST,
            b"<Error><Code>XAmzContentSHA256Mismatch</Code></Error>".to_vec(),
        );
    }
    let mut map = objects.lock().unwrap();
    match method {
        Method::PUT => {
            map.insert(key, body.to_vec());
            (StatusCode::OK, Vec::new())
        }
        Method::GET => match map.get(&key) {
            Some(b) => (StatusCode::OK, b.clone()),
            None => (
                StatusCode::NOT_FOUND,
                b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
            ),
        },
        Method::DELETE => {
            map.remove(&key);
            (StatusCode::NO_CONTENT, Vec::new())
        }
        _ => (StatusCode::METHOD_NOT_ALLOWED, Vec::new()),
    }
}

struct Env {
    app: Router,
    ws: String,
    hub: Arc<Hub>,
    store: Arc<Store>,
    objects: Objects,
    dir: tempfile::TempDir,
}

async fn start() -> Env {
    let objects: Objects = Arc::default();
    let s3_app = Router::new()
        .route("/{*key}", any(s3))
        .with_state(objects.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let s3_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, s3_app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::memory());
    store.set_locations(vec![
        Location {
            id: "tokyo".into(),
            label: "東京".into(),
            description: "S3 互換(ap-northeast-1)".into(),
            blob: Arc::new(S3Blob::new(S3Config {
                endpoint: s3_url,
                bucket: "gb".into(),
                prefix: "prod/".into(),
                region: "ap-northeast-1".into(),
                access_key: "AKTEST".into(),
                secret_key: "secret".into(),
                virtual_host: false,
            })),
        },
        Location {
            id: "nas".into(),
            label: "社内 NAS".into(),
            description: "サーバーのディレクトリ".into(),
            blob: Arc::new(DirBlob::new(dir.path().to_path_buf())),
        },
    ]);
    let hub = Arc::new(Hub::new(store.clone()));
    let app = router(hub.clone(), Config::default());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move { axum::serve(l, served).await.unwrap() });
    Env {
        app,
        ws: format!("ws://{addr}/ws"),
        hub,
        store,
        objects,
        dir,
    }
}

impl Env {
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
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            cookie,
        )
    }

    async fn signup(&self, login: &str) -> String {
        let (s, _, c) = self
            .call(
                "POST",
                "/api/signup",
                None,
                Some(json!({"login": login, "name": "", "password": "password123"})),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        c.unwrap()
    }

    /// つないで最初の状態を受け取る。
    async fn open(
        &self,
        room: &str,
        cookie: &str,
    ) -> (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Board,
    ) {
        let mut req = format!("{}/{room}", self.ws).into_client_request().unwrap();
        req.headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        let (mut ws, _) = connect_async(req).await.unwrap();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let WsMessage::Binary(b) = msg
                && let Ok(Message::Sync { state, .. }) = Message::decode(&b)
            {
                let mut board = Board::new();
                board.apply_remote(&state).unwrap();
                return (ws, board);
            }
        }
    }

    /// メモリ上の部屋を書き出して外す(誰もいなければ)。
    async fn flush(&self) {
        let hub = self.hub.clone();
        tokio::task::spawn_blocking(move || hub.flush())
            .await
            .unwrap();
    }

    fn dir_has(&self, key: &str) -> bool {
        self.dir.path().join(key).exists()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_boards_move_between_storage_locations() {
    let e = start().await;
    let alice = e.signup("alice").await;
    let bob = e.signup("bob").await;

    // 選べる保存場所。
    let (_, locs, _) = e.call("GET", "/api/storage", Some(&alice), None).await;
    let ids: Vec<&str> = locs
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["local", "nas", "tokyo"]);

    let (_, team, _) = e
        .call(
            "POST",
            "/api/teams",
            Some(&alice),
            Some(json!({"name": "研究所"})),
        )
        .await;
    let tid = team["id"].as_str().unwrap().to_string();
    e.call(
        "PUT",
        &format!("/api/teams/{tid}/members"),
        Some(&alice),
        Some(json!({"login": "bob", "role": "admin"})),
    )
    .await;
    let (_, b, _) = e
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "実験ノート", "team": tid})),
        )
        .await;
    let id = b["id"].as_str().unwrap().to_string();

    // 付箋を 1 枚置いて保存し、名前付きのバージョンも作る。
    let (mut ws, mut board) = e.open(&id, &alice).await;
    let eid = board.alloc_id();
    board.push(Shape::sticky(eid, Vec2::ZERO, Rgba8::WHITE));
    for u in board.take_outgoing() {
        ws.send(WsMessage::Binary(Message::Update(u).encode().into()))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (s, v, _) = e
        .call(
            "POST",
            &format!("/api/boards/{id}/versions"),
            Some(&alice),
            Some(json!({"label": "初版"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let vid = v["id"].as_u64().unwrap();
    let after_first = board.len();
    e.flush().await;
    assert!(e.store.load_at(None, &id).unwrap().is_some());

    // オーナー以外は変えられない。
    let path = format!("/api/teams/{tid}/storage");
    let (s, _, _) = e
        .call("PUT", &path, Some(&bob), Some(json!({"location": "tokyo"})))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = e
        .call(
            "PUT",
            &path,
            Some(&alice),
            Some(json!({"location": "mars"})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // 東京(S3)へ移す。編集中のまま移しても中身は残る。
    let (s, team, _) = e
        .call(
            "PUT",
            &path,
            Some(&alice),
            Some(json!({"location": "tokyo"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{team}");
    assert_eq!(team["storage"], "tokyo");
    {
        let objs = e.objects.lock().unwrap();
        assert!(
            objs.contains_key(&format!("gb/prod/boards/{id}.ybin")),
            "{:?}",
            objs.keys()
        );
        assert!(objs.contains_key(&format!("gb/prod/boards/{id}/versions/{vid}.ybin")));
    }
    // このサーバーからは消えている。
    assert!(e.store.load_at(None, &id).unwrap().is_none());

    // 移したあとの編集も S3 に入る。
    let eid2 = board.alloc_id();
    board.push(Shape::sticky(eid2, Vec2::new(300.0, 0.0), Rgba8::WHITE));
    for u in board.take_outgoing() {
        ws.send(WsMessage::Binary(Message::Update(u).encode().into()))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(ws);
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.flush().await;
    e.flush().await;
    let (_ws, reopened) = e.open(&id, &alice).await;
    assert_eq!(reopened.len(), board.len());
    assert!(reopened.len() > after_first);
    drop(_ws);

    // バージョンの中身も S3 から読める。
    let req = Request::builder()
        .uri(format!("/api/boards/{id}/versions/{vid}"))
        .header(header::COOKIE, &alice)
        .body(Body::empty())
        .unwrap();
    let res = e.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let mut old = Board::new();
    old.apply_remote(&bytes).unwrap();
    assert_eq!(old.len(), after_first);

    // 社内 NAS へ。S3 からは消える。
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.flush().await;
    let (s, _, _) = e
        .call("PUT", &path, Some(&alice), Some(json!({"location": "nas"})))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(e.dir_has(&format!("boards/{id}.ybin")));
    assert!(e.dir_has(&format!("boards/{id}/versions/{vid}.ybin")));
    assert!(
        e.objects.lock().unwrap().is_empty(),
        "{:?}",
        e.objects.lock().unwrap().keys()
    );

    // ボードを消すと保存場所からも消える。
    let (s, _, _) = e
        .call("DELETE", &format!("/api/boards/{id}"), Some(&alice), None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(!e.dir_has(&format!("boards/{id}.ybin")));
    assert!(!e.dir_has(&format!("boards/{id}/versions/{vid}.ybin")));

    // 操作ログ。
    let (_, events, _) = e
        .call(
            "GET",
            &format!("/api/audit?team={tid}&action=team.storage"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(events.as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_team_brings_data_back() {
    let e = start().await;
    let alice = e.signup("alice").await;
    let (_, team, _) = e
        .call(
            "POST",
            "/api/teams",
            Some(&alice),
            Some(json!({"name": "臨時"})),
        )
        .await;
    let tid = team["id"].as_str().unwrap().to_string();
    e.call(
        "PUT",
        &format!("/api/teams/{tid}/storage"),
        Some(&alice),
        Some(json!({"location": "tokyo"})),
    )
    .await;
    let (_, b, _) = e
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "x", "team": tid})),
        )
        .await;
    let id = b["id"].as_str().unwrap().to_string();
    let (mut ws, mut board) = e.open(&id, &alice).await;
    let eid = board.alloc_id();
    board.push(Shape::sticky(eid, Vec2::ZERO, Rgba8::WHITE));
    for u in board.take_outgoing() {
        ws.send(WsMessage::Binary(Message::Update(u).encode().into()))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(ws);
    tokio::time::sleep(Duration::from_millis(200)).await;
    e.flush().await;
    let expected = board.len();
    assert!(
        e.objects
            .lock()
            .unwrap()
            .contains_key(&format!("gb/prod/boards/{id}.ybin"))
    );

    // チームを消すとボードは個人ボードに戻り、中身もこのサーバーに戻る。
    let (s, _, _) = e
        .call("DELETE", &format!("/api/teams/{tid}"), Some(&alice), None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(e.objects.lock().unwrap().is_empty());
    e.flush().await;
    let (_ws, reopened) = e.open(&id, &alice).await;
    assert_eq!(reopened.len(), expected);
}
