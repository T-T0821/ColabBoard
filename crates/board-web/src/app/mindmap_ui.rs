//! マインドマップのモード: キー操作(Tab で子、Enter で兄弟、矢印で移動)、折りたたみのつまみ、
//! ドラッグでの親の付け替え、選択中に出る小さなパネル、自動レイアウトの実行。
//!
//! 木の操作とレイアウトは `board_core::mindmap` にあり、ここはブラウザとのつなぎだけ。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::mindmap::{self, MindLayout, Nav};
use board_core::shape::Shape;
use board_core::{ElementId, Rect, Rgba8, Vec2};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, KeyboardEvent, MouseEvent};

use super::{App, by_id, listen};

/// 折りたたみのつまみの半径(画面上の px)。
const TOGGLE_PX: f32 = 9.0;

#[derive(Default)]
pub(super) struct MindUi {
    /// ドラッグ中のノードと、ドラッグ前の位置(離したときに親を付け替えるか決める)。
    drag: Option<(ElementId, Vec2)>,
    /// パネルに出している内容(作り直しを減らす)。
    shown: Option<(ElementId, MindLayout, bool, bool)>,
    pos: Option<(i32, i32)>,
}

impl App {
    /// 選択中のノードが 1 つだけならその ID。
    fn selected_node(&self) -> Option<ElementId> {
        match self.editor.selection() {
            [id] if mindmap::is_node(&self.board, *id) => Some(*id),
            _ => None,
        }
    }

    /// 新しいマインドマップを画面の中央に置き、中心のノードの文字を編集し始める。
    pub(super) fn insert_mindmap(&mut self) {
        if !self.can_edit() {
            return;
        }
        let center = self.camera.screen_to_world(self.camera.viewport * 0.5);
        self.board.end_gesture();
        let root = mindmap::create(&mut self.board, center, "中心のテーマ", MindLayout::Both);
        self.board.end_gesture();
        self.editor.set_tool(board_core::Tool::Select);
        self.editor.set_selection(vec![root]);
        self.begin_text_edit(root);
        self.dirty = true;
    }

    /// 1 回の操作としてノードを足し、選んで文字を編集し始める。
    fn mind_add(&mut self, child: bool) {
        let Some(id) = self.selected_node().or(self
            .editor
            .editing()
            .filter(|id| mindmap::is_node(&self.board, *id)))
        else {
            return;
        };
        self.end_text_edit(true);
        self.board.end_gesture();
        let new = if child {
            mindmap::add_child(&mut self.board, id)
        } else {
            mindmap::add_sibling(&mut self.board, id)
        };
        self.board.end_gesture();
        if let Some(new) = new {
            self.editor.set_selection(vec![new]);
            self.reveal(new);
            self.begin_text_edit(new);
        }
        self.dirty = true;
    }

    /// 要素が画面に入っていなければ表示を動かす。
    fn reveal(&mut self, id: ElementId) {
        let Some(b) = self.board.get(id).map(|e| e.bounds()) else {
            return;
        };
        let (min, max) = self.camera.visible_world_rect();
        let view = Rect::new(min, max).expand(-40.0 / self.camera.zoom);
        if !view.contains_rect(&b) {
            let d = b.center() - Rect::new(min, max).center();
            self.camera.origin += d;
        }
    }

    fn mind_toggle(&mut self, id: ElementId) {
        self.board.end_gesture();
        let done = mindmap::toggle_collapse(&mut self.board, id);
        self.board.end_gesture();
        if done {
            self.editor.set_selection(vec![id]);
            self.dirty = true;
        }
    }

    fn mind_command(&mut self, cmd: &str) {
        if !self.can_edit() {
            return;
        }
        let Some(id) = self.selected_node() else {
            return;
        };
        match cmd {
            "child" => return self.mind_add(true),
            "sibling" => return self.mind_add(false),
            "fold" => return self.mind_toggle(id),
            _ => {}
        }
        self.board.end_gesture();
        match cmd {
            "expand-all" => mindmap::set_all_collapsed(&mut self.board, id, false),
            "collapse-all" => mindmap::set_all_collapsed(&mut self.board, id, true),
            other => {
                if let Some(l) = other.strip_prefix("layout-").map(MindLayout::from_name) {
                    mindmap::set_layout(&mut self.board, id, l);
                }
            }
        }
        self.board.end_gesture();
        // たたんで隠れたノードは選択から外す。
        if self.board.get(id).is_some_and(|e| e.meta().hidden) {
            self.editor
                .set_selection(mindmap::root_of(&self.board, id).into_iter().collect());
        }
        self.dirty = true;
    }

