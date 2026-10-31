//! 手書きのストロークを整った図形に変える(図形認識)。
//!
//! 2 段階に分かれる。
//! 1. **認識**: ストロークを「どの図形か」の一覧([`InkItem`])にする。[`recognize`] は端末だけで
//!    動く幾何学的な認識。サーバーの Claude(画像を見て手書き文字も読む)も同じ形の一覧を返す。
//! 2. **組み立て**: [`build`] が一覧から実際の要素(図形・コネクタ・テキスト)を作り、
//!    消すストロークを返す。どちらの認識でも組み立ては共通。
//!
//! 一覧に入らなかったストロークはそのまま残る(読めない手書きを勝手に消さない)。

use std::collections::HashMap;

use glam::Vec2;

use crate::color::Rgba8;
use crate::connector::{Cap, Connector, Endpoint, PathStyle};
use crate::element::{Element, ElementId, Stroke};
use crate::geom::{Rect, distance_to_segment};
use crate::shape::{Shape, ShapeKind};

/// 認識した 1 つの図形の種類。
#[derive(Clone, Debug, PartialEq)]
pub enum InkKind {
    Shape(ShapeKind),
    /// 矢印のない線。
    Line,
    /// 片側に矢印。どちら側かはストロークの形から決める(決まらなければ描き終わりの側)。
    Arrow,
    /// 両側に矢印。
    DoubleArrow,
    /// 手書きの文字(Claude だけが返す)。図形の中にあればその図形の文字にする。
    Text(String),
}

impl InkKind {
    /// 線・文字の種類の名前(図形は [`ShapeKind::name`])。AI とのやり取りに使う。
    pub const LINE_KINDS: [&'static str; 4] = ["line", "arrow", "double_arrow", "text"];

    /// 名前から作る。`text` は種類が `text` のときの文字。図形は [`AI_SHAPE_KINDS`] の中だけ。
    pub fn from_name(name: &str, text: &str) -> Option<Self> {
        match name {
            "line" => Some(Self::Line),
            "arrow" => Some(Self::Arrow),
            "double_arrow" => Some(Self::DoubleArrow),
            "text" => {
                let t = text.trim();
                (!t.is_empty()).then(|| Self::Text(t.to_string()))
            }
            _ => ShapeKind::from_name(name)
                .filter(|k| AI_SHAPE_KINDS.contains(k))
                .map(Self::Shape),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Shape(k) => k.name(),
            Self::Line => "line",
            Self::Arrow => "arrow",
            Self::DoubleArrow => "double_arrow",
            Self::Text(_) => "text",
        }
    }
}

/// 認識結果の 1 項目。`strokes` はこの図形を描いたストローク(複数でもよい)。
#[derive(Clone, Debug, PartialEq)]
pub struct InkItem {
    pub strokes: Vec<ElementId>,
    pub kind: InkKind,
}

/// 組み立ての結果。
#[derive(Debug, Default)]
pub struct Built {
    /// 消すストローク。
    pub remove: Vec<ElementId>,
    /// 足す要素(図形が先、コネクタが後)。
    pub add: Vec<Element>,
}

/// Claude に選ばせてよい図形の種類(保存名)。手で描いて意味のあるものだけ。
pub const AI_SHAPE_KINDS: &[ShapeKind] = &[
    ShapeKind::Rect,
    ShapeKind::RoundRect,
    ShapeKind::Ellipse,
    ShapeKind::Triangle,
    ShapeKind::TriangleDown,
    ShapeKind::RightTriangle,
    ShapeKind::Diamond,
    ShapeKind::Parallelogram,
    ShapeKind::ParallelogramLeft,
    ShapeKind::Trapezoid,
    ShapeKind::Pentagon,
    ShapeKind::Hexagon,
    ShapeKind::Octagon,
    ShapeKind::Star,
    ShapeKind::Star4,
    ShapeKind::Cross,
    ShapeKind::Heart,
    ShapeKind::Cloud,
    ShapeKind::Lightning,
    ShapeKind::Speech,
    ShapeKind::Pill,
    ShapeKind::Cylinder,
    ShapeKind::Document,
    ShapeKind::ArrowRight,
    ShapeKind::ArrowLeft,
    ShapeKind::ArrowUp,
    ShapeKind::ArrowDown,
];

/// 図形にするには小さすぎるストローク(外接矩形の対角線の長さ)。
const MIN_SIZE: f32 = 12.0;
/// 線の端が図形にこれだけ近ければつなぐ(ワールド単位)。
const ATTACH_DIST: f32 = 24.0;
/// 水平・垂直に揃える角度の幅(度)。
const SNAP_DEG: f32 = 7.0;

// ---------------------------------------------------------------------------
// 幾何の道具

fn bbox(points: &[Vec2]) -> Rect {
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for p in points {
        min = min.min(*p);
        max = max.max(*p);
    }
    Rect::new(min, max)
}

fn path_len(points: &[Vec2]) -> f32 {
    points.windows(2).map(|w| w[0].distance(w[1])).sum()
}

/// 折れ線を `n` 点に等間隔で打ち直す。
fn resample(points: &[Vec2], n: usize) -> Vec<Vec2> {
    let total = path_len(points);
    if points.len() < 2 || total <= f32::EPSILON {
        return points.to_vec();
    }
    let step = total / (n - 1) as f32;
    let mut out = vec![points[0]];
    let mut acc = 0.0;
    let mut prev = points[0];
    let mut i = 1;
    while i < points.len() && out.len() < n {
        let cur = points[i];
        let d = prev.distance(cur);
        if acc + d >= step && d > 0.0 {
            let t = (step - acc) / d;
            let q = prev + (cur - prev) * t;
            out.push(q);
            prev = q;
            acc = 0.0;
        } else {
            acc += d;
            prev = cur;
            i += 1;
        }
    }
    while out.len() < n {
        out.push(*points.last().unwrap());
    }
    out
}

/// Ramer–Douglas–Peucker 法で角だけを残す。
fn rdp(points: &[Vec2], eps: f32) -> Vec<Vec2> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let (a, b) = (points[0], *points.last().unwrap());
    let (mut idx, mut best) = (0, 0.0);
    for (i, p) in points.iter().enumerate().take(points.len() - 1).skip(1) {
        let d = distance_to_segment(*p, a, b);
        if d > best {
            best = d;
            idx = i;
        }
    }
    if best > eps {
        let mut left = rdp(&points[..=idx], eps);
        let right = rdp(&points[idx..], eps);
        left.pop();
        left.extend(right);
        left
    } else {
        vec![a, b]
    }
}

