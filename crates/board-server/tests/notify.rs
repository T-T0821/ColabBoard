//! メール・プッシュ通知。実際には送らず、送ったものを記録して確かめる。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use board_server::notify::{Notifier, Outgoing};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const FCM: &str = "https://fcm.googleapis.com/fcm/send/abc123";

struct Server {
    app: Router,
    log: Arc<Mutex<Vec<Outgoing>>>,
}

impl Server {
    fn new(email: bool) -> Self {
        let store = Arc::new(Store::memory());
        let (notifier, log) = Notifier::recording(&store, email);
        let hub = Arc::new(Hub::new(store));
        let config = Config {
            notifier: Arc::new(notifier),
            ..Config::default()
        };
        Self {
            app: router(hub, config),
            log,
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
        let extra = res
            .headers()
            .get(header::SET_COOKIE)
            .or(res.headers().get(header::LOCATION))
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            extra,
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

    /// 裏で送られるのを少し待って、記録を取り出す。
    async fn take(&self, n: usize) -> Vec<Outgoing> {
        for _ in 0..100 {
            if self.log.lock().unwrap().len() >= n {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

fn kinds(on: [bool; 3]) -> Value {
    json!({"mention": on[0], "reply": on[1], "due": on[2]})
}

#[tokio::test]
async fn email_and_push_for_mentions() {
    let s = Server::new(true);
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let (_, b, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "企画"})),
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

    // 最初の状態。ログインしていなければ見られない。
    let (status, _, _) = s.call("GET", "/api/notify/settings", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, v, _) = s
        .call("GET", "/api/notify/settings", Some(&bob), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["email"], "");
    assert_eq!(v["email_available"], true);
    assert!(v["push_key"].as_str().unwrap().len() > 80);
    assert_eq!(v["email_on"], kinds([true, false, true]));

    // 変なアドレスは断る。
    let (status, _, _) = s
        .call(
            "PUT",
            "/api/notify/settings",
            Some(&bob),
            Some(json!({"email": "bob at example", "email_on": kinds([true; 3]), "push_on": kinds([true; 3])})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // アドレスを登録すると確認のメール。リンクを開くまでは使わない。
    let (status, v, _) = s
        .call(
            "PUT",
            "/api/notify/settings",
            Some(&bob),
            Some(json!({"email": "bob@example.com", "email_on": kinds([true, false, true]), "push_on": kinds([true; 3])})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["email_verified"], false);
    let sent = s.take(1).await;
    let Outgoing::Email { to, subject, body } = &sent[0] else {
        panic!("確認のメールがありません: {sent:?}");
    };
    assert_eq!(to, "bob@example.com");
    assert!(subject.contains("確認"));
    let token = body
        .split("token=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(body.contains("https://board.example.com/api/notify/verify?token="));

    // 続けて送り直すのは 1 分待つ。
    let (status, _, _) = s
        .call(
            "PUT",
            "/api/notify/settings",
            Some(&bob),
            Some(json!({"email": "bob2@example.com", "email_on": kinds([true; 3]), "push_on": kinds([true; 3])})),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // 偽のトークンは効かない。本物で確認できる。2 回目は使えない。
    let (status, _, loc) = s
        .call("GET", "/api/notify/verify?token=nope", None, None)
        .await;
    assert!(status.is_redirection());
    assert_eq!(loc.as_deref(), Some("/?notify=invalid"));
    let (_, _, loc) = s
        .call(
            "GET",
            &format!("/api/notify/verify?token={token}"),
            None,
            None,
        )
        .await;
    assert_eq!(loc.as_deref(), Some("/?notify=verified"));
    let (_, _, loc) = s
        .call(
            "GET",
            &format!("/api/notify/verify?token={token}"),
            None,
            None,
        )
        .await;
    assert_eq!(loc.as_deref(), Some("/?notify=invalid"));
    let (_, v, _) = s
        .call("GET", "/api/notify/settings", Some(&bob), None)
        .await;
    assert_eq!(v["email_verified"], true);

    // プッシュ通知: 知らない送り先は断る。
    let (status, _, _) = s
        .call(
            "POST",
            "/api/notify/push",
            Some(&bob),
            Some(json!({"endpoint": "https://evil.example.com/push"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = s
        .call(
            "POST",
            "/api/notify/push",
            Some(&bob),
            Some(json!({"endpoint": FCM, "keys": {"p256dh": "x", "auth": "y"}})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, v, _) = s
        .call("GET", "/api/notify/settings", Some(&bob), None)
        .await;
    assert_eq!(v["push_devices"], 1);

    // メンションと期限付きのコメント → bob にメールとプッシュ。
    let due = 1_893_456_000_000u64; // 2030-01-01T00:00:00Z
    let (status, _, _) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/comments"),
            Some(&alice),
            Some(json!({"body": "@bob 見積もりをお願いします", "x": 1.0, "y": 2.0, "due": due})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let mut sent = s.take(2).await;
    sent.sort_by_key(|o| matches!(o, Outgoing::Push { .. }));
    assert_eq!(sent.len(), 2, "{sent:?}");
    let Outgoing::Email { to, subject, body } = &sent[0] else {
        panic!("メールがありません: {sent:?}");
    };
    assert_eq!(to, "bob@example.com");
    assert!(
        subject.contains("ALICE があなたをメンションしました"),
        "{subject}"
    );
    assert!(subject.contains("企画"));
    assert!(body.contains("見積もり"));
    assert!(body.contains(&format!("https://board.example.com/?room={id}&comment=")));
    assert_eq!(
        sent[1],
        Outgoing::Push {
            endpoint: FCM.into()
        }
    );
    let (_, list, _) = s
        .call(
            "GET",
            &format!("/api/boards/{id}/comments"),
            Some(&bob),
            None,
        )
        .await;
    assert_eq!(list[0]["due"], due);

    // メールでメンションを受け取らない設定にすると、プッシュだけ。
    s.call(
        "PUT",
        "/api/notify/settings",
        Some(&bob),
        Some(json!({"email": "bob@example.com", "email_on": kinds([false, false, true]), "push_on": kinds([true; 3])})),
    )
    .await;
    s.call(
        "POST",
        &format!("/api/boards/{id}/comments"),
        Some(&alice),
        Some(json!({"body": "@bob もう一つ", "x": 1.0, "y": 2.0})),
    )
    .await;
    let sent = s.take(1).await;
    assert_eq!(
        sent,
        vec![Outgoing::Push {
            endpoint: FCM.into()
        }]
    );

    // テスト送信は種類の設定にかかわらず全部へ。
    let (status, _, _) = s.call("POST", "/api/notify/test", Some(&bob), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s.take(2).await.len(), 2);

    // プッシュ通知をやめる。送り先がなくなるとテスト送信は断る(メールは残る)。
    let (status, _, _) = s
        .call(
            "DELETE",
            "/api/notify/push",
            Some(&bob),
            Some(json!({"endpoint": FCM})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, v, _) = s
        .call("GET", "/api/notify/settings", Some(&bob), None)
        .await;
    assert_eq!(v["push_devices"], 0);
    // alice は送り先がない。
    let (status, _, _) = s.call("POST", "/api/notify/test", Some(&alice), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn email_needs_smtp() {
    let s = Server::new(false);
    let bob = s.signup("bob").await;
    let (_, v, _) = s
        .call("GET", "/api/notify/settings", Some(&bob), None)
        .await;
    assert_eq!(v["email_available"], false);
    let (status, v, _) = s
        .call(
            "PUT",
            "/api/notify/settings",
            Some(&bob),
            Some(json!({"email": "bob@example.com", "email_on": kinds([true; 3]), "push_on": kinds([true; 3])})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        v["error"].as_str().unwrap_or_default().contains("SMTP_URL"),
        "{v}"
    );
    // 種類の設定だけなら変えられる。
    let (status, v, _) = s
        .call(
            "PUT",
            "/api/notify/settings",
            Some(&bob),
            Some(json!({"email": "", "email_on": kinds([true; 3]), "push_on": kinds([false, false, true])})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["push_on"], kinds([false, false, true]));
}
