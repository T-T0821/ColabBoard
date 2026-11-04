//! 会議パネル: AI ファシリテーター(議題と時間配分・論点整理の助言・終わりのまとめ)、
//! リアルタイム議事録、文字起こし。
//!
//! - 状態はボードの共有ドキュメント(`board_core::meeting`)にあるので、全員に同じものが見える。
//! - 文字起こしは各自のブラウザで自分の声だけ([`crate::transcribe`])。確定した文をボードに足す。
//!   通話でミュートしている間は足さない。
//! - リアルタイム議事録は「自動で更新」を入れた人の画面が受け持ち、発言が増えていれば 1 分ごとに
//!   `/ai/minutes` を呼んで結果を共有する(サーバーには既存の議事録の API をそのまま使う)。
//! - 議題を進めると全員に見えるタイマー(`board_core::session`)をその議題の分数で動かす。
//!   議題の時間を過ぎたら、会議を始めた人の画面から 1 回だけ自動で AI に助言を頼む。
//! - 「会議を終えてまとめる」で、文字起こしとボードから議事録(決定事項・アクション)を作ってボードに置く。

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::rc::Rc;

use board_core::meeting::{Advice, AgendaItem, LiveMinutes, Meeting};
use board_core::session::format_remaining;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlElement, HtmlInputElement, HtmlTextAreaElement};

use super::ai::{api_url, post_json};
use super::collab::now_ms;
use super::ink_ui::toast;
use super::{App, by_id, listen, on_click};
use crate::http::escape as esc;
use crate::transcribe::Transcriber;

/// リアルタイム議事録を自動で作り直す間隔(ミリ秒)。
const LIVE_INTERVAL_MS: f64 = 60_000.0;
/// 自動の更新に失敗したとき(ほかの AI 機能と重なったときなど)にやり直すまで(ミリ秒)。
const RETRY_MS: f64 = 15_000.0;
/// 画面を見直す間隔(ミリ秒)。
const POLL_MS: f64 = 250.0;
/// 文字起こしの一覧に出す発言の数。
const SHOW_LINES: usize = 80;

thread_local! {
    static PLAN_BUSY: Cell<bool> = const { Cell::new(false) };
    static CHECK_BUSY: Cell<bool> = const { Cell::new(false) };
    static LIVE_BUSY: Cell<bool> = const { Cell::new(false) };
}

pub struct MeetingUi {
    toggle: HtmlElement,
    panel: HtmlElement,
    clock: HtmlElement,
    goal: HtmlInputElement,
    length: HtmlInputElement,
    notes: HtmlTextAreaElement,
    agenda: HtmlElement,
    advice: HtmlElement,
    minutes_auto: HtmlInputElement,
    minutes_status: HtmlElement,
    minutes_view: HtmlElement,
    transcribe_toggle: HtmlElement,
    transcribe_status: HtmlElement,
    transcript: HtmlElement,
    pub transcriber: Transcriber,
    polled_at: f64,
    /// 前に描いたときの目印(変わったところだけ描き直す)。
    last_agenda: String,
    last_advice: String,
    last_minutes: String,
    last_transcript: (usize, String, u64),
    last_clock: String,
    last_buttons: String,
    /// 自動の助言を頼んだ議題(議題に入った時刻で見分ける)。
    auto_advice_for: Option<f64>,
    /// 前に見た会議が進行中だったか(終わったら文字起こしを止める)。
    was_running: bool,
    /// 描画ループの外で始める自動の呼び出し([`pump`])。
    pending_live: bool,
    pending_check: bool,
}

fn el<T: JsCast>(doc: &Document, id: &str) -> Result<T, String> {
    by_id(doc, id)
}

impl MeetingUi {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            toggle: el(document, "meeting-toggle")?,
            panel: el(document, "meeting-panel")?,
            clock: el(document, "meeting-clock")?,
            goal: el(document, "meeting-goal")?,
            length: el(document, "meeting-length")?,
            notes: el(document, "meeting-notes")?,
            agenda: el(document, "meeting-agenda")?,
            advice: el(document, "meeting-advice-box")?,
            minutes_auto: el(document, "minutes-auto")?,
            minutes_status: el(document, "minutes-status")?,
            minutes_view: el(document, "minutes-view")?,
            transcribe_toggle: el(document, "transcribe-toggle")?,
            transcribe_status: el(document, "transcribe-status")?,
            transcript: el(document, "transcript-list")?,
            transcriber: Transcriber::default(),
            polled_at: f64::NEG_INFINITY,
            last_agenda: String::new(),
            last_advice: String::new(),
            last_minutes: String::new(),
            last_transcript: (usize::MAX, String::new(), u64::MAX),
            last_clock: String::new(),
            last_buttons: String::new(),
            auto_advice_for: None,
            was_running: false,
            pending_live: false,
            pending_check: false,
        })
    }

    fn set_tab(&self, tab: &str) {
        let Ok(list) = self.panel.query_selector_all("[data-mtab]") else {
            return;
        };
        for i in 0..list.length() {
            let Some(e) = list.item(i).and_then(|n| n.dyn_into::<HtmlElement>().ok()) else {
                continue;
            };
            let mine = e.get_attribute("data-mtab").as_deref() == Some(tab);
            if e.tag_name() == "BUTTON" {
                let _ = e.set_attribute("aria-selected", if mine { "true" } else { "false" });
            } else {
                e.set_hidden(!mine);
            }
        }
    }
}

