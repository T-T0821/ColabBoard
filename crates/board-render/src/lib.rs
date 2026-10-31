//! グラボード の GPU 描画エンジン(wgpu)。
//!
//! - ドットグリッド: 全画面三角形 1 枚をフラグメントシェーダで描く
//! - 図形・付箋: 1 要素 = 1 インスタンス。全要素を 1 回の描画命令で描き、
//!   輪郭・枠線・影は SDF で計算する。画面外の要素は頂点シェーダで捨てる。
//!
//! CPU 側の仕事は「ボードが変わったときにインスタンスバッファを送る」ことと
//! 「毎フレーム 96 バイトのカメラ情報を送る」ことだけ。

mod cull;
mod gpu;
pub mod images;
mod text;

pub use cull::VisibleBlocks;
pub use gpu::{Capture, FrameStats, RenderError, Renderer};
pub use wgpu;

use board_core::{Element, Shape, ShapeKind, Stroke};
use bytemuck::{Pod, Zeroable};

/// ダークモードでキャンバスに直接載る文字・線は、黒に近ければ明るくする
/// (shapes.wgsl の `ink` と同じ基準)。`flip` が偽ならそのまま返す。
pub fn dark_ink([r, g, b, a]: [u8; 4], flip: bool) -> [u8; 4] {
    let lum = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
    if flip && lum < 0.25 {
        [230, 232, 240, a]
    } else {
        [r, g, b, a]
    }
}

/// シェーダの `Globals` と同じ並び(96 バイト)。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    pub origin: [f32; 2],
    pub viewport: [f32; 2],
    pub zoom: f32,
    pub srgb_output: u32,
    /// 1 ならダークモード(背景を暗くし、黒に近い線・文字を明るく描く)。
    pub dark: u32,
    pub _pad: u32,
}

/// シェーダの `Instance` と同じ並び(40 バイト)。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct ShapeInstance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub fill: u32,
    pub stroke: u32,
    pub stroke_width: f32,
    pub radius: f32,
    pub kind: u32,
    /// `FLAG_*` の組み合わせ。
    pub flags: u32,
}

impl ShapeInstance {
    pub const ATTRIBS: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Uint32,
        3 => Uint32,
        4 => Float32,
        5 => Float32,
        6 => Uint32,
        7 => Uint32,
    ];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

impl From<&Shape> for ShapeInstance {
    fn from(s: &Shape) -> Self {
        Self {
            pos: s.pos.to_array(),
            size: s.size.to_array(),
            fill: s.fill.to_u32(),
            stroke: s.stroke.to_u32(),
            stroke_width: s.stroke_width,
            radius: match s.kind {
                // 角の丸みは大きさに合わせる(シェーダーが半分の大きさで頭打ちにする)。
                ShapeKind::RoundRect => s.corner_radius.max(s.size.min_element() * 0.18),
                ShapeKind::Pill => f32::MAX,
                _ => s.corner_radius,
            },
            kind: match s.kind {
                ShapeKind::Rect | ShapeKind::Frame | ShapeKind::Text | ShapeKind::Image => {
                    KIND_RECT
                }
                ShapeKind::Ellipse => KIND_ELLIPSE,
                ShapeKind::Sticky => KIND_STICKY,
                ShapeKind::Triangle => KIND_TRIANGLE,
                ShapeKind::Diamond => KIND_DIAMOND,
                ShapeKind::Star => KIND_STAR,
                ShapeKind::Hexagon => KIND_HEXAGON,
                ShapeKind::Parallelogram => KIND_PARALLELOGRAM,
                ShapeKind::Stamp => KIND_STAMP,
                ShapeKind::RoundRect | ShapeKind::Pill => KIND_RECT,
                ShapeKind::Heart => KIND_HEART,
                ShapeKind::Cloud => KIND_CLOUD,
                ShapeKind::Speech => KIND_SPEECH,
                ShapeKind::Cylinder => KIND_CYLINDER,
                ShapeKind::Document => KIND_DOCUMENT,
                ShapeKind::Delay => KIND_DELAY,
                ShapeKind::PredefinedProcess => KIND_PREDEFINED,
                ShapeKind::InternalStorage => KIND_STORAGE,
                k => board_core::shape::POLYGONS
                    .iter()
                    .position(|p| p.0 == k)
                    .map_or(KIND_RECT, |i| KIND_POLYGON + i as u32),
            },
            flags: if s.kind == ShapeKind::Frame {
                FLAG_SURFACE
            } else {
                0
            },
        }
    }
}

