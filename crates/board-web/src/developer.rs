//! ボード一覧の「開発者向け」: API トークンとプラグインの管理。
//!
//! - API トークン(`/api/tokens`): 作る・一覧・無効にする。作ったトークンはその場で一度だけ見せる。
//! - プラグイン(`/api/v1/plugins`): 自分の `.wasm` を入れる・消す。サーバーのプラグインも一覧に出す。
//!
//! ボード一覧は描き直すたびに中身を作り直すので、この欄は 1 つの要素を持ち続け、
//! 描き直しのたびに [`mount`] で一覧の下に付け直す(開閉や入力中の内容が消えないように)。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::access::{CreatedToken, NewToken, TokenInfo, TokenScope};
use board_core::plugin::PluginInfo;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Element, HtmlElement, HtmlInputElement, HtmlSelectElement};

use crate::home::ago;
use crate::http::{self, escape};

#[derive(Default)]
struct State {
    loaded: bool,
    tokens: Vec<TokenInfo>,
    plugins: Vec<PluginInfo>,
    /// 作ったばかりのトークン(この画面でだけ見せる)。
    fresh: Option<CreatedToken>,
    message: Option<(String, bool)>,
    confirm_revoke: Option<String>,
    busy: bool,
}

struct Dev {
    root: HtmlElement,
    body: HtmlElement,
    state: RefCell<State>,
}

thread_local! {
    static DEV: RefCell<Option<Rc<Dev>>> = const { RefCell::new(None) };
}

/// ボード一覧(`.home-card.list`)の下に付ける。ログインしていなければ外す。
pub fn mount(home_root: &HtmlElement, logged_in: bool) {
    let dev = match DEV.with(|d| d.borrow().clone()) {
        Some(d) => d,
        None => {
            if !logged_in {
                return;
            }
            match create(home_root) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("開発者向けの欄を作れません: {e}");
                    return;
                }
            }
        }
    };
    if !logged_in {
        dev.root.remove();
        // 次にログインした人には、その人のものを読み直す。
        *dev.state.borrow_mut() = State::default();
        return;
    }
    if let Ok(Some(card)) = home_root.query_selector(".home-card.list") {
        let _ = card.append_child(&dev.root);
    }
}

fn create(home_root: &HtmlElement) -> Result<Rc<Dev>, String> {
    let doc = home_root.owner_document().ok_or("document がありません")?;
    let root: HtmlElement = doc
        .create_element("details")
        .map_err(|_| "details を作れません")?
        .dyn_into()
        .map_err(|_| "details の種類が違います")?;
    root.set_class_name("dev-tools");
    root.set_inner_html(
        r#"<summary>開発者向け: API トークンとプラグイン</summary><div class="dev-body"></div>"#,
    );
    let body: HtmlElement = root
        .query_selector(".dev-body")
        .ok()
        .flatten()
        .and_then(|e| e.dyn_into().ok())
        .ok_or("dev-body がありません")?;
    let dev = Rc::new(Dev {
        root,
        body,
        state: RefCell::default(),
    });
    install(&dev)?;
    DEV.with(|d| *d.borrow_mut() = Some(dev.clone()));
    Ok(dev)
}

fn scope_label(s: TokenScope) -> &'static str {
    match s {
        TokenScope::Read => "読むだけ",
        TokenScope::Write => "読み書き",
    }
}

