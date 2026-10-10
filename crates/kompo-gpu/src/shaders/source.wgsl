// Decoded frame (Y'CbCr or RGB, any bit depth) -> linear working space,
// premultiplied alpha. Optionally downscales by an integer-ish factor with a
// box filter (preview resolution).
//
// Template parameters: PLANE_T (texture element type), LOAD (code to turn a
// texel into code values).

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
    // chroma shift x, chroma shift y, taps per axis, code scale
    misc: vec4<f32>,
    // layout (0 planar, 1 semiplanar, 2 rgba), transfer id, has_alpha, msb shift (uint planes)
    mode: vec4<u32>,
    // alpha scale (1 / alpha max code), unused...
    extra: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: SourceParams;
@group(0) @binding(1) var plane0: texture_2d<PLANE_T>;
@group(0) @binding(2) var plane1: texture_2d<PLANE_T>;
@group(0) @binding(3) var plane2: texture_2d<PLANE_T>;
@group(0) @binding(4) var plane3: texture_2d<PLANE_T>;

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

fn ld(t: texture_2d<PLANE_T>, c: vec2<i32>) -> vec4<f32> {
    let dims = vec2<i32>(textureDimensions(t));
    let cc = clamp(c, vec2<i32>(0), dims - vec2<i32>(1));
    let v = textureLoad(t, cc, 0);
    LOAD
}

// Bilinear fetch in texel space (center at +0.5).
fn bil(t: texture_2d<PLANE_T>, pos: vec2<f32>) -> vec4<f32> {
    let q = pos - vec2<f32>(0.5);
    let i = vec2<i32>(floor(q));
    let f = q - floor(q);
    let a = ld(t, i);
    let b = ld(t, i + vec2<i32>(1, 0));
    let c = ld(t, i + vec2<i32>(0, 1));
    let d = ld(t, i + vec2<i32>(1, 1));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// Converts the code values at source position `sp` (texel space) to linear premultiplied RGBA.
fn convert(sp: vec2<f32>) -> vec4<f32> {
    let lay = p.mode.x;
    var rgb: vec3<f32>;
    var a = 1.0;
    if (lay == 2u) {
        let v = bil(plane0, sp);
        rgb = v.rgb * p.range.x + vec3<f32>(p.range.y);
        if (p.mode.z == 1u) { a = v.a * p.extra.x; }
    } else {
        let cs = vec2<f32>(exp2(p.misc.x), exp2(p.misc.y));
        let y = bil(plane0, sp).r;
        var cb: f32;
        var cr: f32;
        if (lay == 1u) {
            let uv = bil(plane1, sp / cs);
            cb = uv.r;
            cr = uv.g;
        } else {
            cb = bil(plane1, sp / cs).r;
            cr = bil(plane2, sp / cs).r;
        }
        let yn = y * p.range.x + p.range.y;
        let cbn = cb * p.range.z + p.range.w;
        let crn = cr * p.range.z + p.range.w;
        let ycc = vec3<f32>(yn, cbn, crn);
        rgb = vec3<f32>(dot(p.ycbcr0.xyz, ycc), dot(p.ycbcr1.xyz, ycc), dot(p.ycbcr2.xyz, ycc));
        if (p.mode.z == 1u) { a = bil(plane3, sp).r * p.extra.x; }
    }
    let lin = tf_decode(p.mode.y, rgb);
    let w = vec3<f32>(dot(p.gamut0.xyz, lin), dot(p.gamut1.xyz, lin), dot(p.gamut2.xyz, lin));
    a = clamp(a, 0.0, 1.0);
    return vec4<f32>(w * a, a);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let scale = p.size.zw / p.size.xy;
    let taps = i32(p.misc.z);
    if (taps <= 1) {
        return convert(in.pos.xy * scale);
    }
    // box filter over the footprint of the output pixel
    let base = (in.pos.xy - vec2<f32>(0.5)) * scale;
    var acc = vec4<f32>(0.0);
    let step = scale / f32(taps);
    for (var j = 0; j < taps; j = j + 1) {
        for (var i = 0; i < taps; i = i + 1) {
            acc = acc + convert(base + (vec2<f32>(f32(i), f32(j)) + vec2<f32>(0.5)) * step);
        }
    }
    return acc / f32(taps * taps);
}