/// 下地になる面(フレーム・コネクタのラベル下)。ダークモードでは白い塗りを暗い面の色にする。
pub const FLAG_SURFACE: u32 = 1;

pub const KIND_RECT: u32 = 0;
pub const KIND_ELLIPSE: u32 = 1;
pub const KIND_STICKY: u32 = 2;
pub const KIND_SEGMENT: u32 = 3;
pub const KIND_TRIANGLE: u32 = 4;
pub const KIND_DIAMOND: u32 = 5;
pub const KIND_STAR: u32 = 6;
pub const KIND_HEXAGON: u32 = 7;
pub const KIND_PARALLELOGRAM: u32 = 8;
/// スタンプ: 影付きの丸。
pub const KIND_STAMP: u32 = 9;
pub const KIND_HEART: u32 = 10;
pub const KIND_CLOUD: u32 = 11;
pub const KIND_SPEECH: u32 = 12;
pub const KIND_CYLINDER: u32 = 13;
pub const KIND_DOCUMENT: u32 = 14;
pub const KIND_DELAY: u32 = 15;
pub const KIND_PREDEFINED: u32 = 16;
pub const KIND_STORAGE: u32 = 17;
/// 多角形の図形: `KIND_POLYGON + board_core::shape::POLYGONS での番号`。
pub const KIND_POLYGON: u32 = 32;

impl ShapeInstance {
    fn segment(a: [f32; 2], b: [f32; 2], s: &Stroke) -> Self {
        Self::line(a, b, s.color, s.width)
    }

    fn line(a: [f32; 2], b: [f32; 2], color: board_core::Rgba8, width: f32) -> Self {
        Self {
            pos: a,
            size: b,
            fill: color.to_u32(),
            stroke: 0,
            stroke_width: 0.0,
            radius: width * 0.5,
            kind: KIND_SEGMENT,
            flags: 0,
        }
    }
}

/// 要素をインスタンス列に変換して `out` に足す。手書き線は線分ごとに 1 インスタンス。
pub fn push_instances(e: &Element, out: &mut Vec<ShapeInstance>) {
    match e {
        // 文字だけの要素は図形を描かない(文字は文字描画で出す)。
        // 画像は画像描画で出す。
        Element::Shape(s) if matches!(s.kind, ShapeKind::Text | ShapeKind::Image) => {}
        Element::Shape(s) => out.push(s.into()),
        Element::Connector(c) => {
            let line = |a: board_core::Vec2, b: board_core::Vec2| {
                ShapeInstance::line(a.to_array(), b.to_array(), c.color, c.width)
            };
            out.extend(c.path.windows(2).map(|w| line(w[0], w[1])));
            // ラベルの下は線を隠す。
            if let Some(tb) = c.label_box() {
                let r = tb.rect.expand(2.0);
                let mut bg = Shape::rect(c.id, r.min, r.size(), board_core::Rgba8::WHITE);
                bg.stroke_width = 0.0;
                bg.corner_radius = 4.0;
                let mut inst = ShapeInstance::from(&bg);
                inst.flags = FLAG_SURFACE;
                out.push(inst);
            }
            for (cap, at_end) in [(c.start_cap, false), (c.end_cap, true)] {
                if cap == board_core::Cap::Arrow
                    && let Some(lines) = c.arrow_lines(at_end)
                {
                    out.extend(lines.iter().map(|(a, b)| line(*a, *b)));
                }
            }
        }
        Element::Stroke(s) => match s.points.as_slice() {
            [] => {}
            [p] => out.push(ShapeInstance::segment(p.to_array(), p.to_array(), s)),
            pts => out.extend(
                pts.windows(2)
                    .map(|w| ShapeInstance::segment(w[0].to_array(), w[1].to_array(), s)),
            ),
        },
    }
}

