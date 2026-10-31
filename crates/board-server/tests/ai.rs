//! AI による付箋の要約・クラスタリング。Claude API の代わりに偽のサーバーを立てて確かめる。

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::post;
use axum::{Json, Router};
use board_core::{Board, ElementId, Rgba8, Shape, Vec2};
use board_server::ai::{Ai, AiConfig};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

type Seen = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

/// 偽の Messages API。届いたリクエストを記録し、`reply` を構造化出力として返す。
async fn fake_claude(reply: Value) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let seen2 = seen.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let seen = seen2.clone();
            let reply = reply.clone();
            async move {
                seen.lock().unwrap().push((headers, body));
                Json(json!({
                    "type": "message",
                    "stop_reason": "end_turn",
                    "content": [
                        { "type": "thinking", "thinking": "" },
                        { "type": "text", "text": reply.to_string() },
                    ],
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn ai(base_url: &str) -> Option<Arc<Ai>> {
    Some(Arc::new(Ai::new(AiConfig {
        api_key: "test-key".into(),
        model: "claude-opus-5-5".into(),
        base_url: base_url.into(),
    })))
}

/// 付箋 3 枚のボードを `room` に読み込む。戻り値は付箋の ID(16 進)。
fn seed(hub: &Hub, room: &str) -> (board_server::room::Membership, Vec<String>) {
    let m = hub.join(room);
    let mut b = Board::new();
    b.apply_remote(&m.room.snapshot()).unwrap();
    let mut ids = Vec::new();
    for (i, text) in ["もっと速く", "安くしたい", "速度が大事"]
        .iter()
        .enumerate()
    {
        let id = b.alloc_id();
        let mut s = Shape::sticky(id, Vec2::new(i as f32 * 300.0, 1000.0), Rgba8::WHITE);
        s.text = text.to_string();
        b.push(s);
        ids.push(hex(id));
    }
    for u in b.take_outgoing() {
        m.room.apply(&u).unwrap();
    }
    (m, ids)
}

fn hex(id: ElementId) -> String {
    format!("{:x}", id.0)
}

async fn post_json(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn cluster_validates_ids_and_calls_claude() {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let (_m, ids) = seed(&hub, "ai-room");
    let (url, seen) = fake_claude(json!({
        "clusters": [
            { "title": "速さ", "ids": [ids[0], ids[2], "deadbeef"] },
            { "title": "重複", "ids": [ids[0]] },
        ]
    }))
    .await;
    let app = router(
        hub.clone(),
        Config {
            ai: ai(&url),
            ..Config::default()
        },
    );

    // 新しいボードには案内の付箋もあるので、対象を指定する。
    let (status, body) = post_json(
        &app,
        "/api/boards/ai-room/ai/cluster",
        json!({ "ids": ids }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["clusters"],
        json!([
            { "title": "速さ", "ids": [ids[0], ids[2]] },
            { "title": "その他", "ids": [ids[1]] },
        ])
    );

    let seen = seen.lock().unwrap();
    let (headers, req) = &seen[0];
    assert_eq!(headers["x-api-key"], "test-key");
    assert_eq!(headers["anthropic-version"], "2026-10-21");
    assert_eq!(headers["anthropic-beta"], "server-side-fallback-2026-07-01");
    assert_eq!(req["model"], "claude-opus-5-5");
    assert_eq!(req["fallbacks"], "default");
    assert_eq!(req["output_config"]["format"]["type"], "json_schema");
    let prompt = req["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains(&format!("<note id=\"{}\">安くしたい</note>", ids[1])));
}

#[tokio::test]
async fn summarize_selected_notes_with_cooldown() {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let (_m, ids) = seed(&hub, "sum-room");
    let (url, seen) = fake_claude(json!({ "summary": "速さが論点\n・速度を上げたい" })).await;
    let app = router(
        hub.clone(),
        Config {
            ai: ai(&url),
            ..Config::default()
        },
    );
    let path = "/api/boards/sum-room/ai/summarize";
    let (status, body) = post_json(&app, path, json!({ "ids": [ids[0], ids[2]] })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["summary"], "速さが論点\n・速度を上げたい");
    assert_eq!(body["count"], 2);
    let prompt = seen.lock().unwrap()[0].1["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!prompt.contains("安くしたい"), "選んでいない付箋を送った");

    // 続けて呼ぶと断る。
    let (status, _) = post_json(&app, path, json!({})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn errors_without_key_or_room() {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let (_m, _) = seed(&hub, "loaded");
    let app = router(hub.clone(), Config::default());
    let (status, body) = post_json(&app, "/api/boards/loaded/ai/summarize", json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("ANTHROPIC_API_KEY")
    );

    let (url, seen) = fake_claude(json!({})).await;
    let app = router(
        hub.clone(),
        Config {
            ai: ai(&url),
            ..Config::default()
        },
    );
    // 誰も開いていないボード。
    let (status, _) = post_json(&app, "/api/boards/empty-room/ai/summarize", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 不正なボード名。
    let (status, _) = post_json(&app, "/api/boards/bad.name/ai/summarize", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ink_sends_image_and_validates_items() {
    use board_core::Stroke;
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let m = hub.join("ink-room");
    let mut b = Board::new();
    b.apply_remote(&m.room.snapshot()).unwrap();
    let mut ids = Vec::new();
    for y in [0.0, 300.0] {
        let id = b.alloc_id();
        let mut s = Stroke::new(id, Vec2::new(0.0, y), Rgba8::BLACK, 4.0);
        s.points.push(Vec2::new(200.0, y + 100.0));
        b.push(s);
        ids.push(hex(id));
    }
    for u in b.take_outgoing() {
        m.room.apply(&u).unwrap();
    }
    let (url, seen) = fake_claude(json!({
        "items": [
            { "kind": "rect", "strokes": [ids[0], "deadbeef"], "text": "" },
            { "kind": "text", "strokes": [ids[1]], "text": "受付" },
            { "kind": "frame", "strokes": [ids[1]], "text": "" },
        ]
    }))
    .await;
    let app = router(
        hub.clone(),
        Config {
            ai: ai(&url),
            ..Config::default()
        },
    );
    let (status, body) =
        post_json(&app, "/api/boards/ink-room/ai/ink", json!({ "ids": ids })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["items"],
        json!([
            { "kind": "rect", "strokes": [ids[0]], "text": "" },
            { "kind": "text", "strokes": [ids[1]], "text": "受付" },
        ])
    );
    let seen = seen.lock().unwrap();
    let content = &seen[0].1["messages"][0]["content"];
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    assert!(content[1]["text"].as_str().unwrap().contains(&ids[1]));
    drop(seen);

    // 線を選んでいなければ Claude を呼ばずに断る。
    let (status, _) = post_json(&app, "/api/boards/ink-room/ai/ink", json!({ "ids": [] })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
