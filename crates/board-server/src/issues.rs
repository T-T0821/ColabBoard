//! GitHub・Jira の課題の状態を引く(課題カード用)。
//!
//! - `POST /api/boards/{id}/issues?key=` 本文 `{ "urls": [...] }` → [`IssueLookup`] の配列
//!
//! 使えるのはそのボードの編集者以上。URL は [`board_core::issue::parse_url`] で読み、
//! GitHub は `GITHUB_API_URL`(既定 `https://api.github.com`)、Jira は `JIRA_BASE_URL` の
//! サイトだけに問い合わせる(ほかのサイトの URL は断る)。資格情報は環境変数で渡す。
//!
//! | 環境変数 | 内容 |
//! |----------|------|
//! | `GITHUB_TOKEN` | 非公開リポジトリを読むとき・回数制限を緩めるとき(なくても公開リポジトリは読める) |
//! | `GITHUB_API_URL` | GitHub Enterprise などの API の場所 |
//! | `JIRA_BASE_URL` | `https://example.atlassian.net` |
//! | `JIRA_EMAIL` / `JIRA_API_TOKEN` | Jira の API トークン(Basic 認証) |
//! | `JIRA_API_URL` | Jira の API を別の場所経由で呼ぶとき(既定は `JIRA_BASE_URL`) |
//!
//! 同じ URL は [`CACHE_TTL`] の間は問い合わせ直さない。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use board_core::api::{IssueInfo, IssueLookup, IssueProvider, IssueQuery, IssueState};
use board_core::issue::{IssueRef, MAX_URLS, parse_url};
use serde_json::Value;

use crate::AppState;
use crate::api::{self, ApiResult, Error, KeyQuery};

pub const CACHE_TTL: Duration = Duration::from_secs(60);
const DEFAULT_GITHUB_API: &str = "https://api.github.com";

pub struct JiraConfig {
    /// `https://example.atlassian.net`(末尾 `/` なし、小文字)。
    pub base_url: String,
    /// 実際に問い合わせる API の場所(`None` なら `base_url`。プロキシ越しやテスト用)。
    pub api_url: Option<String>,
    pub email: String,
    pub token: String,
}

/// 引いた時刻と結果。
type Cached = (Instant, Result<IssueInfo, String>);

pub struct IssuesConfig {
    pub github_api: String,
    pub github_token: Option<String>,
    pub jira: Option<JiraConfig>,
    http: reqwest::Client,
    cache: Mutex<HashMap<String, Cached>>,
}

impl Default for IssuesConfig {
    fn default() -> Self {
        Self::new(DEFAULT_GITHUB_API.into(), None, None)
    }
}

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl IssuesConfig {
    pub fn new(github_api: String, github_token: Option<String>, jira: Option<JiraConfig>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("graboard")
            .build()
            .expect("HTTP クライアントを作れません");
        Self {
            github_api: github_api.trim_end_matches('/').to_string(),
            github_token,
            jira: jira.map(|j| JiraConfig {
                base_url: j.base_url.trim_end_matches('/').to_ascii_lowercase(),
                ..j
            }),
            http,
            cache: Mutex::default(),
        }
    }

    pub fn from_env() -> Self {
        let jira = match (
            non_empty("JIRA_BASE_URL"),
            non_empty("JIRA_EMAIL"),
            non_empty("JIRA_API_TOKEN"),
        ) {
            (Some(base_url), Some(email), Some(token)) => Some(JiraConfig {
                base_url,
                api_url: non_empty("JIRA_API_URL"),
                email,
                token,
            }),
            _ => None,
        };
        Self::new(
            non_empty("GITHUB_API_URL").unwrap_or_else(|| DEFAULT_GITHUB_API.into()),
            non_empty("GITHUB_TOKEN"),
            jira,
        )
    }

    fn cached(&self, url: &str) -> Option<Result<IssueInfo, String>> {
        let mut cache = self.cache.lock().unwrap();
        let now = Instant::now();
        cache.retain(|_, (t, _)| now.duration_since(*t) < CACHE_TTL);
        cache.get(url).map(|(_, r)| r.clone())
    }

    /// 1 件引く。
    pub async fn lookup(&self, url: &str) -> Result<IssueInfo, String> {
        if let Some(r) = self.cached(url) {
            return r;
        }
        let r = match parse_url(url) {
            None => Err("GitHub・Jira の課題の URL ではありません".to_string()),
            Some(r) => self.fetch(url, &r).await,
        };
        self.cache
            .lock()
            .unwrap()
            .insert(url.to_string(), (Instant::now(), r.clone()));
        r
    }

    async fn fetch(&self, url: &str, r: &IssueRef) -> Result<IssueInfo, String> {
        match r {
            IssueRef::Github {
                owner,
                repo,
                number,
                pull,
            } => {
                let api = format!("{}/repos/{owner}/{repo}/issues/{number}", self.github_api);
                let mut req = self
                    .http
                    .get(api)
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28");
                if let Some(t) = &self.github_token {
                    req = req.bearer_auth(t);
                }
                let v = send(req).await?;
                Ok(github_info(&v, r.key(), *pull, url))
            }
            IssueRef::Jira { site, key } => {
                let Some(jira) = &self.jira else {
                    return Err("Jira の連携が設定されていません".into());
                };
                if *site != jira.base_url {
                    return Err(format!("{site} の Jira には問い合わせられません"));
                }
                let api = format!(
                    "{}/rest/api/3/issue/{key}?fields=summary,status,assignee",
                    jira.api_url
                        .as_deref()
                        .unwrap_or(&jira.base_url)
                        .trim_end_matches('/')
                );
                let req = self
                    .http
                    .get(api)
                    .header("Accept", "application/json")
                    .basic_auth(&jira.email, Some(&jira.token));
                let v = send(req).await?;
                jira_info(&v, key, url)
            }
        }
    }
}

