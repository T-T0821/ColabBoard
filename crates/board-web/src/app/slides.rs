//! スライド表示: フレームを 1 枚ずつ全画面で見せる。
//!
//! - ヘッダーの「スライド」で始め、← → / Space / クリックで移動、Esc で終わる。
//! - 切り替えはカメラを補間して動かす([`board_core::slides::tween`])。
//! - 共同編集中は「プレゼン」も同時に始めるので、参加者の画面がスライドに追従する。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::slides::{self, Slide};
use board_core::{Camera, ElementId, Tool, Vec2};
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlElement, KeyboardEvent};

use super::{App, by_id, listen, now_ms, on_click};

/// 切り替えにかける時間(ミリ秒)。
const TRANSITION_MS: f64 = 520.0;
/// フレームの周りの余白(画面の短い辺に対する割合)。
const MARGIN: f32 = 0.04;
/// マウスを動かさなければ操作バーを隠すまでの時間(ミリ秒)。
const BAR_IDLE_MS: f64 = 2500.0;

struct Anim {
    from: Camera,
    start: f64,
}

pub struct Slides {
    document: Document,
    bar: HtmlElement,
    counter: HtmlElement,
    title: HtmlElement,
    prev: HtmlElement,
    next: HtmlElement,
    spot: HtmlElement,
    active: bool,
    list: Vec<Slide>,
    index: usize,
    /// 並びを作ったときのボードの版(変わったら作り直す)。
    revision: u64,
    anim: Option<Anim>,
    /// スライドのために自分でプレゼンを始めたか(終わるときに戻す)。
    started_presenting: bool,
    /// 全画面を頼んだか(終わるときに戻す)。
    fullscreen_requested: bool,
    /// 実際に全画面になったか(なった後でやめたらスライドも終わる)。
    fullscreen: bool,
    /// 最後にマウスを動かした時刻。
    poked_at: f64,
    bar_shown: Option<bool>,
    label: Option<(usize, usize, String)>,
    /// 「フレームがありません」を出している期限。
    notice_until: f64,
}

impl Slides {
    pub fn new(document: &Document) -> Result<Self, String> {
        Ok(Self {
            document: document.clone(),
            bar: by_id(document, "slide-bar")?,
            counter: by_id(document, "slide-counter")?,
            title: by_id(document, "slide-title")?,
            prev: by_id(document, "slide-prev")?,
            next: by_id(document, "slide-next")?,
            spot: by_id(document, "slide-spot")?,
            active: false,
            list: Vec::new(),
            index: 0,
            revision: u64::MAX,
            anim: None,
            started_presenting: false,
            fullscreen_requested: false,
            fullscreen: false,
            poked_at: 0.0,
            bar_shown: None,
            label: None,
            notice_until: 0.0,
        })
    }

    fn current_id(&self) -> Option<ElementId> {
        self.list.get(self.index).map(|s| s.id)
    }
}

fn reduced_motion() -> bool {
    web_sys::window()
        .and_then(|w| {
            w.match_media("(prefers-reduced-motion: reduce)")
                .ok()
                .flatten()
        })
        .is_some_and(|m| m.matches())
}

impl App {
    pub(super) fn slides_active(&self) -> bool {
        self.slides.active
    }

    fn start_slides(&mut self) {
        let now = now_ms(&self.window);
        let list = slides::slides(&self.board);
        if list.is_empty() {
            let s = &mut self.slides;
            s.notice_until = now + 3000.0;
            s.title.set_text_content(Some(
                "フレームがありません。フレーム (F) で囲んだ範囲が 1 枚のスライドになります",
            ));
            s.counter.set_text_content(Some(""));
            let _ = s.bar.set_attribute("data-state", "notice");
            let _ = s.bar.remove_attribute("hidden");
            s.label = None;
            return;
        }
        // 編集中のものは片付ける。
        self.end_text_edit(true);
        if self.editor.is_dragging() {
            self.editor.cancel(&mut self.board);
        }
        self.editor.clear_selection();
        self.editor.set_tool(Tool::Select);

        // 今見ている場所にあるフレームから始める。
        let (min, max) = self.camera.visible_world_rect();
        let center = (min + max) * 0.5;
        let start = list
            .iter()
            .position(|s| s.rect.contains(center))
            .unwrap_or(0);

        let s = &mut self.slides;
        s.active = true;
        s.revision = self.board.revision();
        s.list = list;
        s.index = start;
        s.anim = Some(Anim {
            from: self.camera,
            start: now,
        });
        s.poked_at = now;
        s.bar_shown = None;
        s.label = None;
        s.notice_until = 0.0;
        let _ = s.bar.set_attribute("data-state", "playing");
        let _ = s.bar.remove_attribute("hidden");
        let _ = s.spot.remove_attribute("hidden");
        if let Some(body) = self.document.body() {
            let _ = body.class_list().add_1("slideshow");
        }
        // 全画面(ブラウザが許さなければ、そのままウィンドウ内で表示する)。
        s.fullscreen = false;
        if let Some(root) = self.document.document_element() {
            s.fullscreen_requested = root.request_fullscreen().is_ok();
        }
        if !self.presenting() && self.set_presenting(true) {
            self.slides.started_presenting = true;
        }
        self.dirty = true;
    }

