//! ボード一覧とログインの画面(`?room=` のないとき)。
//!
//! GPU は使わず、普通の HTML を組み立てて表示する。ボードを選ぶと `?room=…` を開き直す。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::api::{
    AuditEvent, BoardInfo, Login, Me, NewBoard, NewTeam, Role, SetBoardTeam, SetTeamMember,
    SetTeamStorage, Signup, SsoStatus, StorageLocation, Team, TeamInfo, TeamRole, TeamSso,
};
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
    /// 入っているチーム(チーム機能のないサーバーなら空)。
    teams: Vec<TeamInfo>,
    /// 一覧の絞り込み。
    filter: Filter,
    /// 選んでいるチームの詳細(メンバーの一覧)。
    team: Option<Team>,
    /// チームを作る欄を開いているか。
    creating_team: bool,
    /// チームの削除の確認中か。
    confirm_delete_team: bool,
    /// 開いている操作ログ。
    audit: Option<AuditView>,
    /// サーバーが受け付けるログイン方法。
    sso: SsoStatus,
    /// 選べるデータの保存場所(先頭はこのサーバー)。
    locations: Vec<StorageLocation>,
    /// 保存場所を移している最中か。
    moving: bool,
}

/// 操作ログの表示。
struct AuditView {
    /// チームのログならその ID。`None` は全体のログ(サーバー管理者)。
    team: Option<String>,
    events: Vec<AuditEvent>,
    /// まだ古い記録がありそうか。
    more: bool,
}

const AUDIT_PAGE: usize = 50;

impl AuditView {
    fn query(&self) -> String {
        match &self.team {
            Some(t) => format!("team={}&", http::encode(t)),
            None => String::new(),
        }
    }
}

/// ボード一覧の絞り込み。
#[derive(Clone, PartialEq, Eq)]
enum Filter {
    All,
    Personal,
    Team(String),
}

impl Filter {
    fn team(&self) -> Option<&str> {
        match self {
            Filter::Team(t) => Some(t),
            _ => None,
        }
    }

    fn matches(&self, b: &BoardInfo) -> bool {
        match self {
            Filter::All => true,
            Filter::Personal => b.team.is_none(),
            Filter::Team(t) => b.team.as_deref() == Some(t.as_str()),
        }
    }
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
    let next = safe_next(params.as_ref().and_then(|p| p.get("next")));
    // 通知(Slack など)のリンクから運用の画面を開く。
    let ops_tab = params
        .as_ref()
        .and_then(|p| p.get("ops"))
        .and_then(|t| crate::ops::Tab::parse(&t));
    // SSO から戻ってきたときの失敗の理由。
    let sso_error = params.as_ref().and_then(|p| p.get("sso_error"));
    let sso = if me.is_none() {
        http::get::<SsoStatus>("/sso").await.unwrap_or(SsoStatus {
            password_login: true,
            oidc: None,
        })
    } else {
        SsoStatus {
            password_login: true,
            oidc: None,
        }
    };

