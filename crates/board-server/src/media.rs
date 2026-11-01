//! 動画メモ: ブラウザで録画した短い動画をサーバーに置き、ボードからは URL で参照する。
//!
//! - `POST /api/boards/{id}/media?key=`: 本文が動画(`Content-Type: video/webm` か `video/mp4`)。
//!   編集できる人だけ。`{ "id": "…", "url": "/api/boards/{id}/media/{mid}" }` を返す。
//! - `GET /api/boards/{id}/media/{mid}?key=`: 動画を返す。ボードを開ける人だけ。
//!   シークできるよう `Range: bytes=` に応える。
//!
//! 動画は CRDT に入れない(大きいので)。ボードを消すと一緒に消す。

use std::sync::Arc;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::{self, Error, KeyQuery};
use crate::store::{MEDIA, MEDIA_INFO, Store, StoreError, next_id};

/// 1 本の上限(1 分ほどの動画メモが収まる大きさ)。
pub const MAX_MEDIA_BYTES: usize = 40 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MediaInfo {
    pub mime: String,
    pub bytes: usize,
    pub created: u64,
    pub author: Option<String>,
}

#[derive(Serialize)]
pub struct Uploaded {
    pub id: String,
    pub url: String,
}

/// 受け付ける種類。`video/webm;codecs=vp9,opus` のような指定は種類の部分だけ見る。
pub fn accepted_mime(content_type: &str) -> Option<&'static str> {
    let base = content_type.split(';').next()?.trim().to_ascii_lowercase();
    match base.as_str() {
        "video/webm" => Some("video/webm"),
        "video/mp4" => Some("video/mp4"),
        _ => None,
    }
}

pub fn put(
    store: &Store,
    board: &str,
    data: &[u8],
    mime: &str,
    author: Option<&str>,
) -> Result<u64, StoreError> {
    let id = next_id();
    let info = MediaInfo {
        mime: mime.into(),
        bytes: data.len(),
        created: crate::auth::now_ms(),
        author: author.map(Into::into),
    };
    store.put_seq_raw(MEDIA, board, id, data)?;
    store.put_seq(MEDIA_INFO, board, id, &info)?;
    Ok(id)
}

pub fn get(
    store: &Store,
    board: &str,
    id: u64,
) -> Result<Option<(MediaInfo, Vec<u8>)>, StoreError> {
    let Some(info) = store.get_seq::<MediaInfo>(MEDIA_INFO, board, id)? else {
        return Ok(None);
    };
    Ok(store.get_seq_raw(MEDIA, board, id)?.map(|d| (info, d)))
}

pub fn delete_all(store: &Store, board: &str) -> Result<(), StoreError> {
    store.clear_seq(MEDIA, board)?;
    store.clear_seq(MEDIA_INFO, board)
}

/// `Range: bytes=a-b` を解釈する(1 区間だけ)。範囲外なら `None`。
pub fn parse_range(header: &str, len: usize) -> Option<(usize, usize)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            // 末尾から n バイト。
            let n: usize = n.parse().ok()?;
            if n == 0 {
                return None;
            }
            (len.saturating_sub(n), len - 1)
        }
        (s, "") => (s.parse().ok()?, len - 1),
        (s, e) => {
            let s: usize = s.parse().ok()?;
            let e: usize = e.parse().ok()?;
            (s, e.min(len - 1))
        }
    };
    if start > end || start >= len {
        return None;
    }
    Some((start, end))
}

pub async fn upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
    body: Bytes,
) -> Result<Json<Uploaded>, Error> {
    let user = api::current_user(&state, &headers)?;
    let login = user.as_ref().map(|u| u.login.as_str());
    let (meta, role) = api::board_access(&state, &id, login, q.key.as_deref())?;
    if !role.can_edit() {
        return Err(Error::forbidden());
    }
    // 動画はサーバーに平文で置くので、暗号化ボードでは受け付けない。
    crate::e2e::require_plain(meta.as_ref())?;
    let mime = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(accepted_mime)
        .ok_or_else(|| Error::bad_request("WebM か MP4 の動画を送ってください"))?;
    if body.is_empty() {
        return Err(Error::bad_request("動画が空です"));
    }
    if body.len() > MAX_MEDIA_BYTES {
        return Err(Error::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("動画は {} MB までです", MAX_MEDIA_BYTES / 1024 / 1024),
        ));
    }
    let store = state.store().clone();
    let board = id.clone();
    let login = login.map(String::from);
    let mid =
        tokio::task::spawn_blocking(move || put(&store, &board, &body, mime, login.as_deref()))
            .await
            .map_err(|_| Error::new(StatusCode::INTERNAL_SERVER_ERROR, "保存に失敗しました"))??;
    tracing::info!(board = %id, media = mid, "動画メモを保存");
    Ok(Json(Uploaded {
        id: mid.to_string(),
        url: format!("/api/boards/{id}/media/{mid}"),
    }))
}

pub async fn download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, mid)): Path<(String, u64)>,
    Query(q): Query<KeyQuery>,
) -> Result<Response, Error> {
    let user = api::current_user(&state, &headers)?;
    api::board_access(
        &state,
        &id,
        user.as_ref().map(|u| u.login.as_str()),
        q.key.as_deref(),
    )?;
    let store = state.store().clone();
    let board = id.clone();
    let found = tokio::task::spawn_blocking(move || get(&store, &board, mid))
        .await
        .map_err(|_| Error::new(StatusCode::INTERNAL_SERVER_ERROR, "読み込みに失敗しました"))??;
    let Some((info, data)) = found else {
        return Err(Error::new(StatusCode::NOT_FOUND, "動画が見つかりません"));
    };
    let len = data.len();
    let mime = HeaderValue::from_str(&info.mime).unwrap_or(HeaderValue::from_static("video/webm"));
    let common = [
        (CONTENT_TYPE, mime),
        (ACCEPT_RANGES, HeaderValue::from_static("bytes")),
        // 中身は変わらない(新しく録れば別の ID)。
        (
            CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        ),
    ];
    let range = headers.get(RANGE).and_then(|v| v.to_str().ok());
    match range.map(|r| parse_range(r, len)) {
        None => Ok((common, data).into_response()),
        Some(Some((start, end))) => {
            let part = data[start..=end].to_vec();
            let mut res = (StatusCode::PARTIAL_CONTENT, common, Body::from(part)).into_response();
            let h = res.headers_mut();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")) {
                h.insert(CONTENT_RANGE, v);
            }
            h.insert(CONTENT_LENGTH, HeaderValue::from(end - start + 1));
            Ok(res)
        }
        Some(None) => {
            let mut res = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                res.headers_mut().insert(CONTENT_RANGE, v);
            }
            Ok(res)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_types() {
        assert_eq!(
            accepted_mime("video/webm;codecs=vp9,opus"),
            Some("video/webm")
        );
        assert_eq!(accepted_mime("VIDEO/MP4"), Some("video/mp4"));
        assert_eq!(accepted_mime("image/png"), None);
        assert_eq!(accepted_mime("text/html"), None);
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=90-200", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=5-1", 100), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }

    #[test]
    fn store_roundtrip_and_delete() {
        let store = Store::memory();
        let id = put(&store, "b", b"webm", "video/webm", Some("alice")).unwrap();
        let (info, data) = get(&store, "b", id).unwrap().unwrap();
        assert_eq!(
            (info.mime.as_str(), info.bytes, data.as_slice()),
            ("video/webm", 4, &b"webm"[..])
        );
        assert!(get(&store, "other", id).unwrap().is_none());
        delete_all(&store, "b").unwrap();
        assert!(get(&store, "b", id).unwrap().is_none());
    }
}
