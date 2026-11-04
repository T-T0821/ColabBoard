//! ボードを GitHub のリポジトリで管理する(同期・コミット・プルリクエスト・Webhook での取り込み)。
//!
//! ボードは差分の読みやすいテキスト([`board_core::repo`] の `.graboard.json`。1 要素 1 行)として
//! リポジトリに置き、画像は同じディレクトリの `assets/` に中身のハッシュの名前で置く。
//!
//! - つなぐ: ボードのオーナーがリポジトリ・ブランチ・ファイルの場所を決める(`PUT /api/boards/{id}/github`)。
//! - 同期: 「最後に同期した時点のファイル」「ボードの今」「リポジトリの今」を要素・項目単位で
//!   3 方向マージし、ボードに取り込んでからコミットする。両方で同じ項目を変えたときはボード側を残す。
//! - プルリクエスト: ブランチに直接コミットせず、新しいブランチにコミットしてプルリクエストを出す。
//! - 取り込む前の状態は版の履歴に残すので、取り込みはいつでも戻せる。
//! - Webhook: `push` を受けて、自動取り込みにしたボードへリポジトリの変更をマージする。
//!
//! コミットは Git Data API(blob → tree → commit → ref)で 1 回にまとめる。
//!
//! | 環境変数 | 内容 |
//! |----------|------|
//! | `GITHUB_TOKEN` | サーバーのトークン。`GITHUB_SYNC_REPOS` に合うリポジトリにだけ使う |
//! | `GITHUB_SYNC_REPOS` | サーバーのトークンで書いてよいリポジトリ(`owner/name` か `owner/*` のカンマ区切り) |
//! | `GITHUB_API_URL` | GitHub Enterprise などの API の場所(既定 `https://api.github.com`) |
//! | `GITHUB_WEB_URL` | ファイル・コミットを開く URL の土台(既定 `https://github.com`) |
//! | `GITHUB_WEBHOOK_SECRET` | Webhook の署名の鍵。なければ Webhook は使えない |
//!
//! ボードごとのトークン(fine-grained の個人アクセストークンで、そのリポジトリの Contents と
//! Pull requests の読み書き)はボードの情報に保存し、API では返さない。サーバーのトークンを誰の
//! ボードからでも任意のリポジトリに使えないよう、`GITHUB_SYNC_REPOS` で範囲を決める。

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use board_core::Board;
use board_core::api::{
    ChangeCount, GithubLink, GithubStatus, GithubSyncMode, GithubSyncPoint, GithubSyncRequest,
    GithubSyncResult, Role, SetGithub,
};
use board_core::protocol::TOPIC_VERSIONS;
use board_core::repo::{self, Changes, RepoDoc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::api::{self, ApiResult, Error, KeyQuery};
use crate::audit::{self, Entry};
use crate::auth::{UserRecord, now_ms};
use crate::boards::{self, BoardMeta};
use crate::store::{BOARDS, StoreError};
use crate::versions;

const DEFAULT_API: &str = "https://api.github.com";
const DEFAULT_WEB: &str = "https://github.com";
const MAX_TOKEN_LEN: usize = 255;
const MAX_PATH_LEN: usize = 300;
const MAX_MESSAGE_CHARS: usize = 500;
/// リポジトリから読むファイル 1 つの上限。
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

/// ボードに保存する GitHub のつながり。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GithubHook {
    pub repo: String,
    pub branch: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default)]
    pub auto_pull: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<SyncPoint>,
}

/// 最後に同期した時点。`blob` はそのときのボードのファイルの blob SHA(3 方向マージの土台)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncPoint {
    pub commit: String,
    pub blob: String,
    pub at: u64,
    #[serde(default)]
    pub by: Option<String>,
}

