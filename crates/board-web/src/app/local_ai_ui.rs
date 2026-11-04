//! 「ブラウザ内 AI」パネル。音声の文字起こしと手書き文字の学習を、端末の中だけで行う。
//!
//! - クラウド AI を使わない設定(端末ごと)
//! - 音声: モデルの準備(初回だけ取得)・録音して文字起こし・結果を付箋にしてボードに置く
//! - 手書き: 選んだ線に正しい文字を教える(次から読めるようになる)。教えた線はその文字に置き換える
//!
//! 処理の本体は [`crate::local_ai`]。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::color::STICKY_COLORS;
use board_core::ink::{InkItem, InkKind};
use board_core::{Shape, Stroke, Vec2};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Document, HtmlElement, HtmlInputElement, HtmlSelectElement};

use super::ink_ui::toast;
use super::{App, by_id, listen};
use crate::http::escape;
use crate::local_ai::{self, Accel, AccelPref, Model, Segmenter, Speech};

/// 文字起こしの 1 行。
#[derive(Clone)]
struct Said {
    time: String,
    text: String,
}

#[derive(Default)]
struct State {
    said: Vec<Said>,
    recording: bool,
    /// 文字起こし待ちのかたまりの数。
    pending: usize,
    error: Option<String>,
    /// 話すたびに付箋で置いた数(並べる位置に使う)。
    placed: usize,
    /// 選んでいるモデルがこの端末に保存済みか(確かめたら入る)。
    cached: Option<(Model, bool)>,
}

#[derive(Clone)]
struct Ui {
    toggle: HtmlElement,
    panel: HtmlElement,
    state: Rc<RefCell<State>>,
}

const LANGS: [(&str, &str); 3] = [("ja", "日本語"), ("en", "English"), ("", "自動")];

/// 「計算する装置」の選択肢。
const ACCELS: [(AccelPref, &str); 5] = [
    (AccelPref::Auto, "自動(NPU → GPU → CPU)"),
    (AccelPref::Only(Accel::Npu), "NPU(WebNN)"),
    (AccelPref::Only(Accel::Gpu), "GPU(WebNN)"),
    (AccelPref::Only(Accel::WebGpu), "GPU(WebGPU)"),
    (AccelPref::Cpu, "CPU(wasm)"),
];

fn now_label() -> String {
    let d = js_sys::Date::new_0();
    format!("{:02}:{:02}", d.get_hours(), d.get_minutes())
}

fn fmt_mb(bytes: f64) -> String {
    format!("{:.0} MB", bytes / 1_000_000.0)
}

