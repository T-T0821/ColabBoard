//! ボードの埋め込み(iframe)用の鍵と、埋め込みを許すヘッダー。

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
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
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        headers,
    )
}

#[tokio::test]
async fn embed_key_grants_its_role() {
    let app = router(
        Arc::new(Hub::new(Arc::new(Store::memory()))),
        Config::default(),
    );
    let (_, _, h) = call(
        &app,
        "POST",
        "/api/signup",
        None,
        Some(json!({"login": "alice", "name": "A", "password": "password123"})),
    )
    .await;
    let alice = h
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let (_, b, _) = call(
        &app,
        "POST",
        "/api/boards",
        Some(&alice),
        Some(json!({"title": "埋め込み"})),
    )
    .await;
    let id = b["id"].as_str().unwrap().to_string();

    // オーナーだけが設定できる。
    let (status, _, _) = call(
        &app,
        "PUT",
        &format!("/api/boards/{id}/embed"),
        None,
        Some(json!({"role": "viewer"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, sh, _) = call(
        &app,
        "PUT",
        &format!("/api/boards/{id}/embed"),
        Some(&alice),
        Some(json!({"role": "viewer"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sh}");
    assert!(sh["link"].is_null());
    let key = sh["embed"]["key"].as_str().unwrap().to_string();

    // 鍵があればログインなしで閲覧できる。
    let (status, info, _) = call(
        &app,
        "GET",
        &format!("/api/boards/{id}?key={key}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["role"], "viewer");

    // 編集可にすると同じ鍵で編集者になる。作り直すと古い鍵は使えない。
    call(
        &app,
        "PUT",
        &format!("/api/boards/{id}/embed"),
        Some(&alice),
        Some(json!({"role": "editor"})),
    )
    .await;
    let (_, info, _) = call(
        &app,
        "GET",
        &format!("/api/boards/{id}?key={key}"),
        None,
        None,
    )
    .await;
    assert_eq!(info["role"], "editor");
    let (_, sh, _) = call(
        &app,
        "PUT",
        &format!("/api/boards/{id}/embed"),
        Some(&alice),
        Some(json!({"role": "editor", "regenerate": true})),
    )
    .await;
    assert_ne!(sh["embed"]["key"].as_str().unwrap(), key);
    let (status, _, _) = call(
        &app,
        "GET",
        &format!("/api/boards/{id}?key={key}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn frame_ancestors_header() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>t</title>",
    )
    .unwrap();
    std::fs::write(dir.path().join("app.js"), "1").unwrap();
    let app = router(
        Arc::new(Hub::new(Arc::new(Store::memory()))),
        Config {
            static_dir: Some(dir.path().to_path_buf()),
            embed_origins: "https://wiki.example.com".into(),
            ..Config::default()
        },
    );
    let csp = |h: &axum::http::HeaderMap| {
        h.get(header::CONTENT_SECURITY_POLICY)
            .map(|v| v.to_str().unwrap().to_string())
    };
    let (_, _, h) = call(&app, "GET", "/?room=abc&embed=1", None, None).await;
    assert_eq!(
        csp(&h).as_deref(),
        Some("frame-ancestors https://wiki.example.com")
    );
    let (_, _, h) = call(&app, "GET", "/?room=abc", None, None).await;
    assert_eq!(csp(&h).as_deref(), Some("frame-ancestors 'self'"));
    // HTML 以外には付けない。
    let (_, _, h) = call(&app, "GET", "/app.js", None, None).await;
    assert_eq!(csp(&h), None);
}
