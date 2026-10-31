//! 編集操作(ツール、選択、移動、リサイズ、作成、手書き)。
//!
//! ブラウザのイベントから切り離してあり、座標はスクリーン座標(物理ピクセル)で受け取る。
//! パン/ピンチは [`crate::Navigator`] の担当で、どちらに渡すかは呼び出し側が決める。

use glam::Vec2;

use crate::arrange;
use crate::board::Board;
use crate::camera::Camera;
use crate::color::{Rgba8, STICKY_COLORS};
use crate::element::{Connector, Element, ElementId, Endpoint, PathStyle, Stroke};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind};
use crate::snap;

/// 選択枠や範囲選択の色。
pub const ACCENT: Rgba8 = Rgba8::hex(0x4c5cf5);
/// スマートガイドの色(選択枠と見分けやすい赤)。
pub const GUIDE: Rgba8 = Rgba8::hex(0xf2385a);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Select,
    Hand,
    Sticky,
    /// 図形ライブラリの図形(種類は `Editor::shape_kind`)。
    Shape,
    Text,
    Connector,
    Pen,
    Highlighter,
    Eraser,
    Frame,
    /// スタンプ(種類は `Editor::stamp`)。クリックするたびに押せる。
    Stamp,
}

impl Tool {
    pub const ALL: [Tool; 11] = [
        Tool::Select,
        Tool::Hand,
        Tool::Sticky,
        Tool::Shape,
        Tool::Text,
        Tool::Connector,
        Tool::Pen,
        Tool::Highlighter,
        Tool::Eraser,
        Tool::Frame,
        Tool::Stamp,
    ];

