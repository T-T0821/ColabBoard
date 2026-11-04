//! ボード一覧の「通知の設定」: メール・プッシュ通知(スマホ・PC)で受け取る通知。
//!
//! - メールアドレスを登録すると確認のメールが届き、リンクを開くと使えるようになる。
//! - プッシュ通知はブラウザ(端末)ごとに有効にする。中身はサーバーから取り直して表示する(`sw.js`)。
//! - メンション・返信・期限のどれを受け取るかを、メールとプッシュ通知で別々に選べる。
//!
//! [`crate::developer`] と同じく 1 つの要素を持ち続け、描き直しのたびに一覧の下へ付け直す。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::api::{NotifyKinds, NotifyStatus, PushSubscribe, PushUnsubscribe, SetNotify};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Element, HtmlElement, HtmlInputElement};

use crate::http::{self, escape};

#[wasm_bindgen(inline_js = r#"
function keyBytes(b64) {
  const pad = '='.repeat((4 - (b64.length % 4)) % 4);
  const raw = atob((b64 + pad).replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}
function sameKey(buf, b64) {
  if (!buf) return true;
  const a = new Uint8Array(buf);
  const b = keyBytes(b64);
  return a.length === b.length && a.every((v, i) => v === b[i]);
}
function supported() {
  return 'serviceWorker' in navigator && 'PushManager' in globalThis && 'Notification' in globalThis;
}
async function registration() {
  let reg = await navigator.serviceWorker.getRegistration();
  if (!reg) reg = await navigator.serviceWorker.register('./sw.js');
  return navigator.serviceWorker.ready.then(() => reg);
}
// 'unsupported' | 'denied' | 'on' | 'off'
export async function push_state() {
  if (!supported()) return 'unsupported';
  if (Notification.permission === 'denied') return 'denied';
  const reg = await navigator.serviceWorker.getRegistration();
  const sub = reg ? await reg.pushManager.getSubscription() : null;
  return sub ? 'on' : 'off';
}
// 購読して PushSubscription.toJSON() の JSON を返す。
export async function push_subscribe(key) {
  if (!supported()) throw new Error('このブラウザはプッシュ通知に対応していません');
  const perm = await Notification.requestPermission();
  if (perm !== 'granted') throw new Error('通知が許可されませんでした。ブラウザの設定で通知を許可してください');
  const reg = await registration();
  let sub = await reg.pushManager.getSubscription();
  if (sub && !sameKey(sub.options && sub.options.applicationServerKey, key)) {
    await sub.unsubscribe();
    sub = null;
  }
  if (!sub) {
    sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: keyBytes(key) });
  }
  return JSON.stringify(sub.toJSON());
}
// 購読をやめて、その送り先 URL を返す(なければ空)。
export async function push_unsubscribe() {
  if (!supported()) return '';
  const reg = await navigator.serviceWorker.getRegistration();
  const sub = reg ? await reg.pushManager.getSubscription() : null;
  if (!sub) return '';
  const endpoint = sub.endpoint;
  await sub.unsubscribe();
  return endpoint;
}
export function is_ios_browser() {
  const ios = /iPad|iPhone|iPod/.test(navigator.userAgent) ||
    (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
  const standalone = matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
  return ios && !standalone;
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn push_state() -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    fn push_subscribe(key: &str) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(catch)]
    fn push_unsubscribe() -> Result<js_sys::Promise, JsValue>;
    fn is_ios_browser() -> bool;
}

fn js_err(e: JsValue) -> String {
    e.dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| "プッシュ通知を設定できません".into())
}

async fn call_js(p: Result<js_sys::Promise, JsValue>) -> Result<String, String> {
    let v = JsFuture::from(p.map_err(js_err)?).await.map_err(js_err)?;
    Ok(v.as_string().unwrap_or_default())
}

#[derive(Default)]
struct State {
    loaded: bool,
    status: Option<NotifyStatus>,
    /// このブラウザのプッシュ通知: unsupported / denied / on / off
    push: String,
    message: Option<(String, bool)>,
    busy: bool,
}

struct Panel {
    root: HtmlElement,
    body: HtmlElement,
    state: RefCell<State>,
}

thread_local! {
    static PANEL: RefCell<Option<Rc<Panel>>> = const { RefCell::new(None) };
}

