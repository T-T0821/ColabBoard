//! WASM プラグインのサンドボックスと API を確かめる(テスト用のプラグインは WAT で書く)。

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use board_server::plugins::{Limits, PluginConfig, PluginHost};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// WAT の文字列リテラルにする。
fn lit(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'"' | b'\\' => format!("\\{:02x}", b),
            0x20..=0x7e => (b as char).to_string(),
            _ => format!("\\{:02x}", b),
        })
        .collect()
}

/// テスト用のプラグイン。`body` は gb_run の中身(呼び出しの JSON は 1024 番地に置く)。
fn plugin(manifest: Value, call: Option<Value>, body: &str, extra_imports: &str) -> Vec<u8> {
    let m = manifest.to_string();
    let c = call.map(|c| c.to_string()).unwrap_or_default();
    let wat = format!(
        r#"(module
  (import "graboard" "call" (func $call (param i32 i32) (result i32)))
  (import "graboard" "response" (func $resp (param i32)))
  {extra_imports}
  (memory (export "memory") 2)
  (global $heap (mut i32) (i32.const 65536))
  (func (export "gb_alloc") (param $n i32) (result i32)
    (local $p i32)
    global.get $heap
    local.set $p
    global.get $heap
    local.get $n
    i32.add
    global.set $heap
    local.get $p)
  (data (i32.const 0) "{m}")
  (data (i32.const 1024) "{c}")
  (func (export "gb_manifest") (result i64)
    i64.const {mlen})
  (func (export "gb_run") (param i32 i32) (result i64)
    (local $n i32)
    {body})
)"#,
        m = lit(&m),
        c = lit(&c),
        mlen = m.len(),
    );
    wat::parse_str(&wat).unwrap()
}

/// 呼び出しをして、ホストの応答をそのまま出力として返す本体。
fn echo_call(len: usize) -> String {
    format!(
        "(local.set $n (call $call (i32.const 1024) (i32.const {len})))
    (call $resp (i32.const 16384))
    (i64.or (i64.shl (i64.const 16384) (i64.const 32)) (i64.extend_i32_u (local.get $n)))"
    )
}

fn manifest(id: &str, perms: &[&str]) -> Value {
    json!({"id": id, "name": format!("テスト {id}"), "permissions": perms,
           "commands": [{"id": "go", "title": "実行"}]})
}

fn sticky_plugin(id: &str, perms: &[&str]) -> Vec<u8> {
    let call = json!({"fn": "apply", "ops": [
        {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 1, "y": 2, "text": "プラグインから"}}
    ]});
    let len = call.to_string().len();
    plugin(manifest(id, perms), Some(call), &echo_call(len), "")
}

struct Server {
    app: Router,
}

impl Server {
    fn new(plugins: PluginConfig) -> Self {
        let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
        Self {
            app: router(
                hub,
                Config {
                    plugins,
                    ..Config::default()
                },
            ),
        }
    }

    async fn raw(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        ctype: &str,
        body: Vec<u8>,
    ) -> (StatusCode, Value, Option<String>) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, ctype);
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        let res = self
            .app
            .clone()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
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

    async fn json(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let (s, v, _) = self
            .raw(
                method,
                path,
                cookie,
                "application/json",
                body.to_string().into_bytes(),
            )
            .await;
        (s, v)
    }

    async fn signup(&self, login: &str) -> String {
        let body = json!({"login": login, "name": login, "password": "password123"});
        self.raw(
            "POST",
            "/api/signup",
            None,
            "application/json",
            body.to_string().into_bytes(),
        )
        .await
        .2
        .unwrap()
    }

    async fn install(&self, cookie: &str, wasm: Vec<u8>) -> (StatusCode, Value) {
        let (s, v, _) = self
            .raw(
                "POST",
                "/api/v1/plugins",
                Some(cookie),
                "application/wasm",
                wasm,
            )
            .await;
        (s, v)
    }

    async fn count(&self, board: &str, cookie: &str) -> usize {
        let (_, v) = self
            .json(
                "GET",
                &format!("/api/v1/boards/{board}/elements"),
                Some(cookie),
                Value::Null,
            )
            .await;
        v.as_array().unwrap().len()
    }
}

fn small_limits() -> PluginConfig {
    PluginConfig {
        host: Arc::new(PluginHost::new(Limits {
            fuel: 50_000_000,
            memory_bytes: 4 * 1024 * 1024,
            ..Limits::default()
        })),
        ..PluginConfig::default()
    }
}

