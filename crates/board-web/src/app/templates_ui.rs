//! テンプレートの選択と進め方のガイド、自作テンプレートのギャラリー。
//!
//! 選んだテンプレートは今あるものの右に置き、そこへ表示を移す。置いたあとは
//! ファシリテーションのパネルに「進め方」を出し、手順ごとのボタンからタイマー・投票・AI を始める。
//!
//! ログイン中は「チーム」「自分」のタブに自作テンプレート([`board_core::gallery`])が並ぶ。
//! 選んだ要素(選択がなければボード全体)を名前を付けて保存し、チームに公開すると
//! メンバー全員のギャラリーに出る。ボード一覧のギャラリーから `?room=…&template=…` で
//! 開いたときは、最初の同期のあとに空のボードへそのテンプレートを置く。

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::rc::Rc;

use board_core::api::{
    EditUserTemplate, NewUserTemplate, TeamInfo, UserTemplate, UserTemplateData,
};
use board_core::shape::ShapeKind;
use board_core::templates::{self, Category, StepAction, Template};
use board_core::{Board, Element, ElementId, Endpoint, Rect, Vec2, arrange, gallery};
use wasm_bindgen::JsCast;
use web_sys::{Element as DomElement, HtmlInputElement, HtmlSelectElement, MouseEvent};

use super::ai::{self, Action};
use super::collab::now_ms;
use super::{App, by_id, listen};
use crate::http::{self, escape as esc};
use crate::i18n::tr;

/// URL の `&template=` で頼まれた、空のボードに置くテンプレート。
pub enum Pending {
    Builtin(Template),
    /// 自作テンプレートの中身を取りに行っている。
    Loading,
    User(Vec<Element>),
}

impl App {
    /// 既存の要素と重ならないよう右側に置き(空なら画面の中央)、そこへ表示を移す。
    fn place(
        &mut self,
        guide_panel: bool,
        build: impl FnOnce(&mut Board, Vec2) -> Vec<ElementId>,
    ) -> Vec<ElementId> {
        let center = match self.board.bounds() {
            Some(r) => Vec2::new(r.max.x + 200.0 + 800.0, r.center().y),
            None => self.camera.screen_to_world(self.camera.viewport * 0.5),
        };
        self.board.end_gesture();
        let ids = build(&mut self.board, center);
        self.board.end_gesture();
        if let Some(r) = self.board.bounds_of(&ids) {
            self.camera.fit(r.min, r.max, 96.0);
            if guide_panel {
                // 右側にファシリテーションのパネル(進め方)が開くので、その分あけて合わせ直す。
                let panel = 340.0 / self.camera.zoom;
                self.camera
                    .fit(r.min, Vec2::new(r.max.x + panel, r.max.y), 96.0);
            }
        }
        self.editor.set_selection(ids.clone());
        self.dirty = true;
        ids
    }

    fn insert_template(&mut self, t: Template) {
        if !self.can_edit() {
            return;
        }
        self.place(true, |b, c| templates::insert(b, t, c));
        show_guide(&self.document, t);
    }

    fn insert_user_template(&mut self, elements: Vec<Element>) {
        if !self.can_edit() {
            return;
        }
        self.place(false, |b, c| gallery::insert(b, elements, c));
    }

    /// テンプレートに保存する要素。選択がなければボード全体。グループの仲間・フレームの中身・
    /// 両端が入っているコネクタも含める。
    fn template_targets(&self) -> Vec<ElementId> {
        let sel = self.editor.selection();
        let base: Vec<ElementId> = if sel.is_empty() {
            self.board.elements().iter().map(Element::id).collect()
        } else {
            sel.to_vec()
        };
        let mut ids = arrange::expand_groups(&self.board, &base);
        let mut set: HashSet<ElementId> = ids.iter().copied().collect();
        let frames: Vec<Rect> = ids
            .iter()
            .filter_map(|id| self.board.get(*id)?.as_shape())
            .filter(|s| s.kind == ShapeKind::Frame)
            .map(|s| s.bounds())
            .collect();
        for e in self.board.elements() {
            if set.contains(&e.id()) || matches!(e, Element::Connector(_)) {
                continue;
            }
            if frames.iter().any(|f| f.contains_rect(&e.bounds())) {
                set.insert(e.id());
                ids.push(e.id());
            }
        }
        for e in self.board.elements() {
            if let Element::Connector(c) = e
                && !set.contains(&c.id)
                && [&c.from, &c.to].iter().all(|p| match p {
                    Endpoint::Attached { id, .. } => set.contains(id),
                    Endpoint::Free(_) => false,
                })
            {
                ids.push(c.id);
            }
        }
        ids
    }

    fn me_login(&self) -> Option<String> {
        self.chrome
            .as_ref()
            .and_then(|c| c.borrow().me.as_ref().map(|m| m.login.clone()))
    }

