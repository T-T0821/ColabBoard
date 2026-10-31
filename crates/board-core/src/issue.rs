//! GitHub・Jira の課題カード。
//!
//! 課題の URL を貼ると、番号・タイトル・状態を書いた角丸のカードを置く。カードは
//! ふつうの図形([`ShapeKind::RoundRect`])で、`icon` が `issue:github` / `issue:jira`、
//! リンクが課題の URL。状態はサーバー経由で引き直し、変わっていれば文字と色を書き換える。

use glam::Vec2;

use crate::api::{IssueInfo, IssueProvider, IssueState};
use crate::color::Rgba8;
use crate::element::ElementId;
use crate::shape::{Shape, ShapeKind, TextAlign};

pub const CARD_SIZE: Vec2 = Vec2::new(320.0, 120.0);
const ICON_PREFIX: &str = "issue:";
/// 一度に貼れる URL の数。
pub const MAX_URLS: usize = 20;

/// URL から読み取った課題の場所。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueRef {
    Github {
        owner: String,
        repo: String,
        number: u64,
        pull: bool,
    },
    Jira {
        /// `https://example.atlassian.net`(末尾 `/` なし)。
        site: String,
        key: String,
    },
}

impl IssueRef {
    pub fn provider(&self) -> IssueProvider {
        match self {
            IssueRef::Github { .. } => IssueProvider::Github,
            IssueRef::Jira { .. } => IssueProvider::Jira,
        }
    }

    /// 表示用の番号。
    pub fn key(&self) -> String {
        match self {
            IssueRef::Github {
                owner,
                repo,
                number,
                ..
            } => format!("{owner}/{repo}#{number}"),
            IssueRef::Jira { key, .. } => key.clone(),
        }
    }
}

fn name_ok(s: &str) -> bool {
    (1..=100).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && s != "."
        && s != ".."
}

fn jira_key_ok(s: &str) -> bool {
    let Some((project, num)) = s.split_once('-') else {
        return false;
    };
    (1..=20).contains(&project.len())
        && project.as_bytes()[0].is_ascii_uppercase()
        && project
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && (1..=10).contains(&num.len())
        && num.bytes().all(|b| b.is_ascii_digit())
}

/// 課題の URL を読み取る。
///
/// - GitHub: `https://github.com/{owner}/{repo}/issues/{n}`、`…/pull/{n}`
/// - Jira: `https://{サイト}/browse/{KEY-123}`(サイトがどこかはサーバーが確かめる)
pub fn parse_url(url: &str) -> Option<IssueRef> {
    let url = url.trim();
    let rest = url.strip_prefix("https://")?;
    // クエリとフラグメントは無視する。
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    if host == "github.com" || host == "www.github.com" {
        let [owner, repo, kind, number, ..] = parts.as_slice() else {
            return None;
        };
        let pull = match *kind {
            "issues" => false,
            "pull" => true,
            _ => return None,
        };
        // `…/pull/1/files` なども同じ PR として扱う。
        if parts.len() > 5 || (parts.len() == 5 && !pull) {
            return None;
        }
        if !name_ok(owner) || !name_ok(repo) {
            return None;
        }
        let number: u64 = number.parse().ok().filter(|n| *n > 0)?;
        return Some(IssueRef::Github {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
            pull,
        });
    }
    let valid_host = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b':'));
    match parts.as_slice() {
        ["browse", key] if valid_host && jira_key_ok(key) => Some(IssueRef::Jira {
            site: format!("https://{host}"),
            key: key.to_string(),
        }),
        _ => None,
    }
}

/// 貼り付けた文字が課題の URL だけ(1 行に 1 つ)なら、その URL の一覧。
pub fn urls_in_paste(text: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() || lines.len() > MAX_URLS {
        return None;
    }
    lines
        .iter()
        .map(|l| parse_url(l).map(|_| l.to_string()))
        .collect()
}

pub fn is_issue_card(s: &Shape) -> bool {
    s.icon.starts_with(ICON_PREFIX) && !s.meta.link.is_empty()
}

pub fn icon_name(provider: IssueProvider) -> String {
    format!("{ICON_PREFIX}{}", provider.as_str())
}

fn state_colors(state: Option<IssueState>) -> (Rgba8, Rgba8) {
    // (塗り, 枠)
    match state {
        Some(IssueState::Open) => (Rgba8::hex(0xe6f4ea), Rgba8::hex(0x1a7f37)),
        Some(IssueState::InProgress) => (Rgba8::hex(0xe8f0fe), Rgba8::hex(0x1f6feb)),
        Some(IssueState::Done) => (Rgba8::hex(0xf1e8fd), Rgba8::hex(0x8250df)),
        Some(IssueState::Closed) => (Rgba8::hex(0xeeeeee), Rgba8::hex(0x6e7781)),
        None => (Rgba8::hex(0xfafafa), Rgba8::hex(0xb0b0b0)),
    }
}

