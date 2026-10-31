//! ボード一覧とログインの画面(`?room=` のないとき)。
//!
//! GPU は使わず、普通の HTML を組み立てて表示する。ボードを選ぶと `?room=…` を開き直す。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::api::{BoardInfo, Login, Me, NewBoard, Role, Signup};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element, HtmlElement, HtmlInputElement, Window};

use crate::http::{self, HttpError, escape};

/// ホーム画面を出すか、従来どおりの動き(新しいルームを作る)にするか。
pub enum Outcome {
    /// ホーム画面を表示した(ボード画面は始めない)。
    Shown,
    /// API のないサーバーなので、ボード画面をそのまま始める。
    Fallback,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    Login,
    Signup,
}

struct Home {
    window: Window,
    root: HtmlElement,
    me: Option<Me>,
    boards: Vec<BoardInfo>,
    mode: AuthMode,
    error: Option<String>,
    busy: bool,
    /// 削除の確認中のボード。
    confirm_delete: Option<String>,
    /// ログイン後に戻る先(`?next=`)。
    next: Option<String>,
}

type Shared = Rc<RefCell<Home>>;

/// ログイン後の戻り先として受け付けるのは、同じページの `?room=…` だけにする。
fn safe_next(next: Option<String>) -> Option<String> {
    next.filter(|n| n.starts_with("?room=") && !n.contains("//"))
}

pub async fn run(window: &Window, document: &Document) -> Result<Outcome, String> {
    let me = match http::get::<Me>("/me").await {
        Ok(me) => Some(me),
        Err(HttpError::Status(401, _)) => None,
        Err(HttpError::Status(_, m)) => return Err(m),
        Err(HttpError::Unavailable) => return Ok(Outcome::Fallback),
    };
    let root: HtmlElement = document
        .get_element_by_id("home")
        .ok_or("#home が見つかりません")?
        .dyn_into()
        .map_err(|_| "#home の種類が違います")?;
    if let Some(body) = document.body() {
        let _ = body.class_list().add_1("home-mode");
    }
    root.set_hidden(false);
    let params = window
        .location()
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok());
    let next = safe_next(params.and_then(|p| p.get("next")));

    let home: Shared = Rc::new(RefCell::new(Home {
        window: window.clone(),
        root: root.clone(),
        me,
        boards: Vec::new(),
        mode: AuthMode::Login,
        error: None,
        busy: false,
        confirm_delete: None,
        next,
    }));
    install(&home)?;
    if home.borrow().me.is_some() {
        reload(home.clone()).await;
    } else {
        home.borrow().render();
    }
    Ok(Outcome::Shown)
}

fn role_badge(role: Role) -> String {
    format!(
        r#"<span class="badge" data-role="{}">{}</span>"#,
        role.as_str(),
        role.label()
    )
}

/// 「3 分前」のような表示。
pub fn ago(ms: u64) -> String {
    if ms == 0 {
        return String::new();
    }
    let now = js_sys::Date::now();
    let s = ((now - ms as f64) / 1000.0).max(0.0) as u64;
    match s {
        0..60 => "たった今".into(),
        60..3600 => format!("{} 分前", s / 60),
        3600..86400 => format!("{} 時間前", s / 3600),
        _ => {
            let d = js_sys::Date::new(&JsValue::from_f64(ms as f64));
            format!(
                "{}/{}/{}",
                d.get_full_year(),
                d.get_month() + 1,
                d.get_date()
            )
        }
    }
}

pub fn board_url(id: &str, key: Option<&str>) -> String {
    match key {
        Some(k) => format!("?room={}&key={}", http::encode(id), http::encode(k)),
        None => format!("?room={}", http::encode(id)),
    }
}

impl Home {
    fn render(&self) {
        let html = match &self.me {
            None => self.render_auth(),
            Some(me) => self.render_list(me),
        };
        self.root.set_inner_html(&html);
        // フォームの最初の入力欄へ。
        if let Ok(Some(el)) = self.root.query_selector("[autofocus]")
            && let Ok(el) = el.dyn_into::<HtmlElement>()
        {
            let _ = el.focus();
        }
    }

