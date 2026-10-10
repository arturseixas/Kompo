//! # kompo-gpu
//!
//! Kompo's render graph. Frames are described as a DAG of nodes
//! ([`graph::Node`]) and executed either on the GPU with wgpu (Vulkan, Metal,
//! DX12, GL) and WGSL shaders, or on the CPU with rayon when no adequate GPU
//! is available. All processing happens in linear floating point (f16 on the
//! GPU, f32 on the CPU) with premultiplied alpha.

pub mod context;
pub mod cpu;
pub mod gpu;
pub mod graph;
pub mod image;
pub mod pack;

pub use context::{GpuContext, GpuError};
pub use cpu::CpuRenderer;
pub use gpu::GpuRenderer;
pub use graph::{Affine, CompositeNode, Layer, Node, NodeRef, OutputSpec};
pub use image::CpuImage;

/// Either backend, chosen at startup.
pub enum Renderer {
    Gpu(Box<GpuRenderer>),
    Cpu(CpuRenderer),
}

impl Renderer {
    /// GPU if available (and not disabled with `KOMPO_CPU_RENDER=1`), else CPU.
    pub fn auto(ctx: Option<GpuContext>) -> Self {
        if std::env::var_os("KOMPO_CPU_RENDER").is_some() {
            return Renderer::Cpu(CpuRenderer::new());
        }
        match ctx {
            Some(c) => Renderer::Gpu(Box::new(GpuRenderer::new(c))),
            None => Renderer::Cpu(CpuRenderer::new()),
        }
    }

    pub fn name(&self) -> String {
        match self {
            Renderer::Gpu(g) => format!("GPU: {}", g.ctx.describe()),
            Renderer::Cpu(_) => format!("CPU ({} threads)", rayon::current_num_threads()),
        }
    }

    pub fn is_gpu(&self) -> bool {
        matches!(self, Renderer::Gpu(_))
    }

    /// Renders and encodes to 8-bit RGBA.
    pub fn render_rgba8(&mut self, node: &NodeRef, spec: &OutputSpec) -> Result<Vec<u8>, GpuError> {
        match self {
            Renderer::Gpu(g) => g.render_rgba8(node, spec),
            Renderer::Cpu(c) => Ok(c.render_rgba8(node, spec)),
        }
    }

    /// Renders for display like the viewer does (GPU: into a texture, waiting
    /// for completion; no readback). Used by benchmarks.
    pub fn render_display(&mut self, node: &NodeRef, spec: &OutputSpec) -> Result<(), GpuError> {
        match self {
            Renderer::Gpu(g) => {
                g.render_display_sync(node, spec);
                Ok(())
            }
            Renderer::Cpu(c) => {
                let _ = c.render_rgba8(node, spec);
                Ok(())
            }
        }
    }

    /// Renders and packs into encoder planes (export).
    pub fn render_packed(
        &mut self,
        node: &NodeRef,
        spec: &OutputSpec,
        format: pack::PackFormat,
    ) -> Result<Vec<pack::PackedPlane>, GpuError> {
        match self {
            Renderer::Gpu(g) => g.render_packed(node, spec, format),
            Renderer::Cpu(c) => {
                let img = c.render_working(node);
                let enc = c.encode(&img, spec);
                Ok(pack::pack_cpu(&enc, img.width, img.height, format))
            }
        }
    }

    pub fn render_working(&mut self, node: &NodeRef) -> Result<CpuImage, GpuError> {
        match self {
            Renderer::Gpu(g) => g.render_working(node),
            Renderer::Cpu(c) => Ok((*c.render_working(node)).clone()),
        }
    }
}