    /// ノードを選んでいるときのキー操作。扱ったら true。
    pub(super) fn mind_key(&mut self, e: &KeyboardEvent) -> bool {
        if !self.can_edit() || e.ctrl_key() || e.meta_key() || e.alt_key() {
            return false;
        }
        let Some(id) = self.selected_node() else {
            return false;
        };
        let is_root = mindmap::root_of(&self.board, id) == Some(id);
        match e.key().as_str() {
            "Tab" => self.mind_add(true),
            "Enter" if e.shift_key() || is_root => self.begin_text_edit(id),
            "Enter" => self.mind_add(false),
            "F2" => self.begin_text_edit(id),
            "/" => self.mind_toggle(id),
            k @ ("ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown") => {
                let nav = match k {
                    "ArrowLeft" => Nav::Left,
                    "ArrowRight" => Nav::Right,
                    "ArrowUp" => Nav::Up,
                    _ => Nav::Down,
                };
                if let Some(next) = mindmap::navigate(&self.board, id, nav) {
                    self.editor.set_selection(vec![next]);
                    self.reveal(next);
                    self.dirty = true;
                }
            }
            _ => return false,
        }
        true
    }

    /// 毎フレーム: マインドマップが変わっていたら並べ直す(ドラッグ中は離すまで待つ)。
    pub(super) fn mind_frame(&mut self) {
        if self.editor.is_dragging() {
            return;
        }
        if !self.board.take_mind_dirty() || !self.can_edit() {
            return;
        }
        if mindmap::relayout(&mut self.board) {
            self.dirty = true;
        }
    }

    /// 折りたたみのつまみ(たたんだノードは常に、開いているノードは選択中だけ)。
    fn mind_toggles(&self) -> Vec<mindmap::Toggle> {
        let (min, max) = self.camera.visible_world_rect();
        let r = TOGGLE_PX / self.camera.zoom;
        let sel = self.editor.selection();
        mindmap::toggles(&self.board, &Rect::new(min, max), r)
            .into_iter()
            .filter(|t| t.collapsed || sel.contains(&t.id))
            .collect()
    }

    /// 描画用のつまみ(ワールド座標の図形)。
    pub(super) fn mind_overlay(&self, out: &mut Vec<Shape>) {
        let px = 1.0 / self.camera.zoom;
        let r = TOGGLE_PX * px;
        for t in self.mind_toggles() {
            let (fill, mark) = if t.collapsed {
                (t.color, Rgba8::WHITE)
            } else {
                (Rgba8::WHITE, t.color)
            };
            let mut c = Shape::ellipse(
                ElementId(0),
                t.center - Vec2::splat(r),
                Vec2::splat(2.0 * r),
                fill,
            );
            c.stroke = t.color;
            c.stroke_width = 1.5 * px;
            out.push(c);
            // 「+」(たたんでいる) か「−」(開いている)。
            let bar = |size: Vec2| {
                let mut s = Shape::rect(ElementId(0), t.center - size * 0.5, size, mark);
                s.stroke_width = 0.0;
                s.corner_radius = 0.75 * px;
                s
            };
            out.push(bar(Vec2::new(9.0 * px, 1.8 * px)));
            if t.collapsed {
                out.push(bar(Vec2::new(1.8 * px, 9.0 * px)));
            }
        }
    }

    /// ポインタを押したとき: つまみなら折りたたみを切り替える。扱ったら true。
    pub(super) fn mind_pointer_down(&mut self, p: Vec2) -> bool {
        self.mind.drag = None;
        if !self.can_edit() {
            return false;
        }
        let world = self.camera.screen_to_world(p);
        let r = (TOGGLE_PX + 3.0) / self.camera.zoom;
        if let Some(t) = self
            .mind_toggles()
            .into_iter()
            .find(|t| t.center.distance(world) <= r)
        {
            self.mind_toggle(t.id);
            return true;
        }
        false
    }

    /// 押した直後(エディタが選択を決めた後): 中心以外のノードのドラッグを覚えておく。
    pub(super) fn mind_after_pointer_down(&mut self) {
        if !self.editor.is_dragging() {
            return;
        }
        if let Some(id) = self.selected_node()
            && mindmap::root_of(&self.board, id) != Some(id)
            && let Some(e) = self.board.get(id)
        {
            self.mind.drag = Some((id, e.bounds().min));
        }
    }

    /// 離したとき: ノードを別のノードの上で離したら、その子に付け替える。
    pub(super) fn mind_pointer_up(&mut self, p: Vec2) {
        let Some((id, start)) = self.mind.drag.take() else {
            return;
        };
        let moved = self
            .board
            .get(id)
            .is_some_and(|e| e.bounds().min.distance(start) > 2.0);
        if !moved {
            return;
        }
        let world = self.camera.screen_to_world(p);
        if let Some(target) = mindmap::drop_target(&self.board, id, world) {
            mindmap::reparent(&mut self.board, id, target);
        }
        // 付け替えなければ、次のフレームの並べ直しで元の場所に戻る。
        self.dirty = true;
    }

    /// 文字の編集中のキー: Tab で確定して子を、Enter で確定する。扱ったら true。
    pub(super) fn mind_editor_key(&mut self, e: &KeyboardEvent) -> bool {
        let Some(id) = self.editor.editing() else {
            return false;
        };
        if !mindmap::is_node(&self.board, id) || e.ctrl_key() || e.meta_key() || e.alt_key() {
            return false;
        }
        match e.key().as_str() {
            "Tab" => {
                self.mind_add(!e.shift_key());
                true
            }
            // 確定は呼び出し側が入力欄を外して(blur で)する。
            "Enter" if !e.shift_key() => true,
            _ => false,
        }
    }

