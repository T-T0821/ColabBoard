//! 料金プラン: 上限、テスト決済、Stripe(偽の API と署名付きの Webhook)。

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::post;
use axum::{Json, Router};
use board_core::{Board, Rgba8, Shape, Vec2};
use board_server::ai::{Ai, AiConfig};
use board_server::billing::{BillingConfig, Mode, StripeConfig, sign};
use board_server::guard::Limits;
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Resp {
    status: StatusCode,
    json: Value,
    text: String,
    location: Option<String>,
    cookie: Option<String>,
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    headers: &[(&str, String)],
    body: Option<String>,
) -> Resp {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b)),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let location = res
        .headers()
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_string());
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    Resp {
        status,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text,
        location,
        cookie,
    }
}

async fn signup(app: &Router, login: &str) -> String {
    let r = call(
        app,
        "POST",
        "/api/signup",
        None,
        &[],
        Some(json!({ "login": login, "name": login, "password": "password123" }).to_string()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.cookie.unwrap()
}

async fn create_board(app: &Router, cookie: &str, title: &str) -> Resp {
    call(
        app,
        "POST",
        "/api/boards",
        Some(cookie),
        &[],
        Some(json!({ "title": title }).to_string()),
    )
    .await
}

fn config(mode: Mode, free_boards: u32) -> Config {
    let mut billing = BillingConfig {
        mode,
        ..BillingConfig::default()
    };
    billing.free.boards = free_boards;
    billing.free.members_per_board = 1;
    billing.free.ai_per_month = 1;
    billing.free.media_mb_per_board = 1;
    Config {
        limits: Limits::unlimited(),
        billing,
        ..Config::default()
    }
}

fn app_with(config: Config) -> (Router, Arc<Hub>) {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    (router(hub.clone(), config), hub)
}

#[tokio::test]
async fn off_by_default_has_no_plan_limits() {
    let (app, _) = app_with(Config {
        limits: Limits::unlimited(),
        ..Config::default()
    });
    let alice = signup(&app, "alice").await;
    for i in 0..5 {
        assert_eq!(
            create_board(&app, &alice, &format!("b{i}")).await.status,
            StatusCode::CREATED
        );
    }
    let r = call(&app, "GET", "/api/billing", Some(&alice), &[], None).await;
    assert_eq!(r.json["mode"], "off");
    assert_eq!(r.json["plans"], json!([]));
    let r = call(
        &app,
        "POST",
        "/api/billing/checkout",
        Some(&alice),
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mock_checkout_upgrades_and_cancel_downgrades() {
    let (app, _) = app_with(config(Mode::Mock, 2));
    let alice = signup(&app, "alice").await;
    assert_eq!(
        create_board(&app, &alice, "1").await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        create_board(&app, &alice, "2").await.status,
        StatusCode::CREATED
    );
    let r = create_board(&app, &alice, "3").await;
    assert_eq!(r.status, StatusCode::PAYMENT_REQUIRED);
    assert!(
        r.json["error"].as_str().unwrap().contains("2 個まで"),
        "{}",
        r.text
    );

    let r = call(&app, "GET", "/api/billing", Some(&alice), &[], None).await;
    assert_eq!(r.json["mode"], "mock");
    assert_eq!(r.json["plan"], "free");
    assert_eq!(r.json["usage"]["boards"], 2);
    assert_eq!(r.json["limits"]["boards"], 2);
    assert_eq!(r.json["plans"][1]["id"], "pro");

    // ほかの人の申し込みは使えない。
    let r = call(
        &app,
        "POST",
        "/api/billing/checkout",
        Some(&alice),
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let url = r.json["url"].as_str().unwrap().to_string();
    let path = &url[url.find("/api/").unwrap()..];
    assert!(path.starts_with("/api/billing/mock/checkout/cs_mock_"));
    let bob = signup(&app, "bob").await;
    let r = call(&app, "GET", path, Some(&bob), &[], None).await;
    assert!(r.text.contains("期限が切れました"));
    let r = call(&app, "POST", path, Some(&bob), &[], None).await;
    assert_eq!(r.location.as_deref(), Some("/?billing=cancel"));

    let r = call(&app, "GET", path, Some(&alice), &[], None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text.contains("テスト決済を完了する"));
    let r = call(&app, "POST", path, Some(&alice), &[], None).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location.as_deref(), Some("/?billing=success"));
    // 同じ申し込みは二度使えない。
    let r = call(&app, "POST", path, Some(&alice), &[], None).await;
    assert_eq!(r.location.as_deref(), Some("/?billing=cancel"));

    let r = call(&app, "GET", "/api/billing", Some(&alice), &[], None).await;
    assert_eq!(r.json["plan"], "pro");
    assert_eq!(r.json["can_manage"], true);
    assert_eq!(
        create_board(&app, &alice, "3").await.status,
        StatusCode::CREATED
    );
    let r = call(
        &app,
        "POST",
        "/api/billing/checkout",
        Some(&alice),
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // 操作ログに残る。
    let r = call(&app, "POST", "/api/billing/portal", Some(&alice), &[], None).await;
    assert!(
        r.json["url"]
            .as_str()
            .unwrap()
            .ends_with("/api/billing/mock/portal")
    );
    let r = call(
        &app,
        "GET",
        "/api/billing/mock/portal",
        Some(&alice),
        &[],
        None,
    )
    .await;
    assert!(r.text.contains("解約して無料プランに戻す"));
    let r = call(
        &app,
        "POST",
        "/api/billing/mock/cancel",
        Some(&alice),
        &[],
        None,
    )
    .await;
    assert_eq!(r.location.as_deref(), Some("/?billing=canceled"));
    let r = call(&app, "GET", "/api/billing", Some(&alice), &[], None).await;
    assert_eq!(r.json["plan"], "free");
    // 下げてもボードは残る(増やせないだけ)。
    let r = call(&app, "GET", "/api/boards", Some(&alice), &[], None).await;
    assert_eq!(r.json.as_array().unwrap().len(), 3);
    assert_eq!(
        create_board(&app, &alice, "4").await.status,
        StatusCode::PAYMENT_REQUIRED
    );
}

#[tokio::test]
async fn member_and_media_limits_follow_the_owner_plan() {
    let (app, _) = app_with(config(Mode::Mock, 3));
    let alice = signup(&app, "alice").await;
    signup(&app, "bob").await;
    signup(&app, "carol").await;
    let id = create_board(&app, &alice, "b").await.json["id"]
        .as_str()
        .unwrap()
        .to_string();
    let member =
        |login: &str, role: Option<&str>| json!({ "login": login, "role": role }).to_string();
    let path = format!("/api/boards/{id}/members");
    let r = call(
        &app,
        "PUT",
        &path,
        Some(&alice),
        &[],
        Some(member("bob", Some("editor"))),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = call(
        &app,
        "PUT",
        &path,
        Some(&alice),
        &[],
        Some(member("carol", Some("viewer"))),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYMENT_REQUIRED);
    // 今いる人の権限は変えられる。
    let r = call(
        &app,
        "PUT",
        &path,
        Some(&alice),
        &[],
        Some(member("bob", Some("viewer"))),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    // 動画メモは 1 MB まで。
    let upload = |bytes: usize| {
        let app = app.clone();
        let alice = alice.clone();
        let id = id.clone();
        async move {
            let req = Request::builder()
                .method("POST")
                .uri(format!("/api/boards/{id}/media"))
                .header(header::COOKIE, alice)
                .header(header::CONTENT_TYPE, "video/webm")
                .body(Body::from(vec![0u8; bytes]))
                .unwrap();
            app.oneshot(req).await.unwrap().status()
        }
    };
    assert_eq!(upload(700 * 1024).await, StatusCode::OK);
    assert_eq!(upload(400 * 1024).await, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(upload(300 * 1024).await, StatusCode::OK);
}

/// 偽の Claude API(何を聞かれても同じ要約を返す)。
async fn fake_claude() -> String {
    let app = Router::new().route(
        "/v1/messages",
        post(|| async {
            Json(json!({
                "type": "message", "stop_reason": "end_turn",
                "content": [{ "type": "text", "text": "{\"summary\":\"まとめ\"}" }],
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn seed(hub: &Hub, room: &str) -> board_server::room::Membership {
    let m = hub.join(room);
    let mut b = Board::new();
    b.apply_remote(&m.room.snapshot()).unwrap();
    let id = b.alloc_id();
    let mut s = Shape::sticky(id, Vec2::new(0.0, 0.0), Rgba8::WHITE);
    s.text = "付箋".into();
    b.push(s);
    for u in b.take_outgoing() {
        m.room.apply(&u).unwrap();
    }
    m
}

#[tokio::test]
async fn ai_calls_are_counted_per_month() {
    let url = fake_claude().await;
    let mut cfg = config(Mode::Mock, 3);
    cfg.ai = Some(Arc::new(Ai::new(AiConfig {
        api_key: "k".into(),
        model: "claude-opus-5-5".into(),
        base_url: url,
    })));
    let (app, hub) = app_with(cfg);
    let _a = seed(&hub, "room-a");
    let _b = seed(&hub, "room-b");
    let _c = seed(&hub, "room-c");

    // オーナーのいないボードは、ログインしていないと使えない。
    let r = call(
        &app,
        "POST",
        "/api/boards/room-a/ai/summarize",
        None,
        &[],
        Some("{}".into()),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);

    let alice = signup(&app, "alice").await;
    let r = call(
        &app,
        "POST",
        "/api/boards/room-b/ai/summarize",
        Some(&alice),
        &[],
        Some("{}".into()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // 別のボードでも、同じ人の回数として数える。
    let r = call(
        &app,
        "POST",
        "/api/boards/room-c/ai/summarize",
        Some(&alice),
        &[],
        Some("{}".into()),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYMENT_REQUIRED, "{}", r.text);
    assert!(r.json["error"].as_str().unwrap().contains("1 か月 1 回"));
    let r = call(&app, "GET", "/api/billing", Some(&alice), &[], None).await;
    assert_eq!(r.json["usage"]["ai_this_month"], 1);
}

type Seen = Arc<Mutex<Vec<(String, HeaderMap, String)>>>;

/// 偽の Stripe API。
async fn fake_stripe() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let s1 = seen.clone();
    let s2 = seen.clone();
    let app = Router::new()
        .route(
            "/v1/checkout/sessions",
            post(move |headers: HeaderMap, body: String| {
                let seen = s1.clone();
                async move {
                    seen.lock().unwrap().push(("checkout".into(), headers, body));
                    Json(json!({ "id": "cs_test_1", "url": "https://checkout.stripe.test/c/cs_test_1" }))
                }
            }),
        )
        .route(
            "/v1/billing_portal/sessions",
            post(move |headers: HeaderMap, body: String| {
                let seen = s2.clone();
                async move {
                    seen.lock().unwrap().push(("portal".into(), headers, body));
                    Json(json!({ "url": "https://billing.stripe.test/p/1" }))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn webhook(app: &Router, event: &Value, secret: &str) -> Resp {
    let body = event.to_string();
    let sig = sign(body.as_bytes(), secret, now_secs());
    call(
        app,
        "POST",
        "/api/billing/webhook",
        None,
        &[("stripe-signature", sig)],
        Some(body),
    )
    .await
}

#[tokio::test]
async fn stripe_checkout_webhook_and_portal() {
    let (api, seen) = fake_stripe().await;
    let mut cfg = config(
        Mode::Stripe(StripeConfig {
            secret_key: "sk_test_123".into(),
            webhook_secret: "whsec_abc".into(),
            price_pro: "price_pro_1".into(),
            api_base: api,
        }),
        1,
    );
    cfg.billing.public_url = Some("https://board.example.com".into());
    let (app, _) = app_with(cfg);
    let alice = signup(&app, "alice").await;

    // 支払いの記録がなければポータルは開けない。
    let r = call(&app, "POST", "/api/billing/portal", Some(&alice), &[], None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = call(
        &app,
        "POST",
        "/api/billing/checkout",
        Some(&alice),
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["url"], "https://checkout.stripe.test/c/cs_test_1");
    {
        let seen = seen.lock().unwrap();
        let (_, headers, body) = &seen[0];
        assert_eq!(headers["authorization"], "Bearer sk_test_123");
        assert!(body.contains("mode=subscription"));
        assert!(body.contains("line_items%5B0%5D%5Bprice%5D=price_pro_1"));
        assert!(body.contains("client_reference_id=alice"));
        assert!(body.contains("subscription_data%5Bmetadata%5D%5Blogin%5D=alice"));
        assert!(
            body.contains("success_url=https%3A%2F%2Fboard.example.com%2F%3Fbilling%3Dsuccess")
        );
    }
    // まだ申し込みの途中なので無料のまま。
    let r = call(&app, "GET", "/api/billing", Some(&alice), &[], None).await;
    assert_eq!(r.json["plan"], "free");

    let completed = json!({
        "id": "evt_1", "type": "checkout.session.completed",
        "data": { "object": {
            "mode": "subscription", "client_reference_id": "alice",
            "customer": "cus_A", "subscription": "sub_A",
        }},
    });
    // 署名が違えば断る。
    let r = webhook(&app, &completed, "whsec_wrong").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &app,
        "POST",
        "/api/billing/webhook",
        None,
        &[],
        Some(completed.to_string()),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        call(&app, "GET", "/api/billing", Some(&alice), &[], None)
            .await
            .json["plan"],
        "free"
    );

    let r = webhook(&app, &completed, "whsec_abc").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = call(&app, "GET", "/api/billing", Some(&alice), &[], None).await;
    assert_eq!(r.json["plan"], "pro");
    assert_eq!(r.json["can_manage"], true);
    assert_eq!(
        create_board(&app, &alice, "1").await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        create_board(&app, &alice, "2").await.status,
        StatusCode::CREATED
    );

    let r = call(&app, "POST", "/api/billing/portal", Some(&alice), &[], None).await;
    assert_eq!(r.json["url"], "https://billing.stripe.test/p/1");
    {
        let seen = seen.lock().unwrap();
        let (kind, _, body) = &seen[1];
        assert_eq!(kind, "portal");
        assert!(body.contains("customer=cus_A"));
    }

    // 解約(サブスクリプションの削除)で無料に戻る。
    let deleted = json!({
        "id": "evt_2", "type": "customer.subscription.deleted",
        "data": { "object": { "id": "sub_A", "customer": "cus_A", "status": "canceled" }},
    });
    assert_eq!(
        webhook(&app, &deleted, "whsec_abc").await.status,
        StatusCode::OK
    );
    let r = call(&app, "GET", "/api/billing", Some(&alice), &[], None).await;
    assert_eq!(r.json["plan"], "free");
    assert_eq!(r.json["status"], "canceled");

    // 同じイベントが届き直しても、二度は処理しない(古い完了通知でプロに戻らない)。
    assert_eq!(
        webhook(&app, &completed, "whsec_abc").await.status,
        StatusCode::OK
    );
    let r = call(&app, "GET", "/api/billing", Some(&alice), &[], None).await;
    assert_eq!(r.json["plan"], "free");
}
