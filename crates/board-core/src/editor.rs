//! 編集操作(ツール、選択、移動、リサイズ、作成、手書き)。
//!
//! ブラウザのイベントから切り離してあり、座標はスクリーン座標(物理ピクセル)で受け取る。
//! パン/ピンチは [`crate::Navigator`] の担当で、どちらに渡すかは呼び出し側が決める。

use glam::Vec2;

use crate::board::Board;
use crate::camera::Camera;
use crate::color::{Rgba8, STICKY_COLORS};
use crate::element::{Element, ElementId, Stroke};
use crate::geom::Rect;
use crate::shape::Shape;

/// 選択枠や範囲選択の色。
pub const ACCENT: Rgba8 = Rgba8::hex(0x2f6fed);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Select,
    Hand,
    Rect,
    Ellipse,
    Sticky,
    Pen,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Select,
        Tool::Hand,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Sticky,
        Tool::Pen,
    ];

    /// キーボードショートカット。
    pub fn from_key(key: &str) -> Option<Tool> {
        Some(match key {
            "v" | "V" => Tool::Select,
            "h" | "H" => Tool::Hand,
            "r" | "R" => Tool::Rect,
            "o" | "O" => Tool::Ellipse,
            "s" | "S" => Tool::Sticky,
            "p" | "P" => Tool::Pen,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Hand => "hand",
            Tool::Rect => "rect",
            Tool::Ellipse => "ellipse",
            Tool::Sticky => "sticky",
            Tool::Pen => "pen",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
}

/// 選択枠の 8 つのつまみ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
}

impl Handle {
    const ALL: [Handle; 8] = [
        Handle::NW,
        Handle::N,
        Handle::NE,
        Handle::E,
        Handle::SE,
        Handle::S,
        Handle::SW,
        Handle::W,
    ];

    /// 矩形上の位置(0..1)。
    fn anchor(self) -> Vec2 {
        match self {
            Handle::NW => Vec2::new(0.0, 0.0),
            Handle::N => Vec2::new(0.5, 0.0),
            Handle::NE => Vec2::new(1.0, 0.0),
            Handle::E => Vec2::new(1.0, 0.5),
            Handle::SE => Vec2::new(1.0, 1.0),
            Handle::S => Vec2::new(0.5, 1.0),
            Handle::SW => Vec2::new(0.0, 1.0),
            Handle::W => Vec2::new(0.0, 0.5),
        }
    }

    fn position(self, r: &Rect) -> Vec2 {
        r.min + r.size() * self.anchor()
    }

    pub fn cursor(self) -> &'static str {
        match self {
            Handle::NW | Handle::SE => "nwse-resize",
            Handle::NE | Handle::SW => "nesw-resize",
            Handle::N | Handle::S => "ns-resize",
            Handle::E | Handle::W => "ew-resize",
        }
    }

    /// `orig` のこのつまみを `p` に動かした矩形。`keep_aspect` は角のつまみのみ有効。
    fn resize(self, orig: &Rect, p: Vec2, keep_aspect: bool) -> Rect {
        let a = self.anchor();
        // 反対側の点を固定する。
        let fixed = orig.min + orig.size() * (Vec2::ONE - a);
        let mut moving = p;
        // 辺のつまみは片方の軸だけ動かす。
        if a.x == 0.5 {
            moving.x = orig.max.x;
        }
        if a.y == 0.5 {
            moving.y = orig.max.y;
        }
        let mut r = Rect::from_points(fixed, moving);
        if a.x == 0.5 {
            r.min.x = orig.min.x;
            r.max.x = orig.max.x;
        }
        if a.y == 0.5 {
            r.min.y = orig.min.y;
            r.max.y = orig.max.y;
        }
        let is_corner = a.x != 0.5 && a.y != 0.5;
        if keep_aspect && is_corner && orig.size().min_element() > 0.0 {
            let s = (r.size() / orig.size()).max_element();
            let size = orig.size() * s;
            let dir = (moving - fixed).signum();
            r = Rect::from_points(fixed, fixed + size * dir);
        }
        // 潰れないよう最小 1 単位を保つ。
        let size = r.size().max(Vec2::ONE);
        Rect::new(r.min, r.min + size)
    }
}