/// サーバー全体の設定。
pub struct GithubConfig {
    pub api: String,
    pub web: String,
    pub token: Option<String>,
    /// サーバーのトークンで書いてよいリポジトリ(`owner/name` か `owner/*`。小文字)。
    pub allowed: Vec<String>,
    pub webhook_secret: Option<String>,
    http: reqwest::Client,
    /// 同期中のボード(同じボードの同期を重ねない)。
    busy: Mutex<HashSet<String>>,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self::new(DEFAULT_API.into(), None, Vec::new(), None)
    }
}

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl GithubConfig {
    pub fn new(
        api: String,
        token: Option<String>,
        allowed: Vec<String>,
        webhook_secret: Option<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("graboard")
            .build()
            .expect("HTTP クライアントを作れません");
        Self {
            api: api.trim_end_matches('/').to_string(),
            web: DEFAULT_WEB.into(),
            token,
            allowed: allowed
                .into_iter()
                .map(|a| a.trim().to_ascii_lowercase())
                .filter(|a| !a.is_empty())
                .collect(),
            webhook_secret,
            http,
            busy: Mutex::default(),
        }
    }

    pub fn from_env() -> Self {
        let allowed = non_empty("GITHUB_SYNC_REPOS")
            .map(|v| v.split(',').map(str::to_string).collect())
            .unwrap_or_default();
        let mut c = Self::new(
            non_empty("GITHUB_API_URL").unwrap_or_else(|| DEFAULT_API.into()),
            non_empty("GITHUB_TOKEN"),
            allowed,
            non_empty("GITHUB_WEBHOOK_SECRET"),
        );
        if let Some(web) = non_empty("GITHUB_WEB_URL") {
            c.web = web.trim_end_matches('/').to_string();
        }
        c
    }

    /// サーバーのトークンをこのリポジトリに使ってよいか。
    pub fn server_token_for(&self, repo: &str) -> Option<&str> {
        let repo = repo.to_ascii_lowercase();
        let owner = repo.split('/').next().unwrap_or_default();
        let ok = self
            .allowed
            .iter()
            .any(|a| *a == repo || a.strip_suffix("/*") == Some(owner));
        self.token.as_deref().filter(|_| ok)
    }

    fn token_for<'a>(&'a self, hook: &'a GithubHook) -> Option<&'a str> {
        hook.token
            .as_deref()
            .or_else(|| self.server_token_for(&hook.repo))
    }

    pub fn link(&self, hook: &GithubHook) -> GithubLink {
        GithubLink {
            repo: hook.repo.clone(),
            branch: hook.branch.clone(),
            path: hook.path.clone(),
            has_token: hook.token.is_some(),
            writable: self.token_for(hook).is_some(),
            auto_pull: hook.auto_pull,
            url: format!(
                "{}/{}/blob/{}/{}",
                self.web, hook.repo, hook.branch, hook.path
            ),
            last: hook.last.as_ref().map(|l| GithubSyncPoint {
                commit: l.commit.clone(),
                at: l.at,
                by: l.by.clone(),
                url: format!("{}/{}/commit/{}", self.web, hook.repo, l.commit),
            }),
        }
    }

    fn client<'a>(&'a self, hook: &'a GithubHook) -> Client<'a> {
        Client {
            http: &self.http,
            api: &self.api,
            repo: &hook.repo,
            token: self.token_for(hook),
        }
    }
}

/// 同期中の印。落ちると外れる。
struct BusyGuard<'a> {
    set: &'a Mutex<HashSet<String>>,
    id: String,
}

impl<'a> BusyGuard<'a> {
    fn take(set: &'a Mutex<HashSet<String>>, id: &str) -> Option<Self> {
        set.lock().unwrap().insert(id.to_string()).then(|| Self {
            set,
            id: id.to_string(),
        })
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.id);
    }
}

// ---- 入力の確かめ ----

fn name_ok(s: &str) -> bool {
    (1..=100).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && s != "."
        && s != ".."
}

/// `owner/name`・`https://github.com/owner/name(.git)` を `owner/name` にする。
pub fn parse_repo(input: &str) -> Option<String> {
    let s = input.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .map(|rest| rest.split_once('/').map_or("", |(_, p)| p))
        .unwrap_or(s);
    let s = s.trim_end_matches('/');
    let s = s.strip_suffix(".git").unwrap_or(s);
    let mut parts = s.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if parts.next().is_some() || !name_ok(owner) || !name_ok(name) {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

pub fn branch_ok(b: &str) -> bool {
    (1..=200).contains(&b.len())
        && !b.starts_with(['/', '-', '.'])
        && !b.ends_with(['/', '.'])
        && !b.contains("..")
        && !b.contains("//")
        && !b.ends_with(".lock")
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'/'))
}

/// リポジトリの中のファイルの場所(`dir/name.json`)。
pub fn path_ok(p: &str) -> bool {
    p.len() <= MAX_PATH_LEN
        && p.ends_with(".json")
        && p.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && !seg.starts_with('.')
                && seg.chars().all(|c| {
                    c.is_alphanumeric()
                        || matches!(c, '-' | '_' | '.')
                        || (!c.is_ascii() && !c.is_control() && !c.is_whitespace())
                })
        })
}

/// ボードのファイルと同じディレクトリからの相対パスを、リポジトリの中の場所にする。
fn sibling(path: &str, rel: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{rel}"),
        None => rel.to_string(),
    }
}

fn clean_message(m: Option<&str>) -> Option<String> {
    let m: String = m?
        .trim()
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .take(MAX_MESSAGE_CHARS)
        .collect();
    (!m.is_empty()).then_some(m)
}

// ---- GitHub の API ----

#[derive(Debug)]
enum GhError {
    NotFound,
    /// ブランチが先に進んでいた(同時に更新された)。
    Conflict,
    Other(String),
}

impl GhError {
    fn into_api(self, what: &str) -> Error {
        match self {
            GhError::NotFound => Error::new(
                StatusCode::BAD_GATEWAY,
                format!(
                    "{what}: GitHub で見つかりません(リポジトリ・ブランチ・トークンの権限を確かめてください)"
                ),
            ),
            GhError::Conflict => Error::new(
                StatusCode::CONFLICT,
                "同期の途中で GitHub 側のブランチが更新されました。もう一度同期してください",
            ),
            GhError::Other(e) => Error::new(StatusCode::BAD_GATEWAY, format!("{what}: {e}")),
        }
    }
}

