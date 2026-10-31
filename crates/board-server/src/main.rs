//! `board-server` の起動。
//!
//! 環境変数:
//! - `PORT`(既定 8080)
//! - `DATA_DIR`(既定 `data`): 保存先。`DATA_DIR=` (空)で保存しない。
//! - `STATIC_DIR`(既定 `crates/board-web/dist`): 配信する Web クライアント。
//! - `RUST_LOG`(既定 `info`)

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use board_server::{Config, Hub, Store, router, spawn_flusher};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "data".into());
    let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "crates/board-web/dist".into());

    let store = if data_dir.is_empty() {
        tracing::warn!("DATA_DIR が空なので保存しません");
        None
    } else {
        let path = PathBuf::from(&data_dir).join("boards.redb");
        tracing::info!("保存先: {}", path.display());
        Some(Store::open(&path)?)
    };
    let static_dir = PathBuf::from(static_dir);
    let static_dir = if static_dir.is_dir() {
        tracing::info!("静的ファイル: {}", static_dir.display());
        Some(static_dir)
    } else {
        tracing::warn!(
            "{} がないので静的ファイルは配信しません(trunk build で作れます)",
            static_dir.display()
        );
        None
    };

    let hub = Arc::new(Hub::new(store));
    let flusher = spawn_flusher(hub.clone());
    let app = router(hub.clone(), Config { static_dir });

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