/// ボード一覧(`.home-card.list`)の下に付ける。ログインしていなければ外す。
pub fn mount(home_root: &HtmlElement, logged_in: bool) {
    let panel = match PANEL.with(|d| d.borrow().clone()) {
        Some(d) => d,
        None => {
            if !logged_in {
                return;
            }
            match create(home_root) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("通知の設定の欄を作れません: {e}");
                    return;
                }
            }
        }
    };
    if !logged_in {
        panel.root.remove();
        *panel.state.borrow_mut() = State::default();
        return;
    }
    if let Ok(Some(card)) = home_root.query_selector(".home-card.list") {
        let _ = card.append_child(&panel.root);
    }
}

fn create(home_root: &HtmlElement) -> Result<Rc<Panel>, String> {
    let doc = home_root.owner_document().ok_or("document がありません")?;
    let root: HtmlElement = doc
        .create_element("details")
        .map_err(|_| "details を作れません")?
        .dyn_into()
        .map_err(|_| "details の種類が違います")?;
    root.set_class_name("dev-tools notify-settings");
    root.set_inner_html(
        r#"<summary>通知の設定: メール・スマホへのプッシュ通知</summary><div class="dev-body"></div>"#,
    );
    let body: HtmlElement = root
        .query_selector(".dev-body")
        .ok()
        .flatten()
        .and_then(|e| e.dyn_into().ok())
        .ok_or("dev-body がありません")?;
    let panel = Rc::new(Panel {
        root,
        body,
        state: RefCell::default(),
    });
    // 確認のメールのリンクから戻ってきたら、開いて結果を見せる。
    let from_link = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("notify"));
    if let Some(r) = from_link {
        panel.state.borrow_mut().message = Some(match r.as_str() {
            "verified" => (
                "メールアドレスを確認しました。これから通知がメールでも届きます".into(),
                false,
            ),
            _ => (
                "確認のリンクが古いか、すでに使われています。もう一度アドレスを保存してください"
                    .into(),
                true,
            ),
        });
        let _ = panel.root.set_attribute("open", "");
        panel.render();
        wasm_bindgen_futures::spawn_local(reload(panel.clone()));
    }
    install(&panel)?;
    PANEL.with(|d| *d.borrow_mut() = Some(panel.clone()));
    Ok(panel)
}

fn checks(prefix: &str, k: NotifyKinds, disabled: bool) -> String {
    let d = if disabled { " disabled" } else { "" };
    let one = |key: &str, label: &str, on: bool| {
        format!(
            r#"<label class="check small"><input type="checkbox" data-kind="{prefix}:{key}"{c}{d} /> {label}</label>"#,
            c = if on { " checked" } else { "" },
        )
    };
    format!(
        r#"<div class="notify-kinds">{}{}{}</div>"#,
        one("mention", "メンション", k.mention),
        one("reply", "スレッドへの返信", k.reply),
        one("due", "コメントの期限(近づいた・過ぎた)", k.due),
    )
}