/// 時刻(端末の時計)を `10:32` にする。
fn clock_of(ms: f64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    format!("{:02}:{:02}", d.get_hours(), d.get_minutes())
}

fn today() -> String {
    let d = js_sys::Date::new_0();
    format!(
        "{:04}-{:02}-{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date()
    )
}

fn to_value(v: &wasm_bindgen::JsValue) -> Value {
    js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .collect()
}

/// 音声認識の言語(画面の言語に合わせる)。
fn speech_lang() -> &'static str {
    match crate::i18n::lang() {
        crate::i18n::Lang::En => "en-US",
        crate::i18n::Lang::Ja => "ja-JP",
    }
}

/// 議事録(サーバーの `minutes` の構造)を HTML にする。
pub fn minutes_html(m: &Value) -> String {
    let mut h = String::new();
    let title = m["title"].as_str().unwrap_or_default();
    if !title.is_empty() {
        let _ = write!(h, r#"<h4 translate="no">{}</h4>"#, esc(title));
    }
    let summary = m["summary"].as_str().unwrap_or_default();
    if !summary.is_empty() {
        let _ = write!(
            h,
            r#"<p class="summary" translate="no">{}</p>"#,
            esc(summary)
        );
    }
    let list = |h: &mut String, heading: &str, items: Vec<String>| {
        if items.is_empty() {
            return;
        }
        let _ = write!(h, "<h5>{heading}</h5><ul translate=\"no\">");
        for i in items {
            let _ = write!(h, "<li>{}</li>", esc(&i));
        }
        h.push_str("</ul>");
    };
    let topics: Vec<String> = m["topics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let head = t["heading"].as_str().unwrap_or_default();
            let points = strings(&t["points"]);
            if points.is_empty() {
                head.to_string()
            } else {
                format!("{head}: {}", points.join(" / "))
            }
        })
        .filter(|s| !s.trim().is_empty())
        .collect();
    list(&mut h, "話し合ったこと", topics);
    list(&mut h, "決定事項", strings(&m["decisions"]));
    let actions: Vec<&Value> = m["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| !a["task"].as_str().unwrap_or_default().trim().is_empty())
        .collect();
    if !actions.is_empty() {
        h.push_str(r#"<h5>アクション</h5><table translate="no"><thead><tr><th>やること</th><th>担当</th><th>期限</th></tr></thead><tbody>"#);
        for a in actions {
            let cell = |k: &str| {
                let s = a[k].as_str().unwrap_or_default().trim().to_string();
                if s.is_empty() {
                    "—".to_string()
                } else {
                    esc(&s)
                }
            };
            let _ = write!(
                h,
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                cell("task"),
                cell("owner"),
                cell("due")
            );
        }
        h.push_str("</tbody></table>");
    }
    list(&mut h, "未解決の課題", strings(&m["open_questions"]));
    h
}

/// 助言を HTML にする。
pub fn advice_html(a: &Value, at: &str, item: Option<&str>) -> String {
    let mut h = String::new();
    let _ = write!(
        h,
        r#"<div class="advice-head"><strong>AI ファシリテーター</strong><span translate="no">{}{}</span></div>"#,
        item.map(|t| format!("{} · ", esc(t))).unwrap_or_default(),
        esc(at)
    );
    if a["off_track"].as_bool() == Some(true) {
        h.push_str(r#"<p class="warn">議題から話がそれています</p>"#);
    }
    let focus = a["focus"].as_str().unwrap_or_default();
    if !focus.is_empty() {
        let _ = write!(h, r#"<p class="focus" translate="no">{}</p>"#, esc(focus));
    }
    let points = strings(&a["points"]);
    if !points.is_empty() {
        h.push_str("<h5>論点</h5><ul translate=\"no\">");
        for p in points {
            let _ = write!(h, "<li>{}</li>", esc(&p));
        }
        h.push_str("</ul>");
    }
    let open = strings(&a["open"]);
    if !open.is_empty() {
        h.push_str("<h5>まだ決まっていないこと</h5><ul translate=\"no\">");
        for p in open {
            let _ = write!(h, "<li>{}</li>", esc(&p));
        }
        h.push_str("</ul>");
    }
    let suggestion = a["suggestion"].as_str().unwrap_or_default();
    if !suggestion.is_empty() {
        let _ = write!(
            h,
            r#"<p class="suggestion"><span>次の一手</span><span translate="no">{}</span></p>"#,
            esc(suggestion)
        );
    }
    let time = a["time_advice"].as_str().unwrap_or_default();
    if !time.is_empty() {
        let _ = write!(
            h,
            r#"<p class="time"><span>時間</span><span translate="no">{}</span></p>"#,
            esc(time)
        );
    }
    h
}

impl App {
    /// 毎フレーム呼ぶ。確定した発言をボードに足し、表示を合わせ、自動の更新を受け持つ。
    pub(super) fn meeting_frame(&mut self, now: f64) {
        let finals = self.meeting.transcriber.take_finals();
        if !finals.is_empty() {
            let muted = self
                .voice
                .as_ref()
                .is_some_and(|v| v.in_call() && v.snapshot().muted);
            if !muted && self.room.is_some() {
                let (id, name) = (self.identity.id.clone(), self.identity.name.clone());
                let board = self.live_board_mut();
                for (text, at) in finals {
                    board.add_transcript(&id, &name, &text, at);
                }
            }
        }
        if now - self.meeting.polled_at < POLL_MS {
            return;
        }
        self.meeting.polled_at = now;
        // `now` は描画ループの時刻なので、共有の時刻(UNIX 時刻)と比べるときは時計を読む。
        let now = now_ms();
        let meeting = self.board.meeting();
        let live = self.board.live_minutes();
        let running = meeting.as_ref().is_some_and(Meeting::is_running);
        // 会議が終わったら自分の文字起こしも止める。
        if self.meeting.was_running && !running && self.meeting.transcriber.is_on() {
            self.meeting.transcriber.stop();
            toast(
                &self.document,
                "会議が終わったので文字起こしを止めました",
                "done",
            );
        }
        self.meeting.was_running = running;
        let _ = self.meeting.toggle.set_attribute(
            "data-live",
            if self.meeting.transcriber.is_on() {
                "rec"
            } else if running {
                "on"
            } else {
                "off"
            },
        );

        self.meeting_auto(now, meeting.as_ref(), &live);
        if self.meeting.panel.hidden() {
            return;
        }
        self.render_meeting(now, meeting.as_ref(), &live);
    }

    /// 自動の議事録の更新と、時間切れのときの助言。
    fn meeting_auto(&mut self, now: f64, meeting: Option<&Meeting>, live: &LiveMinutes) {
        if !self.can_edit() || self.room.is_none() {
            return;
        }
        let me = self.identity.id.clone();
        if live.auto
            && live.owner == me
            && !LIVE_BUSY.with(Cell::get)
            && now - live.updated_at_ms >= LIVE_INTERVAL_MS
            && self.board.transcript_len() > live.lines
        {
            self.meeting.pending_live = true;
        }
        if let Some(m) = meeting
            && m.is_running()
            && m.host == me
            && let Some(item) = m.current_item()
        {
            let over = m.item_elapsed_ms(now) >= item.minutes as f64 * 60_000.0;
            if over
                && self.meeting.auto_advice_for != Some(m.item_started_at_ms)
                && !CHECK_BUSY.with(Cell::get)
            {
                self.meeting.auto_advice_for = Some(m.item_started_at_ms);
                self.meeting.pending_check = true;
            }
        }
    }

    fn render_meeting(&mut self, now: f64, meeting: Option<&Meeting>, live: &LiveMinutes) {
        let can_edit = self.can_edit();
        // ---- 時計 ----
        let clock = match meeting {
            Some(m) if m.is_running() => match (m.current, m.current_item()) {
                (Some(i), Some(item)) => format!(
                    "{}/{} · {} / {}",
                    i + 1,
                    m.agenda.len(),
                    format_remaining(m.item_elapsed_ms(now)),
                    format_remaining(item.minutes as f64 * 60_000.0)
                ),
                _ => format!("経過 {}", format_remaining(now - m.started_at_ms)),
            },
            Some(m) if m.ended => "終了".to_string(),
            _ => String::new(),
        };
        if clock != self.meeting.last_clock {
            self.meeting.clock.set_text_content(Some(&clock));
            self.meeting.last_clock = clock;
        }

        // ---- 議題 ----
        let agenda_key = format!(
            "{}|{}|{}",
            meeting
                .map(|m| serde_json::to_string(m).unwrap_or_default())
                .unwrap_or_default(),
            can_edit,
            // 進行中は残り時間のバーを動かす(1 秒ごと)。
            if meeting.is_some_and(Meeting::is_running) {
                (now / 1000.0) as u64
            } else {
                0
            }
        );
        if agenda_key != self.meeting.last_agenda {
            self.meeting.last_agenda = agenda_key;
            self.meeting
                .agenda
                .set_inner_html(&agenda_html(meeting, now, can_edit));
            if let Some(m) = meeting {
                // 自分が書きかけていなければ、目的の欄を共有の値に合わせる。
                let active = self
                    .document
                    .active_element()
                    .is_some_and(|a| a.id() == "meeting-goal");
                if !active && self.meeting.goal.value() != m.goal {
                    self.meeting.goal.set_value(&m.goal);
                }
            }
        }
        let buttons = {
            let has_agenda = meeting.is_some_and(|m| !m.agenda.is_empty());
            let running = meeting.is_some_and(Meeting::is_running);
            let last = meeting.is_some_and(|m| m.current.is_some_and(|c| c + 1 == m.agenda.len()));
            let set = |id: &str, hidden: bool, label: Option<&str>| {
                if let Some(b) = self
                    .document
                    .get_element_by_id(id)
                    .and_then(|e| e.dyn_into::<HtmlElement>().ok())
                {
                    b.set_hidden(hidden || !can_edit);
                    if let Some(l) = label {
                        b.set_text_content(Some(l));
                    }
                }
            };
            set("meeting-start", !has_agenda || running, None);
            set(
                "meeting-next",
                !running,
                Some(if last {
                    "最後の議題を終える"
                } else {
                    "次の議題へ"
                }),
            );
            set("meeting-advice", !has_agenda, None);
            set("meeting-end", !running, None);
            set("meeting-reset", running || meeting.is_none(), None);
            format!("{has_agenda}{running}{last}{can_edit}")
        };
        self.meeting.last_buttons = buttons;

        // ---- 助言 ----
        let advice = self.board.advice();
        let advice_key = advice
            .as_ref()
            .map(|a| format!("{}{}", a.at_ms, a.body))
            .unwrap_or_default();
        if advice_key != self.meeting.last_advice {
            self.meeting.last_advice = advice_key;
            match advice {
                Some(a) => {
                    let item = a
                        .item
                        .and_then(|i| meeting.and_then(|m| m.agenda.get(i)))
                        .map(|i| i.title.clone());
                    self.meeting.advice.set_inner_html(&advice_html(
                        &a.body,
                        &clock_of(a.at_ms),
                        item.as_deref(),
                    ));
                    self.meeting.advice.set_hidden(false);
                }
                None => {
                    self.meeting.advice.set_inner_html("");
                    self.meeting.advice.set_hidden(true);
                }
            }
        }

        // ---- 議事録 ----
        let minutes_key = format!(
            "{}|{}|{}|{}|{}",
            live.updated_at_ms,
            live.auto,
            live.owner,
            LIVE_BUSY.with(Cell::get),
            self.board.transcript_len()
        );
        if minutes_key != self.meeting.last_minutes {
            self.meeting.last_minutes = minutes_key;
            self.meeting.minutes_auto.set_checked(live.auto);
            self.meeting.minutes_auto.set_disabled(!can_edit);
            let lines = self.board.transcript_len();
            let mut status = String::new();
            if LIVE_BUSY.with(Cell::get) {
                status.push_str("AI が議事録を更新しています…");
            } else if live.updated_at_ms > 0.0 {
                let new = lines.saturating_sub(live.lines);
                let _ = write!(status, "{} 更新", clock_of(live.updated_at_ms));
                if new > 0 {
                    let _ = write!(status, "(その後の発言 {new} 件)");
                }
            }
            if live.auto {
                if !status.is_empty() {
                    status.push_str(" · ");
                }
                if live.owner == self.identity.id {
                    status.push_str("この画面から 1 分ごとに更新します");
                } else {
                    let _ = write!(
                        status,
                        "{} さんの画面から自動で更新しています",
                        live.owner_name
                    );
                }
            }
            self.meeting.minutes_status.set_text_content(Some(&status));
            match &live.minutes {
                Some(m) => self.meeting.minutes_view.set_inner_html(&minutes_html(m)),
                None => self.meeting.minutes_view.set_inner_html(
                    r#"<p class="empty">文字起こしやボードの内容から、会議中に議事録を書き進めます。「自動で更新」を入れると、発言が増えるたびに 1 分ごとに書き直します。</p>"#,
                ),
            }
        }

        // ---- 文字起こし ----
        let t = &self.meeting.transcriber;
        let key = (self.board.transcript_len(), t.interim(), t.version());
        if key != self.meeting.last_transcript {
            self.meeting.last_transcript = key;
            let on = t.is_on();
            let _ = self
                .meeting
                .transcribe_toggle
                .set_attribute("aria-pressed", if on { "true" } else { "false" });
            self.meeting.transcribe_toggle.set_text_content(Some(if on {
                "■ 文字起こしを止める"
            } else {
                "● 文字起こしを始める"
            }));
            self.meeting
                .transcribe_toggle
                .set_hidden(!can_edit || self.room.is_none());
            let status = if let Some(e) = t.error() {
                e
            } else if on {
                let muted = self
                    .voice
                    .as_ref()
                    .is_some_and(|v| v.in_call() && v.snapshot().muted);
                if muted {
                    "ミュート中は文字にしません".to_string()
                } else {
                    "あなたの声を文字にしています。ほかの参加者も各自で始めると、発言者ごとに記録されます".to_string()
                }
            } else if !Transcriber::supported() {
                "このブラウザは音声の文字起こしに対応していません(Chrome・Edge・Safari で使えます)"
                    .to_string()
            } else {
                String::new()
            };
            self.meeting
                .transcribe_status
                .set_text_content(Some(&status));
            let _ = self.meeting.transcribe_status.set_attribute(
                "data-state",
                if t.error().is_some() { "error" } else { "info" },
            );
            let lines = self.board.transcript();
            let mut h = String::new();
            for l in lines.iter().skip(lines.len().saturating_sub(SHOW_LINES)) {
                let _ = write!(
                    h,
                    r#"<li><span class="t" translate="no">{}</span><span class="who" translate="no">{}</span><span class="text" translate="no">{}</span></li>"#,
                    clock_of(l.at_ms),
                    esc(&l.name),
                    esc(&l.text)
                );
            }
            let interim = t.interim();
            if !interim.is_empty() {
                let _ = write!(
                    h,
                    r#"<li class="interim"><span class="t">…</span><span class="who" translate="no">{}</span><span class="text" translate="no">{}</span></li>"#,
                    esc(&self.identity.name),
                    esc(&interim)
                );
            }
            if h.is_empty() {
                h.push_str(r#"<li class="empty">まだ発言はありません。「文字起こしを始める」を押して話すと、ここに出ます。</li>"#);
            }
            self.meeting.transcript.set_inner_html(&h);
            self.meeting
                .transcript
                .set_scroll_top(self.meeting.transcript.scroll_height());
        }
    }

    /// 議題を書き換える(書けない人は何もしない)。
    fn edit_meeting(&mut self, f: impl FnOnce(&mut Meeting)) {
        if !self.can_edit() {
            return;
        }
        let mut m = self.board.meeting().unwrap_or_default();
        f(&mut m);
        self.board.set_meeting(Some(&m));
        self.meeting.last_agenda.clear();
    }

    fn meeting_start(&mut self) {
        let now = now_ms();
        let me = self.identity.id.clone();
        let goal = self.meeting.goal.value();
        let mut minutes = None;
        self.edit_meeting(|m| {
            if !goal.trim().is_empty() {
                m.goal = goal;
            }
            m.host = me;
            m.start(now);
            minutes = m.current_item().map(|i| i.minutes);
        });
        if let Some(min) = minutes {
            self.board.start_timer(min as f64 * 60_000.0, now);
        }
        self.board.set_advice(None);
        // 始めた人の画面からリアルタイム議事録を書き進める。
        let mut live = self.board.live_minutes();
        if !live.auto {
            live.auto = true;
            live.owner = self.identity.id.clone();
            live.owner_name = self.identity.name.clone();
            self.board.set_live_minutes(&live);
        }
        self.meeting.auto_advice_for = None;
        toast(
            &self.document,
            "会議を始めました。文字起こしを始めると、発言が議事録に入ります",
            "done",
        );
    }

    fn meeting_next(&mut self) {
        let now = now_ms();
        let mut next = None;
        let mut finished = false;
        self.edit_meeting(|m| {
            m.advance(now);
            next = m.current_item().map(|i| i.minutes);
            finished = m.current.is_none();
        });
        match next {
            Some(min) => self.board.start_timer(min as f64 * 60_000.0, now),
            None => self.board.stop_timer(),
        }
        if finished {
            toast(
                &self.document,
                "議題はすべて終わりました。「会議を終えてまとめる」で議事録をボードに置けます",
                "done",
            );
        }
    }

    fn toggle_transcribe(&mut self) {
        let t = self.meeting.transcriber.clone();
        if t.is_on() {
            t.stop();
            return;
        }
        if !self.can_edit() || self.room.is_none() {
            toast(
                &self.document,
                "文字起こしは共同編集のボードで、編集できる人だけ使えます",
                "error",
            );
            return;
        }
        if let Err(e) = t.start(speech_lang()) {
            toast(&self.document, &e, "error");
        }
    }

    fn place_live_minutes(&mut self) {
        let live = self.board.live_minutes();
        if live.markdown.trim().is_empty() {
            toast(
                &self.document,
                "まだ議事録がありません。「今すぐ更新」で作れます",
                "error",
            );
            return;
        }
        if !self.can_edit() {
            return;
        }
        let n = self.place_minutes(&live.markdown);
        if n > 0 {
            toast(
                &self.document,
                "議事録をボードの右に置きました。Ctrl/⌘+Z で戻せます",
                "done",
            );
        }
    }
}

/// 議題の一覧の HTML。
fn agenda_html(meeting: Option<&Meeting>, now: f64, can_edit: bool) -> String {
    let Some(m) = meeting.filter(|m| !m.agenda.is_empty()) else {
        return r#"<li class="empty">目的と時間を入れて「AI で進行案を作る」を押すと、議題と時間配分を提案します。</li>"#.into();
    };
    let mut h = String::new();
    for (i, a) in m.agenda.iter().enumerate() {
        let state = match m.current {
            Some(c) if c == i && m.is_running() => "current",
            Some(c) if i < c => "done",
            // 終わった後は、時間を使った議題だけ済みにする(飛ばした議題は印を付けない)。
            None if m.started_at_ms > 0.0 => {
                if m.spent_ms.get(i).copied().unwrap_or(0.0) > 0.0 {
                    "done"
                } else {
                    "skipped"
                }
            }
            _ => "todo",
        };
        let spent = m.spent_ms.get(i).copied().unwrap_or(0.0)
            + if state == "current" {
                m.item_elapsed_ms(now)
            } else {
                0.0
            };
        let plan = a.minutes as f64 * 60_000.0;
        let ratio = (spent / plan).clamp(0.0, 1.0);
        let over = spent > plan;
        let time = if state == "todo" || state == "skipped" {
            String::new()
        } else {
            format!(
                r#"<span class="spent" data-over="{over}" translate="no">{}</span>"#,
                format_remaining(spent)
            )
        };
        let minutes = if can_edit && (state == "todo" || state == "current") {
            format!(
                r#"<input type="number" min="1" max="240" value="{}" data-mi="{i}" aria-label="分"> 分"#,
                a.minutes
            )
        } else {
            format!(r#"<span translate="no">{}</span> 分"#, a.minutes)
        };
        let del = if can_edit && state == "todo" {
            format!(
                r#"<button type="button" class="icon" data-del="{i}" aria-label="この議題を消す" title="この議題を消す">✕</button>"#
            )
        } else {
            String::new()
        };
        let goal = if a.goal.is_empty() {
            String::new()
        } else {
            format!(
                r#"<span class="goal" translate="no">{}</span>"#,
                esc(&a.goal)
            )
        };
        let _ = write!(
            h,
            r#"<li data-state="{state}" style="--p:{ratio:.3}"><span class="title" translate="no">{}</span>{goal}<span class="mins">{minutes}{time}</span>{del}</li>"#,
            esc(&a.title)
        );
    }
    let _ = write!(
        h,
        r#"<li class="total">合計 <span translate="no">{}</span> 分</li>"#,
        m.planned_minutes()
    );
    h
}

fn start_plan(app: &Rc<RefCell<App>>) {
    if PLAN_BUSY.with(Cell::get) {
        return;
    }
    let (window, doc, url, body, goal) = {
        let a = app.borrow();
        if !a.can_edit() {
            return;
        }
        let Some(room) = a.room.clone() else {
            toast(
                &a.document,
                "AI 機能は共同編集のボードでだけ使えます",
                "error",
            );
            return;
        };
        let goal = a.meeting.goal.value().trim().to_string();
        if goal.is_empty() {
            toast(&a.document, "会議の目的を書いてください", "error");
            let _ = a.meeting.goal.focus();
            return;
        }
        let minutes = a
            .meeting
            .length
            .value()
            .parse::<u32>()
            .unwrap_or(30)
            .clamp(5, 480);
        let Some(url) = api_url(&a.window, &room, "facilitate") else {
            return;
        };
        let body = json!({
            "mode": "plan",
            "goal": goal,
            "minutes": minutes,
            "notes": a.meeting.notes.value(),
        })
        .to_string();
        (a.window.clone(), a.document.clone(), url, body, goal)
    };
    PLAN_BUSY.with(|b| b.set(true));
    toast(&doc, "AI が議題と時間配分を考えています…", "busy");
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = post_json(&window, &url, &body).await;
        PLAN_BUSY.with(|b| b.set(false));
        let v = match result {
            Ok(v) => to_value(&v),
            Err(e) => {
                toast(&doc, &e, "error");
                return;
            }
        };
        let agenda: Vec<AgendaItem> =
            serde_json::from_value(v["agenda"].clone()).unwrap_or_default();
        if agenda.is_empty() {
            toast(&doc, "進行案を作れませんでした", "error");
            return;
        }
        let tips = v["tips"].as_str().unwrap_or_default().to_string();
        let fallback = v["fallback"].as_bool() == Some(true);
        let mut a = app.borrow_mut();
        a.edit_meeting(|m| {
            *m = Meeting {
                goal,
                agenda,
                ..Default::default()
            };
        });
        a.board.set_advice(None);
        if !tips.is_empty() {
            let advice = Advice {
                body: json!({ "focus": "", "points": [], "open": [], "suggestion": tips, "time_advice": "", "off_track": false }),
                at_ms: now_ms(),
                item: None,
            };
            a.board.set_advice(Some(&advice));
        }
        toast(
            &doc,
            if fallback {
                "AI が使えないので、メモの行を議題にして時間を均等に割りました"
            } else {
                "進行案を作りました。分数は直せます。「会議を始める」で最初の議題のタイマーが動きます"
            },
            "done",
        );
    });
}

/// AI に今の議題の助言を頼む。`auto` は時間切れのときの自動の呼び出し(失敗しても知らせない)。
fn start_check(app: &Rc<RefCell<App>>, auto: bool) {
    if CHECK_BUSY.with(Cell::get) {
        return;
    }
    let (window, doc, url, item) = {
        let a = app.borrow();
        if !a.can_edit() {
            return;
        }
        let Some(room) = a.room.clone() else {
            return;
        };
        let Some(url) = api_url(&a.window, &room, "facilitate") else {
            return;
        };
        let item = a.board.meeting().and_then(|m| m.current);
        (a.window.clone(), a.document.clone(), url, item)
    };
    let body = json!({ "mode": "check", "now": now_ms() }).to_string();
    CHECK_BUSY.with(|b| b.set(true));
    if !auto {
        toast(&doc, "AI が議論を整理しています…", "busy");
    }
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = post_json(&window, &url, &body).await;
        CHECK_BUSY.with(|b| b.set(false));
        let v = match result {
            Ok(v) => to_value(&v),
            Err(e) => {
                if !auto {
                    toast(&doc, &e, "error");
                }
                return;
            }
        };
        let mut a = app.borrow_mut();
        a.board.set_advice(Some(&Advice {
            body: v,
            at_ms: now_ms(),
            item,
        }));
        toast(
            &doc,
            if auto {
                "議題の時間を過ぎました。AI ファシリテーターの助言を会議パネルに出しました"
            } else {
                "助言を会議パネルに出しました"
            },
            "done",
        );
        // 自動のときはパネルを開いて見せる。
        if auto && a.meeting.panel.hidden() {
            a.meeting.panel.set_hidden(false);
            let _ = a.meeting.toggle.set_attribute("aria-pressed", "true");
            a.meeting.set_tab("facil");
        }
    });
}

/// リアルタイム議事録を作り直す。`manual` は「今すぐ更新」から。
fn start_live_minutes(app: &Rc<RefCell<App>>, manual: bool) {
    if LIVE_BUSY.with(Cell::get) {
        return;
    }
    let (window, doc, url, lines) = {
        let a = app.borrow();
        if !a.can_edit() {
            return;
        }
        let Some(room) = a.room.clone() else {
            toast(
                &a.document,
                "AI 機能は共同編集のボードでだけ使えます",
                "error",
            );
            return;
        };
        let Some(url) = api_url(&a.window, &room, "minutes") else {
            return;
        };
        (
            a.window.clone(),
            a.document.clone(),
            url,
            a.board.transcript_len(),
        )
    };
    let body = json!({ "ids": [], "date": today(), "transcript": true }).to_string();
    LIVE_BUSY.with(|b| b.set(true));
    if manual {
        toast(&doc, "AI が議事録を更新しています…", "busy");
    }
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = post_json(&window, &url, &body).await;
        LIVE_BUSY.with(|b| b.set(false));
        let mut a = app.borrow_mut();
        a.meeting.last_minutes.clear();
        let v = match result {
            Ok(v) => to_value(&v),
            Err(e) => {
                // 自動のときは知らせず、少し待ってやり直す。
                if manual {
                    toast(&doc, &e, "error");
                } else {
                    let mut live = a.board.live_minutes();
                    live.updated_at_ms = now_ms() - LIVE_INTERVAL_MS + RETRY_MS;
                    a.board.set_live_minutes(&live);
                }
                return;
            }
        };
        // 待っている間に設定が変わっているかもしれないので、今の値に結果だけ書く。
        let mut live = a.board.live_minutes();
        live.minutes = Some(v["minutes"].clone());
        live.markdown = v["markdown"].as_str().unwrap_or_default().to_string();
        live.updated_at_ms = now_ms();
        live.lines = v["lines"].as_u64().map_or(lines, |n| n as usize);
        a.board.set_live_minutes(&live);
        if manual {
            toast(&doc, "議事録を更新しました", "done");
        }
    });
}