    fn error_html(&self) -> String {
        self.error
            .as_ref()
            .map(|e| format!(r#"<p class="form-error" role="alert">{}</p>"#, escape(e)))
            .unwrap_or_default()
    }

    fn render_auth(&self) -> String {
        let signup = self.mode == AuthMode::Signup;
        let name_field = if signup {
            r#"<label>表示名<input name="name" autocomplete="nickname" maxlength="40" placeholder="カーソルやコメントに出る名前" /></label>"#
        } else {
            ""
        };
        format!(
            r#"<div class="home-card auth">
  <h1>グラボード</h1>
  <p class="muted">{lead}</p>
  <form data-form="auth">
    <label>ログイン名<input name="login" autocomplete="username" required autofocus maxlength="32" placeholder="英小文字・数字・-・_" /></label>
    {name_field}
    <label>パスワード<input name="password" type="password" autocomplete="{pw}" required minlength="{min}" /></label>
    {err}
    <button type="submit" class="primary" {busy}>{submit}</button>
  </form>
  <p class="switch">{switch_lead} <button type="button" class="link" data-action="switch">{switch}</button></p>
  <hr />
  <button type="button" data-action="anonymous">ログインせずに新しいボードを作る</button>
  <p class="muted small">ログインせずに作ったボードは、URL を知っている人なら誰でも編集できます。</p>
</div>"#,
            lead = if signup {
                "アカウントを作ると、ボードの一覧・共有の設定・コメントが使えます。"
            } else {
                "ログインしてボードを開きましょう。"
            },
            pw = if signup {
                "new-password"
            } else {
                "current-password"
            },
            min = if signup { 8 } else { 1 },
            err = self.error_html(),
            busy = if self.busy { "disabled" } else { "" },
            submit = if signup {
                "アカウントを作る"
            } else {
                "ログイン"
            },
            switch_lead = if signup {
                "アカウントをお持ちの方は"
            } else {
                "はじめての方は"
            },
            switch = if signup {
                "ログイン"
            } else {
                "アカウントを作る"
            },
        )
    }

    fn render_list(&self, me: &Me) -> String {
        let mut rows = String::new();
        for b in &self.boards {
            let url = board_url(&b.id, b.key.as_deref());
            let owner = match &b.owner {
                Some(o) if o != &me.login => format!("@{}", escape(o)),
                Some(_) => String::new(),
                None => "誰でも編集可".into(),
            };
            let actions = if b.role == Role::Owner {
                if self.confirm_delete.as_deref() == Some(b.id.as_str()) {
                    format!(
                        r#"<button class="danger" data-action="delete" data-id="{id}">本当に削除</button><button data-action="cancel-delete">やめる</button>"#,
                        id = escape(&b.id)
                    )
                } else {
                    format!(
                        r#"<button class="quiet" data-action="ask-delete" data-id="{}" title="削除">削除</button>"#,
                        escape(&b.id)
                    )
                }
            } else {
                String::new()
            };
            rows.push_str(&format!(
                r#"<li class="board-row"><a href="{url}"><span class="title" translate="no">{title}</span><span class="meta">{badge}<span>{owner}</span><span>{ago}</span></span></a><span class="actions">{actions}</span></li>"#,
                url = escape(&url),
                title = escape(&b.title),
                badge = role_badge(b.role),
                owner = owner,
                ago = ago(b.updated),
            ));
        }
        if self.boards.is_empty() {
            rows =
                r#"<li class="empty muted">まだボードがありません。上の欄から作りましょう。</li>"#
                    .into();
        }
        format!(
            r#"<div class="home-card list">
  <header class="home-head">
    <h1>グラボード</h1>
    <span class="me">{name} <span class="muted">@{login}</span></span>
    <button type="button" class="quiet" data-action="logout">ログアウト</button>
  </header>
  <form data-form="create" class="create">
    <input name="title" maxlength="80" placeholder="新しいボードの名前" autofocus />
    <button type="submit" class="primary" {busy}>ボードを作る</button>
  </form>
  {err}
  <ul class="boards">{rows}</ul>
</div>"#,
            name = escape(&me.name),
            login = escape(&me.login),
            busy = if self.busy { "disabled" } else { "" },
            err = self.error_html(),
        )
    }
}

fn form_value(form: &Element, name: &str) -> String {
    form.query_selector(&format!("[name={name}]"))
        .ok()
        .flatten()
        .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
        .map(|i| i.value())
        .unwrap_or_default()
}

async fn reload(home: Shared) {
    let result = http::get::<Vec<BoardInfo>>("/boards").await;
    let mut h = home.borrow_mut();
    match result {
        Ok(list) => {
            h.boards = list;
            h.error = None;
        }
        Err(HttpError::Status(401, _)) => h.me = None,
        Err(e) => h.error = Some(e.message()),
    }
    h.render();
}

fn navigate(window: &Window, search: &str) {
    let _ = window.location().set_search(search);
}

/// ログインせずに使う: 新しいルーム名を作って開く(従来の動き)。
pub fn random_room() -> String {
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    (0..10)
        .map(|_| {
            CHARS[(js_sys::Math::random() * CHARS.len() as f64) as usize % CHARS.len()] as char
        })
        .collect()
}

fn install(home: &Shared) -> Result<(), String> {
    let root = home.borrow().root.clone();

    let h = home.clone();
    let on_submit = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        e.prevent_default();
        let Some(form) = e.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        if h.borrow().busy {
            return;
        }
        let kind = form.get_attribute("data-form").unwrap_or_default();
        let h = h.clone();
        h.borrow_mut().busy = true;
        wasm_bindgen_futures::spawn_local(async move {
            match kind.as_str() {
                "auth" => {
                    let login = form_value(&form, "login");
                    let password = form_value(&form, "password");
                    let signup = h.borrow().mode == AuthMode::Signup;
                    let res = if signup {
                        let name = form_value(&form, "name");
                        http::send::<_, Me>(
                            "POST",
                            "/signup",
                            &Signup {
                                login,
                                name,
                                password,
                            },
                        )
                        .await
                    } else {
                        http::send::<_, Me>("POST", "/login", &Login { login, password }).await
                    };
                    match res {
                        Ok(me) => {
                            // 表示名をカーソルの名前にも使う。
                            crate::presence::remember_name(&h.borrow().window, &me.name);
                            let next = h.borrow().next.clone();
                            if let Some(next) = next {
                                navigate(&h.borrow().window, &next);
                                return;
                            }
                            {
                                let mut hm = h.borrow_mut();
                                hm.me = Some(me);
                                hm.busy = false;
                                hm.error = None;
                            }
                            reload(h).await;
                        }
                        Err(e) => {
                            let mut hm = h.borrow_mut();
                            hm.busy = false;
                            hm.error = Some(e.message());
                            hm.render();
                        }
                    }
                }
                "create" => {
                    let title = form_value(&form, "title");
                    match http::send::<_, BoardInfo>("POST", "/boards", &NewBoard { title }).await {
                        Ok(b) => navigate(&h.borrow().window, &board_url(&b.id, None)),
                        Err(e) => {
                            let mut hm = h.borrow_mut();
                            hm.busy = false;
                            hm.error = Some(e.message());
                            hm.render();
                        }
                    }
                }
                _ => h.borrow_mut().busy = false,
            }
        });
    });
    root.add_event_listener_with_callback("submit", on_submit.as_ref().unchecked_ref())
        .map_err(|_| "submit を登録できません")?;
    on_submit.forget();

