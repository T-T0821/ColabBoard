//! 会議と予定: Zoom・Google Meet・Teams の会議とカレンダーの予定をボードに紐づける。
//!
//! - 上部の「会議」ボタンでパネルを開く。開始 15 分前から終わりまでは「会議に参加」と目立たせる。
//! - パネルから会議に参加・Google カレンダーに追加・`.ics` の保存(Outlook・Apple カレンダー・Teams)・
//!   会議のチャットに貼る招待文のコピーができる。参加者はその招待文やカレンダーの予定からボードを開く。
//! - 編集者は会議の URL・予定の名前・時刻・カレンダーの予定のリンクを設定できる。
//! - 誰かが変えるとサーバーから `Refresh(TOPIC_MEETING)` が届くので取り直す。
//!
//! URL の検査・カレンダーのリンク・`.ics`・招待文は [`board_core::meeting`] にある。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::meeting::{self, Meeting, MeetingProvider, SetMeeting, Timing};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element, HtmlElement, HtmlInputElement};

use super::App;
use crate::home::board_url;
use crate::http::{self, escape};
use crate::session::SharedChrome;

struct Ui {
    chrome: SharedChrome,
    toggle: HtmlElement,
    panel: HtmlElement,
    meeting: Meeting,
    /// サーバーから取れた(API のない古いサーバーでは出さない)。
    loaded: bool,
    editing: bool,
    busy: bool,
    status: Option<Result<String, String>>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.borrow_mut().as_mut().map(f))
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

fn local_date(ms: u64) -> js_sys::Date {
    js_sys::Date::new(&JsValue::from_f64(ms as f64))
}

/// 「11/3 10:00」。
fn when_short(ms: u64) -> String {
    let d = local_date(ms);
    format!(
        "{}/{} {:02}:{:02}",
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes()
    )
}

/// 「11/3 10:00〜11:00」(日をまたぐときは終わりにも日付)。
fn when_range(m: &Meeting) -> Option<String> {
    let start = m.start?;
    let end = m.end_or_default()?;
    let (s, e) = (local_date(start), local_date(end));
    let same_day = s.get_full_year() == e.get_full_year()
        && s.get_month() == e.get_month()
        && s.get_date() == e.get_date();
    let end_text = if same_day {
        format!("{:02}:{:02}", e.get_hours(), e.get_minutes())
    } else {
        when_short(end)
    };
    Some(format!("{}〜{end_text}", when_short(start)))
}

/// `<input type="datetime-local">` の値(ローカル時刻)。
fn to_input(ms: Option<u64>) -> String {
    let Some(ms) = ms else {
        return String::new();
    };
    let d = local_date(ms);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes()
    )
}

/// `datetime-local` の値を UNIX ミリ秒に(オフセットのない形はローカル時刻として読まれる)。
fn from_input(v: &str) -> Option<u64> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    let t = js_sys::Date::new(&JsValue::from_str(v)).get_time();
    (t.is_finite() && t > 0.0).then_some(t as u64)
}

impl Ui {
    fn can_edit(&self) -> bool {
        let c = self.chrome.borrow();
        c.role().can_edit() && c.info.as_ref().is_some_and(|i| i.owner.is_some())
    }

    fn board_title(&self) -> String {
        self.chrome
            .borrow()
            .info
            .as_ref()
            .map(|i| i.title.clone())
            .unwrap_or_default()
    }

