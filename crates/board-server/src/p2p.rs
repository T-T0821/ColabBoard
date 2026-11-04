//! サーバーを通さない直接接続(P2P)の見張り。
//!
//! 直接接続ではボードの中身がサーバーを通らないので、サーバーにできるのは次のことだけ:
//!
//! - 使ってよいかを決める(サーバー全体は `P2P_ENABLED`、チームのボードはチームの管理者が切り替える)。
//!   クライアントはつなぐ前にここへ問い合わせ、最後に聞いた答えを端末に覚えておく。
//! - 使った記録(誰が・いつ・どのボードで招待/参加/接続/切断したか)を操作ログに残す。中身は受け取らない。
//! - 参加者からの通報を操作ログに残す(管理者が `p2p.report` で絞り込んで見る)。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use board_core::api::{MAX_REPORT_NOTE_CHARS, P2pEvent, P2pPolicy, P2pReport, SetTeamP2p, Team};

use crate::AppState;
use crate::api::{ApiResult, Error, KeyQuery, board_access, current_user, require_user};
use crate::audit::{self, Entry};
use crate::boards::BoardMeta;
use crate::teams;

/// 一度に受け付ける記録の数(オフラインの間にためた分)。
pub const MAX_EVENTS: usize = 100;
const MAX_PEER_NAME_CHARS: usize = 60;

/// このボードで直接接続を使えるか。
pub fn policy(state: &AppState, meta: Option<&BoardMeta>) -> ApiResult<P2pPolicy> {
    if !state.config.p2p {
        return Ok(P2pPolicy {
            allowed: false,
            reason: Some("このサーバーでは直接接続(P2P)が止められています".into()),
        });
    }
    if let Some(tid) = meta.and_then(|m| m.team.as_deref())
        && let Some(team) = teams::get_team(state.store(), tid)?
        && team.p2p_disabled
    {
        return Ok(P2pPolicy {
            allowed: false,
            reason: Some(format!(
                "チーム「{}」の管理者が直接接続(P2P)を止めています",
                team.name
            )),
        });
    }
    Ok(P2pPolicy {
        allowed: true,
        reason: None,
    })
}

fn clip(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

fn format_time(ms: u64) -> String {
    // 操作ログの時刻はサーバーが受け取った時刻。端末の時刻は詳細に添える(UTC)。
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// 1970-01-01 からの日数を年月日にする(Howard Hinnant の方法)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub async fn get_policy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<P2pPolicy>> {
    let user = current_user(&state, &headers)?;
    let login = user.as_ref().map(|u| u.login.as_str());
    let (meta, _) = board_access(&state, &id, login, q.key.as_deref())?;
    Ok(Json(policy(&state, meta.as_ref())?))
}

/// 使った記録を操作ログに残す。止められているボードなら 403 を返す(クライアントは切断する)。
pub async fn post_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(events): Json<Vec<P2pEvent>>,
) -> ApiResult<StatusCode> {
    let user = current_user(&state, &headers)?;
    let login = user.as_ref().map(|u| u.login.clone());
    let (meta, role) = board_access(&state, &id, login.as_deref(), q.key.as_deref())?;
    if events.len() > MAX_EVENTS {
        return Err(Error::bad_request("記録が多すぎます"));
    }
    let team = meta.as_ref().and_then(|m| m.team.clone());
    // 止められていても、止められる前の記録は残す。
    for e in &events {
        let mut detail = format!("端末の時刻 {}", format_time(e.at));
        if let Some(peer) = e.peer.as_deref().map(|p| clip(p, MAX_PEER_NAME_CHARS))
            && !peer.is_empty()
        {
            detail.push_str(&format!(" · 相手 {peer}"));
        }
        if login.is_none() {
            detail.push_str(&format!(" · ログインなし({})", role.label()));
        }
        let mut entry = Entry::new(e.kind.action())
            .board(&id)
            .team(team.as_deref())
            .detail(detail);
        if let Some(l) = &login {
            entry = entry.actor(l);
        }
        audit::record(&state, &headers, entry);
    }
    let p = policy(&state, meta.as_ref())?;
    if !p.allowed {
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            p.reason.unwrap_or_default(),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// 直接つないだ相手を通報する。中身(ボード)は送らず、理由と補足だけを操作ログに残す。
pub async fn post_report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<P2pReport>,
) -> ApiResult<StatusCode> {
    let user = current_user(&state, &headers)?;
    let login = user.as_ref().map(|u| u.login.clone());
    let (meta, _) = board_access(&state, &id, login.as_deref(), q.key.as_deref())?;
    let peer = clip(&req.peer, MAX_PEER_NAME_CHARS);
    let note = clip(&req.note, MAX_REPORT_NOTE_CHARS);
    let mut detail = format!(
        "{} · 相手 {} · 端末の時刻 {}",
        req.reason.label(),
        if peer.is_empty() { "(不明)" } else { &peer },
        format_time(req.at)
    );
    if !note.is_empty() {
        detail.push_str(&format!(" · {note}"));
    }
    let mut entry = Entry::new("p2p.report")
        .board(&id)
        .team(meta.as_ref().and_then(|m| m.team.as_deref()))
        .target(req.reason.id())
        .detail(detail);
    if let Some(l) = &login {
        entry = entry.actor(l);
    }
    audit::record(&state, &headers, entry);
    tracing::warn!(board = %id, reason = req.reason.id(), "直接接続の相手が通報されました");
    Ok(StatusCode::NO_CONTENT)
}

/// チームのボードで直接接続を許すか(チームの管理者以上)。
pub async fn set_team_p2p(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(tid): Path<String>,
    Json(req): Json<SetTeamP2p>,
) -> ApiResult<Json<Team>> {
    let user = require_user(&state, &headers)?;
    let (mut team, role) = teams::member_team(state.store(), &tid, &user.login)?;
    if !role.is_admin() {
        return Err(Error::forbidden());
    }
    team.p2p_disabled = !req.allowed;
    teams::put_team(state.store(), &team)?;
    audit::record(
        &state,
        &headers,
        Entry::new("team.p2p")
            .actor(&user.login)
            .team(Some(&tid))
            .detail(if req.allowed {
                "直接接続(P2P)を許可"
            } else {
                "直接接続(P2P)を停止"
            }),
    );
    Ok(Json(team.view(role)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format() {
        assert_eq!(format_time(0), "1970-01-01 00:00 UTC");
        // 2026-11-01 04:29:45 UTC
        assert_eq!(format_time(1_793_507_385_000), "2026-11-01 04:29 UTC");
        assert_eq!(format_time(951_782_400_000), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn clip_strips_control_chars() {
        assert_eq!(clip(" a\u{0}b\nc ", 10), "abc");
        assert_eq!(clip("あいうえお", 3), "あいう");
    }
}
