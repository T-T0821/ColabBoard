//! グラボード のデスクトップアプリ(Tauri)。
//!
//! - **この PC で使う**(既定): アプリの中で同期サーバー(`board-server`)を動かし、
//!   `http://127.0.0.1:47180/` をウィンドウに開く。ボードはアプリのデータフォルダに保存され、
//!   ネットワークがなくてもすべての機能(AI など外部のサービスを使うものを除く)が使える。
//! - **サーバーに接続**: チームのサーバーの URL をウィンドウに開く。Web 版と同じく、ボードは
//!   端末(IndexedDB)にも保存され、アプリ本体は Service Worker が持つので、一度開いたサーバーは
//!   つながらないときも前回の内容から開いて編集でき、つながると自動でマージされる。
//!
//! 接続先はメニューの「接続先」から切り替える。設定は設定フォルダの `settings.json`。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod local;
mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use settings::{DEFAULT_LOCAL_PORT, Mode, Settings, normalize_server_url};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

use crate::local::LocalServer;

const MAIN: &str = "main";
const CONNECT: &str = "connect";

struct AppState {
    local: Arc<LocalServer>,
    settings: Mutex<Settings>,
    config_dir: PathBuf,
}

impl AppState {
    fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    fn update(&self, f: impl FnOnce(&mut Settings)) {
        let mut s = self.settings.lock().unwrap();
        f(&mut s);
        if let Err(e) = s.save(&self.config_dir) {
            tracing::warn!("設定を保存できませんでした: {e}");
        }
    }
}

/// サーバーが答えるか(`/healthz`)。
async fn reachable(base: &str) -> bool {
    let Ok(url) = url::Url::parse(base).and_then(|u| u.join("healthz")) else {
        return false;
    };
    let host = url.host_str().unwrap_or_default().to_string();
    let port = url.port_or_known_default().unwrap_or(80);
    // TLS や HTTP の中身までは見ず、つながるかだけを見る(証明書の確かめはページを開く WebView に任せる)。
    let connect = tokio::net::TcpStream::connect((host.as_str(), port));
    matches!(
        tokio::time::timeout(Duration::from_secs(3), connect).await,
        Ok(Ok(_))
    )
}

/// 設定どおりの接続先をメインのウィンドウに開く。
async fn open_target(app: &AppHandle) {
    let state = app.state::<AppState>();
    let s = state.settings();
    let local = state.local.url();
    let (url, title) = match (s.mode, s.server_url.clone()) {
        (Mode::Remote, Some(server)) => {
            let host = url::Url::parse(&server)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default();
            if reachable(&server).await {
                if !s.opened_servers.contains(&server) {
                    state.update(|s| s.opened_servers.push(server.clone()));
                }
                (s.start_page(&server), format!("グラボード — {host}"))
            } else if s.opened_servers.contains(&server) {
                // アプリ本体とボードは端末に残っているので、そのまま開いてオフラインで編集する。
                (
                    s.start_page(&server),
                    format!("グラボード — {host}(オフライン)"),
                )
            } else {
                tracing::warn!("{server} につながらないので、この PC のボードを開きます");
                (
                    s.start_page(&local),
                    format!("グラボード — この PC({host} につながりません)"),
                )
            }
        }
        _ => (s.start_page(&local), "グラボード — この PC".to_string()),
    };
    if let Some(win) = app.get_webview_window(MAIN) {
        let _ = win.set_title(&title);
        match url::Url::parse(&url) {
            Ok(u) => {
                if let Err(e) = win.navigate(u) {
                    tracing::error!("{url} を開けません: {e}");
                }
            }
            Err(e) => tracing::error!("{url} を開けません: {e}"),
        }
    }
}

/// ウィンドウの中で開いてよいページか。それ以外(ボードの外へのリンク)は既定のブラウザで開く。
///
/// この PC で使っているときは内蔵サーバーのページだけ。サーバーにつないでいるときは、
/// SSO のログイン画面(サーバーから別のサイトへ移って戻ってくる)のために http / https はすべて許す。
/// `window.open` や `target=_blank` のリンクは [`WebviewWindowBuilder::on_new_window`] で常にブラウザへ回す。
fn allow_in_window(app: &AppHandle, url: &url::Url) -> bool {
    match url.scheme() {
        "http" | "https" => {}
        "tauri" | "about" | "data" | "blob" => return true,
        _ => return false,
    }
    if url.host_str() == Some("tauri.localhost") {
        return true;
    }
    let state = app.state::<AppState>();
    let same = |base: &str| url::Url::parse(base).is_ok_and(|b| b.origin() == url.origin());
    state.settings().mode == Mode::Remote || same(&state.local.url())
}

