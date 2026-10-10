//! GPU (wgpu/WGSL) and CPU (rayon) backends must produce the same image.

use kompo_color::InputTransform;
use kompo_core::effects::BlendMode;
use kompo_core::media::ColorDescription;
use kompo_core::project::WorkingSpace;
use kompo_core::time::Time;
use kompo_gpu::graph::{self, CompositeNode, Layer, Node, OutputSpec};
use kompo_gpu::{CpuImage, CpuRenderer, GpuContext, GpuRenderer, NodeRef};
use kompo_media::frame::{FrameStorage, PixelLayout, SampleType, VideoFrame, next_serial};
use std::sync::{Arc, OnceLock};

fn gpu() -> Option<&'static GpuContext> {
    static CTX: OnceLock<Option<GpuContext>> = OnceLock::new();
    CTX.get_or_init(|| match GpuContext::new_headless() {
        Ok(c) => {
            eprintln!("GPU: {}", c.describe());
            Some(c)
        }
        Err(e) => {
            eprintln!("skipping GPU tests: {e}");
            None
        }
    })
    .as_ref()
}

fn yuv420(w: u32, h: u32) -> VideoFrame {
    let mut y = vec![0u8; (w * h) as usize];
    let cw = w.div_ceil(2);
    let chh = h.div_ceil(2);
    let mut u = vec![0u8; (cw * chh) as usize];
    let mut v = vec![0u8; (cw * chh) as usize];
    for j in 0..h {
        for i in 0..w {
            y[(j * w + i) as usize] = (16 + (i * 219 / w.max(1)) + (j % 7)) as u8;
        }
    }
    for j in 0..chh {
        for i in 0..cw {
            u[(j * cw + i) as usize] = (64 + (i * 128 / cw)) as u8;
            v[(j * cw + i) as usize] = (200 - (j * 128 / chh)) as u8;
        }
    }
    VideoFrame::from_yuv420p(w, h, y, u, v, ColorDescription::REC709)
}

fn yuv420_10bit(w: u32, h: u32) -> VideoFrame {
    let cw = w.div_ceil(2) as usize;
    let chh = h.div_ceil(2) as usize;
    let mut planes = vec![Vec::new(), Vec::new(), Vec::new()];
    for j in 0..h as usize {
        for i in 0..w as usize {
            planes[0].extend_from_slice(&((64 + i * 876 / w as usize + j) as u16).to_le_bytes());
        }
    }
    for j in 0..chh {
        for i in 0..cw {
            planes[1].extend_from_slice(&((300 + i * 400 / cw) as u16).to_le_bytes());
            planes[2].extend_from_slice(&((700 - j * 300 / chh) as u16).to_le_bytes());
        }
    }
    VideoFrame {
        width: w,
        height: h,
        layout: PixelLayout::Planar { chroma_shift: (1, 1), alpha: false },
        sample: SampleType::U16,
        bits: 10,
        shift: 0,
        color: ColorDescription::REC709,
        pts: Time::ZERO,
        duration: Time::ZERO,
        strides: vec![w as usize * 2, cw * 2, cw * 2],
        storage: FrameStorage::Owned(planes),
        serial: next_serial(),
    }
}

fn nv12(w: u32, h: u32) -> VideoFrame {
    let src = yuv420(w, h);
    let cw = w.div_ceil(2) as usize;
    let (u, v) = (src.plane(1), src.plane(2));
    let uv: Vec<u8> = u.iter().zip(v).flat_map(|(a, b)| [*a, *b]).collect();
    VideoFrame {
        width: w,
        height: h,
        layout: PixelLayout::SemiPlanar { chroma_shift: (1, 1) },
        sample: SampleType::U8,
        bits: 8,
        shift: 0,
        color: ColorDescription::REC709,
        pts: Time::ZERO,
        duration: Time::ZERO,
        strides: vec![w as usize, cw * 2],
        storage: FrameStorage::Owned(vec![src.plane(0).to_vec(), uv]),
        serial: next_serial(),
    }
}

fn rgba8(w: u32, h: u32) -> VideoFrame {
    let data: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            [(x * 255 / w) as u8, (y * 255 / h) as u8, 128, if x < w / 2 { 255 } else { 128 }]
        })
        .collect();
    VideoFrame::from_rgba8(w, h, data, ColorDescription::SRGB)
}

