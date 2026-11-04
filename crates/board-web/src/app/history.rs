//! バージョン履歴: 一覧、名前を付けて保存、昔の状態の表示(プレビュー)と復元。
//! ボードを GitHub のリポジトリにつないでいれば、ここから同期・プルリクエストもできる。
//!
//! プレビュー中は昔の状態を読み込んだ別の `Board` を表示し、本物のボードは
//! `App::live_board` に預けておく(届いた更新はそちらに反映し続ける)。プレビュー中は編集できない。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::Board;
use board_core::api::{
    GithubLink, GithubStatus, GithubSyncMode, GithubSyncRequest, GithubSyncResult, NewVersion,
    VersionInfo,
};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, HtmlInputElement};

use super::{App, by_id, listen};
use crate::home::ago;
use crate::http::{self, escape};

#[derive(Default)]
struct Inbox {
    list: Option<Vec<VersionInfo>>,
    preview: Option<(VersionInfo, Vec<u8>)>,
    message: Option<String>,
    /// 復元が終わった。
    restored: bool,
    /// GitHub のつながり(`Some(None)` はつながっていない)。
    github: Option<Option<GithubLink>>,
    github_status: Option<GithubStatus>,
    github_note: Option<GithubNote>,
}

/// GitHub の操作の結果の表示。
#[derive(Clone, Debug)]
struct GithubNote {
    text: String,
    /// コミット・プルリクエストの URL。
    link: Option<(String, String)>,
    error: bool,
}

pub struct History {
    toggle: HtmlElement,
    panel: HtmlElement,
    banner: HtmlElement,
    list: Vec<VersionInfo>,
    inbox: Rc<RefCell<Inbox>>,
    /// 表示中のバージョン。
    previewing: Option<VersionInfo>,
    message: Option<String>,
    busy: bool,
    /// 閉じている間に履歴が変わった。
    stale: bool,
    enabled: bool,
    github: Option<GithubLink>,
    github_status: Option<GithubStatus>,
    github_note: Option<GithubNote>,
    github_busy: bool,
    /// 確認中の上書き。
    github_confirm: Option<GithubSyncMode>,
}

/// 「10/31 19:05」のような表示。
fn when(ms: u64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms as f64));
    format!(
        "{}/{} {:02}:{:02}",
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes()
    )
}

fn title_of(v: &VersionInfo) -> String {
    v.label.clone().unwrap_or_else(|| "自動保存".into())
}

impl History {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            toggle: by_id(document, "history-toggle")?,
            panel: by_id(document, "history-panel")?,
            banner: by_id(document, "preview-banner")?,
            list: Vec::new(),
            inbox: Rc::default(),
            previewing: None,
            message: None,
            busy: false,
            stale: true,
            enabled: false,
            github: None,
            github_status: None,
            github_note: None,
            github_busy: false,
            github_confirm: None,
        })
    }
}

impl App {
    fn history_ctx(&self) -> Option<(String, Option<String>, bool)> {
        let c = self.chrome.as_ref()?.borrow();
        Some((c.room.clone(), c.key.clone(), c.role().can_edit()))
    }

    fn fetch_versions(&mut self) {
        let Some((room, key, _)) = self.history_ctx() else {
            return;
        };
        self.history.stale = false;
        let inbox = self.history.inbox.clone();
        let can_edit = self.history_ctx().is_some_and(|c| c.2);
        wasm_bindgen_futures::spawn_local(async move {
            match http::get::<Vec<VersionInfo>>(&http::board_path(
                &room,
                key.as_deref(),
                "/versions",
            ))
            .await
            {
                Ok(list) => inbox.borrow_mut().list = Some(list),
                Err(e) => inbox.borrow_mut().message = Some(e.message()),
            }
            // GitHub の同期は編集できる人だけ。API のない古いサーバーでは出さない。
            if can_edit
                && let Ok(link) = http::get::<Option<GithubLink>>(&http::board_path(
                    &room,
                    key.as_deref(),
                    "/github",
                ))
                .await
            {
                inbox.borrow_mut().github = Some(link);
            }
        });
    }

