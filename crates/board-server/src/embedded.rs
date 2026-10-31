//! バイナリに埋め込んだ Web クライアントの配信(`embed` 機能)。

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../board-web/dist"]
#[exclude = ".*"]
struct Assets;

pub fn available() -> bool {
    Assets::get("index.html").is_some()
}

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let Some(file) = Assets::get(path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    // trunk がハッシュを付けたファイルは中身が変わると名前も変わるので、長くキャッシュしてよい。
    let hashed = path.ends_with(".wasm") || path.ends_with(".js") || path.ends_with(".css");
    let cache = if path == "index.html" || path == "sw.js" || !hashed {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    (
        [
            (header::CONTENT_TYPE, file.metadata.mimetype().to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        file.data,
    )
        .into_response()
}