/// 描画の 1 段。図形インスタンス `instances` を描いてから、要素 `elements` のうち
/// 文字を持つものの文字を描く。
///
/// 文字は glyphon で別に描くので、全図形の後にまとめて描くと「後ろの付箋の文字が
/// 手前の図形の上に出る」。そこで、文字の上に後ろの要素が重なる所でだけ段を分ける。
/// 重なりがなければ 1 段(描画命令は従来どおり図形 1 回 + 文字 1 回)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    pub instances: std::ops::Range<u32>,
    pub elements: std::ops::Range<usize>,
}

/// 段の上限。超えた分は最後の段にまとめる(文字が手前に出ることがあるだけで、描画は続く)。
pub const MAX_LAYERS: usize = 64;

/// 空間ハッシュのセルの大きさ(ワールド単位)。
const CELL: f32 = 512.0;
/// これより多くのセルにまたがる文字領域は、ハッシュに入れず総当たりで調べる。
const MAX_CELLS: i64 = 64;

fn cells(r: &board_core::Rect) -> Option<(i32, i32, i32, i32)> {
    let x0 = (r.min.x / CELL).floor() as i32;
    let y0 = (r.min.y / CELL).floor() as i32;
    let x1 = (r.max.x / CELL).floor() as i32;
    let y1 = (r.max.y / CELL).floor() as i32;
    let n = (i64::from(x1) - i64::from(x0) + 1) * (i64::from(y1) - i64::from(y0) + 1);
    (n <= MAX_CELLS).then_some((x0, y0, x1, y1))
}

/// 今の段にある文字の領域。
#[derive(Default)]
struct TextRegions {
    grid: std::collections::HashMap<(i32, i32), Vec<board_core::Rect>>,
    large: Vec<board_core::Rect>,
}

impl TextRegions {
    fn clear(&mut self) {
        self.grid.clear();
        self.large.clear();
    }

    fn is_empty(&self) -> bool {
        self.grid.is_empty() && self.large.is_empty()
    }

    fn insert(&mut self, r: board_core::Rect) {
        match cells(&r) {
            Some((x0, y0, x1, y1)) => {
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        self.grid.entry((x, y)).or_default().push(r);
                    }
                }
            }
            None => self.large.push(r),
        }
    }

    fn overlaps(&self, r: &board_core::Rect) -> bool {
        if self.large.iter().any(|t| t.intersects(r)) {
            return true;
        }
        match cells(r) {
            Some((x0, y0, x1, y1)) => (y0..=y1).any(|y| {
                (x0..=x1).any(|x| {
                    self.grid
                        .get(&(x, y))
                        .is_some_and(|v| v.iter().any(|t| t.intersects(r)))
                })
            }),
            None => self.grid.values().flatten().any(|t| t.intersects(r)),
        }
    }
}

/// 画像の描画(画像要素かアイコン)があるか。
pub fn is_image(e: &Element) -> bool {
    matches!(e, Element::Shape(s) if s.kind == ShapeKind::Image || !s.icon.is_empty())
}

/// インスタンス列を作りながら段を決める。
pub fn build_layers(elements: &[Element], out: &mut Vec<ShapeInstance>) -> Vec<Layer> {
    out.clear();
    let mut layers = Vec::new();
    let mut regions = TextRegions::default();
    let mut start_inst = 0u32;
    let mut start_elem = 0usize;
    for (i, e) in elements.iter().enumerate() {
        let bounds = e.bounds();
        if !regions.is_empty() && layers.len() + 1 < MAX_LAYERS && regions.overlaps(&bounds) {
            let end = out.len() as u32;
            layers.push(Layer {
                instances: start_inst..end,
                elements: start_elem..i,
            });
            start_inst = end;
            start_elem = i;
            regions.clear();
        }
        push_instances(e, out);
        // 段の最後に描くもの(文字・画像)の上に後ろの要素が重なったら段を分ける。
        if !e.text().is_empty() || is_image(e) {
            regions.insert(bounds);
        }
    }
    layers.push(Layer {
        instances: start_inst..out.len() as u32,
        elements: start_elem..elements.len(),
    });
    layers
}