impl Ui {
    fn q<T: JsCast>(&self, name: &str) -> Option<T> {
        self.panel
            .query_selector(&format!("[data-lai=\"{name}\"]"))
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<T>().ok())
    }

    /// パネルの骨組み(開いたときに 1 回だけ作る)。
    fn build(&self) {
        let model = local_ai::model();
        let lang = local_ai::language();
        let models: String = Model::ALL
            .iter()
            .map(|m| {
                format!(
                    r#"<option value="{}"{}>{}</option>"#,
                    m.id(),
                    if *m == model { " selected" } else { "" },
                    escape(m.label())
                )
            })
            .collect();
        let langs: String = LANGS
            .iter()
            .map(|(code, name)| {
                format!(
                    r#"<option value="{code}"{}>{name}</option>"#,
                    if *code == lang { " selected" } else { "" }
                )
            })
            .collect();
        let pref = local_ai::accel_pref();
        let accels: String = ACCELS
            .iter()
            .map(|(p, name)| {
                format!(
                    r#"<option value="{}"{}>{}</option>"#,
                    p.id(),
                    if *p == pref { " selected" } else { "" },
                    escape(name)
                )
            })
            .collect();
        let html = format!(
            r#"<header class="row"><strong class="grow">ブラウザ内 AI</strong><button class="quiet" data-lai="close" aria-label="閉じる">×</button></header>
<p class="lai-lead">音声と手書きを、この端末の中だけで文字にします。内容はサーバーにも AI の会社にも送られません。</p>
<label class="lai-check"><input type="checkbox" data-lai="cloud-off"{cloud}> クラウド AI を使わない(この端末)</label>
<p class="lai-note">オンにすると、要約・図の生成・議事録などサーバーの Claude を使う機能を止め、「AI で整形」も端末内で読みます。</p>
<h3>音声の文字起こし</h3>
<div class="lai-row"><label class="label" for="lai-model">モデル</label><select id="lai-model" data-lai="model">{models}</select></div>
<div class="lai-row"><label class="label" for="lai-lang">言語</label><select id="lai-lang" data-lai="lang">{langs}</select></div>
<div class="lai-row"><label class="label" for="lai-accel">計算する装置</label><select id="lai-accel" data-lai="accel">{accels}</select></div>
<p class="lai-note" data-lai="accel-note"></p>
<p class="lai-status" data-lai="status" role="status"></p>
<div class="lai-progress" data-lai="progress" hidden><div></div></div>
<div class="lai-row"><button data-lai="load">モデルを準備</button><button class="primary" data-lai="rec" aria-pressed="false">● 録音して文字起こし</button></div>
<ol class="lai-said" data-lai="said" aria-live="polite"></ol>
<div class="lai-row"><button data-lai="place">付箋にしてボードに置く</button><button class="quiet" data-lai="clear">消す</button></div>
<label class="lai-check"><input type="checkbox" data-lai="auto"> 話すたびに付箋で置く</label>
<h3>手書きの文字</h3>
<p class="lai-note">手書きの線を選んで書式バーの「図形に整形」を押すと、図形と文字(英数字・かな・小学校で習う漢字)に整えます。読み間違えた字は、線を選んで正しい文字を教えると次から読めるようになります。</p>
<div class="lai-row"><input type="text" data-lai="teach-text" placeholder="選んだ手書きの正しい文字" aria-label="選んだ手書きの正しい文字"><button data-lai="teach">覚える</button></div>
<p class="lai-note" data-lai="hw-count"></p>
<div class="lai-row"><button class="quiet" data-lai="forget-hw">覚えた字を忘れる</button><button class="quiet" data-lai="forget-models">保存したモデルを消す</button></div>"#,
            cloud = if local_ai::cloud_off() {
                " checked"
            } else {
                ""
            },
        );
        self.panel.set_inner_html(&html);
    }

    /// 選んでいるモデルが保存済みか確かめて、表示に反映する。
    fn check_cached(&self) {
        let ui = self.clone();
        spawn_local(async move {
            let m = local_ai::model();
            let cached = local_ai::is_cached(m).await;
            ui.state.borrow_mut().cached = Some((m, cached));
            ui.update();
        });
    }

    /// 状態に合わせて表示を変える。
    fn update(&self) {
        let _ = self.toggle.set_attribute(
            "aria-pressed",
            if self.panel.hidden() { "false" } else { "true" },
        );
        if self.panel.hidden() {
            return;
        }
        let st = self.state.borrow();
        let model = local_ai::model();
        let speech = local_ai::speech();
        let (status, busy) = match (&st.error, &speech) {
            (Some(e), _) => (e.clone(), false),
            (_, Speech::Idle) if st.cached == Some((model, true)) => (
                "モデルはこの端末に保存済みです。ネットワークなしで使えます".into(),
                false,
            ),
            (_, Speech::Idle) => (
                format!(
                    "モデルは未取得です。初回だけ約 {} MB をダウンロードし、次からはこの端末に保存したものを使います",
                    match model {
                        Model::Tiny => 150,
                        Model::Base => 290,
                    }
                ),
                false,
            ),
            (_, Speech::Downloading(got, total)) if *total > 0.0 => (
                format!("モデルを取得中… {} / {}", fmt_mb(*got), fmt_mb(*total)),
                true,
            ),
            (_, Speech::Downloading(got, _)) => (format!("モデルを取得中… {}", fmt_mb(*got)), true),
            (_, Speech::Loading) => ("モデルを読み込み中…".into(), true),
            (_, Speech::Preparing(a)) => (
                format!("{}の準備中…(初回はしばらくかかります)", a.label()),
                true,
            ),
            (_, Speech::Ready(_)) if st.recording => (
                if st.pending > 0 {
                    "録音中・文字にしています…".into()
                } else {
                    "録音中です。話すと文字になります".into()
                },
                false,
            ),
            (_, Speech::Ready(_)) if st.pending > 0 => ("文字にしています…".into(), false),
            (_, Speech::Ready(_)) => (
                "準備できました。音声はこの端末の中だけで処理します".into(),
                false,
            ),
            (_, Speech::Failed(e)) => (e.clone(), false),
        };
        if let Some(el) = self.q::<HtmlElement>("status") {
            el.set_text_content(Some(&status));
            let failed = st.error.is_some() || matches!(speech, Speech::Failed(_));
            let _ = el.set_attribute("data-state", if failed { "error" } else { "" });
        }
        if let Some(bar) = self.q::<HtmlElement>("progress") {
            match speech {
                Speech::Downloading(got, total) if total > 0.0 => {
                    bar.set_hidden(false);
                    if let Some(inner) = bar
                        .first_element_child()
                        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
                    {
                        let _ = inner
                            .style()
                            .set_property("width", &format!("{:.1}%", got / total * 100.0));
                    }
                }
                _ => bar.set_hidden(true),
            }
        }
        if let Some(b) = self.q::<HtmlElement>("load") {
            let ready = speech == Speech::Ready(model);
            b.set_hidden(ready);
            let _ = b.toggle_attribute_with_force("disabled", busy);
        }
        if let Some(b) = self.q::<HtmlElement>("rec") {
            b.set_text_content(Some(if st.recording {
                "■ 録音を止める"
            } else {
                "● 録音して文字起こし"
            }));
            let _ = b.set_attribute("aria-pressed", if st.recording { "true" } else { "false" });
            let _ = b.toggle_attribute_with_force("disabled", busy);
        }
        if let Some(list) = self.q::<HtmlElement>("said") {
            let html: String = st
                .said
                .iter()
                .map(|s| {
                    format!(
                        r#"<li><time>{}</time> {}</li>"#,
                        escape(&s.time),
                        escape(&s.text)
                    )
                })
                .collect();
            list.set_inner_html(&html);
            list.set_scroll_top(list.scroll_height());
        }
        for name in ["place", "clear"] {
            if let Some(b) = self.q::<HtmlElement>(name) {
                let _ = b.toggle_attribute_with_force("disabled", st.said.is_empty());
            }
        }
        if let Some(el) = self.q::<HtmlElement>("accel-note") {
            let a = local_ai::accel_state();
            el.set_text_content(Some(if a.note.is_empty() {
                "NPU や GPU があれば、音声を読む重い部分をそこで動かします(内容は端末の外に出ません)。WebNN でどの装置を使うかは最終的にブラウザが決めます"
            } else {
                &a.note
            }));
            let _ = el.set_attribute("data-active", a.active.map_or("cpu", |x| x.id()));
        }
        if let Some(el) = self.q::<HtmlElement>("hw-count") {
            let n = local_ai::samples().len();
            el.set_text_content(Some(&if n == 0 {
                "まだ字を教えていません".to_string()
            } else {
                format!("教えた字: {n} 個(この端末に保存)")
            }));
        }
    }
}

