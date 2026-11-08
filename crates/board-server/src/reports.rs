//! ボードの通報と、管理者による利用停止。
//!
//! ボードを開ける人は誰でも通報でき、サーバー管理者(チームのボードはチームの管理者も)に届く。
//! 管理者はボードを利用停止にでき、そのボードは誰も開けなくなる(内容は消えない。解除もできる)。
//! 通報・利用停止・対応は操作ログに残す。

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use board_core::api::{HandleReport, NewReport, REPORT_REASONS, Report, ReportStatus};
use serde::Deserialize;

use crate::AppState;
use crate::api::{ApiResult, Error, board_access, current_user, require_user};
use crate::audit::{self, Entry, GLOBAL, team_scope};
use crate::auth::now_ms;
use crate::boards::{self, Suspension};
use crate::store::{REPORTS, Store, StoreError, next_id};
use crate::teams;

/// 通報のコメントの最大文字数。
pub const MAX_REPORT_COMMENT: usize = 2000;
/// 通報に添えるボードの文字の最大バイト数。
pub const MAX_EVIDENCE_BYTES: usize = 64 * 1024;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/boards/{id}/reports", post(report))
        .route("/reports", get(list_reports))
        .route("/reports/{rid}", put(handle_report))
}

/// ボードを通報する(開ける人なら誰でも)。
async fn report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<crate::api::KeyQuery>,
    Json(req): Json<NewReport>,
) -> ApiResult<(StatusCode, Json<Report>)> {
    let user = current_user(&state, &headers)?;
    let login = user.as_ref().map(|u| u.login.clone());
    let (meta, _) = board_access(&state, &id, login.as_deref(), q.key.as_deref())?;
    if !REPORT_REASONS.iter().any(|(k, _)| *k == req.reason) {
        return Err(Error::bad_request("通報の理由を選んでください"));
    }
    let comment: String = req
        .comment
        .trim()
        .chars()
        .take(MAX_REPORT_COMMENT)
        .collect();
    let evidence = req
        .evidence
        .map(|e| truncate(e.trim(), MAX_EVIDENCE_BYTES).to_string())
        .filter(|e| !e.is_empty());
    let r = Report {
        id: next_id(),
        at: now_ms(),
        board: id.clone(),
        board_title: meta
            .as_ref()
            .map_or_else(|| id.clone(), |m| m.title.clone()),
        team: meta.as_ref().and_then(|m| m.team.clone()),
        reporter: login.clone(),
        reason: req.reason,
        comment,
        evidence,
        status: ReportStatus::Open,
        handled_by: None,
        handled_at: 0,
        suspended: false,
    };
    save_report(state.store(), &r)?;
    let mut entry = Entry::new("board.report")
        .board(&id)
        .team(r.team.as_deref())
        .detail(format!(
            "{}{}",
            board_core::api::report_reason_label(&r.reason),
            if r.evidence.is_some() {
                "(ボードの文字を添付)"
            } else {
                ""
            }
        ));
    if let Some(l) = &login {
        entry = entry.actor(l);
    }
    audit::record(&state, &headers, entry);
    tracing::warn!(board = %id, reason = %r.reason, "ボードが通報されました");
    Ok((StatusCode::CREATED, Json(r)))
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn save_report(store: &Store, r: &Report) -> Result<(), StoreError> {
    store.put_seq(REPORTS, GLOBAL, r.id, r)?;
    if let Some(t) = &r.team {
        store.put_seq(REPORTS, &team_scope(t), r.id, r)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct ReportQuery {
    /// チームの通報だけ(チームの管理者)。なければ全体(サーバー管理者)。
    team: Option<String>,
}

/// 通報を見てよい範囲。
fn scope_for(state: &AppState, login: &str, team: Option<&str>) -> ApiResult<String> {
    match team {
        Some(t) => {
            let (_, role) = teams::member_team(state.store(), t, login)?;
            if !role.is_admin() && !state.config.is_admin(login) {
                return Err(Error::forbidden());
            }
            Ok(team_scope(t))
        }
        None if state.config.is_admin(login) => Ok(GLOBAL.to_string()),
        None => Err(Error::forbidden()),
    }
}

/// 届いた通報(新しい順)。
async fn list_reports(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<Vec<Report>>> {
    let user = require_user(&state, &headers)?;
    let scope = scope_for(&state, &user.login, q.team.as_deref())?;
    let mut list: Vec<Report> =
        state
            .store()
            .scan_seq_rev(REPORTS, &scope, u64::MAX, audit::MAX_LIMIT, |_| true)?;
    for r in &mut list {
        r.suspended = boards::get(state.store(), &r.board)?.is_some_and(|m| m.suspended.is_some());
    }
    Ok(Json(list))
}

/// 通報に対応する。利用停止にすると、そのボードは誰も開けなくなり、今つないでいる人も切れる。
async fn handle_report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(rid): Path<u64>,
    Query(q): Query<ReportQuery>,
    Json(req): Json<HandleReport>,
) -> ApiResult<Json<Report>> {
    let user = require_user(&state, &headers)?;
    let scope = scope_for(&state, &user.login, q.team.as_deref())?;
    let mut r: Report = state
        .store()
        .get_seq(REPORTS, &scope, rid)?
        .ok_or_else(|| Error::new(StatusCode::NOT_FOUND, "通報が見つかりません"))?;
    r.status = req.status;
    r.handled_by = Some(user.login.clone());
    r.handled_at = now_ms();
    if let Some(suspend) = req.suspend
        && let Some(mut meta) = boards::get(state.store(), &r.board)?
    {
        let changed = suspend != meta.suspended.is_some();
        meta.suspended = suspend.then(|| Suspension {
            by: user.login.clone(),
            at: now_ms(),
        });
        if changed {
            boards::put(state.store(), &meta)?;
            if suspend {
                state.hub.kick(&r.board);
            }
            audit::record(
                &state,
                &headers,
                Entry::new(if suspend {
                    "board.suspend"
                } else {
                    "board.unsuspend"
                })
                .actor(&user.login)
                .board(&r.board)
                .team(meta.team.as_deref())
                .detail(format!("通報 {rid} への対応")),
            );
        }
    }
    save_report(state.store(), &r)?;
    r.suspended = boards::get(state.store(), &r.board)?.is_some_and(|m| m.suspended.is_some());
    audit::record(
        &state,
        &headers,
        Entry::new("report.handle")
            .actor(&user.login)
            .board(&r.board)
            .team(r.team.as_deref())
            .detail(match r.status {
                ReportStatus::Open => "未対応に戻す",
                ReportStatus::Resolved => "対応済み",
                ReportStatus::Dismissed => "問題なし",
            }),
    );
    Ok(Json(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate("あいう", 4), "あ");
        assert_eq!(truncate("abc", 10), "abc");
    }
}
