//! ワークショップの進行: 個人作業エリア、匿名モード、ブレイクアウト(小部屋)。
//!
//! - 個人作業エリア: 持ち主以外には中身を伏せて描き、選択・編集・検索・書き出しからも外す。
//! - 匿名モード: 全員の画面で他の人のカーソルを灰色にし、名前を隠す。
//! - ブレイクアウト: 部屋(フレーム)を作り、各自が人の少ない部屋に自動で入って画面を移す。
//!   部屋の外にいる人のカーソルは出さない。「全員を戻す」でボード全体の表示に戻る。

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::rc::Rc;

use board_core::geom::Rect;
use board_core::{ElementId, Vec2, privacy};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, HtmlInputElement};

use super::{App, by_id, listen, on_click};

pub struct Workshop {
    area_status: HtmlElement,
    anon_toggle: HtmlInputElement,
    room_count: HtmlInputElement,
    breakout_status: HtmlElement,
    room_list: HtmlElement,
    body: HtmlElement,
    /// 隠す要素を計算したときのボードの版。
    hidden_rev: Option<u64>,
    hidden: HashSet<ElementId>,
    last_area_status: String,
    last_anon: Option<bool>,
    last_rooms: String,
    /// 自動で部屋に入ろうとした回(書き込めない人が何度も試さないように)。
    tried_round: Option<String>,
    /// 画面を部屋へ移した回。
    jumped_round: Option<String>,
    /// 自分で部屋を選んだ回(自動の振り直しをしない)。
    manual_round: Option<String>,
}

