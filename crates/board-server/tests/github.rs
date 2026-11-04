//! ボードを GitHub で管理する。本物の GitHub の代わりに、Git Data API と Contents API の
//! 必要なところだけを持つ偽のサーバーを立てて確かめる。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use board_core::repo::RepoDoc;
use board_server::github::{GithubConfig, verify_signature};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const TOKEN: &str = "secret-token";

#[derive(Default)]
struct Git {
    blobs: HashMap<String, Vec<u8>>,
    /// ツリー = パス → blob。
    trees: HashMap<String, BTreeMap<String, String>>,
    /// コミット → (ツリー, 親)。
    commits: HashMap<String, (String, Option<String>)>,
    refs: HashMap<String, String>,
    pulls: Vec<Value>,
}

fn sha(kind: &str, bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(bytes);
    h.finalize()[..20]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Git {
    fn put_blob(&mut self, bytes: Vec<u8>) -> String {
        let s = sha("blob", &bytes);
        self.blobs.insert(s.clone(), bytes);
        s
    }

    fn put_tree(&mut self, files: BTreeMap<String, String>) -> String {
        let s = sha("tree", serde_json::to_string(&files).unwrap().as_bytes());
        self.trees.insert(s.clone(), files);
        s
    }

    fn put_commit(&mut self, tree: String, parent: Option<String>, msg: &str) -> String {
        let s = sha(
            "commit",
            format!("{tree}{parent:?}{msg}{}", self.commits.len()).as_bytes(),
        );
        self.commits.insert(s.clone(), (tree, parent));
        s
    }

    fn resolve(&self, r: &str) -> Option<String> {
        self.refs
            .get(r)
            .cloned()
            .or_else(|| self.commits.contains_key(r).then(|| r.to_string()))
    }

    fn files_at(&self, r: &str) -> Option<&BTreeMap<String, String>> {
        let c = self.resolve(r)?;
        self.trees.get(&self.commits[&c].0)
    }

    /// 誰かが GitHub 上でファイルを書き換えた(main にコミット)。
    fn push_file(&mut self, branch: &str, path: &str, content: &str) {
        let head = self.refs[branch].clone();
        let mut files = self.files_at(&head).unwrap().clone();
        let blob = self.put_blob(content.as_bytes().to_vec());
        files.insert(path.to_string(), blob);
        let tree = self.put_tree(files);
        let c = self.put_commit(tree, Some(head), "GitHub で編集");
        self.refs.insert(branch.to_string(), c);
    }

    fn read(&self, r: &str, path: &str) -> Option<String> {
        let blob = self.files_at(r)?.get(path)?;
        Some(String::from_utf8(self.blobs[blob].clone()).unwrap())
    }
}

type Shared = Arc<Mutex<Git>>;

fn authorized(h: &HeaderMap) -> bool {
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {TOKEN}"))
}

fn not_found() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"})))
}

type Res = (StatusCode, Json<Value>);

