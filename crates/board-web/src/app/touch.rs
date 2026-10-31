//! スマホ・タブレットの操作: 指とペン(Apple Pencil など)。
//!
//! - 指は当たり判定を広げ、少し動くまではドラッグとみなさない(タップのぶれで要素が動かない)。
//! - 選択ツールで何もない所を指でなぞると画面を動かす(2 本指ならピンチズーム)。何もない所のタップで選択解除。
//! - 指の長押しで右クリックメニューを開く(iOS Safari は `contextmenu` を出さないので自前で判定する)。
//! - 指・ペンのダブルタップで文字入力を始める。
//! - ペンを使い始めたら、ペン・蛍光ペン・消しゴムでは指は画面移動になる。ペンが触れている間の指(手のひら)は無視する。
//! - ソフトウェアキーボードが出たら、編集中の図形が隠れないよう画面を動かす。

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use board_core::{Tool, Vec2};
use wasm_bindgen::JsCast;
use web_sys::PointerEvent;

use super::{App, listen, now_ms};

/// これ以上動いたらタップではなくドラッグ(CSS px)。
const TAP_SLOP_CSS: f32 = 8.0;
/// 長押しとみなすまでの時間(ミリ秒)。
const LONG_PRESS_MS: f64 = 500.0;
/// ダブルタップの間隔(ミリ秒)と距離(CSS px)。
const DOUBLE_TAP_MS: f64 = 350.0;
const DOUBLE_TAP_CSS: f32 = 30.0;
/// 指で触るときの当たり判定の大きさ(CSS px 換算での倍率)。
const TOUCH_HIT_SCALE: f32 = 2.5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mouse,
    Pen,
    Touch,
}

impl Kind {
    fn of(e: &PointerEvent) -> Self {
        match e.pointer_type().as_str() {
            "touch" => Kind::Touch,
            "pen" => Kind::Pen,
            _ => Kind::Mouse,
        }
    }
}

/// 1 本目のポインタの押し始め。
struct Press {
    id: i32,
    kind: Kind,
    /// 物理ピクセル。
    start: Vec2,
    /// CSS px(メニューを開く位置)。
    client: (i32, i32),
    time: f64,
    /// タップの許容範囲を超えて動いたか。
    moved: bool,
    /// 選択ツールで何もない所を押したか(タップなら選択解除)。
    on_empty: bool,
}

impl Touch {
    /// 無視していた指なら忘れて `true`。
    pub(super) fn ignored_remove(&mut self, id: i32) -> bool {
        self.ignored.remove(&id)
    }
}

#[derive(Default)]
pub(super) struct Touch {
    press: Option<Press>,
    last_tap: Option<(f64, Vec2)>,
    /// 触れているペン。
    pen_down: Option<i32>,
    /// このページでペンを使ったことがあるか。
    pen_seen: bool,
    /// 無視している指(ペンを使っている間に触れた手のひら)。
    ignored: HashSet<i32>,
    /// 長押しで開くメニューの位置(CSS px)。
    pending_menu: Option<(i32, i32)>,
    /// 編集を始めたのが指かペンか(キーボードで隠れないよう画面を動かす)。
    edit_by_touch: bool,
}

impl App {
    fn dpr(&self) -> f32 {
        self.window.device_pixel_ratio() as f32
    }

    /// ポインタが押されたとき最初に呼ぶ。`false` なら無視する(手のひら)。
    pub(super) fn touch_pointer_down(&mut self, e: &PointerEvent, p: Vec2) -> bool {
        let kind = Kind::of(e);
        let id = e.pointer_id();
        match kind {
            Kind::Pen => {
                self.touch.pen_down = Some(id);
                self.touch.pen_seen = true;
            }
            Kind::Touch if self.touch.pen_down.is_some() => {
                self.touch.ignored.insert(id);
                return false;
            }
            _ => {}
        }
        self.touch.edit_by_touch = kind != Kind::Mouse;
        let scale = if kind == Kind::Touch {
            self.dpr() * TOUCH_HIT_SCALE
        } else {
            1.0
        };
        self.editor.set_hit_scale(scale);
        // 2 本目以降の指が来たらタップ・長押しではない。
        self.touch.press = self.pointers.is_empty().then(|| Press {
            id,
            kind,
            start: p,
            client: (e.client_x(), e.client_y()),
            time: now_ms(&self.window),
            moved: false,
            on_empty: false,
        });
        true
    }

    /// 指で押したとき、編集ではなく画面移動にするか。
    pub(super) fn touch_wants_nav(&mut self, e: &PointerEvent, p: Vec2) -> bool {
        if Kind::of(e) != Kind::Touch || self.editor.is_dragging() {
            return false;
        }
        match self.editor.tool() {
            Tool::Select => {
                let empty = self.editor.hits_nothing(&self.board, &self.camera, p);
                if let Some(press) = &mut self.touch.press {
                    press.on_empty = empty;
                }
                empty
            }
            Tool::Pen | Tool::Highlighter | Tool::Eraser => self.touch.pen_seen,
            _ => false,
        }
    }

    pub(super) fn touch_ignored(&self, id: i32) -> bool {
        self.touch.ignored.contains(&id)
    }

    /// 動いた量を記録する。`false` ならまだタップの範囲内なので、編集のドラッグには渡さない。
    pub(super) fn touch_pointer_move(&mut self, id: i32, p: Vec2) -> bool {
        let slop = TAP_SLOP_CSS * self.dpr();
        match &mut self.touch.press {
            Some(press) if press.id == id => {
                if !press.moved && (p - press.start).length() > slop {
                    press.moved = true;
                }
                press.moved || press.kind == Kind::Mouse
            }
            _ => true,
        }
    }