impl Dev {
    fn render(&self) {
        let st = self.state.borrow();
        let mut h = String::new();
        if let Some((m, err)) = &st.message {
            h.push_str(&format!(
                r#"<p class="dev-message" {kind} role="status">{m}</p>"#,
                kind = if *err { r#"data-kind="error""# } else { "" },
                m = escape(m)
            ));
        }
        if !st.loaded {
            h.push_str(r#"<p class="muted small">読み込み中…</p>"#);
            self.body.set_inner_html(&h);
            return;
        }

        // ---- トークン ----
        h.push_str(&format!(
            "<h3>API トークン{}</h3>",
            crate::hint::html(
                "外部のスクリプトや連携から <code>/api/v1</code> の REST API でボードを操作するためのトークンです。使い方は docs/api.md にあります。"
            )
        ));
        if let Some(t) = &st.fresh {
            h.push_str(&format!(
                r#"<div class="token-fresh"><p><strong>「{name}」を作りました。</strong>このトークンは今しか表示されません。安全な場所に控えてください。</p><div class="row"><input class="grow" readonly value="{token}" data-select-all translate="no" aria-label="作ったトークン" /><button type="button" data-action="copy-token" data-token="{token}">コピー</button><button type="button" class="quiet" data-action="hide-token">閉じる</button></div></div>"#,
                name = escape(&t.info.name),
                token = escape(&t.token),
            ));
        }
        h.push_str(
            r#"<form data-form="token" class="dev-form"><input name="name" maxlength="60" placeholder="用途(例: CI から付箋を置く)" required /><select name="scope" aria-label="できること"><option value="write">読み書き</option><option value="read">読むだけ</option></select><select name="expires" aria-label="有効期間"><option value="90">90 日</option><option value="30">30 日</option><option value="365">1 年</option><option value="">無期限</option></select><button type="submit" class="primary">トークンを作る</button></form>"#,
        );
        if st.tokens.is_empty() {
            h.push_str(r#"<p class="muted small">まだトークンはありません。</p>"#);
        } else {
            h.push_str(r#"<ul class="dev-list">"#);
            for t in &st.tokens {
                let actions = if st.confirm_revoke.as_deref() == Some(t.id.as_str()) {
                    format!(
                        r#"<button type="button" class="danger" data-action="revoke" data-id="{id}">本当に無効にする</button><button type="button" data-action="cancel-revoke">やめる</button>"#,
                        id = escape(&t.id)
                    )
                } else {
                    format!(
                        r#"<button type="button" class="quiet" data-action="ask-revoke" data-id="{}">無効にする</button>"#,
                        escape(&t.id)
                    )
                };
                let boards = match &t.boards {
                    Some(b) => format!(" · ボード {} 枚に限定", b.len()),
                    None => String::new(),
                };
                let expires = match t.expires {
                    Some(e) if e < js_sys::Date::now() as u64 => " · 期限切れ".to_string(),
                    Some(e) => format!(" · {}まで", date(e)),
                    None => String::new(),
                };
                let used = match t.last_used {
                    Some(u) => format!(" · 最後に使った: {}", ago(u)),
                    None => " · まだ使っていません".into(),
                };
                h.push_str(&format!(
                    r#"<li><div class="grow"><strong translate="no">{name}</strong> <span class="badge">{scope}</span><div class="muted small">作成 {created}{boards}{expires}{used}</div></div><span class="actions">{actions}</span></li>"#,
                    name = escape(&t.name),
                    scope = scope_label(t.scope),
                    created = date(t.created),
                ));
            }
            h.push_str("</ul>");
        }

        // ---- プラグイン ----
        h.push_str(&format!(
            "<h3>プラグイン{}</h3>",
            crate::hint::html(
                "ボードに機能を足す WASM プラグインです。入れたプラグインは自分だけが使えます。ボードを開き、右上の「プラグイン」から実行します。作り方は docs/plugins.md にあります。"
            )
        ));
        h.push_str(
            r#"<label class="dev-upload"><span>プラグイン(.wasm)を入れる</span><input type="file" accept=".wasm,application/wasm" data-action-change="upload" /></label>"#,
        );
        if st.plugins.is_empty() {
            h.push_str(r#"<p class="muted small">まだプラグインはありません。</p>"#);
        } else {
            h.push_str(r#"<ul class="dev-list">"#);
            for p in &st.plugins {
                let m = &p.manifest;
                let perms: Vec<String> = m
                    .permissions
                    .iter()
                    .map(|p| {
                        if *p == board_core::plugin::Permission::Network {
                            format!(
                                r#"{}(<span translate="no">{}</span>)"#,
                                p.label(),
                                escape(&m.hosts.join(", "))
                            )
                        } else {
                            p.label().to_string()
                        }
                    })
                    .collect();
                let actions = if p.source == "user" {
                    format!(
                        r#"<button type="button" class="quiet" data-action="uninstall" data-id="{}">消す</button>"#,
                        escape(&m.id)
                    )
                } else {
                    String::new()
                };
                h.push_str(&format!(
                    r#"<li><div class="grow"><strong translate="no">{name}</strong> <span class="badge">{src}</span><div class="muted small"><span translate="no">{id}{ver}</span> · {perms} · コマンド {n} 個 · {kb} KB</div></div><span class="actions">{actions}</span></li>"#,
                    name = escape(&m.name),
                    src = if p.source == "server" { "サーバー" } else { "自分" },
                    id = escape(&m.id),
                    ver = if m.version.is_empty() {
                        String::new()
                    } else {
                        format!(" v{}", escape(&m.version))
                    },
                    perms = if perms.is_empty() {
                        "権限なし".into()
                    } else {
                        perms.join("・")
                    },
                    n = m.commands.len(),
                    kb = p.size.div_ceil(1024),
                ));
            }
            h.push_str("</ul>");
        }
        self.body.set_inner_html(&h);
    }
}

/// 「2026/10/31」のような表示。
fn date(ms: u64) -> String {
    let d = js_sys::Date::new(&JsValue::from_f64(ms as f64));
    format!(
        "{}/{}/{}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date()
    )
}

async fn reload(dev: Rc<Dev>) {
    let tokens = http::get::<Vec<TokenInfo>>("/tokens").await;
    let plugins = http::get::<Vec<PluginInfo>>("/v1/plugins").await;
    {
        let mut st = dev.state.borrow_mut();
        st.loaded = true;
        st.busy = false;
        match tokens {
            Ok(t) => st.tokens = t,
            Err(e) => st.message = Some((e.message(), true)),
        }
        // プラグインに対応していない古いサーバーなら空のまま。
        st.plugins = plugins.unwrap_or_default();
    }
    dev.render();
}

fn finish(dev: &Rc<Dev>, res: Result<String, String>) {
    let dev = dev.clone();
    {
        let mut st = dev.state.borrow_mut();
        st.message = Some(match res {
            Ok(m) => (m, false),
            Err(m) => (m, true),
        });
    }
    wasm_bindgen_futures::spawn_local(reload(dev));
}

fn field(form: &Element, name: &str) -> String {
    let Ok(Some(el)) = form.query_selector(&format!("[name={name}]")) else {
        return String::new();
    };
    if let Some(i) = el.dyn_ref::<HtmlInputElement>() {
        return i.value();
    }
    el.dyn_ref::<HtmlSelectElement>()
        .map(|s| s.value())
        .unwrap_or_default()
}

fn install(dev: &Rc<Dev>) -> Result<(), String> {
    let root = dev.root.clone();

    // 開いたときに読み込む。
    {
        let d = dev.clone();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let open = d.root.has_attribute("open");
            if open && !d.state.borrow().loaded {
                d.render();
                wasm_bindgen_futures::spawn_local(reload(d.clone()));
            }
        });
        root.add_event_listener_with_callback("toggle", cb.as_ref().unchecked_ref())
            .map_err(|_| "toggle を登録できません")?;
        cb.forget();
    }

    // トークンを作る。
    {
        let d = dev.clone();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
            e.prevent_default();
            let Some(form) = e.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
                return;
            };
            if form.get_attribute("data-form").as_deref() != Some("token") || d.state.borrow().busy
            {
                return;
            }
            let req = NewToken {
                name: field(&form, "name"),
                scope: if field(&form, "scope") == "read" {
                    TokenScope::Read
                } else {
                    TokenScope::Write
                },
                boards: None,
                expires_in_days: field(&form, "expires").parse().ok(),
            };
            d.state.borrow_mut().busy = true;
            let d = d.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match http::send::<_, CreatedToken>("POST", "/tokens", &req).await {
                    Ok(t) => {
                        {
                            let mut st = d.state.borrow_mut();
                            st.fresh = Some(t);
                            st.message = None;
                        }
                        reload(d).await;
                    }
                    Err(e) => {
                        d.state.borrow_mut().busy = false;
                        finish(&d, Err(e.message()));
                    }
                }
            });
        });
        root.add_event_listener_with_callback("submit", cb.as_ref().unchecked_ref())
            .map_err(|_| "submit を登録できません")?;
        cb.forget();
    }

    // ボタン。
    {
        let d = dev.clone();
        let cb = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |e: web_sys::MouseEvent| {
            let Some(el) = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|t| t.closest("[data-action]").ok().flatten())
            else {
                // トークンの欄を押したら全体を選ぶ。
                if let Some(i) = e
                    .target()
                    .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
                    .filter(|i| i.has_attribute("data-select-all"))
                {
                    i.select();
                }
                return;
            };
            let id = el.get_attribute("data-id").unwrap_or_default();
            match el.get_attribute("data-action").as_deref() {
                Some("copy-token") => {
                    let token = el.get_attribute("data-token").unwrap_or_default();
                    if let Some(w) = web_sys::window() {
                        let _ = w.navigator().clipboard().write_text(&token);
                    }
                    el.set_text_content(Some("コピーしました"));
                }
                Some("hide-token") => {
                    d.state.borrow_mut().fresh = None;
                    d.render();
                }
                Some("ask-revoke") => {
                    d.state.borrow_mut().confirm_revoke = Some(id);
                    d.render();
                }
                Some("cancel-revoke") => {
                    d.state.borrow_mut().confirm_revoke = None;
                    d.render();
                }
                Some("revoke") => {
                    let d = d.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let res =
                            http::send_empty("DELETE", &format!("/tokens/{}", http::encode(&id)))
                                .await;
                        d.state.borrow_mut().confirm_revoke = None;
                        finish(
                            &d,
                            res.map(|_| "トークンを無効にしました".to_string())
                                .map_err(|e| e.message()),
                        );
                    });
                }
                Some("uninstall") => {
                    let d = d.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let res = http::send_empty(
                            "DELETE",
                            &format!("/v1/plugins/{}", http::encode(&id)),
                        )
                        .await;
                        finish(
                            &d,
                            res.map(|_| "プラグインを消しました".to_string())
                                .map_err(|e| e.message()),
                        );
                    });
                }
                _ => {}
            }
        });
        root.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
            .map_err(|_| "click を登録できません")?;
        cb.forget();
    }

    // .wasm を選んだら入れる。
    {
        let d = dev.clone();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
            let Some(input) = e
                .target()
                .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
                .filter(|i| i.get_attribute("data-action-change").as_deref() == Some("upload"))
            else {
                return;
            };
            let Some(file) = input.files().and_then(|f| f.get(0)) else {
                return;
            };
            let d = d.clone();
            d.state.borrow_mut().message = Some(("入れています…".into(), false));
            d.render();
            wasm_bindgen_futures::spawn_local(async move {
                let bytes = match JsFuture::from(file.array_buffer()).await {
                    Ok(buf) => js_sys::Uint8Array::new(&buf).to_vec(),
                    Err(_) => {
                        finish(&d, Err("ファイルを読めません".into()));
                        return;
                    }
                };
                let res = http::send_bytes::<PluginInfo>(
                    "POST",
                    "/v1/plugins",
                    "application/wasm",
                    &bytes,
                )
                .await;
                finish(
                    &d,
                    res.map(|p| format!("「{}」を入れました", p.manifest.name))
                        .map_err(|e| e.message()),
                );
            });
        });
        root.add_event_listener_with_callback("change", cb.as_ref().unchecked_ref())
            .map_err(|_| "change を登録できません")?;
        cb.forget();
    }
    Ok(())
}
