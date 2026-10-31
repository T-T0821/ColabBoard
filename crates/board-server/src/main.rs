//! `board-server` の起動。
//!
//! 環境変数:
//! - `PORT`(既定 8080)
//! - `DATA_DIR`(既定 `data`): 保存先。`DATA_DIR=` (空)で保存しない。
//! - `STATIC_DIR`(既定 `crates/board-web/dist`): 配信する Web クライアント。
//!   `embed` 機能付きでビルドしたバイナリは、指定がなく既定の場所にもなければ埋め込み版を配る。
//! - `MAX_MESSAGE_MB`(既定 128): WebSocket で受け取る 1 通の上限。とても大きなボードを使うなら増やす。
//! - `ANONYMOUS_BOARDS`(既定 `1`): `0` でログインなしのボード作成を禁止する。
//! - `COOKIE_SECURE`(既定 `0`): `1` でログイン Cookie に `Secure` を付ける(HTTPS 用)。
//! - `ANTHROPIC_API_KEY`: 設定すると AI 機能(付箋の要約・グループ分け)が有効になる。
//! - `ANTHROPIC_MODEL`(既定 `claude-opus-5-5`): AI 機能で使うモデル。
//! - `PUBLIC_URL`: 公開している URL(例 `https://board.example.com/`)。Slack 通知のリンクに使う。
//! - `SLACK_WEBHOOK_PREFIXES`(既定 `https://hooks.slack.com/`): Slack 通知を送ってよい URL の先頭(カンマ区切り)。
//! - `RUST_LOG`(既定 `info`)
//!
//! `board-server healthcheck` は自分の `/healthz` を確かめて終了コードで返す(コンテナの死活監視用)。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use board_server::{Config, Hub, Store, has_embedded_client, router, spawn_flusher};
use tracing_subscriber::EnvFilter;

fn port() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080)
}

/// `GET /healthz` が 200 を返すか。
fn healthcheck() -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port()));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(3)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    if s.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.starts_with("HTTP/1.1 200") || buf.starts_with("HTTP/1.0 200")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(if healthcheck() { 0 } else { 1 });
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let port = port();
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "data".into());
    let static_env = std::env::var("STATIC_DIR").ok();
    let static_dir = static_env
        .clone()
        .unwrap_or_else(|| "crates/board-web/dist".into());
    let max_message_bytes = std::env::var("MAX_MESSAGE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(board_server::DEFAULT_MAX_MESSAGE_BYTES, |mb| {
            mb.max(1) * 1024 * 1024
        });

    let store = if data_dir.is_empty() {
        tracing::warn!("DATA_DIR が空なので保存しません(アカウントも再起動で消えます)");
        Store::memory()
    } else {
        let path = PathBuf::from(&data_dir).join("boards.redb");
        tracing::info!("保存先: {}", path.display());
        Store::open(&path)?
    };
    let flag = |name: &str, default: bool| match std::env::var(name).as_deref() {
        Ok("0") | Ok("false") => false,
        Ok("1") | Ok("true") => true,
        _ => default,
    };
    let static_dir = PathBuf::from(static_dir);
    let static_dir = if static_dir.is_dir() {
        tracing::info!("静的ファイル: {}", static_dir.display());
        Some(static_dir)
    } else if static_env.is_none() && has_embedded_client() {
        tracing::info!("静的ファイル: バイナリに埋め込んだ Web クライアント");
        None
    } else {
        tracing::warn!(
            "{} がないので静的ファイルは配信しません(trunk build で作れます)",
            static_dir.display()
        );
        None
    };

    let hub = Arc::new(Hub::new(Arc::new(store)));
    let flusher = spawn_flusher(hub.clone());
    let config = Config {
        static_dir,
        max_message_bytes,
        anonymous_boards: flag("ANONYMOUS_BOARDS", true),
        secure_cookies: flag("COOKIE_SECURE", false),
        ai: board_server::ai::AiConfig::from_env().map(|c| {
            tracing::info!("AI 機能: 有効(モデル {})", c.model);
            Arc::new(board_server::ai::Ai::new(c))
        }),
        slack: board_server::slack::SlackConfig::from_env(),
    };
    if config.ai.is_none() {
        tracing::info!(
            "AI 機能: 無効(ANTHROPIC_API_KEY を設定すると付箋の要約・グループ分けと図の生成が使えます)"
        );
    }
    if !config.anonymous_boards {
        tracing::info!("ログインなしのボードは無効です");
    }
    let app = router(hub.clone(), config);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("http://localhost:{port} で待ち受けています");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    flusher.abort();
    // 終了前に未保存の変更を書き出す。
    tokio::task::spawn_blocking(move || hub.flush()).await?;
    tracing::info!("停止しました");
    Ok(())
}

/// Ctrl+C か SIGTERM(コンテナの停止)を待つ。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
    tracing::info!("停止します");
}