impl Panel {
    fn render(&self) {
        let st = self.state.borrow();
        let mut h = String::from(
            r#"<p class="muted small">コメントでメンションされたとき、参加しているスレッドに返信があったとき、コメントの期限が近づいた・過ぎたときに、画面のベルに加えてメールやプッシュ通知でも知らせます。</p>"#,
        );
        if let Some((m, err)) = &st.message {
            h.push_str(&format!(
                r#"<p class="dev-message" {kind} role="status">{m}</p>"#,
                kind = if *err { r#"data-kind="error""# } else { "" },
                m = escape(m)
            ));
        }
        let Some(s) = st.status.as_ref().filter(|_| st.loaded) else {
            h.push_str(r#"<p class="muted small">読み込み中…</p>"#);
            self.body.set_inner_html(&h);
            return;
        };
        let busy = if st.busy { " disabled" } else { "" };

        // ---- メール ----
        h.push_str("<h3>メール</h3>");
        if s.email_available {
            let state = if s.email.is_empty() {
                String::new()
            } else if s.email_verified {
                r#"<p class="small notify-ok">✓ 確認済みのアドレスに届きます</p>"#.into()
            } else {
                r#"<p class="small notify-warn">確認のメールを送りました。メールのリンクを開くと届くようになります(届かなければ迷惑メールのフォルダも見てください)</p>"#.into()
            };
            h.push_str(&format!(
                r#"<form data-form="email" class="dev-form"><input name="email" type="email" maxlength="254" placeholder="you@example.com" value="{email}" autocomplete="email" aria-label="通知を受け取るメールアドレス" /><button type="submit" class="primary"{busy}>保存</button></form>{state}"#,
                email = escape(&s.email),
            ));
            h.push_str(&checks("email", s.email_on, s.email.is_empty()));
        } else {
            h.push_str(r#"<p class="muted small">このサーバーではメールを送れません(管理者が <code>SMTP_URL</code> と <code>MAIL_FROM</code> を設定すると使えます)。</p>"#);
        }

        // ---- プッシュ通知 ----
        h.push_str("<h3>プッシュ通知(スマホ・PC)</h3>");
        if s.push_key.is_none() {
            h.push_str(r#"<p class="muted small">このサーバーではプッシュ通知を使えません。</p>"#);
        } else {
            let line = match st.push.as_str() {
                "on" => format!(
                    r#"<p class="small notify-ok">✓ このブラウザで受け取っています</p><div class="row"><button type="button" data-action="push-off"{busy}>このブラウザではやめる</button></div>"#
                ),
                "denied" => r#"<p class="small notify-warn">このブラウザでは通知がブロックされています。サイトの設定で通知を許可してから、もう一度開いてください。</p>"#.to_string(),
                "unsupported" if is_ios_browser() => r#"<p class="small notify-warn">iPhone・iPad では、共有メニューの「ホーム画面に追加」で追加したアプリから開くと有効にできます(iOS 16.4 以降)。</p>"#.to_string(),
                "unsupported" => r#"<p class="muted small">このブラウザはプッシュ通知に対応していません。</p>"#.to_string(),
                _ => format!(
                    r#"<div class="row"><button type="button" class="primary" data-action="push-on"{busy}>このブラウザで受け取る</button></div>{ios}"#,
                    ios = if is_ios_browser() {
                        r#"<p class="muted small">iPhone・iPad では、「ホーム画面に追加」したアプリから有効にしてください。</p>"#
                    } else {
                        ""
                    }
                ),
            };
            h.push_str(&line);
            h.push_str(&checks("push", s.push_on, false));
            if s.push_devices > 0 {
                h.push_str(&format!(
                    r#"<p class="muted small">受け取る端末: {} 台(端末ごとに、その端末で有効にします)</p>"#,
                    s.push_devices
                ));
            }
        }

        let can_test = (s.email_available && s.email_verified) || s.push_devices > 0;
        if can_test {
            h.push_str(&format!(
                r#"<div class="row"><button type="button" data-action="notify-test"{busy}>テスト送信</button><span class="muted small">登録した送り先すべてにテストの通知を送ります</span></div>"#
            ));
        }
        self.body.set_inner_html(&h);
    }

    /// 画面のチェックボックスから設定を作る。
    fn kinds_from_dom(&self, prefix: &str, fallback: NotifyKinds) -> NotifyKinds {
        let get = |key: &str, d: bool| {
            self.body
                .query_selector(&format!(r#"[data-kind="{prefix}:{key}"]"#))
                .ok()
                .flatten()
                .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
                .map_or(d, |i| i.checked())
        };
        NotifyKinds {
            mention: get("mention", fallback.mention),
            reply: get("reply", fallback.reply),
            due: get("due", fallback.due),
        }
    }
}

async fn reload(p: Rc<Panel>) {
    let status = http::get::<NotifyStatus>("/notify/settings").await;
    let push = call_js(push_state())
        .await
        .unwrap_or_else(|_| "unsupported".into());
    {
        let mut st = p.state.borrow_mut();
        st.loaded = true;
        st.busy = false;
        st.push = push;
        match status {
            Ok(s) => st.status = Some(s),
            Err(e) => st.message = Some((e.message(), true)),
        }
    }
    p.render();
}

fn finish(p: &Rc<Panel>, res: Result<String, String>) {
    {
        let mut st = p.state.borrow_mut();
        st.message = Some(match res {
            Ok(m) => (m, false),
            Err(m) => (m, true),
        });
    }
    wasm_bindgen_futures::spawn_local(reload(p.clone()));
}

/// 設定を保存する。`email` が `None` なら今のアドレスのまま。
fn save(p: &Rc<Panel>, email: Option<String>, done: &'static str) {
    let Some(s) = p.state.borrow().status.clone() else {
        return;
    };
    let req = SetNotify {
        email: email.clone().unwrap_or_else(|| s.email.clone()),
        email_on: p.kinds_from_dom("email", s.email_on),
        push_on: p.kinds_from_dom("push", s.push_on),
    };
    let changed_email = email.is_some_and(|e| e.trim() != s.email);
    p.state.borrow_mut().busy = true;
    let p = p.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let res = http::send::<_, NotifyStatus>("PUT", "/notify/settings", &req).await;
        let msg = match &res {
            Ok(st) if changed_email && !st.email.is_empty() => {
                format!("{} に確認のメールを送りました", st.email)
            }
            Ok(_) if changed_email => "メールでの通知をやめました".into(),
            Ok(_) => done.into(),
            Err(_) => String::new(),
        };
        finish(&p, res.map(|_| msg).map_err(|e| e.message()));
    });
}

fn install(p: &Rc<Panel>) -> Result<(), String> {
    let root = p.root.clone();

    // 開いたときに読み込む。
    {
        let d = p.clone();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if d.root.has_attribute("open") && !d.state.borrow().loaded {
                d.render();
                wasm_bindgen_futures::spawn_local(reload(d.clone()));
            }
        });
        root.add_event_listener_with_callback("toggle", cb.as_ref().unchecked_ref())
            .map_err(|_| "toggle を登録できません")?;
        cb.forget();
    }

    // メールアドレスの保存。
    {
        let d = p.clone();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
            e.prevent_default();
            let Some(form) = e.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
                return;
            };
            if form.get_attribute("data-form").as_deref() != Some("email") || d.state.borrow().busy
            {
                return;
            }
            let email = form
                .query_selector("[name=email]")
                .ok()
                .flatten()
                .and_then(|i| i.dyn_into::<HtmlInputElement>().ok())
                .map(|i| i.value())
                .unwrap_or_default();
            save(&d, Some(email.trim().to_string()), "保存しました");
        });
        root.add_event_listener_with_callback("submit", cb.as_ref().unchecked_ref())
            .map_err(|_| "submit を登録できません")?;
        cb.forget();
    }

    // 種類のチェックボックス。
    {
        let d = p.clone();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
            let is_kind = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .is_some_and(|t| t.has_attribute("data-kind"));
            if is_kind && !d.state.borrow().busy {
                save(&d, None, "保存しました");
            }
        });
        root.add_event_listener_with_callback("change", cb.as_ref().unchecked_ref())
            .map_err(|_| "change を登録できません")?;
        cb.forget();
    }

    // ボタン。
    {
        let d = p.clone();
        let cb = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |e: web_sys::MouseEvent| {
            let Some(el) = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|t| t.closest("[data-action]").ok().flatten())
            else {
                return;
            };
            if d.state.borrow().busy {
                return;
            }
            match el.get_attribute("data-action").as_deref() {
                Some("push-on") => {
                    let Some(key) = d
                        .state
                        .borrow()
                        .status
                        .as_ref()
                        .and_then(|s| s.push_key.clone())
                    else {
                        return;
                    };
                    d.state.borrow_mut().busy = true;
                    d.render();
                    let d = d.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let res = async {
                            let json = call_js(push_subscribe(&key)).await?;
                            let sub: PushSubscribe =
                                serde_json::from_str(&json).map_err(|e| e.to_string())?;
                            http::send::<_, serde_json::Value>("POST", "/notify/push", &sub)
                                .await
                                .map_err(|e| e.message())?;
                            Ok::<_, String>("このブラウザでプッシュ通知を受け取ります".into())
                        }
                        .await;
                        finish(&d, res);
                    });
                }
                Some("push-off") => {
                    d.state.borrow_mut().busy = true;
                    d.render();
                    let d = d.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let res = async {
                            let endpoint = call_js(push_unsubscribe()).await?;
                            if !endpoint.is_empty() {
                                http::send::<_, serde_json::Value>(
                                    "DELETE",
                                    "/notify/push",
                                    &PushUnsubscribe { endpoint },
                                )
                                .await
                                .map_err(|e| e.message())?;
                            }
                            Ok::<_, String>("このブラウザのプッシュ通知をやめました".into())
                        }
                        .await;
                        finish(&d, res);
                    });
                }
                Some("notify-test") => {
                    d.state.borrow_mut().busy = true;
                    let d = d.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let res = http::send_empty("POST", "/notify/test").await;
                        finish(
                            &d,
                            res.map(|_| "テストの通知を送りました".to_string())
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
    Ok(())
}
