//! Slack 通知。本物の Slack の代わりに偽の Webhook を立てて確かめる。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::post;
use axum::{Json, Router};
use board_server::slack::SlackConfig;
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

type Seen = Arc<Mutex<Vec<String>>>;

/// 偽の Incoming Webhook。届いた `text` を記録する。
async fn fake_slack() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let s = seen.clone();
    let app = Router::new().route(
        "/services/{*rest}",
        post(move |Json(body): Json<Value>| {
            let s = s.clone();
            async move {
                s.lock()
                    .unwrap()
                    .push(body["text"].as_str().unwrap_or_default().to_string());
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/"), seen)
}

struct Server {
    app: Router,
}

impl Server {
    fn new(prefix: &str) -> Self {
        let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
        let config = Config {
            slack: SlackConfig::new(
                vec![prefix.to_string()],
                Some("https://board.example.com".into()),
            ),
            ..Config::default()
        };
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
                Some(json!({"login": login, "name": login.to_uppercase(), "password": "password123"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        cookie.unwrap()
    }
}

/// 裏で送られるのを少し待つ。
async fn wait_for(seen: &Seen, n: usize) -> Vec<String> {
    for _ in 0..100 {
        if seen.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    seen.lock().unwrap().clone()
}

#[tokio::test]
async fn slack_settings_and_notifications() {
    let (base, seen) = fake_slack().await;
    let s = Server::new(&base);
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let (_, b, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "企画 <会議>"})),
        )
        .await;
    let id = b["id"].as_str().unwrap().to_string();
    s.call(
        "PUT",
        &format!("/api/boards/{id}/members"),
        Some(&alice),
        Some(json!({"login": "bob", "role": "editor"})),
    )
    .await;

    // 最初は未設定。オーナー以外は見られない。
    let (status, v, _) = s
        .call(
            "GET",
            &format!("/api/boards/{id}/slack"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["enabled"], false);
    let (status, _, _) = s
        .call("GET", &format!("/api/boards/{id}/slack"), Some(&bob), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 許可していない URL は登録できない。
    let (status, _, _) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/slack"),
            Some(&alice),
            Some(json!({"webhook": "https://evil.example/x"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let hook = format!("{base}services/T0/B0/secretTAIL");
    let (status, v, _) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/slack"),
            Some(&alice),
            Some(json!({"webhook": hook, "events": {"comments": false, "mentions": true, "versions": true}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["enabled"], true);
    assert_eq!(v["hint"], "…TAIL");
    assert!(!v.to_string().contains("secret"));

    // テスト送信。
    let (status, _, _) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/slack/test"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let got = wait_for(&seen, 1).await;
    assert!(got[0].contains("テスト通知"), "{got:?}");
    assert!(got[0].contains("&lt;会議&gt;"), "{got:?}");

    // メンションのないコメントは送らない設定。メンションありは送る。
    s.call(
        "POST",
        &format!("/api/boards/{id}/comments"),
        Some(&bob),
        Some(json!({"body": "ふつうのコメント", "x": 0.0, "y": 0.0})),
    )
    .await;
    s.call(
        "POST",
        &format!("/api/boards/{id}/comments"),
        Some(&bob),
        Some(json!({"body": "@alice 確認お願いします", "x": 0.0, "y": 0.0})),
    )
    .await;
    let got = wait_for(&seen, 2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let got_all = seen.lock().unwrap().clone();
    assert_eq!(got_all.len(), 2, "{got_all:?}");
    assert!(got[1].contains("*BOB* さんが"), "{got:?}");
    assert!(got[1].contains("> @alice 確認お願いします"), "{got:?}");
    assert!(got[1].contains("メンション: @alice"), "{got:?}");
    assert!(
        got[1].contains(&format!("<https://board.example.com/?room={id}|")),
        "{got:?}"
    );

    // URL はそのままで、出来事だけ変える(バージョンはオフ)。
    let (status, v, _) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/slack"),
            Some(&alice),
            Some(json!({"events": {"comments": true, "mentions": true, "versions": false}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["hint"], "…TAIL");

    // 通知をやめる。
    let (_, v, _) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/slack"),
            Some(&alice),
            Some(json!({"webhook": ""})),
        )
        .await;
    assert_eq!(v["enabled"], false);
    s.call(
        "POST",
        &format!("/api/boards/{id}/comments"),
        Some(&bob),
        Some(json!({"body": "@alice もう一度", "x": 0.0, "y": 0.0})),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(seen.lock().unwrap().len(), 2);
}