    /// ポインタが離れたときの後始末。タップとダブルタップを扱う。
    pub(super) fn touch_pointer_up(&mut self, e: &PointerEvent, p: Vec2) {
        let id = e.pointer_id();
        if self.touch.pen_down == Some(id) {
            self.touch.pen_down = None;
        }
        let Some(press) = self.touch.press.take_if(|press| press.id == id) else {
            return;
        };
        let now = now_ms(&self.window);
        let tap = !press.moved && e.type_() == "pointerup";
        if !tap || press.kind == Kind::Mouse {
            return;
        }
        if press.on_empty {
            self.editor.clear_selection();
            self.dirty = true;
        }
        let near = DOUBLE_TAP_CSS * self.dpr();
        let double = self
            .touch
            .last_tap
            .is_some_and(|(t, q)| now - t < DOUBLE_TAP_MS && (q - p).length() < near);
        if double {
            self.touch.last_tap = None;
            if self.editor.tool() == Tool::Select
                && self.editor.editing().is_none()
                && let Some(id) = self.editor.editable_at(&self.board, &self.camera, p)
            {
                self.begin_text_edit(id);
            }
        } else {
            self.touch.last_tap = Some((now, p));
        }
    }

    /// 毎フレーム: 長押しの判定。
    pub(super) fn touch_frame(&mut self, now: f64) {
        let Some(press) = &self.touch.press else {
            return;
        };
        if press.kind != Kind::Touch
            || press.moved
            || now - press.time < LONG_PRESS_MS
            || self.pointers.len() != 1
        {
            return;
        }
        let Some(press) = self.touch.press.take() else {
            return;
        };
        // 押していた要素の移動や、画面移動は取り消す。
        if self.editor.is_dragging() {
            self.editor.cancel(&mut self.board);
        }
        if self.navigating {
            self.nav.pointer_up(press.id);
            self.navigating = false;
        }
        self.dirty = true;
        let _ = self.window.navigator().vibrate_with_duration(10);
        // メニューのハンドラも App を借りるので、借用を返してから開く(`open_pending_menu`)。
        self.touch.pending_menu = Some(press.client);
    }

    /// 文字入力を始めるとき: 指・ペンなら、キーボードで隠れにくい画面の上の方に図形を寄せる。
    pub(super) fn touch_before_edit(&mut self, rect_min: Vec2, rect_max: Vec2) {
        if !self.touch.edit_by_touch {
            return;
        }
        self.keep_above_keyboard(rect_min, rect_max, true);
    }

    /// 編集中の図形(ワールド座標の矩形)が見えている範囲に収まるよう画面を動かす。
    /// `assume_keyboard` なら、キーボードがまだ出ていなくても画面の下半分を避ける。
    fn keep_above_keyboard(&mut self, rect_min: Vec2, rect_max: Vec2, assume_keyboard: bool) {
        let dpr = self.dpr();
        let canvas_top = self.canvas.get_bounding_client_rect().top() as f32;
        let (vv_top, vv_height) = match self.window.visual_viewport() {
            Some(vv) => (vv.offset_top() as f32, vv.height() as f32),
            None => (0.0, self.camera.viewport.y / dpr),
        };
        let full = self.camera.viewport.y / dpr;
        let mut visible_bottom = vv_top + vv_height - canvas_top;
        if assume_keyboard {
            // キーボードは画面の半分弱を覆うことが多い。
            visible_bottom = visible_bottom.min(full * 0.55);
        }
        let visible_top = (vv_top - canvas_top).max(0.0) + 72.0;
        let top = self.camera.world_to_screen(rect_min).y / dpr;
        let bottom = self.camera.world_to_screen(rect_max).y / dpr;
        let margin = 16.0;
        let dy = if bottom + margin > visible_bottom {
            // 下が隠れるなら上げる(ただし上端が見える範囲を越えない)。
            (visible_bottom - margin - bottom).max(visible_top - top)
        } else if top < visible_top {
            visible_top - top
        } else {
            0.0
        };
        if dy.abs() > 0.5 {
            self.camera.pan_by_screen(Vec2::new(0.0, dy * dpr));
            self.dirty = true;
        }
    }

    /// キーボードの出し入れなどで見えている範囲が変わったとき。
    fn on_visual_viewport_change(&mut self) {
        let Some(id) = self.editor.editing() else {
            return;
        };
        let Some(tb) = self.board.get(id).and_then(board_core::Element::text_box) else {
            return;
        };
        let (min, max) = (tb.rect.min, tb.rect.max);
        self.keep_above_keyboard(min, max, false);
        self.layout_text_editor();
    }
}

/// 長押しが決まっていれば、右クリックと同じメニューを開く。`App` を借りていないときに呼ぶ。
pub(super) fn open_pending_menu(app: &Rc<RefCell<App>>) {
    let (canvas, pos) = {
        let mut a = app.borrow_mut();
        let Some(pos) = a.touch.pending_menu.take() else {
            return;
        };
        (a.canvas.clone(), pos)
    };
    let init = web_sys::MouseEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    init.set_client_x(pos.0);
    init.set_client_y(pos.1);
    init.set_button(2);
    if let Ok(ev) = web_sys::MouseEvent::new_with_mouse_event_init_dict("contextmenu", &init) {
        let _ = canvas.dispatch_event(&ev);
    }
}

/// `visualViewport` の変化(ソフトウェアキーボード)を見張る。
pub(super) fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let Some(vv) = app.borrow().window.visual_viewport() else {
        return Ok(());
    };
    let target: &web_sys::EventTarget = vv.unchecked_ref();
    for name in ["resize", "scroll"] {
        let app = app.clone();
        listen(target, name, true, move |_: web_sys::Event| {
            if let Ok(mut a) = app.try_borrow_mut() {
                a.on_visual_viewport_change();
            }
        })?;
    }
    Ok(())
}