    /// キーボードショートカット。図形の種類も決まるものは `Some(kind)` を返す。
    pub fn from_key(key: &str) -> Option<(Tool, Option<ShapeKind>)> {
        Some(match key {
            "v" | "V" => (Tool::Select, None),
            "h" | "H" => (Tool::Hand, None),
            "s" | "S" => (Tool::Sticky, None),
            "r" | "R" => (Tool::Shape, Some(ShapeKind::Rect)),
            "o" | "O" => (Tool::Shape, Some(ShapeKind::Ellipse)),
            "t" | "T" => (Tool::Text, None),
            "x" | "X" => (Tool::Connector, None),
            "p" | "P" => (Tool::Pen, None),
            "m" | "M" => (Tool::Highlighter, None),
            "e" | "E" => (Tool::Eraser, None),
            "f" | "F" => (Tool::Frame, None),
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Hand => "hand",
            Tool::Sticky => "sticky",
            Tool::Shape => "shape",
            Tool::Text => "text",
            Tool::Connector => "connector",
            Tool::Pen => "pen",
            Tool::Highlighter => "highlighter",
            Tool::Eraser => "eraser",
            Tool::Frame => "frame",
            Tool::Stamp => "stamp",
        }
    }

    /// 1 回使ったら選択ツールに戻るか。
    fn one_shot(self) -> bool {
        matches!(
            self,
            Tool::Sticky | Tool::Shape | Tool::Text | Tool::Connector | Tool::Frame
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    /// 押している間はスナップしない。
    pub alt: bool,
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

    /// 動く辺 [左, 上, 右, 下]。
    fn moving_edges(self) -> [bool; 4] {
        let a = self.anchor();
        [a.x == 0.0, a.y == 0.0, a.x == 1.0, a.y == 1.0]
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
        /// つかんだ点。
        start: Vec2,
        /// ここまでに動かした量。
        applied: Vec2,
        /// 一緒に動かす要素(選択 + グループ + フレームの中身)。
        ids: Vec<ElementId>,
        /// つかんだときの外接矩形(スナップ用)。
        bounds: Option<Rect>,
        /// 吸い付く相手(最初に動かしたときに集める)。
        others: Option<Vec<Rect>>,
    },
    Resize {
        handle: Handle,
        orig: Rect,
        snapshot: Vec<Element>,
        others: Option<Vec<Rect>>,
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
    /// コネクタの端をつかんで動かしている(新しく引いているときも)。
    ConnectorEnd {
        id: ElementId,
        /// true なら終点。
        end: bool,
    },
    Erase,
}

/// 点の下にある最前面の要素(ロック中の要素は素通りする)。
fn pick<'a>(
    board: &'a Board,
    hidden: &std::collections::HashSet<ElementId>,
    world: Vec2,
    tolerance: f32,
) -> Option<&'a Element> {
    board
        .elements()
        .iter()
        .rev()
        .find(|e| !e.is_locked() && !hidden.contains(&e.id()) && e.hit(world, tolerance))
}

/// コネクタの端をつなげる相手(図形のみ、`except` 以外)。
fn connect_target(
    board: &Board,
    world: Vec2,
    camera: &Camera,
    except: Option<ElementId>,
) -> Option<ElementId> {
    let tol = HIT_TOLERANCE_PX / camera.zoom;
    board
        .elements()
        .iter()
        .rev()
        .filter(|e| matches!(e, Element::Shape(s) if s.kind != ShapeKind::Frame))
        .find(|e| Some(e.id()) != except && e.hit(world, tol))
        .map(Element::id)
}

/// 消しゴム: 点の近くの手書き線を消す。消したら true。
fn erase_at(board: &mut Board, world: Vec2, tolerance: f32) -> bool {
    let hit: Vec<ElementId> = board
        .elements()
        .iter()
        .filter(|e| matches!(e, Element::Stroke(_)) && !e.is_locked() && e.hit(world, tolerance))
        .map(Element::id)
        .collect();
    !hit.is_empty() && board.remove(&hit) > 0
}

/// 移動のドラッグを始める。
fn move_drag(board: &Board, world: Vec2, ids: Vec<ElementId>) -> Drag {
    // スナップは図形の外接矩形で考える(付いてくるコネクタは含めない)。
    let shapes: Vec<ElementId> = ids
        .iter()
        .copied()
        .filter(|id| matches!(board.get(*id), Some(Element::Shape(_))))
        .collect();
    Drag::Move {
        start: world,
        applied: Vec2::ZERO,
        bounds: board.bounds_of(&shapes),
        ids,
        others: None,
    }
}

/// 吸い付く相手を、画面に見えている範囲(少し広め)から集める。
fn snap_targets(board: &Board, camera: &Camera, except: &[ElementId]) -> Vec<Rect> {
    let (min, max) = camera.visible_world_rect();
    let area = Rect::new(min, max).expand(200.0 / camera.zoom);
    snap::targets(board, &area, except)
}

/// ガイドを描くための図形(細い長方形)。
fn guide_shapes(guides: &[snap::Guide], px: f32) -> Vec<Shape> {
    let bar = |a: Vec2, b: Vec2| {
        let r = Rect::from_points(a, b).expand(0.5 * px);
        let mut s = Shape::rect(ElementId(0), r.min, r.size(), GUIDE);
        s.stroke_width = 0.0;
        s.corner_radius = 0.0;
        s
    };
    let mut out = Vec::new();
    for g in guides {
        match *g {
            snap::Guide::Line { a, b } => out.push(bar(a, b)),
            snap::Guide::Gap { a, b, vertical } => {
                out.push(bar(a, b));
                // 両端に短い棒。
                let t = 5.0 * px;
                let n = if vertical { Vec2::X } else { Vec2::Y };
                for p in [a, b] {
                    out.push(bar(p - n * t, p + n * t));
                }
            }
        }
    }
    out
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
    pub highlighter_color: Rgba8,
    pub shape_kind: ShapeKind,
    pub connector_style: PathStyle,
    pub stamp: crate::stamp::Stamp,
    /// コネクタを引いているときに、つながりそうな要素(強調表示用)。
    hover_target: Option<ElementId>,
    /// 当たり判定を広げる倍率(指で触るときは大きくする)。
    hit_scale: f32,
    /// 個人作業エリアで隠れている要素(選択も編集もできない)。`privacy::hidden_ids` で作る。
    pub hidden: std::collections::HashSet<ElementId>,
    /// ドラッグ中に出すスマートガイド。
    guides: Vec<snap::Guide>,
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
            highlighter_color: Rgba8([0xff, 0xe0, 0x33, 0x66]),
            shape_kind: ShapeKind::Rect,
            connector_style: PathStyle::default(),
            stamp: crate::stamp::STAMPS[0].clone(),
            hover_target: None,
            hit_scale: 1.0,
            hidden: std::collections::HashSet::new(),
            guides: Vec::new(),
        }
    }

    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// 当たり判定(つまみ・要素・クリックとみなす距離)の倍率。指なら大きくする。
    pub fn set_hit_scale(&mut self, scale: f32) {
        self.hit_scale = scale.max(1.0);
    }

    fn handle_px(&self) -> f32 {
        HANDLE_HIT_PX * self.hit_scale
    }

    fn hit_px(&self) -> f32 {
        HIT_TOLERANCE_PX * self.hit_scale
    }

    fn click_px(&self) -> f32 {
        CLICK_PX * self.hit_scale
    }