fn frame_node(f: VideoFrame) -> NodeRef {
    let bits = f.bits as u32;
    let rgb = f.layout.is_rgb();
    let input = InputTransform::new(f.color, if f.sample == SampleType::F32 { 8 } else { bits }, rgb, WorkingSpace::LinearRec709);
    Node::frame(Arc::new(f), input)
}

fn compare(node: &NodeRef, tol_max: f32, tol_mean: f32) {
    let Some(ctx) = gpu() else { return };
    let mut g = GpuRenderer::new(ctx.clone());
    let mut c = CpuRenderer::new();
    let a = g.render_working(node).unwrap();
    let b = c.render_working(node);
    let max = a.max_diff(&b);
    let mean = a.mean_diff(&b);
    eprintln!("max diff {max:.5}, mean {mean:.6}");
    assert!(max <= tol_max && mean <= tol_mean, "GPU/CPU mismatch: max {max}, mean {mean}");
}

#[test]
fn yuv420p_8bit() {
    compare(&frame_node(yuv420(64, 36)), FILTER_TOL.0, FILTER_TOL.1);
}

#[test]
fn yuv420p_10bit() {
    compare(&frame_node(yuv420_10bit(64, 36)), FILTER_TOL.0, FILTER_TOL.1);
}

// Hardware bilinear filtering quantizes interpolation weights
// (sub-texel precision >= 4 bits per the Vulkan/Metal/D3D specs), so
// interpolated chroma may differ by ~1% from the f32 CPU reference.
const FILTER_TOL: (f32, f32) = (0.02, 0.003);

#[test]
fn nv12_semiplanar() {
    compare(&frame_node(nv12(48, 30)), FILTER_TOL.0, FILTER_TOL.1);
}

#[test]
fn rgba8_with_alpha() {
    compare(&frame_node(rgba8(40, 20)), 0.01, 0.001);
}

#[test]
fn float_exr_like() {
    let data: Vec<f32> = (0..32 * 16).flat_map(|i| [i as f32 / 100.0, 0.5, 4.0, 1.0]).collect();
    compare(&frame_node(VideoFrame::from_rgba_f32(32, 16, &data, ColorDescription::LINEAR_REC709)), 0.02, 0.002);
}

#[test]
fn downscaled_source() {
    let f = yuv420(128, 72);
    let input = InputTransform::new(f.color, 8, false, WorkingSpace::LinearRec709);
    compare(&Node::frame_scaled(Arc::new(f), input, 32, 18), FILTER_TOL.0, FILTER_TOL.1);
}

#[test]
fn composite_transforms_and_blend_modes() {
    let base = frame_node(yuv420(64, 36));
    let top = frame_node(rgba8(40, 20));
    for blend in BlendMode::ALL {
        let t = graph::affine_mul(
            &graph::translate(48.0, 30.0),
            &graph::affine_mul(&graph::rotate_deg(17.0), &graph::affine_mul(&graph::scale(1.3, 0.9), &graph::translate(-20.0, -10.0))),
        );
        let node = Arc::new(Node::Composite(CompositeNode {
            width: 96,
            height: 60,
            background: [0.05, 0.05, 0.05, 1.0],
            layers: vec![
                Layer {
                    input: base.clone(),
                    transform: graph::fit_transform(64.0, 36.0, 1.0, 96.0, 60.0, kompo_core::timeline::ScaleMode::Fit),
                    opacity: 1.0,
                    blend: BlendMode::Normal,
                    crop: [0.0; 4],
                },
                Layer { input: top.clone(), transform: t, opacity: 0.8, blend, crop: [0.1, 0.0, 0.0, 0.2] },
            ],
        }));
        eprintln!("{blend:?}");
        // edges of rotated quads may differ by a pixel between rasterizers: check the mean tightly
        compare(&node, 1.0, 0.004);
    }
}

