//! 会議の進行を助ける機能の UI: 全員に見えるタイマーと、付箋への投票。
//!
//! 状態はボードの共有ドキュメント(`board_core::session`)にあるので、ここは表示と操作だけ。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use board_core::arrange::{self, Arrange};
use board_core::session::{VoteSession, VoteTally, format_remaining};
use board_core::{Board, Camera, ElementId, Vec2};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, HtmlInputElement};

use super::{App, by_id, js_err, listen, on_click};

/// 投票の集計を見直す間隔(ミリ秒)。
const TALLY_INTERVAL_MS: f64 = 200.0;
/// 結果一覧に出す件数。
const MAX_RESULTS: usize = 10;

pub struct Collab {
    document: Document,
    timer: HtmlElement,
    timer_text: HtmlElement,
    timer_toggle: HtmlElement,
    vote_status: HtmlElement,
    vote_results: HtmlElement,
    vote_limit: HtmlInputElement,
    badge_layer: HtmlElement,
    badges: HashMap<ElementId, HtmlElement>,
    last_timer: Option<(String, &'static str)>,
    /// 動いているのを見たタイマーの終了時刻(終わったら 1 回だけ鳴らす)。
    running_end: Option<f64>,
    last_vote: Option<(Option<VoteSession>, VoteTally)>,
    tally_at: f64,
    tally_revision: u64,
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

impl Collab {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            document: document.clone(),
            timer: by_id(document, "timer")?,
            timer_text: by_id(document, "timer-text")?,
            timer_toggle: by_id(document, "timer-toggle")?,
            vote_status: by_id(document, "vote-status")?,
            vote_results: by_id(document, "vote-results")?,
            vote_limit: by_id(document, "vote-limit")?,
            badge_layer: by_id(document, "badges")?,
            badges: HashMap::new(),
            last_timer: None,
            running_end: None,
            last_vote: None,
            tally_at: f64::NEG_INFINITY,
            tally_revision: u64::MAX,
        })
    }

    /// 投票を受け付けているか。
    pub fn voting(board: &Board) -> bool {
        board.vote_session().is_some_and(|v| v.open)
    }

    /// タイマー表示を更新する。終わった瞬間に音を鳴らす。
    fn update_timer(&mut self, board: &Board) {
        let now = now_ms();
        let state = board.timer().map(|t| {
            let text = format_remaining(t.remaining_ms(now));
            let mode = if t.is_paused() {
                "paused"
            } else if t.is_finished(now) {
                "done"
            } else {
                "running"
            };
            if mode == "running" {
                self.running_end = Some(t.ends_at_ms);
            } else if mode == "done" && self.running_end == Some(t.ends_at_ms) {
                self.running_end = None;
                beep();
            }
            (text, mode)
        });
        if state == self.last_timer {
            return;
        }
        match &state {
            Some((text, mode)) => {
                self.timer_text.set_text_content(Some(text));
                let _ = self.timer.set_attribute("data-state", mode);
                self.timer_toggle
                    .set_text_content(Some(if *mode == "paused" {
                        "再開"
                    } else {
                        "一時停止"
                    }));
                self.timer.remove_attribute("hidden").ok();
            }
            None => {
                let _ = self.timer.set_attribute("hidden", "");
            }
        }
        self.last_timer = state;
    }

    /// 投票の状態が変わっていたら表示を作り直す。作り直したら true(バッジの配置が必要)。
    fn update_votes(&mut self, board: &Board, user: &str, force: bool) -> bool {
        let now = now_ms();
        if !force
            && now - self.tally_at < TALLY_INTERVAL_MS
            && board.revision() == self.tally_revision
        {
            return false;
        }
        self.tally_at = now;
        self.tally_revision = board.revision();
        let state = (board.vote_session(), board.vote_tally(user));
        if self.last_vote.as_ref() == Some(&state) {
            return false;
        }
        let (session, tally) = &state;

        let status = match session {
            None => String::new(),
            Some(v) if v.open => format!(
                "付箋をクリックで投票(Shift+クリックで取り消し)。残り {} / {} 票 · {}人が投票中",
                v.limit.saturating_sub(tally.used),
                v.limit,
                tally.voters
            ),
            Some(_) => format!("締め切りました · {}人が投票", tally.voters),
        };
        self.vote_status.set_text_content(Some(&status));
        let _ = self.vote_status.set_attribute(
            "data-state",
            match session {
                None => "none",
                Some(v) if v.open => "open",
                Some(_) => "closed",
            },
        );

        // 結果は締め切ってから見せる(他の人の票に引っ張られないように)。
        let closed = session.as_ref().is_some_and(|v| !v.open);
        self.vote_results.set_inner_html("");
        if closed {
            for (rank, (id, n)) in tally.totals.iter().take(MAX_RESULTS).enumerate() {
                let text = board.get(*id).map(|e| e.text()).unwrap_or("");
                let snippet: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let snippet: String = snippet.chars().take(40).collect();
                let label = if snippet.is_empty() {
                    "(空の付箋)".to_string()
                } else {
                    snippet
                };
                let Ok(li) = self.document.create_element("li") else {
                    continue;
                };
                let Ok(btn) = self.document.create_element("button") else {
                    continue;
                };
                let _ = btn.set_attribute("data-id", &format!("{:x}", id.0));
                let _ = btn.set_attribute("title", "この付箋へ移動");
                btn.set_text_content(Some(&format!("{}. {label}", rank + 1)));
                if let Ok(count) = self.document.create_element("span") {
                    count.set_class_name("votes");
                    count.set_text_content(Some(&format!("{n}票")));
                    let _ = btn.append_child(&count);
                }
                let _ = li.append_child(&btn);
                let _ = self.vote_results.append_child(&li);
            }
        }

        // バッジ: 受付中は自分の票、締め切り後は合計。
        let shown: Vec<(ElementId, u32, &str)> = match session {
            Some(v) if v.open => tally.mine.iter().map(|(id, n)| (*id, *n, "mine")).collect(),
            Some(_) => tally
                .totals
                .iter()
                .map(|(id, n)| (*id, *n, "total"))
                .collect(),
            None => Vec::new(),
        };
        self.badges.retain(|id, el| {
            let keep = shown.iter().any(|(i, ..)| i == id);
            if !keep {
                el.remove();
            }
            keep
        });
        for (id, n, kind) in shown {
            let el = match self.badges.get(&id) {
                Some(el) => el.clone(),
                None => {
                    let Some(el) = self
                        .document
                        .create_element("div")
                        .ok()
                        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
                    else {
                        continue;
                    };
                    if self.badge_layer.append_child(&el).is_err() {
                        continue;
                    }
                    self.badges.insert(id, el.clone());
                    el
                }
            };
            el.set_class_name(&format!("vote-badge {kind}"));
            el.set_text_content(Some(&n.to_string()));
        }

        self.last_vote = Some(state);
        true
    }

    /// バッジを付箋の右上に置く。`origin` は canvas 左上(CSS ピクセル)。
    pub fn layout(&self, board: &Board, camera: &Camera, dpr: f32, origin: Vec2) {
        for (id, el) in &self.badges {
            let style = el.style();
            let Some(e) = board.get(*id) else {
                let _ = style.set_property("display", "none");
                continue;
            };
            let b = e.bounds();
            let p = camera.world_to_screen(Vec2::new(b.max.x, b.min.y)) / dpr + origin;
            let _ = style.set_property("display", "block");
            let _ = style.set_property("transform", &format!("translate({}px, {}px)", p.x, p.y));
        }
    }

    pub fn vote_limit(&self) -> u32 {
        self.vote_limit
            .value()
            .trim()
            .parse::<u32>()
            .unwrap_or(3)
            .clamp(1, board_core::session::MAX_VOTES)
    }
}