/// ボードのページ(`?room=…`)を開いたら、接続先ごとに覚えておく。
fn remember_page(app: &AppHandle, url: &url::Url) {
    if !url.query_pairs().any(|(k, _)| k == "room") {
        return;
    }
    let state = app.state::<AppState>();
    let s = state.settings();
    let local = state.local.url();
    let Some(base) = [Some(local.as_str()), s.server_url.as_deref()]
        .into_iter()
        .flatten()
        .find(|b| url.as_str().starts_with(b))
        .map(str::to_string)
    else {
        return;
    };
    if s.last_pages.get(&base).map(String::as_str) != Some(url.as_str()) {
        state.update(|s| {
            s.last_pages.insert(base, url.to_string());
        });
    }
}

fn open_connect_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(CONNECT) {
        let _ = w.set_focus();
        return;
    }
    let mut b = WebviewWindowBuilder::new(app, CONNECT, WebviewUrl::App("connect.html".into()))
        .title("接続先")
        .inner_size(520.0, 560.0)
        .resizable(false)
        .center();
    if let Some(main) = app.get_webview_window(MAIN) {
        b = b.parent(&main).unwrap_or_else(|_| {
            WebviewWindowBuilder::new(app, CONNECT, WebviewUrl::App("connect.html".into()))
        });
    }
    if let Err(e) = b.build() {
        tracing::error!("接続先の画面を開けません: {e}");
    }
}

#[derive(Serialize)]
struct ConnectionInfo {
    mode: Mode,
    server_url: Option<String>,
    has_ai_key: bool,
    data_dir: Option<String>,
    local_url: String,
}

#[tauri::command]
fn get_connection(state: tauri::State<'_, AppState>) -> ConnectionInfo {
    let s = state.settings();
    ConnectionInfo {
        mode: s.mode,
        server_url: s.server_url.clone(),
        has_ai_key: s.ai_key().is_some(),
        data_dir: state.local.data_dir().map(|p| p.display().to_string()),
        local_url: state.local.url(),
    }
}

/// サーバーに接続できるかを確かめる(接続先の画面の「確かめる」)。
#[tauri::command]
async fn check_server(url: String) -> Result<String, String> {
    let url = normalize_server_url(&url)?;
    if reachable(&url).await {
        Ok(url)
    } else {
        Err(format!("{url} につながりません"))
    }
}

/// 接続先を保存して開き直す。`ai_key` は `None` なら変えない、空文字なら消す。
#[tauri::command]
async fn set_connection(
    app: AppHandle,
    mode: Mode,
    server_url: Option<String>,
    ai_key: Option<String>,
) -> Result<(), String> {
    let server_url = match (mode, server_url.as_deref()) {
        (Mode::Remote, Some(u)) => Some(normalize_server_url(u)?),
        (Mode::Remote, None) => return Err("サーバーの URL を入力してください".into()),
        (Mode::Local, Some(u)) if !u.trim().is_empty() => normalize_server_url(u).ok(),
        (Mode::Local, _) => None,
    };
    let state = app.state::<AppState>();
    let before = state.settings();
    state.update(|s| {
        s.mode = mode;
        if server_url.is_some() || mode == Mode::Remote {
            s.server_url = server_url;
        }
        if let Some(k) = &ai_key {
            s.anthropic_api_key = Some(k.trim().to_string()).filter(|k| !k.is_empty());
        }
    });
    let after = state.settings();
    if before.ai_key() != after.ai_key() {
        let key = after.ai_key().map(str::to_string);
        if let Err(e) = state.local.restart(key.as_deref()).await {
            return Err(format!("内蔵サーバーを再起動できません: {e}"));
        }
    }
    open_target(&app).await;
    if let Some(w) = app.get_webview_window(CONNECT) {
        let _ = w.close();
    }
    Ok(())
}