    let home: Shared = Rc::new(RefCell::new(Home {
        window: window.clone(),
        root: root.clone(),
        me,
        boards: Vec::new(),
        mode: AuthMode::Login,
        error: sso_error,
        busy: false,
        confirm_delete: None,
        next,
        teams: Vec::new(),
        filter: Filter::All,
        team: None,
        creating_team: false,
        confirm_delete_team: false,
        audit: None,
        sso,
        locations: Vec::new(),
        moving: false,
    }));
    install(&home)?;
    if home.borrow().me.is_some() {
        reload(home.clone()).await;
        if let Some(tab) = ops_tab
            && home.borrow().me.as_ref().is_some_and(|m| m.admin)
        {
            crate::ops::open(tab);
        }
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

/// 「2026/10/31 21:05:09」の形(端末の時刻帯)。
fn datetime(ms: u64) -> String {
    let d = js_sys::Date::new(&JsValue::from_f64(ms as f64));
    format!(
        "{}/{:02}/{:02} {:02}:{:02}:{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
        d.get_seconds()
    )
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
        crate::developer::mount(&self.root, self.me.is_some());
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
        let sso = match &self.sso.oidc {
            Some(name) => {
                let next = self
                    .next
                    .as_deref()
                    .map(|n| format!("?next={}", http::encode(n)))
                    .unwrap_or_default();
                format!(
                    r#"<a class="button sso" href="{base}/api/sso/oidc/start{next}"><span translate="no">{name}</span> でログイン</a>{or}"#,
                    base = escape(&http::base()),
                    next = escape(&next),
                    name = escape(name),
                    or = if self.sso.password_login {
                        r#"<p class="or muted">または</p>"#
                    } else {
                        ""
                    },
                )
            }
            None => String::new(),
        };
        let password = if self.sso.password_login {
            format!(
                r#"<form data-form="auth">
    <label>ログイン名<input name="login" autocomplete="username" required autofocus maxlength="32" placeholder="英小文字・数字・-・_" /></label>
    {name_field}
    <label>パスワード<input name="password" type="password" autocomplete="{pw}" required minlength="{min}" /></label>
    {err}
    <button type="submit" class="primary" {busy}>{submit}</button>
  </form>
  <p class="switch">{switch_lead} <button type="button" class="link" data-action="switch">{switch}</button></p>"#,
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
        } else {
            self.error_html()
        };
        format!(
            r#"<div class="home-card auth">
  <h1>グラボード</h1>
  <p class="muted">{lead}</p>
  {sso}
  {password}
  <hr />
  <button type="button" data-action="anonymous">ログインせずに新しいボードを作る</button>
  <p class="muted small">ログインせずに作ったボードは、URL を知っている人なら誰でも編集できます。</p>
</div>"#,
            lead = if signup {
                "アカウントを作ると、ボードの一覧・共有の設定・コメントが使えます。"
            } else {
                "ログインしてボードを開きましょう。"
            },
        )
    }

    fn render_list(&self, me: &Me) -> String {
        let mut rows = String::new();
        for b in self.boards.iter().filter(|b| self.filter.matches(b)) {
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
        if rows.is_empty() {
            rows =
                r#"<li class="empty muted">まだボードがありません。上の欄から作りましょう。</li>"#
                    .into();
        }
        format!(
            r#"<div class="home-card list">
  <header class="home-head">
    <h1>グラボード</h1>
    <span class="me">{name} <span class="muted">@{login}</span></span>
    {admin}
    <button type="button" class="quiet" data-action="logout">ログアウト</button>
  </header>
  {tabs}
  <form data-form="create" class="create">
    <input name="title" maxlength="80" placeholder="{placeholder}" autofocus />
    <button type="submit" class="primary" {busy}>ボードを作る</button>
  </form>
  {err}
  {team_panel}
  {audit}
  <ul class="boards">{rows}</ul>
</div>"#,
            name = escape(&me.name),
            login = escape(&me.login),
            busy = if self.busy { "disabled" } else { "" },
            err = self.error_html(),
            tabs = self.render_tabs(),
            placeholder = match self.current_team() {
                Some(t) => format!("「{}」の新しいボードの名前", escape(&t.name)),
                None => "新しいボードの名前".into(),
            },
            team_panel = self.render_team_panel(me),
            admin = if me.admin {
                r#"<button type="button" class="quiet" data-action="ops" data-id="errors">運用</button><button type="button" class="quiet" data-action="audit" data-id="">全体の操作ログ</button>"#
            } else {
                ""
            },
            audit = self.render_audit(),
        )
    }

    fn render_audit(&self) -> String {
        let Some(view) = &self.audit else {
            return String::new();
        };
        let mut rows = String::new();
        for e in &view.events {
            let what = [e.target.as_deref(), e.detail.as_deref()]
                .into_iter()
                .flatten()
                .map(escape)
                .collect::<Vec<_>>()
                .join(" · ");
            let board = e
                .board
                .as_deref()
                .map(|b| {
                    format!(
                        r#"<a href="{}">{}</a>"#,
                        escape(&board_url(b, None)),
                        escape(b)
                    )
                })
                .unwrap_or_default();
            rows.push_str(&format!(
                r#"<tr data-action-kind="{kind}"><td class="nowrap">{when}</td><td>{label}</td><td>{actor}</td><td>{board}</td><td>{what}</td><td class="muted nowrap">{ip}</td></tr>"#,
                kind = escape(e.action.split('.').next().unwrap_or("")),
                when = escape(&datetime(e.at)),
                label = escape(e.label()),
                actor = e.actor.as_deref().map(|a| format!("@{}", escape(a))).unwrap_or_default(),
                ip = escape(e.ip.as_deref().unwrap_or("")),
            ));
        }
        if rows.is_empty() {
            rows = r#"<tr><td colspan="6" class="muted">まだ記録がありません。</td></tr>"#.into();
        }
        let q = view.query();
        let title = match &view.team {
            Some(t) => match self.teams.iter().find(|x| &x.id == t) {
                Some(team) => format!("「{}」の操作ログ", escape(&team.name)),
                None => "チームの操作ログ".into(),
            },
            None => "全体の操作ログ".into(),
        };
        format!(
            r#"<section class="audit-panel">
  <header><h2>{title}</h2><span class="spacer"></span>
    <a class="button" href="{base}/api/audit/export?{q}format=csv" download>CSV</a>
    <a class="button" href="{base}/api/audit/export?{q}format=jsonl" download>JSON Lines</a>
    <button type="button" class="quiet" data-action="close-audit">閉じる</button></header>
  <div class="audit-scroll"><table class="audit"><thead><tr><th>日時</th><th>操作</th><th>実行者</th><th>ボード</th><th>対象・内容</th><th>接続元</th></tr></thead><tbody>{rows}</tbody></table></div>
  {more}
</section>"#,
            base = escape(&http::base()),
            more = if view.more {
                r#"<button type="button" class="quiet" data-action="audit-more">さらに古い記録</button>"#
            } else {
                ""
            },
        )
    }

    fn current_team(&self) -> Option<&TeamInfo> {
        let id = self.filter.team()?;
        self.teams.iter().find(|t| t.id == id)
    }

    /// 「すべて・個人・チーム…」の切り替えと、チームを作る欄。
    fn render_tabs(&self) -> String {
        let tab = |id: &str, label: &str, on: bool| {
            format!(
                r#"<button type="button" class="tab{cls}" data-action="tab" data-id="{id}" aria-pressed="{on}"{tr}>{label}</button>"#,
                cls = if on { " on" } else { "" },
                // チームの名前は訳さない。
                tr = if id.is_empty() || id == "~" {
                    ""
                } else {
                    r#" translate="no""#
                },
                id = escape(id),
                label = escape(label),
            )
        };
        let mut html = String::from(r#"<nav class="team-tabs" aria-label="チーム">"#);
        html.push_str(&tab("", "すべて", self.filter == Filter::All));
        html.push_str(&tab("~", "個人", self.filter == Filter::Personal));
        for t in &self.teams {
            html.push_str(&tab(
                &t.id,
                &t.name,
                self.filter.team() == Some(t.id.as_str()),
            ));
        }
        if self.creating_team {
            html.push_str(
                r#"<form data-form="team-create" class="team-create"><input name="name" maxlength="60" placeholder="チームの名前" autofocus /><button type="submit" class="primary">作る</button><button type="button" class="quiet" data-action="cancel-team">やめる</button></form>"#,
            );
        } else {
            html.push_str(
                r#"<button type="button" class="tab add" data-action="new-team" title="チームを作る">＋ チーム</button>"#,
            );
        }
        html.push_str("</nav>");
        html
    }

    /// 選んでいるチームのメンバーと設定。
    fn render_team_panel(&self, me: &Me) -> String {
        let Some(team) = self
            .team
            .as_ref()
            .filter(|t| self.filter.team() == Some(t.id.as_str()))
        else {
            return String::new();
        };
        let admin = team.role.is_admin();
        let owner = team.role == TeamRole::Owner;
        let mut members = String::new();
        for (login, role) in &team.members {
            // 管理者以上が関わる変更はオーナーだけ。
            let editable = login != &me.login && admin && (owner || !role.is_admin());
            let role_html = if editable {
                let opts: String = [TeamRole::Member, TeamRole::Admin, TeamRole::Owner]
                    .iter()
                    .filter(|r| owner || !r.is_admin())
                    .map(|r| {
                        format!(
                            r#"<option value="{}"{}>{}</option>"#,
                            r.as_str(),
                            if r == role { " selected" } else { "" },
                            r.label()
                        )
                    })
                    .collect();
                format!(
                    r#"<select data-change="team-role" data-id="{l}" aria-label="@{l} の役割">{opts}</select><button type="button" class="quiet" data-action="remove-member" data-id="{l}">外す</button>"#,
                    l = escape(login)
                )
            } else {
                format!(
                    r#"<span class="badge" data-role="{}">{}</span>"#,
                    role.as_str(),
                    role.label()
                )
            };
            members.push_str(&format!(
                r#"<li><span translate="no">@{}</span><span class="actions">{role_html}</span></li>"#,
                escape(login)
            ));
        }
        let add = if admin {
            r#"<form data-form="team-member" class="inline"><input name="login" maxlength="33" placeholder="追加するログイン名" /><button type="submit">メンバーに追加</button></form>"#
                .to_string()
        } else {
            String::new()
        };
        // 自分がオーナーの個人ボードをこのチームへ移す。
        let movable: String = self
            .boards
            .iter()
            .filter(|b| b.role == Role::Owner && b.team.is_none() && b.owner.is_some())
            .map(|b| {
                format!(
                    r#"<option value="{}">{}</option>"#,
                    escape(&b.id),
                    escape(&b.title)
                )
            })
            .collect();
        let mv = if movable.is_empty() {
            String::new()
        } else {
            format!(
                r#"<form data-form="move-board" class="inline"><select name="board" aria-label="移すボード">{movable}</select><button type="submit">このチームへ移す</button></form>"#
            )
        };
        // SSO 自動参加ドメイン(サーバー管理者だけが設定できる)。
        let sso = if me.admin {
            format!(
                r#"<form data-form="team-sso" class="inline"><span class="muted small nowrap">SSO 自動参加</span><input name="domains" value="{}" placeholder="SSO で自動参加させるメールのドメイン(例 example.co.jp)" aria-label="SSO 自動参加ドメイン" /><button type="submit">保存</button></form>"#,
                escape(&team.sso_domains.join(", "))
            )
        } else if team.sso_domains.is_empty() {
            String::new()
        } else {
            format!(
                r#"<p class="muted small">SSO 自動参加: <span translate="no">{}</span></p>"#,
                escape(&team.sso_domains.join(", "))
            )
        };
        // データの保存場所(用意されていれば)。
        let storage = if self.locations.len() > 1 {
            let current = team.storage.as_deref().unwrap_or("local");
            let label = self
                .locations
                .iter()
                .find(|l| l.id == current)
                .map(|l| format!("{}({})", l.label, l.description))
                .unwrap_or_else(|| current.to_string());
            if owner {
                let opts: String = self
                    .locations
                    .iter()
                    .map(|l| {
                        format!(
                            r#"<option value="{id}"{sel}>{label}({desc})</option>"#,
                            id = escape(&l.id),
                            sel = if l.id == current { " selected" } else { "" },
                            label = escape(&l.label),
                            desc = escape(&l.description),
                        )
                    })
                    .collect();
                format!(
                    r#"<form data-form="team-storage" class="inline"><span class="muted small nowrap">データの保存場所</span><select name="location" aria-label="データの保存場所" translate="no">{opts}</select><button type="submit" {busy}>{btn}</button></form>"#,
                    busy = if self.moving { "disabled" } else { "" },
                    btn = if self.moving {
                        "移しています…"
                    } else {
                        "移す"
                    },
                )
            } else {
                format!(
                    r#"<p class="muted small">データの保存場所: <span translate="no">{}</span></p>"#,
                    escape(&label)
                )
            }
        } else {
            String::new()
        };
        let danger = if owner {
            if self.confirm_delete_team {
                r#"<button type="button" class="danger" data-action="delete-team">本当にチームを削除</button><button type="button" data-action="cancel-delete-team">やめる</button>"#
            } else {
                r#"<button type="button" class="quiet" data-action="ask-delete-team">チームを削除</button>"#
            }
        } else {
            r#"<button type="button" class="quiet" data-action="leave-team">チームを抜ける</button>"#
        };
        format!(
            r#"<section class="team-panel">
  <header><h2 translate="no">{name}</h2><span class="badge" data-role="{rs}">{rl}</span><span class="muted">{n} 人</span><span class="spacer"></span>{log}{danger}</header>
  <ul class="team-members">{members}</ul>
  {add}{mv}{storage}{sso}
  <p class="muted small">チームのボードはメンバー全員が開けます(既定は編集可)。管理者はチームのボードをすべて管理できます。</p>
</section>"#,
            name = escape(&team.name),
            rs = if admin { "owner" } else { "editor" },
            rl = team.role.label(),
            n = team.members.len(),
            log = if admin {
                format!(
                    r#"<button type="button" class="quiet" data-action="audit" data-id="{}">操作ログ</button>"#,
                    escape(&team.id)
                )
            } else {
                String::new()
            },
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
    // チーム機能のない古いサーバーなら空のまま。
    let teams = http::get::<Vec<TeamInfo>>("/teams")
        .await
        .unwrap_or_default();
    let selected = home.borrow().filter.team().map(str::to_string);
    let detail = match &selected {
        Some(t) if teams.iter().any(|x| &x.id == t) => {
            http::get::<Team>(&format!("/teams/{}", http::encode(t)))
                .await
                .ok()
        }
        _ => None,
    };
    let locations = if detail.is_some() && home.borrow().locations.is_empty() {
        http::get::<Vec<StorageLocation>>("/storage")
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut h = home.borrow_mut();
    if !locations.is_empty() {
        h.locations = locations;
    }
    if selected.is_some() && detail.is_none() {
        h.filter = Filter::All;
    }
    h.teams = teams;
    h.team = detail;
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
                    let team = h.borrow().filter.team().map(str::to_string);
                    match http::send::<_, BoardInfo>("POST", "/boards", &NewBoard { title, team })
                        .await
                    {
                        Ok(b) => navigate(&h.borrow().window, &board_url(&b.id, None)),
                        Err(e) => {
                            let mut hm = h.borrow_mut();
                            hm.busy = false;
                            hm.error = Some(e.message());
                            hm.render();
                        }
                    }
                }
                "team-create" => {
                    let name = form_value(&form, "name");
                    let res = http::send::<_, Team>("POST", "/teams", &NewTeam { name }).await;
                    after_team_call(
                        &h,
                        res.map(|t| {
                            let mut hm = h.borrow_mut();
                            hm.filter = Filter::Team(t.id);
                            hm.creating_team = false;
                        }),
                    )
                    .await;
                }
                "team-member" => {
                    let login = form_value(&form, "login");
                    let res = set_team_member(&h, login, Some(TeamRole::Member)).await;
                    after_team_call(&h, res).await;
                }
                "team-sso" => {
                    let domains = form_value(&form, "domains")
                        .split([',', ' ', '、'])
                        .map(str::trim)
                        .filter(|d| !d.is_empty())
                        .map(str::to_string)
                        .collect();
                    let team = h.borrow().filter.team().map(str::to_string);
                    let res = match team {
                        Some(t) => http::send::<_, Team>(
                            "PUT",
                            &format!("/teams/{}/sso", http::encode(&t)),
                            &TeamSso { domains },
                        )
                        .await
                        .map(|_| ()),
                        None => Ok(()),
                    };
                    after_team_call(&h, res).await;
                }
                "team-storage" => {
                    let location = form
                        .query_selector("[name=location]")
                        .ok()
                        .flatten()
                        .and_then(|e| e.dyn_into::<web_sys::HtmlSelectElement>().ok())
                        .map(|s| s.value());
                    let team = h.borrow().filter.team().map(str::to_string);
                    {
                        let mut hm = h.borrow_mut();
                        hm.moving = true;
                        hm.render();
                    }
                    let res = match team {
                        Some(t) => http::send::<_, Team>(
                            "PUT",
                            &format!("/teams/{}/storage", http::encode(&t)),
                            &SetTeamStorage { location },
                        )
                        .await
                        .map(|_| ()),
                        None => Ok(()),
                    };
                    h.borrow_mut().moving = false;
                    after_team_call(&h, res).await;
                }
                "move-board" => {
                    let board = form
                        .query_selector("[name=board]")
                        .ok()
                        .flatten()
                        .and_then(|e| e.dyn_into::<web_sys::HtmlSelectElement>().ok())
                        .map(|s| s.value())
                        .unwrap_or_default();
                    let team = h.borrow().filter.team().map(str::to_string);
                    let res = http::send::<_, BoardInfo>(
                        "PUT",
                        &http::board_path(&board, None, "/team"),
                        &SetBoardTeam { team, access: None },
                    )
                    .await
                    .map(|_| ());
                    after_team_call(&h, res).await;
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
            "tab" => {
                let mut hm = h.borrow_mut();
                hm.filter = match id.as_deref() {
                    None | Some("") => Filter::All,
                    Some("~") => Filter::Personal,
                    Some(t) => Filter::Team(t.to_string()),
                };
                hm.confirm_delete_team = false;
                if hm.audit.as_ref().is_some_and(|a| a.team.is_some()) {
                    hm.audit = None;
                }
                let load = hm.filter.team().is_some();
                drop(hm);
                if load {
                    wasm_bindgen_futures::spawn_local(reload(h.clone()));
                } else {
                    h.borrow().render();
                }
            }
            "audit" => {
                let team = id.filter(|t| !t.is_empty());
                h.borrow_mut().audit = Some(AuditView {
                    team,
                    events: Vec::new(),
                    more: false,
                });
                wasm_bindgen_futures::spawn_local(load_audit(h.clone(), false));
            }
            "ops" => {
                crate::ops::open(
                    crate::ops::Tab::parse(id.as_deref().unwrap_or(""))
                        .unwrap_or(crate::ops::Tab::Errors),
                );
            }
            "audit-more" => {
                wasm_bindgen_futures::spawn_local(load_audit(h.clone(), true));
            }
            "close-audit" => {
                let mut hm = h.borrow_mut();
                hm.audit = None;
                hm.render();
            }
            "new-team" | "cancel-team" => {
                let mut hm = h.borrow_mut();
                hm.creating_team = action == "new-team";
                hm.render();
            }
            "ask-delete-team" | "cancel-delete-team" => {
                let mut hm = h.borrow_mut();
                hm.confirm_delete_team = action == "ask-delete-team";
                hm.render();
            }
            "remove-member" | "leave-team" => {
                let login = if action == "leave-team" {
                    h.borrow().me.as_ref().map(|m| m.login.clone())
                } else {
                    id
                };
                let Some(login) = login else { return };
                let h = h.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let res = set_team_member(&h, login, None).await;
                    after_team_call(&h, res).await;
                });
            }
            "delete-team" => {
                let Some(team) = h.borrow().filter.team().map(str::to_string) else {
                    return;
                };
                let h = h.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let res =
                        http::send_empty("DELETE", &format!("/teams/{}", http::encode(&team)))
                            .await;
                    {
                        let mut hm = h.borrow_mut();
                        hm.confirm_delete_team = false;
                        hm.filter = Filter::All;
                    }
                    after_team_call(&h, res).await;
                });
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

    // メンバーの役割の変更。
    let h = home.clone();
    let on_change = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        let Some(sel) = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlSelectElement>().ok())
        else {
            return;
        };
        if sel.get_attribute("data-change").as_deref() != Some("team-role") {
            return;
        }
        let (Some(login), Some(role)) =
            (sel.get_attribute("data-id"), TeamRole::parse(&sel.value()))
        else {
            return;
        };
        let h = h.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let res = set_team_member(&h, login, Some(role)).await;
            after_team_call(&h, res).await;
        });
    });
    root.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())
        .map_err(|_| "change を登録できません")?;
    on_change.forget();
    Ok(())
}