    pub(super) fn stop_slides(&mut self) {
        if !self.slides.active {
            return;
        }
        let s = &mut self.slides;
        s.active = false;
        s.anim = None;
        let _ = s.bar.set_attribute("hidden", "");
        let _ = s.spot.set_attribute("hidden", "");
        if let Some(body) = self.document.body() {
            let _ = body.class_list().remove_1("slideshow");
        }
        s.fullscreen = false;
        if std::mem::take(&mut s.fullscreen_requested)
            && self.document.fullscreen_element().is_some()
        {
            self.document.exit_fullscreen();
        }
        if std::mem::take(&mut self.slides.started_presenting) {
            self.set_presenting(false);
        }
        self.dirty = true;
    }

    fn go_slide(&mut self, index: usize) {
        let s = &mut self.slides;
        if !s.active || s.list.is_empty() {
            return;
        }
        let index = index.min(s.list.len() - 1);
        if index == s.index && s.anim.is_none() {
            return;
        }
        s.index = index;
        s.anim = Some(Anim {
            from: self.camera,
            start: now_ms(&self.window),
        });
        self.dirty = true;
    }

    fn next_slide(&mut self) {
        let i = self.slides.index + 1;
        self.go_slide(i);
    }

    fn prev_slide(&mut self) {
        let i = self.slides.index.saturating_sub(1);
        self.go_slide(i);
    }

    /// スライド中のキー操作。扱ったら `true`(スライド中はほかのショートカットを効かせない)。
    pub(super) fn slides_key(&mut self, e: &KeyboardEvent) -> bool {
        if !self.slides.active {
            return false;
        }
        self.poke_slides();
        match e.key().as_str() {
            "ArrowRight" | "ArrowDown" | "PageDown" | " " | "Enter" | "n" | "N" => {
                self.next_slide()
            }
            "ArrowLeft" | "ArrowUp" | "PageUp" | "Backspace" | "p" | "P" => self.prev_slide(),
            "Home" => self.go_slide(0),
            "End" => self.go_slide(usize::MAX),
            "Escape" => self.stop_slides(),
            // ブラウザの操作(再読み込みなど)はそのまま。
            _ if e.ctrl_key() || e.meta_key() => return false,
            _ => {}
        }
        true
    }

    /// スライド中のクリック: 左端の 1/4 なら前へ、それ以外は次へ(右クリックは前へ)。
    pub(super) fn slides_pointer_down(&mut self, p: Vec2, button: i16) -> bool {
        if !self.slides.active {
            return false;
        }
        self.poke_slides();
        if button == 2 || (button == 0 && p.x < self.camera.viewport.x * 0.25) {
            self.prev_slide();
        } else if button == 0 {
            self.next_slide();
        }
        true
    }

    /// マウスが動いた(操作バーを出す)。
    pub(super) fn poke_slides(&mut self) {
        if self.slides.active {
            self.slides.poked_at = now_ms(&self.window);
        }
    }