fn card_text(info: &IssueInfo) -> String {
    let kind = if info.pull { "PR " } else { "" };
    let mut s = format!(
        "{} {kind}{} · {}\n{}",
        info.provider.label(),
        info.key,
        info.status,
        info.title.chars().take(200).collect::<String>()
    );
    if let Some(a) = &info.assignee {
        s.push_str(&format!("\n担当: {a}"));
    }
    s
}

/// まだ状態を引けていないカード(URL だけ分かっている)。
pub fn placeholder(id: ElementId, pos: Vec2, url: &str, r: &IssueRef) -> Shape {
    let mut s = Shape::new(id, ShapeKind::RoundRect, pos, CARD_SIZE, Rgba8::WHITE);
    let (fill, stroke) = state_colors(None);
    s.fill = fill;
    s.stroke = stroke;
    s.stroke_width = 2.0;
    s.align = TextAlign::Left;
    s.font_size = 16.0;
    s.text_color = Rgba8::BLACK;
    s.icon = icon_name(r.provider());
    s.meta.link = url.to_string();
    s.text = format!("{} {}\n読み込み中…", r.provider().label(), r.key());
    s
}

/// 取れなかったことをカードに書く(前に取れた内容があれば残す)。
pub fn mark_error(s: &mut Shape, message: &str) -> bool {
    if !s.text.ends_with("読み込み中…") {
        return false;
    }
    let head = s.text.lines().next().unwrap_or_default().to_string();
    s.text = format!("{head}\n取得できません: {message}");
    true
}

/// カードを課題の情報に合わせる。変わったら true。
pub fn apply(s: &mut Shape, info: &IssueInfo) -> bool {
    let before = s.clone();
    let (fill, stroke) = state_colors(Some(info.state));
    s.fill = fill;
    s.stroke = stroke;
    s.text = card_text(info);
    s.icon = icon_name(info.provider);
    s.fit_text_height();
    *s != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_urls() {
        assert_eq!(
            parse_url("https://github.com/T-T0821/ColabBoard/issues/12"),
            Some(IssueRef::Github {
                owner: "T-T0821".into(),
                repo: "ColabBoard".into(),
                number: 12,
                pull: false
            })
        );
        let pr = parse_url("https://github.com/rust-lang/rust/pull/123/files?w=1").unwrap();
        assert_eq!(pr.key(), "rust-lang/rust#123");
        assert!(matches!(pr, IssueRef::Github { pull: true, .. }));
        assert!(parse_url("https://github.com/a/b/issues/0").is_none());
        assert!(parse_url("https://github.com/a/b/issues/1/x").is_none());
        assert!(parse_url("https://github.com/a/b/tree/main").is_none());
        assert!(parse_url("http://github.com/a/b/issues/1").is_none());
        assert!(parse_url("https://github.com/../b/issues/1").is_none());
    }

    #[test]
    fn jira_urls() {
        assert_eq!(
            parse_url("https://Example.atlassian.net/browse/PROJ-42?focused=1"),
            Some(IssueRef::Jira {
                site: "https://example.atlassian.net".into(),
                key: "PROJ-42".into()
            })
        );
        assert!(parse_url("https://example.atlassian.net/browse/proj-42").is_none());
        assert!(parse_url("https://example.atlassian.net/browse/PROJ").is_none());
        assert!(parse_url("https://example.atlassian.net/jira/PROJ-1").is_none());
    }

    #[test]
    fn paste_detection() {
        let two = "https://github.com/a/b/issues/1\n  https://x.atlassian.net/browse/AB-2 \n";
        assert_eq!(urls_in_paste(two).unwrap().len(), 2);
        assert!(urls_in_paste("見て https://github.com/a/b/issues/1").is_none());
        assert!(urls_in_paste("").is_none());
    }

    #[test]
    fn cards() {
        let url = "https://github.com/a/b/pull/7";
        let r = parse_url(url).unwrap();
        let mut s = placeholder(ElementId(1), Vec2::ZERO, url, &r);
        assert!(is_issue_card(&s));
        assert!(s.text.contains("a/b#7"));
        assert!(mark_error(&mut s, "404"));
        assert!(s.text.contains("取得できません"));
        let info = IssueInfo {
            provider: IssueProvider::Github,
            key: "a/b#7".into(),
            title: "直す".into(),
            state: IssueState::Done,
            status: "Merged".into(),
            assignee: Some("alice".into()),
            pull: true,
            url: url.into(),
        };
        assert!(apply(&mut s, &info));
        assert_eq!(s.text, "GitHub PR a/b#7 · Merged\n直す\n担当: alice");
        assert!(!apply(&mut s, &info));
        // 取れた後のエラーでは内容を消さない。
        assert!(!mark_error(&mut s, "x"));
    }
}
