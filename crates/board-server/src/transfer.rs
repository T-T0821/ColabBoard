//! サーバー側の書き出しと取り込み(`/api/v1/boards/{id}/export`・`/import`)。
//!
//! 描画は [`board_export`](GPU なしで CPU で塗る)、ファイルの読み書きは
//! [`board_core::convert`] で、CLI(`graboard`)と同じエンジンを使う。
//!
//! - `GET /export?format=png|pdf|graboard|json|md`(閲覧者以上)。画像の設定は
//!   `scale`(0.25〜4)・`padding`・`background`(`#rrggbb` か `transparent`)・
//!   `frame`(名前か番号)・`pages=frames`(PDF を 1 フレーム 1 ページに)。
//!   個人作業エリアの中身は伏せて描く。
//! - `POST /import?format=…`(編集者以上)。本文がファイルの中身。省くと中身から形式を推測する。
//!   既存の要素の右に並べ、作った要素の ID を返す。

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use board_core::api::Role;
use board_core::convert::{self, InputFormat, TextFormat};
use board_export::{Format, Options, Target};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::Error;
use crate::rest::{access, actor, blocking, need};

/// 取り込むファイルの大きさの上限。
pub const MAX_IMPORT_BYTES: usize = 20 * 1024 * 1024;
/// サーバーで描く画像の画素数の上限(1 ページ)。
const MAX_PIXELS: u64 = 24_000_000;
/// PDF のページ数の上限。
const MAX_PAGES: usize = 60;

#[derive(Deserialize, Default)]
pub struct ExportQuery {
    pub key: Option<String>,
    pub format: Option<String>,
    pub scale: Option<f32>,
    pub padding: Option<f32>,
    pub background: Option<String>,
    pub frame: Option<String>,
    pub pages: Option<String>,
}

enum Kind {
    Image(Format),
    Text(TextFormat),
}

impl ExportQuery {
    fn kind(&self) -> Result<Kind, Error> {
        let f = self.format.as_deref().unwrap_or("png");
        if let Some(i) = Format::from_name(f) {
            return Ok(Kind::Image(i));
        }
        TextFormat::from_name(f).map(Kind::Text).ok_or_else(|| {
            Error::bad_request("format は png / pdf / graboard / json / md のどれかにしてください")
        })
    }

    fn target(&self) -> Target {
        if self.pages.as_deref() == Some("frames") {
            Target::Frames
        } else if let Some(f) = self.frame.as_deref().filter(|f| !f.trim().is_empty()) {
            Target::Frame(f.to_string())
        } else {
            Target::Board
        }
    }

    fn options(&self) -> Result<Options, Error> {
        let mut o = Options {
            max_pixels: MAX_PIXELS,
            hide_private: true,
            ..Options::default()
        };
        if let Some(s) = self.scale {
            if !(0.25..=4.0).contains(&s) {
                return Err(Error::bad_request("scale は 0.25〜4 にしてください"));
            }
            o.scale = s;
        }
        if let Some(p) = self.padding {
            o.padding = p.clamp(0.0, 400.0);
        }
        match self.background.as_deref() {
            None | Some("") => {}
            Some("transparent" | "none") => o.background = None,
            Some(c) => {
                o.background = Some(
                    board_core::ops::parse_color(c)
                        .ok_or_else(|| Error::bad_request("background の色が読めません"))?,
                );
            }
        }
        Ok(o)
    }
}

/// ファイル名に使えるようにした題名。
fn file_name(title: &str, ext: &str) -> String {
    let base: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(80)
        .collect();
    let base = base.trim();
    format!("{}.{ext}", if base.is_empty() { "graboard" } else { base })
}

