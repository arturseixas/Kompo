// One pass of a separable Gaussian blur (edge clamped). Weights are computed
// from sigma; dir is (1,0) or (0,1).
struct P { dir_sigma: vec4<f32>, };  // xy = direction, z = sigma, w = radius
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var src: texture_2d<f32>;

struct VsOut { @builtin(position) pos: vec4<f32>, };
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var v: VsOut;
    v.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return v;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(src));
    let base = vec2<i32>(in.pos.xy);
    let dir = vec2<i32>(p.dir_sigma.xy);
    let sigma = p.dir_sigma.z;
    let r = i32(p.dir_sigma.w);
    var acc = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var i = -r; i <= r; i = i + 1) {
        let w = exp(-f32(i * i) / (2.0 * sigma * sigma));
        let q = clamp(base + dir * i, vec2<i32>(0), dims - vec2<i32>(1));
        acc = acc + w * textureLoad(src, q, 0);
        wsum = wsum + w;
    }
    return acc / wsum;
}
