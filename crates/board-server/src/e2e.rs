//! エンドツーエンド暗号化ボードの決まりごとと、悪用への備え。
//!
//! 暗号化ボードの中身はサーバーでは読めない(鍵はブラウザにしかない)。そのぶん運営側が中身を
//! 確かめられないので、次の 3 つで備える。
//!
//! - **使えるかを管理者が決める**: サーバー全体はサーバー管理者が(既定は環境変数 `E2E_BOARDS`、
//!   画面から変えるとそちらが優先)、チームのボードはチームの管理者が(既定は不可)オン・オフする。
//!   止めても、すでにある暗号化ボードは持ち主が開ける(中身を失わせない)。新しく作れなくなる。
//! - **使った記録を操作ログに残す**: 作成・参加・鍵の作り直し・通報・利用停止を、誰がいつ行ったかだけ記録する。
//!   中身は記録しない(できない)。参加は同じ人・同じボードで [`OPEN_LOG_INTERVAL_MS`] に 1 回まで。
//! - **参加者が通報できる**: ボードを開ける人は通報でき、サーバー管理者(チームのボードはチームの管理者も)
//!   に届く。暗号化ボードではサーバーが中身を読めないので、通報する人が自分の判断で
//!   ボードの文字を添えられる。管理者はボードを利用停止にできる。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use base64::Engine;
use board_core::api::{
    E2eInfo, E2ePolicy, HandleReport, NewReport, REPORT_REASONS, Report, ReportStatus, Role,
    RotateE2e, SetTeamE2e, Team,
};
use serde::Deserialize;

use crate::AppState;
use crate::api::{ApiResult, Error, board_access, current_user, require_user};
use crate::audit::{self, Entry, GLOBAL, team_scope};
use crate::auth::now_ms;
use crate::boards::{self, BoardMeta, Suspension};
use crate::store::{REPORTS, SETTINGS, Store, StoreError, next_id};
use crate::teams::{self, TeamRecord};

/// 参加の記録の最短間隔(つなぎ直しのたびに記録しない)。
pub const OPEN_LOG_INTERVAL_MS: u64 = 10 * 60 * 1000;
/// 通報のコメントの最大文字数。
pub const MAX_REPORT_COMMENT: usize = 2000;
/// 通報に添えるボードの文字の最大バイト数。
pub const MAX_EVIDENCE_BYTES: usize = 64 * 1024;
/// 鍵の作り直しで受け取る暗号文の最大バイト数(base64 を戻した後)。
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

const SETTING_KEY: &str = "e2e_boards";

/// 暗号化ボードではこの機能は使えない、というエラー。
pub fn sealed_error() -> Error {
    Error::new(
        StatusCode::CONFLICT,
        "エンドツーエンド暗号化ボードではこの機能は使えません(サーバーが中身を読めないため)",
    )
}

/// 暗号化ボードなら断る(サーバーが中身を読む・中身を受け取る機能の入口で使う)。
pub fn require_plain(meta: Option<&BoardMeta>) -> ApiResult<()> {
    if meta.is_some_and(|m| m.e2e.is_some()) {
        return Err(sealed_error());
    }
    Ok(())
}

/// サーバー全体で暗号化ボードを作れるか。画面で変えた値があればそれ、なければ環境変数の値。
pub fn server_enabled(state: &AppState) -> bool {
    match state.store().get_json::<bool>(SETTINGS, SETTING_KEY) {
        Ok(Some(v)) => v,
        Ok(None) => state.config.e2e_boards,
        Err(e) => {
            tracing::error!("設定を読めません: {e}");
            state.config.e2e_boards
        }
    }
}

/// 暗号化ボードを作れるか(チームのボードならチームの許可も要る)。
pub fn check_allowed(state: &AppState, team: Option<&TeamRecord>) -> ApiResult<()> {
    if !server_enabled(state) {
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            "このサーバーでは暗号化ボードは管理者により無効になっています",
        ));
    }
    if let Some(t) = team
        && !t.e2e
    {
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            "このチームでは暗号化ボードは許可されていません(チームの管理者が設定できます)",
        ));
    }
    Ok(())
}

/// 鍵の指紋として受け付ける形(16 進 32 文字)。
pub fn valid_key_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 参加の記録(中身は残さない)。同じ人・同じボードは一定時間に 1 回まで。
#[derive(Default)]
pub struct OpenLog(Mutex<HashMap<(String, String), u64>>);

impl OpenLog {
    /// 記録すべきなら true。
    pub fn should_log(&self, board: &str, who: &str, now: u64) -> bool {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 10_000 {
            map.retain(|_, at| now.saturating_sub(*at) < OPEN_LOG_INTERVAL_MS);
        }
        let key = (board.to_string(), who.to_string());
        match map.get(&key) {
            Some(at) if now.saturating_sub(*at) < OPEN_LOG_INTERVAL_MS => false,
            _ => {
                map.insert(key, now);
                true
            }
        }
    }
}

/// 暗号化ボードへの参加を操作ログに残す(WebSocket の接続時)。
pub fn log_open(
    state: &AppState,
    headers: &HeaderMap,
    meta: &BoardMeta,
    login: Option<&str>,
    role: Role,
) {
    let who = login.unwrap_or("(共有リンク)");
    if !state.e2e_opens.should_log(&meta.id, who, now_ms()) {
        return;
    }
    let mut entry = Entry::new("board.e2e.open")
        .board(&meta.id)
        .team(meta.team.as_deref())
        .detail(format!("権限: {}", role_label(role)));
    if let Some(l) = login {
        entry = entry.actor(l);
    }
    audit::record(state, headers, entry);
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Viewer => "閲覧",
        Role::Commenter => "コメント",
        Role::Editor => "編集",
        Role::Owner => "オーナー",
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/e2e", get(policy).put(set_policy))
        .route("/boards/{id}/e2e/rotate", post(rotate))
        .route("/boards/{id}/reports", post(report))
        .route("/teams/{tid}/e2e", put(set_team))
        .route("/reports", get(list_reports))
        .route("/reports/{rid}", put(handle_report))
}