/// 会議を終え、文字起こしとボードから議事録を作ってボードに置く。
fn finish_meeting(app: &Rc<RefCell<App>>) {
    if LIVE_BUSY.with(Cell::get) {
        toast(
            &app.borrow().document,
            "議事録を更新中です。少し待ってからもう一度押してください",
            "error",
        );
        return;
    }
    let (window, doc, url) = {
        let mut a = app.borrow_mut();
        if !a.can_edit() {
            return;
        }
        let Some(room) = a.room.clone() else {
            return;
        };
        let Some(url) = api_url(&a.window, &room, "minutes") else {
            return;
        };
        let now = now_ms();
        a.edit_meeting(|m| m.finish(now));
        a.board.stop_timer();
        let mut live = a.board.live_minutes();
        live.auto = false;
        a.board.set_live_minutes(&live);
        a.meeting.transcriber.stop();
        (a.window.clone(), a.document.clone(), url)
    };
    let body = json!({ "ids": [], "date": today(), "transcript": true }).to_string();
    LIVE_BUSY.with(|b| b.set(true));
    toast(
        &doc,
        "AI が会議のまとめ(決定事項・アクション)を書いています…",
        "busy",
    );
    let app = app.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = post_json(&window, &url, &body).await;
        LIVE_BUSY.with(|b| b.set(false));
        let mut a = app.borrow_mut();
        a.meeting.last_minutes.clear();
        let v = match result {
            Ok(v) => to_value(&v),
            Err(e) => {
                toast(&doc, &e, "error");
                return;
            }
        };
        let md = v["markdown"].as_str().unwrap_or_default().to_string();
        let mut live = a.board.live_minutes();
        live.minutes = Some(v["minutes"].clone());
        live.markdown = md.clone();
        live.updated_at_ms = now_ms();
        live.lines = v["lines"].as_u64().unwrap_or(0) as usize;
        a.board.set_live_minutes(&live);
        let n = a.place_minutes(&md);
        let actions = v["minutes"]["actions"].as_array().map_or(0, Vec::len);
        if n == 0 {
            toast(&doc, "議事録を作れませんでした", "error");
        } else {
            toast(
                &doc,
                &format!(
                    "会議のまとめをボードの右に置きました(アクション {actions} 件)。Ctrl/⌘+Z で戻せます"
                ),
                "done",
            );
        }
    });
}

