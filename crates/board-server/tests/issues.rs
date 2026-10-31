//! 課題カード用の API。本物の GitHub・Jira の代わりに偽の API を立てて確かめる。

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::get;
use axum::{Json, Router};
use board_core::api::{IssueLookup, IssueState};
use board_server::issues::{IssuesConfig, JiraConfig};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// 偽の GitHub と Jira(同じサーバーに両方の道を作る)。
async fn fake_api() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let s1 = seen.clone();
    let s2 = seen.clone();
    let app = Router::new()
        .route(
            "/repos/{owner}/{repo}/issues/{n}",
            get(move |Path((o, r, n)): Path<(String, String, u64)>, h: HeaderMap| {
                let s = s1.clone();
                async move {
                    let auth = h
                        .get(header::AUTHORIZATION)
                        .map(|v| v.to_str().unwrap().to_string());
                    s.lock().unwrap().push((format!("gh {o}/{r}#{n}"), auth));
                    match n {
                        1 => Ok(Json(json!({"title": "ログインできない", "state": "open", "assignee": {"login": "alice"}}))),
                        2 => Ok(Json(json!({"title": "速くする", "state": "closed", "pull_request": {"merged_at": "2026-10-01T00:00:00Z"}}))),
                        _ => Err(StatusCode::NOT_FOUND),
                    }
                }
            }),
        )
        .route(
            "/rest/api/3/issue/{key}",
            get(move |Path(key): Path<String>, h: HeaderMap| {
                let s = s2.clone();
                async move {
                    let auth = h
                        .get(header::AUTHORIZATION)
                        .map(|v| v.to_str().unwrap().to_string());
                    s.lock().unwrap().push((format!("jira {key}"), auth));
                    Json(json!({"fields": {"summary": "画面を直す", "status": {"name": "進行中", "statusCategory": {"key": "indeterminate"}}, "assignee": {"displayName": "Bob"}}}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

async fn post(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
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
async fn looks_up_github_and_jira() {
    let (base, seen) = fake_api().await;
    let config = Config {
        issues: IssuesConfig::new(
            base.clone(),
            Some("gh-token".into()),
            Some(JiraConfig {
                base_url: "https://example.atlassian.net".into(),
                api_url: Some(base.clone()),
                email: "me@example.com".into(),
                token: "jira-token".into(),
            }),
        ),
        ..Config::default()
    };
    let app = router(Arc::new(Hub::new(Arc::new(Store::memory()))), config);

    let urls = [
        "https://github.com/acme/web/issues/1",
        "https://github.com/acme/web/pull/2",
        "https://github.com/acme/web/issues/3",
        "https://other.atlassian.net/browse/X-1",
        "https://example.atlassian.net/browse/PROJ-7",
        "https://example.com/not-an-issue",
    ];
    let (status, v) = post(&app, "/api/boards/room1/issues", json!({ "urls": urls })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let got: Vec<IssueLookup> = serde_json::from_value(v).unwrap();
    let find = |u: &str| got.iter().find(|l| l.url == u).unwrap();

    let one = find(urls[0]).issue.as_ref().unwrap();
    assert_eq!(one.key, "acme/web#1");
    assert_eq!(one.state, IssueState::Open);
    assert_eq!(one.assignee.as_deref(), Some("alice"));
    let two = find(urls[1]).issue.as_ref().unwrap();
    assert!(two.pull);
    assert_eq!(
        (two.state, two.status.as_str()),
        (IssueState::Done, "Merged")
    );
    assert_eq!(find(urls[2]).error.as_deref(), Some("見つかりません"));
    // 設定していない Jira サイトには問い合わせない。
    assert!(
        find(urls[3])
            .error
            .as_ref()
            .unwrap()
            .contains("問い合わせられません")
    );
    let jira = find(urls[4]).issue.as_ref().unwrap();
    assert_eq!(jira.key, "PROJ-7");
    assert_eq!(jira.state, IssueState::InProgress);
    assert_eq!(jira.assignee.as_deref(), Some("Bob"));
    assert!(find(urls[5]).error.is_some());

    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4, "{seen:?}");
        for (what, auth) in seen.iter() {
            if what.starts_with("gh ") {
                assert_eq!(auth.as_deref(), Some("Bearer gh-token"));
            } else {
                // me@example.com:jira-token の Base64。
                assert_eq!(
                    auth.as_deref(),
                    Some("Basic bWVAZXhhbXBsZS5jb206amlyYS10b2tlbg==")
                );
            }
        }
    }

    // 同じ URL は少しの間は問い合わせ直さない。
    post(
        &app,
        "/api/boards/room1/issues",
        json!({ "urls": [urls[0]] }),
    )
    .await;
    assert_eq!(seen.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn needs_edit_permission() {
    let app = router(
        Arc::new(Hub::new(Arc::new(Store::memory()))),
        Config {
            anonymous_boards: false,
            ..Config::default()
        },
    );
    let (status, _) = post(
        &app,
        "/api/boards/room1/issues",
        json!({ "urls": ["https://github.com/a/b/issues/1"] }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
