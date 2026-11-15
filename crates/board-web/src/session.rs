//! ボード画面のログイン状態・権限・ボード名・共有パネル。
//!
//! 権限の判定はサーバーが行う。ここでは表示を合わせ、編集できない人には編集操作を出さない。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::api::{
    BoardInfo, GithubLink, Me, RenameBoard, Role, SetGithub, SetLink, SetMember, SetPublic,
    SetSlack, Sharing, SlackEvents, SlackSettings,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element, HtmlElement, HtmlInputElement, HtmlSelectElement, Window};

use crate::home::board_url;
use crate::http::{self, HttpError, escape};

/// ボードを開けるか。
#[derive(Clone, Debug)]
pub enum Access {
    Granted(BoardInfo),
    /// 権限がない(またはボードがない)。
    Denied,
    /// API のないサーバー・つながらない。従来どおり誰でも編集できるものとして扱う。
    Unavailable,
}

pub async fn check_access(room: &str, key: Option<&str>) -> Access {
    match http::get::<BoardInfo>(&http::board_path(room, key, "")).await {
        Ok(info) => Access::Granted(info),
        Err(HttpError::Status(403 | 404, _)) => Access::Denied,
        Err(HttpError::Status(s, m)) => {
            log::warn!("ボード情報を取得できません({s}): {m}");
            Access::Unavailable
        }
        Err(HttpError::Unavailable) => Access::Unavailable,
    }
}

pub async fn fetch_me() -> Option<Me> {
    http::get::<Me>("/me").await.ok()
}

/// 開く権限がないときの画面。
pub fn show_denied(document: &Document, logged_in: bool, revoked: bool) {
    let Some(root) = document.get_element_by_id("home") else {
        return;
    };
    if let Some(body) = document.body() {
        let _ = body.class_list().add_1("home-mode");
    }
    let next = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .unwrap_or_default();
    let login = if logged_in {
        String::new()
    } else {
        format!(
            r#"<a class="button primary" href="?next={}">ログインして開く</a>"#,
            http::encode(&next)
        )
    };
    root.set_inner_html(&format!(
        r#"<div class="home-card auth">
  <h1>{title}</h1>
  <p class="muted">{lead}</p>
  <p class="row">{login}<a class="button" href="./">ボード一覧へ</a></p>
</div>"#,
        title = if revoked {
            "このボードを開けなくなりました"
        } else {
            "このボードを開けません"
        },
        lead = if logged_in {
            "ボードが削除されたか、開く権限がありません。オーナーに招待してもらうか、共有リンクを教えてもらってください。"
        } else {
            "ボードが見つからないか、ログインが必要です。"
        },
    ));
    if let Ok(root) = root.dyn_into::<HtmlElement>() {
        root.set_hidden(false);
    }
}

/// ボード画面の上部(ボード名・アカウント・共有)。
pub struct BoardChrome {
    window: Window,
    document: Document,
    pub room: String,
    pub key: Option<String>,
    pub me: Option<Me>,
    pub info: Option<BoardInfo>,
    sharing: Option<Sharing>,
    share_error: Option<String>,
    slack: Option<SlackSettings>,
    /// Slack の設定・テスト送信の結果(成功なら `Ok`)。
    slack_status: Option<Result<String, String>>,
    /// GitHub のつながり(`None` は API のない古いサーバー、`Some(None)` はつながっていない)。
    github: Option<Option<GithubLink>>,
    github_status: Option<Result<String, String>>,
}

pub type SharedChrome = Rc<RefCell<BoardChrome>>;

impl BoardChrome {
    pub fn new(
        window: &Window,
        document: &Document,
        room: String,
        key: Option<String>,
        me: Option<Me>,
        info: Option<BoardInfo>,
    ) -> SharedChrome {
        let chrome = Rc::new(RefCell::new(Self {
            window: window.clone(),
            document: document.clone(),
            room,
            key,
            me,
            info,
            sharing: None,
            share_error: None,
            slack: None,
            slack_status: None,
            github: None,
            github_status: None,
        }));
        chrome.borrow().render_header();
        install(&chrome);
        chrome
    }

