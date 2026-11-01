//! 会議ツール(Zoom・Google Meet・Microsoft Teams)とカレンダーの連携。
//!
//! ボードに「会議の参加 URL」と「予定(名前・開始・終了)」を紐づけておくと、
//!
//! - ボードの上部から会議に参加できる(開始が近づくとボタンが目立つ)。
//! - Google カレンダーに、ボードの URL を説明に入れた予定を作れる([`google_calendar_url`])。
//! - Outlook・Apple カレンダー・Teams 向けに `.ics` を書き出せる([`ics`])。
//! - 会議のチャットに貼る招待文を作れる([`invite_text`])。会議の参加者はそこからボードを開く。
//!
//! URL の検査([`check_meeting_url`]・[`check_calendar_url`])はクライアントとサーバーの両方で使う。
//! 時刻はすべて UNIX ミリ秒(UTC)。

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};

/// 予定の名前の最大文字数。
pub const MAX_TITLE_CHARS: usize = 120;
/// URL の最大バイト数。
pub const MAX_URL_LEN: usize = 1024;
/// 予定の長さの上限(1 日)。
pub const MAX_DURATION_MS: u64 = 24 * 60 * 60 * 1000;
/// 終わりがないときの長さ(1 時間)。
pub const DEFAULT_DURATION_MS: u64 = 60 * 60 * 1000;
/// 開始の何分前から「まもなく開始」とするか。
pub const SOON_MS: u64 = 15 * 60 * 1000;

/// 会議ツール。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeetingProvider {
    Zoom,
    Meet,
    Teams,
    /// サーバー設定 `MEETING_HOSTS` で足した、その他の会議ツール。
    Other,
}

impl MeetingProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Zoom => "Zoom",
            Self::Meet => "Google Meet",
            Self::Teams => "Microsoft Teams",
            Self::Other => "オンライン会議",
        }
    }
}

/// ボードに紐づけた会議と予定。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meeting {
    /// 会議の参加 URL。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// 参加 URL から判定した会議ツール。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<MeetingProvider>,
    /// 予定の名前。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 開始・終了(UNIX ミリ秒)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    /// カレンダーの予定の URL(Google カレンダー・Outlook の予定を開くリンク)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_url: Option<String>,
    /// 最後に変えた人と時刻。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    #[serde(default)]
    pub updated: u64,
}

/// `PUT /api/boards/{id}/meeting` の中身。すべて空なら紐づけを外す。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMeeting {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub start: Option<u64>,
    #[serde(default)]
    pub end: Option<u64>,
    #[serde(default)]
    pub calendar_url: Option<String>,
}

impl SetMeeting {
    pub fn is_empty(&self) -> bool {
        let blank = |s: &Option<String>| s.as_deref().is_none_or(|s| s.trim().is_empty());
        blank(&self.url)
            && blank(&self.title)
            && blank(&self.calendar_url)
            && self.start.is_none()
            && self.end.is_none()
    }
}

/// 会議の状態。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    /// 時刻が決まっていない。
    Unscheduled,
    /// まだ先。
    Later,
    /// 開始 15 分前から開始まで。
    Soon,
    /// 開催中。
    Live,
    /// 終わった。
    Ended,
}

impl Meeting {
    /// 終了時刻(なければ開始から 1 時間)。
    pub fn end_or_default(&self) -> Option<u64> {
        let start = self.start?;
        Some(
            self.end
                .filter(|e| *e > start)
                .unwrap_or(start + DEFAULT_DURATION_MS),
        )
    }

    pub fn timing(&self, now: u64) -> Timing {
        let (Some(start), Some(end)) = (self.start, self.end_or_default()) else {
            return Timing::Unscheduled;
        };
        if now >= end {
            Timing::Ended
        } else if now >= start {
            Timing::Live
        } else if start - now <= SOON_MS {
            Timing::Soon
        } else {
            Timing::Later
        }
    }

    /// 表示用の名前(なければ会議ツールの名前かボード名)。
    pub fn display_title(&self, board_title: &str) -> String {
        if let Some(t) = self.title.as_deref().filter(|t| !t.is_empty()) {
            return t.to_string();
        }
        if board_title.is_empty() {
            "会議".to_string()
        } else {
            board_title.to_string()
        }
    }
}