type GhResult<T> = Result<T, GhError>;

struct RemoteFile {
    sha: String,
    bytes: Vec<u8>,
}

/// ひとつのリポジトリを相手にする API の呼び出し。
struct Client<'a> {
    http: &'a reqwest::Client,
    api: &'a str,
    repo: &'a str,
    token: Option<&'a str>,
}

/// パスの各部分を URL に入れられる形にする。
fn encode_path(p: &str) -> String {
    p.split('/')
        .map(|seg| {
            seg.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

impl Client<'_> {
    fn url(&self, rest: &str) -> String {
        format!("{}/repos/{}{rest}", self.api, self.repo)
    }

    async fn send(
        &self,
        method: reqwest::Method,
        url: String,
        body: Option<Value>,
    ) -> GhResult<Value> {
        let mut req = self
            .http
            .request(method, url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(t) = self.token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| GhError::Other(format!("GitHub に接続できません({e})")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| GhError::Other(e.to_string()))?;
        if bytes.len() > MAX_FILE_BYTES * 2 {
            return Err(GhError::Other("GitHub からの応答が大きすぎます".into()));
        }
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        match status.as_u16() {
            200..=299 => Ok(v),
            404 => Err(GhError::NotFound),
            409 | 422
                if v["message"].as_str().is_some_and(|m| {
                    m.contains("fast forward")
                        || m.contains("fast-forward")
                        || m.contains("Update is not")
                }) =>
            {
                Err(GhError::Conflict)
            }
            401 => Err(GhError::Other(
                "トークンが正しくないか、期限切れです".into(),
            )),
            403 => Err(GhError::Other(format!(
                "権限がありません({})",
                v["message"].as_str().unwrap_or("forbidden")
            ))),
            s => Err(GhError::Other(format!(
                "HTTP {s}: {}",
                v["message"].as_str().unwrap_or("")
            ))),
        }
    }

    async fn get(&self, rest: &str) -> GhResult<Value> {
        self.send(reqwest::Method::GET, self.url(rest), None).await
    }

    async fn post(&self, rest: &str, body: Value) -> GhResult<Value> {
        self.send(reqwest::Method::POST, self.url(rest), Some(body))
            .await
    }

    /// リポジトリの既定のブランチ(読めるかどうかの確認も兼ねる)。
    async fn default_branch(&self) -> GhResult<String> {
        let v = self.send(reqwest::Method::GET, self.url(""), None).await?;
        Ok(v["default_branch"].as_str().unwrap_or("main").to_string())
    }

    async fn blob(&self, sha: &str) -> GhResult<Vec<u8>> {
        let v = self.get(&format!("/git/blobs/{sha}")).await?;
        decode_content(&v)
    }

    /// `git_ref`(ブランチ名かコミット)の時点のファイル。なければ `None`。
    async fn file(&self, git_ref: &str, path: &str) -> GhResult<Option<RemoteFile>> {
        let v = match self
            .get(&format!(
                "/contents/{}?ref={}",
                encode_path(path),
                encode_path(git_ref)
            ))
            .await
        {
            Ok(v) => v,
            Err(GhError::NotFound) => return Ok(None),
            Err(e) => return Err(e),
        };
        if v.is_array() || v["type"].as_str().is_some_and(|t| t != "file") {
            return Err(GhError::Other(format!("{path} はファイルではありません")));
        }
        let sha = v["sha"].as_str().unwrap_or_default().to_string();
        // 1 MB を超えるファイルは中身が入っていないので、blob で読む。
        let bytes = match v["encoding"].as_str() {
            Some("base64") if v["content"].as_str().is_some_and(|c| !c.is_empty()) => {
                decode_content(&v)?
            }
            _ if v["size"].as_u64() == Some(0) => Vec::new(),
            _ => self.blob(&sha).await?,
        };
        Ok(Some(RemoteFile { sha, bytes }))
    }

    /// ディレクトリの中のファイル名。なければ空。
    async fn list_dir(&self, git_ref: &str, dir: &str) -> GhResult<HashSet<String>> {
        match self
            .get(&format!(
                "/contents/{}?ref={}",
                encode_path(dir),
                encode_path(git_ref)
            ))
            .await
        {
            Ok(Value::Array(items)) => Ok(items
                .iter()
                .filter_map(|i| i["path"].as_str().map(str::to_string))
                .collect()),
            Ok(_) | Err(GhError::NotFound) => Ok(HashSet::new()),
            Err(e) => Err(e),
        }
    }

    async fn head(&self, branch: &str) -> GhResult<String> {
        let v = self
            .get(&format!("/git/ref/heads/{}", encode_path(branch)))
            .await?;
        v["object"]["sha"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| GhError::Other("ブランチの先頭が読めません".into()))
    }

    /// ファイルを書き換えたコミットを作る(ブランチはまだ動かさない)。
    async fn commit(
        &self,
        parent: &str,
        text: (&str, &str),
        binaries: &[(String, Vec<u8>)],
        message: &str,
    ) -> GhResult<String> {
        let c = self.get(&format!("/git/commits/{parent}")).await?;
        let base_tree = c["tree"]["sha"]
            .as_str()
            .ok_or_else(|| GhError::Other("コミットのツリーが読めません".into()))?
            .to_string();
        let mut entries = vec![json!({
            "path": text.0, "mode": "100644", "type": "blob", "content": text.1,
        })];
        for (path, bytes) in binaries {
            let b = self
                .post(
                    "/git/blobs",
                    json!({ "content": B64.encode(bytes), "encoding": "base64" }),
                )
                .await?;
            let sha = b["sha"]
                .as_str()
                .ok_or_else(|| GhError::Other("画像を送れませんでした".into()))?;
            entries.push(json!({ "path": path, "mode": "100644", "type": "blob", "sha": sha }));
        }
        let tree = self
            .post(
                "/git/trees",
                json!({ "base_tree": base_tree, "tree": entries }),
            )
            .await?;
        let tree_sha = tree["sha"]
            .as_str()
            .ok_or_else(|| GhError::Other("ツリーを作れませんでした".into()))?;
        let commit = self
            .post(
                "/git/commits",
                json!({ "message": message, "tree": tree_sha, "parents": [parent] }),
            )
            .await?;
        commit["sha"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| GhError::Other("コミットを作れませんでした".into()))
    }

    async fn move_branch(&self, branch: &str, sha: &str) -> GhResult<()> {
        self.send(
            reqwest::Method::PATCH,
            self.url(&format!("/git/refs/heads/{}", encode_path(branch))),
            Some(json!({ "sha": sha, "force": false })),
        )
        .await
        .map(|_| ())
    }

    async fn create_branch(&self, branch: &str, sha: &str) -> GhResult<()> {
        self.post(
            "/git/refs",
            json!({ "ref": format!("refs/heads/{branch}"), "sha": sha }),
        )
        .await
        .map(|_| ())
    }

    async fn pull_request(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> GhResult<String> {
        let v = self
            .post(
                "/pulls",
                json!({ "title": title, "head": head, "base": base, "body": body }),
            )
            .await?;
        Ok(v["html_url"].as_str().unwrap_or_default().to_string())
    }
}

fn decode_content(v: &Value) -> GhResult<Vec<u8>> {
    let raw: String = v["content"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let bytes = B64
        .decode(raw)
        .map_err(|_| GhError::Other("ファイルの中身が読めません".into()))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(GhError::Other("ファイルが大きすぎます".into()));
    }
    Ok(bytes)
}

fn parse_remote(f: &RemoteFile, path: &str) -> ApiResult<RepoDoc> {
    let text = std::str::from_utf8(&f.bytes)
        .map_err(|_| Error::bad_request(format!("リポジトリの {path} が UTF-8 ではありません")))?;
    RepoDoc::parse(text)
        .map_err(|e| Error::bad_request(format!("リポジトリの {path} が読めません: {e}")))
}

fn count(c: Changes) -> ChangeCount {
    ChangeCount {
        added: c.added,
        updated: c.updated,
        removed: c.removed,
    }
}

// ---- ハンドラー ----

fn require_editor(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
    key: Option<&str>,
) -> ApiResult<(BoardMeta, Option<UserRecord>)> {
    let user = api::current_user(state, headers)?;
    let (meta, role) = api::board_access(state, id, user.as_ref().map(|u| u.login.as_str()), key)?;
    if role < Role::Editor {
        return Err(Error::forbidden());
    }
    let meta =
        meta.ok_or_else(|| Error::bad_request("誰でも編集できるボードは GitHub につなげません"))?;
    Ok((meta, user))
}

fn linked(meta: &BoardMeta) -> ApiResult<GithubHook> {
    meta.github
        .clone()
        .ok_or_else(|| Error::bad_request("このボードは GitHub につながっていません"))
}

pub async fn get_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<Option<GithubLink>>> {
    let (meta, _) = require_editor(&state, &headers, &id, q.key.as_deref())?;
    Ok(Json(
        meta.github.as_ref().map(|h| state.config.github.link(h)),
    ))
}

pub async fn set_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SetGithub>,
) -> ApiResult<Json<GithubLink>> {
    let meta = api::owner_board(&state, &headers, &id)?;
    // 暗号化ボードはサーバーが中身を読めないので、リポジトリに書き出せない。
    crate::e2e::require_plain(Some(&meta))?;
    let user = api::require_user(&state, &headers)?;
    let cfg = &state.config.github;
    let repo = parse_repo(&req.repo).ok_or_else(|| {
        Error::bad_request(
            "リポジトリは owner/name か https://github.com/owner/name の形で入力してください",
        )
    })?;
    let token = match req.token.as_deref().map(str::trim) {
        Some("") => None,
        Some(t) => {
            if t.len() > MAX_TOKEN_LEN || !t.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(Error::bad_request("トークンの形が正しくありません"));
            }
            Some(t.to_string())
        }
        // 同じリポジトリのままなら、保存してあるトークンを使い続ける。
        None => meta
            .github
            .as_ref()
            .filter(|h| h.repo.eq_ignore_ascii_case(&repo))
            .and_then(|h| h.token.clone()),
    };
    let path = match req.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => p.trim_start_matches('/').to_string(),
        None => format!("boards/{id}{}", repo::EXTENSION),
    };
    if !path_ok(&path) {
        return Err(Error::bad_request(
            "ファイルの場所は boards/plan.graboard.json のように .json で終わる相対パスにしてください",
        ));
    }
    let mut hook = GithubHook {
        repo,
        branch: String::new(),
        path,
        token,
        auto_pull: req.auto_pull,
        last: None,
    };
    // 読めるか確かめ、ブランチを決める。
    let default_branch = cfg
        .client(&hook)
        .default_branch()
        .await
        .map_err(|e| e.into_api("リポジトリを読めません"))?;
    hook.branch = match req
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(b) if branch_ok(b) => b.to_string(),
        Some(_) => return Err(Error::bad_request("ブランチ名が正しくありません")),
        None => default_branch,
    };
    cfg.client(&hook)
        .head(&hook.branch)
        .await
        .map_err(|e| e.into_api(&format!("ブランチ「{}」", hook.branch)))?;
    // 同じ場所につなぎ直しただけなら、最後の同期を覚えておく。
    if let Some(old) = &meta.github
        && old.repo.eq_ignore_ascii_case(&hook.repo)
        && old.branch == hook.branch
        && old.path == hook.path
    {
        hook.last = old.last.clone();
    }
    let link = cfg.link(&hook);
    let mut meta = boards::get(state.store(), &id)?.unwrap_or(meta);
    meta.github = Some(hook.clone());
    meta.updated = now_ms();
    boards::put(state.store(), &meta)?;
    let mut entry = Entry::new("board.github.link")
        .board(&id)
        .team(meta.team.clone())
        .target(format!("{}@{}:{}", hook.repo, hook.branch, hook.path));
    entry.actor = Some(user.login);
    audit::record(&state, &headers, entry);
    Ok(Json(link))
}