    /// その位置を押しても何もつかまない(空いている場所)か。選択ツール用。
    pub fn hits_nothing(&self, board: &Board, camera: &Camera, screen: Vec2) -> bool {
        self.connector_end_at(board, camera, screen).is_none()
            && self.handle_at(board, camera, screen).is_none()
            && pick(
                board,
                &self.hidden,
                camera.screen_to_world(screen),
                self.hit_px() / camera.zoom,
            )
            .is_none()
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
    }

    /// スタンプツールにして種類を選ぶ。
    pub fn set_stamp(&mut self, stamp: crate::stamp::Stamp) {
        self.tool = Tool::Stamp;
        self.stamp = stamp;
    }

    /// 次回も使う設定(最後に使った色・図形など)を文字列にする。`load_prefs` で戻せる。
    pub fn save_prefs(&self) -> String {
        format!(
            "shape={};fill={};sticky={};pen={};pw={};hl={};path={};stamp={}",
            self.shape_kind.name(),
            self.shape_fill.to_hex(),
            self.sticky_color.to_hex(),
            self.pen_color.to_hex(),
            self.pen_width,
            self.highlighter_color.to_hex(),
            self.connector_style.name(),
            self.stamp.name,
        )
    }

    /// `save_prefs` の文字列を読む。読めない項目は今のまま。
    pub fn load_prefs(&mut self, s: &str) {
        for (k, v) in s.split(';').filter_map(|kv| kv.split_once('=')) {
            match k {
                "shape" => {
                    if let Some(kind) = ShapeKind::from_name(v)
                        && ShapeKind::LIBRARY.contains(&kind)
                    {
                        self.shape_kind = kind;
                    }
                }
                "fill" => self.shape_fill = Rgba8::parse_hex(v).unwrap_or(self.shape_fill),
                "sticky" => self.sticky_color = Rgba8::parse_hex(v).unwrap_or(self.sticky_color),
                "pen" => self.pen_color = Rgba8::parse_hex(v).unwrap_or(self.pen_color),
                "pw" => {
                    if let Ok(w) = v.parse::<f32>()
                        && (0.5..=64.0).contains(&w)
                    {
                        self.pen_width = w;
                    }
                }
                "hl" => {
                    self.highlighter_color = Rgba8::parse_hex(v).unwrap_or(self.highlighter_color);
                }
                "path" => self.connector_style = PathStyle::from_name(v),
                "stamp" => {
                    if let Some(st) = crate::stamp::Stamp::by_name(v) {
                        self.stamp = st;
                    }
                }
                _ => {}
            }
        }
    }

    /// 図形ツールにして種類を選ぶ。
    pub fn set_shape_kind(&mut self, kind: ShapeKind) {
        self.tool = Tool::Shape;
        self.shape_kind = kind;
    }

    /// コネクタの線の引き方(キーボードでつなぐときにも使う)。
    pub fn connector_style(&self) -> PathStyle {
        self.connector_style
    }

    /// 今のツールで作る要素を `center` を中心に既定の大きさで置く(キーボード操作用)。
    /// 置けないツールなら `None`。置いた要素を選び、1 回きりのツールは選択に戻す。
    pub fn create_at(&mut self, board: &mut Board, center: Vec2) -> Option<ElementId> {
        let id = board.alloc_id();
        let shape = match self.tool {
            Tool::Sticky => Shape::sticky(id, Vec2::ZERO, self.sticky_color),
            Tool::Shape => Shape::new(
                id,
                self.shape_kind,
                Vec2::ZERO,
                self.shape_kind.default_size(),
                self.shape_fill,
            ),
            Tool::Text => {
                let mut t = Shape::text(id, Vec2::ZERO, ShapeKind::Text.default_size());
                t.text_color = Rgba8::BLACK;
                t
            }
            Tool::Frame => Shape::frame(id, Vec2::ZERO, ShapeKind::Frame.default_size()),
            Tool::Stamp => self.stamp.shape(id, Vec2::ZERO),
            _ => return None,
        };
        let mut shape = shape;
        let size = shape.size;
        let want = Rect::new(center - size * 0.5, center + size * 0.5);
        shape.pos = if self.tool == Tool::Frame {
            want.min
        } else {
            crate::a11y::free_spot(board, want, Vec2::new(size.x + 40.0, 0.0))
        };
        shape.fit_text_height();
        if self.tool == Tool::Frame {
            let (lo, _) = board.z_range();
            board.push(shape);
            board.set_z(&[(id, lo - 1.0)]);
        } else {
            board.push(shape);
        }
        self.selection = vec![id];
        if self.tool.one_shot() {
            self.tool = Tool::Select;
        }
        Some(id)
    }

    pub fn selection(&self) -> &[ElementId] {
        &self.selection
    }

