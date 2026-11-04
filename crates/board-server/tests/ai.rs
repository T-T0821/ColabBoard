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
    let content = seen.lock().unwrap()[0].1["messages"][0]["content"].clone();
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    assert!(content[1]["text"].as_str().unwrap().contains(&ids[1]));

    // 線を選んでいなければ Claude を呼ばずに断る。
    let (status, _) = post_json(&app, "/api/boards/ink-room/ai/ink", json!({ "ids": [] })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ask_and_minutes_read_the_board() {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let (_m, ids) = seed(&hub, "ask-room");
    let (url, seen) = fake_claude(json!({
        "answer": "速さが大事という意見が 2 つあります",
        "refs": [ids[2], "deadbeef", ids[0]],
    }))
    .await;
    let app = router(
        hub.clone(),
        Config {
            ai: ai(&url),
            ..Config::default()
        },
    );
    let (status, body) = post_json(
        &app,
        "/api/boards/ask-room/ai/ask",
        json!({ "question": "何が大事?", "ids": ids, "history": [{ "q": "前", "a": "答え" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["refs"], json!([ids[2], ids[0]]));
    let prompt = seen.lock().unwrap()[0].1["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(prompt.contains("<question>何が大事?</question>"));
    assert!(prompt.contains("<q>前</q>"));
    assert!(prompt.contains("安くしたい"));

    // 空の質問は Claude を呼ばずに断る。
    let (status, _) = post_json(
        &app,
        "/api/boards/ask-room/ai/ask",
        json!({ "question": " " }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (url, _) = fake_claude(json!({
        "title": "改善会議", "summary": "速さが論点。", "topics": [],
        "decisions": ["速度を優先"], "actions": [{ "task": "計測", "owner": "", "due": "" }],
        "open_questions": [],
    }))
    .await;
    let app = router(
        hub.clone(),
        Config {
            ai: ai(&url),
            ..Config::default()
        },
    );
    let (status, body) = post_json(
        &app,
        "/api/boards/ask-room/ai/minutes",
        json!({ "ids": ids, "date": "2026-10-31<x>" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let md = body["markdown"].as_str().unwrap();
    assert!(
        md.starts_with("# 議事録: 改善会議\n\n2026-10-31 作成(AI)"),
        "{md}"
    );
    assert!(md.contains("| 計測 | — | — |"));
}

/// ルームのボードに文字起こしと議題を書き込む。
fn seed_meeting(m: &board_server::room::Membership) {
    use board_core::meeting::{AgendaItem, Meeting};
    let mut b = Board::new();
    b.apply_remote(&m.room.snapshot()).unwrap();
    b.add_transcript("ua", "佐藤", "今日は来期の方針を決めます", 1_000.0);
    b.add_transcript("ub", "鈴木", "速度改善を最優先にしたいです", 65_000.0);
    b.add_transcript("ua", "佐藤", "では計測は鈴木さんにお願いします", 125_000.0);
    let mut meeting = Meeting {
        goal: "来期の方針を決める".into(),
        agenda: vec![
            AgendaItem {
                title: "振り返り".into(),
                minutes: 10,
                goal: String::new(),
            },
            AgendaItem {
                title: "方針".into(),
                minutes: 20,
                goal: "1 つに絞る".into(),
            },
        ],
        ..Default::default()
    };
    meeting.start(1_000.0);
    meeting.advance(61_000.0);
    b.set_meeting(Some(&meeting));
    for u in b.take_outgoing() {
        m.room.apply(&u).unwrap();
    }
}

#[tokio::test]
async fn minutes_read_the_transcript() {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let (m, _ids) = seed(&hub, "live-room");
    seed_meeting(&m);
    let (url, seen) = fake_claude(json!({
        "title": "方針会議", "summary": "速度改善を優先。", "topics": [],
        "decisions": ["速度改善を最優先"],
        "actions": [{ "task": "計測", "owner": "鈴木", "due": "" }],
        "open_questions": [],
    }))
    .await;
    let app = router(
        hub.clone(),
        Config {
            ai: ai(&url),
            ..Config::default()
        },
    );
    let (status, body) = post_json(&app, "/api/boards/live-room/ai/minutes", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["lines"], 3);
    assert_eq!(body["minutes"]["actions"][0]["owner"], "鈴木");
    assert!(
        body["markdown"]
            .as_str()
            .unwrap()
            .contains("| 計測 | 鈴木 | — |")
    );
    let prompt = seen.lock().unwrap()[0].1["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(prompt.contains("<transcript>"), "{prompt}");
    assert!(
        prompt.contains(r#"<say t="01:04" who="鈴木">速度改善を最優先にしたいです</say>"#),
        "{prompt}"
    );
    assert!(prompt.contains("もっと速く"), "ボードも読む");
    let system = seen.lock().unwrap()[0].1["system"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(system.contains("<transcript>"));
}

#[tokio::test]
async fn facilitator_plans_and_checks() {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let (m, _ids) = seed(&hub, "facil-room");
    let (url, seen) = fake_claude(json!({
        "agenda": [
            { "title": "目的の確認", "minutes": 5, "goal": "" },
            { "title": "アイデア出し", "minutes": 30, "goal": "案を 10 個" },
            { "title": "まとめ", "minutes": 5, "goal": "担当を決める" },
        ],
        "tips": "最後の 5 分は必ず残す",
    }))
    .await;
    let app = router(
        hub.clone(),
        Config {
            ai: ai(&url),
            ..Config::default()
        },
    );
    // 目的がなければ Claude を呼ばずに断る。
    let (status, _) = post_json(
        &app,
        "/api/boards/facil-room/ai/facilitate",
        json!({ "mode": "plan", "goal": " ", "minutes": 30 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = post_json(
        &app,
        "/api/boards/facil-room/ai/facilitate",
        json!({ "mode": "plan", "goal": "新機能のアイデアを出す", "minutes": 30, "notes": "- 予算" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let total: u64 = body["agenda"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["minutes"].as_u64().unwrap())
        .sum();
    assert_eq!(total, 30, "分の合計を会議の長さにそろえる");
    let prompt = seen.lock().unwrap()[0].1["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(prompt.contains("<minutes>30</minutes>"), "{prompt}");
    assert!(prompt.contains("- 予算"));

    // 議題がまだなければ助言は断る。
    let (status, _) = post_json(
        &app,
        "/api/boards/facil-room/ai/facilitate",
        json!({ "mode": "check" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    seed_meeting(&m);
    let (url, seen) = fake_claude(json!({
        "focus": "速度改善を優先するか",
        "points": ["速度改善を最優先にしたい", " "],
        "open": ["期限"],
        "suggestion": "期限を決めましょう",
        "time_advice": "残り 18 分です",
        "off_track": false,
    }))
    .await;
    let app = router(
        hub.clone(),
        Config {
            ai: ai(&url),
            ..Config::default()
        },
    );
    let (status, body) = post_json(
        &app,
        "/api/boards/facil-room/ai/facilitate",
        json!({ "mode": "check", "now": 181_000.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["points"], json!(["速度改善を最優先にしたい"]));
    assert_eq!(body["suggestion"], "期限を決めましょう");
    let prompt = seen.lock().unwrap()[0].1["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        prompt.contains(
            r#"<item state="current" planned_min="20" spent_min="2" goal="1 つに絞る">方針</item>"#
        ),
        "{prompt}"
    );
    assert!(prompt.contains("計測は鈴木さんにお願いします"));
}

#[tokio::test]
async fn facilitator_plan_works_without_ai() {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let (_m, _ids) = seed(&hub, "noai-room");
    let app = router(hub.clone(), Config::default());
    let (status, body) = post_json(
        &app,
        "/api/boards/noai-room/ai/facilitate",
        json!({ "mode": "plan", "goal": "定例", "minutes": 20, "notes": "共有\n相談" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fallback"], true);
    assert_eq!(body["agenda"][0]["title"], "共有");
    assert_eq!(body["agenda"][1]["minutes"], 10);
}