impl App {
    /// 文字起こしを付箋にして置く。`index` 番目は前のものの下に並べる。
    fn place_transcript(&mut self, text: &str, index: usize) {
        if !self.can_edit() || text.trim().is_empty() {
            return;
        }
        let lines: usize = text
            .lines()
            .map(|l| l.chars().count().div_ceil(16).max(1))
            .sum();
        let height = (lines as f32 * 28.0 + 64.0).clamp(200.0, 1600.0);
        let center = self.view_center();
        let id = self.board.alloc_id();
        let pos = center
            + Vec2::new(-200.0, -100.0 + (index % 6) as f32 * 40.0)
            + Vec2::new((index / 6) as f32 * 440.0, 0.0);
        let mut note = Shape::sticky(id, pos, STICKY_COLORS[2]);
        note.size = Vec2::new(400.0, height);
        note.text = text.trim().to_string();
        self.board.end_gesture();
        self.board.push(note);
        self.board.end_gesture();
        self.editor.set_selection(vec![id]);
        self.dirty = true;
    }

    /// 選んだ手書きに正しい文字を教え、その文字に置き換える。
    fn teach_handwriting(&mut self, text: &str) -> Result<usize, String> {
        if !self.can_edit() {
            return Err("編集できないボードです".into());
        }
        let strokes: Vec<Stroke> = self.ink_strokes();
        if strokes.is_empty() {
            return Err("教える手書きの線を選んでください".into());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err("正しい文字を入れてください".into());
        }
        let pts: Vec<&[Vec2]> = strokes.iter().map(|s| s.points.as_slice()).collect();
        let rec = local_ai::recognizer();
        let samples = rec
            .teach(&pts, text)
            .ok_or("線を文字の数に区切れませんでした。1 行ずつ選んで教えてください")?;
        let n = samples.len();
        local_ai::add_samples(samples);
        let item = InkItem {
            strokes: strokes.iter().map(|s| s.id).collect(),
            kind: InkKind::Text(text.to_string()),
        };
        self.apply_ink(&[item], &strokes);
        Ok(n)
    }
}

