// Working space (linear, premultiplied) -> output encoding.
// mode.x: 0 = RGB, 1 = Y'CbCr (for encoders); mode.y: transfer id;
// mode.z: 1 = composite over opaque background; mode.w: dither (8-bit targets).

struct OutputParams {
    gamut0: vec4<f32>,
    gamut1: vec4<f32>,
    gamut2: vec4<f32>,
    ycc0: vec4<f32>,
    ycc1: vec4<f32>,
    ycc2: vec4<f32>,
    // y_scale, y_offset, c_scale, c_offset (normalized, inverse of input range)
    range: vec4<f32>,
    background: vec4<f32>,
    mode: vec4<u32>,
};

@group(0) @binding(0) var<uniform> o: OutputParams;
@group(0) @binding(1) var src: texture_2d<f32>;

TRANSFER_WGSL

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var v: VsOut;
    v.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return v;
}

fn hash(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var c = textureLoad(src, vec2<i32>(in.pos.xy), 0);
    if (o.mode.z == 1u) {
        c = c + o.background * (1.0 - c.a);
        c.a = 1.0;
    }
    var rgb = select(vec3<f32>(0.0), c.rgb / c.a, c.a > 1e-6);
    rgb = vec3<f32>(dot(o.gamut0.xyz, rgb), dot(o.gamut1.xyz, rgb), dot(o.gamut2.xyz, rgb));
    rgb = tf_encode(o.mode.y, rgb);
    var outc: vec3<f32>;
    if (o.mode.x == 1u) {
        let ycc = vec3<f32>(dot(o.ycc0.xyz, rgb), dot(o.ycc1.xyz, rgb), dot(o.ycc2.xyz, rgb));
        outc = vec3<f32>(ycc.x * o.range.x + o.range.y, ycc.y * o.range.z + o.range.w, ycc.z * o.range.z + o.range.w);
    } else {
        outc = rgb;
    }
    if (o.mode.w == 1u) {
        outc = outc + (vec3<f32>(hash(in.pos.xy), hash(in.pos.yx + vec2<f32>(17.0)), hash(in.pos.xy * 1.7)) - vec3<f32>(0.5)) / 255.0;
    }
    return vec4<f32>(clamp(outc, vec3<f32>(0.0), vec3<f32>(1.0)), clamp(c.a, 0.0, 1.0));
}