async fn fake_github() -> (String, Shared) {
    let git: Shared = Arc::default();
    {
        let mut g = git.lock().unwrap();
        let readme = g.put_blob(b"# plans\n".to_vec());
        let tree = g.put_tree(BTreeMap::from([("README.md".to_string(), readme)]));
        let c = g.put_commit(tree, None, "init");
        g.refs.insert("main".into(), c);
    }
    let app = Router::new()
        .route(
            "/repos/{o}/{r}",
            get(|h: HeaderMap, Path((o, r)): Path<(String, String)>| async move {
                if !authorized(&h) || o != "octo" || r != "plans" {
                    return not_found();
                }
                (StatusCode::OK, Json(json!({"full_name": "octo/plans", "default_branch": "main"})))
            }),
        )
        .route(
            "/repos/{o}/{r}/contents/{*path}",
            get(
                |State(g): State<Shared>,
                 h: HeaderMap,
                 Path((_, _, path)): Path<(String, String, String)>,
                 Query(q): Query<HashMap<String, String>>| async move {
                    if !authorized(&h) {
                        return not_found();
                    }
                    let g = g.lock().unwrap();
                    let r = q.get("ref").cloned().unwrap_or("main".into());
                    let Some(files) = g.files_at(&r) else {
                        return not_found();
                    };
                    if let Some(blob) = files.get(&path) {
                        let bytes = &g.blobs[blob];
                        return (
                            StatusCode::OK,
                            Json(json!({
                                "type": "file", "path": path, "sha": blob, "size": bytes.len(),
                                "encoding": "base64", "content": B64.encode(bytes),
                            })),
                        );
                    }
                    let prefix = format!("{path}/");
                    let items: Vec<Value> = files
                        .keys()
                        .filter(|p| p.starts_with(&prefix))
                        .map(|p| json!({"type": "file", "path": p}))
                        .collect();
                    if items.is_empty() {
                        not_found()
                    } else {
                        (StatusCode::OK, Json(Value::Array(items)))
                    }
                },
            ),
        )
        .route(
            "/repos/{o}/{r}/git/blobs/{sha}",
            get(
                |State(g): State<Shared>, Path((_, _, s)): Path<(String, String, String)>| async move {
                    let g = g.lock().unwrap();
                    match g.blobs.get(&s) {
                        Some(b) => (
                            StatusCode::OK,
                            Json(json!({"sha": s, "encoding": "base64", "content": B64.encode(b)})),
                        ),
                        None => not_found(),
                    }
                },
            ),
        )
        .route(
            "/repos/{o}/{r}/git/blobs",
            post(
                |State(g): State<Shared>, h: HeaderMap, Json(b): Json<Value>| async move {
                    if !authorized(&h) {
                        return not_found();
                    }
                    let bytes = B64.decode(b["content"].as_str().unwrap()).unwrap();
                    let s = g.lock().unwrap().put_blob(bytes);
                    (StatusCode::CREATED, Json(json!({"sha": s})))
                },
            ),
        )
        .route(
            "/repos/{o}/{r}/git/ref/heads/{*branch}",
            get(
                |State(g): State<Shared>, Path((_, _, b)): Path<(String, String, String)>| async move {
                    match g.lock().unwrap().refs.get(&b) {
                        Some(c) => (StatusCode::OK, Json(json!({"object": {"sha": c}}))),
                        None => not_found(),
                    }
                },
            ),
        )
        .route(
            "/repos/{o}/{r}/git/commits/{sha}",
            get(
                |State(g): State<Shared>, Path((_, _, s)): Path<(String, String, String)>| async move {
                    match g.lock().unwrap().commits.get(&s) {
                        Some((tree, _)) => {
                            (StatusCode::OK, Json(json!({"sha": s, "tree": {"sha": tree}})))
                        }
                        None => not_found(),
                    }
                },
            ),
        )
        .route(
            "/repos/{o}/{r}/git/trees",
            post(
                |State(g): State<Shared>, h: HeaderMap, Json(b): Json<Value>| async move {
                    if !authorized(&h) {
                        return not_found();
                    }
                    let mut g = g.lock().unwrap();
                    let mut files = g.trees[b["base_tree"].as_str().unwrap()].clone();
                    for e in b["tree"].as_array().unwrap() {
                        let path = e["path"].as_str().unwrap().to_string();
                        let blob = match e["content"].as_str() {
                            Some(c) => g.put_blob(c.as_bytes().to_vec()),
                            None => e["sha"].as_str().unwrap().to_string(),
                        };
                        files.insert(path, blob);
                    }
                    let s = g.put_tree(files);
                    (StatusCode::CREATED, Json(json!({"sha": s})))
                },
            ),
        )
        .route(
            "/repos/{o}/{r}/git/commits",
            post(
                |State(g): State<Shared>, h: HeaderMap, Json(b): Json<Value>| async move {
                    if !authorized(&h) {
                        return not_found();
                    }
                    let mut g = g.lock().unwrap();
                    let parent = b["parents"][0].as_str().map(str::to_string);
                    let s = g.put_commit(
                        b["tree"].as_str().unwrap().to_string(),
                        parent,
                        b["message"].as_str().unwrap(),
                    );
                    (StatusCode::CREATED, Json(json!({"sha": s})))
                },
            ),
        )
        .route(
            "/repos/{o}/{r}/git/refs",
            post(
                |State(g): State<Shared>, h: HeaderMap, Json(b): Json<Value>| async move {
                    if !authorized(&h) {
                        return not_found();
                    }
                    let name = b["ref"].as_str().unwrap().strip_prefix("refs/heads/").unwrap();
                    g.lock()
                        .unwrap()
                        .refs
                        .insert(name.to_string(), b["sha"].as_str().unwrap().to_string());
                    (StatusCode::CREATED, Json(json!({})))
                },
            ),
        )
        .route(
            "/repos/{o}/{r}/git/refs/heads/{*branch}",
            axum::routing::patch(
                |State(g): State<Shared>,
                 h: HeaderMap,
                 Path((_, _, branch)): Path<(String, String, String)>,
                 Json(b): Json<Value>| async move {
                    if !authorized(&h) {
                        return not_found();
                    }
                    let mut g = g.lock().unwrap();
                    let new = b["sha"].as_str().unwrap().to_string();
                    let parent = g.commits[&new].1.clone();
                    if parent.as_ref() != g.refs.get(&branch) {
                        return (
                            StatusCode::UNPROCESSABLE_ENTITY,
                            Json(json!({"message": "Update is not a fast forward"})),
                        );
                    }
                    g.refs.insert(branch, new);
                    (StatusCode::OK, Json(json!({})))
                },
            ),
        )
        .route(
            "/repos/{o}/{r}/pulls",
            post(
                |State(g): State<Shared>, h: HeaderMap, Json(b): Json<Value>| async move {
                    if !authorized(&h) {
                        return not_found();
                    }
                    let mut g = g.lock().unwrap();
                    g.pulls.push(b);
                    let n = g.pulls.len();
                    let r: Res = (
                        StatusCode::CREATED,
                        Json(json!({"html_url": format!("https://github.com/octo/plans/pull/{n}")})),
                    );
                    r
                },
            ),
        )
        .with_state(git.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), git)
}