enum Drag {
    Move {
        last: Vec2,
    },
    Resize {
        handle: Handle,
        orig: Rect,
        snapshot: Vec<Element>,
    },
    Marquee {
        start: Vec2,
        current: Vec2,
        base: Vec<ElementId>,
    },
    Create {
        id: ElementId,
        start: Vec2,
    },
    Draw {
        id: ElementId,
    },
}

/// 1 人分の編集状態。
pub struct Editor {
    tool: Tool,
    selection: Vec<ElementId>,
    drag: Option<Drag>,
    /// 文字を編集中の要素。
    editing: Option<ElementId>,
    pending_edit: Option<ElementId>,
    pub shape_fill: Rgba8,
    pub sticky_color: Rgba8,
    pub pen_color: Rgba8,
    pub pen_width: f32,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

/// つまみの当たり判定の大きさ(スクリーン px)。
const HANDLE_HIT_PX: f32 = 10.0;
/// 要素の当たり判定の許容距離(スクリーン px)。
const HIT_TOLERANCE_PX: f32 = 4.0;
/// これより小さいドラッグはクリック扱い(スクリーン px)。
const CLICK_PX: f32 = 4.0;

impl Editor {
    pub fn new() -> Self {
        Self {
            tool: Tool::Select,
            selection: Vec::new(),
            drag: None,
            editing: None,
            pending_edit: None,
            shape_fill: Rgba8::WHITE,
            sticky_color: STICKY_COLORS[0],
            pen_color: Rgba8::BLACK,
            pen_width: 3.0,
        }
    }

    pub fn tool(&self) -> Tool {
        self.tool
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
    }

    pub fn selection(&self) -> &[ElementId] {
        &self.selection
    }

    pub fn editing(&self) -> Option<ElementId> {
        self.editing
    }

    /// 文字編集を始める。図形(付箋・四角形・楕円)だけが対象。
    pub fn begin_edit(&mut self, board: &Board, id: ElementId) -> bool {
        if board.get(id).and_then(Element::as_shape).is_none() {
            return false;
        }
        self.drag = None;
        self.selection = vec![id];
        self.editing = Some(id);
        true
    }

    /// 文字編集を終える。`text` があれば反映する。
    pub fn end_edit(&mut self, board: &mut Board, text: Option<String>) {
        let Some(id) = self.editing.take() else {
            return;
        };
        if let Some(text) = text {
            board.update(id, |e| {
                if let Element::Shape(s) = e {
                    s.text = text;
                }
            });
        }
    }

    /// ダブルクリックで編集できる要素。
    pub fn editable_at(&self, board: &Board, camera: &Camera, screen: Vec2) -> Option<ElementId> {
        let world = camera.screen_to_world(screen);
        board
            .hit_test(world, HIT_TOLERANCE_PX / camera.zoom)
            .filter(|e| e.as_shape().is_some())
            .map(Element::id)
    }