#[test]
fn output_transform_matches() {
    let Some(ctx) = gpu() else { return };
    let node = frame_node(yuv420(64, 36));
    let spec = OutputSpec { dither: false, ..OutputSpec::display(WorkingSpace::LinearRec709) };
    let mut g = GpuRenderer::new(ctx.clone());
    let mut c = CpuRenderer::new();
    let a = g.render_rgba8(&node, &spec).unwrap();
    let b = c.render_rgba8(&node, &spec);
    let max = a.iter().zip(&b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap();
    assert!(max <= 2, "8-bit output differs by {max}");
}

#[test]
fn rec709_roundtrip_is_identity() {
    // A Rec.709 source shown on the Rec.709 display transform must reproduce its R'G'B' values.
    let mut c = CpuRenderer::new();
    let f = VideoFrame::from_yuv420p(4, 4, vec![180; 16], vec![128; 4], vec![128; 4], ColorDescription::REC709);
    let node = frame_node(f);
    let spec = OutputSpec { dither: false, ..OutputSpec::display(WorkingSpace::LinearRec709) };
    let out = c.render_rgba8(&node, &spec);
    // limited range Y=180 -> (180-16)/219 = 0.7489 -> 191 in full range RGB
    for px in out.chunks(4) {
        assert!(px[..3].iter().all(|v| (*v as i32 - 191).abs() <= 1), "{px:?}");
    }
    if let Some(ctx) = gpu() {
        let mut g = GpuRenderer::new(ctx.clone());
        let out = g.render_rgba8(&node, &spec).unwrap();
        for px in out.chunks(4) {
            assert!(px[..3].iter().all(|v| (*v as i32 - 191).abs() <= 1), "gpu {px:?}");
        }
    }
    let _ = CpuImage::new(1, 1);
}

#[test]
fn texture_pool_is_bounded() {
    let Some(ctx) = gpu() else { return };
    let mut g = GpuRenderer::new(ctx.clone());
    let spec = OutputSpec::display(WorkingSpace::LinearRec709);
    for _ in 0..200 {
        let node = frame_node(yuv420(64, 36)); // new serial each time: no cache hits
        g.render_rgba8(&node, &spec).unwrap();
    }
    let s = g.stats();
    assert!(s.textures_created < 40, "pool keeps allocating: {}", s.textures_created);
    assert!(s.cached_frames <= g.frame_cache_size);
}

#[test]
fn packed_export_planes_match_cpu() {
    use kompo_core::media::MatrixCoefficients;
    use kompo_gpu::pack::PackFormat;
    let Some(ctx) = gpu() else { return };
    let node = frame_node(rgba8(37, 21)); // odd sizes exercise chroma rounding
    let mut g = GpuRenderer::new(ctx.clone());
    let mut c = CpuRenderer::new();
    let formats = [
        (PackFormat::Planar { chroma_shift: (1, 1), bits: 8, alpha: false }, Some((MatrixCoefficients::Bt709, false, 8))),
        (PackFormat::Planar { chroma_shift: (1, 0), bits: 10, alpha: false }, Some((MatrixCoefficients::Bt709, false, 10))),
        (PackFormat::Planar { chroma_shift: (0, 0), bits: 12, alpha: true }, Some((MatrixCoefficients::Bt709, false, 12))),
        (PackFormat::SemiPlanar { bits: 8 }, Some((MatrixCoefficients::Bt709, false, 8))),
        (PackFormat::SemiPlanar { bits: 10 }, Some((MatrixCoefficients::Bt2020Ncl, false, 10))),
        (PackFormat::Rgba { bits: 8 }, None),
        (PackFormat::Rgba { bits: 16 }, None),
    ];
    for (fmt, ycc) in formats {
        let spec = OutputSpec {
            ycbcr: ycc,
            dither: false,
            background: if fmt.has_alpha() { None } else { Some([0.0, 0.0, 0.0, 1.0]) },
            ..OutputSpec::display(WorkingSpace::LinearRec709)
        };
        let gp = g.render_packed(&node, &spec, fmt).unwrap();
        let img = c.render_working(&node);
        let cp = kompo_gpu::pack::pack_cpu(&c.encode(&img, &spec), img.width, img.height, fmt);
        assert_eq!(gp.len(), cp.len());
        for (a, b) in gp.iter().zip(&cp) {
            assert_eq!((a.width, a.height, a.stride), (b.width, b.height, b.stride), "{fmt:?}");
            let bps = if fmt.bits() > 8 { 2 } else { 1 };
            let max_diff = a
                .data
                .chunks(bps)
                .zip(b.data.chunks(bps))
                .map(|(x, y)| {
                    let v = |s: &[u8]| if bps == 1 { s[0] as i32 } else { u16::from_le_bytes([s[0], s[1]]) as i32 };
                    (v(x) - v(y)).abs() >> fmt.msb_shift()
                })
                .max()
                .unwrap();
            // f16 intermediate precision on the GPU: at most a couple of codes at 10-12 bits
            // 16-bit: the GPU working space is f16 (~11 significant bits), CPU is f32
            let tol = match fmt.bits() {
                8 => 1,
                10 => 2,
                12 => 8,
                _ => 48,
            };
            assert!(max_diff <= tol, "{fmt:?}: max code difference {max_diff}");
        }
    }
}
