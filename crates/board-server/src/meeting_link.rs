//! 会議ツール(Zoom・Google Meet・Teams)とカレンダーの予定をボードに紐づける。
//!
//! - 紐づけはボードの情報([`BoardMeta`](crate::boards::BoardMeta))に保存する。
//!   見られるのはボードを開ける人(閲覧者以上)、変えられるのは編集者以上。
//! - 会議の URL は Zoom・Meet・Teams のものだけ受け付ける(ボードを開いた人を任意のサイトへ誘導させないため)。
//!   環境変数 `MEETING_HOSTS`(カンマ区切りのドメイン)で、Webex など他の会議ツールを足せる。
//! - 変えたら `Message::Refresh(TOPIC_MEETING_LINK)` でボードを開いている人に知らせる。
//! - Google カレンダーの予定作成リンク・`.ics`・招待文はクライアントが
//!   [`board_core::meeting_link`] で作る(サーバーは保存と検査だけ)。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use board_core::meeting_link::{self as meeting, MeetingLink, SetMeetingLink, UrlError};
use board_core::protocol::TOPIC_MEETING_LINK;

use crate::AppState;
use crate::api::{self, ApiResult, Error, KeyQuery};
use crate::audit::{self, Entry};
use crate::auth;
use crate::boards;

/// 環境変数 `MEETING_HOSTS` を読む。
pub fn hosts_from_env() -> Vec<String> {
    std::env::var("MEETING_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty() && s.contains('.'))
        .collect()
}

pub async fn get_meeting(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> ApiResult<Json<MeetingLink>> {
    let user = api::current_user(&state, &headers)?;
    let (meta, _) = api::board_access(
        &state,
        &id,
        user.as_ref().map(|u| u.login.as_str()),
        q.key.as_deref(),
    )?;
    Ok(Json(meta.and_then(|m| m.meeting_link).unwrap_or_default()))
}

/// 受け取った内容を確かめて、保存する形にする。
pub fn validate(
    req: SetMeetingLink,
    extra_hosts: &[String],
    who: &str,
    now: u64,
) -> Result<Option<MeetingLink>, String> {
    if req.is_empty() {
        return Ok(None);
    }
    let trimmed = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let url = trimmed(req.url);
    let provider = match &url {
        Some(u) => Some(meeting::check_meeting_url(u, extra_hosts).map_err(|e| {
            match e {
                UrlError::Malformed => {
                    "会議の URL は https:// で始まる形で入力してください".to_string()
                }
                UrlError::Host => {
                    "会議の URL は Zoom・Google Meet・Microsoft Teams のものを入力してください"
                        .to_string()
                }
            }
        })?),
        None => None,
    };
    let calendar_url = trimmed(req.calendar_url);
    if let Some(u) = &calendar_url {
        meeting::check_calendar_url(u).map_err(|_| {
            "予定の URL は Google カレンダーか Outlook の予定のリンクを入力してください".to_string()
        })?;
    }
    let title = trimmed(req.title).map(|t| {
        t.chars()
            .filter(|c| !c.is_control())
            .take(meeting::MAX_TITLE_CHARS)
            .collect::<String>()
    });
    let (start, end) = match (req.start, req.end) {
        (None, Some(_)) => return Err("終わりだけでなく開始の時刻も入れてください".into()),
        (Some(s), Some(e)) if e <= s => {
            return Err("終わりの時刻は開始より後にしてください".into());
        }
        (Some(s), Some(e)) if e - s > meeting::MAX_DURATION_MS => {
            return Err("予定の長さは 24 時間までにしてください".into());
        }
        other => other,
    };
    Ok(Some(MeetingLink {
        url,
        provider,
        title,
        start,
        end,
        calendar_url,
        updated_by: Some(who.to_string()),
        updated: now,
    }))
}

pub async fn set_meeting(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    Json(req): Json<SetMeetingLink>,
) -> ApiResult<Json<MeetingLink>> {
    let user = api::require_user(&state, &headers)?;
    let (meta, role) = api::board_access(&state, &id, Some(&user.login), q.key.as_deref())?;
    if !role.can_edit() {
        return Err(Error::forbidden());
    }
    let mut meta = meta.ok_or_else(|| {
        Error::bad_request("会議を紐づけられるのは、ログインして作ったボードだけです")
    })?;
    let now = auth::now_ms();
    let next =
        validate(req, &state.config.meeting_hosts, &user.login, now).map_err(Error::bad_request)?;
    let removed = next.is_none();
    meta.meeting_link = next;
    meta.updated = now;
    boards::put(state.store(), &meta)?;
    audit::record(
        &state,
        &headers,
        Entry::new("board.meeting_link")
            .actor(&user.login)
            .board(&id)
            .team(meta.team.clone())
            .detail(match &meta.meeting_link {
                Some(m) => m.provider.map_or("予定", |p| p.label()).to_string(),
                None if removed => "外した".to_string(),
                None => String::new(),
            }),
    );
    state.hub.refresh(&id, TOPIC_MEETING_LINK);
    Ok(Json(meta.meeting_link.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::meeting_link::MeetingProvider;

    fn req(url: &str) -> SetMeetingLink {
        SetMeetingLink {
            url: Some(url.into()),
            ..Default::default()
        }
    }

    #[test]
    fn validates() {
        let m = validate(req(" https://zoom.us/j/1 "), &[], "alice", 5)
            .unwrap()
            .unwrap();
        assert_eq!(m.url.as_deref(), Some("https://zoom.us/j/1"));
        assert_eq!(m.provider, Some(MeetingProvider::Zoom));
        assert_eq!(m.updated_by.as_deref(), Some("alice"));
        assert!(validate(req("https://evil.example/"), &[], "a", 0).is_err());
        assert!(validate(req("javascript:alert(1)"), &[], "a", 0).is_err());
        assert_eq!(validate(SetMeetingLink::default(), &[], "a", 0), Ok(None));
        let bad_time = SetMeetingLink {
            start: Some(10),
            end: Some(5),
            ..Default::default()
        };
        assert!(validate(bad_time, &[], "a", 0).is_err());
        let only_end = SetMeetingLink {
            end: Some(5),
            ..Default::default()
        };
        assert!(validate(only_end, &[], "a", 0).is_err());
        let cal = SetMeetingLink {
            calendar_url: Some("https://example.com/cal".into()),
            ..Default::default()
        };
        assert!(validate(cal, &[], "a", 0).is_err());
        let long = SetMeetingLink {
            title: Some("あ".repeat(500)),
            ..Default::default()
        };
        let m = validate(long, &[], "a", 0).unwrap().unwrap();
        assert_eq!(m.title.unwrap().chars().count(), meeting::MAX_TITLE_CHARS);
    }
}