/// 操作ログを読む。`more` なら今ある分より古いものを足す。
async fn load_audit(h: Shared, more: bool) {
    let path = {
        let hm = h.borrow();
        let Some(view) = &hm.audit else { return };
        let before = if more {
            view.events
                .last()
                .map(|e| format!("&before={}", e.id))
                .unwrap_or_default()
        } else {
            String::new()
        };
        format!("/audit?{}limit={AUDIT_PAGE}{before}", view.query())
    };
    let res = http::get::<Vec<AuditEvent>>(&path).await;
    let mut hm = h.borrow_mut();
    match res {
        Ok(list) => {
            if let Some(view) = hm.audit.as_mut() {
                view.more = list.len() >= AUDIT_PAGE;
                if !more {
                    view.events.clear();
                }
                view.events.extend(list);
            }
        }
        Err(e) => {
            hm.audit = None;
            hm.error = Some(e.message());
        }
    }
    hm.render();
}

async fn set_team_member(
    h: &Shared,
    login: String,
    role: Option<TeamRole>,
) -> Result<(), HttpError> {
    let Some(team) = h.borrow().filter.team().map(str::to_string) else {
        return Ok(());
    };
    http::send::<_, Team>(
        "PUT",
        &format!("/teams/{}/members", http::encode(&team)),
        &SetTeamMember { login, role },
    )
    .await
    .map(|_| ())
}

/// チームの操作のあと: 失敗なら理由を出し、どちらでも一覧を読み直す。
async fn after_team_call(h: &Shared, res: Result<(), HttpError>) {
    {
        let mut hm = h.borrow_mut();
        hm.busy = false;
        if let Err(e) = res {
            hm.error = Some(e.message());
            hm.render();
            return;
        }
    }
    reload(h.clone()).await;
}
