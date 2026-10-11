//! wgpu backend of the render graph.

use crate::context::{GpuContext, GpuError};
use crate::graph::{CompositeNode, FrameNode, Node, NodeRef, OutputSpec, affine_apply, affine_inverse};
use crate::image::CpuImage;
use kompo_core::effects::BlendMode;
use kompo_media::{PixelLayout, SampleType};
use std::collections::HashMap;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Working texture format (linear, premultiplied).
pub const WORKING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SourceParams {
    ycbcr: [[f32; 4]; 3],
    gamut: [[f32; 4]; 3],
    range: [f32; 4],
    size: [f32; 4],
    misc: [f32; 4],
    mode: [u32; 4],
    extra: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadParams {
    inv0: [f32; 4],
    inv1: [f32; 4],
    c01: [f32; 4],
    c23: [f32; 4],
    crop: [f32; 4],
    size: [f32; 4],
    misc: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PackParams {
    src_size: [u32; 2],
    plane_size: [u32; 2],
    kind: u32,
    bytes_per_sample: u32,
    shift_x: u32,
    shift_y: u32,
    max_code: f32,
    msb_shift: u32,
    stride_words: u32,
    offset_words: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OutputParams {
    gamut: [[f32; 4]; 3],
    ycc: [[f32; 4]; 3],
    range: [f32; 4],
    background: [f32; 4],
    mode: [u32; 4],
}

fn row4(r: [f32; 3]) -> [f32; 4] {
    [r[0], r[1], r[2], 0.0]
}

/// A pooled texture.
pub struct Tex {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    upload: bool,
}

type TexKey = (u32, u32, wgpu::TextureFormat, bool);

/// Reuses intermediate textures across frames; unused ones are released
/// after a while so memory stays bounded during long sessions.
struct TexturePool {
    free: HashMap<TexKey, Vec<(Tex, u64)>>,
    frame: u64,
    created: u64,
}

impl TexturePool {
    fn acquire(&mut self, device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat) -> Tex {
        self.acquire_kind(device, w, h, format, false)
    }

    /// `upload`: sampled + copy-destination only (any format, e.g. R8Unorm planes).
    fn acquire_kind(&mut self, device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat, upload: bool) -> Tex {
        if let Some(v) = self.free.get_mut(&(w, h, format, upload))
            && let Some((t, _)) = v.pop()
        {
            return t;
        }
        self.created += 1;
        let usage = if upload {
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST
        } else {
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kompo pooled"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Tex { texture, view, width: w, height: h, format, upload }
    }

    fn release(&mut self, t: Tex) {
        let frame = self.frame;
        self.free.entry((t.width, t.height, t.format, t.upload)).or_default().push((t, frame));
    }

    /// Drops textures unused for `max_age` frames.
    fn trim(&mut self, max_age: u64) {
        let frame = self.frame;
        for v in self.free.values_mut() {
            v.retain(|(_, f)| frame - *f <= max_age);
        }
        self.free.retain(|_, v| !v.is_empty());
    }

    fn bytes(&self) -> u64 {
        self.free.values().flatten().map(|(t, _)| t.width as u64 * t.height as u64 * 8).sum()
    }
}

/// Uploaded planes + converted texture of a decoded frame, cached by serial.
struct CachedFrame {
    serial: u64,
    size: (u32, u32),
    tex: Arc<Tex>,
    last_used: u64,
}

pub struct GpuStats {
    pub textures_created: u64,
    pub pool_bytes: u64,
    pub cached_frames: usize,
    pub uploads: u64,
}

pub struct GpuRenderer {
    pub ctx: GpuContext,
    source_bgl: wgpu::BindGroupLayout,
    source_sampled: wgpu::RenderPipeline,
    source_uint_bgl: wgpu::BindGroupLayout,
    source_uint: wgpu::RenderPipeline,
    /// R16Unorm & co. are available: 16-bit planes are filtered in hardware.
    norm16: bool,
    quad_bgl: wgpu::BindGroupLayout,
    quad_over: wgpu::RenderPipeline,
    quad_add: wgpu::RenderPipeline,
    quad_blend: wgpu::RenderPipeline,
    output_bgl: wgpu::BindGroupLayout,
    output_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    output_module: wgpu::ShaderModule,
    output_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    dummy_float: wgpu::TextureView,
    dummy_uint: wgpu::TextureView,
    pool: TexturePool,
    frames: Vec<CachedFrame>,
    frame_index: u64,
    uploads: u64,
    /// How many converted source frames to keep (scrubbing back and forth).
    pub frame_cache_size: usize,
    pack: Option<wgpu::ComputePipeline>,
    lut_pl: Option<(wgpu::BindGroupLayout, wgpu::RenderPipeline)>,
    blur_pl: Option<(wgpu::BindGroupLayout, wgpu::RenderPipeline)>,
    luts: Vec<(Arc<crate::ops::Lut3D>, wgpu::TextureView)>,
}

fn transfer_src(src: &str) -> String {
    src.replace("TRANSFER_WGSL", kompo_color::transfer::WGSL)
}

impl GpuRenderer {
    pub fn new(ctx: GpuContext) -> Self {
        let device = &ctx.device;
        let tex_entry = |binding: u32, sample_type: wgpu::TextureSampleType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let uniform_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let nf = wgpu::TextureSampleType::Float { filterable: false };
        let ff = wgpu::TextureSampleType::Float { filterable: true };
        let sampler_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let source_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("source sampled"),
            entries: &[uniform_entry(0), tex_entry(1, ff), tex_entry(2, ff), tex_entry(3, ff), tex_entry(4, ff), sampler_entry(5)],
        });
        let u = wgpu::TextureSampleType::Uint;
        let source_uint_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("source uint"),
            entries: &[uniform_entry(0), tex_entry(1, u), tex_entry(2, u), tex_entry(3, u), tex_entry(4, u)],
        });
        let source_sampled = {
            let src = transfer_src(include_str!("shaders/source_sampled.wgsl"));
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("source sampled"),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&source_bgl)],
                immediate_size: 0,
            });
            fullscreen_pipeline(device, &module, &layout, "fs_main", WORKING_FORMAT, None)
        };
        let source_uint = {
            let src = transfer_src(include_str!("shaders/source.wgsl"))
                .replace("PLANE_T", "u32")
                .replace("LOAD", "return vec4<f32>(v >> vec4<u32>(p.mode.w));");
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("source uint"),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&source_uint_bgl)],
                immediate_size: 0,
            });
            fullscreen_pipeline(device, &module, &layout, "fs_main", WORKING_FORMAT, None)
        };
        let norm16 = ctx.features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);

        let quad_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad"),
            entries: &[
                uniform_entry(0),
                tex_entry(1, ff),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex_entry(3, nf),
            ],
        });
        let quad_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/composite.wgsl").into()),
        });
        let quad_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&quad_bgl)],
            immediate_size: 0,
        });
        let over = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;
        let add = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: over.alpha,
        };
        let quad_over = fullscreen_pipeline(device, &quad_module, &quad_layout, "fs_over", WORKING_FORMAT, Some(over));
        let quad_add = fullscreen_pipeline(device, &quad_module, &quad_layout, "fs_over", WORKING_FORMAT, Some(add));
        let quad_blend = fullscreen_pipeline(device, &quad_module, &quad_layout, "fs_blend", WORKING_FORMAT, None);

        let output_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("output"),
            entries: &[uniform_entry(0), tex_entry(1, nf)],
        });
        let output_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("output"),
            source: wgpu::ShaderSource::Wgsl(transfer_src(include_str!("shaders/output.wgsl")).into()),
        });
        let output_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&output_bgl)],
            immediate_size: 0,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy = |format: wgpu::TextureFormat| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("dummy"),
                    size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let dummy_float = dummy(wgpu::TextureFormat::R8Unorm);
        let dummy_uint = dummy(wgpu::TextureFormat::R16Uint);
        Self {
            source_bgl,
            source_sampled,
            source_uint_bgl,
            source_uint,
            norm16,
            quad_bgl,
            quad_over,
            quad_add,
            quad_blend,
            output_bgl,
            output_pipelines: HashMap::new(),
            output_module,
            output_layout,
            sampler,
            dummy_float,
            dummy_uint,
            pool: TexturePool { free: HashMap::new(), frame: 0, created: 0 },
            frames: Vec::new(),
            frame_index: 0,
            uploads: 0,
            frame_cache_size: 8,
            pack: None,
            lut_pl: None,
            blur_pl: None,
            luts: Vec::new(),
            ctx,
        }
    }

    pub fn stats(&self) -> GpuStats {
        GpuStats {
            textures_created: self.pool.created,
            pool_bytes: self.pool.bytes(),
            cached_frames: self.frames.len(),
            uploads: self.uploads,
        }
    }

    /// Drops cached frames and pooled textures.
    pub fn purge(&mut self) {
        self.frames.clear();
        self.pool.free.clear();
    }

    fn output_pipeline(&mut self, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        if let Some(p) = self.output_pipelines.get(&format) {
            return p.clone();
        }
        let p = fullscreen_pipeline(&self.ctx.device, &self.output_module, &self.output_layout, "fs_main", format, None);
        self.output_pipelines.insert(format, p.clone());
        p
    }

    fn upload_plane(&mut self, data: &[u8], stride: usize, w: u32, h: u32, format: wgpu::TextureFormat) -> Tex {
        let tex = self.pool.acquire_kind(&self.ctx.device, w, h, format, true);
        let bpp = format.block_copy_size(None).unwrap_or(1) as usize;
        let needed = stride * (h as usize - 1) + w as usize * bpp;
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data[..needed.min(data.len())],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(stride as u32), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.uploads += 1;
        tex
    }

    fn plane_format(&self, sample: SampleType, channels: usize) -> wgpu::TextureFormat {
        use wgpu::TextureFormat as F;
        match (sample, channels) {
            (SampleType::U8, 1) => F::R8Unorm,
            (SampleType::U8, 2) => F::Rg8Unorm,
            (SampleType::U8, _) => F::Rgba8Unorm,
            (SampleType::U16, 1) if self.norm16 => F::R16Unorm,
            (SampleType::U16, 2) if self.norm16 => F::Rg16Unorm,
            (SampleType::U16, _) if self.norm16 => F::Rgba16Unorm,
            (SampleType::U16, 1) => F::R16Uint,
            (SampleType::U16, 2) => F::Rg16Uint,
            (SampleType::U16, _) => F::Rgba16Uint,
            (SampleType::F16 | SampleType::F32, _) => F::Rgba16Float,
        }
    }

    /// Samples per axis used when converting `src` texels to `dst` pixels
    /// (one bilinear sample covers 2x2 texels). Shared with the CPU backend.
    pub fn source_samples(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> u32 {
        crate::cpu::source_samples(src_w, src_h, dst_w, dst_h)
    }

    fn eval_frame(&mut self, enc: &mut wgpu::CommandEncoder, n: &FrameNode, memo: &mut Memo) -> Arc<Tex> {
        let f = &n.frame;
        if let Some(c) = self.frames.iter_mut().find(|c| c.serial == f.serial && c.size == (n.width, n.height)) {
            c.last_used = self.frame_index;
            return c.tex.clone();
        }
        let uint = f.sample == SampleType::U16 && !self.norm16;
        let planes = f.layout.plane_count();
        let mut uploads = Vec::with_capacity(4);
        for i in 0..planes {
            let (pw, ph) = f.plane_size(i);
            let fmt = self.plane_format(f.sample, f.layout.plane_channels(i));
            let tex = if f.sample == SampleType::F32 {
                // f32 is not filterable everywhere: upload as f16
                let row_floats = pw as usize * 4;
                let mut half_data = Vec::with_capacity(row_floats * ph as usize);
                for y in 0..ph as usize {
                    let row = &f.plane(i)[y * f.stride(i)..y * f.stride(i) + row_floats * 4];
                    half_data.extend(row.chunks_exact(4).map(|b| half::f16::from_f32(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))));
                }
                self.upload_plane(bytemuck::cast_slice(&half_data), row_floats * 2, pw, ph, fmt)
            } else {
                self.upload_plane(f.plane(i), f.stride(i), pw, ph, fmt)
            };
            uploads.push(tex);
        }
        let it = &n.input;
        let float_sample = matches!(f.sample, SampleType::F16 | SampleType::F32);
        let (range, code_scale, alpha_scale) = if float_sample {
            ([1.0, 0.0, 1.0, 0.0], 1.0, 1.0)
        } else {
            let range = [it.range.y_scale, it.range.y_offset, it.range.c_scale, it.range.c_offset];
            match f.sample {
                SampleType::U8 => (range, 255.0, 1.0 / 255.0),
                _ => {
                    let amax = ((1u32 << f.bits) - 1) as f32;
                    // normalized 16-bit: value = (code << shift) / 65535
                    let code = if uint { 1.0 } else { 65535.0 / (1u32 << f.shift) as f32 };
                    (range, code, 1.0 / amax)
                }
            }
        };
        let (shift_x, shift_y, layout_id) = match f.layout {
            PixelLayout::Planar { chroma_shift, .. } => (chroma_shift.0, chroma_shift.1, 0u32),
            PixelLayout::SemiPlanar { chroma_shift } => (chroma_shift.0, chroma_shift.1, 1),
            PixelLayout::Rgba => (0, 0, 2),
        };
        let samples = if uint {
            ((f.width as f32 / n.width as f32).max(f.height as f32 / n.height as f32).ceil() as u32).clamp(1, 4)
        } else {
            Self::source_samples(f.width, f.height, n.width, n.height)
        };
        let params = SourceParams {
            ycbcr: [row4(it.ycbcr_to_rgb[0]), row4(it.ycbcr_to_rgb[1]), row4(it.ycbcr_to_rgb[2])],
            gamut: [row4(it.gamut[0]), row4(it.gamut[1]), row4(it.gamut[2])],
            range,
            size: [n.width as f32, n.height as f32, f.width as f32, f.height as f32],
            misc: [shift_x as f32, shift_y as f32, samples as f32, code_scale],
            mode: [layout_id, it.transfer, f.layout.has_alpha() as u32, f.shift as u32],
            extra: [alpha_scale, 0.0, 0.0, 0.0],
        };
        let ubuf = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("source params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let dummy = if uint { &self.dummy_uint } else { &self.dummy_float };
        let v = |i: usize| uploads.get(i).map(|t| &t.view).unwrap_or(dummy);
        let mut entries = vec![
            wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(v(0)) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(v(1)) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(v(2)) },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(v(3)) },
        ];
        if !uint {
            entries.push(wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.sampler) });
        }
        let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("source"),
            layout: if uint { &self.source_uint_bgl } else { &self.source_bgl },
            entries: &entries,
        });
        let out = self.pool.acquire(&self.ctx.device, n.width, n.height, WORKING_FORMAT);
        {
            let mut pass = begin_pass(enc, &out.view, Some(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(if uint { &self.source_uint } else { &self.source_sampled });
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        memo.uploads.extend(uploads);
        let tex = Arc::new(out);
        if self.frames.len() >= self.frame_cache_size.max(1)
            && let Some((i, _)) = self.frames.iter().enumerate().min_by_key(|(_, c)| c.last_used)
        {
            let old = self.frames.swap_remove(i);
            if let Ok(t) = Arc::try_unwrap(old.tex) {
                self.pool.release(t);
            }
        }
        self.frames.push(CachedFrame { serial: f.serial, size: (n.width, n.height), tex: tex.clone(), last_used: self.frame_index });
        tex
    }

    fn upload_image(&mut self, img: &CpuImage) -> Tex {
        let tex = self.pool.acquire(&self.ctx.device, img.width, img.height, WORKING_FORMAT);
        let half: Vec<half::f16> = img.data.iter().map(|v| half::f16::from_f32(*v)).collect();
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&half),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(img.width * 8), rows_per_image: None },
            wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
        );
        self.uploads += 1;
        tex
    }

    fn eval_composite(&mut self, enc: &mut wgpu::CommandEncoder, c: &CompositeNode, memo: &mut Memo) -> Arc<Tex> {
        let inputs: Vec<Arc<Tex>> = c.layers.iter().map(|l| self.eval(enc, &l.input, memo)).collect();
        let canvas = self.pool.acquire(&self.ctx.device, c.width, c.height, WORKING_FORMAT);
        let bgc = c.background;
        // clear
        drop(begin_pass(enc, &canvas.view, Some(wgpu::Color { r: bgc[0] as f64, g: bgc[1] as f64, b: bgc[2] as f64, a: bgc[3] as f64 })));
        let (cw, ch) = (c.width as f32, c.height as f32);
        let mut scratch: Option<Tex> = None;
        for (layer, tex) in c.layers.iter().zip(&inputs) {
            if layer.opacity <= 0.0 {
                continue;
            }
            let Some(inv) = affine_inverse(&layer.transform) else { continue };
            let (lw, lh) = (tex.width as f32, tex.height as f32);
            let cr = layer.crop;
            let (x0, y0, x1, y1) = (cr[0] * lw, cr[1] * lh, (1.0 - cr[2]) * lw, (1.0 - cr[3]) * lh);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let ndc = |x: f32, y: f32| {
                let (px, py) = affine_apply(&layer.transform, x, y);
                (px / cw * 2.0 - 1.0, 1.0 - py / ch * 2.0)
            };
            let (a, b, cc, d) = (ndc(x0, y0), ndc(x1, y0), ndc(x0, y1), ndc(x1, y1));
            let params = QuadParams {
                inv0: [inv[0][0], inv[0][1], inv[0][2], 0.0],
                inv1: [inv[1][0], inv[1][1], inv[1][2], 0.0],
                c01: [a.0, a.1, b.0, b.1],
                c23: [cc.0, cc.1, d.0, d.1],
                crop: [cr[0], cr[1], 1.0 - cr[2], 1.0 - cr[3]],
                size: [lw, lh, cw, ch],
                misc: [layer.opacity.min(1.0), layer.blend.index() as f32, 0.0, 0.0],
            };
            let hardware = matches!(layer.blend, BlendMode::Normal | BlendMode::Add);
            let dst_view = if hardware {
                &self.dummy_float
            } else {
                let s = scratch.get_or_insert_with(|| self.pool.acquire(&self.ctx.device, c.width, c.height, WORKING_FORMAT));
                enc.copy_texture_to_texture(
                    canvas.texture.as_image_copy(),
                    s.texture.as_image_copy(),
                    wgpu::Extent3d { width: c.width, height: c.height, depth_or_array_layers: 1 },
                );
                &s.view
            };
            let ubuf = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("quad params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("quad"),
                layout: &self.quad_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tex.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(dst_view) },
                ],
            });
            let mut pass = begin_pass(enc, &canvas.view, None);
            pass.set_pipeline(match layer.blend {
                BlendMode::Normal => &self.quad_over,
                BlendMode::Add => &self.quad_add,
                _ => &self.quad_blend,
            });
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..1);
        }
        if let Some(s) = scratch {
            self.pool.release(s);
        }
        memo.temps.extend(inputs);
        Arc::new(canvas)
    }

    fn eval(&mut self, enc: &mut wgpu::CommandEncoder, node: &NodeRef, memo: &mut Memo) -> Arc<Tex> {
        let key = Arc::as_ptr(node) as usize;
        if let Some(t) = memo.done.get(&key) {
            return t.clone();
        }
        let t = match node.as_ref() {
            Node::Frame(f) => self.eval_frame(enc, f, memo),
            Node::Image(img) => Arc::new(self.upload_image(img)),
            Node::Solid { width, height, color } => {
                let t = self.pool.acquire(&self.ctx.device, *width, *height, WORKING_FORMAT);
                drop(begin_pass(
                    enc,
                    &t.view,
                    Some(wgpu::Color { r: color[0] as f64, g: color[1] as f64, b: color[2] as f64, a: color[3] as f64 }),
                ));
                Arc::new(t)
            }
            Node::Composite(c) => self.eval_composite(enc, c, memo),
            Node::Lut { input, lut } => {
                let src = self.eval(enc, input, memo);
                Arc::new(self.run_lut(enc, &src, lut))
            }
            Node::Blur { input, sigma } => {
                let src = self.eval(enc, input, memo);
                self.run_blur(enc, src, *sigma, memo)
            }
            Node::Cpu { input, op } => {
                let src = self.eval(enc, input, memo);
                match self.read_tex(enc, &src) {
                    Ok(img) => Arc::new(self.upload_image(&op.apply(&img))),
                    Err(e) => {
                        log::warn!("{}: readback failed: {e}", op.name());
                        src
                    }
                }
            }
        };
        memo.done.insert(key, t.clone());
        t
    }

    /// Submits what is recorded so far and reads `t` back to system memory.
    fn read_tex(&mut self, enc: &mut wgpu::CommandEncoder, t: &Tex) -> Result<CpuImage, GpuError> {
        let fresh = self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("kompo cpu op") });
        let old = std::mem::replace(enc, fresh);
        self.ctx.queue.submit(Some(old.finish()));
        let data = readback(&self.ctx, &t.texture, t.width, t.height, 8)?;
        Ok(CpuImage {
            width: t.width,
            height: t.height,
            data: bytemuck::cast_slice::<u8, half::f16>(&data).iter().map(|v| v.to_f32()).collect(),
        })
    }

    fn aux_pipeline(
        &self,
        label: &str,
        src: &str,
        entries: &[wgpu::BindGroupLayoutEntry],
    ) -> (wgpu::BindGroupLayout, wgpu::RenderPipeline) {
        let device = &self.ctx.device;
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries });
        let module =
            device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(src.into()) });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pl = fullscreen_pipeline(device, &module, &layout, "fs_main", WORKING_FORMAT, None);
        (bgl, pl)
    }

    fn run_lut(&mut self, enc: &mut wgpu::CommandEncoder, src: &Tex, lut: &Arc<crate::ops::Lut3D>) -> Tex {
        if self.lut_pl.is_none() {
            let entry = |binding: u32, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty,
                count: None,
            };
            let tex = |dim| wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: dim,
                multisampled: false,
            };
            let entries = [
                entry(
                    0,
                    wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                ),
                entry(1, tex(wgpu::TextureViewDimension::D2)),
                entry(2, tex(wgpu::TextureViewDimension::D3)),
                entry(3, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ];
            self.lut_pl = Some(self.aux_pipeline("lut", include_str!("shaders/lut.wgsl"), &entries));
        }
        let view = match self.luts.iter().find(|(l, _)| Arc::ptr_eq(l, lut)) {
            Some((_, v)) => v.clone(),
            None => {
                let n = lut.size;
                let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("lut3d"),
                    size: wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D3,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let texels = lut.to_rgba_f16();
                self.ctx.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytemuck::cast_slice(&texels),
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(n * 8), rows_per_image: Some(n) },
                    wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
                );
                let view = tex.create_view(&Default::default());
                if self.luts.len() >= 24 {
                    self.luts.remove(0);
                }
                self.luts.push((lut.clone(), view.clone()));
                view
            }
        };
        let (bgl, pl) = self.lut_pl.as_ref().unwrap();
        let ubuf = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lut params"),
            contents: bytemuck::bytes_of(&[lut.size as f32, 0.0, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&src.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        let out = self.pool.acquire(&self.ctx.device, src.width, src.height, WORKING_FORMAT);
        {
            let mut pass = begin_pass(enc, &out.view, None);
            pass.set_pipeline(pl);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        out
    }

    fn run_blur(&mut self, enc: &mut wgpu::CommandEncoder, src: Arc<Tex>, sigma: [f32; 2], memo: &mut Memo) -> Arc<Tex> {
        if self.blur_pl.is_none() {
            let entries = [
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ];
            self.blur_pl = Some(self.aux_pipeline("blur", include_str!("shaders/blur.wgsl"), &entries));
        }
        let mut cur = src;
        for (axis, dir) in [(0usize, [1.0f32, 0.0]), (1, [0.0, 1.0])] {
            let s = sigma[axis];
            if s <= 0.0 {
                continue;
            }
            let radius = (s * 3.0).ceil().min(crate::ops::MAX_BLUR_RADIUS as f32);
            let (bgl, pl) = self.blur_pl.as_ref().unwrap();
            let ubuf = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur params"),
                contents: bytemuck::bytes_of(&[dir[0], dir[1], s, radius]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blur"),
                layout: bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&cur.view) },
                ],
            });
            let out = self.pool.acquire(&self.ctx.device, cur.width, cur.height, WORKING_FORMAT);
            {
                let mut pass = begin_pass(enc, &out.view, None);
                pass.set_pipeline(pl);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..3, 0..1);
            }
            memo.temps.push(cur);
            cur = Arc::new(out);
        }
        cur
    }

    /// Returns textures used during a frame to the pool (except cached frames).
    fn finish_frame(&mut self, memo: Memo) {
        for t in memo.uploads {
            self.pool.release(t);
        }
        for t in memo.done.into_values().chain(memo.temps) {
            if let Ok(t) = Arc::try_unwrap(t) {
                self.pool.release(t);
            }
        }
        self.frame_index += 1;
        self.pool.frame = self.frame_index;
        if self.frame_index.is_multiple_of(60) {
            self.pool.trim(240);
        }
    }

    fn encode_output(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        src: &Tex,
        spec: &OutputSpec,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        let t = &spec.transform;
        let (ycc, range, ycc_mode) = match spec.ycbcr {
            Some((matrix, full, bits)) => {
                let m = kompo_color::ycbcr::rgb_to_ycbcr_matrix(matrix);
                let max = ((1u64 << bits) - 1) as f32;
                let s = (1u64 << (bits - 8)) as f32;
                let r = if full {
                    [1.0, 0.0, 1.0, (1u64 << (bits - 1)) as f32 / max]
                } else {
                    [219.0 * s / max, 16.0 * s / max, 224.0 * s / max, 128.0 * s / max]
                };
                (m, r, 1u32)
            }
            None => ([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], [1.0, 0.0, 1.0, 0.0], 0u32),
        };
        let params = OutputParams {
            gamut: [row4(t.gamut[0]), row4(t.gamut[1]), row4(t.gamut[2])],
            ycc: [row4(ycc[0]), row4(ycc[1]), row4(ycc[2])],
            range,
            background: spec.background.unwrap_or([0.0; 4]),
            mode: [ycc_mode, t.transfer, spec.background.is_some() as u32, spec.dither as u32],
        };
        let ubuf = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("output params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("output"),
            layout: &self.output_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&src.view) },
            ],
        });
        let pipeline = self.output_pipeline(format);
        let mut pass = begin_pass(enc, target, Some(wgpu::Color::BLACK));
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Renders `node` and writes the encoded result into `target` (same size as the node).
    pub fn render_to_view(&mut self, node: &NodeRef, spec: &OutputSpec, target: &wgpu::TextureView, format: wgpu::TextureFormat) {
        let mut enc = self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("kompo frame") });
        let mut memo = Memo::default();
        let src = self.eval(&mut enc, node, &mut memo);
        self.encode_output(&mut enc, &src, spec, target, format);
        self.ctx.queue.submit(Some(enc.finish()));
        self.finish_frame(memo);
    }

    /// Renders into a pooled display texture and waits for the GPU (no readback).
    pub fn render_display_sync(&mut self, node: &NodeRef, spec: &OutputSpec) {
        let (w, h) = node.size();
        let target = self.pool.acquire(&self.ctx.device, w, h, wgpu::TextureFormat::Rgba8Unorm);
        self.render_to_view(node, spec, &target.view, wgpu::TextureFormat::Rgba8Unorm);
        self.ctx.wait_idle();
        self.pool.release(target);
    }

    /// Renders and reads back the encoded image as RGBA8.
    pub fn render_rgba8(&mut self, node: &NodeRef, spec: &OutputSpec) -> Result<Vec<u8>, GpuError> {
        let (w, h) = node.size();
        let target = self.pool.acquire(&self.ctx.device, w, h, wgpu::TextureFormat::Rgba8Unorm);
        self.render_to_view(node, spec, &target.view, wgpu::TextureFormat::Rgba8Unorm);
        let data = readback(&self.ctx, &target.texture, w, h, 4)?;
        self.pool.release(target);
        Ok(data)
    }

    /// Renders and reads back the encoded image as RGBA f16 (10/12-bit delivery).
    pub fn render_rgba16f(&mut self, node: &NodeRef, spec: &OutputSpec) -> Result<Vec<half::f16>, GpuError> {
        let (w, h) = node.size();
        let target = self.pool.acquire(&self.ctx.device, w, h, wgpu::TextureFormat::Rgba16Float);
        self.render_to_view(node, spec, &target.view, wgpu::TextureFormat::Rgba16Float);
        let data = readback(&self.ctx, &target.texture, w, h, 8)?;
        self.pool.release(target);
        Ok(bytemuck::cast_slice(&data).to_vec())
    }

    /// Renders, applies the output transform and packs the result into the
    /// planes of an encoder pixel format — all on the GPU (export path).
    pub fn render_packed(
        &mut self,
        node: &NodeRef,
        spec: &OutputSpec,
        format: crate::pack::PackFormat,
    ) -> Result<Vec<crate::pack::PackedPlane>, GpuError> {
        let (w, h) = node.size();
        let encoded = self.pool.acquire(&self.ctx.device, w, h, wgpu::TextureFormat::Rgba32Float);
        let mut enc = self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("kompo export") });
        let mut memo = Memo::default();
        let src = self.eval(&mut enc, node, &mut memo);
        self.encode_output(&mut enc, &src, spec, &encoded.view, wgpu::TextureFormat::Rgba32Float);
        let planes = format.planes(w, h);
        let total_words: u64 = planes.iter().map(|p| p.stride_words() as u64 * p.height as u64).sum();
        let size = total_words * 4;
        let storage = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("packed planes"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let pipeline = self.pack_pipeline();
        let layout = pipeline.get_bind_group_layout(0);
        let mut offset = 0u32;
        let mut bind_groups = Vec::new();
        for ps in &planes {
            let params = PackParams {
                src_size: [w, h],
                plane_size: [ps.width, ps.height],
                kind: ps.kind,
                bytes_per_sample: ps.bytes_per_sample,
                shift_x: ps.shift.0 as u32,
                shift_y: ps.shift.1 as u32,
                max_code: ((1u64 << format.bits()) - 1) as f32,
                msb_shift: format.msb_shift(),
                stride_words: ps.stride_words(),
                offset_words: offset,
            };
            offset += ps.stride_words() * ps.height;
            let ubuf = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pack params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pack"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&encoded.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: storage.as_entire_binding() },
                ],
            });
            bind_groups.push((bg, ps.stride_words().div_ceil(8), ps.height.div_ceil(8)));
        }
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("pack"), timestamp_writes: None });
            pass.set_pipeline(&pipeline);
            for (bg, gx, gy) in &bind_groups {
                pass.set_bind_group(0, bg, &[]);
                pass.dispatch_workgroups(*gx, *gy, 1);
            }
        }
        let readback = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("packed readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_buffer_to_buffer(&storage, 0, &readback, 0, size);
        self.ctx.queue.submit(Some(enc.finish()));
        self.finish_frame(memo);
        self.pool.release(encoded);
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.ctx
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        rx.recv().map_err(|e| GpuError::Readback(e.to_string()))?.map_err(|e| GpuError::Readback(e.to_string()))?;
        let mapped = slice.get_mapped_range().map_err(|e| GpuError::Readback(format!("{e:?}")))?;
        let mut out = Vec::with_capacity(planes.len());
        let mut word = 0usize;
        for ps in &planes {
            let row = ps.row_bytes();
            let stride_bytes = ps.stride_words() as usize * 4;
            let mut data = Vec::with_capacity(row * ps.height as usize);
            for y in 0..ps.height as usize {
                let s = word * 4 + y * stride_bytes;
                data.extend_from_slice(&mapped[s..s + row]);
            }
            word += ps.stride_words() as usize * ps.height as usize;
            out.push(crate::pack::PackedPlane { data, stride: row, width: ps.width, height: ps.height });
        }
        drop(mapped);
        readback.unmap();
        Ok(out)
    }

    fn pack_pipeline(&mut self) -> wgpu::ComputePipeline {
        if let Some(p) = &self.pack {
            return p.clone();
        }
        let module = self.ctx.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pack"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/pack.wgsl").into()),
        });
        let p = self.ctx.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("pack"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        self.pack = Some(p.clone());
        p
    }

    /// Renders the working-space image (no output transform) for tests and analysis.
    pub fn render_working(&mut self, node: &NodeRef) -> Result<CpuImage, GpuError> {
        let mut enc = self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("kompo working") });
        let mut memo = Memo::default();
        let src = self.eval(&mut enc, node, &mut memo);
        self.ctx.queue.submit(Some(enc.finish()));
        let data = readback(&self.ctx, &src.texture, src.width, src.height, 8)?;
        let img = CpuImage {
            width: src.width,
            height: src.height,
            data: bytemuck::cast_slice::<u8, half::f16>(&data).iter().map(|v| v.to_f32()).collect(),
        };
        self.finish_frame(memo);
        Ok(img)
    }
}

