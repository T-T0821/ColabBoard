//! ボードを探す: 自分用のお気に入り・タグ・フォルダと、開けるボードすべてをまたいだ検索。
//!
//! - `PUT /api/boards/{id}/labels` [`SetLabels`] → [`BoardInfo`]
//! - `POST /api/folders/rename` [`RenameFolder`] → `{ "changed": n }`
//! - `GET /api/search?q=` → [`SearchHit`] の配列(よく一致した順)
//!
//! 検索語は空白で区切り、すべてを含むボードを返す(名前・タグ・フォルダ・中身のどこかに)。
//! `#企画` のように `#` を付けた語はタグだけを見る。大文字と小文字、全角と半角の英数字、
//! ひらがなとカタカナは区別しない。中身はサーバーにあるボードの状態から読む
//! (付箋・図形・テキスト・表のセル・フレームの見出し・コネクタのラベル)。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use board_core::api::{
    BoardInfo, MatchKind, RenameFolder, SearchHit, SearchMatch, SetLabels, clean_folder,
};
use board_core::{Board, Element};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{ApiResult, Error, require_user};
use crate::boards;
use crate::room::Hub;
use crate::{AppState, valid_room_name};

/// 検索語の長さの上限。
pub const MAX_QUERY_CHARS: usize = 100;
/// 返すボードの数の上限。
pub const MAX_RESULTS: usize = 50;
/// 1 つのボードで返す中身の一致の数。
pub const MAX_CONTENT_MATCHES: usize = 3;
/// 抜き出しの前後に付ける文字数。
const SNIPPET_AROUND: usize = 24;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/boards/{id}/labels", put(set_labels))
        .route("/folders/rename", post(rename_folder))
        .route("/search", get(search))
}

async fn set_labels(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SetLabels>,
) -> ApiResult<Json<BoardInfo>> {
    let user = require_user(&state, &headers)?;
    if !valid_room_name(&id) {
        return Err(Error::not_found());
    }
    // 一覧に出ている(開ける)ボードだけ。
    let list = boards::list_for(state.store(), &user.login, state.config.anonymous_boards)?;
    let Some(mut info) = list.into_iter().find(|b| b.id == id) else {
        return Err(Error::not_found());
    };
    if let Some(f) = &req.folder
        && !f.trim().is_empty()
        && clean_folder(f).is_none()
    {
        return Err(Error::bad_request("フォルダの名前を入れてください"));
    }
    info.labels = boards::set_labels(state.store(), &user.login, &id, &req)?;
    Ok(Json(info))
}

async fn rename_folder(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RenameFolder>,
) -> ApiResult<Json<Value>> {
    let user = require_user(&state, &headers)?;
    let changed = boards::rename_folder(state.store(), &user.login, &req.from, &req.to)?;
    Ok(Json(json!({ "changed": changed })))
}

#[derive(Deserialize)]
struct Params {
    #[serde(default)]
    q: String,
}

async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<Params>,
) -> ApiResult<Json<Vec<SearchHit>>> {
    let user = require_user(&state, &headers)?;
    let query = SearchQuery::parse(&q.q);
    if query.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let list = boards::list_for(state.store(), &user.login, state.config.anonymous_boards)?;
    let hub = state.hub.clone();
    let hits = tokio::task::spawn_blocking(move || run(&hub, list, &query))
        .await
        .map_err(|e| Error::from(Box::new(e) as crate::store::StoreError))?;
    Ok(Json(hits))
}

/// ボードの中身を読んで探す。読めないボードは名前などだけで判定する。
fn run(hub: &Hub, list: Vec<BoardInfo>, query: &SearchQuery) -> Vec<SearchHit> {
    let mut scored: Vec<(i64, SearchHit)> = Vec::new();
    for info in list {
        let texts = match hub.current_state(&info.id) {
            Ok(Some(state)) => {
                let mut board = Board::new();
                match board.apply_remote(&state) {
                    Ok(()) => board_texts(&board),
                    Err(e) => {
                        tracing::warn!(board = %info.id, "検索でボードを読めません: {e}");
                        Vec::new()
                    }
                }
            }
            Ok(None) => Vec::new(),
            Err(e) => {
                tracing::warn!(board = %info.id, "検索でボードを読めません: {e}");
                Vec::new()
            }
        };
        if let Some(hit) = match_board(info, &texts, query) {
            scored.push(hit);
        }
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            let t = |h: &SearchHit| h.board.labels.opened.max(h.board.updated);
            t(&b.1).cmp(&t(&a.1))
        })
    });
    scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, h)| h)
        .collect()
}

/// ボードの中の文字(要素 ID と本文)。ボード上の順。
pub fn board_texts(board: &Board) -> Vec<(u64, String)> {
    board
        .elements()
        .iter()
        .filter_map(|e| {
            let text = match e {
                Element::Shape(s) => s.text.trim(),
                Element::Connector(c) => c.text.trim(),
                Element::Stroke(_) => return None,
            };
            (!text.is_empty()).then(|| (e.id().0, text.to_string()))
        })
        .collect()
}