/// 前回と今回のインスタンス列で、違いのある範囲(転送が必要な範囲)。同じなら `None`。
pub fn changed_range(
    old: &[ShapeInstance],
    new: &[ShapeInstance],
) -> Option<std::ops::Range<usize>> {
    let common = old.len().min(new.len());
    let first = (0..common).find(|&i| old[i] != new[i]).unwrap_or(common);
    if first == common && old.len() == new.len() {
        return None;
    }
    if old.len() != new.len() {
        // 要素の追加・削除で後ろがずれる。最初の違いから末尾まで送る。
        return (first < new.len())
            .then_some(first..new.len())
            .or(Some(new.len()..new.len()));
    }
    let last = (first..common)
        .rev()
        .find(|&i| old[i] != new[i])
        .unwrap_or(first);
    Some(first..last + 1)
}

pub const GRID_WGSL: &str = include_str!("grid.wgsl");
const SHAPES_WGSL_BASE: &str = include_str!("shapes.wgsl");

/// 図形のシェーダー。多角形の頂点表(`board_core::shape::POLYGONS`)と雲の形を
/// コアの定義から WGSL に書き出して足す(当たり判定と描画の形を一致させるため)。
pub fn shapes_wgsl() -> String {
    use std::fmt::Write;
    let v = |p: [f32; 2]| format!("vec2f({:?}, {:?})", p[0], p[1]);
    let mut s = String::from(SHAPES_WGSL_BASE);
    s.push_str("\n// ---- 以下は board-render/src/lib.rs の shapes_wgsl() が生成 ----\n");
    s.push_str("fn polygon_of(kind: u32) -> Polygon {\n    var out: Polygon;\n    switch kind {\n");
    for (i, (_, pts)) in board_core::shape::POLYGONS.iter().enumerate() {
        let mut arr: Vec<String> = pts.iter().map(|&p| v(p)).collect();
        arr.resize(MAX_POLYGON, "vec2f(0.0, 0.0)".into());
        let _ = writeln!(
            s,
            "        case {}u: {{ out.v = array<vec2f, {MAX_POLYGON}>({}); out.n = {}u; }}",
            KIND_POLYGON + i as u32,
            arr.join(", "),
            pts.len()
        );
    }
    s.push_str("        default: { out.n = 0u; }\n    }\n    return out;\n}\n");
    s.push_str("fn sd_cloud(p: vec2f, half: vec2f) -> f32 {\n    var d = 1e9;\n");
    for (c, r) in board_core::shape::CLOUD_LOBES {
        let _ = writeln!(
            s,
            "    d = min(d, sd_ellipse(p - {} * half, {} * half));",
            v(c),
            v(r)
        );
    }
    s.push_str("    return d;\n}\n");
    let tail: Vec<String> = board_core::shape::SPEECH_TAIL
        .iter()
        .map(|&p| v(p))
        .collect();
    let _ = writeln!(
        s,
        "const SPEECH_BODY: f32 = {:?};\nfn speech_tail() -> Polygon {{\n    var out: Polygon;\n    out.v = array<vec2f, {MAX_POLYGON}>({}{});\n    out.n = 3u;\n    return out;\n}}",
        board_core::shape::SPEECH_BODY,
        tail.join(", "),
        ", vec2f(0.0, 0.0)".repeat(MAX_POLYGON - 3)
    );
    s
}