impl Workshop {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            area_status: by_id(document, "area-status")?,
            anon_toggle: by_id(document, "anon-toggle")?,
            room_count: by_id(document, "room-count")?,
            breakout_status: by_id(document, "breakout-status")?,
            room_list: by_id(document, "room-list")?,
            body: document.body().ok_or("body がありません")?,
            hidden_rev: None,
            hidden: HashSet::new(),
            last_area_status: String::new(),
            last_anon: None,
            last_rooms: String::new(),
            tried_round: None,
            jumped_round: None,
            manual_round: None,
        })
    }

    fn room_count(&self) -> usize {
        self.room_count
            .value()
            .trim()
            .parse::<usize>()
            .unwrap_or(3)
            .clamp(1, board_core::workshop::MAX_ROOMS)
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl App {
    /// 毎フレーム: 共有状態の変化を画面に反映する。
    pub(super) fn workshop_frame(&mut self) {
        self.sync_privacy();
        self.sync_anonymous();
        self.sync_breakout();
    }

    fn sync_privacy(&mut self) {
        let rev = self.board.revision();
        if self.workshop.hidden_rev == Some(rev) {
            return;
        }
        self.workshop.hidden_rev = Some(rev);
        let me = self.identity.id.clone();
        let hidden = privacy::hidden_ids(&self.board, &me);
        if hidden != self.workshop.hidden {
            let sel: Vec<ElementId> = self
                .editor
                .selection()
                .iter()
                .copied()
                .filter(|id| !hidden.contains(id))
                .collect();
            if sel.len() != self.editor.selection().len() {
                self.editor.set_selection(sel);
            }
            self.editor.hidden = hidden.clone();
            self.renderer.set_hidden(&hidden);
            self.workshop.hidden = hidden;
            self.dirty = true;
        }
        let areas = self.board.personal_areas();
        let mine = areas.iter().filter(|(_, o)| *o == me).count();
        let others = areas.len() - mine;
        let status = match (mine, others) {
            (0, 0) => String::new(),
            (m, 0) => format!("自分のエリア {m} つ(中身はあなたにだけ見えます)"),
            (0, o) => format!("ほかの人のエリア {o} つ(中身は伏せて表示)"),
            (m, o) => format!("自分のエリア {m} つ・ほかの人のエリア {o} つ"),
        };
        if status != self.workshop.last_area_status {
            self.workshop.area_status.set_text_content(Some(&status));
            self.workshop.last_area_status = status;
        }
    }

    /// 隠れている要素(検索・書き出しから外す)。
    pub(super) fn hidden_elements(&self) -> &HashSet<ElementId> {
        &self.workshop.hidden
    }

    fn sync_anonymous(&mut self) {
        let anon = self.board.anonymous();
        if self.workshop.last_anon == Some(anon) {
            return;
        }
        self.workshop.last_anon = Some(anon);
        self.workshop.anon_toggle.set_checked(anon);
        self.renderer.set_show_authors(self.show_authors && !anon);
        self.dirty = true;
        let _ = self
            .workshop
            .body
            .class_list()
            .toggle_with_force("anonymous", anon);
    }

    fn sync_breakout(&mut self) {
        let me = self.identity.id.clone();
        let Some(b) = self.board.breakout() else {
            if self.workshop.jumped_round.take().is_some() {
                // 小部屋が終わったら全体を表示する。
                self.fit();
            }
            self.workshop.tried_round = None;
            self.set_cursor_area(None);
            if !self.workshop.last_rooms.is_empty() {
                self.workshop.last_rooms.clear();
                self.workshop.breakout_status.set_text_content(None);
                self.workshop.room_list.set_inner_html("");
            }
            return;
        };
        let members = self.board.breakout_members();
        let mut mine = members.iter().find(|m| m.user == me).map(|m| m.room);
        if mine.is_none() && self.workshop.tried_round.as_deref() != Some(b.round.as_str()) {
            self.workshop.tried_round = Some(b.round.clone());
            if let Some(r) = self.board.suggest_room(&me) {
                let name = self.identity.name.clone();
                if self.board.join_room(&me, &name, r) {
                    mine = Some(r);
                }
            }
        }
        // 同時に入って偏ったら直す(自分で部屋を選んだ人は動かさない)。
        if mine.is_some()
            && self.workshop.manual_round.as_deref() != Some(b.round.as_str())
            && let Some(r) = self.board.rebalance_room(&me)
        {
            let name = self.identity.name.clone();
            if self.board.join_room(&me, &name, r) {
                mine = Some(r);
                if self.workshop.jumped_round.as_deref() == Some(b.round.as_str()) {
                    self.show_room(r);
                }
            }
        }
        if let Some(r) = mine
            && self.workshop.jumped_round.as_deref() != Some(b.round.as_str())
        {
            self.workshop.jumped_round = Some(b.round.clone());
            self.show_room(r);
        }
        self.set_cursor_area(mine.and_then(|r| self.board.room_bounds(r)));

        // 部屋の一覧(変わったときだけ作り直す)。
        let mut sig = format!("{}|{:?}", b.round, mine);
        for m in &members {
            let _ = write!(sig, "|{}:{}:{}", m.room, m.user, m.name);
        }
        if sig == self.workshop.last_rooms {
            return;
        }
        self.workshop.last_rooms = sig;
        let status = match mine {
            Some(r) => format!(
                "{} にいます。部屋の外の人のカーソルは表示しません",
                b.rooms[r].name
            ),
            None => "部屋を選んでください".to_string(),
        };
        self.workshop
            .breakout_status
            .set_text_content(Some(&status));
        let mut html = String::new();
        for (i, room) in b.rooms.iter().enumerate() {
            let names: Vec<String> = members
                .iter()
                .filter(|m| m.room == i)
                .map(|m| {
                    if m.name.is_empty() {
                        "参加者".to_string()
                    } else {
                        m.name.clone()
                    }
                })
                .collect();
            let here = mine == Some(i);
            let _ = write!(
                html,
                r#"<li{}><span><strong>{}</strong> {}</span><button data-room="{i}">{}</button></li>"#,
                if here { r#" data-here="""# } else { "" },
                esc(&room.name),
                if names.is_empty() {
                    "(だれもいません)".to_string()
                } else {
                    esc(&names.join("、"))
                },
                if here { "表示" } else { "移る" },
            );
        }
        self.workshop.room_list.set_inner_html(&html);
    }

    fn set_cursor_area(&mut self, area: Option<Rect>) {
        if self.cursors.area != area {
            self.cursors.area = area;
            self.cursors_dirty = true;
        }
    }

    fn show_room(&mut self, room: usize) {
        if let Some(r) = self.board.room_bounds(room) {
            self.camera.fit(r.min, r.max, 64.0);
            self.dirty = true;
        }
    }

    fn create_area(&mut self) {
        if !self.can_edit() {
            return;
        }
        let me = self.identity.id.clone();
        // すでにあればそこを表示する。
        if let Some((id, _)) = self
            .board
            .personal_areas()
            .into_iter()
            .find(|(_, o)| *o == me)
            && let Some(r) = self.board.bounds_of(&[id])
        {
            self.camera.fit(r.min, r.max, 96.0);
            self.dirty = true;
            return;
        }
        let center = self.camera.screen_to_world(self.camera.viewport * 0.5);
        let name = if self.identity.name.is_empty() {
            "自分".to_string()
        } else {
            self.identity.name.clone()
        };
        self.board.end_gesture();
        let id = self
            .board
            .add_personal_area(&me, &name, center - privacy::AREA_SIZE * 0.5);
        self.board.end_gesture();
        self.editor.set_selection(vec![id]);
        self.dirty = true;
    }

    fn reveal_areas(&mut self, all: bool) {
        if !self.can_edit() {
            return;
        }
        let me = self.identity.id.clone();
        self.board.end_gesture();
        let n = self.board.reveal_areas((!all).then_some(me.as_str()));
        self.board.end_gesture();
        if n == 0 {
            self.workshop
                .area_status
                .set_text_content(Some("公開するエリアがありません"));
            self.workshop.last_area_status.clear();
        }
        self.dirty = true;
    }

    fn start_breakout(&mut self) {
        if !self.can_edit() {
            return;
        }
        let n = self.workshop.room_count();
        let origin = match self.board.bounds() {
            Some(r) => Vec2::new(r.max.x + 400.0, r.min.y),
            None => self.camera.screen_to_world(Vec2::ZERO),
        };
        self.board.end_gesture();
        self.board.start_breakout(n, origin);
        self.board.end_gesture();
        self.dirty = true;
    }

    fn end_breakout(&mut self) {
        if self.board.breakout().is_some() {
            self.board.end_breakout();
        }
    }

    fn move_to_room(&mut self, room: usize) {
        let me = self.identity.id.clone();
        if self.board.my_room(&me) != Some(room) {
            let name = self.identity.name.clone();
            self.board.join_room(&me, &name, room);
            self.workshop.manual_round = self.board.breakout().map(|b| b.round);
        }
        self.show_room(room);
    }
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    on_click(doc, "area-create", app, |a| a.create_area())?;
    on_click(doc, "area-reveal", app, |a| a.reveal_areas(false))?;
    on_click(doc, "area-reveal-all", app, |a| a.reveal_areas(true))?;
    on_click(doc, "breakout-start", app, |a| a.start_breakout())?;
    on_click(doc, "breakout-end", app, |a| a.end_breakout())?;
    {
        let toggle: HtmlInputElement = by_id(doc, "anon-toggle")?;
        let t2 = toggle.clone();
        let app = app.clone();
        listen(
            toggle.as_ref(),
            "change",
            false,
            move |_: web_sys::Event| {
                let mut a = app.borrow_mut();
                if a.can_edit() {
                    a.board.set_anonymous(t2.checked());
                } else {
                    t2.set_checked(a.board.anonymous());
                }
            },
        )?;
    }
    let list: HtmlElement = by_id(doc, "room-list")?;
    let app = app.clone();
    listen(
        list.as_ref(),
        "click",
        false,
        move |e: web_sys::MouseEvent| {
            let room = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|el| el.closest("[data-room]").ok().flatten())
                .and_then(|el| el.get_attribute("data-room"))
                .and_then(|r| r.parse::<usize>().ok());
            if let Some(room) = room {
                app.borrow_mut().move_to_room(room);
            }
        },
    )
}