/// 自分が暗号化ボードを作れる場所。
async fn policy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<E2ePolicy>> {
    let enabled = server_enabled(&state);
    let teams = match current_user(&state, &headers)? {
        Some(u) if enabled => teams::team_ids_of(state.store(), &u.login)?
            .into_iter()
            .filter(|t| {
                teams::get_team(state.store(), t)
                    .ok()
                    .flatten()
                    .is_some_and(|t| t.e2e)
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(Json(E2ePolicy { enabled, teams }))
}

#[derive(Deserialize)]
struct SetPolicy {
    enabled: bool,
}

/// サーバー全体のオン・オフ(サーバー管理者)。
async fn set_policy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<SetPolicy>,
) -> ApiResult<Json<E2ePolicy>> {
    let user = require_user(&state, &headers)?;
    if !state.config.is_admin(&user.login) {
        return Err(Error::forbidden());
    }
    state
        .store()
        .put_json(SETTINGS, SETTING_KEY, &req.enabled)?;
    audit::record(
        &state,
        &headers,
        Entry::new("server.settings")
            .actor(&user.login)
            .detail(format!(
                "暗号化ボード: {}",
                if req.enabled { "有効" } else { "無効" }
            )),
    );
    Ok(Json(E2ePolicy {
        enabled: req.enabled,
        teams: Vec::new(),
    }))
}

/// チームでのオン・オフ(チームの管理者・オーナー)。
async fn set_team(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(tid): Path<String>,
    Json(req): Json<SetTeamE2e>,
) -> ApiResult<Json<Team>> {
    let user = require_user(&state, &headers)?;
    let (mut team, role) = teams::member_team(state.store(), &tid, &user.login)?;
    if !role.is_admin() {
        return Err(Error::forbidden());
    }
    team.e2e = req.allowed;
    teams::put_team(state.store(), &team)?;
    audit::record(
        &state,
        &headers,
        Entry::new("team.settings")
            .actor(&user.login)
            .team(Some(&tid))
            .detail(format!(
                "暗号化ボード: {}",
                if req.allowed { "許可" } else { "禁止" }
            )),
    );
    Ok(Json(team.view(role)))
}

/// 鍵を作り直す(オーナー)。ログを新しい鍵の暗号文 1 つに置き換え、参加者をつなぎ直させる。
/// 古い鍵を知っている人(外したメンバーなど)は、これ以降の中身を読めなくなる。
async fn rotate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<RotateE2e>,
) -> ApiResult<Json<E2eInfo>> {
    let user = require_user(&state, &headers)?;
    let (meta, role) = board_access(&state, &id, Some(&user.login), None)?;
    let mut meta = meta.ok_or_else(Error::not_found)?;
    if meta.e2e.is_none() {
        return Err(Error::bad_request("暗号化ボードではありません"));
    }
    if role < Role::Owner {
        return Err(Error::forbidden());
    }
    if !valid_key_id(&req.key_id) {
        return Err(Error::bad_request("鍵の指紋の形式が違います"));
    }
    let blob = base64::engine::general_purpose::STANDARD
        .decode(req.snapshot.as_bytes())
        .map_err(|_| Error::bad_request("暗号文の形式が違います"))?;
    if blob.is_empty() || blob.len() > MAX_SNAPSHOT_BYTES {
        return Err(Error::bad_request("暗号文の大きさが正しくありません"));
    }
    let info = E2eInfo {
        key_id: req.key_id,
        rotated: now_ms(),
    };
    meta.e2e = Some(info.clone());
    meta.updated = info.rotated;
    boards::put(state.store(), &meta)?;
    let hub = state.hub.clone();
    let room = id.clone();
    tokio::task::spawn_blocking(move || hub.reset_sealed(&room, blob))
        .await
        .map_err(|e| Error::from(StoreError::from(e.to_string())))?
        .map_err(Error::bad_request)?;
    audit::record(
        &state,
        &headers,
        Entry::new("board.e2e.rotate")
            .actor(&user.login)
            .board(&id)
            .team(meta.team.as_deref()),
    );
    Ok(Json(info))
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
        e2e: meta.as_ref().is_some_and(|m| m.e2e.is_some()),
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
    fn key_ids() {
        assert!(valid_key_id("0123456789abcdef0123456789abcdef"));
        assert!(!valid_key_id("0123456789ABCDEF0123456789abcdef"));
        assert!(!valid_key_id("short"));
        assert!(!valid_key_id("0123456789abcdef0123456789abcdeg"));
    }

    #[test]
    fn open_log_throttles() {
        let log = OpenLog::default();
        assert!(log.should_log("b", "alice", 1_000));
        assert!(!log.should_log("b", "alice", 2_000));
        assert!(log.should_log("b", "bob", 2_000));
        assert!(log.should_log("c", "alice", 2_000));
        assert!(log.should_log("b", "alice", 1_000 + OPEN_LOG_INTERVAL_MS));
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate("あいう", 4), "あ");
        assert_eq!(truncate("abc", 10), "abc");
    }
}
