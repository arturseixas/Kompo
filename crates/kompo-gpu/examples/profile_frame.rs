//! Times the stages of rendering a 4K YUV frame into a 1080p display target.
use kompo_color::InputTransform;
use kompo_core::media::ColorDescription;
use kompo_core::project::WorkingSpace;
use kompo_gpu::graph::{Node, OutputSpec};
use kompo_gpu::{GpuContext, GpuRenderer};
use kompo_media::VideoFrame;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let ctx = GpuContext::new_headless().expect("gpu");
    println!("{}", ctx.describe());
    let mut g = GpuRenderer::new(ctx.clone());
    let (w, h) = (3840u32, 2160u32);
    let spec = OutputSpec::display(WorkingSpace::LinearRec709);
    for (label, ow, oh) in [("4K->4K", 3840, 2160), ("4K->1080p", 1920, 1080), ("4K->540p", 960, 540)] {
        let mut times = Vec::new();
        for _ in 0..5 {
            let f = VideoFrame::from_yuv420p(
                w,
                h,
                vec![120; (w * h) as usize],
                vec![128; (w * h / 4) as usize],
                vec![128; (w * h / 4) as usize],
                ColorDescription::REC709,
            );
            let input = InputTransform::new(f.color, 8, false, WorkingSpace::LinearRec709);
            let node = Node::frame_scaled(Arc::new(f), input, ow, oh);
            let t = Instant::now();
            g.render_display_sync(&node, &spec);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!("{label}: median {:.1} ms", times[2]);
    }
    // upload only
    let data = vec![0u8; (w * h) as usize];
    let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let t = Instant::now();
    for _ in 0..10 {
        ctx.queue.write_texture(
            tex.as_image_copy(),
            &data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        ctx.queue.submit([]);
        ctx.wait_idle();
    }
    println!("upload 4K luma plane (8 MB): {:.1} ms", t.elapsed().as_secs_f64() * 100.0);
}