/// `Content-Disposition`(日本語の名前は RFC 5987 で)。
fn disposition(name: &str) -> HeaderValue {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = percent_encode(name);
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub async fn export(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Result<Response, Error> {
    let actor = actor(&state, &headers)?;
    let (meta, role) = access(&state, &actor, &id, q.key.as_deref())?;
    need(role, Role::Viewer)?;
    let kind = q.kind()?;
    let target = q.target();
    let opts = q.options()?;
    let title = meta.map(|m| m.title).unwrap_or_default();
    let hub = state.hub.clone();
    let st = state.clone();
    let name_title = title.clone();
    let (bytes, mime, ext) = blocking(move || {
        let font = st.font.clone();
        hub.read(
            &id,
            |board| -> Result<(Vec<u8>, &'static str, &'static str), Error> {
                match kind {
                    Kind::Image(f) => {
                        if target == Target::Frames
                            && board_core::slides::slides(board).len() > MAX_PAGES
                        {
                            return Err(Error::bad_request(format!(
                                "PDF にできるのは {MAX_PAGES} ページまでです"
                            )));
                        }
                        let bytes =
                            board_export::export(board, f, &target, &opts, font.as_ref(), &title)
                                .map_err(|e| match e {
                                board_export::Error::Encode(_) => {
                                    Error::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                                }
                                board_export::Error::FrameNotFound(_) => {
                                    Error::new(StatusCode::NOT_FOUND, e.to_string())
                                }
                                _ => Error::bad_request(e.to_string()),
                            })?;
                        Ok((bytes, f.mime(), f.extension()))
                    }
                    Kind::Text(f) => {
                        // 文字の書き出しも、個人作業エリアの中身は含めない。
                        let hidden = board_core::privacy::hidden_ids(board, "");
                        let bytes = if hidden.is_empty() {
                            convert::save(board, f)
                        } else {
                            let mut copy = board_core::Board::new();
                            copy.push_many(board_export::visible_elements(board, true));
                            convert::save(&copy, f)
                        }
                        .map_err(Error::from)?;
                        Ok((bytes, f.mime(), f.extension()))
                    }
                }
            },
        )?
    })
    .await?;
    let mut res = (StatusCode::OK, bytes).into_response();
    let h = res.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    h.insert(
        CONTENT_DISPOSITION,
        disposition(&file_name(&name_title, ext)),
    );
    Ok(res)
}

#[derive(Deserialize)]
pub struct ImportQuery {
    pub key: Option<String>,
    pub format: Option<String>,
}

#[derive(Serialize)]
pub struct Imported {
    /// 作った要素の ID。
    pub created: Vec<String>,
}

pub async fn import(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ImportQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<Imported>), Error> {
    let actor = actor(&state, &headers)?;
    let (_, role) = access(&state, &actor, &id, q.key.as_deref())?;
    need(role, Role::Editor)?;
    if body.is_empty() {
        return Err(Error::bad_request("ファイルが空です"));
    }
    let format = match q.format.as_deref() {
        Some(f) => InputFormat::from_name(f)
            .ok_or_else(|| Error::bad_request(format!("読めない形式です: {f}")))?,
        None => InputFormat::sniff(&body).unwrap_or(InputFormat::Graboard),
    };
    let hub = state.hub.clone();
    let board_id = id.clone();
    let ids = blocking(move || {
        hub.edit(&board_id, |b| {
            convert::import(b, format, &body).map_err(Error::bad_request)
        })
    })
    .await?;
    crate::audit::record(
        &state,
        &headers,
        crate::audit::Entry::new("board.import")
            .actor(actor.login().unwrap_or(""))
            .target(&id)
            .detail(format!("{} {}", format.name(), ids.len())),
    );
    Ok((
        StatusCode::CREATED,
        Json(Imported {
            created: ids.iter().map(|i| board_core::ops::id_string(*i)).collect(),
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parsing() {
        let q = ExportQuery {
            format: Some("pdf".into()),
            pages: Some("frames".into()),
            background: Some("transparent".into()),
            ..Default::default()
        };
        assert!(matches!(q.kind(), Ok(Kind::Image(Format::Pdf))));
        assert_eq!(q.target(), Target::Frames);
        let Ok(o) = q.options() else { panic!() };
        assert_eq!(o.background, None);
        assert!(o.hide_private);
        let bad = ExportQuery {
            scale: Some(100.0),
            ..Default::default()
        };
        assert!(bad.options().is_err());
        let bad = ExportQuery {
            format: Some("gif".into()),
            ..Default::default()
        };
        assert!(bad.kind().is_err());
    }

    #[test]
    fn file_names() {
        assert_eq!(file_name("企画/会議", "png"), "企画_会議.png");
        assert_eq!(file_name("  ", "pdf"), "graboard.pdf");
        let d = disposition("企画.png");
        let s = d.to_str().unwrap();
        assert!(s.contains("filename*=UTF-8''%E4%BC%81%E7%94%BB.png"));
    }
}
