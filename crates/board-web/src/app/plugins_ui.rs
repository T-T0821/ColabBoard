//! プラグイン(WASM)の実行パネル。
//!
//! 右上の「プラグイン」から開く。サーバーのプラグインと自分で入れたプラグインのコマンドを並べ、
//! 押すと選択中の要素を渡して実行する(`POST /api/v1/boards/{id}/plugins/{pid}/run`)。
//! ボードへの変更は WebSocket でいつもどおり届く。結果のメッセージを出し、
//! プラグインが選んでほしいと返した要素を選択する。
//! プラグインを入れる・消すのはボード一覧の「開発者向け」から([`crate::developer`])。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::api::Role;
use board_core::ops::{id_string, parse_id};
use board_core::plugin::{Permission, PluginInfo, RunRequest, RunResult};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement};

use super::{App, by_id, listen};
use crate::http::{self, escape};

#[derive(Default)]
struct State {
    plugins: Vec<PluginInfo>,
    loaded: bool,
    busy: bool,
    /// (メッセージ, エラーか)
    status: Option<(String, bool)>,
    logs: Vec<String>,
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
            let writes = m.can(Permission::BoardWrite);
            let blocked = writes && !role.can_edit();
            let perms: Vec<&str> = m.permissions.iter().map(|p| p.label()).collect();
            html.push_str(&format!(
                r#"<li class="plugin"><div class="row"><strong class="grow" translate="no">{name}</strong><span class="badge">{src}</span></div>{desc}<p class="muted small">{perms}{ver}</p><div class="plugin-commands">"#,
                name = escape(&m.name),
                src = if p.source == "server" { "サーバー" } else { "自分" },
                desc = if m.description.is_empty() {
                    String::new()
                } else {
                    format!(r#"<p class="small" translate="no">{}</p>"#, escape(&m.description))
                },
                perms = if perms.is_empty() {
                    "権限なし".to_string()
                } else {
                    perms.join("・")
                },
                ver = if m.version.is_empty() {
                    String::new()
                } else {
                    format!(" · v{}", escape(&m.version))
                },
            ));
            for c in &m.commands {
                html.push_str(&format!(
                    r#"<button data-action="run-plugin" data-plugin="{pid}" data-command="{cid}" {dis} title="{tip}" translate="no">{title}</button>"#,
                    pid = escape(&m.id),
                    cid = escape(&c.id),
                    dis = if st.busy || blocked { "disabled" } else { "" },
                    tip = escape(if blocked {
                        "このプラグインはボードを編集するので、編集権限が必要です"
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
                    _ => {}
                }
            },
        )?;
    }
    Ok(())
}
