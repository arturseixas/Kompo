//! GPU device management.

use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(String),
    #[error("device request failed: {0}")]
    Device(String),
    #[error("GPU readback failed: {0}")]
    Readback(String),
    #[error("{0}")]
    Other(String),
}

/// Shared wgpu device/queue plus adapter information.
#[derive(Clone)]
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_info: wgpu::AdapterInfo,
    pub limits: wgpu::Limits,
    pub features: wgpu::Features,
    inner: Arc<()>,
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext").field("adapter", &self.adapter_info.name).field("backend", &self.adapter_info.backend).finish()
    }
}

/// Features Kompo uses when available (never required).
pub fn optional_features() -> wgpu::Features {
    wgpu::Features::FLOAT32_FILTERABLE | wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
}

/// Device descriptor shared by the headless context and the UI (eframe).
pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    let mut limits = wgpu::Limits::default().using_resolution(adapter.limits());
    // 8K+ textures where the adapter allows (export of 8K sequences), never above what it supports.
    limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d.min(16384);
    wgpu::DeviceDescriptor {
        label: Some("kompo device"),
        required_features: adapter.features() & optional_features(),
        required_limits: limits,
        ..Default::default()
    }
}

impl GpuContext {
    /// Wraps an existing device (e.g. the one eframe created for the window).
    pub fn from_existing(adapter: &wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self { limits: device.limits(), features: device.features(), adapter_info: adapter.get_info(), device, queue, inner: Arc::new(()) }
    }

    /// Creates a headless context (export, benchmarks, tests). Honors
    /// `WGPU_BACKEND` / `WGPU_ADAPTER_NAME` environment variables.
    pub fn new_headless() -> Result<Self, GpuError> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        if wgpu::Backends::from_env().is_none() {
            desc.backends = wgpu::Backends::PRIMARY;
        }
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance),
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }))
        .map_err(|e| GpuError::NoAdapter(e.to_string()))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&device_descriptor(&adapter))).map_err(|e| GpuError::Device(e.to_string()))?;
        Ok(Self::from_existing(&adapter, device, queue))
    }

    pub fn describe(&self) -> String {
        format!("{} ({:?}, {:?})", self.adapter_info.name, self.adapter_info.backend, self.adapter_info.device_type)
    }

    /// `true` for software rasterizers (llvmpipe/lavapipe, WARP, SwiftShader).
    pub fn is_software(&self) -> bool {
        self.adapter_info.device_type == wgpu::DeviceType::Cpu
    }

    pub fn wait_idle(&self) {
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    }

    pub fn ptr_eq(&self, o: &GpuContext) -> bool {
        Arc::ptr_eq(&self.inner, &o.inner)
    }
}
