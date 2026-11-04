//! 会議(Zoom・Meet・Teams)とカレンダーの予定の紐づけ。

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Server {
    app: Router,
}

impl Server {
    fn new(config: Config) -> Self {
        let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
        Self {
            app: router(hub, config),
        }
    }

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
        let (status, _, cookie) = self
            .call(
                "POST",
                "/api/signup",
                None,
                Some(json!({"login": login, "name": login, "password": "password123"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        cookie.unwrap()
    }
}

#[tokio::test]
async fn link_meeting_to_board() {
    let s = Server::new(Config::default());
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let carol = s.signup("carol").await;
    let (_, b, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "定例"})),
        )
        .await;
    let id = b["id"].as_str().unwrap().to_string();
    for (login, role) in [("bob", "editor"), ("carol", "viewer")] {
        s.call(
            "PUT",
            &format!("/api/boards/{id}/members"),
            Some(&alice),
            Some(json!({"login": login, "role": role})),
        )
        .await;
    }
    let path = format!("/api/boards/{id}/meeting-link");

    // 最初は何もない。
    let (status, v, _) = s.call("GET", &path, Some(&carol), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, json!({"updated": 0}));

    // 編集者が紐づける。
    let (status, v, _) = s
        .call(
            "PUT",
            &path,
            Some(&bob),
            Some(json!({
                "url": "https://meet.google.com/abc-defg-hij",
                "title": "週次定例",
                "start": 1_793_667_600_000u64,
                "end": 1_793_671_200_000u64,
                "calendar_url": "https://calendar.google.com/calendar/event?eid=xyz"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["provider"], "meet");
    assert_eq!(v["updated_by"], "bob");

    // 閲覧者は見られるが変えられない。
    let (_, v, _) = s.call("GET", &path, Some(&carol), None).await;
    assert_eq!(v["title"], "週次定例");
    let (status, _, _) = s
        .call(
            "PUT",
            &path,
            Some(&carol),
            Some(json!({"url": "https://zoom.us/j/1"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // メンバーでない人・ログインしていない人は見られない。
    let dave = s.signup("dave").await;
    let (status, _, _) = s.call("GET", &path, Some(&dave), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = s.call("GET", &path, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 会議ツール以外の URL は断る。
    let (status, v, _) = s
        .call(
            "PUT",
            &path,
            Some(&bob),
            Some(json!({"url": "https://evil.example/j/1"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("Zoom"));

    // 空にすると外れる。
    let (status, v, _) = s.call("PUT", &path, Some(&alice), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(v.get("url").is_none());
    let (_, v, _) = s.call("GET", &path, Some(&alice), None).await;
    assert!(v.get("url").is_none());
}

#[tokio::test]
async fn extra_hosts_and_anonymous_boards() {
    let s = Server::new(Config {
        meeting_hosts: vec!["webex.com".into()],
        ..Config::default()
    });
    let alice = s.signup("alice").await;
    let (_, b, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "x"})),
        )
        .await;
    let id = b["id"].as_str().unwrap();
    let (status, v, _) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/meeting-link"),
            Some(&alice),
            Some(json!({"url": "https://acme.webex.com/meet/alice"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["provider"], "other");

    // 誰でも編集できる(情報のない)ボードには紐づけられない。
    let (status, _, _) = s
        .call(
            "PUT",
            "/api/boards/anon-room/meeting-link",
            Some(&alice),
            Some(json!({"url": "https://zoom.us/j/1"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, v, _) = s
        .call("GET", "/api/boards/anon-room/meeting-link", None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(v.get("url").is_none());
}