pub async fn unlink(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let mut meta = api::owner_board(&state, &headers, &id)?;
    let user = api::require_user(&state, &headers)?;
    if meta.github.take().is_some() {
        meta.updated = now_ms();
        boards::put(state.store(), &meta)?;
        let mut entry = Entry::new("board.github.unlink")
            .board(&id)
            .team(meta.team.clone());
        entry.actor = Some(user.login);
        audit::record(&state, &headers, entry);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// ボードの今の中身。
async fn board_doc(
    state: &AppState,
    id: &str,
    title: &str,
) -> ApiResult<(RepoDoc, Vec<repo::Asset>)> {
    let current = state.hub.current_state(id)?;
    let title = title.to_string();
    tokio::task::spawn_blocking(move || {
        let mut b = Board::new();
        if let Some(s) = current {
            b.apply_remote(&s)?;
        }
        Ok::<_, String>(RepoDoc::from_board(&b, &title))
    })
    .await
    .map_err(|e| Error::from(StoreError::from(e.to_string())))?
    .map_err(|e| Error::bad_request(format!("ボードが読めません: {e}")))
}

pub async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<GithubStatus>> {
    let (meta, _) = require_editor(&state, &headers, &id, q.key.as_deref())?;
    let hook = linked(&meta)?;
    let cfg = &state.config.github;
    let client = cfg.client(&hook);
    let remote = client
        .file(&hook.branch, &hook.path)
        .await
        .map_err(|e| e.into_api("ファイルを読めません"))?;
    let (ours, _) = board_doc(&state, &id, &meta.title).await?;
    let base = match (&hook.last, &remote) {
        (Some(l), Some(r)) if l.blob == r.sha => Some(parse_remote(r, &hook.path)?),
        (Some(l), _) => match client.blob(&l.blob).await {
            Ok(b) => std::str::from_utf8(&b)
                .ok()
                .and_then(|t| RepoDoc::parse(t).ok()),
            Err(_) => None,
        },
        (None, Some(r)) => Some(parse_remote(r, &hook.path)?),
        (None, None) => None,
    };
    let repo_changed = match (&hook.last, &remote) {
        (Some(l), Some(r)) => l.blob != r.sha,
        (Some(_), None) => true,
        (None, _) => false,
    };
    Ok(Json(GithubStatus {
        link: cfg.link(&hook),
        remote_exists: remote.is_some(),
        board_changed: base.is_none_or(|b| b.elements != ours.elements),
        repo_changed,
    }))
}

pub async fn sync(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<GithubSyncRequest>,
) -> ApiResult<Json<GithubSyncResult>> {
    let (meta, user) = require_editor(&state, &headers, &id, q.key.as_deref())?;
    let res = run(&state, &meta, user.as_ref(), &req, true).await?;
    let mut entry = Entry::new("board.github.sync")
        .board(&id)
        .team(meta.team.clone())
        .detail(format!(
            "{:?} board+{}~{}-{} repo+{}~{}-{}",
            req.mode,
            res.board.added,
            res.board.updated,
            res.board.removed,
            res.repo.added,
            res.repo.updated,
            res.repo.removed
        ));
    if let Some(c) = res.commit.as_ref() {
        entry = entry.target(c.clone());
    }
    entry.actor = user.map(|u| u.login);
    audit::record(&state, &headers, entry);
    Ok(Json(res))
}

/// 同期の本体。`commit` が false なら、リポジトリの変更をボードに取り込むだけ(Webhook)。
async fn run(
    state: &AppState,
    meta: &BoardMeta,
    user: Option<&UserRecord>,
    req: &GithubSyncRequest,
    commit: bool,
) -> ApiResult<GithubSyncResult> {
    let id = meta.id.as_str();
    let cfg = &state.config.github;
    let _busy = BusyGuard::take(&cfg.busy, id).ok_or_else(|| {
        Error::new(
            StatusCode::CONFLICT,
            "このボードは同期中です。少し待ってください",
        )
    })?;
    let hook = linked(meta)?;
    let client = cfg.client(&hook);
    let writes = commit && (req.mode != GithubSyncMode::Pull);
    if writes && client.token.is_none() {
        return Err(Error::bad_request(
            "リポジトリに書き込むトークンがありません。共有パネルの GitHub の設定でトークンを入力してください",
        ));
    }

    let remote = client
        .file(&hook.branch, &hook.path)
        .await
        .map_err(|e| e.into_api("ファイルを読めません"))?;
    let theirs = remote
        .as_ref()
        .map(|r| parse_remote(r, &hook.path))
        .transpose()?;
    let (ours, our_assets) = board_doc(state, id, &meta.title).await?;

    // 目指す中身(ボードとリポジトリの両方をこれにする)。
    let mut conflicts = Vec::new();
    let mut target = match (req.mode, &theirs) {
        (GithubSyncMode::Pull, None) => {
            return Err(Error::bad_request(format!(
                "リポジトリに {} がありません",
                hook.path
            )));
        }
        (GithubSyncMode::Pull, Some(t)) => t.clone(),
        (GithubSyncMode::Push, _) | (_, None) => ours.clone(),
        (GithubSyncMode::Sync, Some(t)) => {
            let unchanged = matches!((&hook.last, &remote), (Some(l), Some(r)) if l.blob == r.sha);
            if unchanged {
                // リポジトリは最後の同期から変わっていない: ボードの今をそのまま書く。
                ours.clone()
            } else {
                let base = match &hook.last {
                    Some(l) => client
                        .blob(&l.blob)
                        .await
                        .ok()
                        .and_then(|b| String::from_utf8(b).ok())
                        .and_then(|t| RepoDoc::parse(&t).ok()),
                    None => None,
                };
                let merged = repo::merge3(base.as_ref(), &ours, t);
                conflicts = merged
                    .conflicts
                    .iter()
                    .map(|cid| merged.doc.describe(cid))
                    .collect();
                merged.doc
            }
        }
    };
    // 題名はボードのものを使う。
    target.title = meta.title.clone();

    // ボードに取り込む。
    let board_changes = ours.changes_to(&target);
    if !board_changes.is_empty() {
        // ボードにない画像はリポジトリから読む。
        let have: HashSet<&str> = our_assets.iter().map(|a| a.path.as_str()).collect();
        let mut fetched: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for rel in target.asset_paths() {
            if have.contains(rel.as_str())
                || !rel.starts_with(repo::ASSET_DIR)
                || rel.contains("..")
            {
                continue;
            }
            if let Ok(Some(f)) = client.file(&hook.branch, &sibling(&hook.path, &rel)).await {
                fetched.insert(rel, f.bytes);
            }
        }
        if let Some(current) = state.hub.current_state(id)? {
            let label = format!("GitHub({})から取り込む前", hook.repo);
            versions::create(state.store(), id, &current, Some(&label), user)?;
            state.hub.refresh(id, TOPIC_VERSIONS);
        }
        let hub = state.hub.clone();
        let room = id.to_string();
        let doc = target.clone();
        tokio::task::spawn_blocking(move || {
            hub.edit(&room, |b| {
                repo::apply_to_board(b, &doc, |p| fetched.get(p).cloned())
            })
        })
        .await
        .map_err(|e| Error::from(StoreError::from(e.to_string())))?
        .map_err(|e: String| Error::bad_request(format!("ボードに取り込めませんでした: {e}")))?;
    }

    // リポジトリに書く。
    let empty = RepoDoc::default();
    let repo_changes = theirs.as_ref().unwrap_or(&empty).changes_to(&target);
    let title_changed = theirs.as_ref().is_some_and(|t| t.title != target.title);
    let needs_commit = writes && (theirs.is_none() || !repo_changes.is_empty() || title_changed);
    let mut new_last = hook.last.clone();
    let mut commit_sha = None;
    let mut pr_url = None;
    if needs_commit {
        let text = target.to_text();
        // リポジトリにない画像だけ送る(名前が中身のハッシュなので、名前があれば同じもの)。
        let wanted = target.asset_paths();
        let dir = sibling(&hook.path, repo::ASSET_DIR);
        let existing = if wanted.is_empty() {
            HashSet::new()
        } else {
            client
                .list_dir(&hook.branch, &dir)
                .await
                .map_err(|e| e.into_api("画像の一覧を読めません"))?
        };
        let binaries: Vec<(String, Vec<u8>)> = our_assets
            .iter()
            .filter(|a| wanted.contains(&a.path))
            .map(|a| (sibling(&hook.path, &a.path), a.bytes.clone()))
            .filter(|(p, _)| !existing.contains(p))
            .collect();
        let who = user.map(|u| u.name.as_str()).unwrap_or("グラボード");
        let message = clean_message(req.message.as_deref()).unwrap_or_else(|| {
            format!(
                "{}: ボードを更新(追加 {}・変更 {}・削除 {})",
                meta.title, repo_changes.added, repo_changes.updated, repo_changes.removed
            )
        });
        let message = format!("{message}\n\nグラボードから {who} が保存");
        let head = client
            .head(&hook.branch)
            .await
            .map_err(|e| e.into_api(&format!("ブランチ「{}」", hook.branch)))?;
        let sha = client
            .commit(&head, (&hook.path, &text), &binaries, &message)
            .await
            .map_err(|e| e.into_api("コミットできません"))?;
        if req.pull_request {
            let branch = format!("graboard/{id}-{}", now_ms());
            client
                .create_branch(&branch, &sha)
                .await
                .map_err(|e| e.into_api("ブランチを作れません"))?;
            let title = clean_message(req.message.as_deref())
                .and_then(|m| m.lines().next().map(str::to_string))
                .unwrap_or_else(|| format!("{} を更新", meta.title));
            let body = format!(
                "グラボードのボード「{}」の変更です(追加 {}・変更 {}・削除 {})。\n\n{} が作成しました。",
                meta.title, repo_changes.added, repo_changes.updated, repo_changes.removed, who
            );
            pr_url = Some(
                client
                    .pull_request(&branch, &hook.branch, &title, &body)
                    .await
                    .map_err(|e| e.into_api("プルリクエストを出せません"))?,
            );
            // ブランチはまだ変わっていないので、同期の土台はリポジトリの今のまま。
            if let Some(r) = &remote {
                new_last = Some(SyncPoint {
                    commit: head,
                    blob: r.sha.clone(),
                    at: now_ms(),
                    by: user.map(|u| u.login.clone()),
                });
            }
        } else {
            client
                .move_branch(&hook.branch, &sha)
                .await
                .map_err(|e| e.into_api("ブランチを進められません"))?;
            let blob = client
                .file(&sha, &hook.path)
                .await
                .map_err(|e| e.into_api("コミットしたファイルを読めません"))?
                .map(|f| f.sha)
                .unwrap_or_default();
            new_last = Some(SyncPoint {
                commit: sha.clone(),
                blob,
                at: now_ms(),
                by: user.map(|u| u.login.clone()),
            });
        }
        commit_sha = Some(sha);
    } else if let Some(r) = &remote {
        // 書かなくてよかった: リポジトリの今を土台にする。
        let head = client.head(&hook.branch).await.ok();
        new_last = Some(SyncPoint {
            commit: head.unwrap_or_else(|| {
                hook.last
                    .as_ref()
                    .map(|l| l.commit.clone())
                    .unwrap_or_default()
            }),
            blob: r.sha.clone(),
            at: now_ms(),
            by: user.map(|u| u.login.clone()),
        });
    }

    // 最後の同期を覚える(その間に設定が変わっていたら触らない)。
    let mut fresh = boards::get(state.store(), id)?.unwrap_or_else(|| meta.clone());
    let mut link_hook = hook.clone();
    if let Some(h) = fresh.github.as_mut()
        && h.repo == hook.repo
        && h.branch == hook.branch
        && h.path == hook.path
    {
        h.last = new_last.clone();
        link_hook = h.clone();
        boards::put(state.store(), &fresh)?;
    }
    tracing::info!(board = %id, repo = %hook.repo, mode = ?req.mode, "GitHub と同期");
    let repo_count = if needs_commit {
        count(repo_changes)
    } else {
        ChangeCount::default()
    };
    Ok(GithubSyncResult {
        board: count(board_changes),
        repo: repo_count,
        conflicts,
        commit_url: commit_sha
            .as_ref()
            .map(|c| format!("{}/{}/commit/{c}", cfg.web, hook.repo)),
        commit: commit_sha,
        pull_request_url: pr_url,
        link: cfg.link(&link_hook),
    })
}

// ---- Webhook ----

/// HMAC-SHA256。
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// `X-Hub-Signature-256: sha256=<hex>` を確かめる。
pub fn verify_signature(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex) = header.trim().strip_prefix("sha256=") else {
        return false;
    };
    let want: String = hmac_sha256(secret.as_bytes(), body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    // 長さが同じなら、時間が中身によらない比べ方をする。
    want.len() == hex.len()
        && want
            .bytes()
            .zip(hex.to_ascii_lowercase().bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

pub async fn webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let Some(secret) = state.config.github.webhook_secret.as_deref() else {
        return Err(Error::not_found());
    };
    let sig = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !verify_signature(secret, &body, sig) {
        return Err(Error::unauthorized());
    }
    let event = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if event != "push" {
        return Ok(StatusCode::NO_CONTENT);
    }
    let v: Value =
        serde_json::from_slice(&body).map_err(|_| Error::bad_request("JSON として読めません"))?;
    let repo = v["repository"]["full_name"].as_str().unwrap_or_default();
    let Some(branch) = v["ref"]
        .as_str()
        .and_then(|r| r.strip_prefix("refs/heads/"))
    else {
        return Ok(StatusCode::NO_CONTENT);
    };
    // push で変わったファイル。
    let touched: HashSet<&str> = v["commits"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| ["added", "modified", "removed"].map(|k| &c[k]))
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let targets: Vec<BoardMeta> = state
        .store()
        .scan_all_json::<BoardMeta>(BOARDS)?
        .into_iter()
        .filter(|m| !m.deleted)
        .filter(|m| {
            m.github.as_ref().is_some_and(|h| {
                h.auto_pull
                    && h.repo.eq_ignore_ascii_case(repo)
                    && h.branch == branch
                    && (touched.is_empty() || touched.contains(h.path.as_str()))
            })
        })
        .collect();
    for meta in targets {
        let req = GithubSyncRequest::default();
        match run(&state, &meta, None, &req, false).await {
            Ok(r) => tracing::info!(
                board = %meta.id,
                added = r.board.added,
                updated = r.board.updated,
                removed = r.board.removed,
                "GitHub の push を取り込みました"
            ),
            Err(e) => {
                tracing::warn!(board = %meta.id, "GitHub の push を取り込めません: {}", e.message())
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos() {
        assert_eq!(parse_repo("octo/plans").as_deref(), Some("octo/plans"));
        assert_eq!(
            parse_repo("https://github.com/octo/plans.git").as_deref(),
            Some("octo/plans")
        );
        assert_eq!(
            parse_repo("https://github.com/octo/plans/").as_deref(),
            Some("octo/plans")
        );
        assert!(parse_repo("octo").is_none());
        assert!(parse_repo("octo/plans/tree/main").is_none());
        assert!(parse_repo("octo/../x").is_none());
    }

    #[test]
    fn branches_and_paths() {
        assert!(branch_ok("main"));
        assert!(branch_ok("feature/board-1"));
        assert!(!branch_ok("-x"));
        assert!(!branch_ok("a..b"));
        assert!(!branch_ok("a b"));
        assert!(path_ok("boards/plan.graboard.json"));
        assert!(path_ok("ボード/企画.graboard.json"));
        assert!(!path_ok("/abs.json"));
        assert!(!path_ok("../x.json"));
        assert!(!path_ok("boards/.hidden.json"));
        assert!(!path_ok("boards/plan.png"));
        assert!(!path_ok("a b.json"));
        assert_eq!(
            sibling("boards/a.json", "assets/x.png"),
            "boards/assets/x.png"
        );
        assert_eq!(sibling("a.json", "assets/x.png"), "assets/x.png");
    }

    #[test]
    fn server_token_scope() {
        let c = GithubConfig::new(
            DEFAULT_API.into(),
            Some("t".into()),
            vec!["Octo/*".into(), "acme/boards".into()],
            None,
        );
        assert_eq!(c.server_token_for("octo/anything"), Some("t"));
        assert_eq!(c.server_token_for("acme/boards"), Some("t"));
        assert_eq!(c.server_token_for("acme/other"), None);
        let none = GithubConfig::new(DEFAULT_API.into(), Some("t".into()), vec![], None);
        assert_eq!(none.server_token_for("octo/x"), None);
    }

    #[test]
    fn webhook_signature() {
        // GitHub のドキュメントの例。
        let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(verify_signature(
            "It's a Secret to Everybody",
            b"Hello, World!",
            sig
        ));
        assert!(!verify_signature("wrong", b"Hello, World!", sig));
        assert!(!verify_signature(
            "It's a Secret to Everybody",
            b"Hello",
            sig
        ));
        assert!(!verify_signature(
            "It's a Secret to Everybody",
            b"Hello, World!",
            "sha1=00"
        ));
    }
}
