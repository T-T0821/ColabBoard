//! ColabBoard の GPU 描画エンジン(wgpu)。
//!
//! - ドットグリッド: 全画面三角形 1 枚をフラグメントシェーダで描く
//! - 図形・付箋: 1 要素 = 1 インスタンス。全要素を 1 回の描画命令で描き、
//!   輪郭・枠線・影は SDF で計算する。画面外の要素は頂点シェーダで捨てる。
//!
//! CPU 側の仕事は「ボードが変わったときにインスタンスバッファを送る」ことと
//! 「毎フレーム 96 バイトのカメラ情報を送る」ことだけ。

mod gpu;
mod text;

pub use gpu::{FrameStats, RenderError, Renderer};
pub use wgpu;

use board_core::{Element, Shape, ShapeKind, Stroke};
use bytemuck::{Pod, Zeroable};

/// シェーダの `Globals` と同じ並び(96 バイト)。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    pub origin: [f32; 2],
    pub viewport: [f32; 2],
    pub zoom: f32,
    pub srgb_output: u32,
    pub _pad: [u32; 2],
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
    pub _pad: u32,
}

impl ShapeInstance {
    pub const ATTRIBS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Uint32,
        3 => Uint32,
        4 => Float32,
        5 => Float32,
        6 => Uint32,
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
            radius: s.corner_radius,
            kind: match s.kind {
                ShapeKind::Rect => KIND_RECT,
                ShapeKind::Ellipse => KIND_ELLIPSE,
                ShapeKind::Sticky => KIND_STICKY,
            },
            _pad: 0,
        }
    }
}

pub const KIND_RECT: u32 = 0;
pub const KIND_ELLIPSE: u32 = 1;
pub const KIND_STICKY: u32 = 2;
pub const KIND_SEGMENT: u32 = 3;

impl ShapeInstance {
    fn segment(a: [f32; 2], b: [f32; 2], s: &Stroke) -> Self {
        Self {
            pos: a,
            size: b,
            fill: s.color.to_u32(),
            stroke: 0,
            stroke_width: 0.0,
            radius: s.width * 0.5,
            kind: KIND_SEGMENT,
            _pad: 0,
        }
    }
}

/// 要素をインスタンス列に変換して `out` に足す。手書き線は線分ごとに 1 インスタンス。
pub fn push_instances(e: &Element, out: &mut Vec<ShapeInstance>) {
    match e {
        Element::Shape(s) => out.push(s.into()),
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

pub const GRID_WGSL: &str = include_str!("grid.wgsl");
pub const SHAPES_WGSL: &str = include_str!("shapes.wgsl");

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

    #[test]
    fn struct_sizes_match_shaders() {
        assert_eq!(std::mem::size_of::<Globals>(), 96);
        assert_eq!(std::mem::size_of::<ShapeInstance>(), 40);
    }

    /// WGSL を naga で検証する(GPU なしで CI でも走る)。
    #[test]
    fn shaders_validate() {
        for (name, src) in [("grid", GRID_WGSL), ("shapes", SHAPES_WGSL)] {
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