    /// 自分の権限。情報がなければ(API のないサーバー)編集できるものとする。
    pub fn role(&self) -> Role {
        self.info.as_ref().map_or(Role::Editor, |i| i.role)
    }

    pub fn set_info(&mut self, info: BoardInfo) {
        self.info = Some(info);
        self.render_header();
    }

    fn el(&self, id: &str) -> Option<Element> {
        self.document.get_element_by_id(id)
    }

    pub fn render_header(&self) {
        let role = self.role();
        if let Some(body) = self.document.body() {
            let _ = body.set_attribute("data-role", role.as_str());
        }
        if let Some(title) = self.el("board-title") {
            let text = self.info.as_ref().map(|i| i.title.as_str()).unwrap_or("");
            title.set_text_content(Some(text));
            let editable = role.can_edit() && self.info.as_ref().is_some_and(|i| i.owner.is_some());
            let _ = title.set_attribute("data-editable", if editable { "true" } else { "false" });
            if editable {
                let _ = title.set_attribute("title", "クリックして名前を変更");
            }
        }
        if let Some(badge) = self.el("board-role") {
            if role.can_edit() {
                let _ = badge.set_attribute("hidden", "");
            } else {
                let _ = badge.remove_attribute("hidden");
                badge.set_text_content(Some(role.label()));
            }
        }
        if let Some(account) = self.el("account") {
            match &self.me {
                Some(me) => {
                    account.set_inner_html(&format!(
                        r#"<a href="./" title="ボード一覧">{}</a>"#,
                        escape(&me.name)
                    ));
                }
                None if self.info.is_some() => {
                    let next = http::encode(&board_url(&self.room, self.key.as_deref()));
                    account.set_inner_html(&format!(r#"<a href="?next={next}">ログイン</a>"#));
                }
                None => account.set_inner_html(""),
            }
        }
        if let Some(share) = self.el("share") {
            share.set_text_content(Some(if self.can_manage() {
                "共有"
            } else {
                "リンクをコピー"
            }));
        }
    }

    /// 共有設定を変えられるか(オーナーのいるボードのオーナー)。
    pub fn can_manage(&self) -> bool {
        self.info
            .as_ref()
            .is_some_and(|i| i.role == Role::Owner && i.owner.is_some())
    }

    fn share_panel(&self) -> Option<HtmlElement> {
        self.el("share-panel")?.dyn_into().ok()
    }

    pub fn share_open(&self) -> bool {
        self.share_panel().is_some_and(|p| !p.hidden())
    }

    pub fn close_share(&self) {
        if let Some(p) = self.share_panel() {
            p.set_hidden(true);
        }
    }

    fn link_url(&self, key: &str) -> String {
        let loc = self.window.location();
        let base = format!(
            "{}{}",
            loc.origin().unwrap_or_default(),
            loc.pathname().unwrap_or_default()
        );
        format!("{base}{}", board_url(&self.room, Some(key)))
    }

    /// 埋め込み用の iframe のコード。
    fn embed_code(&self, key: &str) -> String {
        format!(
            r#"<iframe src="{}&embed=1" width="800" height="600" style="border:0" allow="clipboard-write; fullscreen" title="グラボード"></iframe>"#,
            self.link_url(key)
        )
    }

    /// 公開ページの URL(`/p/{board}/{slug}`)。
    fn public_url(&self, slug: &str) -> String {
        let loc = self.window.location();
        let dir = loc.pathname().unwrap_or_default();
        let dir = dir.rsplit_once('/').map_or("", |(d, _)| d);
        format!(
            "{}{dir}/p/{}/{}",
            loc.origin().unwrap_or_default(),
            self.room,
            slug
        )
    }

    fn render_share(&self) {
        let Some(panel) = self.share_panel() else {
            return;
        };
        let Some(sh) = &self.sharing else {
            panel.set_inner_html(r#"<p class="muted">読み込み中…</p>"#);
            return;
        };
        let options = |selected: Option<Role>, with_off: bool| {
            let mut s = String::new();
            if with_off {
                s.push_str(&format!(
                    r#"<option value="" {}>オフ(招待した人だけ)</option>"#,
                    if selected.is_none() { "selected" } else { "" }
                ));
            }
            for r in [Role::Viewer, Role::Commenter, Role::Editor] {
                s.push_str(&format!(
                    r#"<option value="{}" {}>{}</option>"#,
                    r.as_str(),
                    if selected == Some(r) { "selected" } else { "" },
                    r.label()
                ));
            }
            s
        };
        let link = match &sh.link {
            Some(l) => format!(
                r#"<div class="row"><input class="grow" readonly value="{url}" data-copy /><button data-action="copy-link">コピー</button></div>
<button class="link small" data-action="regenerate">リンクを作り直す(古いリンクは使えなくなります)</button>"#,
                url = escape(&self.link_url(&l.key))
            ),
            None => String::new(),
        };
        let embed = match &sh.embed {
            Some(l) => format!(
                r#"<div class="row"><textarea class="grow embed-code" readonly rows="3">{code}</textarea></div>
<div class="row"><button data-action="copy-embed">コードをコピー</button><button class="link small" data-action="regenerate-embed">作り直す(今の埋め込みは表示されなくなります)</button></div>"#,
                code = escape(&self.embed_code(&l.key))
            ),
            None => String::new(),
        };
        let public = match &sh.public {
            Some(p) => format!(
                r#"<div class="row"><input class="grow" readonly value="{url}" data-copy /><button data-action="copy-public">コピー</button></div>
<div class="row"><a class="small" href="{url}" target="_blank" rel="noopener">公開ページを開く</a><span class="grow"></span><button class="link small" data-action="regenerate-public">URL を作り直す</button></div>"#,
                url = escape(&self.public_url(&p.slug))
            ),
            None => String::new(),
        };
        let mut members = String::new();
        if let Some(owner) = &sh.owner {
            members.push_str(&format!(
                r#"<li><span class="grow">@{}</span><span class="muted">オーナー</span></li>"#,
                escape(owner)
            ));
        }
        for (login, role) in &sh.members {
            members.push_str(&format!(
                r#"<li><span class="grow">@{l}</span><select data-member="{l}">{opts}</select><button class="quiet" data-action="remove-member" data-login="{l}" title="外す">×</button></li>"#,
                l = escape(login),
                opts = options(Some(*role), false)
            ));
        }
        let err = self
            .share_error
            .as_ref()
            .map(|e| format!(r#"<p class="form-error">{}</p>"#, escape(e)))
            .unwrap_or_default();
        panel.set_inner_html(&format!(
            r#"<header class="row"><strong class="grow">共有</strong><button class="quiet" data-action="close-share" aria-label="閉じる">×</button></header>
<section>
  <label class="field">リンクを知っている人<select data-link-role>{link_opts}</select></label>
  {link}
</section>
<section class="public-share">
  <label class="row switch"><strong class="grow">Web に公開{public_hint}</strong><input type="checkbox" data-public {public_checked} /></label>
  {public}
</section>
<section>
  <strong>メンバー</strong>
  <ul class="members">{members}</ul>
  <form class="row" data-form="invite">
    <input class="grow" name="login" placeholder="ログイン名で招待" autocomplete="off" />
    <select name="role">{invite_opts}</select>
    <button type="submit">招待</button>
  </form>
  {err}
</section>
<section>
  <label class="field">他のサイトに埋め込む(iframe)<select data-embed-role>{embed_opts}</select></label>
  {embed}
</section>
<section data-slot="slack"></section>
<section data-slot="github"></section>"#,
            embed_opts = options(sh.embed.as_ref().map(|l| l.role), true)
                .replace("オフ(招待した人だけ)", "オフ"),
            link_opts = options(sh.link.as_ref().map(|l| l.role), true),
            invite_opts = options(Some(Role::Editor), false),
            public_checked = if sh.public.is_some() { "checked" } else { "" },
            public_hint = crate::hint::html(
                "オンにすると、ログインしていない人も見られる閲覧専用のページができます(編集・コメントはできません)。SNS やチャットに貼るとプレビュー画像が出ます。"
            ),
        ));
        if let Ok(Some(section)) = panel.query_selector("[data-slot=slack]") {
            section.set_inner_html(&self.slack_html());
        }
        if let Ok(Some(section)) = panel.query_selector("[data-slot=github]") {
            section.set_inner_html(&self.github_html());
        }
    }

    /// GitHub で管理する設定欄。
    fn github_html(&self) -> String {
        let Some(link) = &self.github else {
            return String::new();
        };
        let l = link.as_ref();
        let value = |v: Option<&str>| escape(v.unwrap_or(""));
        let status = match &self.github_status {
            Some(Ok(m)) => format!(r#"<p class="muted small">{}</p>"#, escape(m)),
            Some(Err(m)) => format!(r#"<p class="form-error">{}</p>"#, escape(m)),
            None => String::new(),
        };
        let base = http::base();
        let origin = if base.is_empty() {
            self.window.location().origin().unwrap_or_default()
        } else {
            base
        };
        let hook_url = format!("{origin}/api/github/webhook");
        let off = if l.is_some() {
            r#"<button type="button" class="quiet" data-action="github-off">外す</button>"#
        } else {
            ""
        };
        format!(
            r#"<strong>GitHub で管理</strong>
<p class="muted small">ボードを差分の読みやすいテキスト(1 要素 1 行の JSON)としてリポジトリに置き、コミット・プルリクエストで管理します。同期は「履歴」から編集者ができます。</p>
<form data-form="github" class="stack">
  <input name="repo" autocomplete="off" required placeholder="owner/name か https://github.com/owner/name" aria-label="リポジトリ" value="{repo}" />
  <div class="row"><input class="grow" name="branch" autocomplete="off" placeholder="ブランチ(省略時は既定)" aria-label="ブランチ" value="{branch}" /><input class="grow" name="path" autocomplete="off" placeholder="boards/{room}.graboard.json" aria-label="ファイルの場所" value="{path}" /></div>
  <input name="token" type="password" autocomplete="off" placeholder="{token_ph}" aria-label="GitHub のトークン" />
  <label class="check"><input type="checkbox" name="auto_pull" {auto} /> GitHub への push を自動で取り込む</label>
  <p class="muted small">トークンは fine-grained の個人アクセストークンで、このリポジトリの Contents と Pull requests を読み書きできるものにしてください(保存後は表示しません)。自動で取り込むには、リポジトリの Settings → Webhooks に <code translate="no">{hook}</code>(Content type は application/json、Secret はサーバー管理者に確認)を登録します。</p>
  <div class="row"><button type="submit">保存</button>{off}</div>
</form>
{status}"#,
            repo = value(l.map(|l| l.repo.as_str())),
            branch = value(l.map(|l| l.branch.as_str())),
            path = value(l.map(|l| l.path.as_str())),
            room = escape(&self.room),
            token_ph = if l.is_some_and(|l| l.has_token) {
                "保存済み。変えるときだけ入力"
            } else if l.is_some_and(|l| l.writable) {
                "サーバーのトークンを使います(ボード専用にするなら入力)"
            } else {
                "トークン(github_pat_…)"
            },
            auto = if l.is_some_and(|l| l.auto_pull) {
                "checked"
            } else {
                ""
            },
            hook = escape(&hook_url),
        )
    }

    /// Slack 通知の設定欄。
    fn slack_html(&self) -> String {
        let Some(sl) = &self.slack else {
            return String::new();
        };
        let check = |name: &str, label: &str, on: bool| {
            format!(
                r#"<label class="check"><input type="checkbox" name="{name}" {} /> {label}</label>"#,
                if on { "checked" } else { "" }
            )
        };
        let (placeholder, buttons) = if sl.enabled {
            (
                format!(
                    "設定済み({})。変えるときだけ入力",
                    escape(sl.hint.as_deref().unwrap_or(""))
                ),
                r#"<button type="submit">保存</button><button type="button" data-action="slack-test">テスト送信</button><button type="button" class="quiet" data-action="slack-off">やめる</button>"#,
            )
        } else {
            (
                "https://hooks.slack.com/services/…".to_string(),
                r#"<button type="submit">保存</button>"#,
            )
        };
        let status = match &self.slack_status {
            Some(Ok(m)) => format!(r#"<p class="muted small">{}</p>"#, escape(m)),
            Some(Err(m)) => format!(r#"<p class="form-error">{}</p>"#, escape(m)),
            None => String::new(),
        };
        format!(
            r#"<strong>Slack に通知</strong>
<form data-form="slack" class="stack">
  <input name="webhook" type="url" autocomplete="off" placeholder="{placeholder}" aria-label="Slack の Incoming Webhook URL" />
  <div class="checks">{c}{m}{v}</div>
  <div class="row">{buttons}</div>
</form>
{status}"#,
            c = check("comments", "すべてのコメント", sl.events.comments),
            m = check("mentions", "メンション", sl.events.mentions),
            v = check("versions", "バージョンの保存", sl.events.versions),
        )
    }
}

async fn load_sharing(chrome: SharedChrome) {
    let room = chrome.borrow().room.clone();
    let res = http::get::<Sharing>(&http::board_path(&room, None, "/sharing")).await;
    // Slack の設定は、API のない古いサーバーでは取れないので、取れたときだけ出す。
    let slack = http::get::<SlackSettings>(&http::board_path(&room, None, "/slack"))
        .await
        .ok();
    let github = http::get::<Option<GithubLink>>(&http::board_path(&room, None, "/github"))
        .await
        .ok();
    let mut c = chrome.borrow_mut();
    c.slack = slack;
    c.github = github;
    match res {
        Ok(sh) => {
            c.sharing = Some(sh);
            c.share_error = None;
        }
        Err(e) => c.share_error = Some(e.message()),
    }
    c.render_share();
}

/// 共有設定を変える操作を送り、結果で表示を更新する。
fn update_sharing(chrome: &SharedChrome, req: SharingRequest) {
    let chrome = chrome.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let room = chrome.borrow().room.clone();
        let res = match req {
            SharingRequest::Link(body) => {
                http::send::<_, Sharing>("PUT", &http::board_path(&room, None, "/link"), &body)
                    .await
            }
            SharingRequest::Embed(body) => {
                http::send::<_, Sharing>("PUT", &http::board_path(&room, None, "/embed"), &body)
                    .await
            }
            SharingRequest::Public(body) => {
                http::send::<_, Sharing>("PUT", &http::board_path(&room, None, "/public"), &body)
                    .await
            }
            SharingRequest::Member(body) => {
                http::send::<_, Sharing>("PUT", &http::board_path(&room, None, "/members"), &body)
                    .await
            }
        };
        let mut c = chrome.borrow_mut();
        match res {
            Ok(sh) => {
                c.sharing = Some(sh);
                c.share_error = None;
            }
            Err(e) => c.share_error = Some(e.message()),
        }
        c.render_share();
    });
}

enum SharingRequest {
    Link(SetLink),
    Embed(SetLink),
    Public(SetPublic),
    Member(SetMember),
}

/// Slack 通知の設定を変える(`None` ならテスト送信)。
fn update_slack(chrome: &SharedChrome, req: Option<SetSlack>) {
    let chrome = chrome.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let room = chrome.borrow().room.clone();
        let status = match req {
            Some(body) => {
                let off = body.webhook.as_deref() == Some("");
                match http::send::<_, SlackSettings>(
                    "PUT",
                    &http::board_path(&room, None, "/slack"),
                    &body,
                )
                .await
                {
                    Ok(sl) => {
                        chrome.borrow_mut().slack = Some(sl);
                        Ok(if off {
                            "Slack への通知をやめました"
                        } else {
                            "保存しました"
                        }
                        .to_string())
                    }
                    Err(e) => Err(e.message()),
                }
            }
            None => http::send_empty("POST", &http::board_path(&room, None, "/slack/test"))
                .await
                .map(|_| "テストのメッセージを送りました".to_string())
                .map_err(|e| e.message()),
        };
        let mut c = chrome.borrow_mut();
        c.slack_status = Some(status);
        c.render_share();
    });
}

/// GitHub のつながりを変える(`None` なら外す)。
fn update_github(chrome: &SharedChrome, req: Option<SetGithub>) {
    let chrome = chrome.clone();
    {
        let mut c = chrome.borrow_mut();
        c.github_status = Some(Ok("リポジトリを確かめています…".into()));
        c.render_share();
    }
    wasm_bindgen_futures::spawn_local(async move {
        let room = chrome.borrow().room.clone();
        let path = http::board_path(&room, None, "/github");
        let status = match req {
            Some(body) => match http::send::<_, GithubLink>("PUT", &path, &body).await {
                Ok(link) => {
                    let msg = format!("{} の {} につなぎました", link.repo, link.branch);
                    chrome.borrow_mut().github = Some(Some(link));
                    Ok(msg)
                }
                Err(e) => Err(e.message()),
            },
            None => match http::send_empty("DELETE", &path).await {
                Ok(()) => {
                    chrome.borrow_mut().github = Some(None);
                    Ok("GitHub から外しました(リポジトリのファイルはそのままです)".into())
                }
                Err(e) => Err(e.message()),
            },
        };
        let mut c = chrome.borrow_mut();
        c.github_status = Some(status);
        c.render_share();
    });
}

fn github_form(form: &Element) -> SetGithub {
    let input = |name: &str| {
        form.query_selector(&format!("[name={name}]"))
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
    };
    let text = |name: &str| {
        input(name)
            .map(|i| i.value().trim().to_string())
            .filter(|v| !v.is_empty())
    };
    SetGithub {
        repo: text("repo").unwrap_or_default(),
        branch: text("branch"),
        path: text("path"),
        token: text("token"),
        auto_pull: input("auto_pull").is_some_and(|i| i.checked()),
    }
}

fn slack_form(form: &Element) -> SetSlack {
    let input = |name: &str| {
        form.query_selector(&format!("[name={name}]"))
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
    };
    let checked = |name: &str| input(name).is_some_and(|i| i.checked());
    let webhook = input("webhook")
        .map(|i| i.value().trim().to_string())
        .filter(|v| !v.is_empty());
    SetSlack {
        webhook,
        events: SlackEvents {
            comments: checked("comments"),
            mentions: checked("mentions"),
            versions: checked("versions"),
        },
    }
}

impl BoardChrome {
    /// 「共有」ボタン。オーナーなら設定パネル、それ以外は今の URL をコピー。
    pub fn on_share_click(chrome: &SharedChrome) -> bool {
        if !chrome.borrow().can_manage() {
            return false;
        }
        let c = chrome.borrow();
        let Some(panel) = c.share_panel() else {
            return false;
        };
        if !panel.hidden() {
            panel.set_hidden(true);
            return true;
        }
        panel.set_hidden(false);
        c.render_share();
        drop(c);
        wasm_bindgen_futures::spawn_local(load_sharing(chrome.clone()));
        true
    }
}

fn copy_text(window: &Window, text: &str) {
    let _ = window.navigator().clipboard().write_text(text);
}

fn begin_rename(chrome: &SharedChrome) {
    let c = chrome.borrow();
    let Some(title) = c.el("board-title") else {
        return;
    };
    if title.get_attribute("data-editable").as_deref() != Some("true") {
        return;
    }
    let Ok(input) = c
        .document
        .create_element("input")
        .map(|e| e.unchecked_into::<HtmlInputElement>())
    else {
        return;
    };
    input.set_id("board-title-input");
    input.set_value(c.info.as_ref().map(|i| i.title.as_str()).unwrap_or(""));
    input.set_max_length(80);
    let _ = title.replace_with_with_node_1(&input);
    let _ = input.focus();
    input.select();
    drop(c);

    let done = Rc::new(RefCell::new(false));
    let finish = {
        let chrome = chrome.clone();
        let input = input.clone();
        let title = title.clone();
        move |commit: bool| {
            if std::mem::replace(&mut *done.borrow_mut(), true) {
                return;
            }
            let _ = input.replace_with_with_node_1(&title);
            let value = input.value();
            if !commit || value.trim().is_empty() {
                return;
            }
            title.set_text_content(Some(value.trim()));
            let chrome = chrome.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let (room, key) = {
                    let c = chrome.borrow();
                    (c.room.clone(), c.key.clone())
                };
                match http::send::<_, BoardInfo>(
                    "PATCH",
                    &http::board_path(&room, key.as_deref(), ""),
                    &RenameBoard { title: value },
                )
                .await
                {
                    Ok(info) => chrome.borrow_mut().set_info(info),
                    Err(e) => {
                        log::warn!("名前を変えられません: {}", e.message());
                        chrome.borrow().render_header();
                    }
                }
            });
        }
    };
    let finish = Rc::new(finish);
    {
        let finish = finish.clone();
        let on_key =
            Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
                match e.key().as_str() {
                    "Enter" if !e.is_composing() => finish(true),
                    "Escape" => finish(false),
                    _ => {}
                }
            });
        let _ = input.add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref());
        on_key.forget();
    }
    {
        let on_blur = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| finish(true));
        let _ = input.add_event_listener_with_callback("blur", on_blur.as_ref().unchecked_ref());
        on_blur.forget();
    }
}