/// 解釈した検索語。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SearchQuery {
    /// どこに含まれてもよい語(正規化済み)。
    pub words: Vec<String>,
    /// タグだけを見る語(`#` を付けたもの、正規化済み)。
    pub tags: Vec<String>,
}

impl SearchQuery {
    pub fn parse(raw: &str) -> Self {
        let raw: String = raw.chars().take(MAX_QUERY_CHARS).collect();
        let mut q = Self::default();
        for w in raw.split(|c: char| c.is_whitespace()) {
            if let Some(t) = w.strip_prefix(['#', '＃']) {
                let t = normalize(t);
                if !t.is_empty() && !q.tags.contains(&t) {
                    q.tags.push(t);
                }
            } else {
                let w = normalize(w);
                if !w.is_empty() && !q.words.contains(&w) {
                    q.words.push(w);
                }
            }
        }
        q
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.tags.is_empty()
    }
}

/// 比べるための形にする。1 文字を 1 文字に置き換えるので、位置は元の文字列と対応する。
fn fold(c: char) -> char {
    let c = match c {
        // 全角の英数字・記号 → 半角。
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        '\u{3000}' => ' ',
        // カタカナ → ひらがな。
        '\u{30A1}'..='\u{30F6}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
        _ => c,
    };
    let mut low = c.to_lowercase();
    match (low.next(), low.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

pub fn normalize(s: &str) -> String {
    s.chars().map(fold).collect()
}

/// `hay`(元の文字列)の中で `needle`(正規化済み)が始まる文字位置。
fn find_chars(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| {
        hay[i..i + needle.len()]
            .iter()
            .zip(needle)
            .all(|(h, n)| fold(*h) == *n)
    })
}

/// 一致した部分の前後を切り出す(改行は空白に)。一致した位置(切り出しの中の文字位置と長さ)も返す。
pub fn snippet(text: &str, word: &str) -> (String, Option<[usize; 2]>) {
    let chars: Vec<char> = text.chars().collect();
    let needle: Vec<char> = word.chars().collect();
    let flat = |cs: &[char]| -> String {
        cs.iter()
            .map(|c| if c.is_whitespace() { ' ' } else { *c })
            .collect::<String>()
    };
    let Some(at) = find_chars(&chars, &needle) else {
        let head: Vec<char> = chars.iter().take(SNIPPET_AROUND * 2).copied().collect();
        let more = chars.len() > head.len();
        return (
            format!("{}{}", flat(&head), if more { "…" } else { "" }),
            None,
        );
    };
    let start = at.saturating_sub(SNIPPET_AROUND);
    let end = (at + needle.len() + SNIPPET_AROUND).min(chars.len());
    let lead = usize::from(start > 0);
    (
        format!(
            "{}{}{}",
            if start > 0 { "…" } else { "" },
            flat(&chars[start..end]),
            if end < chars.len() { "…" } else { "" }
        ),
        Some([at - start + lead, needle.len()]),
    )
}

#[cfg(test)]
fn contains(hay: &str, word: &str) -> bool {
    normalize(hay).contains(word)
}

/// ボードが検索語に合えば、点数と一致した場所。
pub fn match_board(
    info: BoardInfo,
    texts: &[(u64, String)],
    q: &SearchQuery,
) -> Option<(i64, SearchHit)> {
    let tags_norm: Vec<String> = info.labels.tags.iter().map(|t| normalize(t)).collect();
    // `#` 付きの語はタグに含まれること。
    if !q
        .tags
        .iter()
        .all(|t| tags_norm.iter().any(|x| x.contains(t.as_str())))
    {
        return None;
    }
    let title = normalize(&info.title);
    let folder = info.labels.folder.as_deref().map(normalize);
    let content_norm: Vec<String> = texts.iter().map(|(_, t)| normalize(t)).collect();
    let mut score: i64 = 0;
    let mut matches: Vec<SearchMatch> = Vec::new();
    for w in &q.words {
        let in_title = title.contains(w.as_str());
        let in_tag = tags_norm.iter().any(|t| t.contains(w.as_str()));
        let in_folder = folder.as_deref().is_some_and(|f| f.contains(w.as_str()));
        let in_content = content_norm.iter().any(|t| t.contains(w.as_str()));
        if !(in_title || in_tag || in_folder || in_content) {
            return None;
        }
        if in_title {
            score += if title == *w { 200 } else { 100 };
        }
        if in_tag {
            score += 50;
        }
        if in_folder {
            score += 30;
        }
    }
    let words_or_tags: Vec<&String> = if q.words.is_empty() {
        q.tags.iter().collect()
    } else {
        q.words.iter().collect()
    };
    if q.words.iter().any(|w| title.contains(w.as_str())) {
        let w = q.words.iter().find(|w| title.contains(w.as_str())).unwrap();
        let (text, highlight) = snippet(&info.title, w);
        matches.push(SearchMatch {
            kind: MatchKind::Title,
            text,
            highlight,
            element: None,
        });
    }
    for (tag, norm) in info.labels.tags.iter().zip(&tags_norm) {
        if words_or_tags.iter().any(|w| norm.contains(w.as_str())) {
            score += if q.words.is_empty() { 50 } else { 0 };
            matches.push(SearchMatch {
                kind: MatchKind::Tag,
                text: tag.clone(),
                highlight: None,
                element: None,
            });
        }
    }
    if let (Some(f), Some(fnorm)) = (&info.labels.folder, &folder)
        && q.words.iter().any(|w| fnorm.contains(w.as_str()))
    {
        matches.push(SearchMatch {
            kind: MatchKind::Folder,
            text: f.clone(),
            highlight: None,
            element: None,
        });
    }
    let mut content_count = 0;
    for ((id, text), norm) in texts.iter().zip(&content_norm) {
        let Some(w) = q.words.iter().find(|w| norm.contains(w.as_str())) else {
            continue;
        };
        content_count += 1;
        if content_count <= MAX_CONTENT_MATCHES {
            let (text, highlight) = snippet(text, w);
            matches.push(SearchMatch {
                kind: MatchKind::Content,
                text,
                highlight,
                element: Some(format!("{id:x}")),
            });
        }
    }
    score += content_count.min(20) as i64;
    if info.labels.favorite {
        score += 10;
    }
    Some((
        score,
        SearchHit {
            board: info,
            matches,
            content_count,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::api::{BoardLabels, Role};

    fn info(title: &str, tags: &[&str], folder: Option<&str>) -> BoardInfo {
        BoardInfo {
            id: "b".into(),
            title: title.into(),
            owner: None,
            role: Role::Editor,
            created: 0,
            updated: 0,
            key: None,
            team: None,
            labels: BoardLabels {
                favorite: false,
                tags: tags.iter().map(|t| t.to_string()).collect(),
                folder: folder.map(str::to_string),
                opened: 0,
            },
            e2e: None,
        }
    }

    #[test]
    fn parses_queries() {
        let q = SearchQuery::parse("  企画　#Ｑ３  ＡＢＣ abc ");
        assert_eq!(q.words, vec!["企画".to_string(), "abc".to_string()]);
        assert_eq!(q.tags, vec!["q3".to_string()]);
        assert!(SearchQuery::parse("   ").is_empty());
    }

    #[test]
    fn folds_width_case_and_kana() {
        assert_eq!(normalize("ＡbＣ１"), "abc1");
        assert_eq!(normalize("カタカナ"), "かたかな");
        assert!(contains("リリース計画", &normalize("りりーす")));
    }

    #[test]
    fn all_words_must_match_somewhere() {
        let texts = vec![(0x1a, "来週のリリース\n手順".to_string())];
        let q = SearchQuery::parse("企画 リリース");
        let (_, hit) = match_board(info("新規企画", &[], None), &texts, &q).unwrap();
        assert_eq!(hit.content_count, 1);
        assert_eq!(hit.matches[0].kind, MatchKind::Title);
        let c = hit
            .matches
            .iter()
            .find(|m| m.kind == MatchKind::Content)
            .unwrap();
        assert_eq!(c.element.as_deref(), Some("1a"));
        assert_eq!(c.text, "来週のリリース 手順");
        assert!(
            match_board(
                info("新規企画", &[], None),
                &texts,
                &SearchQuery::parse("企画 予算")
            )
            .is_none()
        );
    }

    #[test]
    fn tags_and_folders() {
        let b = info("定例", &["営業", "Q3"], Some("2026年"));
        assert!(match_board(b.clone(), &[], &SearchQuery::parse("#q3")).is_some());
        assert!(match_board(b.clone(), &[], &SearchQuery::parse("#企画")).is_none());
        let (_, hit) = match_board(b.clone(), &[], &SearchQuery::parse("2026")).unwrap();
        assert_eq!(hit.matches[0].kind, MatchKind::Folder);
        let (s_title, _) = match_board(b.clone(), &[], &SearchQuery::parse("定例")).unwrap();
        let (s_tag, _) = match_board(b, &[], &SearchQuery::parse("営業")).unwrap();
        assert!(s_title > s_tag);
    }

    #[test]
    fn snippets_are_cut_around_the_match() {
        let long = format!("{}見つけたい言葉{}", "あ".repeat(40), "い".repeat(40));
        let (s, hl) = snippet(&long, &normalize("見つけたい"));
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert!(s.contains("見つけたい言葉"));
        let [at, len] = hl.unwrap();
        let marked: String = s.chars().skip(at).take(len).collect();
        assert_eq!(marked, "見つけたい");
        assert_eq!(snippet("短い", "短"), ("短い".to_string(), Some([0, 1])));
        // 大文字・全角でも元の文字の位置を指す。
        let (s, hl) = snippet("Ｒｅｌｅａｓｅ Plan", "release");
        assert_eq!(s, "Ｒｅｌｅａｓｅ Plan");
        assert_eq!(hl, Some([0, 7]));
    }
}