    pub fn editing(&self) -> Option<ElementId> {
        self.editing
    }

    /// 文字編集を始める。図形とコネクタ(ラベル)が対象。
    pub fn begin_edit(&mut self, board: &Board, id: ElementId) -> bool {
        if board.get(id).is_none_or(|e| {
            e.text_box().is_none()
                || e.is_locked()
                || e.as_shape().is_some_and(|s| s.kind == ShapeKind::Stamp)
        }) {
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
            board.update(id, |e| e.set_text(text));
        }
        // 中身のない文字要素は残さない。
        let empty_text = board.get(id).is_some_and(|e| {
            matches!(e, Element::Shape(s) if s.kind == ShapeKind::Text && s.text.trim().is_empty())
        });
        if empty_text {
            board.remove(&[id]);
            self.selection.retain(|s| *s != id);
        }
    }

    /// ダブルクリックで編集できる要素。
    pub fn editable_at(&self, board: &Board, camera: &Camera, screen: Vec2) -> Option<ElementId> {
        let world = camera.screen_to_world(screen);
        pick(board, &self.hidden, world, self.hit_px() / camera.zoom)
            .filter(|e| e.text_box().is_some())
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

    pub fn select_all(&mut self, board: &Board) {
        self.selection = board
            .elements()
            .iter()
            .filter(|e| !e.is_locked() && !self.hidden.contains(&e.id()))
            .map(Element::id)
            .collect();
    }

    /// 右クリックした位置の要素を選ぶ(選択中の要素ならそのまま)。何もなければ選択を外す。
    pub fn select_at(&mut self, board: &Board, camera: &Camera, screen: Vec2) {
        let world = camera.screen_to_world(screen);
        match pick(board, &self.hidden, world, self.hit_px() / camera.zoom).map(Element::id) {
            Some(id) if self.selection.contains(&id) => {}
            Some(id) => self.selection = arrange::expand_groups(board, &[id]),
            None => self.selection.clear(),
        }
    }

    /// 選択を置き換える(貼り付け後など)。
    pub fn set_selection(&mut self, ids: Vec<ElementId>) {
        self.selection = ids;
    }

    pub fn delete_selection(&mut self, board: &mut Board) -> usize {
        let ids: Vec<ElementId> = self
            .selection
            .iter()
            .copied()
            .filter(|id| board.get(*id).is_some_and(|e| !e.is_locked()))
            .collect();
        let n = board.remove(&ids);
        self.selection.clear();
        n
    }

    /// 現在のツールに応じた既定色を変え、選択中の要素にも適用する。
    pub fn apply_color(&mut self, board: &mut Board, c: Rgba8) {
        match self.tool {
            Tool::Pen => self.pen_color = c,
            Tool::Highlighter => {
                self.highlighter_color = Rgba8([c.0[0], c.0[1], c.0[2], 0x66]);
            }
            Tool::Sticky => self.sticky_color = c,
            Tool::Shape => self.shape_fill = c,
            _ => {}
        }
        // 選択中の要素の色を変えたら、次に作る同じ種類の要素もその色にする(最後に使った色)。
        for id in self.selection.clone() {
            match board.get(id) {
                Some(Element::Shape(s)) if s.kind == ShapeKind::Sticky => self.sticky_color = c,
                Some(Element::Shape(s)) if ShapeKind::LIBRARY.contains(&s.kind) => {
                    self.shape_fill = c;
                }
                Some(Element::Stroke(s)) if s.color.0[3] == 0xff => self.pen_color = c,
                _ => {}
            }
            board.update(id, |e| e.set_color(c));
        }
    }

    fn selection_bounds(&self, board: &Board) -> Option<Rect> {
        board.bounds_of(&self.selection)
    }

    /// 選択中のコネクタが 1 本だけならそれを返す(端のつまみを出す)。
    fn single_connector<'a>(&self, board: &'a Board) -> Option<&'a Connector> {
        match self.selection.as_slice() {
            [id] => match board.get(*id) {
                Some(Element::Connector(c)) => Some(c),
                _ => None,
            },
            _ => None,
        }
    }

    /// コネクタの端のつまみ(true なら終点)。
    fn connector_end_at(&self, board: &Board, camera: &Camera, screen: Vec2) -> Option<bool> {
        let c = self.single_connector(board)?;
        [(c.end(), true), (c.start(), false)]
            .into_iter()
            .find(|(p, _)| {
                (camera.world_to_screen(*p) - screen).abs().max_element() <= self.handle_px()
            })
            .map(|(_, end)| end)
    }

    fn handle_at(&self, board: &Board, camera: &Camera, screen: Vec2) -> Option<(Handle, Rect)> {
        if self.single_connector(board).is_some() {
            return None;
        }
        let r = self.selection_bounds(board)?;
        Handle::ALL.into_iter().find_map(|h| {
            let hs = camera.world_to_screen(h.position(&r));
            ((hs - screen).abs().max_element() <= self.handle_px()).then_some((h, r))
        })
    }

    /// その位置で表示するマウスカーソル(CSS の cursor 値)。
    pub fn cursor(&self, board: &Board, camera: &Camera, screen: Vec2) -> &'static str {
        match self.tool {
            Tool::Hand => "grab",
            Tool::Text => "text",
            Tool::Eraser => "cell",
            Tool::Sticky
            | Tool::Shape
            | Tool::Connector
            | Tool::Pen
            | Tool::Highlighter
            | Tool::Frame
            | Tool::Stamp => "crosshair",
            Tool::Select => {
                if let Some((h, _)) = self.handle_at(board, camera, screen) {
                    return h.cursor();
                }
                if self.connector_end_at(board, camera, screen).is_some() {
                    return "crosshair";
                }
                if matches!(self.drag, Some(Drag::Move { .. })) {
                    return "move";
                }
                let world = camera.screen_to_world(screen);
                match pick(board, &self.hidden, world, self.hit_px() / camera.zoom) {
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
                if let Some(end) = self.connector_end_at(board, camera, screen) {
                    let id = self.selection[0];
                    self.drag = Some(Drag::ConnectorEnd { id, end });
                    return true;
                }
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
                        others: None,
                    });
                    return true;
                }
                let hit =
                    pick(board, &self.hidden, world, self.hit_px() / camera.zoom).map(Element::id);
                match hit {
                    Some(id) => {
                        let selected = self.selection.contains(&id);
                        // グループの仲間もまとめて選ぶ。
                        let members = arrange::expand_groups(board, &[id]);
                        if mods.shift {
                            if selected {
                                self.selection.retain(|s| !members.contains(s));
                                return true;
                            }
                            for m in members {
                                if !self.selection.contains(&m) {
                                    self.selection.push(m);
                                }
                            }
                        } else if !selected {
                            self.selection = members;
                        }
                        let ids = arrange::move_set(board, &self.selection);
                        self.drag = Some(move_drag(board, world, ids));
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
            Tool::Shape | Tool::Frame | Tool::Text => {
                let id = board.alloc_id();
                let shape = match self.tool {
                    Tool::Frame => Shape::frame(id, world, Vec2::ZERO),
                    Tool::Text => {
                        let mut t = Shape::text(id, world, Vec2::ZERO);
                        t.text_color = Rgba8::BLACK;
                        t
                    }
                    _ => Shape::new(id, self.shape_kind, world, Vec2::ZERO, self.shape_fill),
                };
                if self.tool == Tool::Frame {
                    // フレームは中身より後ろに置く。
                    let (lo, _) = board.z_range();
                    board.push(shape);
                    board.set_z(&[(id, lo - 1.0)]);
                } else {
                    board.push(shape);
                }
                self.selection = vec![id];
                self.drag = Some(Drag::Create { id, start: world });
                true
            }
            Tool::Connector => {
                let id = board.alloc_id();
                let from = match connect_target(board, world, camera, None) {
                    Some(t) => Endpoint::Attached {
                        id: t,
                        anchor: None,
                    },
                    None => Endpoint::Free(world),
                };
                let mut c = Connector::new(id, from, Endpoint::Free(world));
                c.style = self.connector_style;
                board.push(c);
                self.selection = vec![id];
                self.drag = Some(Drag::ConnectorEnd { id, end: true });
                true
            }
            Tool::Eraser => {
                self.selection.clear();
                self.drag = Some(Drag::Erase);
                erase_at(board, world, 8.0 / camera.zoom);
                true
            }
            Tool::Stamp => {
                let id = board.alloc_id();
                board.push(self.stamp.shape(id, world));
                self.selection = vec![id];
                // そのままドラッグで位置を調整できる。ツールはスタンプのまま。
                self.drag = Some(move_drag(board, world, vec![id]));
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
                self.drag = Some(move_drag(board, world, vec![id]));
                true
            }
            Tool::Pen | Tool::Highlighter => {
                let id = board.alloc_id();
                let mut st = if self.tool == Tool::Pen {
                    Stroke::new(id, world, self.pen_color, self.pen_width)
                } else {
                    Stroke::new(id, world, self.highlighter_color, 20.0)
                };
                st.highlighter = self.tool == Tool::Highlighter;
                board.push(st);
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
            Drag::Move {
                start,
                applied,
                ids,
                bounds,
                others,
            } => {
                let mut want = world - *start;
                self.guides.clear();
                if let Some(b) = bounds.filter(|_| !mods.alt) {
                    let others = others.get_or_insert_with(|| snap_targets(board, camera, ids));
                    let moved = Rect::new(b.min + want, b.max + want);
                    let s = snap::snap_move(moved, others, snap::SNAP_PX / camera.zoom);
                    want += s.offset;
                    self.guides = s.guides;
                }
                let d = want - *applied;
                if d == Vec2::ZERO {
                    return true;
                }
                *applied = want;
                for id in ids.iter() {
                    board.update(*id, |e| e.translate(d));
                }
            }
            Drag::Resize {
                handle,
                orig,
                snapshot,
                others,
            } => {
                let mut target = handle.resize(orig, world, mods.shift);
                self.guides.clear();
                // 縦横比を保つときはスナップしない(比がずれるため)。
                if !mods.alt && !mods.shift {
                    let ids: Vec<ElementId> = snapshot.iter().map(Element::id).collect();
                    let others = others.get_or_insert_with(|| snap_targets(board, camera, &ids));
                    let (r, g) = snap::snap_resize(
                        target,
                        handle.moving_edges(),
                        others,
                        snap::SNAP_PX / camera.zoom,
                    );
                    target = r;
                    self.guides = g;
                }
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
                let inside: Vec<ElementId> = board
                    .query_inside(&r)
                    .into_iter()
                    .filter(|id| {
                        !self.hidden.contains(id) && board.get(*id).is_some_and(|e| !e.is_locked())
                    })
                    .collect();
                for id in arrange::expand_groups(board, &inside) {
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
            Drag::ConnectorEnd { id, end } => {
                let (id, end) = (*id, *end);
                let other = board.get(id).and_then(|e| match e {
                    Element::Connector(c) => if end { c.from } else { c.to }.attached_to(),
                    _ => None,
                });
                self.hover_target = connect_target(board, world, camera, other);
                let target = self.hover_target;
                board.update(id, |e| {
                    if let Element::Connector(c) = e {
                        let ep = match target {
                            Some(t) => Endpoint::Attached {
                                id: t,
                                anchor: None,
                            },
                            None => Endpoint::Free(world),
                        };
                        if end {
                            c.to = ep;
                        } else {
                            c.from = ep;
                        }
                    }
                });
            }
            Drag::Erase => {
                return erase_at(board, world, 8.0 / camera.zoom);
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
        self.guides.clear();
        match self.drag.take() {
            Some(Drag::Create { id, start }) => {
                let clicked = (world - start).abs().max_element() * camera.zoom < self.click_px();
                let mut is_text = false;
                board.update(id, |e| {
                    if let Element::Shape(s) = e {
                        is_text = s.kind == ShapeKind::Text;
                        if clicked {
                            // クリックだけなら既定の大きさで置く。
                            let size = s.kind.default_size();
                            s.set_bounds(Rect::new(start, start + size));
                        }
                        s.fit_text_height();
                    }
                });
                if is_text {
                    self.pending_edit = Some(id);
                }
                if self.tool.one_shot() {
                    self.tool = Tool::Select;
                }
                true
            }
            Some(Drag::ConnectorEnd { id, .. }) => {
                self.hover_target = None;
                // 長さのないコネクタ(クリックしただけ)は消す。
                let tiny = match board.get(id) {
                    Some(Element::Connector(c)) => {
                        c.from
                            .attached_to()
                            .is_none_or(|a| c.to.attached_to() == Some(a))
                            && c.start().distance(c.end()) * camera.zoom < self.click_px()
                    }
                    _ => false,
                };
                if tiny {
                    board.remove(&[id]);
                    self.selection.clear();
                }
                if self.tool.one_shot() {
                    self.tool = Tool::Select;
                }
                true
            }
            Some(Drag::Marquee { .. }) => true,
            Some(_) => true,
            None => false,
        }
    }

    /// ドラッグを取り消す(2 本目の指が来てピンチに切り替わるときなど)。
    pub fn cancel(&mut self, board: &mut Board) {
        self.guides.clear();
        match self.drag.take() {
            Some(Drag::ConnectorEnd { .. }) => {
                self.hover_target = None;
            }
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
        if let Some(t) = self.hover_target.and_then(|id| board.get(id)) {
            out.push(outline(t.bounds(), 2.0));
        }
        if let Some(c) = self.single_connector(board) {
            // コネクタは両端に丸いつまみを出す。
            let r = 6.0 * px;
            for p in [c.start(), c.end()] {
                let mut s = Shape::ellipse(
                    ElementId(0),
                    p - Vec2::splat(r),
                    Vec2::splat(2.0 * r),
                    Rgba8::WHITE,
                );
                s.stroke = ACCENT;
                s.stroke_width = 2.0 * px;
                out.push(s);
            }
            return out;
        }
        let resizing = matches!(self.drag, Some(Drag::Resize { .. }));
        let show_handles = !matches!(self.drag, Some(Drag::Move { .. }) | Some(Drag::Draw { .. }));
        if let Some(r) = self.selection_bounds(board) {
            out.push(outline(r, 1.5));
            if show_handles || resizing {
                // つまみは白い丸に細いアクセント色の縁(FigJam・Miro と同じ見た目)。
                let hs = 10.0 * px;
                for h in Handle::ALL {
                    let c = h.position(&r);
                    let mut s = Shape::ellipse(
                        ElementId(0),
                        c - Vec2::splat(hs * 0.5),
                        Vec2::splat(hs),
                        Rgba8::WHITE,
                    );
                    s.stroke = ACCENT;
                    s.stroke_width = 1.5 * px;
                    out.push(s);
                }
            }
        }
        out.extend(guide_shapes(&self.guides, px));
        if let Some(Drag::Marquee { start, current, .. }) = &self.drag {
            let r = Rect::from_points(*start, *current);
            let mut s = Shape::rect(
                ElementId(0),
                r.min,
                r.size(),
                Rgba8([0x4c, 0x5c, 0xf5, 0x14]),
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

    const NO: Modifiers = Modifiers {
        shift: false,
        alt: false,
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        alt: false,
    };

    #[test]
    fn prefs_round_trip() {
        let mut e = Editor::new();
        e.set_shape_kind(ShapeKind::Cloud);
        e.shape_fill = Rgba8::hex(0x123456);
        e.sticky_color = STICKY_COLORS[3];
        e.highlighter_color = Rgba8([1, 2, 3, 0x66]);
        e.pen_width = 6.0;
        e.connector_style = PathStyle::Curved;
        let saved = e.save_prefs();
        let mut f = Editor::new();
        f.load_prefs(&saved);
        assert_eq!(f.save_prefs(), saved);
        assert_eq!(f.tool(), Tool::Select, "読み込みでツールは変えない");
        // 壊れた値は無視する。
        f.load_prefs("shape=sticky;fill=#zz;pw=999;junk");
        assert_eq!(f.save_prefs(), saved);
    }

    #[test]
    fn recoloring_a_selection_becomes_the_next_default() {
        let mut b = Board::new();
        let mut e = Editor::new();
        let id = b.alloc_id();
        b.push(Shape::sticky(id, Vec2::ZERO, STICKY_COLORS[0]));
        e.set_selection(vec![id]);
        e.apply_color(&mut b, STICKY_COLORS[4]);
        assert_eq!(e.sticky_color, STICKY_COLORS[4]);
    }

    #[test]
    fn create_rect_by_drag_then_switch_to_select() {
        let (mut b, c, mut e) = setup();
        e.set_shape_kind(ShapeKind::Rect);
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
    fn hit_scale_widens_handles_and_empty_test() {
        let (mut b, c, mut e) = setup();
        let a = add_rect(&mut b, Vec2::ZERO, Vec2::splat(100.0));
        assert!(!e.hits_nothing(&b, &c, Vec2::splat(50.0)));
        assert!(e.hits_nothing(&b, &c, Vec2::splat(120.0)));
        e.set_selection(vec![a]);
        // 右下のつまみから 20px 離れた所: 既定では空き、指(倍率 3)ならつまみ。
        let near = Vec2::splat(120.0);
        assert!(e.hits_nothing(&b, &c, near));
        e.set_hit_scale(3.0);
        assert!(!e.hits_nothing(&b, &c, near));
        e.pointer_down(&mut b, &c, near, NO);
        e.pointer_move(&mut b, &c, Vec2::splat(220.0), NO);
        e.pointer_up(&mut b, &c, Vec2::splat(220.0));
        // つまみをつかんで大きさが変わった(移動ではない)。
        let r = b.get(a).unwrap().bounds();
        assert_eq!(r.min, Vec2::ZERO);
        assert!(r.max.x > 150.0);
    }

    #[test]
    fn click_with_rect_tool_uses_default_size() {
        let (mut b, c, mut e) = setup();
        e.set_shape_kind(ShapeKind::Ellipse);
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
    fn move_snaps_to_neighbor_and_alt_disables() {
        let (mut b, c, mut e) = setup();
        add_rect(&mut b, Vec2::ZERO, Vec2::splat(100.0));
        let z = add_rect(&mut b, Vec2::new(200.0, 300.0), Vec2::splat(100.0));
        // 左端を x=3 まで動かす → x=0 に吸い付き、ガイドが出る。
        e.pointer_down(&mut b, &c, Vec2::new(250.0, 350.0), NO);
        e.pointer_move(&mut b, &c, Vec2::new(53.0, 350.0), NO);
        assert_eq!(b.get(z).unwrap().bounds().min, Vec2::new(0.0, 300.0));
        assert!(!e.guides.is_empty());
        assert!(e.overlay(&b, &c).iter().any(|s| s.fill == GUIDE));
        // Alt を押している間は吸い付かない。
        let alt = Modifiers {
            shift: false,
            alt: true,
        };
        e.pointer_move(&mut b, &c, Vec2::new(54.0, 350.0), alt);
        assert_eq!(b.get(z).unwrap().bounds().min, Vec2::new(4.0, 300.0));
        assert!(e.guides.is_empty());
        e.pointer_up(&mut b, &c, Vec2::new(54.0, 350.0));
        assert!(e.guides.is_empty());
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
    fn connector_tool_attaches_both_ends() {
        let (mut b, c, mut e) = setup();
        let a = add_rect(&mut b, Vec2::ZERO, Vec2::splat(100.0));
        let z = add_rect(&mut b, Vec2::new(300.0, 0.0), Vec2::splat(100.0));
        e.set_tool(Tool::Connector);
        e.pointer_down(&mut b, &c, Vec2::splat(50.0), NO);
        e.pointer_move(&mut b, &c, Vec2::new(200.0, 50.0), NO);
        assert!(e.overlay(&b, &c).len() >= 2);
        e.pointer_move(&mut b, &c, Vec2::new(350.0, 50.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(350.0, 50.0));
        assert_eq!(e.tool(), Tool::Select);
        let Some(Element::Connector(k)) = b.elements().last() else {
            panic!("connector expected")
        };
        assert_eq!(k.from.attached_to(), Some(a));
        assert_eq!(k.to.attached_to(), Some(z));
        let kid = k.id;

        // 終点のつまみをつかんで外すと自由な端になる。
        let end = c.world_to_screen(k.end());
        e.pointer_down(&mut b, &c, end, NO);
        e.pointer_move(&mut b, &c, Vec2::new(200.0, 400.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(200.0, 400.0));
        let Some(Element::Connector(k)) = b.get(kid) else {
            panic!()
        };
        assert_eq!(k.to, Endpoint::Free(Vec2::new(200.0, 400.0)));

        // クリックだけのコネクタは残らない。
        let n = b.len();
        e.set_tool(Tool::Connector);
        e.pointer_down(&mut b, &c, Vec2::new(600.0, 600.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(600.0, 600.0));
        assert_eq!(b.len(), n);
    }

    #[test]
    fn text_tool_and_eraser() {
        let (mut b, c, mut e) = setup();
        e.set_tool(Tool::Text);
        e.pointer_down(&mut b, &c, Vec2::new(10.0, 10.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(10.0, 10.0));
        let id = e.take_pending_edit().expect("文字入力に入る");
        assert!(e.begin_edit(&b, id));
        e.end_edit(&mut b, Some(String::new()));
        assert!(b.is_empty(), "空の文字要素は消える");

        e.set_tool(Tool::Highlighter);
        e.pointer_down(&mut b, &c, Vec2::new(0.0, 100.0), NO);
        e.pointer_move(&mut b, &c, Vec2::new(100.0, 100.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(100.0, 100.0));
        assert!(matches!(&b.elements()[0], Element::Stroke(s) if s.highlighter));
        e.set_tool(Tool::Eraser);
        e.pointer_down(&mut b, &c, Vec2::new(50.0, 300.0), NO);
        e.pointer_move(&mut b, &c, Vec2::new(50.0, 100.0), NO);
        e.pointer_up(&mut b, &c, Vec2::new(50.0, 100.0));
        assert!(b.is_empty());
    }

    #[test]
    fn frame_goes_behind() {
        let (mut b, c, mut e) = setup();
        let a = add_rect(&mut b, Vec2::splat(50.0), Vec2::splat(50.0));
        e.set_tool(Tool::Frame);
        e.pointer_down(&mut b, &c, Vec2::ZERO, NO);
        e.pointer_move(&mut b, &c, Vec2::splat(300.0), NO);
        e.pointer_up(&mut b, &c, Vec2::splat(300.0));
        assert_eq!(b.elements().last().unwrap().id(), a);
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