    fn board_team(&self) -> Option<String> {
        self.chrome
            .as_ref()
            .and_then(|c| c.borrow().info.as_ref().and_then(|i| i.team.clone()))
    }

    /// 最初の同期が済んでいて、頼まれたテンプレートが届いていれば置く(ボードが空か、最初の案内だけのとき)。
    pub(super) fn apply_pending_template(&mut self) {
        if self.syncs == 0 || matches!(self.pending_template, None | Some(Pending::Loading)) {
            return;
        }
        let Some(p) = self.pending_template.take() else {
            return;
        };
        forget_template_param(&self.window);
        // 新しいボードにはサーバーが最初の案内を入れるので、それだけなら片付けてから置く。
        // (ID は作った端末ごとに違うので、文字と位置で見分ける。)
        let welcome = Board::welcome();
        let only_welcome = self.board.len() == welcome.len()
            && self.board.elements().iter().all(|e| {
                matches!(e, Element::Connector(_))
                    || welcome.elements().iter().any(|w| {
                        !matches!(w, Element::Connector(_))
                            && w.text() == e.text()
                            && w.bounds() == e.bounds()
                    })
            });
        if only_welcome {
            let ids: Vec<ElementId> = self.board.elements().iter().map(Element::id).collect();
            self.board.remove(&ids);
        } else if !self.board.is_empty() {
            return;
        }
        match p {
            Pending::Builtin(t) => self.insert_template(t),
            Pending::User(elements) => self.insert_user_template(elements),
            Pending::Loading => {}
        }
    }
}

/// 読み直しで同じテンプレートをもう一度置かないよう、URL から `template` を外す。
fn forget_template_param(window: &web_sys::Window) {
    let loc = window.location();
    let Ok(search) = loc.search() else { return };
    let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search) else {
        return;
    };
    if !params.has("template") {
        return;
    }
    params.delete("template");
    let q = String::from(params.to_string());
    let url = if q.is_empty() {
        loc.pathname().unwrap_or_default()
    } else {
        format!("?{q}")
    };
    if let Ok(h) = window.history() {
        let _ = h.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url));
    }
}

/// `&template=` を読む。`kpt` などは標準のテンプレート、`u:{id}` は自作テンプレート。
pub fn start_pending(app: &Rc<RefCell<App>>, param: Option<String>) {
    let Some(param) = param else { return };
    if let Some(id) = param.strip_prefix("u:") {
        app.borrow_mut().pending_template = Some(Pending::Loading);
        let app = app.clone();
        let id = id.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let res = http::send_empty_json::<UserTemplateData>(
                "POST",
                &format!("/templates/{}/use", http::encode(&id)),
            )
            .await;
            let mut a = app.borrow_mut();
            match res.map(|d| gallery::parse(&d.data)) {
                Ok(Ok(elements)) => {
                    a.pending_template = Some(Pending::User(elements));
                    a.apply_pending_template();
                }
                Ok(Err(e)) => {
                    log::warn!("テンプレートを読めません: {e}");
                    a.pending_template = None;
                }
                Err(e) => {
                    log::warn!("テンプレートを取れません: {}", e.message());
                    a.pending_template = None;
                }
            }
        });
    } else if let Some(t) = Template::from_name(&param) {
        let mut a = app.borrow_mut();
        a.pending_template = Some(Pending::Builtin(t));
        a.apply_pending_template();
    }
}

/// 手順のボタンの文言。説明だけの手順にはボタンを付けない。
fn action_label(a: StepAction) -> Option<String> {
    Some(match a {
        StepAction::None => return None,
        StepAction::Timer(m) => format!("{m}分タイマー"),
        StepAction::Vote(n) => format!("投票開始({n}票)"),
        StepAction::CloseVote => "締め切る".into(),
        StepAction::AiCluster => "AI でまとめる".into(),
        StepAction::AiSummary => "AI で要約".into(),
    })
}

/// パネルに進め方を出して開く。
fn show_guide(doc: &web_sys::Document, t: Template) {
    let (Ok(guide), Ok(title), Ok(list)) = (
        by_id::<DomElement>(doc, "template-guide"),
        by_id::<DomElement>(doc, "template-guide-title"),
        by_id::<DomElement>(doc, "template-guide-steps"),
    ) else {
        return;
    };
    title.set_text_content(Some(&format!("進め方: {}", t.title())));
    let _ = list.set_attribute("data-template", t.name());
    let mut html = String::new();
    for (i, s) in t.steps().iter().enumerate() {
        let _ = write!(html, "<li data-index=\"{i}\"><span>{}</span>", esc(s.label));
        if let Some(label) = action_label(s.action) {
            let _ = write!(html, "<button data-step=\"{i}\">{}</button>", esc(&label));
        }
        html.push_str("</li>");
    }
    list.set_inner_html(&html);
    let _ = guide.remove_attribute("hidden");
    if let Ok(panel) = by_id::<DomElement>(doc, "collab") {
        let _ = panel.set_attribute("open", "");
    }
}