    /// 付箋を置いた直後など、すぐ文字入力に入るべき要素。
    pub fn take_pending_edit(&mut self) -> Option<ElementId> {
        self.pending_edit.take()
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// 選択から消えた要素(他人が消したなど)を取り除く。
    pub fn prune_selection(&mut self, board: &Board) {
        self.selection.retain(|id| board.get(*id).is_some());
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// 選択を置き換える(存在しない要素は除く)。
    pub fn set_selection(&mut self, board: &Board, ids: &[ElementId]) {
        self.selection = ids
            .iter()
            .copied()
            .filter(|id| board.get(*id).is_some())
            .collect();
    }

    pub fn select_all(&mut self, board: &Board) {
        self.selection = board.elements().iter().map(Element::id).collect();
    }

    pub fn delete_selection(&mut self, board: &mut Board) -> usize {
        let n = board.remove(&self.selection);
        self.selection.clear();
        n
    }

    /// 現在のツールに応じた既定色を変え、選択中の要素にも適用する。
    pub fn apply_color(&mut self, board: &mut Board, c: Rgba8) {
        match self.tool {
            Tool::Pen => self.pen_color = c,
            Tool::Sticky => self.sticky_color = c,
            Tool::Rect | Tool::Ellipse => self.shape_fill = c,
            Tool::Select | Tool::Hand => {}
        }
        for id in self.selection.clone() {
            board.update(id, |e| e.set_color(c));
        }
    }

    fn selection_bounds(&self, board: &Board) -> Option<Rect> {
        board.bounds_of(&self.selection)
    }

    fn handle_at(&self, board: &Board, camera: &Camera, screen: Vec2) -> Option<(Handle, Rect)> {
        let r = self.selection_bounds(board)?;
        Handle::ALL.into_iter().find_map(|h| {
            let hs = camera.world_to_screen(h.position(&r));
            ((hs - screen).abs().max_element() <= HANDLE_HIT_PX).then_some((h, r))
        })
    }

    /// その位置で表示するマウスカーソル(CSS の cursor 値)。
    pub fn cursor(&self, board: &Board, camera: &Camera, screen: Vec2) -> &'static str {
        match self.tool {
            Tool::Hand => "grab",
            Tool::Rect | Tool::Ellipse | Tool::Sticky | Tool::Pen => "crosshair",
            Tool::Select => {
                if let Some((h, _)) = self.handle_at(board, camera, screen) {
                    return h.cursor();
                }
                if matches!(self.drag, Some(Drag::Move { .. })) {
                    return "move";
                }
                let world = camera.screen_to_world(screen);
                match board.hit_test(world, HIT_TOLERANCE_PX / camera.zoom) {
                    Some(e) if self.selection.contains(&e.id()) => "move",
                    Some(_) => "pointer",
                    None => "default",
                }
            }
        }
    }

    /// 戻り値は再描画が必要か。
    pub fn pointer_down(
        &mut self,
        board: &mut Board,
        camera: &Camera,
        screen: Vec2,
        mods: Modifiers,
    ) -> bool {
        let world = camera.screen_to_world(screen);
        match self.tool {
            Tool::Hand => false,
            Tool::Select => {
                if let Some((handle, orig)) = self.handle_at(board, camera, screen) {
                    let snapshot = self
                        .selection
                        .iter()
                        .filter_map(|id| board.get(*id).cloned())
                        .collect();
                    self.drag = Some(Drag::Resize {
                        handle,
                        orig,
                        snapshot,
                    });
                    return true;
                }
                let hit = board
                    .hit_test(world, HIT_TOLERANCE_PX / camera.zoom)
                    .map(Element::id);
                match hit {
                    Some(id) => {
                        let selected = self.selection.contains(&id);
                        if mods.shift {
                            if selected {
                                self.selection.retain(|s| *s != id);
                                return true;
                            }
                            self.selection.push(id);
                        } else if !selected {
                            self.selection = vec![id];
                        }
                        self.drag = Some(Drag::Move { last: world });
                    }
                    None => {
                        let base = if mods.shift {
                            self.selection.clone()
                        } else {
                            Vec::new()
                        };
                        self.selection = base.clone();
                        self.drag = Some(Drag::Marquee {
                            start: world,
                            current: world,
                            base,
                        });
                    }
                }
                true
            }
            Tool::Rect | Tool::Ellipse => {
                let id = board.alloc_id();
                let shape = if self.tool == Tool::Rect {
                    Shape::rect(id, world, Vec2::ZERO, self.shape_fill)
                } else {
                    Shape::ellipse(id, world, Vec2::ZERO, self.shape_fill)
                };
                board.push(shape);
                self.selection = vec![id];
                self.drag = Some(Drag::Create { id, start: world });
                true
            }
            Tool::Sticky => {
                let id = board.alloc_id();
                let mut note = Shape::sticky(id, world, self.sticky_color);
                note.pos = world - note.size * 0.5;
                board.push(note);
                self.selection = vec![id];
                self.tool = Tool::Select;
                self.pending_edit = Some(id);
                // そのままドラッグで位置を調整できる。
                self.drag = Some(Drag::Move { last: world });
                true
            }
            Tool::Pen => {
                let id = board.alloc_id();
                board.push(Stroke::new(id, world, self.pen_color, self.pen_width));
                self.selection.clear();
                self.drag = Some(Drag::Draw { id });
                true
            }
        }
    }

    pub fn pointer_move(
        &mut self,
        board: &mut Board,
        camera: &Camera,
        screen: Vec2,
        mods: Modifiers,
    ) -> bool {
        let world = camera.screen_to_world(screen);
        let Some(drag) = self.drag.as_mut() else {
            return false;
        };
        match drag {
            Drag::Move { last } => {
                let d = world - *last;
                if d == Vec2::ZERO {
                    return false;
                }
                *last = world;
                for id in &self.selection {
                    board.update(*id, |e| e.translate(d));
                }
            }
            Drag::Resize {
                handle,
                orig,
                snapshot,
            } => {
                let target = handle.resize(orig, world, mods.shift);
                for original in snapshot.iter() {
                    let mut e = original.clone();
                    e.remap(orig, &target);
                    board.push(e);
                }
            }
            Drag::Marquee {
                start,
                current,
                base,
            } => {
                *current = world;
                let r = Rect::from_points(*start, *current);
                let mut sel = base.clone();
                for id in board.query_inside(&r) {
                    if !sel.contains(&id) {
                        sel.push(id);
                    }
                }
                self.selection = sel;
            }
            Drag::Create { id, start } => {
                let mut r = Rect::from_points(*start, world);
                if mods.shift {
                    let side = r.size().max_element();
                    let dir = (world - *start).signum();
                    r = Rect::from_points(*start, *start + Vec2::splat(side) * dir);
                }
                board.update(*id, |e| {
                    if let Element::Shape(s) = e {
                        s.set_bounds(r);
                    }
                });
            }
            Drag::Draw { id } => {
                let min_dist = 1.5 / camera.zoom;
                let added = board
                    .update(*id, |e| match e {
                        Element::Stroke(s) => s.push_point(world, min_dist),
                        _ => false,
                    })
                    .unwrap_or(false);
                return added;
            }
        }
        true
    }

    pub fn pointer_up(&mut self, board: &mut Board, camera: &Camera, screen: Vec2) -> bool {
        let world = camera.screen_to_world(screen);
        match self.drag.take() {
            Some(Drag::Create { id, start }) => {
                // クリックだけなら既定の大きさで置く。
                if (world - start).abs().max_element() * camera.zoom < CLICK_PX {
                    board.update(id, |e| {
                        if let Element::Shape(s) = e {
                            s.set_bounds(Rect::new(start, start + Vec2::new(160.0, 100.0)));
                        }
                    });
                }
                self.tool = Tool::Select;
                true
            }
            Some(Drag::Marquee { .. }) => true,
            Some(_) => true,
            None => false,
        }
    }

    /// ドラッグを取り消す(2 本目の指が来てピンチに切り替わるときなど)。
    pub fn cancel(&mut self, board: &mut Board) {
        match self.drag.take() {
            Some(Drag::Create { id, .. }) | Some(Drag::Draw { id }) => {
                board.remove(&[id]);
                self.selection.retain(|s| *s != id);
            }
            Some(Drag::Resize { snapshot, .. }) => {
                for e in snapshot {
                    board.push(e);
                }
            }
            _ => {}
        }
    }

    /// 選択枠・つまみ・範囲選択の矩形。ワールド座標の図形として返し、通常の図形と同じ経路で描く。
    pub fn overlay(&self, board: &Board, camera: &Camera) -> Vec<Shape> {
        let px = 1.0 / camera.zoom;
        let mut out = Vec::new();
        let outline = |r: Rect, width_px: f32| {
            let mut s = Shape::rect(ElementId(0), r.min, r.size(), Rgba8::TRANSPARENT);
            s.stroke = ACCENT;
            s.stroke_width = width_px * px;
            s.corner_radius = 0.0;
            // 枠線は要素の外側に描く。
            s.set_bounds(r.expand(width_px * px));
            s
        };

        if self.selection.len() > 1 {
            for id in &self.selection {
                if let Some(e) = board.get(*id) {
                    out.push(outline(e.bounds(), 1.0));
                }
            }
        }
        let resizing = matches!(self.drag, Some(Drag::Resize { .. }));
        let show_handles = !matches!(self.drag, Some(Drag::Move { .. }) | Some(Drag::Draw { .. }));
        if let Some(r) = self.selection_bounds(board) {
            out.push(outline(r, 1.5));
            if show_handles || resizing {
                let hs = 8.0 * px;
                for h in Handle::ALL {
                    let c = h.position(&r);
                    let mut s = Shape::rect(
                        ElementId(0),
                        c - Vec2::splat(hs * 0.5),
                        Vec2::splat(hs),
                        Rgba8::WHITE,
                    );
                    s.stroke = ACCENT;
                    s.stroke_width = 1.5 * px;
                    s.corner_radius = 1.5 * px;
                    out.push(s);
                }
            }
        }
        if let Some(Drag::Marquee { start, current, .. }) = &self.drag {
            let r = Rect::from_points(*start, *current);
            let mut s = Shape::rect(
                ElementId(0),
                r.min,
                r.size(),
                Rgba8([0x2f, 0x6f, 0xed, 0x18]),
            );
            s.stroke = ACCENT;
            s.stroke_width = px;
            s.corner_radius = 0.0;
            out.push(s);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Board, Camera, Editor) {
        (
            Board::new(),
            Camera::new(Vec2::new(800.0, 600.0)),
            Editor::new(),
        )
    }

    fn add_rect(b: &mut Board, pos: Vec2, size: Vec2) -> ElementId {
        let id = b.alloc_id();
        b.push(Shape::rect(id, pos, size, Rgba8::WHITE))
    }

    const NO: Modifiers = Modifiers { shift: false };
    const SHIFT: Modifiers = Modifiers { shift: true };

    #[test]
    fn create_rect_by_drag_then_switch_to_select() {
        let (mut b, c, mut e) = setup();
        e.set_tool(Tool::Rect);
        e.pointer_down(&mut b, &c, Vec2::new(10.0, 10.0), NO);
        e.pointer_move(&mut b, &c, Vec2::new(110.0, 60.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(110.0, 60.0));
        assert_eq!(b.len(), 1);
        let r = b.elements()[0].bounds();
        assert_eq!(r, Rect::new(Vec2::new(10.0, 10.0), Vec2::new(110.0, 60.0)));
        assert_eq!(e.tool(), Tool::Select);
        assert_eq!(e.selection(), &[b.elements()[0].id()]);
    }

    #[test]
    fn click_with_rect_tool_uses_default_size() {
        let (mut b, c, mut e) = setup();
        e.set_tool(Tool::Ellipse);
        e.pointer_down(&mut b, &c, Vec2::new(10.0, 10.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(11.0, 10.0));
        assert_eq!(b.elements()[0].bounds().size(), Vec2::new(160.0, 100.0));
    }

    #[test]
    fn select_move_and_shift_toggle() {
        let (mut b, c, mut e) = setup();
        let a = add_rect(&mut b, Vec2::ZERO, Vec2::splat(100.0));
        let z = add_rect(&mut b, Vec2::new(200.0, 0.0), Vec2::splat(100.0));

        e.pointer_down(&mut b, &c, Vec2::splat(50.0), NO);
        e.pointer_move(&mut b, &c, Vec2::new(60.0, 70.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(60.0, 70.0));
        assert_eq!(e.selection(), &[a]);
        assert_eq!(b.get(a).unwrap().bounds().min, Vec2::new(10.0, 20.0));

        e.pointer_down(&mut b, &c, Vec2::new(250.0, 50.0), SHIFT);
        e.pointer_up(&mut b, &c, Vec2::new(250.0, 50.0));
        assert_eq!(e.selection(), &[a, z]);
        e.pointer_down(&mut b, &c, Vec2::new(250.0, 50.0), SHIFT);
        e.pointer_up(&mut b, &c, Vec2::new(250.0, 50.0));
        assert_eq!(e.selection(), &[a]);

        // 何もない所をクリックすると選択解除。
        e.pointer_down(&mut b, &c, Vec2::new(500.0, 500.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(500.0, 500.0));
        assert!(e.selection().is_empty());
    }

    #[test]
    fn marquee_selects_contained() {
        let (mut b, c, mut e) = setup();
        let a = add_rect(&mut b, Vec2::splat(10.0), Vec2::splat(50.0));
        add_rect(&mut b, Vec2::splat(300.0), Vec2::splat(50.0));
        e.pointer_down(&mut b, &c, Vec2::ZERO, NO);
        e.pointer_move(&mut b, &c, Vec2::splat(100.0), NO);
        assert_eq!(e.selection(), &[a]);
        assert!(!e.overlay(&b, &c).is_empty());
        e.pointer_up(&mut b, &c, Vec2::splat(100.0));
        assert_eq!(e.selection(), &[a]);
    }

    #[test]
    fn resize_with_corner_handle() {
        let (mut b, c, mut e) = setup();
        let a = add_rect(&mut b, Vec2::splat(100.0), Vec2::splat(100.0));
        e.pointer_down(&mut b, &c, Vec2::splat(150.0), NO);
        e.pointer_up(&mut b, &c, Vec2::splat(150.0));
        // 右下のつまみを (300, 250) へ。
        e.pointer_down(&mut b, &c, Vec2::splat(200.0), NO);
        e.pointer_move(&mut b, &c, Vec2::new(300.0, 250.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(300.0, 250.0));
        assert_eq!(
            b.get(a).unwrap().bounds(),
            Rect::new(Vec2::splat(100.0), Vec2::new(300.0, 250.0))
        );
        // Shift で縦横比を保つ。
        e.pointer_down(&mut b, &c, Vec2::new(300.0, 250.0), NO);
        e.pointer_move(&mut b, &c, Vec2::new(500.0, 260.0), SHIFT);
        e.pointer_up(&mut b, &c, Vec2::new(500.0, 260.0));
        let s = b.get(a).unwrap().bounds().size();
        assert!((s.x / s.y - 200.0 / 150.0).abs() < 1e-3);
    }

    #[test]
    fn edge_handle_moves_one_axis() {
        let r = Rect::new(Vec2::ZERO, Vec2::splat(100.0));
        let out = Handle::E.resize(&r, Vec2::new(150.0, 999.0), false);
        assert_eq!(out, Rect::new(Vec2::ZERO, Vec2::new(150.0, 100.0)));
        let out = Handle::N.resize(&r, Vec2::new(-50.0, -20.0), false);
        assert_eq!(
            out,
            Rect::new(Vec2::new(0.0, -20.0), Vec2::new(100.0, 100.0))
        );
    }

    #[test]
    fn pen_draws_and_cancel_removes() {
        let (mut b, c, mut e) = setup();
        e.set_tool(Tool::Pen);
        e.pointer_down(&mut b, &c, Vec2::ZERO, NO);
        for i in 1..10 {
            e.pointer_move(&mut b, &c, Vec2::new(i as f32 * 10.0, 0.0), NO);
        }
        e.pointer_up(&mut b, &c, Vec2::new(90.0, 0.0));
        match &b.elements()[0] {
            Element::Stroke(s) => assert_eq!(s.points.len(), 10),
            _ => panic!("stroke expected"),
        }
        assert_eq!(e.tool(), Tool::Pen);

        e.pointer_down(&mut b, &c, Vec2::new(0.0, 100.0), NO);
        e.cancel(&mut b);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn sticky_places_centered_and_delete() {
        let (mut b, c, mut e) = setup();
        e.set_tool(Tool::Sticky);
        e.pointer_down(&mut b, &c, Vec2::new(400.0, 300.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(400.0, 300.0));
        assert_eq!(b.elements()[0].bounds().center(), Vec2::new(400.0, 300.0));
        assert_eq!(e.tool(), Tool::Select);
        e.apply_color(&mut b, Rgba8::hex(0x123456));
        assert_eq!(
            b.elements()[0].as_shape().unwrap().fill,
            Rgba8::hex(0x123456)
        );
        assert_eq!(e.delete_selection(&mut b), 1);
        assert!(b.is_empty());
    }

    #[test]
    fn edit_text_flow() {
        let (mut b, c, mut e) = setup();
        e.set_tool(Tool::Sticky);
        e.pointer_down(&mut b, &c, Vec2::new(400.0, 300.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(400.0, 300.0));
        let id = e.take_pending_edit().unwrap();
        assert!(e.take_pending_edit().is_none());
        assert!(e.begin_edit(&b, id));
        assert_eq!(e.editing(), Some(id));
        e.end_edit(&mut b, Some("こんにちは".into()));
        assert_eq!(e.editing(), None);
        assert_eq!(b.get(id).unwrap().text(), "こんにちは");
        assert_eq!(e.editable_at(&b, &c, Vec2::new(400.0, 300.0)), Some(id));

        // 手書き線は編集できない。
        e.set_tool(Tool::Pen);
        e.pointer_down(&mut b, &c, Vec2::new(0.0, 0.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(0.0, 0.0));
        let stroke = b.elements().last().unwrap().id();
        assert!(!e.begin_edit(&b, stroke));
    }
}
