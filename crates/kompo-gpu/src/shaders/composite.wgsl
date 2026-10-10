// Draws a transformed, cropped layer onto a canvas.
// Normal/Add blending uses fixed-function blending (premultiplied);
// other modes read the canvas copy (`dst`) and blend in the shader.

struct QuadParams {
    // canvas-space -> layer UV (inverse transform), rows (affine 2x3 in xyz)
    inv0: vec4<f32>,
    inv1: vec4<f32>,
    // corners in NDC: (x0,y0,x1,y1), (x2,y2,x3,y3)
    c01: vec4<f32>,
    c23: vec4<f32>,
    // crop uv rect: u0, v0, u1, v1
    crop: vec4<f32>,
    // layer size w, h, canvas w, canvas h
    size: vec4<f32>,
    // opacity, blend mode, unused, unused
    misc: vec4<f32>,
};

@group(0) @binding(0) var<uniform> q: QuadParams;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var dst: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    // two triangles: 0 1 2, 2 1 3 (corner order: top-left, top-right, bottom-left, bottom-right)
    var idx = array<u32, 6>(0u, 1u, 2u, 2u, 1u, 3u);
    let k = idx[i];
    var c: vec2<f32>;
    if (k == 0u) { c = q.c01.xy; } else if (k == 1u) { c = q.c01.zw; } else if (k == 2u) { c = q.c23.xy; } else { c = q.c23.zw; }
    var o: VsOut;
    o.pos = vec4<f32>(c, 0.0, 1.0);
    return o;
}

fn layer_color(frag: vec2<f32>) -> vec4<f32> {
    let pc = vec3<f32>(frag, 1.0);
    let lp = vec2<f32>(dot(q.inv0.xyz, pc), dot(q.inv1.xyz, pc));
    let uv = lp / q.size.xy;
    if (uv.x < q.crop.x || uv.y < q.crop.y || uv.x > q.crop.z || uv.y > q.crop.w) {
        return vec4<f32>(0.0);
    }
    return textureSampleLevel(src, samp, uv, 0.0) * q.misc.x;
}

@fragment
fn fs_over(in: VsOut) -> @location(0) vec4<f32> {
    return layer_color(in.pos.xy);
}

fn blend_ch(mode: i32, b: f32, s: f32) -> f32 {
    switch mode {
        case 2: { return b * s; }
        case 3: { return b + s - b * s; }
        case 4: { return select(1.0 - 2.0 * (1.0 - b) * (1.0 - s), 2.0 * b * s, b <= 0.5); }
        case 5: {
            let d = select(sqrt(max(b, 0.0)), ((16.0 * b - 12.0) * b + 4.0) * b, b <= 0.25);
            return select(b + (2.0 * s - 1.0) * (d - b), b - (1.0 - 2.0 * s) * b * (1.0 - b), s <= 0.5);
        }
        case 6: { return select(1.0 - 2.0 * (1.0 - b) * (1.0 - s), 2.0 * b * s, s <= 0.5); }
        case 7: { return min(b, s); }
        case 8: { return max(b, s); }
        case 9: { return abs(b - s); }
        case 10: { return select(min(1.0, b / max(1.0 - s, 1e-6)), 1.0, s >= 1.0); }
        case 11: { return select(1.0 - min(1.0, (1.0 - b) / max(s, 1e-6)), 0.0, s <= 0.0); }
        default: { return s; }
    }
}

@fragment
fn fs_blend(in: VsOut) -> @location(0) vec4<f32> {
    let s = layer_color(in.pos.xy);
    let d = textureLoad(dst, vec2<i32>(in.pos.xy), 0);
    let mode = i32(q.misc.y);
    // un-premultiply
    let sc = select(vec3<f32>(0.0), s.rgb / s.a, s.a > 1e-6);
    let dc = select(vec3<f32>(0.0), d.rgb / d.a, d.a > 1e-6);
    let bc = vec3<f32>(blend_ch(mode, dc.r, sc.r), blend_ch(mode, dc.g, sc.g), blend_ch(mode, dc.b, sc.b));
    // W3C compositing: mix blend result by backdrop alpha, then source-over
    let mixed = (1.0 - d.a) * sc + d.a * bc;
    let rgb = s.a * mixed + (1.0 - s.a) * d.rgb;
    let a = s.a + d.a * (1.0 - s.a);
    return vec4<f32>(rgb, a);
}