#[tauri::command]
fn open_data_dir(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let dir = state.local.data_dir().ok_or("保存先がありません")?;
    app.opener()
        .open_path(dir.display().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    // 既定のメニュー(macOS では「編集」がないとコピー・貼り付けのショートカットが効かない)に「接続先」を足す。
    let menu = Menu::default(app)?;
    let conn = Submenu::with_items(
        app,
        "接続先",
        true,
        &[
            &MenuItem::with_id(
                app,
                "conn-local",
                "この PC で使う(オフライン)",
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(
                app,
                "conn-settings",
                "サーバーに接続…",
                true,
                Some("CmdOrCtrl+Shift+K"),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "reload", "再読み込み", true, Some("CmdOrCtrl+R"))?,
            &MenuItem::with_id(
                app,
                "open-data",
                "データのフォルダを開く",
                true,
                None::<&str>,
            )?,
        ],
    )?;
    menu.append(&conn)?;
    Ok(menu)
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "conn-local" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                app.state::<AppState>().update(|s| s.mode = Mode::Local);
                open_target(&app).await;
            });
        }
        "conn-settings" => open_connect_window(app),
        "reload" => {
            if let Some(w) = app.get_webview_window(MAIN) {
                let _ = w.eval("location.reload()");
            }
        }
        "open-data" => {
            let state = app.state::<AppState>();
            if let Some(dir) = state.local.data_dir() {
                let _ = app
                    .opener()
                    .open_path(dir.display().to_string(), None::<&str>);
            }
        }
        _ => {}
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let app = tauri::Builder::default()
        // 2 つ目を起動したら、今のウィンドウを前に出す(保存先のファイルは 1 つのアプリしか開けない)。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window(MAIN) {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_connection,
            check_server,
            set_connection,
            open_data_dir
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let config_dir = app.path().app_config_dir()?;
            let data_dir = app.path().app_data_dir()?;
            let settings = Settings::load(&config_dir);
            let port = settings.local_port.unwrap_or(DEFAULT_LOCAL_PORT);
            let ai_key = settings.ai_key().map(str::to_string);
            let local = tauri::async_runtime::block_on(LocalServer::start(
                &data_dir,
                port,
                ai_key.as_deref(),
            ))
            .map_err(|e| format!("内蔵サーバーを起動できません: {e}"))?;
            let state = AppState {
                local: Arc::new(local),
                settings: Mutex::new(settings),
                config_dir,
            };
            let actual = state.local.port();
            state.update(|s| s.local_port = Some(actual));
            app.manage(state);

            // macOS はアプリのメニュー、Windows / Linux はメインのウィンドウのメニュー
            // (接続先の画面にはメニューを出さない)。
            let menu = build_menu(&handle)?;
            #[cfg(target_os = "macos")]
            app.set_menu(menu)?;
            app.on_menu_event(|app, e| on_menu(app, e.id().as_ref()));

            let nav_handle = handle.clone();
            let open_handle = handle.clone();
            let main = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::App("loading.html".into()))
                .title("グラボード")
                .inner_size(1280.0, 820.0)
                .min_inner_size(480.0, 360.0)
                .center()
                .on_navigation(move |url| {
                    if allow_in_window(&nav_handle, url) {
                        remember_page(&nav_handle, url);
                        true
                    } else {
                        let _ = nav_handle.opener().open_url(url.as_str(), None::<&str>);
                        false
                    }
                })
                .on_new_window(move |url, _features| {
                    // `window.open` / `target=_blank`(共有リンクなど)は既定のブラウザで開く。
                    if matches!(url.scheme(), "http" | "https" | "mailto") {
                        let _ = open_handle.opener().open_url(url.as_str(), None::<&str>);
                        tauri::webview::NewWindowResponse::Deny
                    } else {
                        // blob: / data:(書き出した画像のプレビューなど)はアプリのウィンドウで開く。
                        tauri::webview::NewWindowResponse::Allow
                    }
                });
            #[cfg(not(target_os = "macos"))]
            let main = main.menu(menu);
            main.build()?;

            tauri::async_runtime::spawn(async move { open_target(&handle).await });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("グラボード を起動できません");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            // 未保存の変更を書き出してから終わる。
            app.state::<AppState>().local.shutdown();
        }
    });
}