/// 閉じた形の角。始点がたまたま辺の途中にあっても角として数えないよう、
/// 始点から一番遠い点を起点にして 2 つに分けて求める。
fn closed_corners(points: &[Vec2], eps: f32) -> Vec<Vec2> {
    let start = points[0];
    let far = points
        .iter()
        .enumerate()
        .max_by(|a, b| start.distance(*a.1).total_cmp(&start.distance(*b.1)))
        .map_or(0, |(i, _)| i);
    let mut ring: Vec<Vec2> = points[far..].to_vec();
    ring.extend_from_slice(&points[..=far]);
    // ring は far → … → 始点 → … → far。一番遠い点の反対側でもう一度分ける。
    let pivot = ring[0];
    let mid = ring
        .iter()
        .enumerate()
        .max_by(|a, b| pivot.distance(*a.1).total_cmp(&pivot.distance(*b.1)))
        .map_or(ring.len() / 2, |(i, _)| i);
    let mut a = rdp(&ring[..=mid], eps);
    let b = rdp(&ring[mid..], eps);
    a.pop();
    a.extend(b);
    a.pop(); // 最後は ring[0] と同じ点
    // 始点・終点の継ぎ目でほぼ一直線になっている点を除く。
    let mut changed = true;
    while changed && a.len() > 3 {
        changed = false;
        for i in 0..a.len() {
            let p = a[(i + a.len() - 1) % a.len()];
            let n = a[(i + 1) % a.len()];
            if distance_to_segment(a[i], p, n) < eps {
                a.remove(i);
                changed = true;
                break;
            }
        }
    }
    a
}

fn polygon_area(points: &[Vec2]) -> f32 {
    let n = points.len();
    let mut s = 0.0;
    for i in 0..n {
        let (a, b) = (points[i], points[(i + 1) % n]);
        s += a.x * b.y - b.x * a.y;
    }
    s.abs() * 0.5
}

