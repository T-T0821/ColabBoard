// 無限キャンバスのドットグリッド。全画面三角形 1 枚をフラグメントシェーダで塗る。

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

struct VsOut {
    @builtin(position) pos: vec4f,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let uv = vec2f(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VsOut;
    out.pos = vec4f(uv * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

fn to_output(c: vec3f) -> vec3f {
    if (g.srgb_output == 1u) {
        return pow(c, vec3f(2.2));
    }
    return c;
}

// 1 段階分のドット。spacing はワールド単位。戻り値はドットの濃さ(0..1)。
fn dots(world: vec2f, spacing: f32) -> f32 {
    let cell = world / spacing;
    let d = (cell - round(cell)) * spacing * g.zoom; // 最寄りの格子点までの距離(px)
    let r = 1.25;
    return clamp(r + 0.5 - length(d), 0.0, 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let world = g.origin + in.pos.xy / g.zoom;

    // 画面上の間隔が 16px 以上になるまで 5 倍ずつ間隔を広げる(ズームアウトで潰れない)。
    var spacing = 24.0;
    loop {
        if (spacing * g.zoom >= 16.0) { break; }
        spacing = spacing * 5.0;
    }
    // 次の段階へ滑らかに切り替える。
    let t = clamp((spacing * g.zoom - 16.0) / 64.0, 0.0, 1.0);
    let a = mix(dots(world, spacing * 5.0), dots(world, spacing), t);

    let bg = vec3f(0.961, 0.961, 0.957);
    let dot = vec3f(0.78, 0.78, 0.77);
    return vec4f(to_output(mix(bg, dot, a)), 1.0);
}