    let h = home.clone();
    let on_click = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |e: web_sys::MouseEvent| {
        let Some(el) = e
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|t| t.closest("[data-action]").ok().flatten())
        else {
            return;
        };
        let action = el.get_attribute("data-action").unwrap_or_default();
        let id = el.get_attribute("data-id");
        match action.as_str() {
            "switch" => {
                let mut hm = h.borrow_mut();
                hm.mode = match hm.mode {
                    AuthMode::Login => AuthMode::Signup,
                    AuthMode::Signup => AuthMode::Login,
                };
                hm.error = None;
                hm.render();
            }
            "anonymous" => {
                let room = random_room();
                navigate(&h.borrow().window, &board_url(&room, None));
            }
            "logout" => {
                let h = h.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = http::send_empty("POST", "/logout").await;
                    let mut hm = h.borrow_mut();
                    hm.me = None;
                    hm.boards.clear();
                    hm.error = None;
                    hm.render();
                });
            }
            "ask-delete" => {
                let mut hm = h.borrow_mut();
                hm.confirm_delete = id;
                hm.render();
            }
            "cancel-delete" => {
                let mut hm = h.borrow_mut();
                hm.confirm_delete = None;
                hm.render();
            }
            "delete" => {
                let Some(id) = id else { return };
                let h = h.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let res = http::send_empty("DELETE", &http::board_path(&id, None, "")).await;
                    h.borrow_mut().confirm_delete = None;
                    if let Err(e) = res {
                        h.borrow_mut().error = Some(e.message());
                    }
                    reload(h).await;
                });
            }
            _ => {}
        }
    });
    root.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())
        .map_err(|_| "click を登録できません")?;
    on_click.forget();
    Ok(())
}