    /// 参加者に配るボードの URL。共有リンクで開いているなら、その鍵も付ける。
    fn board_link(&self) -> String {
        let c = self.chrome.borrow();
        let loc = web_sys::window().map(|w| w.location());
        let (origin, path) = loc
            .map(|l| {
                (
                    l.origin().unwrap_or_default(),
                    l.pathname().unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        format!("{origin}{path}{}", board_url(&c.room, c.key.as_deref()))
    }

    fn path(&self) -> String {
        let c = self.chrome.borrow();
        http::board_path(&c.room, c.key.as_deref(), "/meeting")
    }

    fn has_meeting(&self) -> bool {
        self.meeting.url.is_some() || self.meeting.start.is_some() || self.meeting.title.is_some()
    }

    fn render_toggle(&self) {
        // オーナーのいないボードでは紐づけられないので、紐づいていなければ出さない。
        let visible = self.loaded && (self.has_meeting() || self.can_edit());
        self.toggle.set_hidden(!visible);
        let timing = self.meeting.timing(now_ms());
        let join = self.meeting.url.is_some() && matches!(timing, Timing::Soon | Timing::Live);
        self.toggle
            .set_text_content(Some(if join { "会議に参加" } else { "会議" }));
        let _ = self.toggle.set_attribute(
            "data-state",
            match timing {
                Timing::Live => "live",
                Timing::Soon => "soon",
                _ => "",
            },
        );
        let _ = self.toggle.set_attribute(
            "aria-pressed",
            if self.panel.hidden() { "false" } else { "true" },
        );
        let tip = match (self.meeting.start, timing) {
            (Some(s), Timing::Later | Timing::Soon) => format!("次の会議: {}", when_short(s)),
            (_, Timing::Live) => "会議の時間です".to_string(),
            _ => "Zoom・Meet・Teams の会議とカレンダーの予定".to_string(),
        };
        self.toggle.set_title(&tip);
    }

    fn render_panel(&self) {
        if self.panel.hidden() {
            return;
        }
        let status = match &self.status {
            Some(Ok(m)) => format!(r#"<p class="muted small">{}</p>"#, escape(m)),
            Some(Err(m)) => format!(r#"<p class="form-error">{}</p>"#, escape(m)),
            None => String::new(),
        };
        let header = r#"<header class="row"><strong class="grow">会議と予定</strong><button class="quiet" data-action="meeting-close" aria-label="閉じる">×</button></header>"#;
        let body = if self.editing || (!self.has_meeting() && self.can_edit()) {
            self.form_html()
        } else if !self.has_meeting() {
            r#"<p class="muted">まだ会議は紐づけられていません。編集できる人が Zoom・Meet・Teams の会議やカレンダーの予定を紐づけられます。</p>"#.to_string()
        } else {
            self.view_html()
        };
        self.panel
            .set_inner_html(&format!("{header}{body}{status}"));
    }

    fn view_html(&self) -> String {
        let m = &self.meeting;
        let title = self.board_title();
        let link = self.board_link();
        let timing = m.timing(now_ms());
        let badge = match timing {
            Timing::Live => r#"<span class="meeting-badge live">開催中</span>"#,
            Timing::Soon => r#"<span class="meeting-badge soon">まもなく開始</span>"#,
            Timing::Ended => r#"<span class="meeting-badge">終わりました</span>"#,
            _ => "",
        };
        let when = when_range(m)
            .map(|w| format!(r#"<span class="muted">{}</span>"#, escape(&w)))
            .unwrap_or_else(|| r#"<span class="muted">時刻は未定</span>"#.to_string());
        let provider = m
            .provider
            .map(|p| {
                format!(
                    r#"<span class="meeting-provider" data-provider="{}">{}</span>"#,
                    provider_key(p),
                    p.label()
                )
            })
            .unwrap_or_default();
        let join = match &m.url {
            Some(url) => format!(
                r#"<a class="button primary meeting-join" href="{}" target="_blank" rel="noopener">{} に参加 ↗</a>"#,
                escape(url),
                m.provider.map_or("会議", MeetingProvider::label)
            ),
            None => String::new(),
        };
        let gcal = meeting::google_calendar_url(m, &title, &link);
        let ics = if m.start.is_some() {
            r#"<button type="button" data-action="meeting-ics">.ics を保存(Outlook・Teams)</button>"#
        } else {
            ""
        };
        let open_cal = match &m.calendar_url {
            Some(u) => format!(
                r#"<a class="small" href="{}" target="_blank" rel="noopener">カレンダーの予定を開く ↗</a>"#,
                escape(u)
            ),
            None => String::new(),
        };
        let invite = meeting::invite_text(m, &title, &link, when_range(m).as_deref());
        let edit = if self.can_edit() {
            r#"<div class="row"><button type="button" data-action="meeting-edit">編集</button><span class="grow"></span><button type="button" class="link small" data-action="meeting-remove">紐づけを外す</button></div>"#
        } else {
            ""
        };
        let who = m
            .updated_by
            .as_deref()
            .map(|w| {
                format!(
                    r#"<p class="muted small">@<span translate="no">{}</span> が設定</p>"#,
                    escape(w)
                )
            })
            .unwrap_or_default();
        format!(
            r#"<section class="meeting-summary">
  <div class="row">{provider}{badge}</div>
  <strong class="meeting-title" translate="no">{t}</strong>
  {when}
  {join}
</section>
<section>
  <strong>カレンダー</strong>
  <div class="row wrap"><a class="button" href="{gcal}" target="_blank" rel="noopener">Google カレンダーに追加 ↗</a>{ics}</div>
  <p class="muted small">予定の説明にボードの URL が入るので、カレンダーからボードを開けます。</p>
  {open_cal}
</section>
<section>
  <strong>会議の参加者に知らせる</strong>
  <textarea class="embed-code" readonly rows="5">{invite}</textarea>
  <div class="row"><button type="button" data-action="meeting-copy">招待文をコピー</button></div>
  <p class="muted small">Zoom・Meet・Teams の会議のチャットに貼ると、参加者がそこからボードを開けます。Teams ではチャネルやチャットの「+」から「Web サイト」タブにボードの URL を追加できます。参加者がボードを開けるよう、共有の設定も確かめてください。</p>
</section>
{edit}
{who}"#,
            t = escape(&m.display_title(&title)),
            gcal = escape(&gcal),
            invite = escape(&invite),
        )
    }

    fn form_html(&self) -> String {
        let m = &self.meeting;
        let val = |s: &Option<String>| escape(s.as_deref().unwrap_or(""));
        let cancel = if self.has_meeting() {
            r#"<button type="button" class="quiet" data-action="meeting-cancel">キャンセル</button>"#
        } else {
            ""
        };
        format!(
            r#"<form data-form="meeting" class="stack">
  <label class="field">会議の URL<input name="url" type="url" autocomplete="off" value="{url}" placeholder="https://zoom.us/j/… ・ https://meet.google.com/… ・ https://teams.microsoft.com/…" /></label>
  <label class="field">予定の名前<input name="title" maxlength="120" autocomplete="off" value="{title}" placeholder="例: 週次定例" /></label>
  <div class="row wrap">
    <label class="field grow">開始<input name="start" type="datetime-local" value="{start}" /></label>
    <label class="field grow">終わり<input name="end" type="datetime-local" value="{end}" /></label>
  </div>
  <label class="field">カレンダーの予定のリンク(任意)<input name="calendar_url" type="url" autocomplete="off" value="{cal}" placeholder="https://calendar.google.com/calendar/event?eid=…" /></label>
  <p class="muted small">Google カレンダーの予定を開き、「︙」→「予定の公開」などでリンクをコピーして貼ると、ボードから予定に戻れます。</p>
  <div class="row"><button type="submit" class="primary" {busy}>保存</button>{cancel}</div>
</form>"#,
            url = val(&m.url),
            title = val(&m.title),
            start = to_input(m.start),
            end = to_input(m.end),
            cal = val(&m.calendar_url),
            busy = if self.busy { "disabled" } else { "" },
        )
    }

    fn render(&self) {
        self.render_toggle();
        self.render_panel();
    }
}

fn provider_key(p: MeetingProvider) -> &'static str {
    match p {
        MeetingProvider::Zoom => "zoom",
        MeetingProvider::Meet => "meet",
        MeetingProvider::Teams => "teams",
        MeetingProvider::Other => "other",
    }
}

/// サーバーから取り直す。
pub fn reload() {
    let Some(path) = with_ui(|u| u.path()) else {
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        let res = http::get::<Meeting>(&path).await;
        with_ui(|u| {
            match res {
                Ok(m) => {
                    u.meeting = m;
                    u.loaded = true;
                }
                Err(e) => log::info!("会議の情報を取れません: {}", e.message()),
            }
            u.render();
        });
    });
}

fn save(req: SetMeeting, message: &'static str) {
    let Some(path) = with_ui(|u| {
        u.busy = true;
        u.status = None;
        u.path()
    }) else {
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        let res = http::send::<_, Meeting>("PUT", &path, &req).await;
        with_ui(|u| {
            u.busy = false;
            match res {
                Ok(m) => {
                    u.meeting = m;
                    u.editing = false;
                    u.status = Some(Ok(message.to_string()));
                }
                Err(e) => u.status = Some(Err(e.message())),
            }
            u.render();
        });
    });
}

fn read_form(form: &Element) -> SetMeeting {
    let value = |name: &str| {
        form.query_selector(&format!("[name={name}]"))
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
            .map(|i| i.value().trim().to_string())
            .filter(|v| !v.is_empty())
    };
    SetMeeting {
        url: value("url"),
        title: value("title"),
        start: value("start").as_deref().and_then(from_input),
        end: value("end").as_deref().and_then(from_input),
        calendar_url: value("calendar_url"),
    }
}

fn download_ics() -> Result<(), String> {
    let (text, name) = with_ui(|u| {
        let c = u.chrome.borrow();
        let room = c.room.clone();
        drop(c);
        let host = web_sys::window()
            .and_then(|w| w.location().hostname().ok())
            .unwrap_or_else(|| "colabboard".into());
        let title = u.board_title();
        (
            meeting::ics(&u.meeting, &room, &title, &u.board_link(), &host, now_ms()),
            format!("{}.ics", file_stem(&title)),
        )
    })
    .ok_or("会議の情報がありません")?;
    let text = text.ok_or("開始の時刻を入れると保存できます")?;
    super::export::download(&name, "text/calendar;charset=utf-8", text.as_bytes())
}

/// ファイル名に使えない文字を除いた名前。
fn file_stem(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if r#"/\:*?"<>|"#.contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim();
    if s.is_empty() {
        "meeting".into()
    } else {
        s.to_string()
    }
}

/// パネルを開く・閉じる。
pub fn toggle() {
    let opened = with_ui(|u| {
        let open = u.panel.hidden();
        u.panel.set_hidden(!open);
        u.status = None;
        u.editing = false;
        u.render();
        open
    });
    if opened == Some(true) {
        reload();
    }
}

/// Esc で閉じる。
pub fn escape_key() -> bool {
    with_ui(|u| {
        if u.panel.hidden() {
            return false;
        }
        u.panel.set_hidden(true);
        u.render_toggle();
        true
    })
    .unwrap_or(false)
}

pub fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    let Some(chrome) = app.borrow().chrome.clone() else {
        return Ok(());
    };
    let toggle_el: HtmlElement = super::by_id(doc, "meeting-toggle")?;
    let panel: HtmlElement = super::by_id(doc, "meeting-panel")?;
    UI.with(|u| {
        *u.borrow_mut() = Some(Ui {
            chrome,
            toggle: toggle_el.clone(),
            panel: panel.clone(),
            meeting: Meeting::default(),
            loaded: false,
            editing: false,
            busy: false,
            status: None,
        })
    });

    let on_toggle = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |_| toggle());
    toggle_el
        .add_event_listener_with_callback("click", on_toggle.as_ref().unchecked_ref())
        .map_err(super::js_err)?;
    on_toggle.forget();

    let on_click = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |e: web_sys::MouseEvent| {
        let Some(el) = e
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|t| t.closest("[data-action]").ok().flatten())
        else {
            return;
        };
        match el.get_attribute("data-action").as_deref() {
            Some("meeting-close") => {
                escape_key();
            }
            Some("meeting-edit") => {
                with_ui(|u| {
                    u.editing = true;
                    u.status = None;
                    u.render_panel();
                });
            }
            Some("meeting-cancel") => {
                with_ui(|u| {
                    u.editing = false;
                    u.render_panel();
                });
            }
            Some("meeting-remove") => save(SetMeeting::default(), "紐づけを外しました"),
            Some("meeting-copy") => {
                let text = with_ui(|u| {
                    let title = u.board_title();
                    meeting::invite_text(
                        &u.meeting,
                        &title,
                        &u.board_link(),
                        when_range(&u.meeting).as_deref(),
                    )
                });
                if let (Some(text), Some(w)) = (text, web_sys::window()) {
                    let _ = w.navigator().clipboard().write_text(&text);
                    el.set_text_content(Some("コピーしました"));
                }
            }
            Some("meeting-ics") => {
                if let Err(err) = download_ics() {
                    with_ui(|u| {
                        u.status = Some(Err(err));
                        u.render_panel();
                    });
                }
            }
            _ => {}
        }
    });
    panel
        .add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())
        .map_err(super::js_err)?;
    on_click.forget();

    let on_submit = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        let Some(form) = e
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .filter(|f| f.get_attribute("data-form").as_deref() == Some("meeting"))
        else {
            return;
        };
        e.prevent_default();
        let req = read_form(&form);
        if req.is_empty() {
            with_ui(|u| {
                u.status = Some(Err("会議の URL か予定を入れてください".into()));
                u.render_panel();
            });
            return;
        }
        save(req, "保存しました");
    });
    panel
        .add_event_listener_with_callback("submit", on_submit.as_ref().unchecked_ref())
        .map_err(super::js_err)?;
    on_submit.forget();

    // 「まもなく開始」「開催中」の表示を時刻に合わせて変える。
    if let Some(w) = web_sys::window() {
        let tick = Closure::<dyn FnMut()>::new(move || {
            with_ui(|u| u.render_toggle());
        });
        let _ = w.set_interval_with_callback_and_timeout_and_arguments_0(
            tick.as_ref().unchecked_ref(),
            30_000,
        );
        tick.forget();
    }

    reload();
    Ok(())
}