fn install(chrome: &SharedChrome) {
    let c = chrome.borrow();
    if let Some(title) = c.el("board-title") {
        let chrome = chrome.clone();
        let cb = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |_| begin_rename(&chrome));
        let _ = title.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref());
        cb.forget();
    }
    let Some(panel) = c.share_panel() else {
        return;
    };
    drop(c);

    let ch = chrome.clone();
    let on_click = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |e: web_sys::MouseEvent| {
        let Some(el) = e
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|t| t.closest("[data-action]").ok().flatten())
        else {
            return;
        };
        match el.get_attribute("data-action").as_deref() {
            Some("close-share") => ch.borrow().close_share(),
            Some("copy-link") => {
                let c = ch.borrow();
                if let Some(key) = c
                    .sharing
                    .as_ref()
                    .and_then(|s| s.link.as_ref())
                    .map(|l| l.key.clone())
                {
                    copy_text(&c.window, &c.link_url(&key));
                    el.set_text_content(Some("コピーしました"));
                }
            }
            Some("copy-embed") => {
                let c = ch.borrow();
                if let Some(key) = c
                    .sharing
                    .as_ref()
                    .and_then(|s| s.embed.as_ref())
                    .map(|l| l.key.clone())
                {
                    copy_text(&c.window, &c.embed_code(&key));
                    el.set_text_content(Some("コピーしました"));
                }
            }
            Some("regenerate-embed") => {
                let role = ch
                    .borrow()
                    .sharing
                    .as_ref()
                    .and_then(|s| s.embed.as_ref())
                    .map(|l| l.role);
                update_sharing(
                    &ch,
                    SharingRequest::Embed(SetLink {
                        role,
                        regenerate: true,
                    }),
                );
            }
            Some("regenerate") => {
                let role = ch
                    .borrow()
                    .sharing
                    .as_ref()
                    .and_then(|s| s.link.as_ref())
                    .map(|l| l.role);
                update_sharing(
                    &ch,
                    SharingRequest::Link(SetLink {
                        role,
                        regenerate: true,
                    }),
                );
            }
            Some("slack-test") => update_slack(&ch, None),
            Some("github-off") => update_github(&ch, None),
            Some("slack-off") => update_slack(
                &ch,
                Some(SetSlack {
                    webhook: Some(String::new()),
                    events: SlackEvents::default(),
                }),
            ),
            Some("copy-public") => {
                let c = ch.borrow();
                if let Some(slug) = c
                    .sharing
                    .as_ref()
                    .and_then(|s| s.public.as_ref())
                    .map(|p| p.slug.clone())
                {
                    copy_text(&c.window, &c.public_url(&slug));
                    el.set_text_content(Some("コピーしました"));
                }
            }
            Some("regenerate-public") => update_sharing(
                &ch,
                SharingRequest::Public(SetPublic {
                    enabled: true,
                    regenerate: true,
                }),
            ),
            Some("remove-member") => {
                if let Some(login) = el.get_attribute("data-login") {
                    update_sharing(&ch, SharingRequest::Member(SetMember { login, role: None }));
                }
            }
            _ => {}
        }
    });
    let _ = panel.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref());
    on_click.forget();

    let ch = chrome.clone();
    let on_change = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        if let Some(input) = e
            .target()
            .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
            .filter(|i| i.has_attribute("data-public"))
        {
            update_sharing(
                &ch,
                SharingRequest::Public(SetPublic {
                    enabled: input.checked(),
                    regenerate: false,
                }),
            );
            return;
        }
        let Some(sel) = e
            .target()
            .and_then(|t| t.dyn_into::<HtmlSelectElement>().ok())
        else {
            return;
        };
        if sel.has_attribute("data-embed-role") {
            let role = Role::parse(&sel.value());
            update_sharing(
                &ch,
                SharingRequest::Embed(SetLink {
                    role,
                    regenerate: false,
                }),
            );
        } else if sel.has_attribute("data-link-role") {
            let role = Role::parse(&sel.value());
            update_sharing(
                &ch,
                SharingRequest::Link(SetLink {
                    role,
                    regenerate: false,
                }),
            );
        } else if let Some(login) = sel.get_attribute("data-member") {
            let role = Role::parse(&sel.value());
            update_sharing(&ch, SharingRequest::Member(SetMember { login, role }));
        }
    });
    let _ = panel.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref());
    on_change.forget();

    let ch = chrome.clone();
    let on_submit = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        e.prevent_default();
        let Some(form) = e.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        if form.get_attribute("data-form").as_deref() == Some("slack") {
            update_slack(&ch, Some(slack_form(&form)));
            return;
        }
        if form.get_attribute("data-form").as_deref() == Some("github") {
            update_github(&ch, Some(github_form(&form)));
            return;
        }
        let input = form
            .query_selector("[name=login]")
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<HtmlInputElement>().ok());
        let role = form
            .query_selector("[name=role]")
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<HtmlSelectElement>().ok())
            .and_then(|s| Role::parse(&s.value()));
        let Some(input) = input else { return };
        let login = input.value().trim().to_string();
        if login.is_empty() {
            return;
        }
        update_sharing(
            &ch,
            SharingRequest::Member(SetMember {
                login,
                role: role.or(Some(Role::Editor)),
            }),
        );
    });
    let _ = panel.add_event_listener_with_callback("submit", on_submit.as_ref().unchecked_ref());
    on_submit.forget();
}
