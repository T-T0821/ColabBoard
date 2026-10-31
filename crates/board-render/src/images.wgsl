// 画像の描画。1 インスタンス = 1 枚の画像(ワールド座標の矩形にテクスチャを貼る)。

struct Globals {
    view_proj: mat4x4f,
    origin: vec2f,
    viewport: vec2f,
    zoom: f32,
    srgb_output: u32,
    dark: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Instance {
    @location(0) pos: vec2f,
    @location(1) size: vec2f,
};

struct VsOut {
    @builtin(position) clip: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0),
    );
    let uv = corners[vi];
    var out: VsOut;
    out.clip = g.view_proj * vec4f(inst.pos + uv * inst.size, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    // テクスチャは sRGB の値のまま入れてある(図形の色と同じ扱い)。
    let c = textureSample(tex, samp, in.uv);
    var rgb = c.rgb;
    if (g.srgb_output == 1u) {
        rgb = pow(rgb, vec3f(2.2));
    }
    return vec4f(rgb * c.a, c.a);
}
