// 図形・付箋のインスタンス描画。
// 1 インスタンス = 1 要素。四角形 1 枚を出し、輪郭・枠線・影は SDF で計算する。

struct Globals {
    view_proj: mat4x4f,
    origin: vec2f,
    viewport: vec2f,
    zoom: f32,
    srgb_output: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> g: Globals;

const KIND_RECT: u32 = 0u;
const KIND_ELLIPSE: u32 = 1u;
const KIND_STICKY: u32 = 2u;
// 手書き線の 1 区間。pos = 始点、size = 終点、radius = 線幅の半分。
const KIND_SEGMENT: u32 = 3u;

struct Instance {
    @location(0) pos: vec2f,
    @location(1) size: vec2f,
    @location(2) fill: u32,
    @location(3) stroke: u32,
    @location(4) stroke_width: f32,
    @location(5) radius: f32,
    @location(6) kind: u32,
};

struct VsOut {
    @builtin(position) clip: vec4f,
    // 要素中心からの相対座標(ワールド単位)。
    @location(0) local: vec2f,
    @location(1) @interpolate(flat) half: vec2f,
    @location(2) @interpolate(flat) fill: vec4f,
    @location(3) @interpolate(flat) stroke: vec4f,
    @location(4) @interpolate(flat) params: vec2f, // x: stroke_width, y: radius
    @location(5) @interpolate(flat) kind: u32,
};

const SHADOW_OFFSET: vec2f = vec2f(0.0, 4.0);
const SHADOW_BLUR: f32 = 10.0;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    var out: VsOut;

    // 要素ごとに「中心・半径(half)・x 軸の向き・外接矩形」を求める。
    var center: vec2f;
    var half: vec2f;
    var axis = vec2f(1.0, 0.0);
    var lo: vec2f;
    var hi: vec2f;
    var radius = inst.radius;
    if (inst.kind == KIND_SEGMENT) {
        let d = inst.size - inst.pos;
        let len = length(d);
        if (len > 1e-6) {
            axis = d / len;
        }
        center = (inst.pos + inst.size) * 0.5;
        half = vec2f(len * 0.5 + inst.radius, inst.radius);
        lo = min(inst.pos, inst.size) - inst.radius;
        hi = max(inst.pos, inst.size) + inst.radius;
    } else {
        half = inst.size * 0.5;
        center = inst.pos + half;
        lo = inst.pos;
        hi = inst.pos + inst.size;
        radius = min(inst.radius, min(half.x, half.y));
    }

    // 画面外の要素は面積 0 の三角形にしてラスタライズさせない(GPU 側カリング)。
    let view_min = g.origin;
    let view_max = g.origin + g.viewport / g.zoom;
    var margin = inst.stroke_width + 2.0 / g.zoom;
    if (inst.kind == KIND_STICKY) {
        margin = margin + SHADOW_BLUR + SHADOW_OFFSET.y;
    }
    let offscreen = any(hi + margin < view_min) || any(lo - margin > view_max);
    // 1px 未満に縮んだ要素も描かない(ズームアウト時の LOD)。
    let tiny = max(half.x, half.y) * 2.0 * g.zoom < 0.5;
    if (offscreen || tiny) {
        out.clip = vec4f(0.0, 0.0, -2.0, 1.0);
        return out;
    }

    // 0..5 の頂点番号を四角形の角に対応させる(三角形 2 枚)。
    var corners = array<vec2f, 6>(
        vec2f(-1.0, -1.0), vec2f(1.0, -1.0), vec2f(-1.0, 1.0),
        vec2f(-1.0, 1.0), vec2f(1.0, -1.0), vec2f(1.0, 1.0),
    );
    let c = corners[vi];
    let local = c * (half + margin);
    let normal = vec2f(-axis.y, axis.x);
    let world = center + axis * local.x + normal * local.y;

    out.clip = g.view_proj * vec4f(world, 0.0, 1.0);
    out.local = local;
    out.half = half;
    out.fill = unpack4x8unorm(inst.fill);
    out.stroke = unpack4x8unorm(inst.stroke);
    out.params = vec2f(inst.stroke_width, radius);
    out.kind = inst.kind;
    return out;
}

fn sd_round_box(p: vec2f, half: vec2f, r: f32) -> f32 {
    let q = abs(p) - half + r;
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// 楕円の近似 SDF(輪郭付近で十分正確)。
fn sd_ellipse(p: vec2f, half: vec2f) -> f32 {
    let k1 = length(p / half);
    let k2 = length(p / (half * half));
    return k1 * (k1 - 1.0) / max(k2, 1e-6);
}

fn shape_sdf(p: vec2f, half: vec2f, kind: u32, radius: f32) -> f32 {
    if (kind == KIND_ELLIPSE) {
        return sd_ellipse(p, half);
    }
    return sd_round_box(p, half, radius);
}

fn to_output(c: vec3f) -> vec3f {
    if (g.srgb_output == 1u) {
        return pow(c, vec3f(2.2));
    }
    return c;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let px = 1.0 / g.zoom; // 1 物理ピクセルのワールド長
    let d = shape_sdf(in.local, in.half, in.kind, in.params.y);

    // 外形のカバレッジ(アンチエイリアス込み)。
    let coverage = clamp(0.5 - d / px, 0.0, 1.0);

    // 乗算済みアルファで扱う。
    let fill = vec4f(to_output(in.fill.rgb) * in.fill.a, in.fill.a);
    var color = fill;

    // 枠線: 外形の内側 stroke_width の帯。
    let sw = in.params.x;
    if (sw > 0.0 && in.stroke.a > 0.0) {
        let sw_eff = max(sw, px); // 細すぎる線は 1px に保つ
        let inner = clamp(0.5 - (d + sw_eff) / px, 0.0, 1.0);
        let stroke = vec4f(to_output(in.stroke.rgb) * in.stroke.a, in.stroke.a);
        color = mix(stroke, fill, inner);
    }

    var out = color * coverage;

    // 付箋は柔らかい影を下に敷く。
    if (in.kind == KIND_STICKY) {
        let ds = shape_sdf(in.local - SHADOW_OFFSET, in.half, in.kind, in.params.y);
        let shadow = (1.0 - smoothstep(-SHADOW_BLUR * 0.5, SHADOW_BLUR, ds)) * 0.16;
        out = out + vec4f(0.0, 0.0, 0.0, shadow) * (1.0 - out.a);
    }

    if (out.a <= 0.0) {
        discard;
    }
    return out;
}
