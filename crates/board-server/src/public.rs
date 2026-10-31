//! Web に公開したボードのページ。
//!
//! - `GET /p/{board}/{slug}`: 公開ページ。SNS やチャットに貼ったときのために OGP の
//!   `<meta>` を入れた HTML を返し、ブラウザではすぐ閲覧専用のボード(`/?room=…&key=…&public`)へ移る。
//! - `GET /p/{board}/{slug}/og.png`: OGP 画像([`crate::og`])。
//!
//! 公開をやめる・URL を作り直すと、古い URL は 404 になる。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use board_core::{Board, Element, ShapeKind};

use crate::api::Error;
use crate::boards::{self, BoardMeta};
use crate::{AppState, og, valid_room_name};

/// OGP 画像を描き直すまでの最短間隔。
const OG_TTL: Duration = Duration::from_secs(60);
/// 説明文の最大の長さ(文字)。
const DESCRIPTION_CHARS: usize = 120;

/// 描いた OGP 画像(ボードごと)。
#[derive(Default)]
pub struct OgCache {
    map: Mutex<HashMap<String, Cached>>,
}

/// 描いた時刻と PNG。
type Cached = (Instant, Arc<Vec<u8>>);

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Html(page_shell(
            "見つかりません",
            "<h1>このページは公開されていません</h1><p>公開が終わったか、URL が変わった可能性があります。</p>",
            "",
        )),
    )
        .into_response()
}

fn load(state: &AppState, id: &str, slug: &str) -> Result<Option<(BoardMeta, Board)>, Error> {
    if !valid_room_name(id) || !valid_room_name(slug) {
        return Ok(None);
    }
    let Some(meta) = boards::published(state.store(), id, slug)? else {
        return Ok(None);
    };
    let mut board = Board::new();
    if let Some(bytes) = state.hub.current_state(id)?
        && let Err(e) = board.apply_remote(&bytes)
    {
        tracing::warn!(board = id, "公開ページ用にボードを読めません: {e}");
    }
    Ok(Some((meta, board)))
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// 説明文: フレーム名と付箋の文字を並べる。何もなければ要素数。
pub fn describe(board: &Board) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut frames = Vec::new();
    let mut others = Vec::new();
    for e in board.elements() {
        if let Element::Shape(s) = e {
            let t: String = s.text.split_whitespace().collect::<Vec<_>>().join(" ");
            if t.is_empty() {
                continue;
            }
            match s.kind {
                ShapeKind::Frame => frames.push(t),
                ShapeKind::Sticky | ShapeKind::Text => others.push(t),
                _ => {}
            }
        }
    }
    parts.extend(frames);
    parts.extend(others);
    let joined = parts.join("・");
    if joined.is_empty() {
        return format!("グラボードで公開中のホワイトボード({} 要素)", board.len());
    }
    let mut out: String = joined.chars().take(DESCRIPTION_CHARS).collect();
    if joined.chars().count() > DESCRIPTION_CHARS {
        out.push('…');
    }
    out
}

/// このリクエストの元の URL(`https://host`)。OGP の URL は絶対 URL が必要。
fn origin(state: &AppState, headers: &HeaderMap) -> String {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(HOST))
        .and_then(|h| h.to_str().ok())
        .filter(|h| {
            h.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
        })
        .unwrap_or("localhost");
    let https = state.config.secure_cookies
        || headers
            .get("x-forwarded-proto")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|p| p.eq_ignore_ascii_case("https"));
    format!("{}://{host}", if https { "https" } else { "http" })
}