/// シェーダーの多角形の頂点数の上限(shapes.wgsl の `Polygon` と同じ)。
const MAX_POLYGON: usize = 12;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_becomes_segments() {
        use board_core::{ElementId, Rgba8, Vec2};
        let mut st = Stroke::new(ElementId(1), Vec2::ZERO, Rgba8::BLACK, 4.0);
        let mut out = Vec::new();
        push_instances(&Element::Stroke(st.clone()), &mut out);
        assert_eq!(out.len(), 1);
        st.push_point(Vec2::new(10.0, 0.0), 0.0);
        st.push_point(Vec2::new(20.0, 5.0), 0.0);
        out.clear();
        push_instances(&Element::Stroke(st), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].pos, [10.0, 0.0]);
        assert_eq!(out[1].size, [20.0, 5.0]);
        assert_eq!(out[1].radius, 2.0);
        assert_eq!(out[1].kind, KIND_SEGMENT);
    }

    fn sticky(id: u64, x: f32, text: &str) -> Element {
        use board_core::{ElementId, Rgba8, Vec2};
        let mut s = Shape::sticky(ElementId(id), Vec2::new(x, 0.0), Rgba8::WHITE);
        s.text = text.into();
        s.into()
    }

    #[test]
    fn single_layer_without_overlap() {
        let els = vec![
            sticky(1, 0.0, "a"),
            sticky(2, 1000.0, "b"),
            sticky(3, 2000.0, ""),
        ];
        let mut out = Vec::new();
        let layers = build_layers(&els, &mut out);
        assert_eq!(
            layers,
            vec![Layer {
                instances: 0..3,
                elements: 0..3
            }]
        );
    }

    #[test]
    fn splits_when_later_shape_covers_text() {
        // 2 番目が 1 番目の文字の上に重なる → 段を分ける。3 番目は重ならない。
        let els = vec![
            sticky(1, 0.0, "a"),
            sticky(2, 50.0, ""),
            sticky(3, 5000.0, "c"),
        ];
        let mut out = Vec::new();
        let layers = build_layers(&els, &mut out);
        assert_eq!(
            layers,
            vec![
                Layer {
                    instances: 0..1,
                    elements: 0..1
                },
                Layer {
                    instances: 1..3,
                    elements: 1..3
                },
            ]
        );
    }

    #[test]
    fn layer_count_is_capped() {
        let els: Vec<Element> = (0..200).map(|i| sticky(i, i as f32 * 10.0, "x")).collect();
        let mut out = Vec::new();
        let layers = build_layers(&els, &mut out);
        assert_eq!(layers.len(), MAX_LAYERS);
        assert_eq!(layers.last().unwrap().elements.end, 200);
        assert_eq!(layers.last().unwrap().instances.end, 200);
    }

    #[test]
    fn changed_ranges() {
        let a = ShapeInstance::zeroed();
        let mut b = a;
        b.kind = 9;
        assert_eq!(changed_range(&[a, a, a], &[a, a, a]), None);
        assert_eq!(changed_range(&[a, a, a], &[a, b, a]), Some(1..2));
        assert_eq!(changed_range(&[b, a, b], &[a, a, a]), Some(0..3));
        assert_eq!(changed_range(&[a, a], &[a, a, b]), Some(2..3));
        assert_eq!(changed_range(&[a, b, a], &[a, a]), Some(1..2));
        assert_eq!(changed_range(&[a, a, a], &[a, a]), Some(2..2));
    }

    #[test]
    fn struct_sizes_match_shaders() {
        assert_eq!(std::mem::size_of::<Globals>(), 96);
        assert_eq!(dark_ink([31, 31, 31, 255], true), [230, 232, 240, 255]);
        assert_eq!(dark_ink([31, 31, 31, 255], false), [31, 31, 31, 255]);
        assert_eq!(dark_ink([255, 243, 163, 255], true), [255, 243, 163, 255]);
        assert_eq!(std::mem::size_of::<ShapeInstance>(), 40);
    }

    /// WGSL を naga で検証する(GPU なしで CI でも走る)。
    #[test]
    fn shaders_validate() {
        for (name, src) in [
            ("grid", GRID_WGSL),
            ("shapes", shapes_wgsl().as_str()),
            ("images", crate::images::IMAGES_WGSL),
        ] {
            let module = naga::front::wgsl::parse_str(src)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(src)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }
}