#[derive(Default)]
struct Memo {
    done: HashMap<usize, Arc<Tex>>,
    temps: Vec<Arc<Tex>>,
    uploads: Vec<Tex>,
}

fn begin_pass<'a>(enc: &'a mut wgpu::CommandEncoder, view: &'a wgpu::TextureView, clear: Option<wgpu::Color>) -> wgpu::RenderPass<'a> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: match clear {
                    Some(c) => wgpu::LoadOp::Clear(c),
                    None => wgpu::LoadOp::Load,
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

pub(crate) fn fullscreen_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    fs: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fs),
        layout: Some(layout),
        vertex: wgpu::VertexState { module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Copies a texture to CPU memory (tightly packed rows).
pub fn readback(ctx: &GpuContext, tex: &wgpu::Texture, w: u32, h: u32, bpp: u32) -> Result<Vec<u8>, GpuError> {
    let unpadded = w * bpp;
    let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    ctx.queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    ctx.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).map_err(|e| GpuError::Readback(e.to_string()))?;
    rx.recv().map_err(|e| GpuError::Readback(e.to_string()))?.map_err(|e| GpuError::Readback(e.to_string()))?;
    let mapped = slice.get_mapped_range().map_err(|e| GpuError::Readback(format!("{e:?}")))?;
    let mut out = Vec::with_capacity((unpadded * h) as usize);
    for row in 0..h {
        let s = (row * padded) as usize;
        out.extend_from_slice(&mapped[s..s + unpadded as usize]);
    }
    drop(mapped);
    buf.unmap();
    Ok(out)
}
