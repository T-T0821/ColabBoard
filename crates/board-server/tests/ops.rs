//! 運用の API: バックアップ(`/api/admin/backups`)。

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use board_server::backup::{BackupConfig, Backups};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Env {
    app: Router,
    _dir: tempfile::TempDir,
}

fn start() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::memory());
    let hub = Arc::new(Hub::new(store));
    let backup = BackupConfig::from_env(Some(dir.path()), |_| None)
        .unwrap()
        .unwrap();
    let config = Config {
        admins: vec!["admin".into()],
        backups: Some(Arc::new(Backups::new(backup, hub.clone()))),
        ..Config::default()
    };
    Env {
        app: router(hub, config),
        _dir: dir,
    }
}

impl Env {
    async fn raw(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        headers: &[(&str, &str)],
        body: Body,
    ) -> (StatusCode, Vec<u8>, Option<String>) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = self
            .app
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let cookie = res
            .headers()
            .get(header::SET_COOKIE)
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
        let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, bytes, cookie)
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (headers, body): (&[(&str, &str)], Body) = match body {
            Some(b) => (
                &[("content-type", "application/json")],
                Body::from(b.to_string()),
            ),
            None => (&[], Body::empty()),
        };
        let (s, bytes, _) = self.raw(method, path, cookie, headers, body).await;
        (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn signup(&self, login: &str) -> String {
        let body = json!({"login": login, "name": "", "password": "password123"});
        let (s, _, c) = self
            .raw(
                "POST",
                "/api/signup",
                None,
                &[("content-type", "application/json")],
                Body::from(body.to_string()),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        c.unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_board_comes_back_from_backup() {
    let e = start();
    let admin = e.signup("admin").await;
    let alice = e.signup("alice").await;
    let (s, board) = e
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "大事なボード"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let id = board["id"].as_str().unwrap().to_string();

    let (s, _) = e
        .call("POST", "/api/admin/backups", Some(&alice), None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, info) = e
        .call("POST", "/api/admin/backups", Some(&admin), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{info}");
    assert_eq!(info["trigger"], "manual");
    assert_eq!(info["by"], "admin");
    assert_eq!(info["boards"], 1);
    let bid = info["id"].as_u64().unwrap();

    let (_, st) = e
        .call("GET", "/api/admin/backups", Some(&admin), None)
        .await;
    assert_eq!(st["enabled"], true);
    assert_eq!(st["interval_hours"], 24);
    assert_eq!(st["backups"][0]["id"], bid);

    let (s, bytes, _) = e
        .raw(
            "GET",
            &format!("/api/admin/backups/{bid}/download"),
            Some(&admin),
            &[],
            Body::empty(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(&bytes[..2], &[0x1f, 0x8b], "gzip");

    // 消してしまう。
    let (s, _) = e
        .call("DELETE", &format!("/api/boards/{id}"), Some(&alice), None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = e.call("GET", "/api/boards", Some(&alice), None).await;
    assert_eq!(list.as_array().unwrap().len(), 0);

    let (s, boards) = e
        .call(
            "GET",
            &format!("/api/admin/backups/{bid}/boards"),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(boards[0]["id"], id.as_str());
    assert_eq!(boards[0]["title"], "大事なボード");
    assert_eq!(boards[0]["current"], "deleted");

    let (s, r) = e
        .call(
            "POST",
            &format!("/api/admin/backups/{bid}/boards/{id}/restore"),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["recreated"], true);
    let (_, list) = e.call("GET", "/api/boards", Some(&alice), None).await;
    assert_eq!(list[0]["id"], id.as_str());

    // 今あるボードなら中身だけを戻す。
    let (s, r) = e
        .call(
            "POST",
            &format!("/api/admin/backups/{bid}/boards/{id}/restore"),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["recreated"], false);
    // 一度も開いていないボードには中身がないので、戻す前のバージョンもない(中身ありはユニットテストで確かめる)。
    assert!(r["previous"].is_null());

    let (s, _) = e
        .call(
            "POST",
            &format!("/api/admin/backups/{bid}/boards/nothing/restore"),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = e
        .call("GET", "/api/admin/backups/1/boards", Some(&admin), None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (_, audit) = e
        .call("GET", "/api/audit?action=backup", Some(&admin), None)
        .await;
    let actions: Vec<&str> = audit
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        vec!["backup.restore", "backup.restore", "backup.create"]
    );
}

#[tokio::test]
async fn backups_disabled_without_location() {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let e = Env {
        app: router(
            hub,
            Config {
                admins: vec!["admin".into()],
                ..Config::default()
            },
        ),
        _dir: tempfile::tempdir().unwrap(),
    };
    let admin = e.signup("admin").await;
    let (s, st) = e
        .call("GET", "/api/admin/backups", Some(&admin), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(st["enabled"], false);
    let (s, _) = e
        .call("POST", "/api/admin/backups", Some(&admin), None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
