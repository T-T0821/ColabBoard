//! バイナリに埋め込んだ Web クライアントの配信(`embed` 機能)。
//!
//! `trunk build --release` が作る `.br` / `.gz`(`tools/precompress`)も埋め込まれるので、
//! ブラウザが受け取れるならそれをそのまま返す(wasm は brotli で約 1/4 になる)。

use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../board-web/dist"]
#[exclude = ".*"]
struct Assets;

pub fn available() -> bool {
    Assets::get("index.html").is_some()
}

/// 埋め込んだファイルの中身(OGP 画像のフォントなど)。
pub fn file(path: &str) -> Option<Vec<u8>> {
    Assets::get(path).map(|f| f.data.into_owned())
}

pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let Some(file) = Assets::get(path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let cache = if is_hashed(path) {
        // trunk がハッシュを付けたファイルは中身が変わると名前も変わるので、長くキャッシュしてよい。
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let accepts = |enc: &str| {
        headers
            .get_all(header::ACCEPT_ENCODING)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|v| {
                let mut parts = v.split(';');
                parts.next().is_some_and(|name| name.trim() == enc)
                    && parts.all(|q| q.trim() != "q=0")
            })
    };
    let compressed = [("br", ".br"), ("gzip", ".gz")]
        .into_iter()
        .filter(|(enc, _)| accepts(enc))
        .find_map(|(enc, ext)| Some((enc, Assets::get(&format!("{path}{ext}"))?)));
    let mut res = match compressed {
        Some((enc, c)) => {
            let mut res = c.data.into_response();
            res.headers_mut()
                .insert(header::CONTENT_ENCODING, HeaderValue::from_static(enc));
            res
        }
        None => file.data.into_response(),
    };
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(file.metadata.mimetype()) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    res
}

/// trunk が付けたハッシュ(`name-0123456789abcdef.js`、`…_bg.wasm`)を含むか。
fn is_hashed(path: &str) -> bool {
    let Some((stem, ext)) = path.rsplit_once('.') else {
        return false;
    };
    if !matches!(ext, "js" | "wasm" | "css") {
        return false;
    }
    let stem = stem.strip_suffix("_bg").unwrap_or(stem);
    stem.rsplit_once('-')
        .is_some_and(|(_, hash)| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::is_hashed;

    #[test]
    fn hashed_names() {
        assert!(is_hashed("board-web-2f9e328adb9aed82.js"));
        assert!(is_hashed("board-web-a198497814fb2da9_bg.wasm"));
        assert!(is_hashed("style-b1fefdf75a8409e4.css"));
        // ファイル処理の Worker は名前が固定なので、長くキャッシュしてはいけない。
        assert!(!is_hashed("board-files.js"));
        assert!(!is_hashed("board-files_bg.wasm"));
        assert!(!is_hashed("board-files_loader.js"));
        assert!(!is_hashed("sw.js"));
        assert!(!is_hashed("index.html"));
    }
}