/// 終了の合図(短いビープ 2 回)。音が出せない環境では何もしない。
fn beep() {
    let Ok(ctx) = web_sys::AudioContext::new() else {
        return;
    };
    let t0 = ctx.current_time();
    for i in 0..2 {
        let (Ok(osc), Ok(gain)) = (ctx.create_oscillator(), ctx.create_gain()) else {
            return;
        };
        osc.frequency().set_value(880.0);
        let start = t0 + i as f64 * 0.25;
        let g = gain.gain();
        g.set_value(0.0);
        let _ = g.set_value_at_time(0.0, start);
        let _ = g.linear_ramp_to_value_at_time(0.2, start + 0.01);
        let _ = g.linear_ramp_to_value_at_time(0.0, start + 0.18);
        let _ = osc.connect_with_audio_node(&gain);
        let _ = gain.connect_with_audio_node(&ctx.destination());
        let _ = osc.start_with_when(start);
        let _ = osc.stop_with_when(start + 0.2);
    }
}

impl App {
    /// 毎フレーム呼ぶ。
    pub(super) fn collab_frame(&mut self) {
        self.collab.update_timer(&self.board);
        if self
            .collab
            .update_votes(&self.board, &self.identity.id, false)
        {
            self.layout_collab();
        }
    }

    pub(super) fn layout_collab(&self) {
        let rect = self.canvas.get_bounding_client_rect();
        let dpr = self.window.device_pixel_ratio() as f32;
        self.collab.layout(
            &self.board,
            &self.camera,
            dpr,
            Vec2::new(rect.left() as f32, rect.top() as f32),
        );
    }