    /// 毎フレーム: カメラを動かし、操作バーを更新する。
    pub(super) fn slides_frame(&mut self, now: f64) {
        if !self.slides.active {
            if self.slides.notice_until > 0.0 && now > self.slides.notice_until {
                self.slides.notice_until = 0.0;
                let _ = self.slides.bar.set_attribute("hidden", "");
            }
            return;
        }
        // 全画面を(Esc などで)やめたらスライドも終わる。
        // (ブラウザが全画面を断ったときは、ウィンドウの中でそのまま続ける)
        let in_fullscreen = self.document.fullscreen_element().is_some();
        if in_fullscreen {
            self.slides.fullscreen = true;
        } else if self.slides.fullscreen {
            self.slides.fullscreen_requested = false;
            self.stop_slides();
            return;
        }
        // フレームが増えた・動いた・消えたら並びを作り直す(今のスライドは保つ)。
        if self.slides.revision != self.board.revision() {
            let current = self.slides.current_id();
            let list = slides::slides(&self.board);
            if list.is_empty() {
                self.stop_slides();
                return;
            }
            let s = &mut self.slides;
            s.index = current
                .and_then(|id| list.iter().position(|x| x.id == id))
                .unwrap_or(s.index.min(list.len() - 1));
            s.list = list;
            s.revision = self.board.revision();
            self.dirty = true;
        }

        let Some(slide) = self.slides.list.get(self.slides.index).cloned() else {
            return;
        };
        let target = slides::camera_for(&slide.rect, self.camera.viewport, MARGIN);
        match &self.slides.anim {
            Some(a) => {
                let t = if reduced_motion() {
                    1.0
                } else {
                    ((now - a.start) / TRANSITION_MS) as f32
                };
                self.camera = slides::tween(&a.from, &target, t);
                if t >= 1.0 {
                    self.slides.anim = None;
                }
                self.dirty = true;
            }
            None => {
                // 画面の大きさが変わってもフレームに合わせ続ける。
                if self.camera != target {
                    self.camera = target;
                    self.dirty = true;
                }
            }
        }

        // 周りを暗くする(フレームの位置に穴を開ける)。
        if self.dirty {
            let dpr = self.window.device_pixel_ratio() as f32;
            let a = self.camera.world_to_screen(slide.rect.min) / dpr;
            let b = self.camera.world_to_screen(slide.rect.max) / dpr;
            let css = self.slides.spot.style();
            let _ = css.set_property("left", &format!("{}px", a.x));
            let _ = css.set_property("top", &format!("{}px", a.y));
            let _ = css.set_property("width", &format!("{}px", (b.x - a.x).max(0.0)));
            let _ = css.set_property("height", &format!("{}px", (b.y - a.y).max(0.0)));
        }

        let s = &mut self.slides;
        let label = (s.index, s.list.len(), slide.title.clone());
        if s.label.as_ref() != Some(&label) {
            s.counter
                .set_text_content(Some(&format!("{} / {}", label.0 + 1, label.1)));
            let title = if label.2.is_empty() {
                "無題のフレーム"
            } else {
                &label.2
            };
            s.title.set_text_content(Some(title));
            set_disabled(&s.prev, label.0 == 0);
            set_disabled(&s.next, label.0 + 1 >= label.1);
            s.label = Some(label);
        }
        let show = now - s.poked_at < BAR_IDLE_MS;
        if s.bar_shown != Some(show) {
            s.bar_shown = Some(show);
            let _ = s
                .bar
                .set_attribute("data-idle", if show { "false" } else { "true" });
            if let Some(body) = s.document.body() {
                let _ = body
                    .style()
                    .set_property("cursor", if show { "" } else { "none" });
            }
        }
    }
}

fn set_disabled(el: &HtmlElement, disabled: bool) {
    if disabled {
        let _ = el.set_attribute("disabled", "");
    } else {
        let _ = el.remove_attribute("disabled");
    }
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    on_click(doc, "slides-start", app, |a| a.start_slides())?;
    on_click(doc, "slide-prev", app, |a| a.prev_slide())?;
    on_click(doc, "slide-next", app, |a| a.next_slide())?;
    on_click(doc, "slide-exit", app, |a| a.stop_slides())?;
    let window = web_sys::window().ok_or("window がありません")?;
    let app = app.clone();
    listen(
        window.unchecked_ref(),
        "mousemove",
        true,
        move |_: web_sys::MouseEvent| {
            if let Ok(mut a) = app.try_borrow_mut() {
                a.poke_slides();
            }
        },
    )
}
