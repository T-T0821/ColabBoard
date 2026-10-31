//! 動画メモの保存・配信と権限。

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
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let h = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, bytes, h)
}

async fn login(app: &Router, name: &str) -> String {
    let body = json!({"login": name, "name": name, "password": "password123"}).to_string();
    let (s, _, h) = call(
        app,
        "POST",
        "/api/signup",
        &[("content-type", "application/json")],
        body.into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    h.get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn upload_and_stream_with_permissions() {
    let app = router(
        Arc::new(Hub::new(Arc::new(Store::memory()))),
        Config::default(),
    );
    let alice = login(&app, "alice").await;
    let bob = login(&app, "bob").await;
    let (s, b, _) = call(
        &app,
        "POST",
        "/api/boards",
        &[("cookie", &alice), ("content-type", "application/json")],
        json!({"title": "動画"}).to_string().into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let id = serde_json::from_slice::<Value>(&b).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let upload = format!("/api/boards/{id}/media");
    let video: Vec<u8> = (0..=255u8).cycle().take(3000).collect();

    // 他人と未ログインは置けない。種類の違うものも置けない。
    let webm = ("content-type", "video/webm;codecs=vp9,opus");
    for cookie in [&bob, ""] {
        let (s, _, _) = call(
            &app,
            "POST",
            &upload,
            &[("cookie", cookie), webm],
            video.clone(),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    let (s, _, _) = call(
        &app,
        "POST",
        &upload,
        &[("cookie", &alice), ("content-type", "text/html")],
        video.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // オーナーは置ける。2 MB を超える大きさも受け付ける。
    let big = vec![7u8; 3 * 1024 * 1024];
    let (s, _, _) = call(&app, "POST", &upload, &[("cookie", &alice), webm], big).await;
    assert_eq!(s, StatusCode::OK);
    let (s, b, _) = call(
        &app,
        "POST",
        &upload,
        &[("cookie", &alice), webm],
        video.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let url = serde_json::from_slice::<Value>(&b).unwrap()["url"]
        .as_str()
        .unwrap()
        .to_string();

    // 全体と範囲指定で取れる。
    let (s, body, h) = call(&app, "GET", &url, &[("cookie", &alice)], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, video);
    assert_eq!(h[header::CONTENT_TYPE], "video/webm");
    let (s, body, h) = call(
        &app,
        "GET",
        &url,
        &[("cookie", &alice), ("range", "bytes=100-199")],
        vec![],
    )
    .await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, video[100..200]);
    assert_eq!(h[header::CONTENT_RANGE], "bytes 100-199/3000");
    let (s, _, _) = call(
        &app,
        "GET",
        &url,
        &[("cookie", &alice), ("range", "bytes=5000-")],
        vec![],
    )
    .await;
    assert_eq!(s, StatusCode::RANGE_NOT_SATISFIABLE);

    // 権限のない人は見られない。
    let (s, _, _) = call(&app, "GET", &url, &[("cookie", &bob)], vec![]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // ボードを消すと動画も消える。
    let (s, _, _) = call(
        &app,
        "DELETE",
        &format!("/api/boards/{id}"),
        &[("cookie", &alice)],
        vec![],
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = call(&app, "GET", &url, &[("cookie", &alice)], vec![]).await;
    assert_ne!(s, StatusCode::OK);
}
