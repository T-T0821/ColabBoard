//! 操作ログ(監査ログ)。
//!
//! ログイン・ボードやチームの作成と削除・共有設定の変更などを、誰がいつどこから行ったか記録する。
//! 記録は全体のログ(`*`)と、チームに関わるものはチームのログ(`team:{id}`)の両方に書く。
//!
//! - 全体のログはサーバー管理者(`ADMIN_LOGINS`)だけが見られる。
//! - チームのログはそのチームの管理者・オーナーが見られる。
//! - `AUDIT_RETENTION_DAYS` より古い記録は定期的に消す。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use board_core::api::AuditEvent;
use serde::Deserialize;

use crate::AppState;
use crate::api::{Error, require_user};
use crate::store::{AUDIT, Store, StoreError, next_id};
use crate::teams;

/// 全体のログのキー。
pub const GLOBAL: &str = "*";
/// 一度に返す件数の上限。
pub const MAX_LIMIT: usize = 500;
/// ミドルウェアが付ける接続元 IP のヘッダー(外から来たものは消してから付ける)。
pub const CLIENT_IP_HEADER: HeaderName = HeaderName::from_static("x-graboard-client-ip");

pub fn team_scope(team: &str) -> String {
    format!("team:{team}")
}

/// 記録する操作。
#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub action: &'static str,
    pub actor: Option<String>,
    pub board: Option<String>,
    pub team: Option<String>,
    pub target: Option<String>,
    pub detail: Option<String>,
}

impl Entry {
    pub fn new(action: &'static str) -> Self {
        Self {
            action,
            ..Default::default()
        }
    }
    pub fn actor(mut self, login: impl Into<String>) -> Self {
        self.actor = Some(login.into());
        self
    }
    pub fn board(mut self, id: impl Into<String>) -> Self {
        self.board = Some(id.into());
        self
    }
    pub fn team(mut self, id: Option<impl Into<String>>) -> Self {
        self.team = id.map(Into::into);
        self
    }
    pub fn target(mut self, t: impl Into<String>) -> Self {
        self.target = Some(t.into());
        self
    }
    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
}

/// 書き込む(失敗しても操作そのものは止めない)。
pub fn write(store: &Store, entry: Entry, ip: Option<String>) -> Option<AuditEvent> {
    let event = AuditEvent {
        id: next_id(),
        at: crate::auth::now_ms(),
        action: entry.action.to_string(),
        actor: entry.actor,
        board: entry.board,
        team: entry.team,
        target: entry.target,
        detail: entry.detail,
        ip,
    };
    let res = (|| -> Result<(), StoreError> {
        store.put_seq(AUDIT, GLOBAL, event.id, &event)?;
        if let Some(team) = &event.team {
            store.put_seq(AUDIT, &team_scope(team), event.id, &event)?;
        }
        Ok(())
    })();
    match res {
        Ok(()) => Some(event),
        Err(e) => {
            tracing::error!("操作ログを書けませんでした: {e}");
            None
        }
    }
}

/// リクエストの接続元とともに記録する。
pub fn record(state: &AppState, headers: &HeaderMap, entry: Entry) {
    let ip = headers
        .get(&CLIENT_IP_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    write(state.store(), entry, ip);
}

/// 新しい順に読む。`before` より前(ID が小さい)のものだけ。
pub fn list(
    store: &Store,
    scope: &str,
    before: Option<u64>,
    limit: usize,
    action: Option<&str>,
) -> Result<Vec<AuditEvent>, StoreError> {
    let upper = before.map_or(u64::MAX, |b| b.saturating_sub(1));
    store.scan_seq_rev(AUDIT, scope, upper, limit, |e: &AuditEvent| {
        action.is_none_or(|a| e.action == a || e.action.starts_with(&format!("{a}.")))
    })
}

/// 古い記録を消す。消した件数を返す。
pub fn prune(store: &Store, older_than_ms: u64) -> Result<u64, StoreError> {
    store.retain_seq_newer(AUDIT, older_than_ms.saturating_mul(1000))
}

/// 定期的に古い記録を消す(1 時間ごと)。
pub fn spawn_pruner(store: Arc<Store>, retention_days: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let cutoff = crate::auth::now_ms().saturating_sub(retention_days * 24 * 3600 * 1000);
            let store = store.clone();
            match tokio::task::spawn_blocking(move || prune(&store, cutoff)).await {
                Ok(Ok(n)) if n > 0 => tracing::info!(removed = n, "古い操作ログを消しました"),
                Ok(Err(e)) => tracing::warn!("操作ログの整理に失敗: {e}"),
                _ => {}
            }
        }
    })
}

/// 接続元 IP をヘッダーに載せるミドルウェア。
/// 外から同じ名前のヘッダーが来ても消す。`TRUST_PROXY=1` なら `X-Forwarded-For` の先頭を使う。
pub async fn client_ip(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let forwarded = state
        .config
        .trust_proxy
        .then(|| {
            req.headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(|v| v.trim().to_string())
                .filter(|v| v.parse::<std::net::IpAddr>().is_ok())
        })
        .flatten();
    let ip = forwarded.or_else(|| {
        req.extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip().to_string())
    });
    let headers = req.headers_mut();
    headers.remove(&CLIENT_IP_HEADER);
    if let Some(ip) = ip.and_then(|ip| HeaderValue::from_str(&ip).ok()) {
        headers.insert(CLIENT_IP_HEADER, ip);
    }
    next.run(req).await
}