/// 手順 `i` のボタンが押された。
fn run_step(app: &Rc<RefCell<App>>, t: Template, i: usize) {
    let Some(step) = t.steps().get(i) else {
        return;
    };
    match step.action {
        StepAction::None => {}
        StepAction::Timer(m) => {
            app.borrow_mut()
                .board
                .start_timer(f64::from(m) * 60_000.0, now_ms());
        }
        StepAction::Vote(n) => {
            let mut a = app.borrow_mut();
            if let Ok(input) = by_id::<HtmlInputElement>(&a.document, "vote-limit") {
                input.set_value(&n.to_string());
            }
            a.board.start_vote(n);
            a.refresh_votes();
        }
        StepAction::CloseVote => {
            let mut a = app.borrow_mut();
            a.board.close_vote();
            a.refresh_votes();
        }
        StepAction::AiCluster => ai::start(app, Action::Cluster),
        StepAction::AiSummary => ai::start(app, Action::Summarize),
    }
}

fn picker_html() -> String {
    let mut html = String::from(
        r#"<input class="template-search" type="search" placeholder="テンプレートを検索(例: 振り返り、KPT)" aria-label="テンプレートを検索" />
<div class="template-tabs" role="tablist" aria-label="テンプレートの種類" hidden>
<button type="button" role="tab" data-tab="builtin" aria-selected="true">標準</button>
<button type="button" role="tab" data-tab="team" aria-selected="false">チーム</button>
<button type="button" role="tab" data-tab="mine" aria-selected="false">自分</button>
<button type="button" class="template-save" data-action="save-open">＋ テンプレートに保存</button>
</div>"#,
    );
    html.push_str(r#"<div class="template-list" data-pane="builtin">"#);
    for c in Category::ALL {
        let _ = write!(
            html,
            r#"<section data-category><h4>{}</h4><div class="template-grid">"#,
            esc(c.title())
        );
        for t in Template::ALL.iter().filter(|t| t.category() == c) {
            let keys = format!(
                "{} {} {} {}",
                t.name(),
                t.title(),
                t.description(),
                c.title()
            )
            .to_lowercase();
            let _ = write!(
                html,
                r#"<button data-template="{}" data-keys="{}"><strong>{}</strong><span>{}</span></button>"#,
                t.name(),
                esc(&keys),
                esc(t.title()),
                esc(t.description())
            );
        }
        html.push_str("</div></section>");
    }
    html.push_str(r#"<p class="template-empty" hidden>見つかりません</p></div>"#);
    html.push_str(r#"<div class="template-list" data-pane="user" hidden></div>"#);
    html.push_str(r#"<form class="template-form" hidden></form>"#);
    html
}

/// 検索語で絞り込む(空白区切りの語をすべて含むものを残す)。
fn filter(picker: &DomElement, query: &str) {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let Ok(panes) = picker.query_selector_all("[data-pane]") else {
        return;
    };
    for p in 0..panes.length() {
        let Some(pane) = panes.item(p).and_then(|n| n.dyn_into::<DomElement>().ok()) else {
            continue;
        };
        let Ok(sections) = pane.query_selector_all("[data-category]") else {
            continue;
        };
        if sections.length() == 0 {
            continue;
        }
        let mut any = false;
        for i in 0..sections.length() {
            let Some(sec) = sections
                .item(i)
                .and_then(|n| n.dyn_into::<DomElement>().ok())
            else {
                continue;
            };
            let mut shown = 0;
            if let Ok(items) = sec.query_selector_all("[data-keys]") {
                for j in 0..items.length() {
                    let Some(b) = items.item(j).and_then(|n| n.dyn_into::<DomElement>().ok())
                    else {
                        continue;
                    };
                    let keys = b.get_attribute("data-keys").unwrap_or_default();
                    if words.iter().all(|w| keys.contains(w.as_str())) {
                        let _ = b.remove_attribute("hidden");
                        shown += 1;
                    } else {
                        let _ = b.set_attribute("hidden", "");
                    }
                }
            }
            if shown == 0 {
                let _ = sec.set_attribute("hidden", "");
            } else {
                let _ = sec.remove_attribute("hidden");
                any = true;
            }
        }
        if let Ok(Some(empty)) = pane.query_selector(".template-empty") {
            if any {
                let _ = empty.set_attribute("hidden", "");
            } else {
                let _ = empty.remove_attribute("hidden");
            }
        }
    }
}

// ---- 自作テンプレートのギャラリー ----

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Builtin,
    Team,
    Mine,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Tab::Builtin => "builtin",
            Tab::Team => "team",
            Tab::Mine => "mine",
        }
    }

    fn from_name(s: &str) -> Option<Tab> {
        [Tab::Builtin, Tab::Team, Tab::Mine]
            .into_iter()
            .find(|t| t.name() == s)
    }
}

/// 開いているフォーム。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Form {
    /// 選んだ要素を新しく保存する。
    New,
    /// 既存のテンプレートの変更。
    Edit(String),
}

