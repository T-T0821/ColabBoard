//! 直接接続(P2P)の利用可否・利用記録・通報を確かめる。

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn app(config: Config) -> Router {
    router(Arc::new(Hub::new(Arc::new(Store::memory()))), config)
}

async fn call(
    app: &Router,
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
    let res = app.clone().oneshot(req).await.unwrap();
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

async fn signup(app: &Router, login: &str) -> String {
    let (status, body, cookie) = call(
        app,
        "POST",
        "/api/signup",
        None,
        Some(json!({"login": login, "name": login.to_uppercase(), "password": "password123"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    cookie.unwrap()
}

fn event(kind: &str) -> Value {
    json!({"kind": kind, "at": 1_793_507_385_000u64, "peer": "佐藤"})
}

#[tokio::test]
async fn anonymous_board_allows_and_logs_without_content() {
    let app = app(Config {
        admins: vec!["root".into()],
        ..Config::default()
    });
    let root = signup(&app, "root").await;
    let (status, policy, _) = call(&app, "GET", "/api/boards/free-room/p2p", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(policy["allowed"], true);

    let (status, _, _) = call(
        &app,
        "POST",
        "/api/boards/free-room/p2p/events",
        None,
        Some(json!([event("invite"), event("connect")])),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, events, _) = call(&app, "GET", "/api/audit?action=p2p", Some(&root), None).await;
    let events = events.as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["action"], "p2p.connect");
    assert_eq!(events[1]["action"], "p2p.invite");
    assert_eq!(events[0]["board"], "free-room");
    let detail = events[0]["detail"].as_str().unwrap();
    assert!(detail.contains("2026-11-01 04:29 UTC"), "{detail}");
    assert!(detail.contains("相手 佐藤"), "{detail}");
    assert!(detail.contains("ログインなし"), "{detail}");
}

#[tokio::test]
async fn team_admin_can_turn_p2p_off() {
    let app = app(Config::default());
    let alice = signup(&app, "alice").await;
    let bob = signup(&app, "bob").await;
    let (_, team, _) = call(
        &app,
        "POST",
        "/api/teams",
        Some(&alice),
        Some(json!({"name": "営業"})),
    )
    .await;
    let tid = team["id"].as_str().unwrap().to_string();
    call(
        &app,
        "PUT",
        &format!("/api/teams/{tid}/members"),
        Some(&alice),
        Some(json!({"login": "bob", "role": "member"})),
    )
    .await;
    let (_, board, _) = call(
        &app,
        "POST",
        "/api/boards",
        Some(&alice),
        Some(json!({"title": "見込み客", "team": tid})),
    )
    .await;
    let bid = board["id"].as_str().unwrap().to_string();
    let policy_path = format!("/api/boards/{bid}/p2p");

    let (_, policy, _) = call(&app, "GET", &policy_path, Some(&bob), None).await;
    assert_eq!(policy["allowed"], true);

    // メンバーは切り替えられない。
    let (status, _, _) = call(
        &app,
        "PUT",
        &format!("/api/teams/{tid}/p2p"),
        Some(&bob),
        Some(json!({"allowed": false})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, team, _) = call(
        &app,
        "PUT",
        &format!("/api/teams/{tid}/p2p"),
        Some(&alice),
        Some(json!({"allowed": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(team["p2p_disabled"], true);

    let (_, policy, _) = call(&app, "GET", &policy_path, Some(&bob), None).await;
    assert_eq!(policy["allowed"], false);
    assert!(policy["reason"].as_str().unwrap().contains("営業"));

    // 止められた後の記録は 403 になる(クライアントは切断する)が、記録は残る。
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("{policy_path}/events"),
        Some(&bob),
        Some(json!([event("join")])),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 通報も残る。
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("{policy_path}/report"),
        Some(&bob),
        Some(json!({"reason": "leak", "peer": "知らない人", "note": "社外の人が入ってきた", "at": 0})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, events, _) = call(
        &app,
        "GET",
        &format!("/api/audit?team={tid}"),
        Some(&alice),
        None,
    )
    .await;
    let actions: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        &actions[..3],
        &["p2p.report", "p2p.join", "team.p2p"],
        "{actions:?}"
    );
    let report = &events[0];
    assert_eq!(report["actor"], "bob");
    assert_eq!(report["target"], "leak");
    let detail = report["detail"].as_str().unwrap();
    assert!(detail.contains("情報の持ち出し"), "{detail}");
    assert!(detail.contains("社外の人が入ってきた"), "{detail}");

    // 戻せる。
    call(
        &app,
        "PUT",
        &format!("/api/teams/{tid}/p2p"),
        Some(&alice),
        Some(json!({"allowed": true})),
    )
    .await;
    let (_, policy, _) = call(&app, "GET", &policy_path, Some(&bob), None).await;
    assert_eq!(policy["allowed"], true);
}

#[tokio::test]
async fn server_can_disable_and_outsiders_are_refused() {
    let app_off = app(Config {
        p2p: false,
        ..Config::default()
    });
    let (_, policy, _) = call(&app_off, "GET", "/api/boards/free-room/p2p", None, None).await;
    assert_eq!(policy["allowed"], false);

    let app = app(Config::default());
    let alice = signup(&app, "alice").await;
    let (_, board, _) = call(
        &app,
        "POST",
        "/api/boards",
        Some(&alice),
        Some(json!({"title": "秘密"})),
    )
    .await;
    let bid = board["id"].as_str().unwrap();
    // ボードを開けない人は、記録も通報もできない。
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/api/boards/{bid}/p2p/events"),
        None,
        Some(json!([event("join")])),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 多すぎる記録は断る。
    let many: Vec<Value> = (0..101).map(|_| event("connect")).collect();
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/api/boards/{bid}/p2p/events"),
        Some(&alice),
        Some(Value::Array(many)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