#[tokio::test]
async fn install_run_and_permissions() {
    let s = Server::new(small_limits());
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let (_, b) = s
        .json("POST", "/api/boards", Some(&alice), json!({"title": "p"}))
        .await;
    let board = b["id"].as_str().unwrap().to_string();

    let (status, info) = s
        .install(
            &alice,
            sticky_plugin("sticky", &["board:read", "board:write"]),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{info}");
    assert_eq!(info["id"], "sticky");
    assert_eq!(info["source"], "user");
    let (_, list) = s
        .json("GET", "/api/v1/plugins", Some(&alice), Value::Null)
        .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    // 他の人には見えない。
    let (_, list) = s
        .json("GET", "/api/v1/plugins", Some(&bob), Value::Null)
        .await;
    assert!(list.as_array().unwrap().is_empty());

    let before = s.count(&board, &alice).await;
    let run = format!("/api/v1/boards/{board}/plugins/sticky/run");
    let (status, r) = s
        .json("POST", &run, Some(&alice), json!({"command": "go"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["ops"], 1);
    assert!(r["fuel_used"].as_u64().unwrap() > 0);
    assert_eq!(s.count(&board, &alice).await, before + 1);
    let (_, found) = s
        .json(
            "GET",
            &format!("/api/v1/boards/{board}/elements?text=プラグイン"),
            Some(&alice),
            Value::Null,
        )
        .await;
    assert_eq!(found.as_array().unwrap().len(), 1);

    // ないコマンド。
    let (status, _) = s
        .json("POST", &run, Some(&alice), json!({"command": "nope"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 閲覧者は編集するプラグインを実行できない。
    s.json(
        "PUT",
        &format!("/api/boards/{board}/members"),
        Some(&alice),
        json!({"login": "bob", "role": "viewer"}),
    )
    .await;
    s.install(
        &bob,
        sticky_plugin("sticky", &["board:read", "board:write"]),
    )
    .await;
    let (status, _) = s
        .json("POST", &run, Some(&bob), json!({"command": "go"}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 権限を求めていないプラグインは書けない(何も変わらない)。
    s.install(&alice, sticky_plugin("sneaky", &["board:read"]))
        .await;
    let (status, err) = s
        .json(
            "POST",
            &format!("/api/v1/boards/{board}/plugins/sneaky/run"),
            Some(&alice),
            json!({"command": "go"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("権限"), "{err}");
    assert_eq!(s.count(&board, &alice).await, before + 1);

    // 消す。
    let (status, _) = s
        .json(
            "DELETE",
            "/api/v1/plugins/sneaky",
            Some(&alice),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = s
        .json(
            "POST",
            &format!("/api/v1/boards/{board}/plugins/sneaky/run"),
            Some(&alice),
            json!({"command": "go"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sandbox_limits() {
    let s = Server::new(small_limits());
    let alice = s.signup("alice").await;
    let (_, b) = s
        .json("POST", "/api/boards", Some(&alice), json!({"title": "p"}))
        .await;
    let board = b["id"].as_str().unwrap().to_string();
    let run = |id: &str| format!("/api/v1/boards/{board}/plugins/{id}/run");

    // 無限ループは燃料切れで止まり、それまでの書き込みも残らない。
    let call = json!({"fn": "apply", "ops": [{"op": "create", "element": {"type": "shape"}}]});
    let len = call.to_string().len();
    let looping = plugin(
        manifest("loop", &["board:write"]),
        Some(call),
        &format!(
            "(drop (call $call (i32.const 1024) (i32.const {len}))) (loop $l (br $l)) (i64.const 0)"
        ),
        "",
    );
    assert_eq!(s.install(&alice, looping).await.0, StatusCode::CREATED);
    let before = s.count(&board, &alice).await;
    let (status, err) = s
        .json("POST", &run("loop"), Some(&alice), json!({"command": "go"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("命令数"), "{err}");
    assert_eq!(s.count(&board, &alice).await, before);

    // メモリを使いすぎると止まる。
    let hog = plugin(
        manifest("hog", &[]),
        None,
        "(drop (memory.grow (i32.const 1000))) (i64.const 0)",
        "",
    );
    s.install(&alice, hog).await;
    let (status, err) = s
        .json("POST", &run("hog"), Some(&alice), json!({"command": "go"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("メモリ"), "{err}");

    // 範囲外を指す出力。
    let oob = plugin(
        manifest("oob", &[]),
        None,
        "(i64.const 0x7fffffff00000010)",
        "",
    );
    s.install(&alice, oob).await;
    let (status, _) = s
        .json("POST", &run("oob"), Some(&alice), json!({"command": "go"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // WASI など、渡していない関数を使うモジュールは入れられない。
    let wasi = plugin(
        manifest("wasi", &[]),
        None,
        "(i64.const 0)",
        r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#,
    );
    let (status, err) = s.install(&alice, wasi).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        err["error"].as_str().unwrap().contains("graboard.call"),
        "{err}"
    );

    // 壊れたモジュール・おかしなマニフェスト。
    assert_eq!(
        s.install(&alice, b"not wasm".to_vec()).await.0,
        StatusCode::BAD_REQUEST
    );
    let bad = plugin(
        json!({"id": "Bad", "name": "x", "commands": []}),
        None,
        "(i64.const 0)",
        "",
    );
    assert_eq!(s.install(&alice, bad).await.0, StatusCode::BAD_REQUEST);

    // プラグイン自身が返したエラー。
    let err_out = r#"{"err":"入力が足りません"}"#;
    let failing = plugin(
        manifest("fail", &[]),
        Some(serde_json::from_str(err_out).unwrap()),
        &format!(
            "(i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const {}))",
            err_out.len()
        ),
        "",
    );
    s.install(&alice, failing).await;
    let (status, err) = s
        .json("POST", &run("fail"), Some(&alice), json!({"command": "go"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        err["error"].as_str().unwrap().contains("入力が足りません"),
        "{err}"
    );
}

#[tokio::test]
async fn server_plugins_are_shared() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.wasm"),
        sticky_plugin("shared", &["board:read", "board:write"]),
    )
    .unwrap();
    std::fs::write(dir.path().join("broken.wasm"), b"junk").unwrap();
    let mut config = small_limits();
    config.server = config.host.load_dir(dir.path());
    config.user_plugins = false;
    assert_eq!(config.server.len(), 1);
    let s = Server::new(config);
    let alice = s.signup("alice").await;
    let (_, list) = s
        .json("GET", "/api/v1/plugins", Some(&alice), Value::Null)
        .await;
    assert_eq!(list[0]["source"], "server");
    // 誰でも編集できるボードでも、ログインなしで実行できる。
    let (status, r) = s
        .json(
            "POST",
            "/api/v1/boards/open-room/plugins/shared/run",
            None,
            json!({"command": "go"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    // 自分で入れるのは禁止されている。
    let (status, _) = s.install(&alice, sticky_plugin("mine", &[])).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