/// 音声のかたまりを文字にする(結果は順に届く)。
fn transcribe(app: Rc<RefCell<App>>, ui: Ui, pcm: Vec<f32>, rate: f32) {
    ui.state.borrow_mut().pending += 1;
    ui.update();
    spawn_local(async move {
        let result = local_ai::transcribe(&pcm, rate, &local_ai::language()).await;
        let auto = ui
            .q::<HtmlInputElement>("auto")
            .is_some_and(|c| c.checked());
        let place = {
            let mut st = ui.state.borrow_mut();
            st.pending = st.pending.saturating_sub(1);
            match result {
                Ok(t) if !t.text.trim().is_empty() => {
                    log::info!("文字起こし {:.0} ms", t.ms);
                    st.said.push(Said {
                        time: now_label(),
                        text: t.text.clone(),
                    });
                    auto.then(|| {
                        st.placed += 1;
                        (t.text, st.placed - 1)
                    })
                }
                Ok(_) => None,
                Err(e) => {
                    st.error = Some(e);
                    None
                }
            }
        };
        if let Some((text, index)) = place {
            app.borrow_mut().place_transcript(&text, index);
        }
        ui.update();
    });
}

fn start_recording(app: Rc<RefCell<App>>, ui: Ui) {
    spawn_local(async move {
        ui.state.borrow_mut().error = None;
        if let Err(e) = local_ai::load(local_ai::model()).await {
            ui.state.borrow_mut().error = Some(e);
            ui.update();
            return;
        }
        let seg: Rc<RefCell<Option<Segmenter>>> = Rc::default();
        let seg2 = seg.clone();
        let (app2, ui2) = (app.clone(), ui.clone());
        let started = local_ai::start_mic(move |samples, rate| {
            let mut s = seg2.borrow_mut();
            let s = s.get_or_insert_with(|| Segmenter::new(rate));
            for chunk in s.push(&samples) {
                transcribe(app2.clone(), ui2.clone(), chunk, rate);
            }
        })
        .await;
        match started {
            Ok(()) => {
                ui.state.borrow_mut().recording = true;
                SEGMENTER.with(|s| *s.borrow_mut() = Some(seg));
            }
            Err(e) => ui.state.borrow_mut().error = Some(e),
        }
        ui.update();
    });
}

thread_local! {
    /// 録音中の切り出し(止めたときに話しかけの部分も文字にするため)。
    static SEGMENTER: RefCell<Option<Rc<RefCell<Option<Segmenter>>>>> = const { RefCell::new(None) };
}

