//! 生成 AI による図の作成を、Claude API の偽サーバーで確かめる。

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::post;
use board_server::ai::{Ai, AiConfig};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// 受け取ったリクエストを記録し、決まった図を返す偽の Messages API。
async fn fake_claude(seen: Arc<Mutex<Vec<(HeaderMap, Value)>>>) -> String {
    let app = Router::new().route(
        "/v1/messages",
        post(
            move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push((headers, body));
                    let spec = json!({
                        "title": "t", "direction": "right",
                        "groups": [{"id": "vpc", "kind": "vpc", "label": "", "parent": ""}],
                        "nodes": [
                            {"id": "alb", "kind": "alb", "label": "", "group": "vpc"},
                            {"id": "web", "kind": "ec2", "label": "Web", "group": "vpc"}
                        ],
                        "edges": [{"from": "alb", "to": "web", "label": ""}]
                    });
                    axum::Json(json!({
                        "type": "message",
                        "stop_reason": "end_turn",
                        "content": [{"type": "text", "text": spec.to_string()}]
                    }))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder().method(method).uri(path);
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn ai(key: &str, base_url: String) -> Arc<Ai> {
    Arc::new(Ai::new(AiConfig {
        api_key: key.into(),
        model: "claude-opus-5-5".into(),
        base_url,
    }))
}

fn app(ai: Option<Arc<Ai>>) -> Router {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    router(
        hub,
        Config {
            ai,
            ..Config::default()
        },
    )
}

#[tokio::test]
async fn generates_diagram_through_claude_api() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let base = fake_claude(seen.clone()).await;
    let app = app(Some(ai("test-key", base)));

    let (status, body) = call(&app, "GET", "/api/ai/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);

    let (status, body) = call(
        &app,
        "POST",
        "/api/boards/free-room/ai/diagram",
        Some(json!({"prompt": "VPC の中に ALB と EC2", "mode": "aws"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["nodes"].as_array().unwrap().len(), 2);
    // 空文字の所属は「なし」になる。
    assert_eq!(body["groups"][0]["parent"], Value::Null);

    let seen = seen.lock().unwrap();
    let (headers, req) = &seen[0];
    assert_eq!(headers["x-api-key"], "test-key");
    assert!(headers.contains_key("anthropic-version"));
    assert_eq!(req["model"], "claude-opus-5-5");
    assert!(
        req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("ALB")
    );
}

#[tokio::test]
async fn rejects_when_disabled_or_invalid() {
    let app_off = app(None);
    let (status, body) = call(&app_off, "GET", "/api/ai/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    let (status, _) = call(
        &app_off,
        "POST",
        "/api/boards/r/ai/diagram",
        Some(json!({"prompt": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let base = fake_claude(seen.clone()).await;
    let app = app(Some(ai("k", base)));
    let (status, _) = call(
        &app,
        "POST",
        "/api/boards/r/ai/diagram",
        Some(json!({"prompt": "   "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // 存在しない・不正なボードには使えない。
    let (status, _) = call(
        &app,
        "POST",
        "/api/boards/bad_room!/ai/diagram",
        Some(json!({"prompt": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(seen.lock().unwrap().is_empty());
}