/// フレームの後に呼ぶ。`meeting_frame` が決めた自動の呼び出しを始める(App を借りていない所で)。
pub(super) fn pump(app: &Rc<RefCell<App>>) {
    let (live, check) = {
        let mut a = app.borrow_mut();
        (
            std::mem::take(&mut a.meeting.pending_live),
            std::mem::take(&mut a.meeting.pending_check),
        )
    };
    if live {
        start_live_minutes(app, false);
    }
    if check {
        start_check(app, true);
    }
}

fn index_attr(e: &web_sys::Event, attr: &str) -> Option<(usize, web_sys::Element)> {
    let el = e
        .target()?
        .dyn_into::<web_sys::Element>()
        .ok()?
        .closest(&format!("[{attr}]"))
        .ok()??;
    let i = el.get_attribute(attr)?.parse().ok()?;
    Some((i, el))
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    let (toggle, panel, agenda, auto) = {
        let a = app.borrow();
        (
            a.meeting.toggle.clone(),
            a.meeting.panel.clone(),
            a.meeting.agenda.clone(),
            a.meeting.minutes_auto.clone(),
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
                let open = a.meeting.panel.hidden();
                a.meeting.panel.set_hidden(!open);
                let _ = a
                    .meeting
                    .toggle
                    .set_attribute("aria-pressed", if open { "true" } else { "false" });
                // 開いたらすぐ描き直す。
                a.meeting.polled_at = f64::NEG_INFINITY;
                a.meeting.last_agenda.clear();
                a.meeting.last_advice.clear();
                a.meeting.last_minutes.clear();
                a.meeting.last_transcript.0 = usize::MAX;
            },
        )?;
    }
    on_click(doc, "meeting-close", app, |a| {
        a.meeting.panel.set_hidden(true);
        let _ = a.meeting.toggle.set_attribute("aria-pressed", "false");
    })?;
    {
        let app = app.clone();
        listen(
            panel.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                let Some(tab) = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    .and_then(|t| t.closest("button[data-mtab]").ok().flatten())
                    .and_then(|t| t.get_attribute("data-mtab"))
                else {
                    return;
                };
                app.borrow().meeting.set_tab(&tab);
            },
        )?;
    }
    {
        let app = app.clone();
        on_click(doc, "meeting-plan", &app.clone(), move |_| {
            let app = app.clone();
            // 借用を返してから呼ぶ。
            wasm_bindgen_futures::spawn_local(async move { start_plan(&app) });
        })?;
    }
    on_click(doc, "meeting-start", app, |a| a.meeting_start())?;
    on_click(doc, "meeting-next", app, |a| a.meeting_next())?;
    {
        let app = app.clone();
        on_click(doc, "meeting-advice", &app.clone(), move |_| {
            let app = app.clone();
            wasm_bindgen_futures::spawn_local(async move { start_check(&app, false) });
        })?;
    }
    {
        let app = app.clone();
        on_click(doc, "meeting-end", &app.clone(), move |_| {
            let app = app.clone();
            wasm_bindgen_futures::spawn_local(async move { finish_meeting(&app) });
        })?;
    }
    on_click(doc, "meeting-reset", app, |a| {
        if !a.can_edit() {
            return;
        }
        a.board.set_meeting(None);
        a.board.set_advice(None);
        a.meeting.last_agenda.clear();
    })?;
    {
        let app = app.clone();
        listen(
            agenda.as_ref(),
            "change",
            false,
            move |e: web_sys::Event| {
                let Some((i, el)) = index_attr(&e, "data-mi") else {
                    return;
                };
                let Some(v) = el
                    .dyn_into::<HtmlInputElement>()
                    .ok()
                    .and_then(|i| i.value().parse::<u32>().ok())
                else {
                    return;
                };
                app.borrow_mut().edit_meeting(|m| {
                    if let Some(a) = m.agenda.get_mut(i) {
                        a.minutes = v.clamp(1, 240);
                    }
                });
            },
        )?;
    }
    {
        let app = app.clone();
        listen(
            agenda.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                let Some((i, _)) = index_attr(&e, "data-del") else {
                    return;
                };
                app.borrow_mut().edit_meeting(|m| {
                    if i < m.agenda.len() && m.current.is_none_or(|c| i > c) {
                        m.agenda.remove(i);
                        if i < m.spent_ms.len() {
                            m.spent_ms.remove(i);
                        }
                    }
                });
            },
        )?;
    }
    {
        let app = app.clone();
        listen(auto.as_ref(), "change", false, move |_: web_sys::Event| {
            let mut a = app.borrow_mut();
            if !a.can_edit() {
                return;
            }
            let mut live = a.board.live_minutes();
            live.auto = a.meeting.minutes_auto.checked();
            if live.auto {
                // 入れた人が受け持つ。
                live.owner = a.identity.id.clone();
                live.owner_name = a.identity.name.clone();
            }
            a.board.set_live_minutes(&live);
            a.meeting.last_minutes.clear();
        })?;
    }
    {
        let app = app.clone();
        on_click(doc, "minutes-refresh", &app.clone(), move |_| {
            let app = app.clone();
            wasm_bindgen_futures::spawn_local(async move { start_live_minutes(&app, true) });
        })?;
    }
    on_click(doc, "minutes-place", app, |a| a.place_live_minutes())?;
    on_click(doc, "transcribe-toggle", app, |a| a.toggle_transcribe())?;
    on_click(doc, "transcript-clear", app, |a| {
        if !a.can_edit() {
            return;
        }
        let msg = crate::i18n::tr("文字起こしを全員の画面から消します。よろしいですか?");
        if a.window.confirm_with_message(&msg).unwrap_or(false) {
            a.live_board_mut().clear_transcript();
        }
    })?;
    Ok(())
}