/// 凸包(Andrew の単調連鎖法)。
fn convex_hull(points: &[Vec2]) -> Vec<Vec2> {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    let mut lower: Vec<Vec2> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<Vec2> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// 外接矩形に内接する楕円からのずれ(半径比の平均誤差)。
fn ellipse_error(points: &[Vec2], b: &Rect) -> f32 {
    let c = b.center();
    let h = (b.size() * 0.5).max(Vec2::splat(1e-3));
    points
        .iter()
        .map(|p| (((*p - c) / h).length() - 1.0).abs())
        .sum::<f32>()
        / points.len() as f32
}

/// 点の、外接矩形の中での相対位置(0..1)。
fn rel(p: Vec2, b: &Rect) -> Vec2 {
    (p - b.min) / b.size().max(Vec2::splat(1e-3))
}

// ---------------------------------------------------------------------------
// 1 本のストロークの分類

/// 1 本のストロークの見立て。
#[derive(Clone, Debug, PartialEq)]
enum Single {
    Shape(ShapeKind),
    /// 線(片側・両側に矢じりを一緒に描いていることもある)。
    Line {
        arrow_start: bool,
        arrow_end: bool,
    },
    /// 矢じりだけ(「く」の字)。`tip` が先端。
    Hook {
        tip: Vec2,
    },
}

/// 1 本の線の芯と、どちらの端に矢じりがついているか。
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shaft {
    a: Vec2,
    b: Vec2,
    head_a: bool,
    head_b: bool,
}

/// 開いたストロークを「線 + 端の矢じり」として読む。
///
/// 折れ線の一番長い辺を芯とし、残りの点がすべて芯の端の近くにあれば、その端に矢じりがあるとみなす。
fn shaft_of(points: &[Vec2]) -> Option<Shaft> {
    let len = path_len(points);
    if len < MIN_SIZE {
        return None;
    }
    let simple = rdp(points, (len * 0.04).max(3.0));
    if simple.len() == 2 {
        return Some(Shaft {
            a: simple[0],
            b: simple[1],
            head_a: false,
            head_b: false,
        });
    }
    let (i, seg) = simple
        .windows(2)
        .enumerate()
        .map(|(i, w)| (i, w[0].distance(w[1])))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if seg < len * 0.45 {
        return None;
    }
    let (a, b) = (simple[i], simple[i + 1]);
    let r = seg * 0.4;
    let before = &simple[..i];
    let after = &simple[i + 2..];
    if !before.iter().all(|p| p.distance(a) <= r) || !after.iter().all(|p| p.distance(b) <= r) {
        return None;
    }
    // 矢じりは芯に対して斜めに戻る線。短すぎるものはペンのぶれとみなす。
    let extra = |pts: &[Vec2], end: Vec2| -> bool {
        let mut chain = vec![end];
        chain.extend_from_slice(pts);
        path_len(&chain) >= seg * 0.12
    };
    let head_a = !before.is_empty() && extra(before, a);
    let head_b = !after.is_empty() && extra(after, b);
    Some(Shaft {
        a,
        b,
        head_a,
        head_b,
    })
}

/// 「く」の字(矢じり)なら先端を返す。
fn hook_of(points: &[Vec2]) -> Option<Vec2> {
    let len = path_len(points);
    let simple = rdp(points, (len * 0.08).max(2.0));
    if simple.len() != 3 {
        return None;
    }
    let (p, t, n) = (simple[0], simple[1], simple[2]);
    let (l1, l2) = (p.distance(t), t.distance(n));
    if l1 < 4.0 || l2 < 4.0 || l1 / l2 > 3.0 || l2 / l1 > 3.0 {
        return None;
    }
    let angle = (p - t).angle_to(n - t).abs().to_degrees();
    (20.0..=130.0).contains(&angle).then_some(t)
}

fn classify_closed(points: &[Vec2]) -> Option<ShapeKind> {
    let pts = resample(points, 96);
    let b = bbox(&pts);
    let diag = b.size().length();
    let hull = convex_hull(&pts);
    let hull_area = polygon_area(&hull);
    let box_area = (b.size().x * b.size().y).max(1e-3);
    let fill = hull_area / box_area;
    let solidity = polygon_area(&pts) / hull_area.max(1e-3);
    let corners = closed_corners(&pts, diag * 0.07);
    let k = corners.len();
    let e_err = ellipse_error(&pts, &b);

    // 星はへこみが大きい。
    if solidity < 0.72 && k >= 7 {
        return Some(if k >= 9 {
            ShapeKind::Star
        } else {
            ShapeKind::Star4
        });
    }
    if e_err < 0.085 && !(k <= 4 && e_err > 0.05) {
        return Some(ShapeKind::Ellipse);
    }
    match k {
        3 => Some(triangle_kind(&corners, &b)),
        4 => Some(quad_kind(&corners, &b, fill)),
        5 => Some(ShapeKind::Pentagon),
        6 => Some(ShapeKind::Hexagon),
        7 | 8 => Some(ShapeKind::Octagon),
        _ if e_err < 0.14 => Some(ShapeKind::Ellipse),
        _ if fill > 0.8 => Some(ShapeKind::Rect),
        _ => None,
    }
}

fn triangle_kind(c: &[Vec2], b: &Rect) -> ShapeKind {
    let r: Vec<Vec2> = c.iter().map(|p| rel(*p, b)).collect();
    // 頂点が外接矩形の角にあって、残り 2 つがその角を挟む 2 辺の端にあれば直角三角形。
    let near = |p: Vec2, q: Vec2| p.distance(q) < 0.2;
    let corners = [Vec2::ZERO, Vec2::X, Vec2::ONE, Vec2::Y];
    let at_corner = r
        .iter()
        .filter(|p| corners.iter().any(|c| near(**p, *c)))
        .count();
    if at_corner == 3 {
        return ShapeKind::RightTriangle;
    }
    let top = r.iter().filter(|p| p.y < 0.25).count();
    let bottom = r.iter().filter(|p| p.y > 0.75).count();
    if top == 2 && bottom == 1 {
        ShapeKind::TriangleDown
    } else {
        ShapeKind::Triangle
    }
}

fn quad_kind(c: &[Vec2], b: &Rect, fill: f32) -> ShapeKind {
    let r: Vec<Vec2> = c.iter().map(|p| rel(*p, b)).collect();
    // ひし形は各辺の真ん中あたりに頂点がある。
    let mids = r
        .iter()
        .filter(|p| {
            let edge_x = p.x < 0.2 || p.x > 0.8;
            let edge_y = p.y < 0.2 || p.y > 0.8;
            edge_x != edge_y && (0.25..=0.75).contains(&if edge_x { p.y } else { p.x })
        })
        .count();
    if mids >= 3 && fill < 0.75 {
        return ShapeKind::Diamond;
    }
    if fill > 0.85 {
        return ShapeKind::Rect;
    }
    let mut by_y = r.clone();
    by_y.sort_by(|a, b| a.y.total_cmp(&b.y));
    let (t0, t1) = (by_y[0], by_y[1]);
    let (b0, b1) = (by_y[2], by_y[3]);
    let top_w = (t0.x - t1.x).abs();
    let bot_w = (b0.x - b1.x).abs();
    let top_c = (t0.x + t1.x) * 0.5;
    let bot_c = (b0.x + b1.x) * 0.5;
    if top_w < bot_w * 0.75 && (top_c - bot_c).abs() < 0.15 {
        ShapeKind::Trapezoid
    } else if (top_w - bot_w).abs() < 0.25 && top_c - bot_c > 0.12 {
        ShapeKind::Parallelogram
    } else if (top_w - bot_w).abs() < 0.25 && bot_c - top_c > 0.12 {
        ShapeKind::ParallelogramLeft
    } else if mids >= 3 {
        ShapeKind::Diamond
    } else {
        ShapeKind::Rect
    }
}

fn classify(points: &[Vec2]) -> Option<Single> {
    let b = bbox(points);
    let diag = b.size().length();
    if points.len() < 2 || diag < MIN_SIZE {
        return None;
    }
    let len = path_len(points);
    let gap = points[0].distance(*points.last().unwrap());
    let closed = len > diag * 1.6 && gap < diag * 0.22;
    if closed {
        return classify_closed(points).map(Single::Shape);
    }
    if let Some(s) = shaft_of(points) {
        let chord = s.a.distance(s.b);
        // 矢じりのない線は、ほぼまっすぐなものだけ。
        if s.head_a || s.head_b || chord >= len * 0.9 {
            return Some(Single::Line {
                arrow_start: s.head_a,
                arrow_end: s.head_b,
            });
        }
    }
    if let Some(tip) = hook_of(points) {
        return Some(Single::Hook { tip });
    }
    None
}

/// 端末だけで図形を認識する。蛍光ペンと小さすぎるストロークは対象外。
pub fn recognize(strokes: &[&Stroke]) -> Vec<InkItem> {
    let mut items: Vec<InkItem> = Vec::new();
    let mut lines: Vec<(usize, Shaft)> = Vec::new();
    let mut hooks: Vec<(ElementId, Vec2)> = Vec::new();
    for s in strokes.iter().filter(|s| !s.highlighter) {
        match classify(&s.points) {
            Some(Single::Shape(kind)) => items.push(InkItem {
                strokes: vec![s.id],
                kind: InkKind::Shape(kind),
            }),
            Some(Single::Line {
                arrow_start,
                arrow_end,
            }) => {
                let kind = match (arrow_start, arrow_end) {
                    (true, true) => InkKind::DoubleArrow,
                    (false, false) => InkKind::Line,
                    _ => InkKind::Arrow,
                };
                if let Some(shaft) = shaft_of(&s.points) {
                    lines.push((items.len(), shaft));
                }
                items.push(InkItem {
                    strokes: vec![s.id],
                    kind,
                });
            }
            Some(Single::Hook { tip }) => hooks.push((s.id, tip)),
            None => {}
        }
    }
    // 別に描いた矢じりを、先端が近い線の端にくっつける。
    for (hook, tip) in hooks {
        let best = lines
            .iter_mut()
            .filter_map(|(i, sh)| {
                let r = (sh.a.distance(sh.b) * 0.3).max(ATTACH_DIST);
                let (da, db) = (tip.distance(sh.a), tip.distance(sh.b));
                let d = da.min(db);
                (d <= r).then_some((i, sh, d, da < db))
            })
            .min_by(|a, b| a.2.total_cmp(&b.2));
        if let Some((i, sh, _, at_a)) = best {
            if at_a {
                sh.head_a = true;
            } else {
                sh.head_b = true;
            }
            let item = &mut items[*i];
            item.strokes.push(hook);
            item.kind = if sh.head_a && sh.head_b {
                InkKind::DoubleArrow
            } else {
                InkKind::Arrow
            };
        }
    }
    items
}

// ---------------------------------------------------------------------------
// 組み立て

/// 水平・垂直に近い線を揃える。
fn snap_line(a: Vec2, b: Vec2) -> (Vec2, Vec2) {
    let d = b - a;
    let ang = d.y.atan2(d.x).to_degrees().rem_euclid(180.0);
    let m = (a + b) * 0.5;
    if !(SNAP_DEG..=180.0 - SNAP_DEG).contains(&ang) {
        (Vec2::new(a.x, m.y), Vec2::new(b.x, m.y))
    } else if (ang - 90.0).abs() < SNAP_DEG {
        (Vec2::new(m.x, a.y), Vec2::new(m.x, b.y))
    } else {
        (a, b)
    }
}

/// 線(と矢じり)のストロークから、芯の両端とどちらに矢印があるかを求める。
fn line_geometry(strokes: &[&Stroke]) -> Option<Shaft> {
    let main = strokes.iter().max_by(|a, b| {
        let ca = a.points[0].distance(*a.points.last().unwrap());
        let cb = b.points[0].distance(*b.points.last().unwrap());
        ca.total_cmp(&cb)
    })?;
    let mut shaft = shaft_of(&main.points).unwrap_or(Shaft {
        a: main.points[0],
        b: *main.points.last()?,
        head_a: false,
        head_b: false,
    });
    for s in strokes.iter().filter(|s| s.id != main.id) {
        let c = bbox(&s.points).center();
        let tip = hook_of(&s.points).unwrap_or(c);
        if tip.distance(shaft.a) < tip.distance(shaft.b) {
            shaft.head_a = true;
        } else {
            shaft.head_b = true;
        }
    }
    Some(shaft)
}

fn opaque(c: Rgba8) -> Rgba8 {
    Rgba8([c.0[0], c.0[1], c.0[2], 255])
}

/// 認識結果から要素を作る。
///
/// - `strokes`: 元のストローク(ID で引く)
/// - `targets`: 線の端をつないでよい既存の図形(ID と外接矩形)
/// - `alloc`: 新しい要素の ID を払い出す
pub fn build(
    items: &[InkItem],
    strokes: &[&Stroke],
    targets: &[(ElementId, Rect)],
    alloc: &mut dyn FnMut() -> ElementId,
) -> Built {
    let by_id: HashMap<ElementId, &Stroke> = strokes.iter().map(|s| (s.id, *s)).collect();
    let mut out = Built::default();
    let mut shapes: Vec<Shape> = Vec::new();
    let mut texts: Vec<(Rect, String, Rgba8)> = Vec::new();
    let mut lines: Vec<(Shaft, Rgba8, f32)> = Vec::new();

    for item in items {
        let ss: Vec<&Stroke> = item
            .strokes
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        if ss.is_empty() {
            continue;
        }
        let pts: Vec<Vec2> = ss.iter().flat_map(|s| s.points.iter().copied()).collect();
        let first = ss[0];
        let color = opaque(first.color);
        let width = first.width.clamp(1.0, 12.0);
        let b = bbox(&pts);
        match &item.kind {
            InkKind::Shape(kind) => {
                let mut size = b.size().max(Vec2::splat(MIN_SIZE));
                let center = b.center();
                // ほぼ正方形・正円なら揃える。
                if (size.x - size.y).abs() < size.max_element() * 0.12 {
                    size = Vec2::splat((size.x + size.y) * 0.5);
                }
                let mut s = Shape::new(
                    alloc(),
                    *kind,
                    center - size * 0.5,
                    size,
                    Rgba8::TRANSPARENT,
                );
                s.stroke = color;
                s.stroke_width = width.max(2.0);
                s.text_color = color;
                shapes.push(s);
            }
            InkKind::Text(t) => {
                let t = t.trim();
                if !t.is_empty() {
                    texts.push((b, t.to_string(), color));
                }
            }
            InkKind::Line | InkKind::Arrow | InkKind::DoubleArrow => {
                let Some(mut sh) = line_geometry(&ss) else {
                    continue;
                };
                match item.kind {
                    InkKind::Line => (sh.head_a, sh.head_b) = (false, false),
                    InkKind::DoubleArrow => (sh.head_a, sh.head_b) = (true, true),
                    _ if !sh.head_a && !sh.head_b => sh.head_b = true,
                    _ if sh.head_a && sh.head_b => sh.head_a = false,
                    _ => {}
                }
                (sh.a, sh.b) = snap_line(sh.a, sh.b);
                lines.push((sh, color, width));
            }
        }
        out.remove.extend(ss.iter().map(|s| s.id));
    }

    // 文字: 図形の中にあればその図形の文字、なければテキスト要素。
    for (b, text, color) in texts {
        let c = b.center();
        if let Some(s) = shapes
            .iter_mut()
            .filter(|s| s.bounds().contains(c))
            .min_by(|a, b| {
                let (sa, sb) = (a.size.x * a.size.y, b.size.x * b.size.y);
                sa.total_cmp(&sb)
            })
        {
            if !s.text.is_empty() {
                s.text.push('\n');
            }
            s.text.push_str(&text);
            continue;
        }
        let lines_n = text.lines().count().max(1) as f32;
        let font = (b.size().y / lines_n * 0.8).clamp(14.0, 160.0);
        let width = (b.size().x + font).max(font * 2.0);
        let mut t = Shape::text(alloc(), b.min, Vec2::new(width, b.size().y.max(font * 1.4)));
        t.text = text;
        t.font_size = font;
        t.text_color = color;
        t.fit_text_height();
        shapes.push(t);
    }

    // 線の端を近くの図形(新しく作ったもの、既存のもの)につなぐ。
    let mut attach_to: Vec<(ElementId, Rect)> = shapes
        .iter()
        .filter(|s| s.kind != ShapeKind::Text)
        .map(|s| (s.id, s.bounds()))
        .collect();
    attach_to.extend_from_slice(targets);
    let find = |p: Vec2| -> Option<ElementId> {
        attach_to
            .iter()
            .filter(|(_, r)| r.expand(ATTACH_DIST).contains(p))
            .min_by(|a, b| {
                let (sa, sb) = (a.1.size(), b.1.size());
                (sa.x * sa.y).total_cmp(&(sb.x * sb.y))
            })
            .map(|(id, _)| *id)
    };
    let mut connectors = Vec::new();
    for (sh, color, width) in lines {
        let (fa, fb) = (find(sh.a), find(sh.b));
        let same = fa.is_some() && fa == fb;
        let end = |p: Vec2, hit: Option<ElementId>| match hit {
            Some(id) if !same => Endpoint::Attached { id, anchor: None },
            _ => Endpoint::Free(p),
        };
        let mut c = Connector::new(alloc(), end(sh.a, fa), end(sh.b, fb));
        c.style = PathStyle::Straight;
        c.color = color;
        c.width = width.max(2.0);
        c.start_cap = if sh.head_a { Cap::Arrow } else { Cap::None };
        c.end_cap = if sh.head_b { Cap::Arrow } else { Cap::None };
        connectors.push(c);
    }

    out.add.extend(shapes.into_iter().map(Element::from));
    out.add.extend(connectors.into_iter().map(Element::from));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 決まった揺れ(乱数を使わず再現できる)を足す。
    fn wobble(points: Vec<Vec2>, amp: f32) -> Vec<Vec2> {
        points
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let t = i as f32;
                p + Vec2::new((t * 1.7).sin(), (t * 2.3).cos()) * amp
            })
            .collect()
    }

    fn poly(corners: &[Vec2], per_edge: usize, close_gap: f32) -> Vec<Vec2> {
        let mut out = Vec::new();
        for i in 0..corners.len() {
            let (a, b) = (corners[i], corners[(i + 1) % corners.len()]);
            for j in 0..per_edge {
                out.push(a.lerp(b, j as f32 / per_edge as f32));
            }
        }
        // 描き終わりが始点に少し届かない。
        let last = corners[0] + (corners[1] - corners[0]).normalize() * -close_gap;
        out.push(last);
        out
    }

    fn stroke(id: u64, pts: Vec<Vec2>) -> Stroke {
        let mut s = Stroke::new(ElementId(id), pts[0], Rgba8::BLACK, 4.0);
        s.points = pts;
        s
    }

    fn kind_of(pts: Vec<Vec2>) -> Option<InkKind> {
        let s = stroke(1, pts);
        recognize(&[&s]).into_iter().next().map(|i| i.kind)
    }

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    #[test]
    fn recognizes_basic_shapes() {
        let rect = poly(
            &[v(0., 0.), v(300., 0.), v(300., 180.), v(0., 180.)],
            20,
            6.0,
        );
        assert_eq!(
            kind_of(wobble(rect, 3.0)),
            Some(InkKind::Shape(ShapeKind::Rect))
        );

        // 始点が辺の途中から始まる四角形でも角は 4 つ。
        let rect2 = poly(
            &[
                v(150., 0.),
                v(300., 0.),
                v(300., 180.),
                v(0., 180.),
                v(0., 0.),
            ],
            20,
            4.0,
        );
        assert_eq!(
            kind_of(wobble(rect2, 2.0)),
            Some(InkKind::Shape(ShapeKind::Rect))
        );

        let circle: Vec<Vec2> = (0..=70)
            .map(|i| {
                let a = i as f32 / 72.0 * std::f32::consts::TAU;
                v(200.0 + a.cos() * 120.0, 200.0 + a.sin() * 90.0)
            })
            .collect();
        assert_eq!(
            kind_of(wobble(circle, 3.0)),
            Some(InkKind::Shape(ShapeKind::Ellipse))
        );

        let tri = poly(&[v(100., 0.), v(200., 170.), v(0., 170.)], 20, 5.0);
        assert_eq!(
            kind_of(wobble(tri, 2.0)),
            Some(InkKind::Shape(ShapeKind::Triangle))
        );

        let tri_down = poly(&[v(0., 0.), v(200., 0.), v(100., 170.)], 20, 5.0);
        assert_eq!(
            kind_of(wobble(tri_down, 2.0)),
            Some(InkKind::Shape(ShapeKind::TriangleDown))
        );

        let diamond = poly(
            &[v(100., 0.), v(200., 80.), v(100., 160.), v(0., 80.)],
            20,
            5.0,
        );
        assert_eq!(
            kind_of(wobble(diamond, 2.0)),
            Some(InkKind::Shape(ShapeKind::Diamond))
        );

        let star: Vec<Vec2> = (0..10)
            .map(|i| {
                let a = i as f32 / 10.0 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                let r = if i % 2 == 0 { 100.0 } else { 40.0 };
                v(a.cos() * r, a.sin() * r)
            })
            .collect();
        assert_eq!(
            kind_of(poly(&star, 10, 3.0)),
            Some(InkKind::Shape(ShapeKind::Star))
        );
    }

    #[test]
    fn recognizes_lines_and_arrows() {
        let line: Vec<Vec2> = (0..30).map(|i| v(i as f32 * 10.0, 50.0)).collect();
        assert_eq!(kind_of(wobble(line.clone(), 1.5)), Some(InkKind::Line));

        // 一筆書きの矢印: 芯 → 片方の矢じり → 先端に戻る → もう片方。
        let mut arrow = line.clone();
        arrow.extend([
            v(270., 30.),
            v(250., 20.),
            v(270., 30.),
            v(290., 50.),
            v(270., 70.),
            v(250., 80.),
        ]);
        assert_eq!(kind_of(arrow), Some(InkKind::Arrow));

        // 矢じりを別のストロークで描く。
        let shaft = stroke(1, line);
        let head = stroke(2, vec![v(260., 25.), v(290., 50.), v(260., 75.)]);
        let items = recognize(&[&shaft, &head]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, InkKind::Arrow);
        assert_eq!(items[0].strokes, vec![ElementId(1), ElementId(2)]);
        let mut next = 100;
        let built = build(&items, &[&shaft, &head], &[], &mut || {
            next += 1;
            ElementId(next)
        });
        assert_eq!(built.remove, vec![ElementId(1), ElementId(2)]);
        let Element::Connector(c) = &built.add[0] else {
            panic!("コネクタではない");
        };
        assert_eq!((c.start_cap, c.end_cap), (Cap::None, Cap::Arrow));
        // 水平に揃う。
        let (Endpoint::Free(a), Endpoint::Free(b)) = (c.from, c.to) else {
            panic!("端がつながっている");
        };
        assert_eq!(a.y, b.y);
    }

    #[test]
    fn scribbles_are_kept() {
        let scribble: Vec<Vec2> = (0..40)
            .map(|i| v(i as f32 * 5.0, if i % 2 == 0 { 0.0 } else { 40.0 }))
            .collect();
        assert_eq!(kind_of(scribble), None);
        assert_eq!(kind_of(vec![v(0., 0.), v(3., 3.)]), None);
    }

    #[test]
    fn build_attaches_lines_and_places_text() {
        let a = stroke(
            1,
            poly(
                &[v(0., 0.), v(200., 0.), v(200., 100.), v(0., 100.)],
                10,
                3.0,
            ),
        );
        let b = stroke(
            2,
            poly(
                &[v(400., 0.), v(600., 0.), v(600., 100.), v(400., 100.)],
                10,
                3.0,
            ),
        );
        let l = stroke(
            3,
            (0..20).map(|i| v(210.0 + i as f32 * 9.5, 52.0)).collect(),
        );
        let t = stroke(4, vec![v(60., 30.), v(140., 70.)]);
        let items = vec![
            InkItem {
                strokes: vec![ElementId(1)],
                kind: InkKind::Shape(ShapeKind::Rect),
            },
            InkItem {
                strokes: vec![ElementId(2)],
                kind: InkKind::Shape(ShapeKind::Rect),
            },
            InkItem {
                strokes: vec![ElementId(3)],
                kind: InkKind::Arrow,
            },
            InkItem {
                strokes: vec![ElementId(4)],
                kind: InkKind::Text("受付".into()),
            },
            // 知らないストロークは無視する。
            InkItem {
                strokes: vec![ElementId(99)],
                kind: InkKind::Line,
            },
        ];
        let mut next = 100;
        let built = build(&items, &[&a, &b, &l, &t], &[], &mut || {
            next += 1;
            ElementId(next)
        });
        assert_eq!(built.remove.len(), 4);
        let shapes: Vec<&Shape> = built.add.iter().filter_map(|e| e.as_shape()).collect();
        assert_eq!(shapes.len(), 2);
        assert_eq!(shapes[0].text, "受付");
        let Element::Connector(c) = built.add.last().unwrap() else {
            panic!("コネクタではない");
        };
        assert_eq!(c.from.attached_to(), Some(shapes[0].id));
        assert_eq!(c.to.attached_to(), Some(shapes[1].id));
        assert_eq!(c.end_cap, Cap::Arrow);
    }
}
