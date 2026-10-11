// Applies a 3D LUT (log-shaper lattice) to a premultiplied linear image.
struct P { size: vec4<f32>, };  // x = lattice size
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var lut: texture_3d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VsOut { @builtin(position) pos: vec4<f32>, };
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var v: VsOut;
    v.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return v;
}

const MIN_EXP: f32 = -10.0;
const STOPS: f32 = 16.0;

fn shaper(x: vec3<f32>) -> vec3<f32> {
    let off = exp2(MIN_EXP);
    return clamp((log2(max(x, vec3<f32>(0.0)) + vec3<f32>(off)) - vec3<f32>(MIN_EXP)) / STOPS, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(in.pos.xy), 0);
    if (c.a <= 1e-6) { return c; }
    let s = shaper(c.rgb / c.a);
    let n = p.size.x;
    let uvw = s * ((n - 1.0) / n) + vec3<f32>(0.5 / n);
    let o = textureSampleLevel(lut, samp, uvw, 0.0).rgb;
    return vec4<f32>(o * c.a, c.a);
}