async fn send(req: reqwest::RequestBuilder) -> Result<Value, String> {
    let resp = req.send().await.map_err(|e| {
        tracing::warn!("課題を引けません: {e}");
        "つながりません".to_string()
    })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => "権限がありません".into(),
            404 => "見つかりません".into(),
            s => format!("HTTP {s}"),
        });
    }
    resp.json::<Value>()
        .await
        .map_err(|_| "応答を読めません".to_string())
}

fn str_at<'a>(v: &'a Value, path: &str) -> Option<&'a str> {
    v.pointer(path).and_then(Value::as_str)
}

fn github_info(v: &Value, key: String, pull_url: bool, url: &str) -> IssueInfo {
    let pull = pull_url || v.get("pull_request").is_some_and(|p| !p.is_null());
    let open = str_at(v, "/state") == Some("open");
    let merged = str_at(v, "/pull_request/merged_at").is_some();
    let draft = v.get("draft").and_then(Value::as_bool).unwrap_or(false);
    let (state, status) = if open {
        if draft {
            (IssueState::InProgress, "Draft")
        } else {
            (IssueState::Open, "Open")
        }
    } else if merged {
        (IssueState::Done, "Merged")
    } else if pull {
        (IssueState::Closed, "Closed")
    } else if str_at(v, "/state_reason") == Some("not_planned") {
        (IssueState::Closed, "Closed (not planned)")
    } else {
        (IssueState::Done, "Closed")
    };
    IssueInfo {
        provider: IssueProvider::Github,
        key,
        title: str_at(v, "/title").unwrap_or_default().to_string(),
        state,
        status: status.to_string(),
        assignee: str_at(v, "/assignee/login").map(str::to_string),
        pull,
        url: url.to_string(),
    }
}

fn jira_info(v: &Value, key: &str, url: &str) -> Result<IssueInfo, String> {
    let fields = v.get("fields").ok_or("応答を読めません")?;
    let state = match str_at(fields, "/status/statusCategory/key") {
        Some("done") => IssueState::Done,
        Some("indeterminate") => IssueState::InProgress,
        _ => IssueState::Open,
    };
    Ok(IssueInfo {
        provider: IssueProvider::Jira,
        key: key.to_string(),
        title: str_at(fields, "/summary").unwrap_or_default().to_string(),
        state,
        status: str_at(fields, "/status/name").unwrap_or("?").to_string(),
        assignee: str_at(fields, "/assignee/displayName").map(str::to_string),
        pull: false,
        url: url.to_string(),
    })
}

pub async fn lookup(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<IssueQuery>,
) -> ApiResult<Json<Vec<IssueLookup>>> {
    let user = api::current_user(&state, &headers)?;
    let (meta, role) = api::board_access(
        &state,
        &id,
        user.as_ref().map(|u| u.login.as_str()),
        q.key.as_deref(),
    )?;
    if !role.can_edit() {
        return Err(Error::forbidden());
    }
    // 課題の URL もボードの中身なので、暗号化ボードではサーバーに渡さない。
    crate::e2e::require_plain(meta.as_ref())?;
    if req.urls.len() > MAX_URLS * 10 {
        return Err(Error::bad_request("URL が多すぎます"));
    }
    let mut urls = req.urls;
    urls.sort();
    urls.dedup();
    let cfg = &state.config.issues;
    let results = futures_util::future::join_all(urls.iter().map(|u| cfg.lookup(u.trim()))).await;
    Ok(Json(
        urls.into_iter()
            .zip(results)
            .map(|(url, r)| match r {
                Ok(issue) => IssueLookup {
                    url,
                    issue: Some(issue),
                    error: None,
                },
                Err(e) => IssueLookup {
                    url,
                    issue: None,
                    error: Some(e),
                },
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn github_states() {
        let url = "https://github.com/a/b/issues/1";
        let i = github_info(
            &json!({"state": "open", "title": "T", "assignee": {"login": "al"}}),
            "a/b#1".into(),
            false,
            url,
        );
        assert_eq!((i.state, i.status.as_str()), (IssueState::Open, "Open"));
        assert_eq!(i.assignee.as_deref(), Some("al"));
        let i = github_info(
            &json!({"state": "closed", "state_reason": "not_planned"}),
            "a/b#1".into(),
            false,
            url,
        );
        assert_eq!(i.state, IssueState::Closed);
        let i = github_info(
            &json!({"state": "closed", "pull_request": {"merged_at": "2026-01-01T00:00:00Z"}}),
            "a/b#1".into(),
            false,
            url,
        );
        assert!(i.pull);
        assert_eq!((i.state, i.status.as_str()), (IssueState::Done, "Merged"));
        let i = github_info(
            &json!({"state": "open", "draft": true, "pull_request": {}}),
            "a/b#1".into(),
            true,
            url,
        );
        assert_eq!(i.state, IssueState::InProgress);
    }

    #[test]
    fn jira_states() {
        let v = json!({"fields": {"summary": "S", "status": {"name": "進行中", "statusCategory": {"key": "indeterminate"}}, "assignee": null}});
        let i = jira_info(&v, "P-1", "u").unwrap();
        assert_eq!(i.state, IssueState::InProgress);
        assert_eq!(i.status, "進行中");
        assert!(i.assignee.is_none());
    }
}