// ---- HTTP ----

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/audit", get(list_events))
        .route("/audit/export", get(export))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    pub team: Option<String>,
    pub before: Option<u64>,
    pub limit: Option<usize>,
    pub action: Option<String>,
    pub format: Option<String>,
}

/// 見てよいログのキー。
fn scope_for(state: &AppState, headers: &HeaderMap, team: Option<&str>) -> Result<String, Error> {
    let user = require_user(state, headers)?;
    match team {
        Some(t) => {
            let (_, role) = teams::member_team(state.store(), t, &user.login)?;
            if !role.is_admin() {
                return Err(Error::forbidden());
            }
            Ok(team_scope(t))
        }
        None if state.config.is_admin(&user.login) => Ok(GLOBAL.to_string()),
        None => Err(Error::forbidden()),
    }
}

async fn list_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Vec<AuditEvent>>, Error> {
    let scope = scope_for(&state, &headers, q.team.as_deref())?;
    let limit = q.limit.unwrap_or(100).clamp(1, MAX_LIMIT);
    Ok(Json(list(
        state.store(),
        &scope,
        q.before,
        limit,
        q.action.as_deref().filter(|a| !a.is_empty()),
    )?))
}

async fn export(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Result<Response, Error> {
    let scope = scope_for(&state, &headers, q.team.as_deref())?;
    let events = list(
        state.store(),
        &scope,
        q.before,
        usize::MAX,
        q.action.as_deref().filter(|a| !a.is_empty()),
    )?;
    let csv = q.format.as_deref() != Some("jsonl");
    let (body, ctype, ext) = if csv {
        (to_csv(&events), "text/csv; charset=utf-8", "csv")
    } else {
        (to_jsonl(&events), "application/x-ndjson", "jsonl")
    };
    let name = match &q.team {
        Some(t) => format!("audit-{t}.{ext}"),
        None => format!("audit.{ext}"),
    };
    Ok((
        [
            (CONTENT_TYPE, ctype.to_string()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        body,
    )
        .into_response())
}

pub fn to_jsonl(events: &[AuditEvent]) -> String {
    let mut out = String::new();
    for e in events {
        if let Ok(line) = serde_json::to_string(e) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// CSV(Excel で開けるよう先頭に BOM を付ける)。
pub fn to_csv(events: &[AuditEvent]) -> String {
    let mut out =
        String::from("\u{feff}id,time,action,label,actor,board,team,target,detail,ip\r\n");
    for e in events {
        let cells = [
            e.id.to_string(),
            iso8601(e.at),
            e.action.clone(),
            e.label().to_string(),
            e.actor.clone().unwrap_or_default(),
            e.board.clone().unwrap_or_default(),
            e.team.clone().unwrap_or_default(),
            e.target.clone().unwrap_or_default(),
            e.detail.clone().unwrap_or_default(),
            e.ip.clone().unwrap_or_default(),
        ];
        let row: Vec<String> = cells.iter().map(|c| csv_cell(c)).collect();
        out.push_str(&row.join(","));
        out.push_str("\r\n");
    }
    out
}

fn csv_cell(s: &str) -> String {
    // 表計算ソフトで式として解釈されないようにする。
    let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// UNIX ミリ秒を `2026-10-31T12:34:56.789Z` に。
pub fn iso8601(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // 1970-01-01 からの日数を年月日に(Howard Hinnant の方法)。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_list_prune() {
        let store = Store::memory();
        let a = write(
            &store,
            Entry::new("auth.login").actor("alice"),
            Some("10.0.0.1".into()),
        )
        .unwrap();
        let b = write(
            &store,
            Entry::new("team.member")
                .actor("alice")
                .team(Some("t1"))
                .target("bob")
                .detail("member"),
            None,
        )
        .unwrap();
        let all = list(&store, GLOBAL, None, 10, None).unwrap();
        assert_eq!(
            all.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![b.id, a.id]
        );
        let team = list(&store, &team_scope("t1"), None, 10, None).unwrap();
        assert_eq!(team.len(), 1);
        assert_eq!(team[0].target.as_deref(), Some("bob"));
        // 絞り込みとページ送り。
        assert_eq!(
            list(&store, GLOBAL, None, 10, Some("auth")).unwrap().len(),
            1
        );
        assert_eq!(
            list(&store, GLOBAL, None, 10, Some("aut")).unwrap().len(),
            0
        );
        assert_eq!(
            list(&store, GLOBAL, Some(b.id), 10, None).unwrap()[0].id,
            a.id
        );
        assert_eq!(list(&store, GLOBAL, None, 1, None).unwrap().len(), 1);
        // 全部消える。
        assert_eq!(prune(&store, b.at + 1).unwrap(), 3);
        assert!(list(&store, GLOBAL, None, 10, None).unwrap().is_empty());
    }

    #[test]
    fn csv_escaping() {
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_cell("=1+1"), "'=1+1");
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_793_450_096_789), "2026-10-31T12:34:56.789Z");
        let e = AuditEvent {
            id: 1,
            at: 0,
            action: "board.create".into(),
            actor: Some("alice".into()),
            board: Some("b1".into()),
            team: None,
            target: None,
            detail: Some("企画, 会議".into()),
            ip: None,
        };
        let csv = to_csv(std::slice::from_ref(&e));
        assert!(csv.contains("ボード作成"));
        assert!(csv.contains("\"企画, 会議\""));
        assert_eq!(to_jsonl(&[e]).lines().count(), 1);
    }
}