/// URL の検査の結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlError {
    /// `https://` で始まっていない・長すぎる・空白を含むなど。
    Malformed,
    /// 認めていないホスト。
    Host,
}

/// URL を `https://` とホストに分ける。ホストは小文字。
fn https_host(url: &str) -> Result<&str, UrlError> {
    if url.len() > MAX_URL_LEN || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(UrlError::Malformed);
    }
    let rest = url
        .get(..8)
        .filter(|s| s.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])
        .ok_or(UrlError::Malformed)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // ユーザー名付き(`https://zoom.us@evil.example/`)やポート指定は認めない。
    if authority.is_empty()
        || authority.contains('@')
        || authority.contains(':')
        || authority.contains('\\')
    {
        return Err(UrlError::Malformed);
    }
    Ok(authority)
}

/// `host` が `domain` そのものか、そのサブドメインか。
fn host_is(host: &str, domain: &str) -> bool {
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case(domain)
        || (host.len() > domain.len()
            && host[host.len() - domain.len()..].eq_ignore_ascii_case(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

/// 会議ツールのホスト。
const ZOOM: &[&str] = &["zoom.us", "zoom.com", "zoomgov.com"];
const MEET: &[&str] = &["meet.google.com"];
const TEAMS: &[&str] = &[
    "teams.microsoft.com",
    "teams.live.com",
    "teams.microsoft.us",
    "teams.cloud.microsoft",
    "gov.teams.microsoft.us",
];

/// 会議の参加 URL を確かめ、会議ツールを返す。
/// `extra_hosts` はサーバー設定で足したホスト(サブドメインも含む)。
pub fn check_meeting_url(url: &str, extra_hosts: &[String]) -> Result<MeetingProvider, UrlError> {
    let host = https_host(url)?;
    let any = |list: &[&str]| list.iter().any(|d| host_is(host, d));
    if any(ZOOM) {
        Ok(MeetingProvider::Zoom)
    } else if any(MEET) {
        Ok(MeetingProvider::Meet)
    } else if any(TEAMS) {
        Ok(MeetingProvider::Teams)
    } else if extra_hosts
        .iter()
        .any(|d| !d.is_empty() && host_is(host, d.trim()))
    {
        Ok(MeetingProvider::Other)
    } else {
        Err(UrlError::Host)
    }
}

/// カレンダーの予定の URL を確かめる(Google カレンダーと Outlook)。
pub fn check_calendar_url(url: &str) -> Result<(), UrlError> {
    let host = https_host(url)?;
    const HOSTS: &[&str] = &[
        "calendar.google.com",
        "calendar.app.google",
        "outlook.office.com",
        "outlook.office365.com",
        "outlook.live.com",
        "outlook.cloud.microsoft",
    ];
    if HOSTS.iter().any(|d| host_is(host, d)) {
        Ok(())
    } else {
        Err(UrlError::Host)
    }
}

/// UNIX ミリ秒を `20261103T010000Z`(UTC)にする。
pub fn utc_stamp(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// 1970-01-01 からの日数を年月日に(Howard Hinnant のアルゴリズム)。
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

/// 予定の説明(カレンダー・招待文で共通)。
fn description(m: &Meeting, board_title: &str, board_url: &str) -> String {
    let mut s = format!("ボード「{board_title}」: {board_url}");
    if let Some(url) = &m.url {
        let label = m.provider.map_or("会議", MeetingProvider::label);
        s.push_str(&format!("\n{label} に参加: {url}"));
    }
    s
}

/// Google カレンダーの「予定を作成」画面の URL。ボードの URL を説明に、会議の URL を場所に入れる。
/// 時刻がなければ時刻なしで開く(カレンダー側で選ぶ)。
pub fn google_calendar_url(m: &Meeting, board_title: &str, board_url: &str) -> String {
    let enc = |s: &str| utf8_percent_encode(s, NON_ALPHANUMERIC).to_string();
    let mut url = format!(
        "https://calendar.google.com/calendar/render?action=TEMPLATE&text={}&details={}",
        enc(&m.display_title(board_title)),
        enc(&description(m, board_title, board_url)),
    );
    if let (Some(start), Some(end)) = (m.start, m.end_or_default()) {
        url.push_str(&format!("&dates={}/{}", utc_stamp(start), utc_stamp(end)));
    }
    if let Some(loc) = &m.url {
        url.push_str(&format!("&location={}", enc(loc)));
    }
    url
}

/// iCalendar の文字(`\` `;` `,` 改行)を逃がす。
fn ics_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace(['\n', '\r'], "\\n")
}

/// 1 行 75 オクテットで折り返す(RFC 5545 3.1)。UTF-8 の文字の途中では切らない。
fn ics_fold(line: &str) -> String {
    let mut out = String::new();
    let mut len = 0;
    for c in line.chars() {
        let n = c.len_utf8();
        if len + n > 75 {
            out.push_str("\r\n ");
            len = 1;
        }
        out.push(c);
        len += n;
    }
    out.push_str("\r\n");
    out
}

/// `.ics`(iCalendar)の中身。時刻のない予定は `None`。
/// `uid_domain` は UID の後ろに付けるドメイン(公開 URL のホストなど)。
pub fn ics(
    m: &Meeting,
    board_id: &str,
    board_title: &str,
    board_url: &str,
    uid_domain: &str,
    now: u64,
) -> Option<String> {
    let start = m.start?;
    let end = m.end_or_default()?;
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//colabboard//meeting//JA".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
        "METHOD:PUBLISH".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:board-{board_id}-{start}@{uid_domain}"),
        format!("DTSTAMP:{}", utc_stamp(now)),
        format!("DTSTART:{}", utc_stamp(start)),
        format!("DTEND:{}", utc_stamp(end)),
        format!("SUMMARY:{}", ics_escape(&m.display_title(board_title))),
        format!(
            "DESCRIPTION:{}",
            ics_escape(&description(m, board_title, board_url))
        ),
        format!("URL:{board_url}"),
    ];
    if let Some(url) = &m.url {
        lines.push(format!("LOCATION:{}", ics_escape(url)));
    }
    lines.push("END:VEVENT".to_string());
    lines.push("END:VCALENDAR".to_string());
    Some(lines.iter().map(|l| ics_fold(l)).collect())
}

/// 会議のチャットや招待メールに貼る文。
pub fn invite_text(m: &Meeting, board_title: &str, board_url: &str, when: Option<&str>) -> String {
    let mut s = format!("📋 {}\n", m.display_title(board_title));
    if let Some(w) = when {
        s.push_str(&format!("🕒 {w}\n"));
    }
    s.push_str(&format!("ボード: {board_url}\n"));
    if let Some(url) = &m.url {
        let label = m.provider.map_or("会議", MeetingProvider::label);
        s.push_str(&format!("{label}: {url}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers() {
        let ok = |u: &str| check_meeting_url(u, &[]);
        assert_eq!(
            ok("https://us02web.zoom.us/j/123?pwd=abc"),
            Ok(MeetingProvider::Zoom)
        );
        assert_eq!(ok("https://zoom.us/j/1"), Ok(MeetingProvider::Zoom));
        assert_eq!(
            ok("https://meet.google.com/abc-defg-hij"),
            Ok(MeetingProvider::Meet)
        );
        assert_eq!(
            ok("https://teams.microsoft.com/l/meetup-join/19%3a..."),
            Ok(MeetingProvider::Teams)
        );
        assert_eq!(
            ok("https://teams.live.com/meet/9"),
            Ok(MeetingProvider::Teams)
        );
        assert_eq!(ok("HTTPS://Meet.Google.com/x"), Ok(MeetingProvider::Meet));
        // なりすまし・危険な形。
        assert_eq!(ok("http://zoom.us/j/1"), Err(UrlError::Malformed));
        assert_eq!(ok("javascript:alert(1)"), Err(UrlError::Malformed));
        assert_eq!(
            ok("https://zoom.us@evil.example/"),
            Err(UrlError::Malformed)
        );
        assert_eq!(ok("https://evilzoom.us/j/1"), Err(UrlError::Host));
        assert_eq!(ok("https://zoom.us.evil.example/j/1"), Err(UrlError::Host));
        assert_eq!(ok("https://zoom.us:8443/j/1"), Err(UrlError::Malformed));
        assert_eq!(ok("https://zoom.us/j/1 2"), Err(UrlError::Malformed));
        assert_eq!(ok("https://example.com/"), Err(UrlError::Host));
        // 足したホスト。
        assert_eq!(
            check_meeting_url("https://acme.webex.com/meet/x", &["webex.com".into()]),
            Ok(MeetingProvider::Other)
        );
    }

    #[test]
    fn calendar_urls() {
        assert!(check_calendar_url("https://calendar.google.com/calendar/event?eid=abc").is_ok());
        assert!(check_calendar_url("https://outlook.office.com/calendar/item/x").is_ok());
        assert!(check_calendar_url("https://calendar.google.com.evil.example/").is_err());
        assert!(check_calendar_url("data:text/html,x").is_err());
    }

    #[test]
    fn stamps() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        // 2026-11-03 01:00:00 UTC
        assert_eq!(utc_stamp(1_793_667_600_000), "20261103T010000Z");
        // うるう日 2024-02-29 12:34:56 UTC
        assert_eq!(utc_stamp(1_709_210_096_000), "20240229T123456Z");
    }

    fn sample() -> Meeting {
        Meeting {
            url: Some("https://meet.google.com/abc-defg-hij".into()),
            provider: Some(MeetingProvider::Meet),
            title: Some("定例, 第3回".into()),
            start: Some(1_793_667_600_000),
            end: None,
            ..Meeting::default()
        }
    }

    #[test]
    fn timing() {
        let m = sample();
        let s = m.start.unwrap();
        assert_eq!(m.timing(s - SOON_MS - 1), Timing::Later);
        assert_eq!(m.timing(s - 60_000), Timing::Soon);
        assert_eq!(m.timing(s), Timing::Live);
        assert_eq!(m.timing(s + DEFAULT_DURATION_MS), Timing::Ended);
        assert_eq!(Meeting::default().timing(s), Timing::Unscheduled);
    }

    #[test]
    fn google_calendar() {
        let url = google_calendar_url(&sample(), "企画", "https://b.example/?room=r1");
        assert!(url.starts_with("https://calendar.google.com/calendar/render?action=TEMPLATE"));
        assert!(url.contains("&dates=20261103T010000Z/20261103T020000Z"));
        assert!(url.contains("&location=https%3A%2F%2Fmeet%2Egoogle%2Ecom"));
        assert!(url.contains("room%3Dr1"));
        // 時刻なし。
        let m = Meeting::default();
        assert!(!google_calendar_url(&m, "企画", "u").contains("dates="));
    }

    #[test]
    fn ics_file() {
        let s = ics(
            &sample(),
            "r1",
            "企画",
            "https://b.example/?room=r1",
            "b.example",
            0,
        )
        .unwrap();
        assert!(s.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(s.contains("DTSTART:20261103T010000Z\r\n"));
        assert!(s.contains("DTEND:20261103T020000Z\r\n"));
        assert!(s.contains("SUMMARY:定例\\, 第3回\r\n"));
        assert!(s.contains("UID:board-r1-1793667600000@b.example"));
        assert!(s.ends_with("END:VCALENDAR\r\n"));
        for line in s.split("\r\n") {
            assert!(line.len() <= 75, "{line}");
        }
        assert!(ics(&Meeting::default(), "r1", "t", "u", "d", 0).is_none());
    }

    #[test]
    fn invite() {
        let t = invite_text(
            &sample(),
            "企画",
            "https://b.example/?room=r1",
            Some("11/3 10:00"),
        );
        assert!(t.contains("ボード: https://b.example/?room=r1"));
        assert!(t.contains("Google Meet: https://meet.google.com/abc-defg-hij"));
        assert!(t.contains("🕒 11/3 10:00"));
    }

    #[test]
    fn empty_request() {
        assert!(SetMeeting::default().is_empty());
        assert!(
            SetMeeting {
                url: Some("  ".into()),
                ..Default::default()
            }
            .is_empty()
        );
        assert!(
            !SetMeeting {
                start: Some(1),
                ..Default::default()
            }
            .is_empty()
        );
    }
}
