//! アプリの中で動かす同期サーバー(内蔵サーバー)。
//!
//! `board-server` をそのまま使い、`127.0.0.1` だけで待ち受ける(ほかの PC からはつながらない)。
//! ボードとアカウントはアプリのデータフォルダの `boards.redb` に保存する。Web クライアントは
//! バイナリに埋め込んであるので、ネットワークがなくても全部の画面が開ける。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use board_server::{Config, Hub, Store, router, spawn_flusher};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub struct LocalServer {
    hub: Arc<Hub>,
    addr: SocketAddr,
    db_path: PathBuf,
    serve: Mutex<Option<JoinHandle<()>>>,
    background: Vec<JoinHandle<()>>,
}

/// 内蔵サーバーの設定。環境変数(Slack・課題カードなど)はサーバー版と同じく読む。
fn config(ai_key: Option<&str>) -> Config {
    let ai = match ai_key {
        Some(key) => Some(board_server::ai::AiConfig {
            api_key: key.to_string(),
            model: std::env::var("ANTHROPIC_MODEL")
                .ok()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| board_server::ai::DEFAULT_MODEL.to_string()),
            base_url: std::env::var("ANTHROPIC_BASE_URL")
                .ok()
                .filter(|u| !u.trim().is_empty())
                .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
        }),
        None => board_server::ai::AiConfig::from_env(),
    };
    Config {
        static_dir: None,
        max_message_bytes: board_server::DEFAULT_MAX_MESSAGE_BYTES,
        anonymous_boards: true,
        secure_cookies: false,
        ice_servers: board_server::default_ice_servers(),
        ai: ai.map(|c| Arc::new(board_server::ai::Ai::new(c))),
        slack: board_server::slack::SlackConfig::from_env(),
        plugins: board_server::plugins::PluginConfig::from_env(),
        issues: board_server::issues::IssuesConfig::from_env(),
        embed_origins: "'self'".into(),
        admins: Vec::new(),
        trust_proxy: false,
        oidc: None,
        password_login: true,
        limits: board_server::guard::Limits::from_env(false),
        ..Config::default()
    }
}

impl LocalServer {
    /// 保存先を開いて待ち受けを始める。`preferred_port` が使われていれば空いているポートにする。
    pub async fn start(
        data_dir: &Path,
        preferred_port: u16,
        ai_key: Option<&str>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("boards.redb");
        let store = Store::open(&db_path)?;
        let hub = Arc::new(Hub::new(Arc::new(store)));
        let listener = match TcpListener::bind(("127.0.0.1", preferred_port)).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("ポート {preferred_port} が使えないので別のポートにします: {e}");
                TcpListener::bind(("127.0.0.1", 0)).await?
            }
        };
        let addr = listener.local_addr()?;
        let background = vec![
            spawn_flusher(hub.clone()),
            board_server::audit::spawn_pruner(hub.store().clone(), 365),
        ];
        let server = Self {
            hub,
            addr,
            db_path,
            serve: Mutex::new(None),
            background,
        };
        server.serve_on(listener, ai_key);
        tracing::info!(
            "内蔵サーバー: http://{addr}/ (保存先 {})",
            server.db_path.display()
        );
        Ok(server)
    }

    fn serve_on(&self, listener: TcpListener, ai_key: Option<&str>) {
        let app = router(self.hub.clone(), config(ai_key));
        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                tracing::error!("内蔵サーバーが止まりました: {e}");
            }
        });
        if let Some(old) = self.serve.lock().unwrap().replace(task) {
            old.abort();
        }
    }

    /// 設定(AI のキー)を変えて同じポートで待ち受け直す。ボードの中身(Hub)はそのまま。
    /// 開いているページは WebSocket が一度切れて、自動でつなぎ直す。
    pub async fn restart(&self, ai_key: Option<&str>) -> std::io::Result<()> {
        let old = self.serve.lock().unwrap().take();
        if let Some(old) = old {
            old.abort();
            let _ = old.await;
        }
        let listener = TcpListener::bind(self.addr).await?;
        self.serve_on(listener, ai_key);
        Ok(())
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// ページの URL。`localhost` ではなく `127.0.0.1` にするのは、名前解決で IPv6 に
    /// 行ってつながらない環境を避けるため(どちらも「安全なオリジン」なのでマイクも使える)。
    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    pub fn data_dir(&self) -> Option<&Path> {
        self.db_path.parent()
    }

    /// 未保存の変更をディスクに書き出す(終了前に呼ぶ)。
    pub fn shutdown(&self) {
        if let Some(t) = self.serve.lock().unwrap().take() {
            t.abort();
        }
        for t in &self.background {
            t.abort();
        }
        self.hub.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内蔵サーバーが埋め込みの Web クライアントを返し、設定を変えて待ち受け直しても同じポートで答える。
    #[tokio::test]
    async fn serves_client_and_restarts() {
        let dir = std::env::temp_dir().join(format!("graboard-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let server = LocalServer::start(&dir, 0, None).await.unwrap();
        let get = |path: &str| {
            let addr = server.addr;
            let path = path.to_string();
            async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
                s.write_all(format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut buf = Vec::new();
                s.read_to_end(&mut buf).await.unwrap();
                String::from_utf8_lossy(&buf).into_owned()
            }
        };
        let health = get("/healthz").await;
        assert!(
            health.lines().next().is_some_and(|l| l.contains(" 200 ")),
            "{health}"
        );
        if board_server::has_embedded_client() {
            let index = get("/").await;
            assert!(
                index.lines().next().is_some_and(|l| l.contains(" 200 ")),
                "{}",
                &index[..index.len().min(200)]
            );
        }
        // AI のキーを変えて待ち受け直しても同じポートで答える。
        server.restart(Some("sk-test")).await.unwrap();
        let health = get("/healthz").await;
        assert!(
            health.lines().next().is_some_and(|l| l.contains(" 200 ")),
            "{health}"
        );
        server.shutdown();
        drop(server);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