fn stop_recording(app: Rc<RefCell<App>>, ui: Ui) {
    local_ai::stop_mic();
    ui.state.borrow_mut().recording = false;
    if let Some(seg) = SEGMENTER.with(|s| s.borrow_mut().take()) {
        let rest = seg
            .borrow_mut()
            .as_mut()
            .and_then(|s| s.flush().map(|pcm| (pcm, s.rate())));
        if let Some((pcm, rate)) = rest {
            transcribe(app, ui.clone(), pcm, rate);
        }
    }
    ui.update();
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    let ui = Ui {
        toggle: by_id(doc, "local-ai-toggle")?,
        panel: by_id(doc, "local-ai-panel")?,
        state: Rc::default(),
    };
    {
        let ui2 = ui.clone();
        local_ai::on_change(Rc::new(move || ui2.update()));
    }
    {
        let ui2 = ui.clone();
        listen(
            ui.toggle.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                let open = ui2.panel.hidden();
                if open && ui2.panel.child_element_count() == 0 {
                    ui2.build();
                }
                ui2.panel.set_hidden(!open);
                ui2.update();
                if open {
                    ui2.check_cached();
                }
            },
        )?;
    }
    {
        let ui2 = ui.clone();
        listen(
            ui.panel.as_ref(),
            "change",
            false,
            move |e: web_sys::Event| {
                let Some(el) = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                else {
                    return;
                };
                match el.get_attribute("data-lai").as_deref() {
                    Some("cloud-off") => {
                        if let Ok(c) = el.dyn_into::<HtmlInputElement>() {
                            local_ai::set_cloud_off(c.checked());
                        }
                    }
                    Some("model") => {
                        if let Some(m) = el
                            .dyn_into::<HtmlSelectElement>()
                            .ok()
                            .and_then(|s| Model::from_id(&s.value()))
                        {
                            local_ai::set_model(m);
                            ui2.check_cached();
                        }
                    }
                    Some("accel") => {
                        if let Some(p) = el
                            .dyn_into::<HtmlSelectElement>()
                            .ok()
                            .and_then(|s| AccelPref::from_id(&s.value()))
                        {
                            local_ai::set_accel_pref(p);
                            spawn_local(local_ai::reload_accel());
                        }
                    }
                    Some("lang") => {
                        if let Ok(s) = el.dyn_into::<HtmlSelectElement>() {
                            local_ai::set_language(&s.value());
                        }
                    }
                    _ => {}
                }
                ui2.update();
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
                let Some(el) = e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    .and_then(|t| t.closest("[data-lai]").ok().flatten())
                else {
                    return;
                };
                let doc = app.borrow().document.clone();
                match el.get_attribute("data-lai").as_deref() {
                    Some("close") => {
                        ui2.panel.set_hidden(true);
                        ui2.update();
                    }
                    Some("load") => {
                        let ui3 = ui2.clone();
                        ui3.state.borrow_mut().error = None;
                        spawn_local(async move {
                            if let Err(e) = local_ai::load(local_ai::model()).await {
                                ui3.state.borrow_mut().error = Some(e);
                            }
                            ui3.update();
                        });
                    }
                    Some("rec") => {
                        if ui2.state.borrow().recording {
                            stop_recording(app.clone(), ui2.clone());
                        } else {
                            start_recording(app.clone(), ui2.clone());
                        }
                    }
                    Some("place") => {
                        let text: Vec<String> = ui2
                            .state
                            .borrow()
                            .said
                            .iter()
                            .map(|s| s.text.clone())
                            .collect();
                        let index = {
                            let mut st = ui2.state.borrow_mut();
                            st.placed += 1;
                            st.placed - 1
                        };
                        app.borrow_mut().place_transcript(&text.join("\n"), index);
                    }
                    Some("clear") => {
                        ui2.state.borrow_mut().said.clear();
                        ui2.update();
                    }
                    Some("teach") => {
                        let Some(input) = ui2.q::<HtmlInputElement>("teach-text") else {
                            return;
                        };
                        let result = app.borrow_mut().teach_handwriting(&input.value());
                        match result {
                            Ok(n) => {
                                input.set_value("");
                                toast(
                                    &doc,
                                    &format!("{n} 字を覚えました。次から同じような字を読めます"),
                                    "done",
                                );
                            }
                            Err(e) => toast(&doc, &e, "error"),
                        }
                        ui2.update();
                    }
                    Some("forget-hw") => {
                        local_ai::clear_samples();
                        toast(&doc, "教えた字を忘れました", "done");
                        ui2.update();
                    }
                    Some("forget-models") => {
                        if ui2.state.borrow().recording {
                            stop_recording(app.clone(), ui2.clone());
                        }
                        let ui3 = ui2.clone();
                        spawn_local(async move {
                            local_ai::forget_models().await;
                            ui3.state.borrow_mut().cached = None;
                            ui3.update();
                            toast(&doc, "保存したモデルを消しました", "done");
                        });
                    }
                    _ => {}
                }
            },
        )?;
    }
    Ok(())
}