fn page_shell(title: &str, body: &str, head: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="ja">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
{head}<style>
:root {{ --bg:#f6f7f9; --panel:#fff; --text:#1b1d24; --muted:#6b7080; --border:rgba(20,24,40,.09); --accent:#4c5cf5; --r:16px; color-scheme: light; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#1c1d22; --panel:#24262e; --text:#e9eaf0; --muted:#9a9fb0; --border:rgba(255,255,255,.08); --accent:#7380ff; color-scheme: dark; }} }}
body {{ margin:0; min-height:100vh; display:grid; place-items:center; background:var(--bg); color:var(--text);
  font-family:"Inter",system-ui,-apple-system,"Segoe UI","Hiragino Sans","Noto Sans JP",sans-serif; }}
main {{ width:min(640px, calc(100vw - 32px)); background:var(--panel); border:1px solid var(--border); border-radius:var(--r);
  box-shadow:0 8px 24px rgba(16,20,40,.08); overflow:hidden; }}
main img {{ display:block; width:100%; height:auto; border-bottom:1px solid var(--border); }}
.body {{ padding:20px 24px 24px; }}
h1 {{ font-size:20px; margin:0 0 6px; }}
p {{ color:var(--muted); margin:0 0 16px; line-height:1.6; }}
a.button {{ display:inline-block; background:var(--accent); color:#fff; text-decoration:none; font-weight:600; padding:10px 18px; border-radius:999px; }}
</style>
</head>
<body>
<main>{body}</main>
</body>
</html>
"#
    )
}

/// 公開ページの HTML。
pub fn render_page(meta: &BoardMeta, board: &Board, origin: &str, slug: &str) -> String {
    let title = escape(&meta.title);
    let desc = escape(&describe(board));
    let page = format!("{origin}/p/{}/{slug}", meta.id);
    let image = format!("{page}/og.png?v={}", meta.updated);
    let app = format!("/?room={}&key={slug}&public", meta.id);
    let head = format!(
        r#"<meta name="description" content="{desc}">
<link rel="canonical" href="{page}">
<meta property="og:type" content="website">
<meta property="og:site_name" content="グラボード">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{desc}">
<meta property="og:url" content="{page}">
<meta property="og:image" content="{image}">
<meta property="og:image:width" content="{w}">
<meta property="og:image:height" content="{h}">
<meta property="og:image:alt" content="{title} のプレビュー">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="{title}">
<meta name="twitter:description" content="{desc}">
<meta name="twitter:image" content="{image}">
<script>location.replace({app_js});</script>
"#,
        w = og::WIDTH,
        h = og::HEIGHT,
        app_js = serde_json::to_string(&app).unwrap_or_default(),
    );
    let body = format!(
        r#"<img src="{image}" alt="{title} のプレビュー" width="{w}" height="{h}">
<div class="body"><h1>{title}</h1><p>{desc}</p><a class="button" href="{app}">ボードを見る</a></div>"#,
        app = escape(&app),
        w = og::WIDTH,
        h = og::HEIGHT,
    );
    page_shell(&format!("{title} | グラボード"), &body, &head)
}

pub async fn page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, slug)): Path<(String, String)>,
) -> Response {
    let st = state.clone();
    let origin = origin(&state, &headers);
    let loaded = tokio::task::spawn_blocking(move || {
        load(&st, &id, &slug)
            .map(|o| o.map(|(meta, board)| render_page(&meta, &board, &origin, &slug)))
    })
    .await;
    match loaded {
        Ok(Ok(Some(html))) => {
            let mut res = Html(html).into_response();
            res.headers_mut().insert(
                CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=60"),
            );
            res
        }
        Ok(Ok(None)) => not_found(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn image(
    State(state): State<Arc<AppState>>,
    Path((id, slug)): Path<(String, String)>,
) -> Response {
    let st = state.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Option<Arc<Vec<u8>>>, Error> {
        // 公開されているかは毎回確かめる(やめたらすぐ見えなくする)。
        if !valid_room_name(&id) || !valid_room_name(&slug) {
            return Ok(None);
        }
        if boards::published(st.store(), &id, &slug)?.is_none() {
            return Ok(None);
        }
        if let Some((at, png)) = st.og.map.lock().ok().and_then(|m| m.get(&id).cloned())
            && at.elapsed() < OG_TTL
        {
            return Ok(Some(png));
        }
        let Some((meta, board)) = load(&st, &id, &slug)? else {
            return Ok(None);
        };
        let png = Arc::new(og::render(&board, &meta.title, st.font.as_ref()));
        if let Ok(mut m) = st.og.map.lock() {
            // 古いものは捨てる(公開をやめたボードが残り続けないように)。
            m.retain(|_, (at, _)| at.elapsed() < OG_TTL * 10);
            m.insert(id, (Instant::now(), png.clone()));
        }
        Ok(Some(png))
    })
    .await;
    match result {
        Ok(Ok(Some(png))) => (
            [
                (CONTENT_TYPE, HeaderValue::from_static("image/png")),
                (
                    CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=300"),
                ),
            ],
            png.as_ref().clone(),
        )
            .into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(
            escape(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }

    #[test]
    fn description() {
        assert!(describe(&Board::new()).contains("0 要素"));
        let d = describe(&Board::welcome());
        assert!(!d.is_empty());
        assert!(d.chars().count() <= DESCRIPTION_CHARS + 1);
    }
}
