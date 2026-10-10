// Packs an encoded image (normalized code values: Y'CbCr or R'G'B' in .rgb,
// alpha in .a) into the exact plane layout an encoder expects: 8/16-bit
// samples, planar or interleaved, with 4:2:0 / 4:2:2 chroma averaging.
// One invocation writes one 32-bit word of one plane row.

struct PackParams {
    src_size: vec2<u32>,
    plane_size: vec2<u32>,
    // 0 luma, 1 Cb, 2 Cr, 3 CbCr interleaved, 4 alpha, 5 RGBA interleaved
    kind: u32,
    bytes_per_sample: u32,
    shift_x: u32,
    shift_y: u32,
    max_code: f32,
    msb_shift: u32,
    stride_words: u32,
    offset_words: u32,
};

@group(0) @binding(0) var<uniform> p: PackParams;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<u32>;

fn channels() -> u32 {
    if (p.kind == 3u) { return 2u; }
    if (p.kind == 5u) { return 4u; }
    return 1u;
}

// Value of channel `c` of plane sample at (x, y), averaged over the chroma block.
fn sample_value(x: u32, y: u32, c: u32) -> f32 {
    let bw = 1u << p.shift_x;
    let bh = 1u << p.shift_y;
    var acc = 0.0;
    var n = 0.0;
    for (var j = 0u; j < bh; j = j + 1u) {
        for (var i = 0u; i < bw; i = i + 1u) {
            let sx = min(x * bw + i, p.src_size.x - 1u);
            let sy = min(y * bh + j, p.src_size.y - 1u);
            let v = textureLoad(src, vec2<i32>(i32(sx), i32(sy)), 0);
            var comp: f32;
            switch p.kind {
                case 0u: { comp = v.r; }
                case 1u: { comp = v.g; }
                case 2u: { comp = v.b; }
                case 3u: { comp = select(v.b, v.g, c == 0u); }
                case 4u: { comp = v.a; }
                default: {
                    if (c == 0u) { comp = v.r; } else if (c == 1u) { comp = v.g; } else if (c == 2u) { comp = v.b; } else { comp = v.a; }
                }
            }
            acc = acc + comp;
            n = n + 1.0;
        }
    }
    return acc / n;
}

fn code(v: f32) -> u32 {
    let c = u32(clamp(round(clamp(v, 0.0, 1.0) * p.max_code), 0.0, p.max_code));
    return c << p.msb_shift;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let word = id.x;
    let row = id.y;
    if (word >= p.stride_words || row >= p.plane_size.y) {
        return;
    }
    let ch = channels();
    let bps = p.bytes_per_sample;
    let per_word = 4u / bps;
    var packed = 0u;
    for (var i = 0u; i < per_word; i = i + 1u) {
        let s = word * per_word + i;
        let x = s / ch;
        let c = s % ch;
        if (x < p.plane_size.x) {
            let v = code(sample_value(x, row, c));
            if (bps == 1u) {
                packed = packed | ((v & 0xffu) << (8u * i));
            } else {
                packed = packed | ((v & 0xffffu) << (16u * i));
            }
        }
    }
    out[p.offset_words + row * p.stride_words + word] = packed;
}
