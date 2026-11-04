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
//! - `TRUST_PROXY`(既定 `0`): `1` でリバースプロキシの `X-Forwarded-For` から利用者の IP を取る。
//! - `ALLOWED_ORIGINS`: ログイン付きの WebSocket を受け付けるほかのオリジン(カンマ区切り)。
//! - 回数制限・上限(`LOGIN_FAILURES_PER_ACCOUNT` `MAX_USERS` `MAX_BOARDS_PER_USER` など)と
//!   `CSP` `HSTS`: [`board_server::guard::Limits::from_env`] と docs/self-hosting.md を参照。
//! - `ANTHROPIC_API_KEY`: 設定すると AI 機能(付箋の要約・グループ分け)が有効になる。
//! - `ANTHROPIC_MODEL`(既定 `claude-opus-5-5`): AI 機能で使うモデル。
//! - `PUBLIC_URL`: 公開している URL(例 `https://board.example.com/`)。Slack 通知のリンクに使う。
//! - `SLACK_WEBHOOK_PREFIXES`(既定 `https://hooks.slack.com/`): Slack 通知を送ってよい URL の先頭(カンマ区切り)。
//! - `PLUGIN_DIR`: ここにある `*.wasm` をサーバーのプラグインとして読み込む(全員が使える)。
//! - `USER_PLUGINS`(既定 `1`): `0` で各自がプラグインを入れるのを禁止する。
//! - `GITHUB_TOKEN` / `GITHUB_API_URL`: 課題カードで GitHub を引くとき(トークンなしでも公開リポジトリは読める)。
//! - `JIRA_BASE_URL` / `JIRA_EMAIL` / `JIRA_API_TOKEN`: 課題カードで Jira を引くとき。
//! - `ICE_SERVERS`: 音声通話で使う STUN / TURN サーバー(`RTCIceServer` の JSON 配列)。
//!   既定は Google の公開 STUN。社内ネットワーク越しにつなぐなら TURN を足す。
//! - `ADMIN_LOGINS`: サーバー管理者のログイン名(`,` 区切り)。全体の操作ログを見られる。
//! - `TRUST_PROXY`(既定 `0`): `1` でリバースプロキシの `X-Forwarded-For` を接続元として記録する。
//! - `AUDIT_RETENTION_DAYS`(既定 `365`): 操作ログを残す日数。
//! - `EMBED_ALLOWED_ORIGINS`(既定 `*`): ボードを iframe で埋め込んでよいサイト(空白区切り。例 `https://wiki.example.com`)。
//! - `OIDC_ISSUER`・`OIDC_CLIENT_ID`・`OIDC_CLIENT_SECRET`・`OIDC_REDIRECT_URL`: そろえると
//!   OpenID Connect の SSO が有効になる。`OIDC_NAME`(ボタンの名前)・`OIDC_SCOPES`・
//!   `OIDC_ALLOWED_DOMAINS`(ログインできるメールのドメイン、`,` 区切り)も設定できる。
//! - `STORAGE_LOCATIONS`: チームが選べるデータの保存場所(S3 互換・ディレクトリ)。書き方は
//!   [`board_server::storage`] を参照。
//! - 料金プラン: `BILLING=mock`(テスト決済)か、`STRIPE_SECRET_KEY`・`STRIPE_WEBHOOK_SECRET`・
//!   `STRIPE_PRICE_PRO` で Stripe。上限は `FREE_BOARDS` などで変えられる。
//!   [`board_server::billing::BillingConfig::from_env`] と docs/billing.md を参照。
//! - `PASSWORD_LOGIN`(既定 `1`): `0` でパスワードのログインとアカウント作成を止める(SSO のみ)。
//! - `SMTP_URL`・`MAIL_FROM`: そろえるとメンション・期限などをメールでも知らせる
//!   (例 `SMTP_URL=smtps://user:pass@smtp.example.com`、`MAIL_FROM=グラボード <noreply@example.com>`)。
//! - `PUSH_NOTIFICATIONS`(既定 `1`): `0` でスマホ・PC へのプッシュ通知(Web Push)を止める。
//!   `VAPID_PRIVATE_KEY`・`VAPID_SUBJECT`・`PUSH_ALLOWED_HOSTS` も設定できる([`board_server::notify`])。
//! - `DUE_REMIND_HOURS`(既定 `24`): コメントの期限の何時間前に知らせるか。
//! - `NOTIFY_UTC_OFFSET`(既定 `+09:00`): 通知メールに書く時刻のタイムゾーン。
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

    let locations = board_server::storage::parse_locations(
        &std::env::var("STORAGE_LOCATIONS").unwrap_or_default(),
        |k| std::env::var(k).ok().filter(|v| !v.is_empty()),
    )?;
    for l in &locations {
        tracing::info!("保存場所: {}({}、{})", l.id, l.label, l.description);
    }
    store.set_locations(locations);
    let hub = Arc::new(Hub::new(Arc::new(store)));
    let flusher = spawn_flusher(hub.clone());
    let notifier = Arc::new(board_server::notify::Notifier::new(
        board_server::notify::NotifyConfig::from_env(),
        hub.store(),
    )?);
    tracing::info!(
        "通知: メール {}・プッシュ通知 {}",
        if notifier.email_available() {
            "有効"
        } else {
            "無効(SMTP_URL と MAIL_FROM で有効)"
        },
        if notifier.push_key().is_some() {
            "有効"
        } else {
            "無効"
        },
    );
    let due_checker = board_server::notify::spawn_due_checker(
        hub.store().clone(),
        notifier.clone(),
        flag("ANONYMOUS_BOARDS", true),
    );
    let config = Config {
        static_dir,
        max_message_bytes,
        anonymous_boards: flag("ANONYMOUS_BOARDS", true),
        secure_cookies: flag("COOKIE_SECURE", false),
        ice_servers: board_server::ice_servers_from_env(std::env::var("ICE_SERVERS").ok()),
        ai: board_server::ai::AiConfig::from_env().map(|c| {
            tracing::info!("AI 機能: 有効(モデル {})", c.model);
            Arc::new(board_server::ai::Ai::new(c))
        }),
        slack: board_server::slack::SlackConfig::from_env(),
        plugins: board_server::plugins::PluginConfig::from_env(),
        issues: board_server::issues::IssuesConfig::from_env(),
        embed_origins: std::env::var("EMBED_ALLOWED_ORIGINS")
            .ok()
            .map(|v| v.replace(',', " ").trim().to_string())
            .filter(|v| !v.is_empty() && !v.contains(';'))
            .unwrap_or_else(|| "*".into()),
        admins: std::env::var("ADMIN_LOGINS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
        trust_proxy: flag("TRUST_PROXY", false),
        oidc: board_server::oidc::OidcConfig::from_env().map(|c| {
            tracing::info!("SSO(OpenID Connect): 有効({})", c.issuer);
            Arc::new(board_server::oidc::Oidc::new(c))
        }),
        password_login: flag("PASSWORD_LOGIN", true),
        limits: board_server::guard::Limits::from_env(flag("COOKIE_SECURE", false)),
        notifier,
        billing: {
            let b = board_server::billing::BillingConfig::from_env();
            match b.mode.name() {
                "off" => {}
                "mock" => tracing::warn!(
                    "料金プラン: テスト決済(BILLING=mock)。本番では使わないでください"
                ),
                _ => tracing::info!("料金プラン: Stripe で支払い"),
            }
            b
        },
    };
    if !config.password_login && config.oidc.is_none() {
        tracing::warn!(
            "PASSWORD_LOGIN=0 ですが SSO が設定されていないので、誰もログインできません"
        );
    }
    let retention_days: u64 = std::env::var("AUDIT_RETENTION_DAYS")
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(365);
    let pruner = board_server::audit::spawn_pruner(hub.store().clone(), retention_days);
    if let Some(j) = &config.issues.jira {
        tracing::info!("Jira 連携: {}", j.base_url);
    }
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
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    flusher.abort();
    pruner.abort();
    due_checker.abort();
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