    /// 投票中なら、付箋のクリックを投票として扱う。扱ったら true。
    pub(super) fn collab_pointer_down(&mut self, screen: Vec2, shift: bool) -> bool {
        if !Collab::voting(&self.board) {
            return false;
        }
        let world = self.camera.screen_to_world(screen);
        let Some(id) = self
            .board
            .hit_test(world, 0.0)
            .map(|e| e.id())
            .filter(|id| self.board.is_votable(*id))
        else {
            return false;
        };
        let user = self.identity.id.clone();
        if shift {
            self.board.retract_vote(&user, id);
        } else {
            self.board.cast_vote(&user, id);
        }
        self.collab.update_votes(&self.board, &user, true);
        self.layout_collab();
        true
    }

    /// 要素が画面に入るように移動して選択する。
    fn focus_element(&mut self, id: ElementId) {
        let Some(b) = self.board.get(id).map(|e| e.bounds()) else {
            return;
        };
        let pad = b.size().max_element() * 1.5;
        self.camera.fit(b.min - pad, b.max + pad, 48.0);
        self.editor.set_selection(&self.board, &[id]);
        self.dirty = true;
    }

    /// 付箋を自動で並べ直す。
    fn arrange(&mut self, mode: Arrange) {
        if self.editor.is_dragging() {
            return;
        }
        let ids = arrange::targets(&self.board, self.editor.selection());
        if arrange::arrange(&mut self.board, &ids, mode) > 0 {
            self.dirty = true;
        }
    }

    fn refresh_votes(&mut self) {
        let user = self.identity.id.clone();
        self.collab.update_votes(&self.board, &user, true);
        self.layout_collab();
    }
}

/// パネルのボタンをつなぐ。
pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    let buttons = doc.query_selector_all("[data-timer-min]").map_err(js_err)?;
    for i in 0..buttons.length() {
        let Some(el) = buttons.item(i).and_then(|n| n.dyn_into::<Element>().ok()) else {
            continue;
        };
        let Some(min) = el
            .get_attribute("data-timer-min")
            .and_then(|m| m.parse::<f64>().ok())
        else {
            continue;
        };
        let app = app.clone();
        listen(
            el.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                app.borrow_mut().board.start_timer(min * 60_000.0, now_ms());
            },
        )?;
    }
    on_click(doc, "timer-toggle", app, |a| {
        a.board.toggle_timer_pause(now_ms())
    })?;
    on_click(doc, "timer-add", app, |a| {
        a.board.extend_timer(60_000.0, now_ms())
    })?;
    on_click(doc, "timer-stop", app, |a| a.board.stop_timer())?;

    on_click(doc, "vote-start", app, |a| {
        let limit = a.collab.vote_limit();
        a.board.start_vote(limit);
        a.refresh_votes();
    })?;
    on_click(doc, "vote-close", app, |a| {
        a.board.close_vote();
        a.refresh_votes();
    })?;
    on_click(doc, "vote-clear", app, |a| {
        a.board.clear_vote();
        a.refresh_votes();
    })?;
    on_click(doc, "arrange-grid", app, |a| a.arrange(Arrange::Grid))?;
    on_click(doc, "arrange-color", app, |a| a.arrange(Arrange::ByColor))?;
    {
        let results: HtmlElement = by_id(doc, "vote-results")?;
        let app = app.clone();
        listen(
            results.as_ref(),
            "click",
            false,
            move |e: web_sys::MouseEvent| {
                let id = e
                    .target()
                    .and_then(|t| t.dyn_into::<Element>().ok())
                    .and_then(|el| el.closest("[data-id]").ok().flatten())
                    .and_then(|el| el.get_attribute("data-id"))
                    .and_then(|h| u64::from_str_radix(&h, 16).ok());
                if let Some(id) = id {
                    app.borrow_mut().focus_element(ElementId(id));
                }
            },
        )?;
    }
    Ok(())
}
