// Decoded frame -> linear working space, premultiplied, using hardware
// bilinear filtering (normalized/float planes). Downscaling places one
// bilinear sample at the center of each 2x2 texel block, which is an exact
// box filter for integer factors (1 sample per plane at 2x, 2x2 at 4x).

struct SourceParams {
    ycbcr0: vec4<f32>,
    ycbcr1: vec4<f32>,
    ycbcr2: vec4<f32>,
    gamut0: vec4<f32>,
    gamut1: vec4<f32>,
    gamut2: vec4<f32>,
    // y_scale, y_offset, c_scale, c_offset
    range: vec4<f32>,
    // out_w, out_h, src_w, src_h
    size: vec4<f32>,
    // chroma shift x, chroma shift y, samples per axis, code scale
    misc: vec4<f32>,
    // layout (0 planar, 1 semiplanar, 2 rgba), transfer id, has_alpha, unused
    mode: vec4<u32>,
    // alpha scale, unused...
    extra: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: SourceParams;
@group(0) @binding(1) var plane0: texture_2d<f32>;
@group(0) @binding(2) var plane1: texture_2d<f32>;
@group(0) @binding(3) var plane2: texture_2d<f32>;
@group(0) @binding(4) var plane3: texture_2d<f32>;
@group(0) @binding(5) var samp: sampler;

TRANSFER_WGSL

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VsOut;
    o.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return o;
}

// Bilinear sample at texel-space position `sp` of a plane of size `dims`, in code values.
fn smp(t: texture_2d<f32>, sp: vec2<f32>) -> vec4<f32> {
    let dims = vec2<f32>(textureDimensions(t));
    return textureSampleLevel(t, samp, sp / dims, 0.0) * p.misc.w;
}

fn convert(sp: vec2<f32>) -> vec4<f32> {
    let lay = p.mode.x;
    var rgb: vec3<f32>;
    var a = 1.0;
    if (lay == 2u) {
        let v = smp(plane0, sp);
        rgb = v.rgb * p.range.x + vec3<f32>(p.range.y);
        if (p.mode.z == 1u) { a = v.a * p.extra.x; }
    } else {
        let cs = vec2<f32>(exp2(p.misc.x), exp2(p.misc.y));
        let y = smp(plane0, sp).r;
        var cb: f32;
        var cr: f32;
        if (lay == 1u) {
            let uv = smp(plane1, sp / cs);
            cb = uv.r;
            cr = uv.g;
        } else {
            cb = smp(plane1, sp / cs).r;
            cr = smp(plane2, sp / cs).r;
        }
        let ycc = vec3<f32>(y * p.range.x + p.range.y, cb * p.range.z + p.range.w, cr * p.range.z + p.range.w);
        rgb = vec3<f32>(dot(p.ycbcr0.xyz, ycc), dot(p.ycbcr1.xyz, ycc), dot(p.ycbcr2.xyz, ycc));
        if (p.mode.z == 1u) { a = smp(plane3, sp).r * p.extra.x; }
    }
    let lin = tf_decode(p.mode.y, rgb);
    let w = vec3<f32>(dot(p.gamut0.xyz, lin), dot(p.gamut1.xyz, lin), dot(p.gamut2.xyz, lin));
    a = clamp(a, 0.0, 1.0);
    return vec4<f32>(w * a, a);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let scale = p.size.zw / p.size.xy;
    let n = i32(p.misc.z);
    if (n <= 1 && scale.x <= 1.0001 && scale.y <= 1.0001) {
        return convert(in.pos.xy * scale);
    }
    let base = (in.pos.xy - vec2<f32>(0.5)) * scale;
    let step = scale / f32(n);
    var acc = vec4<f32>(0.0);
    for (var j = 0; j < n; j = j + 1) {
        for (var i = 0; i < n; i = i + 1) {
            acc = acc + convert(base + (vec2<f32>(f32(i), f32(j)) + vec2<f32>(0.5)) * step);
        }
    }
    return acc / f32(n * n);
}