    /// GitHub と同期する(`None` なら状態を確かめるだけ)。
    fn github_request(&mut self, req: Option<GithubSyncRequest>) {
        let Some((room, key, _)) = self.history_ctx() else {
            return;
        };
        if self.history.github_busy {
            return;
        }
        self.history.github_busy = true;
        self.history.github_note = Some(GithubNote {
            text: if req.is_some() {
                "GitHub と同期しています…".into()
            } else {
                "GitHub の状態を確かめています…".into()
            },
            link: None,
            error: false,
        });
        self.render_history();
        let inbox = self.history.inbox.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let note = match req {
                None => match http::get::<GithubStatus>(&http::board_path(
                    &room,
                    key.as_deref(),
                    "/github/status",
                ))
                .await
                {
                    Ok(st) => {
                        let text = status_text(&st);
                        inbox.borrow_mut().github_status = Some(st);
                        GithubNote {
                            text,
                            link: None,
                            error: false,
                        }
                    }
                    Err(e) => GithubNote {
                        text: e.message(),
                        link: None,
                        error: true,
                    },
                },
                Some(req) => match http::send::<_, GithubSyncResult>(
                    "POST",
                    &http::board_path(&room, key.as_deref(), "/github/sync"),
                    &req,
                )
                .await
                {
                    Ok(r) => {
                        let note = result_note(&r);
                        let mut i = inbox.borrow_mut();
                        i.github = Some(Some(r.link));
                        i.github_status = None;
                        note
                    }
                    Err(e) => GithubNote {
                        text: e.message(),
                        link: None,
                        error: true,
                    },
                },
            };
            inbox.borrow_mut().github_note = Some(note);
            if let Ok(list) =
                http::get::<Vec<VersionInfo>>(&http::board_path(&room, key.as_deref(), "/versions"))
                    .await
            {
                inbox.borrow_mut().list = Some(list);
            }
        });
    }

    /// サーバーから「履歴が変わった」と届いた。
    pub(super) fn versions_changed(&mut self) {
        if self.history.panel.hidden() {
            self.history.stale = true;
        } else {
            self.fetch_versions();
        }
    }

    pub(super) fn history_frame(&mut self) {
        let enabled = self.chrome.is_some();
        if enabled != self.history.enabled {
            self.history.enabled = enabled;
            self.history.toggle.set_hidden(!enabled);
        }
        let mut inbox = self.history.inbox.borrow_mut();
        let list = inbox.list.take();
        let preview = inbox.preview.take();
        let message = inbox.message.take();
        let restored = std::mem::take(&mut inbox.restored);
        let github = inbox.github.take();
        let github_status = inbox.github_status.take();
        let github_note = inbox.github_note.take();
        drop(inbox);
        let mut changed = false;
        if let Some(link) = github {
            self.history.github = link;
            changed = true;
        }
        if let Some(st) = github_status {
            self.history.github_status = Some(st);
            changed = true;
        }
        if let Some(note) = github_note {
            self.history.github_note = Some(note);
            self.history.github_busy = false;
            changed = true;
        }
        if let Some(list) = list {
            self.history.list = list;
            changed = true;
        }
        if let Some(m) = message {
            self.history.message = Some(m);
            self.history.busy = false;
            changed = true;
        }
        if let Some((info, bytes)) = preview {
            self.start_preview(info, &bytes);
            changed = true;
        }
        if restored {
            self.history.busy = false;
            self.end_preview();
            self.history.message = Some("復元しました。戻す前の状態も履歴に残っています".into());
            changed = true;
        }
        if changed {
            self.render_history();
        }
    }

    fn start_preview(&mut self, info: VersionInfo, bytes: &[u8]) {
        let mut old = Board::new();
        if let Err(e) = old.apply_remote(bytes) {
            self.history.message = Some(format!("読み込めませんでした: {e}"));
            return;
        }
        self.history.busy = false;
        self.end_text_edit(true);
        if self.editor.is_dragging() {
            self.editor.cancel(&mut self.board);
        }
        self.editor.clear_selection();
        let live = std::mem::replace(&mut self.board, old);
        // すでにプレビュー中なら、預けている本物のボードはそのまま。
        if self.live_board.is_none() {
            self.live_board = Some(live);
        }
        self.history.previewing = Some(info);
        if let Some(body) = self.document.body() {
            let _ = body.set_attribute("data-preview", "on");
        }
        self.last_count = None;
        self.dirty = true;
    }

    fn end_preview(&mut self) {
        if let Some(live) = self.live_board.take() {
            self.board = live;
            self.editor.clear_selection();
        }
        self.history.previewing = None;
        if let Some(body) = self.document.body() {
            let _ = body.remove_attribute("data-preview");
        }
        self.last_count = None;
        self.dirty = true;
    }

    /// Esc: プレビューをやめるか、パネルを閉じる。
    pub(super) fn history_escape(&mut self) -> bool {
        if self.history.previewing.is_some() {
            self.end_preview();
            self.render_history();
            return true;
        }
        if !self.history.panel.hidden() {
            self.history.panel.set_hidden(true);
            self.render_history();
            return true;
        }
        false
    }

    fn render_history(&self) {
        let h = &self.history;
        let can_edit = self.history_ctx().is_some_and(|c| c.2);
        let _ = h.toggle.set_attribute(
            "aria-pressed",
            if h.panel.hidden() { "false" } else { "true" },
        );

        // プレビューの帯
        match &h.previewing {
            Some(v) => {
                h.banner.set_inner_html(&format!(
                    r#"<span>表示中: <strong>{title}</strong> <span class="muted">{when}{by}</span></span>{restore}<button data-action="exit-preview">今の状態に戻る</button>"#,
                    title = escape(&title_of(v)),
                    when = when(v.created),
                    by = v
                        .author_name
                        .as_ref()
                        .map(|n| format!(" · {}", escape(n)))
                        .unwrap_or_default(),
                    restore = if can_edit {
                        format!(
                            r#"<button class="primary" data-action="restore" data-id="{}" {}>このバージョンに戻す</button>"#,
                            v.id,
                            if h.busy { "disabled" } else { "" }
                        )
                    } else {
                        String::new()
                    },
                ));
                h.banner.set_hidden(false);
            }
            None => h.banner.set_hidden(true),
        }

        if h.panel.hidden() {
            return;
        }
        let mut html = String::from(
            r#"<header class="row"><strong class="grow">バージョン履歴</strong><button class="quiet" data-action="close-history" aria-label="閉じる">×</button></header>"#,
        );
        if can_edit {
            html.push_str(
                r#"<form class="row" data-form="save-version"><input class="grow" name="label" maxlength="80" placeholder="今の状態に名前を付けて保存" /><button type="submit">保存</button></form>"#,
            );
        }
        if can_edit && let Some(link) = &h.github {
            html.push_str(&github_html(
                link,
                h.github_status.as_ref(),
                h.github_note.as_ref(),
                h.github_busy,
                h.github_confirm,
            ));
        }
        if let Some(m) = &h.message {
            html.push_str(&format!(r#"<p class="muted small">{}</p>"#, escape(m)));
        }
        html.push_str(&format!(
            r#"<ul class="versions"><li><button class="version {cur}" data-action="exit-preview"><strong>今の状態</strong><span class="muted small">編集中のボード</span></button></li>"#,
            cur = if h.previewing.is_none() { "open" } else { "" }
        ));
        for v in &h.list {
            let open = h.previewing.as_ref().is_some_and(|p| p.id == v.id);
            html.push_str(&format!(
                r#"<li><button class="version {open} {auto}" data-action="preview" data-id="{id}"><strong>{title}</strong><span class="muted small">{when}({ago}){by} · 要素 {n}</span></button></li>"#,
                open = if open { "open" } else { "" },
                auto = if v.label.is_none() { "auto" } else { "" },
                id = v.id,
                title = escape(&title_of(v)),
                when = when(v.created),
                ago = ago(v.created),
                by = v
                    .author_name
                    .as_ref()
                    .map(|n| format!(" · {}", escape(n)))
                    .unwrap_or_default(),
                n = v.elements,
            ));
        }
        if h.list.is_empty() {
            html.push_str(
                r#"<li class="muted small">まだ履歴はありません。編集すると 10 分ごとに自動で保存されます。</li>"#,
            );
        }
        html.push_str("</ul>");
        h.panel.set_inner_html(&html);
    }

    fn on_history_action(&mut self, action: &str, el: &Element) {
        let id = el
            .get_attribute("data-id")
            .and_then(|i| i.parse::<u64>().ok());
        let Some((room, key, _)) = self.history_ctx() else {
            return;
        };
        match action {
            "close-history" => {
                self.history.panel.set_hidden(true);
                self.render_history();
            }
            "exit-preview" => {
                self.end_preview();
                self.render_history();
            }
            "preview" => {
                let Some(info) =
                    id.and_then(|id| self.history.list.iter().find(|v| v.id == id).cloned())
                else {
                    return;
                };
                self.history.busy = true;
                self.history.message = None;
                let inbox = self.history.inbox.clone();
                let path =
                    http::board_path(&room, key.as_deref(), &format!("/versions/{}", info.id));
                wasm_bindgen_futures::spawn_local(async move {
                    match http::get_bytes(&path).await {
                        Ok(bytes) => inbox.borrow_mut().preview = Some((info, bytes)),
                        Err(e) => inbox.borrow_mut().message = Some(e.message()),
                    }
                });
            }
            "restore" => {
                let Some(id) = id else { return };
                if self.history.busy {
                    return;
                }
                self.history.busy = true;
                self.render_history();
                let inbox = self.history.inbox.clone();
                let path =
                    http::board_path(&room, key.as_deref(), &format!("/versions/{id}/restore"));
                let list_path = http::board_path(&room, key.as_deref(), "/versions");
                wasm_bindgen_futures::spawn_local(async move {
                    match http::send::<_, VersionInfo>("POST", &path, &serde_json::json!({})).await
                    {
                        Ok(_) => inbox.borrow_mut().restored = true,
                        Err(e) => inbox.borrow_mut().message = Some(e.message()),
                    }
                    if let Ok(list) = http::get::<Vec<VersionInfo>>(&list_path).await {
                        inbox.borrow_mut().list = Some(list);
                    }
                });
            }
            "github-status" => self.github_request(None),
            "github-pr" => {
                let message = self.github_message();
                self.github_request(Some(GithubSyncRequest {
                    mode: GithubSyncMode::Sync,
                    message,
                    pull_request: true,
                }));
            }
            // 上書きは、ボタンの下に出す確認を押してから。
            "github-pull" | "github-push" => {
                self.history.github_confirm = Some(if action == "github-pull" {
                    GithubSyncMode::Pull
                } else {
                    GithubSyncMode::Push
                });
                self.render_history();
            }
            "github-confirm-cancel" => {
                self.history.github_confirm = None;
                self.render_history();
            }
            "github-confirm" => {
                let Some(mode) = self.history.github_confirm.take() else {
                    return;
                };
                let message = self.github_message();
                self.github_request(Some(GithubSyncRequest {
                    mode,
                    message,
                    pull_request: false,
                }));
            }
            _ => {}
        }
    }

    /// 同期の欄に入力したコミットメッセージ。
    fn github_message(&self) -> Option<String> {
        self.history
            .panel
            .query_selector("[data-form=github-sync] [name=message]")
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
            .map(|i| i.value().trim().to_string())
            .filter(|m| !m.is_empty())
    }

    fn save_version(&mut self, label: String) {
        let Some((room, key, _)) = self.history_ctx() else {
            return;
        };
        let inbox = self.history.inbox.clone();
        let path = http::board_path(&room, key.as_deref(), "/versions");
        let label = (!label.trim().is_empty()).then_some(label);
        wasm_bindgen_futures::spawn_local(async move {
            match http::send::<_, VersionInfo>("POST", &path, &NewVersion { label }).await {
                Ok(v) => {
                    inbox.borrow_mut().message = Some(format!("「{}」を保存しました", title_of(&v)))
                }
                Err(e) => inbox.borrow_mut().message = Some(e.message()),
            }
            if let Ok(list) = http::get::<Vec<VersionInfo>>(&path).await {
                inbox.borrow_mut().list = Some(list);
            }
        });
    }
}

fn status_text(st: &GithubStatus) -> String {
    match (st.remote_exists, st.board_changed, st.repo_changed) {
        (false, _, _) => "リポジトリにはまだファイルがありません。同期すると作ります".into(),
        (true, false, false) => "ボードとリポジトリは同じです".into(),
        (true, true, false) => "ボードに、まだリポジトリにない変更があります".into(),
        (true, false, true) => "リポジトリに、まだボードにない変更があります".into(),
        (true, true, true) => {
            "ボードとリポジトリの両方に変更があります。同期するとマージします".into()
        }
    }
}

fn changes(c: &board_core::api::ChangeCount) -> String {
    let mut parts = Vec::new();
    if c.added > 0 {
        parts.push(format!("追加 {}", c.added));
    }
    if c.updated > 0 {
        parts.push(format!("変更 {}", c.updated));
    }
    if c.removed > 0 {
        parts.push(format!("削除 {}", c.removed));
    }
    parts.join("・")
}

fn result_note(r: &GithubSyncResult) -> GithubNote {
    let mut text = Vec::new();
    let board = changes(&r.board);
    if !board.is_empty() {
        text.push(format!("リポジトリの変更をボードに取り込みました({board})"));
    }
    let repo = changes(&r.repo);
    if r.pull_request_url.is_some() {
        text.push(format!(
            "プルリクエストを出しました{}",
            if repo.is_empty() {
                String::new()
            } else {
                format!("({repo})")
            }
        ));
    } else if r.commit.is_some() {
        text.push(if repo.is_empty() {
            "コミットしました".to_string()
        } else {
            format!("コミットしました({repo})")
        });
    }
    if text.is_empty() {
        text.push("変更はありません。ボードとリポジトリは同じです".into());
    }
    if !r.conflicts.is_empty() {
        let list: Vec<&str> = r.conflicts.iter().take(5).map(String::as_str).collect();
        text.push(format!(
            "両方で変わっていた {} 件はボード側を残しました: {}{}",
            r.conflicts.len(),
            list.join("、"),
            if r.conflicts.len() > 5 { " ほか" } else { "" }
        ));
    }
    let link = match (&r.pull_request_url, &r.commit_url, &r.commit) {
        (Some(pr), _, _) => Some((pr.clone(), "プルリクエストを開く".to_string())),
        (None, Some(url), Some(sha)) => Some((
            url.clone(),
            format!("コミット {}", sha.chars().take(7).collect::<String>()),
        )),
        _ => None,
    };
    GithubNote {
        text: text.join("。"),
        link,
        error: false,
    }
}

/// 履歴パネルの GitHub の欄。
fn github_html(
    link: &GithubLink,
    status: Option<&GithubStatus>,
    note: Option<&GithubNote>,
    busy: bool,
    confirm: Option<GithubSyncMode>,
) -> String {
    let last = match &link.last {
        Some(l) => format!(
            r#"最後の同期: <a href="{url}" target="_blank" rel="noopener">{when}</a>{by}"#,
            url = escape(&l.url),
            when = when(l.at),
            by =
                l.by.as_ref()
                    .map(|b| format!(" · @{}", escape(b)))
                    .unwrap_or_default()
        ),
        None => "まだ同期していません".into(),
    };
    let note = match note {
        Some(n) => format!(
            r#"<p class="{cls}">{text}{link}</p>"#,
            cls = if n.error { "form-error" } else { "muted small" },
            text = escape(&n.text),
            link = n
                .link
                .as_ref()
                .map(|(u, l)| format!(
                    r#" <a href="{}" target="_blank" rel="noopener">{}</a>"#,
                    escape(u),
                    escape(l)
                ))
                .unwrap_or_default()
        ),
        None => match status {
            Some(st) => format!(r#"<p class="muted small">{}</p>"#, escape(&status_text(st))),
            None => String::new(),
        },
    };
    let disabled = if busy || !link.writable {
        "disabled"
    } else {
        ""
    };
    let hint = if link.writable {
        String::new()
    } else {
        r#"<p class="muted small">書き込みに使うトークンがありません。ボードのオーナーが共有パネルの「GitHub で管理」で設定してください。</p>"#.to_string()
    };
    format!(
        r#"<section class="github-sync" aria-label="GitHub">
<div class="row"><strong class="grow">GitHub</strong><a class="small" href="{url}" target="_blank" rel="noopener" translate="no">{repo}</a></div>
<p class="muted small"><span translate="no">{branch} · {path}</span><br />{last}</p>
<form class="stack" data-form="github-sync">
  <input name="message" maxlength="500" autocomplete="off" placeholder="コミットメッセージ(省略可)" aria-label="コミットメッセージ" />
  <div class="row wrap"><button type="submit" class="primary" {disabled}>同期</button><button type="button" data-action="github-pr" {disabled}>プルリクエストにする</button><button type="button" class="quiet" data-action="github-status" {busy}>変更を確かめる</button></div>
</form>
{hint}{note}
<details class="small" {open}><summary>上書きする</summary><div class="row wrap"><button type="button" class="quiet" data-action="github-pull" {busy}>リポジトリの内容をボードに読み込む</button><button type="button" class="quiet" data-action="github-push" {disabled}>ボードの内容をリポジトリに書く</button></div>{confirm}</details>
</section>"#,
        open = if confirm.is_some() { "open" } else { "" },
        confirm = match confirm {
            Some(mode) => format!(
                r#"<p class="small">{}</p><div class="row"><button type="button" class="primary" data-action="github-confirm">上書きする</button><button type="button" class="quiet" data-action="github-confirm-cancel">やめる</button></div>"#,
                if mode == GithubSyncMode::Pull {
                    "リポジトリのファイルの内容でボードを上書きします。今の状態はバージョン履歴に残ります。"
                } else {
                    "ボードの今の状態でリポジトリのファイルを上書きします。GitHub 側の変更は取り込みません。"
                }
            ),
            None => String::new(),
        },
        url = escape(&link.url),
        repo = escape(&link.repo),
        branch = escape(&link.branch),
        path = escape(&link.path),
        busy = if busy { "disabled" } else { "" },
    )
}

fn action_of(e: &web_sys::Event) -> Option<(String, Element)> {
    let el = e
        .target()?
        .dyn_into::<Element>()
        .ok()?
        .closest("[data-action]")
        .ok()??;
    Some((el.get_attribute("data-action")?, el))
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let (toggle, panel, banner) = {
        let a = app.borrow();
        (
            a.history.toggle.clone(),
            a.history.panel.clone(),
            a.history.banner.clone(),
        )
    };
    {
        let app = app.clone();
        listen(
            toggle.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                let mut a = app.borrow_mut();
                let open = a.history.panel.hidden();
                a.history.panel.set_hidden(!open);
                if open && (a.history.stale || a.history.list.is_empty()) {
                    a.fetch_versions();
                }
                a.render_history();
            },
        )?;
    }
    for target in [&panel, &banner] {
        let app = app.clone();
        listen(
            target.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                if let Some((action, el)) = action_of(&e) {
                    app.borrow_mut().on_history_action(&action, &el);
                }
            },
        )?;
    }
    {
        let app = app.clone();
        listen(panel.as_ref(), "submit", false, move |e: web_sys::Event| {
            e.prevent_default();
            let form = e.target().and_then(|t| t.dyn_into::<Element>().ok());
            if form
                .as_ref()
                .and_then(|f| f.get_attribute("data-form"))
                .as_deref()
                == Some("github-sync")
            {
                let mut a = app.borrow_mut();
                let message = a.github_message();
                a.github_request(Some(GithubSyncRequest {
                    mode: GithubSyncMode::Sync,
                    message,
                    pull_request: false,
                }));
                return;
            }
            let Some(input) = form
                .and_then(|f| f.query_selector("[name=label]").ok().flatten())
                .and_then(|i| i.dyn_into::<HtmlInputElement>().ok())
            else {
                return;
            };
            let label = input.value();
            input.set_value("");
            app.borrow_mut().save_version(label);
        })?;
    }
    Ok(())
}
