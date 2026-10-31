//! 音声通話の画面: 上の「通話」ボタン、通話中のバー(参加者・ミュート・退出)、カーソル横の印。
//!
//! 通話そのものは [`crate::voice`]。ここは状態が変わったときに表示を合わせるだけ。

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::{Document, HtmlElement};

use super::{App, by_id, on_click};
use crate::voice::Snapshot;

pub struct CallUi {
    toggle: HtmlElement,
    bar: HtmlElement,
    people: HtmlElement,
    mute: HtmlElement,
    status: HtmlElement,
    last_version: Option<u64>,
    last_names: Vec<(u32, String, u32)>,
}

impl CallUi {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            toggle: by_id(document, "call-toggle")?,
            bar: by_id(document, "call-bar")?,
            people: by_id(document, "call-people")?,
            mute: by_id(document, "call-mute")?,
            status: by_id(document, "call-status")?,
            last_version: None,
            last_names: Vec::new(),
        })
    }
}

/// 名前の頭文字(丸いアイコンに出す)。
fn initial(name: &str) -> String {
    let name = name.strip_prefix("ゲスト・").unwrap_or(name);
    name.chars()
        .next()
        .map(|c| c.to_string())
        .unwrap_or_default()
}

fn person_html(name: &str, color: u32, muted: bool, speaking: bool, connected: bool) -> String {
    let name_esc = crate::http::escape(name);
    let state = if muted {
        "（ミュート中）"
    } else if speaking {
        "（話しています）"
    } else if !connected {
        "（接続中…）"
    } else {
        ""
    };
    format!(
        r#"<span class="call-person" style="--c:#{color:06x}" data-muted="{muted}" data-speaking="{speaking}" data-connected="{connected}" title="{name_esc}{state}" role="listitem"><span class="call-avatar" aria-hidden="true">{}</span><span class="sr-only">{name_esc}{state}</span></span>"#,
        crate::http::escape(&initial(name)),
        color = color & 0xff_ffff,
    )
}

impl App {
    pub(super) fn call_frame(&mut self) {
        let Some(voice) = self.voice.clone() else {
            return;
        };
        let version = voice.version();
        // 名前や色はカーソルの情報から引くので、それが変わったときも描き直す。
        let names: Vec<(u32, String, u32)> = self
            .cursors
            .peers()
            .map(|p| (p.peer, p.name.clone(), p.color))
            .collect();
        if self.call_ui.last_version == Some(version) && self.call_ui.last_names == names {
            return;
        }
        self.call_ui.last_version = Some(version);
        self.call_ui.last_names = names;
        let snap = voice.snapshot();
        self.render_call(&snap);
        for p in self.cursors.peers().map(|p| p.peer).collect::<Vec<_>>() {
            self.cursors.set_voice(p, voice.state_of(p));
        }
    }

    fn render_call(&self, s: &Snapshot) {
        let ui = &self.call_ui;
        let others = s.others.len();
        let (label, pressed) = if s.in_call {
            ("通話中".to_string(), "true")
        } else if s.joining {
            ("接続中…".to_string(), "false")
        } else if others > 0 {
            (format!("通話に参加 · {others}人"), "false")
        } else {
            ("通話".to_string(), "false")
        };
        ui.toggle.set_text_content(Some(&label));
        let _ = ui.toggle.set_attribute("aria-pressed", pressed);
        let _ = ui
            .toggle
            .set_attribute("data-live", if others > 0 { "true" } else { "false" });
        ui.toggle.set_title(if s.in_call {
            "通話から抜ける"
        } else {
            "ボードを開いたまま、参加者と音声で話す"
        });

        let show_bar = s.in_call || s.error.is_some();
        ui.bar.set_hidden(!show_bar);
        if !show_bar {
            return;
        }
        let mut html = String::new();
        if s.in_call {
            html.push_str(&person_html(
                &format!("{}(あなた)", self.identity.name),
                self.identity.color,
                s.muted,
                s.speaking,
                true,
            ));
            for v in &s.others {
                let color = self.cursors.get(v.peer).map_or(0x8a8f9c, |p| p.color);
                html.push_str(&person_html(
                    &self.peer_name(v.peer),
                    color,
                    v.muted,
                    v.speaking,
                    s.connected.contains(&v.peer),
                ));
            }
        }
        ui.people.set_inner_html(&html);
        ui.people.set_hidden(!s.in_call);
        ui.mute.set_hidden(!s.in_call);
        let _ = ui
            .mute
            .set_attribute("aria-pressed", if s.muted { "true" } else { "false" });
        let mute_label = if s.muted {
            "ミュートを解除"
        } else {
            "ミュート"
        };
        ui.mute.set_title(mute_label);
        let _ = ui.mute.set_attribute("aria-label", mute_label);
        let leave_hidden = !s.in_call;
        if let Some(leave) = self.document.get_element_by_id("call-leave") {
            let _ = leave.toggle_attribute_with_force("hidden", leave_hidden);
        }
        let status = match &s.error {
            Some(e) => e.clone(),
            None if s.others.is_empty() => "ほかの人が入るのを待っています".into(),
            None => String::new(),
        };
        ui.status.set_text_content(Some(&status));
        ui.status.set_hidden(status.is_empty());
        let _ = ui.status.set_attribute(
            "data-state",
            if s.error.is_some() { "error" } else { "info" },
        );
    }
}

pub fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    on_click(doc, "call-toggle", app, |a| {
        let Some(v) = a.voice.clone() else {
            super::show_error("通話はサーバーにつながっているボードで使えます");
            return;
        };
        if v.in_call() {
            v.leave();
        } else {
            v.join();
        }
    })?;
    on_click(doc, "call-mute", app, |a| {
        if let Some(v) = &a.voice {
            v.toggle_mute();
        }
    })?;
    on_click(doc, "call-leave", app, |a| {
        if let Some(v) = &a.voice {
            v.leave();
            v.clear_error();
        }
    })?;
    on_click(doc, "call-dismiss", app, |a| {
        if let Some(v) = &a.voice {
            v.clear_error();
        }
    })?;
    Ok(())
}