    /// 選んだノードの下に、並べ方などの小さなパネルを出す。
    pub(super) fn layout_mind_panel(&mut self) {
        let Some(panel) = self
            .document
            .get_element_by_id("mind-panel")
            .and_then(|e| e.dyn_into::<HtmlElement>().ok())
        else {
            return;
        };
        let node = self
            .selected_node()
            .filter(|_| self.can_edit() && !self.editor.is_dragging() && !self.navigating)
            .filter(|_| self.editor.editing().is_none());
        let Some((id, bounds)) = node.and_then(|id| Some((id, self.board.get(id)?.bounds())))
        else {
            if self.mind.shown.take().is_some() {
                panel.set_hidden(true);
                self.mind.pos = None;
            }
            return;
        };
        let layout = mindmap::layout_of(&self.board, id).unwrap_or_default();
        let has_kids = mindmap::has_children(&self.board, id);
        let collapsed = self
            .board
            .get(id)
            .and_then(mindmap::link_of)
            .is_some_and(|l| l.collapsed);
        let key = (id, layout, has_kids, collapsed);
        if self.mind.shown != Some(key) {
            panel.set_inner_html(&render(layout, has_kids, collapsed));
            panel.set_hidden(false);
            self.mind.shown = Some(key);
        }
        let dpr = self.window.device_pixel_ratio() as f32;
        let canvas = self.canvas.get_bounding_client_rect();
        let top = self.camera.world_to_screen(bounds.min) / dpr;
        let bottom = self.camera.world_to_screen(bounds.max) / dpr;
        let w = panel.offset_width() as f32;
        let h = panel.offset_height() as f32;
        let vw = canvas.width() as f32;
        let vh = canvas.height() as f32;
        let x = ((top.x + bottom.x) * 0.5 - w * 0.5).clamp(8.0, (vw - w - 8.0).max(8.0));
        let y = (bottom.y + 16.0).clamp(8.0, (vh - h - 80.0).max(8.0));
        let pos = (
            (canvas.left() as f32 + x).round() as i32,
            (canvas.top() as f32 + y).round() as i32,
        );
        if self.mind.pos != Some(pos) {
            self.mind.pos = Some(pos);
            let css = panel.style();
            let _ = css.set_property("left", &format!("{}px", pos.0));
            let _ = css.set_property("top", &format!("{}px", pos.1));
        }
    }
}

fn render(layout: MindLayout, has_kids: bool, collapsed: bool) -> String {
    let mut h = String::from(r#"<div class="mind-seg" role="group" aria-label="並べ方">"#);
    for l in MindLayout::ALL {
        h.push_str(&format!(
            r#"<button data-mind="layout-{}" aria-pressed="{}" title="並べ方: {}">{}</button>"#,
            l.name(),
            l == layout,
            l.label(),
            l.label()
        ));
    }
    h.push_str(r#"</div><span class="fb-sep"></span>"#);
    h.push_str(
        r#"<button data-mind="child" title="子を追加 (Tab)">＋子 <kbd>Tab</kbd></button>
<button data-mind="sibling" title="兄弟を追加 (Enter)">＋兄弟 <kbd>Enter</kbd></button>"#,
    );
    if has_kids {
        h.push_str(&format!(
            r#"<button data-mind="fold" title="この枝を{0} (/)">{0}</button>"#,
            if collapsed { "開く" } else { "たたむ" }
        ));
    }
    h.push_str(
        r#"<span class="fb-sep"></span><button data-mind="expand-all" title="すべての枝を開く">すべて開く</button>
<button data-mind="collapse-all" title="すべての枝をたたむ">すべてたたむ</button>"#,
    );
    h
}

pub fn install(app: &Rc<RefCell<App>>) -> Result<(), String> {
    let doc = app.borrow().document.clone();
    let button: Element = by_id(&doc, "mindmap-button")?;
    {
        let app = app.clone();
        listen(button.as_ref(), "click", false, move |_: MouseEvent| {
            app.borrow_mut().insert_mindmap();
        })?;
    }
    let panel: Element = by_id(&doc, "mind-panel")?;
    {
        let app = app.clone();
        listen(panel.as_ref(), "click", false, move |e: MouseEvent| {
            let Some(cmd) = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|t| t.closest("[data-mind]").ok().flatten())
                .and_then(|b| b.get_attribute("data-mind"))
            else {
                return;
            };
            app.borrow_mut().mind_command(&cmd);
        })?;
    }
    // パネルを押してもキャンバスの選択が外れないようにする。
    listen(
        panel.as_ref(),
        "pointerdown",
        false,
        |e: web_sys::PointerEvent| {
            e.stop_propagation();
        },
    )?;
    Ok(())
}
