//! プラグイン(WASM)の実行パネル。
//!
//! 右上の「プラグイン」から開く。サーバーのプラグインと自分で入れたプラグインのコマンドを並べ、
//! 押すと選択中の要素を渡して実行する(`POST /api/v1/boards/{id}/plugins/{pid}/run`)。
//! ボードへの変更は WebSocket でいつもどおり届く。結果のメッセージを出し、
//! プラグインが選んでほしいと返した要素を選択する。
//! プラグインを入れる・消すのはボード一覧の「開発者向け」から([`crate::developer`])。
//!
//! 権限: 各プラグインが求める権限と接続先を出す。通信・保存のように許可の要る権限を
//! まだ決めていなければ、許可するかどうかを選ぶ欄を出し、決めるまで実行できない。
//! 決めたあとも「権限の設定」から変えられる(`PUT /api/v1/plugins/{id}/grant`)。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::api::Role;
use board_core::ops::{id_string, parse_id};
use board_core::plugin::{
    FetchRecord, GrantRequest, Permission, PluginGrant, PluginInfo, RunRequest, RunResult,
};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement};

use super::{App, by_id, listen};
use crate::http::{self, escape};

/// 権限の欄(許可の確認と設定で共有する)。
fn permission_form(p: &PluginInfo, busy: bool) -> String {
    let m = &p.manifest;
    let grant = p.grant.clone().unwrap_or_default();
    let first = !p.pending.is_empty();
    let mut h = format!(
        r#"<form class="plugin-grant" data-plugin="{}" onsubmit="return false">"#,
        escape(&m.id)
    );
    for perm in Permission::ALL {
        if !m.can(perm) {
            continue;
        }
        let denied = p.denied.permissions.contains(&perm);
        // 初めての確認では、求められているものに印を付けておく(外せば許可しない)。
        let checked = !denied && (grant.allows(perm) || p.pending.permissions.contains(&perm));
        h.push_str(&format!(
            r#"<label class="plugin-perm"><input type="checkbox" name="perm" value="{v}" {c} {d} /><span><strong>{label}</strong><span class="muted small">{detail}</span>{note}</span></label>"#,
            v = perm.as_str(),
            c = if checked { "checked" } else { "" },
            d = if denied { "disabled" } else { "" },
            label = perm.label(),
            detail = perm.detail(),
            note = if denied {
                r#"<span class="muted small">(サーバーの設定で使えません)</span>"#
            } else {
                ""
            },
        ));
        if perm == Permission::Network && !denied {
            h.push_str(r#"<div class="plugin-hosts">"#);
            for host in &m.hosts {
                let blocked = p.denied.hosts.contains(host);
                let checked =
                    !blocked && (grant.allows_host(host) || p.pending.hosts.contains(host));
                h.push_str(&format!(
                    r#"<label class="small"><input type="checkbox" name="host" value="{v}" {c} {d} /><code translate="no">{v}</code>{note}</label>"#,
                    v = escape(host),
                    c = if checked { "checked" } else { "" },
                    d = if blocked { "disabled" } else { "" },
                    note = if blocked { r#" <span class="muted">(サーバーの設定で使えません)</span>"# } else { "" },
                ));
            }
            h.push_str("</div>");
        }
    }
    let dis = if busy { "disabled" } else { "" };
    h.push_str(r#"<div class="row plugin-grant-actions">"#);
    if first {
        h.push_str(&format!(
            r#"<button type="button" class="primary" data-action="save-grant" data-plugin="{id}" {dis}>選んだ権限を許可</button><button type="button" class="quiet" data-action="deny-grant" data-plugin="{id}" {dis}>許可しない</button>"#,
            id = escape(&m.id),
        ));
    } else {
        h.push_str(&format!(
            r#"<button type="button" data-action="save-grant" data-plugin="{id}" {dis}>保存</button><button type="button" class="quiet" data-action="reset-grant" data-plugin="{id}" {dis} title="ボードの読み書き以外の許可を取り消し、次の実行でまた確認します">最初に戻す</button>"#,
            id = escape(&m.id),
        ));
        if let Some(bytes) = p.storage_bytes.filter(|b| *b > 0) {
            h.push_str(&format!(
                r#"<button type="button" class="quiet" data-action="clear-storage" data-plugin="{id}" {dis}>保存したデータを消す({kb} KB)</button>"#,
                id = escape(&m.id),
                kb = bytes.div_ceil(1024),
            ));
        }
    }
    h.push_str("</div></form>");
    h
}

/// 権限の短い一覧(「ボードを読む・外部と通信する(api.github.com)」)。
fn permission_summary(p: &PluginInfo) -> String {
    let m = &p.manifest;
    let grant = p.grant.clone().unwrap_or_default();
    let parts: Vec<String> = m
        .permissions
        .iter()
        .map(|perm| {
            let mut s = escape(perm.label());
            if *perm == Permission::Network {
                s.push_str(&format!(
                    r#"(<span translate="no">{}</span>)"#,
                    escape(&m.hosts.join(", "))
                ));
            }
            let off = p.denied.permissions.contains(perm)
                || (p.pending.is_empty() && p.grant.is_some() && !grant.allows(*perm));
            if off {
                format!(r#"<s title="許可していません">{s}</s>"#)
            } else {
                s
            }
        })
        .collect();
    if parts.is_empty() {
        "権限なし".into()
    } else {
        parts.join("・")
    }
}

#[derive(Default)]
struct State {
    plugins: Vec<PluginInfo>,
    loaded: bool,
    busy: bool,
    /// (メッセージ, エラーか)
    status: Option<(String, bool)>,
    logs: Vec<String>,
    fetches: Vec<FetchRecord>,
}

#[derive(Clone)]
struct Ui {
    toggle: HtmlElement,
    panel: HtmlElement,
    state: Rc<RefCell<State>>,
}

/// ボード ID・鍵・権限。
fn context(app: &Rc<RefCell<App>>) -> Option<(String, Option<String>, Role)> {
    let a = app.borrow();
    if let Some(c) = &a.chrome {
        let c = c.borrow();
        return Some((c.room.clone(), c.key.clone(), c.role()));
    }
    // ログインなしの「誰でも編集できるボード」。
    a.room.clone().map(|r| (r, None, Role::Editor))
}

fn run_path(room: &str, key: Option<&str>, plugin: &str) -> String {
    let base = format!(
        "/v1/boards/{}/plugins/{}/run",
        http::encode(room),
        http::encode(plugin)
    );
    match key {
        Some(k) => format!("{base}?key={}", http::encode(k)),
        None => base,
    }
}

impl Ui {
    fn render(&self, role: Role) {
        let _ = self.toggle.set_attribute(
            "aria-pressed",
            if self.panel.hidden() { "false" } else { "true" },
        );
        if self.panel.hidden() {
            return;
        }
        let st = self.state.borrow();
        let mut html = String::from(
            r#"<header class="row"><strong class="grow">プラグイン</strong><button class="quiet" data-action="close-plugins" aria-label="閉じる">×</button></header>"#,
        );
        match &st.status {
            Some((m, true)) => html.push_str(&format!(
                r#"<p class="plugin-status" data-kind="error" role="alert">{}</p>"#,
                escape(m)
            )),
            Some((m, false)) => html.push_str(&format!(
                r#"<p class="plugin-status" role="status">{}</p>"#,
                escape(m)
            )),
            None => {}
        }
        if !st.fetches.is_empty() {
            html.push_str(&format!(
                r#"<details class="plugin-logs"><summary class="muted small">外部との通信 {} 件</summary><pre translate="no">"#,
                st.fetches.len()
            ));
            for f in &st.fetches {
                let status = f.status.map_or("失敗".to_string(), |s| s.to_string());
                html.push_str(&escape(&format!("{} {} → {}\n", f.method, f.url, status)));
            }
            html.push_str("</pre></details>");
        }
        if !st.logs.is_empty() {
            html.push_str(
                r#"<details class="plugin-logs"><summary class="muted small">ログ</summary><pre>"#,
            );
            for l in &st.logs {
                html.push_str(&escape(l));
                html.push('\n');
            }
            html.push_str("</pre></details>");
        }
        if !st.loaded {
            html.push_str(r#"<p class="muted small">読み込み中…</p>"#);
        } else if st.plugins.is_empty() {
            html.push_str(&format!(
                r#"<p class="muted small">使えるプラグインはまだありません。{}</p>"#,
                crate::hint::html(
                    "ボード一覧の「開発者向け」から自分のプラグイン(.wasm)を入れるか、サーバーの管理者に PLUGIN_DIR を設定してもらってください。"
                )
            ));
        }
        html.push_str(r#"<ul class="plugins">"#);
        for p in &st.plugins {
            let m = &p.manifest;
            let grant = p.grant.clone().unwrap_or_default();
            let writes = m.can(Permission::BoardWrite)
                && grant.allows(Permission::BoardWrite)
                && p.pending.is_empty();
            let blocked = writes && !role.can_edit();
            let needs = !p.pending.is_empty();
            html.push_str(&format!(
                r#"<li class="plugin"><div class="row"><strong class="grow" translate="no">{name}</strong><span class="badge">{src}</span></div>{desc}<p class="muted small">{perms}{ver}</p>"#,
                name = escape(&m.name),
                src = if p.source == "server" { "サーバー" } else { "自分" },
                desc = if m.description.is_empty() {
                    String::new()
                } else {
                    format!(r#"<p class="small" translate="no">{}</p>"#, escape(&m.description))
                },
                perms = permission_summary(p),
                ver = if m.version.is_empty() {
                    String::new()
                } else {
                    format!(" · v{}", escape(&m.version))
                },
            ));
            if needs && p.grant.is_none() {
                html.push_str(
                    r#"<p class="plugin-consent small" role="note">このプラグインは外部との通信やデータの保存を求めています。ログインすると、許可してから実行できます。</p>"#,
                );
            } else if needs {
                html.push_str(
                    r#"<div class="plugin-consent" role="group" aria-label="権限の確認"><p class="small"><strong>このプラグインは次の権限を求めています。</strong>許可するものを選んでください。許可しなかった権限は、プラグインからは使えません。</p>"#,
                );
                html.push_str(&permission_form(p, st.busy));
                html.push_str("</div>");
            } else if p.grant.is_some() && !m.permissions.is_empty() {
                html.push_str(r#"<details class="plugin-settings"><summary class="muted small">権限の設定</summary>"#);
                html.push_str(&permission_form(p, st.busy));
                html.push_str("</details>");
            }
            html.push_str(r#"<div class="plugin-commands">"#);
            for c in &m.commands {
                html.push_str(&format!(
                    r#"<button data-action="run-plugin" data-plugin="{pid}" data-command="{cid}" {dis} title="{tip}" translate="no">{title}</button>"#,
                    pid = escape(&m.id),
                    cid = escape(&c.id),
                    dis = if st.busy || blocked || needs { "disabled" } else { "" },
                    tip = escape(if blocked {
                        "このプラグインはボードを編集するので、編集権限が必要です"
                    } else if needs {
                        "先に権限を許可するかどうか決めてください"
                    } else if c.description.is_empty() {
                        "選択中の要素(なければボード全体)に実行"
                    } else {
                        &c.description
                    }),
                    title = escape(&c.title),
                ));
            }
            html.push_str("</div></li>");
        }
        html.push_str("</ul>");
        self.panel.set_inner_html(&html);
    }
}

fn load(app: Rc<RefCell<App>>, ui: Ui) {
    wasm_bindgen_futures::spawn_local(async move {
        let res = http::get::<Vec<PluginInfo>>("/v1/plugins").await;
        {
            let mut st = ui.state.borrow_mut();
            st.loaded = true;
            match res {
                Ok(list) => st.plugins = list,
                Err(e) => st.status = Some((e.message(), true)),
            }
        }
        let role = context(&app).map_or(Role::Viewer, |c| c.2);
        ui.render(role);
    });
}

fn run(app: Rc<RefCell<App>>, ui: Ui, plugin: String, command: String) {
    let Some((room, key, role)) = context(&app) else {
        return;
    };
    let selection: Vec<String> = app
        .borrow()
        .editor
        .selection()
        .iter()
        .map(|id| id_string(*id))
        .collect();
    {
        let mut st = ui.state.borrow_mut();
        if st.busy {
            return;
        }
        st.busy = true;
        st.status = Some(("実行中…".into(), false));
        st.logs.clear();
        st.fetches.clear();
    }
    ui.render(role);
    wasm_bindgen_futures::spawn_local(async move {
        let req = RunRequest {
            command,
            args: serde_json::Value::Null,
            selection,
        };
        let res =
            http::send::<_, RunResult>("POST", &run_path(&room, key.as_deref(), &plugin), &req)
                .await;
        {
            let mut st = ui.state.borrow_mut();
            st.busy = false;
            match res {
                Ok(r) => {
                    let msg = r.output.message.clone().unwrap_or_else(|| {
                        if r.ops > 0 {
                            format!("実行しました({} 件の変更)", r.ops)
                        } else {
                            "実行しました".into()
                        }
                    });
                    st.status = Some((msg, false));
                    st.logs = r.logs;
                    st.fetches = r.fetches;
                    if let Some(ids) = r.output.select {
                        let ids = ids.iter().filter_map(|s| parse_id(s)).collect();
                        let mut a = app.borrow_mut();
                        a.editor.set_selection(ids);
                        a.dirty = true;
                    }
                }
                Err(e) => st.status = Some((e.message(), true)),
            }
        }
        ui.render(role);
        // 許可が変わっているかもしれないので読み直す。
        load(app, ui);
    });
}

/// 権限の欄で選ばれているもの。
fn checked(form: &Element, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(list) = form.query_selector_all(&format!("input[name={name}]:checked")) {
        for i in 0..list.length() {
            if let Some(v) = list
                .item(i)
                .and_then(|n| n.dyn_into::<Element>().ok())
                .and_then(|e| e.get_attribute("value"))
            {
                out.push(v);
            }
        }
    }
    out
}

/// 許可を保存する・取り消す・保存領域を消す。
fn change_grant(app: Rc<RefCell<App>>, ui: Ui, el: Element, action: String) {
    let Some(pid) = el.get_attribute("data-plugin") else {
        return;
    };
    let base = format!("/v1/plugins/{}", http::encode(&pid));
    let req = match action.as_str() {
        "save-grant" => {
            let Some(form) = el.closest("form").ok().flatten() else {
                return;
            };
            let allowed: Vec<Permission> = checked(&form, "perm")
                .iter()
                .filter_map(|v| Permission::ALL.into_iter().find(|p| p.as_str() == v))
                .collect();
            Some(GrantRequest {
                allowed,
                hosts: checked(&form, "host"),
            })
        }
        // 許可しない: ボードの読み書きだけ残す。
        "deny-grant" => Some(GrantRequest {
            allowed: vec![Permission::BoardRead, Permission::BoardWrite],
            hosts: Vec::new(),
        }),
        _ => None,
    };
    let role = context(&app).map_or(Role::Viewer, |c| c.2);
    ui.state.borrow_mut().busy = true;
    ui.render(role);
    wasm_bindgen_futures::spawn_local(async move {
        let res: Result<String, http::HttpError> = match (action.as_str(), req) {
            (_, Some(req)) => http::send::<_, PluginGrant>("PUT", &format!("{base}/grant"), &req)
                .await
                .map(|_| "権限の設定を保存しました".to_string()),
            ("reset-grant", None) => http::send_empty("DELETE", &format!("{base}/grant"))
                .await
                .map(|_| "権限の設定を最初に戻しました".to_string()),
            ("clear-storage", None) => http::send_empty("DELETE", &format!("{base}/storage"))
                .await
                .map(|_| "保存したデータを消しました".to_string()),
            _ => return,
        };
        {
            let mut st = ui.state.borrow_mut();
            st.busy = false;
            st.status = Some(match res {
                Ok(m) => (m, false),
                Err(e) => (e.message(), true),
            });
        }
        load(app, ui);
    });
}

fn action_of(e: &web_sys::Event) -> Option<Element> {
    e.target()?
        .dyn_into::<Element>()
        .ok()?
        .closest("[data-action]")
        .ok()?
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    let ui = Ui {
        toggle: by_id(doc, "plugins-toggle")?,
        panel: by_id(doc, "plugins-panel")?,
        state: Rc::default(),
    };
    // サーバーにつながるボードでだけ出す。
    ui.toggle.set_hidden(context(app).is_none());
    {
        let app = app.clone();
        let ui2 = ui.clone();
        listen(
            ui.toggle.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                let open = ui2.panel.hidden();
                ui2.panel.set_hidden(!open);
                let role = context(&app).map_or(Role::Viewer, |c| c.2);
                if open {
                    // 開くたびに読み直す(入れたばかりのプラグインも出るように)。
                    load(app.clone(), ui2.clone());
                }
                ui2.render(role);
            },
        )?;
    }
    {
        let app = app.clone();
        let ui2 = ui.clone();
        listen(
            ui.panel.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                let Some(el) = action_of(&e) else { return };
                match el.get_attribute("data-action").as_deref() {
                    Some("close-plugins") => {
                        ui2.panel.set_hidden(true);
                        ui2.render(Role::Viewer);
                    }
                    Some("run-plugin") => {
                        let (Some(p), Some(c)) = (
                            el.get_attribute("data-plugin"),
                            el.get_attribute("data-command"),
                        ) else {
                            return;
                        };
                        run(app.clone(), ui2.clone(), p, c);
                    }
                    Some(a @ ("save-grant" | "deny-grant" | "reset-grant" | "clear-storage")) => {
                        change_grant(app.clone(), ui2.clone(), el.clone(), a.to_string());
                    }
                    _ => {}
                }
            },
        )?;
    }
    Ok(())
}