#[derive(Default)]
struct Gallery {
    tab: Tab,
    login: Option<String>,
    templates: Vec<UserTemplate>,
    teams: Vec<TeamInfo>,
    loaded: bool,
    loading: bool,
    form: Option<Form>,
    /// 新しく保存するときの要素の数と、選択から取ったか。
    target: (usize, bool),
    confirm_delete: bool,
    busy: bool,
    error: Option<String>,
    /// 保存した直後の知らせ(文言とテンプレートの名前)。
    notice: Option<(&'static str, String)>,
}

type SharedGallery = Rc<RefCell<Gallery>>;

impl Gallery {
    fn find(&self, id: &str) -> Option<&UserTemplate> {
        self.templates.iter().find(|t| t.id == id)
    }

    fn card(&self, t: &UserTemplate, show_scope: bool) -> String {
        let keys = format!(
            "{} {} {} {} {} {}",
            t.title,
            t.description,
            t.category,
            t.owner,
            t.owner_name,
            t.team_name.as_deref().unwrap_or("")
        )
        .to_lowercase();
        let mut meta = String::new();
        if !t.category.is_empty() {
            let _ = write!(
                meta,
                r#"<span class="chip" translate="no">{}</span>"#,
                esc(&t.category)
            );
        }
        if show_scope {
            match &t.team_name {
                Some(n) => {
                    let _ = write!(
                        meta,
                        r#"<span class="scope" data-scope="team" title="チームに公開中"><span translate="no">{}</span></span>"#,
                        esc(n)
                    );
                }
                None => meta.push_str(r#"<span class="scope">自分だけ</span>"#),
            }
        } else {
            let _ = write!(
                meta,
                r#"<span translate="no">{}</span>"#,
                esc(&t.owner_name)
            );
        }
        let _ = write!(meta, "<span>{} 回使用</span>", t.uses);
        let edit = if t.can_edit {
            format!(
                r#"<button type="button" class="template-edit" data-edit="{}" title="編集・公開先の変更" aria-label="編集・公開先の変更">編集</button>"#,
                esc(&t.id)
            )
        } else {
            String::new()
        };
        format!(
            r#"<div class="template-card" data-keys="{keys}"><button type="button" class="template-use" data-user-template="{id}" title="ボードに置く"><span class="template-thumb">{preview}</span><strong translate="no">{title}</strong><span class="template-desc" translate="no">{desc}</span><span class="template-meta">{meta}</span></button>{edit}</div>"#,
            keys = esc(&keys),
            id = esc(&t.id),
            // 縮小図はサーバーが数値と色だけで作ったもの。
            preview = if t.preview.starts_with("<svg") {
                t.preview.as_str()
            } else {
                ""
            },
            title = esc(&t.title),
            desc = esc(&t.description),
        )
    }

    fn section(&self, title: &str, translate: bool, cards: &str) -> String {
        format!(
            r#"<section data-category><h4{tr}>{}</h4><div class="template-grid">{cards}</div></section>"#,
            esc(title),
            tr = if translate { "" } else { r#" translate="no""# },
        )
    }

    fn pane_html(&self) -> String {
        if !self.loaded {
            return format!(
                r#"<p class="template-note">{}</p>"#,
                self.error.as_deref().map_or("読み込み中…".to_string(), esc)
            );
        }
        let me = self.login.as_deref().unwrap_or("");
        let mut html = String::new();
        if let Some((label, title)) = &self.notice {
            let _ = write!(
                html,
                r#"<p class="template-notice" role="status"><span>{}</span> <span translate="no">「{}」</span></p>"#,
                esc(label),
                esc(title)
            );
        }
        match self.tab {
            Tab::Builtin => {}
            Tab::Team => {
                if self.teams.is_empty() {
                    html.push_str(r#"<p class="template-note">チームに入ると、チームで公開されたテンプレートがここに並びます。チームはボード一覧で作れます。</p>"#);
                    return html;
                }
                let mut any = false;
                for team in &self.teams {
                    let cards: String = self
                        .templates
                        .iter()
                        .filter(|t| t.team.as_deref() == Some(team.id.as_str()))
                        .map(|t| self.card(t, false))
                        .collect();
                    if !cards.is_empty() {
                        any = true;
                        html.push_str(&self.section(&team.name, false, &cards));
                    }
                }
                if !any {
                    html.push_str(r#"<p class="template-note">まだチームに公開されたテンプレートはありません。よく使う配置を選んで「＋ テンプレートに保存」から公開できます。</p>"#);
                }
            }
            Tab::Mine => {
                let cards: String = self
                    .templates
                    .iter()
                    .filter(|t| t.owner == me)
                    .map(|t| self.card(t, true))
                    .collect();
                if cards.is_empty() {
                    html.push_str(r#"<p class="template-note">まだ自作のテンプレートはありません。よく使う配置を選んで「＋ テンプレートに保存」を押すと、ここに並びます。</p>"#);
                } else {
                    html.push_str(&self.section("自分のテンプレート", true, &cards));
                }
            }
        }
        html.push_str(r#"<p class="template-empty" hidden>見つかりません</p>"#);
        html
    }

    fn form_html(&self, board_team: Option<&str>) -> String {
        let Some(form) = &self.form else {
            return String::new();
        };
        let me = self.login.as_deref().unwrap_or("");
        let editing = match form {
            Form::Edit(id) => self.find(id),
            Form::New => None,
        };
        let (heading, summary) = match editing {
            Some(t) => (
                "テンプレートを編集".to_string(),
                format!("要素 {} 個", t.elements),
            ),
            None => {
                let (n, selected) = self.target;
                (
                    "テンプレートに保存".to_string(),
                    if selected {
                        format!("選んでいる {n} 個の要素を保存します。")
                    } else {
                        format!("何も選んでいないので、ボード全体({n} 個の要素)を保存します。")
                    },
                )
            }
        };
        let title = editing.map_or("", |t| t.title.as_str());
        let desc = editing.map_or("", |t| t.description.as_str());
        let cat = editing.map_or("", |t| t.category.as_str());
        let current_team = match editing {
            Some(t) => t.team.as_deref(),
            None => board_team.filter(|b| self.teams.iter().any(|t| t.id == *b)),
        };
        // 公開先。作った人でない管理者は、今のチームか「作った人だけ」しか選べない。
        let owner = editing.is_none_or(|t| t.owner == me);
        let mut options = format!(
            r#"<option value=""{}>{}</option>"#,
            if current_team.is_none() {
                " selected"
            } else {
                ""
            },
            if owner {
                "自分だけ"
            } else {
                "作った人だけ(公開をやめる)"
            }
        );
        for team in &self.teams {
            let selected = current_team == Some(team.id.as_str());
            if !owner && !selected {
                continue;
            }
            let _ = write!(
                options,
                r#"<option value="{}"{} translate="no">👥 {}</option>"#,
                esc(&team.id),
                if selected { " selected" } else { "" },
                esc(&team.name)
            );
        }
        // 分類の候補(標準テンプレートの分類 + 使われている分類)。
        let mut cats: Vec<String> = Category::ALL
            .iter()
            .map(|c| c.title().to_string())
            .collect();
        for t in &self.templates {
            if !t.category.is_empty() && !cats.contains(&t.category) {
                cats.push(t.category.clone());
            }
        }
        let datalist: String = cats
            .iter()
            .map(|c| format!(r#"<option value="{}"></option>"#, esc(c)))
            .collect();
        let delete = match editing {
            Some(_) if self.confirm_delete => {
                r#"<button type="button" class="danger" data-action="delete">本当に削除</button>"#
            }
            Some(_) => {
                r#"<button type="button" class="quiet" data-action="ask-delete">削除</button>"#
            }
            None => "",
        };
        let error = self
            .error
            .as_deref()
            .map(|e| format!(r#"<p class="form-error" role="alert">{}</p>"#, esc(e)))
            .unwrap_or_default();
        format!(
            r#"<h4>{heading}</h4>
<p class="template-note">{summary}</p>
<label>名前<input name="title" maxlength="{tmax}" required value="{title}" placeholder="例: 週次定例の振り返り" autocomplete="off" /></label>
<label>説明<input name="description" maxlength="{dmax}" value="{desc}" placeholder="どんな場面で使うか" autocomplete="off" /></label>
<label>分類<input name="category" maxlength="{cmax}" value="{cat}" list="template-categories" placeholder="例: 振り返り" autocomplete="off" /></label>
<datalist id="template-categories">{datalist}</datalist>
<label>公開先<select name="team">{options}</select></label>
<p class="template-note">チームに公開すると、メンバー全員の「チーム」タブに並び、誰でもボードに置けます。</p>
{error}
<div class="template-form-actions"><button type="submit" class="primary"{busy}>{submit}</button><button type="button" data-action="cancel">やめる</button>{delete}</div>"#,
            tmax = gallery::MAX_TITLE_CHARS,
            dmax = gallery::MAX_DESCRIPTION_CHARS,
            cmax = gallery::MAX_CATEGORY_CHARS,
            title = esc(title),
            desc = esc(desc),
            cat = esc(cat),
            busy = if self.busy { " disabled" } else { "" },
            submit = if editing.is_some() {
                "変更を保存"
            } else {
                "保存"
            },
        )
    }
}

/// タブ・一覧・フォームを今の状態にする。
fn render(picker: &DomElement, g: &Gallery, board_team: Option<&str>) {
    if let Ok(Some(tabs)) = picker.query_selector(".template-tabs") {
        if g.login.is_some() {
            let _ = tabs.remove_attribute("hidden");
        } else {
            let _ = tabs.set_attribute("hidden", "");
        }
        if let Ok(buttons) = tabs.query_selector_all("[data-tab]") {
            for i in 0..buttons.length() {
                if let Some(b) = buttons
                    .item(i)
                    .and_then(|n| n.dyn_into::<DomElement>().ok())
                {
                    let on = b.get_attribute("data-tab").as_deref() == Some(g.tab.name());
                    let _ = b.set_attribute("aria-selected", if on { "true" } else { "false" });
                }
            }
        }
    }
    let form_open = g.form.is_some();
    let show = |sel: &str, on: bool| {
        if let Ok(Some(el)) = picker.query_selector(sel) {
            if on {
                let _ = el.remove_attribute("hidden");
            } else {
                let _ = el.set_attribute("hidden", "");
            }
        }
    };
    show("[data-pane=builtin]", !form_open && g.tab == Tab::Builtin);
    show("[data-pane=user]", !form_open && g.tab != Tab::Builtin);
    show(".template-search", !form_open);
    show(".template-form", form_open);
    if g.tab != Tab::Builtin
        && !form_open
        && let Ok(Some(pane)) = picker.query_selector("[data-pane=user]")
    {
        pane.set_inner_html(&g.pane_html());
    }
    if let Ok(Some(form)) = picker.query_selector(".template-form") {
        form.set_inner_html(&g.form_html(board_team));
        if form_open
            && let Ok(Some(input)) = form.query_selector("[name=title]")
            && let Ok(input) = input.dyn_into::<web_sys::HtmlElement>()
        {
            let _ = input.focus();
        }
    }
    if let Ok(Some(input)) = picker.query_selector(".template-search") {
        let input: HtmlInputElement = input.unchecked_into();
        filter(picker, &input.value());
    }
}

fn rerender(app: &Rc<RefCell<App>>, picker: &DomElement, g: &SharedGallery) {
    let team = app.borrow().board_team();
    render(picker, &g.borrow(), team.as_deref());
}

/// 自作テンプレートとチームの一覧を取り直す。
fn reload(app: Rc<RefCell<App>>, picker: DomElement, g: SharedGallery) {
    if g.borrow().loading || g.borrow().login.is_none() {
        return;
    }
    g.borrow_mut().loading = true;
    wasm_bindgen_futures::spawn_local(async move {
        let templates = http::get::<Vec<UserTemplate>>("/templates").await;
        let teams = http::get::<Vec<TeamInfo>>("/teams").await;
        {
            let mut gm = g.borrow_mut();
            gm.loading = false;
            match (templates, teams) {
                (Ok(t), Ok(teams)) => {
                    gm.templates = t;
                    gm.teams = teams;
                    gm.loaded = true;
                    gm.error = None;
                }
                (Err(e), _) | (_, Err(e)) => {
                    gm.error = Some(e.message());
                }
            }
        }
        rerender(&app, &picker, &g);
    });
}

fn form_value(form: &DomElement, name: &str) -> String {
    let Ok(Some(el)) = form.query_selector(&format!("[name={name}]")) else {
        return String::new();
    };
    if let Some(s) = el.dyn_ref::<HtmlSelectElement>() {
        return s.value();
    }
    el.dyn_ref::<HtmlInputElement>()
        .map(HtmlInputElement::value)
        .unwrap_or_default()
}

/// フォームの送信(新しく保存するか、変更を保存する)。
fn submit(app: Rc<RefCell<App>>, picker: DomElement, g: SharedGallery, form: DomElement) {
    let Some(mode) = g.borrow().form.clone() else {
        return;
    };
    if g.borrow().busy {
        return;
    }
    let title = form_value(&form, "title");
    let description = form_value(&form, "description");
    let category = form_value(&form, "category");
    let team = form_value(&form, "team");
    let data = match &mode {
        Form::New => {
            let a = app.borrow();
            let ids = a.template_targets();
            match arrange::copy(&a.board, &ids) {
                Some(d) => Some(d),
                None => {
                    drop(a);
                    g.borrow_mut().error = Some(tr("保存する要素がありません"));
                    rerender(&app, &picker, &g);
                    return;
                }
            }
        }
        Form::Edit(_) => None,
    };
    g.borrow_mut().busy = true;
    wasm_bindgen_futures::spawn_local(async move {
        let res = match (&mode, data) {
            (Form::New, Some(data)) => {
                http::send::<_, UserTemplate>(
                    "POST",
                    "/templates",
                    &NewUserTemplate {
                        title,
                        description,
                        category,
                        team: Some(team).filter(|t| !t.is_empty()),
                        data,
                    },
                )
                .await
            }
            (Form::Edit(id), _) => {
                http::send::<_, UserTemplate>(
                    "PATCH",
                    &format!("/templates/{}", http::encode(id)),
                    &EditUserTemplate {
                        title: Some(title),
                        description: Some(description),
                        category: Some(category),
                        team: Some(team),
                    },
                )
                .await
            }
            (Form::New, None) => return,
        };
        {
            let mut gm = g.borrow_mut();
            gm.busy = false;
            match res {
                Ok(t) => {
                    let label = match (&mode, &t.team) {
                        (Form::New, Some(_)) => "チームに公開しました",
                        (Form::New, None) => "保存しました",
                        (Form::Edit(_), _) => "更新しました",
                    };
                    gm.notice = Some((label, t.title.clone()));
                    gm.tab = if t.team.is_some() {
                        Tab::Team
                    } else {
                        Tab::Mine
                    };
                    gm.templates.retain(|x| x.id != t.id);
                    gm.templates.insert(0, t);
                    gm.form = None;
                    gm.error = None;
                    gm.confirm_delete = false;
                }
                Err(e) => gm.error = Some(e.message()),
            }
        }
        rerender(&app, &picker, &g);
    });
}

fn delete(app: Rc<RefCell<App>>, picker: DomElement, g: SharedGallery) {
    let Some(Form::Edit(id)) = g.borrow().form.clone() else {
        return;
    };
    g.borrow_mut().busy = true;
    wasm_bindgen_futures::spawn_local(async move {
        let res = http::send_empty("DELETE", &format!("/templates/{}", http::encode(&id))).await;
        {
            let mut gm = g.borrow_mut();
            gm.busy = false;
            gm.confirm_delete = false;
            match res {
                Ok(()) => {
                    let title = gm.find(&id).map(|t| t.title.clone()).unwrap_or_default();
                    gm.templates.retain(|t| t.id != id);
                    gm.form = None;
                    gm.error = None;
                    gm.notice = Some(("削除しました", title));
                }
                Err(e) => gm.error = Some(e.message()),
            }
        }
        rerender(&app, &picker, &g);
    });
}

/// 自作テンプレートを取ってきて置く(使われた回数も増える)。
fn use_template(app: Rc<RefCell<App>>, picker: DomElement, g: SharedGallery, id: String) {
    if !app.borrow().can_edit() {
        return;
    }
    let _ = picker.remove_attribute("data-open");
    wasm_bindgen_futures::spawn_local(async move {
        let res = http::send_empty_json::<UserTemplateData>(
            "POST",
            &format!("/templates/{}/use", http::encode(&id)),
        )
        .await;
        match res
            .map_err(|e| e.message())
            .and_then(|d| gallery::parse(&d.data).map(|els| (d.template, els)))
        {
            Ok((t, elements)) => {
                app.borrow_mut().insert_user_template(elements);
                let mut gm = g.borrow_mut();
                if let Some(x) = gm.templates.iter_mut().find(|x| x.id == t.id) {
                    *x = t;
                }
            }
            Err(e) => {
                log::warn!("テンプレートを置けません: {e}");
                g.borrow_mut().error = Some(e);
                let _ = picker.set_attribute("data-open", "");
                rerender(&app, &picker, &g);
            }
        }
    });
}

fn on_picker_click(
    app: &Rc<RefCell<App>>,
    picker: &DomElement,
    g: &SharedGallery,
    target: &DomElement,
) {
    let closest = |sel: &str| target.closest(sel).ok().flatten();
    if let Some(t) = closest("[data-template]")
        .and_then(|b| b.get_attribute("data-template"))
        .and_then(|n| Template::from_name(&n))
    {
        let _ = picker.remove_attribute("data-open");
        app.borrow_mut().insert_template(t);
        return;
    }
    if let Some(tab) = closest("[data-tab]")
        .and_then(|b| b.get_attribute("data-tab"))
        .and_then(|n| Tab::from_name(&n))
    {
        {
            let mut gm = g.borrow_mut();
            gm.tab = tab;
            gm.form = None;
            gm.notice = None;
        }
        rerender(app, picker, g);
        return;
    }
    if let Some(id) =
        closest("[data-user-template]").and_then(|b| b.get_attribute("data-user-template"))
    {
        use_template(app.clone(), picker.clone(), g.clone(), id);
        return;
    }
    if let Some(id) = closest("[data-edit]").and_then(|b| b.get_attribute("data-edit")) {
        {
            let mut gm = g.borrow_mut();
            gm.form = Some(Form::Edit(id));
            gm.confirm_delete = false;
            gm.error = None;
            gm.notice = None;
        }
        rerender(app, picker, g);
        return;
    }
    let Some(action) = closest("[data-action]").and_then(|b| b.get_attribute("data-action")) else {
        return;
    };
    match action.as_str() {
        "save-open" => {
            let (n, selected) = {
                let a = app.borrow();
                (a.template_targets().len(), !a.editor.selection().is_empty())
            };
            let mut gm = g.borrow_mut();
            gm.target = (n, selected);
            gm.form = Some(Form::New);
            gm.notice = None;
            gm.error = if n == 0 {
                Some(tr(
                    "保存する要素がありません。付箋や図形を置いてから保存してください",
                ))
            } else {
                None
            };
        }
        "cancel" => {
            let mut gm = g.borrow_mut();
            gm.form = None;
            gm.error = None;
            gm.confirm_delete = false;
        }
        "ask-delete" => g.borrow_mut().confirm_delete = true,
        "delete" => {
            delete(app.clone(), picker.clone(), g.clone());
            return;
        }
        _ => return,
    }
    rerender(app, picker, g);
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let picker: DomElement = by_id(&doc, "template-picker")?;
    picker.set_inner_html(&picker_html());
    let gallery: SharedGallery = Rc::new(RefCell::new(Gallery {
        login: app.borrow().me_login(),
        ..Gallery::default()
    }));
    rerender(app, &picker, &gallery);
    {
        let app = app.clone();
        let picker2 = picker.clone();
        let g = gallery.clone();
        listen(picker.as_ref(), "click", false, move |e: MouseEvent| {
            let Some(target) = e.target().and_then(|t| t.dyn_into::<DomElement>().ok()) else {
                return;
            };
            on_picker_click(&app, &picker2, &g, &target);
        })?;
    }
    {
        let app = app.clone();
        let picker2 = picker.clone();
        let g = gallery.clone();
        listen(
            picker.as_ref(),
            "submit",
            false,
            move |e: web_sys::Event| {
                e.prevent_default();
                let Some(form) = e.target().and_then(|t| t.dyn_into::<DomElement>().ok()) else {
                    return;
                };
                submit(app.clone(), picker2.clone(), g.clone(), form);
            },
        )?;
    }
    if let Ok(Some(input)) = picker.query_selector(".template-search") {
        let picker2 = picker.clone();
        let input2: HtmlInputElement = input.clone().unchecked_into();
        listen(input.as_ref(), "input", false, move |_: web_sys::Event| {
            filter(&picker2, &input2.value());
        })?;
    }
    // 文字入力中のキーをボードのショートカットに渡さない。
    listen(
        picker.as_ref(),
        "keydown",
        false,
        move |e: web_sys::KeyboardEvent| {
            let typing = e
                .target()
                .and_then(|t| t.dyn_into::<DomElement>().ok())
                .is_some_and(|t| matches!(t.tag_name().as_str(), "INPUT" | "SELECT" | "TEXTAREA"));
            if typing {
                e.stop_propagation();
            }
        },
    )?;
    {
        // 開くたびに一覧を取り直す(ほかの人が公開したものも出るように)。
        let button: DomElement = by_id(&doc, "templates-button")?;
        let app2 = app.clone();
        let picker2 = picker.clone();
        let g = gallery.clone();
        listen(button.as_ref(), "click", false, move |_: MouseEvent| {
            if picker2.has_attribute("data-open") {
                return;
            }
            {
                let mut gm = g.borrow_mut();
                gm.notice = None;
                gm.error = None;
                gm.form = None;
            }
            rerender(&app2, &picker2, &g);
            reload(app2.clone(), picker2.clone(), g.clone());
        })?;
    }
    {
        let list: DomElement = by_id(&doc, "template-guide-steps")?;
        let list2 = list.clone();
        let app = app.clone();
        listen(list.as_ref(), "click", false, move |e: MouseEvent| {
            let Some(btn) = e
                .target()
                .and_then(|t| t.dyn_into::<DomElement>().ok())
                .and_then(|t| t.closest("[data-step]").ok().flatten())
            else {
                return;
            };
            let Some(i) = btn
                .get_attribute("data-step")
                .and_then(|s| s.parse::<usize>().ok())
            else {
                return;
            };
            let Some(t) = list2
                .get_attribute("data-template")
                .and_then(|n| Template::from_name(&n))
            else {
                return;
            };
            // 押した手順に印を付ける(どこまで進んだかわかるように)。
            if let Ok(Some(li)) = btn.closest("li") {
                let _ = li.set_attribute("data-done", "");
            }
            let app = app.clone();
            // ほかのハンドラが App を借りていても困らないよう、借用が終わってから動かす。
            wasm_bindgen_futures::spawn_local(async move { run_step(&app, t, i) });
        })?;
    }
    {
        let close: DomElement = by_id(&doc, "template-guide-close")?;
        let guide: DomElement = by_id(&doc, "template-guide")?;
        listen(close.as_ref(), "click", false, move |_: MouseEvent| {
            let _ = guide.set_attribute("hidden", "");
        })?;
    }
    super::toolbar::popover(app, "templates-button", picker)
}
