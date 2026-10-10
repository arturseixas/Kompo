// Transfer functions. Must match kompo-color/src/transfer.rs exactly.
// Ids: 0 linear, 1 sRGB, 2 BT.1886 (2.4), 3 gamma 2.2, 4 gamma 2.6, 5 PQ, 6 HLG,
//      7 ACEScct, 8 ACEScc, 9 ARRI LogC3 EI800, 10 S-Log3, 11 V-Log.

fn tf_spow(x: f32, p: f32) -> f32 { return sign(x) * pow(abs(x), p); }

fn tf_decode1(id: u32, v: f32) -> f32 {
    switch id {
        case 0u: { return v; }
        case 1u: {
            let a = abs(v);
            let l = select(pow((a + 0.055) / 1.055, 2.4), a / 12.92, a <= 0.04045);
            return sign(v) * l;
        }
        case 3u: { return tf_spow(v, 2.2); }
        case 4u: { return tf_spow(v, 2.6); }
        case 5u: {
            let m1 = 2610.0 / 16384.0; let m2 = 2523.0 / 4096.0 * 128.0;
            let c1 = 3424.0 / 4096.0; let c2 = 2413.0 / 4096.0 * 32.0; let c3 = 2392.0 / 4096.0 * 32.0;
            let p = pow(max(v, 0.0), 1.0 / m2);
            let n = max(p - c1, 0.0) / (c2 - c3 * p);
            return pow(n, 1.0 / m1) * 10000.0 / 203.0;
        }
        case 6u: {
            let a = 0.17883277; let b = 0.28466892; let c = 0.55991073;
            let x = max(v, 0.0);
            let e = select((exp((x - c) / a) + b) / 12.0, x * x / 3.0, x <= 0.5);
            return e / 0.26496245;
        }
        case 7u: {
            return select(exp2(v * 17.52 - 9.72), (v - 0.0729055341958355) / 10.5402377416545, v <= 0.155251141552511);
        }
        case 8u: {
            return select(exp2(v * 17.52 - 9.72), (exp2(v * 17.52 - 9.72) - exp2(-16.0)) * 2.0, v < (9.72 - 15.0) / 17.52);
        }
        case 9u: {
            let cut = 0.010591; let a = 5.555556; let b = 0.052272; let c = 0.247190; let d = 0.385537; let e = 5.367655; let f = 0.092809;
            return select((v - f) / e, (pow(10.0, (v - d) / c) - b) / a, v > e * cut + f);
        }
        case 10u: {
            return select((v * 1023.0 - 95.0) * 0.01125 / (171.2102946 - 95.0),
                          pow(10.0, (v * 1023.0 - 420.0) / 261.5) * 0.19 - 0.01,
                          v >= 171.2102946 / 1023.0);
        }
        case 11u: {
            return select((v - 0.125) / 5.6, pow(10.0, (v - 0.598206) / 0.241514) - 0.00873, v >= 0.181);
        }
        default: { return tf_spow(v, 2.4); }
    }
}

fn tf_encode1(id: u32, l: f32) -> f32 {
    switch id {
        case 0u: { return l; }
        case 1u: {
            let a = abs(l);
            let v = select(1.055 * pow(a, 1.0 / 2.4) - 0.055, a * 12.92, a <= 0.0031308);
            return sign(l) * v;
        }
        case 3u: { return tf_spow(l, 1.0 / 2.2); }
        case 4u: { return tf_spow(l, 1.0 / 2.6); }
        case 5u: {
            let m1 = 2610.0 / 16384.0; let m2 = 2523.0 / 4096.0 * 128.0;
            let c1 = 3424.0 / 4096.0; let c2 = 2413.0 / 4096.0 * 32.0; let c3 = 2392.0 / 4096.0 * 32.0;
            let y = max(l * 203.0 / 10000.0, 0.0);
            let p = pow(y, m1);
            return pow((c1 + c2 * p) / (1.0 + c3 * p), m2);
        }
        case 6u: {
            let a = 0.17883277; let b = 0.28466892; let c = 0.55991073;
            let e = max(l * 0.26496245, 0.0);
            return select(a * log(max(12.0 * e - b, 1e-10)) + c, sqrt(3.0 * e), e <= 1.0 / 12.0);
        }
        case 7u: {
            return select((log2(max(l, 1e-10)) + 9.72) / 17.52, 10.5402377416545 * l + 0.0729055341958355, l <= 0.0078125);
        }
        case 8u: {
            if (l <= 0.0) { return (-16.0 + 9.72) / 17.52; }
            if (l < exp2(-15.0)) { return (log2(exp2(-16.0) + l * 0.5) + 9.72) / 17.52; }
            return (log2(l) + 9.72) / 17.52;
        }
        case 9u: {
            let cut = 0.010591; let a = 5.555556; let b = 0.052272; let c = 0.247190; let d = 0.385537; let e = 5.367655; let f = 0.092809;
            return select(e * l + f, c * log2(max(a * l + b, 1e-10)) / log2(10.0) + d, l > cut);
        }
        case 10u: {
            return select((l * (171.2102946 - 95.0) / 0.01125 + 95.0) / 1023.0,
                          (420.0 + log2(max((l + 0.01) / 0.19, 1e-10)) / log2(10.0) * 261.5) / 1023.0,
                          l >= 0.01125);
        }
        case 11u: {
            return select(5.6 * l + 0.125, 0.241514 * log2(max(l + 0.00873, 1e-10)) / log2(10.0) + 0.598206, l >= 0.01);
        }
        default: { return tf_spow(l, 1.0 / 2.4); }
    }
}

fn tf_decode(id: u32, c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(tf_decode1(id, c.r), tf_decode1(id, c.g), tf_decode1(id, c.b));
}

fn tf_encode(id: u32, c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(tf_encode1(id, c.r), tf_encode1(id, c.g), tf_encode1(id, c.b));
}