struct Server {
    app: Router,
}

impl Server {
    fn new(api: &str) -> Self {
        let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
        let config = Config {
            github: GithubConfig::new(api.into(), None, vec![], Some("whsec".into())),
            ..Config::default()
        };
        Self {
            app: router(hub, config),
        }
    }

    async fn raw(&self, req: Request<Body>) -> (StatusCode, Value, Option<String>) {
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

    async fn call(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
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
        let (s, v, _) = self.raw(req).await;
        (s, v)
    }

    async fn signup(&self, login: &str) -> String {
        let req = Request::builder()
            .method("POST")
            .uri("/api/signup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"login": login, "name": login.to_uppercase(), "password": "password123"})
                    .to_string(),
            ))
            .unwrap();
        let (status, _, cookie) = self.raw(req).await;
        assert_eq!(status, StatusCode::OK);
        cookie.unwrap()
    }

    async fn ops(&self, id: &str, cookie: &str, ops: Value) -> Value {
        let (s, v) = self
            .call(
                "POST",
                &format!("/api/v1/boards/{id}/ops"),
                Some(cookie),
                Some(json!({ "ops": ops })),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn element(&self, id: &str, cookie: &str, eid: &str) -> (StatusCode, Value) {
        self.call(
            "GET",
            &format!("/api/v1/boards/{id}/elements/{eid}"),
            Some(cookie),
            None,
        )
        .await
    }

    async fn webhook(&self, body: &Value, secret: &str) -> StatusCode {
        let bytes = body.to_string();
        let mac = hmac(secret.as_bytes(), bytes.as_bytes());
        let req = Request::builder()
            .method("POST")
            .uri("/api/github/webhook")
            .header("x-github-event", "push")
            .header("x-hub-signature-256", format!("sha256={mac}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bytes))
            .unwrap();
        self.raw(req).await.0
    }
}

/// テスト側の HMAC-SHA256(サーバーの実装とは別に書く)。
fn hmac(key: &[u8], msg: &[u8]) -> String {
    let mut k = key.to_vec();
    k.resize(64, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new()
        .chain_update(&ipad)
        .chain_update(msg)
        .finalize();
    let out = Sha256::new()
        .chain_update(&opad)
        .chain_update(inner)
        .finalize();
    let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
    assert!(verify_signature(
        std::str::from_utf8(key).unwrap(),
        msg,
        &format!("sha256={hex}")
    ));
    hex
}

#[tokio::test]
async fn link_sync_merge_pull_request_and_webhook() {
    let (api, git) = fake_github().await;
    let s = Server::new(&api);
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let (_, b) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "振り返り"})),
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
    let r = s
        .ops(
            &id,
            &alice,
            json!([
                {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 0, "y": 0, "text": "よかったこと"}},
                {"op": "create", "element": {"type": "shape", "kind": "sticky", "x": 300, "y": 0, "text": "次にやること"}},
                {"op": "create", "element": {"type": "connector", "from": {"element": "$0"}, "to": {"element": "$1"}}}
            ]),
        )
        .await;
    let a = r["created"][0].as_str().unwrap().to_string();
    let b2 = r["created"][1].as_str().unwrap().to_string();

    // つながっていない。
    let (st, v) = s
        .call("GET", &format!("/api/boards/{id}/github"), Some(&bob), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(v.is_null());

    // オーナーだけがつなげる。読めないリポジトリ・トークンなしは断る。
    let link = json!({"repo": "https://github.com/octo/plans", "token": TOKEN});
    let (st, _) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/github"),
            Some(&bob),
            Some(link.clone()),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/github"),
            Some(&alice),
            Some(json!({"repo": "octo/plans"})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{v}");
    let (st, v) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/github"),
            Some(&alice),
            Some(json!({"repo": "octo/plans", "token": TOKEN, "path": "../x.json"})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/github"),
            Some(&alice),
            Some(link),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["repo"], "octo/plans");
    assert_eq!(v["branch"], "main");
    let path = format!("boards/{id}.graboard.json");
    assert_eq!(v["path"], path.as_str());
    assert_eq!(v["has_token"], true);
    assert_eq!(v["writable"], true);
    assert!(!v.to_string().contains(TOKEN));

    // 編集者は状態を見られる。まだファイルはない。
    let (st, v) = s
        .call(
            "GET",
            &format!("/api/boards/{id}/github/status"),
            Some(&bob),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["remote_exists"], false);
    assert_eq!(v["board_changed"], true);

    // 初めての同期: ボードをそのままコミットする。
    let (st, v) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/github/sync"),
            Some(&bob),
            Some(json!({"message": "振り返りを追加"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let n = v["repo"]["added"].as_u64().unwrap();
    assert!(n >= 3, "{v}");
    assert!(v["commit"].is_string());
    assert!(
        v["commit_url"]
            .as_str()
            .unwrap()
            .starts_with("https://github.com/octo/plans/commit/")
    );
    assert_eq!(v["link"]["last"]["by"], "bob");
    let text = git.lock().unwrap().read("main", &path).unwrap();
    assert!(text.contains("よかったこと"));
    assert_eq!(
        text.lines().filter(|l| l.starts_with("{\"id\"")).count() as u64,
        n
    );
    // README はそのまま残る。
    assert!(git.lock().unwrap().read("main", "README.md").is_some());

    // もう一度同期しても何も起きない。
    let (_, v) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/github/sync"),
            Some(&bob),
            Some(json!({})),
        )
        .await;
    assert!(v["commit"].is_null(), "{v}");
    let (_, v) = s
        .call(
            "GET",
            &format!("/api/boards/{id}/github/status"),
            Some(&bob),
            None,
        )
        .await;
    assert_eq!(v["board_changed"], false);
    assert_eq!(v["repo_changed"], false);

    // GitHub 側で付箋 A の文字を変え、ボード側では付箋 B を動かす。
    let mut doc = RepoDoc::parse(&text).unwrap();
    doc.elements
        .get_mut(&a)
        .unwrap()
        .insert("text".into(), json!("GitHub で編集"));
    git.lock().unwrap().push_file("main", &path, &doc.to_text());
    s.ops(
        &id,
        &alice,
        json!([{"op": "update", "id": b2, "set": {"x": 640}}]),
    )
    .await;
    let (_, v) = s
        .call(
            "GET",
            &format!("/api/boards/{id}/github/status"),
            Some(&bob),
            None,
        )
        .await;
    assert_eq!(v["board_changed"], true);
    assert_eq!(v["repo_changed"], true);

    let (st, v) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/github/sync"),
            Some(&alice),
            Some(json!({})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["board"], json!({"added": 0, "updated": 1, "removed": 0}));
    assert_eq!(v["repo"], json!({"added": 0, "updated": 1, "removed": 0}));
    assert_eq!(v["conflicts"], json!([]));
    let (_, e) = s.element(&id, &alice, &a).await;
    assert_eq!(e["text"], "GitHub で編集");
    let text = git.lock().unwrap().read("main", &path).unwrap();
    assert!(text.contains("GitHub で編集"));
    assert!(text.contains("\"x\":640"));

    // 取り込む前の状態は版の履歴に残る。
    let (_, versions) = s
        .call(
            "GET",
            &format!("/api/boards/{id}/versions"),
            Some(&alice),
            None,
        )
        .await;
    assert!(
        versions
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["label"] == "GitHub(octo/plans)から取り込む前"),
        "{versions}"
    );

    // 両方で同じ付箋の文字を変えたら、ボード側を残して衝突として知らせる。
    let text = git.lock().unwrap().read("main", &path).unwrap();
    let mut doc = RepoDoc::parse(&text).unwrap();
    doc.elements
        .get_mut(&a)
        .unwrap()
        .insert("text".into(), json!("リポジトリ側"));
    git.lock().unwrap().push_file("main", &path, &doc.to_text());
    s.ops(
        &id,
        &alice,
        json!([{"op": "update", "id": a, "set": {"text": "ボード側"}}]),
    )
    .await;
    let (_, v) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/github/sync"),
            Some(&alice),
            Some(json!({})),
        )
        .await;
    assert_eq!(v["conflicts"], json!(["「ボード側」"]), "{v}");
    assert!(
        git.lock()
            .unwrap()
            .read("main", &path)
            .unwrap()
            .contains("ボード側")
    );

    // プルリクエスト: main は変えずに、新しいブランチから出す。
    s.ops(
        &id,
        &alice,
        json!([{"op": "update", "id": b2, "set": {"text": "PR で提案"}}]),
    )
    .await;
    let main_before = git.lock().unwrap().refs["main"].clone();
    let (st, v) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/github/sync"),
            Some(&alice),
            Some(json!({"pull_request": true, "message": "次にやることを見直す"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        v["pull_request_url"],
        "https://github.com/octo/plans/pull/1"
    );
    {
        let g = git.lock().unwrap();
        assert_eq!(g.refs["main"], main_before);
        assert_eq!(g.pulls[0]["title"], "次にやることを見直す");
        assert_eq!(g.pulls[0]["base"], "main");
        let head = g.pulls[0]["head"].as_str().unwrap();
        assert!(head.starts_with(&format!("graboard/{id}-")));
        assert!(g.read(head, &path).unwrap().contains("PR で提案"));
        assert!(!g.read("main", &path).unwrap().contains("PR で提案"));
    }

    // 読み込み: リポジトリのファイルでボードを上書きする。
    let (st, v) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/github/sync"),
            Some(&alice),
            Some(json!({"mode": "pull"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["board"]["updated"], 1);
    assert!(v["commit"].is_null());
    let (_, e) = s.element(&id, &alice, &b2).await;
    assert_eq!(e["text"], "次にやること");

    // Webhook: 自動取り込みにして、GitHub への push を受けたらボードにマージする。
    let (st, v) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/github"),
            Some(&alice),
            Some(json!({"repo": "octo/plans", "auto_pull": true})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        v["has_token"], true,
        "同じリポジトリならトークンを使い続ける"
    );
    assert!(v["last"].is_object(), "同じ場所なら最後の同期を覚えている");
    let text = git.lock().unwrap().read("main", &path).unwrap();
    let mut doc = RepoDoc::parse(&text).unwrap();
    doc.elements.remove(&b2);
    git.lock().unwrap().push_file("main", &path, &doc.to_text());
    let event = json!({
        "ref": "refs/heads/main",
        "repository": {"full_name": "octo/plans"},
        "commits": [{"added": [], "modified": [path], "removed": []}],
    });
    assert_eq!(s.webhook(&event, "wrong").await, StatusCode::UNAUTHORIZED);
    assert_eq!(s.element(&id, &alice, &b2).await.0, StatusCode::OK);
    assert_eq!(s.webhook(&event, "whsec").await, StatusCode::NO_CONTENT);
    assert_eq!(s.element(&id, &alice, &b2).await.0, StatusCode::NOT_FOUND);
    // つながっていたコネクタは点につながる。
    let (_, v) = s
        .call(
            "GET",
            &format!("/api/boards/{id}/github/status"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(v["repo_changed"], false, "{v}");

    // 外す。
    let (st, _) = s
        .call(
            "DELETE",
            &format!("/api/boards/{id}/github"),
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/github/sync"),
            Some(&alice),
            Some(json!({})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sync_without_token_is_refused() {
    let (api, _git) = fake_github().await;
    let s = Server::new(&api);
    let alice = s.signup("alice").await;
    let (_, b) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "t"})),
        )
        .await;
    let id = b["id"].as_str().unwrap();
    // サーバーのトークンもない設定では、そもそもリポジトリを読めない。
    let (st, v) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/github"),
            Some(&alice),
            Some(json!({"repo": "octo/plans"})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or_default()
            .contains("見つかりません")
            || v.to_string().contains("見つかりません"),
        "{v}"
    );
}
