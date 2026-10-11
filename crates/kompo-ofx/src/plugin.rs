//! Loading `.ofx` bundles, describing plug-ins and running instances.

use crate::consts::*;
use crate::host::{Buf, Depth, Effect, Handle, OfxHost, ParamType, PropSet, Status, host};
use kompo_core::animation::AnimatedParam;
use libloading::Library;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[derive(Debug, thiserror::Error)]
pub enum OfxError {
    #[error("plug-in '{0}' not found")]
    NotFound(String),
    #[error("{0}")]
    Load(String),
    #[error("plug-in action {action} failed with status {status}")]
    Action { action: String, status: i32 },
    #[error("plug-in does not support a context Kompo offers")]
    NoContext,
}

type MainEntry = unsafe extern "C" fn(*const c_char, *const c_void, Handle, Handle) -> Status;

#[repr(C)]
struct RawPlugin {
    plugin_api: *const c_char,
    api_version: c_int,
    plugin_identifier: *const c_char,
    version_major: c_uint,
    version_minor: c_uint,
    set_host: unsafe extern "C" fn(*const OfxHost),
    main_entry: MainEntry,
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: String,
    pub label: String,
    pub ty: ParamType,
    pub default: Vec<f64>,
    pub default_text: String,
    pub min: Vec<f64>,
    pub max: Vec<f64>,
    pub display_min: Vec<f64>,
    pub display_max: Vec<f64>,
    pub choices: Vec<String>,
    pub hint: String,
    pub parent: Option<String>,
    pub secret: bool,
    /// `OfxParamDoubleTypeAngle`, `...Scale`, etc.
    pub double_type: String,
}

/// What the UI and the pipeline need to know about a plug-in.
#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub id: String,
    pub label: String,
    pub group: String,
    pub version: (u32, u32),
    pub path: PathBuf,
    pub context: Option<String>,
    pub supported_contexts: Vec<String>,
    pub pixel_depths: Vec<String>,
    pub params: Vec<ParamInfo>,
    /// `None` when usable; otherwise why Kompo cannot run it.
    pub unsupported: Option<String>,
    /// The plug-in advertises OpenGL rendering (Kompo renders it on the CPU).
    pub opengl_capable: bool,
}

struct Loaded {
    info: PluginInfo,
    entry: MainEntry,
    descriptor: *mut Effect,
    _lib: Arc<Library>,
}

unsafe impl Send for Loaded {}
unsafe impl Sync for Loaded {}

fn registry() -> &'static Mutex<Vec<Arc<Loaded>>> {
    static R: OnceLock<Mutex<Vec<Arc<Loaded>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn live() -> &'static Mutex<Vec<usize>> {
    static L: OnceLock<Mutex<Vec<usize>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Effects with images possibly outstanding (used to release image handles).
pub(crate) fn live_effects() -> Vec<*mut Effect> {
    live().lock().iter().map(|p| *p as *mut Effect).collect()
}

unsafe fn call(entry: MainEntry, action: &str, handle: Handle, inargs: *mut PropSet, outargs: *mut PropSet) -> Status {
    let a = CString::new(action).unwrap_or_default();
    unsafe { entry(a.as_ptr(), handle as *const c_void, inargs as Handle, outargs as Handle) }
}

fn ok(st: Status) -> bool {
    st == kOfxStatOK || st == kOfxStatReplyDefault
}

// ---------------------------------------------------------------------------
// discovery
// ---------------------------------------------------------------------------

/// Standard OFX plug-in folders for this platform plus `OFX_PLUGIN_PATH` and `KOMPO_OFX_PATH`.
pub fn default_search_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        v.push("/usr/OFX/Plugins".into());
        v.push("/usr/local/OFX/Plugins".into());
        if let Some(h) = std::env::var_os("HOME") {
            v.push(PathBuf::from(h).join(".OFX/Plugins"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        v.push("/Library/OFX/Plugins".into());
        if let Some(h) = std::env::var_os("HOME") {
            v.push(PathBuf::from(h).join("Library/OFX/Plugins"));
        }
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("CommonProgramFiles").map(PathBuf::from).unwrap_or_else(|| "C:\\Program Files\\Common Files".into());
        v.push(base.join("OFX\\Plugins"));
    }
    for var in ["OFX_PLUGIN_PATH", "KOMPO_OFX_PATH"] {
        if let Some(p) = std::env::var_os(var) {
            v.extend(std::env::split_paths(&p));
        }
    }
    v
}

fn arch_dirs() -> &'static [&'static str] {
    if cfg!(target_os = "linux") {
        &["Linux-x86-64", "Linux-aarch64", "Linux-arm-64", "Linux"]
    } else if cfg!(target_os = "macos") {
        &["MacOS", "MacOS-x86-64", "MacOS-arm-64"]
    } else {
        &["Win64", "Win32"]
    }
}

/// Shared libraries of a search folder: `X.ofx.bundle/Contents/<arch>/X.ofx` and loose `.ofx` files.
fn find_binaries(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if p.is_dir() && name.ends_with(".ofx.bundle") {
            let stem = name.trim_end_matches(".bundle").to_string();
            for arch in arch_dirs() {
                let bin = p.join("Contents").join(arch).join(&stem);
                if bin.is_file() {
                    out.push(bin);
                    break;
                }
            }
        } else if p.is_dir() {
            out.extend(find_binaries(&p));
        } else if name.ends_with(".ofx") {
            out.push(p);
        }
    }
    out
}

fn summarize_param(p: &crate::host::Param) -> ParamInfo {
    let s = unsafe { &*p.props };
    let nums = |k: &str, n: usize| (0..n).filter_map(|i| s.get_f(k, i)).collect::<Vec<f64>>();
    let d = p.ty.dims().max(1);
    ParamInfo {
        name: p.name.clone(),
        label: s.get_str(kOfxPropLabel, 0).unwrap_or_else(|| p.name.clone()),
        ty: p.ty,
        default: nums(kOfxParamPropDefault, d),
        default_text: s.get_str(kOfxParamPropDefault, 0).unwrap_or_default(),
        min: nums(kOfxParamPropMin, d),
        max: nums(kOfxParamPropMax, d),
        display_min: nums(kOfxParamPropDisplayMin, d),
        display_max: nums(kOfxParamPropDisplayMax, d),
        choices: s.strings(kOfxParamPropChoiceOption),
        hint: s.get_str(kOfxParamPropHint, 0).unwrap_or_default(),
        parent: s.get_str(kOfxParamPropParent, 0).filter(|p| !p.is_empty()),
        secret: s.get_i(kOfxParamPropSecret, 0) == Some(1),
        double_type: s.get_str(kOfxParamPropDoubleType, 0).unwrap_or_default(),
    }
}

/// Loads every plug-in of one binary and runs Describe / DescribeInContext.
fn load_binary(path: &Path) -> Vec<Arc<Loaded>> {
    let mut out = Vec::new();
    let lib = match unsafe { Library::new(path) } {
        Ok(l) => Arc::new(l),
        Err(e) => {
            log::warn!("OFX: cannot load {}: {e}", path.display());
            return out;
        }
    };
    let (count, get): (unsafe extern "C" fn() -> c_int, unsafe extern "C" fn(c_int) -> *const RawPlugin) = unsafe {
        match (
            lib.get::<unsafe extern "C" fn() -> c_int>(b"OfxGetNumberOfPlugins\0"),
            lib.get::<unsafe extern "C" fn(c_int) -> *const RawPlugin>(b"OfxGetPlugin\0"),
        ) {
            (Ok(c), Ok(g)) => (*c, *g),
            _ => {
                log::warn!("OFX: {} does not export the OFX entry points", path.display());
                return out;
            }
        }
    };
    for i in 0..unsafe { count() } {
        let raw = unsafe { get(i) };
        let Some(raw) = (unsafe { raw.as_ref() }) else { continue };
        let api = unsafe { std::ffi::CStr::from_ptr(raw.plugin_api) }.to_string_lossy().into_owned();
        let id = unsafe { std::ffi::CStr::from_ptr(raw.plugin_identifier) }.to_string_lossy().into_owned();
        let mut info = PluginInfo {
            id: id.clone(),
            label: id.clone(),
            group: String::new(),
            version: (raw.version_major, raw.version_minor),
            path: path.to_path_buf(),
            context: None,
            supported_contexts: Vec::new(),
            pixel_depths: Vec::new(),
            params: Vec::new(),
            unsupported: None,
            opengl_capable: false,
        };
        if api != kOfxImageEffectPluginApi || raw.api_version != 1 {
            info.unsupported = Some(format!("API {api} v{} (only image effects v1 are supported)", raw.api_version));
            out.push(Arc::new(Loaded { info, entry: raw.main_entry, descriptor: std::ptr::null_mut(), _lib: lib.clone() }));
            continue;
        }
        unsafe { (raw.set_host)(host()) };
        let entry = raw.main_entry;
        let descriptor = Effect::new(&id, false);
        let eff = unsafe { &*descriptor };
        let described = unsafe {
            let st = call(entry, kOfxActionLoad, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            if st == kOfxStatErrMissingHostFeature {
                Err("needs a host feature Kompo does not provide (e.g. temporal clip access)".to_string())
            } else if !ok(st) {
                Err(format!("Load failed with status {st}"))
            } else {
                let st = call(entry, kOfxActionDescribe, descriptor as Handle, std::ptr::null_mut(), std::ptr::null_mut());
                if ok(st) { Ok(()) } else { Err(format!("Describe failed with status {st}")) }
            }
        };
        if let Err(e) = described {
            info.unsupported = Some(e);
            out.push(Arc::new(Loaded { info, entry, descriptor, _lib: lib.clone() }));
            continue;
        }
        let p = eff.props();
        info.label = p.get_str(kOfxPropLabel, 0).unwrap_or_else(|| id.clone());
        info.group = p.get_str(kOfxImageEffectPluginPropGrouping, 0).unwrap_or_default();
        info.supported_contexts = p.strings(kOfxImageEffectPropSupportedContexts);
        info.pixel_depths = p.strings(kOfxImageEffectPropSupportedPixelDepths);
        info.opengl_capable = p.get_str(kOfxImageEffectPropOpenGLRenderSupported, 0).is_some_and(|v| v == "true" || v == "needed");
        let ctx = [kOfxImageEffectContextFilter, kOfxImageEffectContextGenerator, kOfxImageEffectContextGeneral]
            .into_iter()
            .find(|c| info.supported_contexts.iter().any(|s| s == c));
        let Some(ctx) = ctx else {
            info.unsupported = Some(format!(
                "contexts [{}] are not supported (Kompo offers filter, generator and general)",
                info.supported_contexts.join(", ")
            ));
            out.push(Arc::new(Loaded { info, entry, descriptor, _lib: lib.clone() }));
            continue;
        };
        if !info.pixel_depths.iter().any(|d| d == kOfxBitDepthFloat || d == kOfxBitDepthShort || d == kOfxBitDepthByte) {
            info.unsupported = Some("no supported pixel depth (Kompo offers float, 16-bit and 8-bit)".into());
            out.push(Arc::new(Loaded { info, entry, descriptor, _lib: lib.clone() }));
            continue;
        }
        info.context = Some(ctx.to_string());
        let args = PropSet::new_ptr();
        unsafe { (*args).set_str(kOfxImageEffectPropContext, 0, ctx) };
        let st = unsafe { call(entry, kOfxImageEffectActionDescribeInContext, descriptor as Handle, args, std::ptr::null_mut()) };
        if !ok(st) {
            info.unsupported = Some(format!("DescribeInContext failed with status {st}"));
        } else {
            info.params = eff.params.lock().iter().map(|p| summarize_param(unsafe { &**p })).collect();
        }
        out.push(Arc::new(Loaded { info, entry, descriptor, _lib: lib.clone() }));
    }
    out
}

/// Scans `dirs` (default folders when empty), loading each plug-in once.
pub fn scan(dirs: &[PathBuf]) -> Vec<PluginInfo> {
    let dirs: Vec<PathBuf> = if dirs.is_empty() { default_search_paths() } else { dirs.to_vec() };
    let mut found = Vec::new();
    for d in dirs {
        for bin in find_binaries(&d) {
            if registry().lock().iter().any(|l| l.info.path == bin) {
                continue;
            }
            let loaded = load_binary(&bin);
            registry().lock().extend(loaded);
        }
    }
    for l in registry().lock().iter() {
        found.push(l.info.clone());
    }
    found
}

/// Plug-ins loaded so far.
pub fn plugins() -> Vec<PluginInfo> {
    registry().lock().iter().map(|l| l.info.clone()).collect()
}

pub fn find(id: &str) -> Option<PluginInfo> {
    registry().lock().iter().find(|l| l.info.id == id).map(|l| l.info.clone())
}

// ---------------------------------------------------------------------------
// instances
// ---------------------------------------------------------------------------

/// A running instance of a plug-in.
pub struct OfxInstance {
    plugin: Arc<Loaded>,
    effect: *mut Effect,
    depth: Depth,
    applied: HashMap<String, Vec<f64>>,
    texts: HashMap<String, String>,
    created: bool,
    /// Parameters the host set explicitly (their defaults are no longer tracked).
    user_set: std::collections::HashSet<String>,
    scaled_for: Option<(u32, u32)>,
}

unsafe impl Send for OfxInstance {}

impl OfxInstance {
    pub fn new(id: &str) -> Result<Self, OfxError> {
        let plugin = registry().lock().iter().find(|l| l.info.id == id).cloned().ok_or_else(|| OfxError::NotFound(id.into()))?;
        if let Some(why) = &plugin.info.unsupported {
            return Err(OfxError::Load(why.clone()));
        }
        let depths = &plugin.info.pixel_depths;
        let depth = if depths.iter().any(|d| d == kOfxBitDepthFloat) {
            Depth::Float
        } else if depths.iter().any(|d| d == kOfxBitDepthShort) {
            Depth::Short
        } else {
            Depth::Byte
        };
        let effect = unsafe { (*plugin.descriptor).instantiate() };
        live().lock().push(effect as usize);
        let e = unsafe { &*effect };
        let p = e.props();
        let ctx = plugin.info.context.clone().unwrap_or_default();
        p.set_str(kOfxImageEffectPropContext, 0, &ctx);
        p.set_int(kOfxPropIsInteractive, 0, 0);
        p.set_str(kOfxImageEffectPropPixelDepth, 0, depth.name());
        p.set_str(kOfxImageEffectPropComponents, 0, kOfxImageComponentRGBA);
        p.set_str(kOfxImageEffectPropPreMultiplication, 0, kOfxImagePreMultiplied);
        p.set_dbl(kOfxImageEffectPropRenderScale, 0, 1.0);
        p.set_dbl(kOfxImageEffectPropRenderScale, 1, 1.0);
        let st = unsafe { call(plugin.entry, kOfxActionCreateInstance, effect as Handle, std::ptr::null_mut(), std::ptr::null_mut()) };
        if !ok(st) {
            live().lock().retain(|x| *x != effect as usize);
            return Err(OfxError::Action { action: kOfxActionCreateInstance.into(), status: st });
        }
        Ok(Self {
            plugin,
            effect,
            depth,
            applied: HashMap::new(),
            texts: HashMap::new(),
            created: true,
            user_set: Default::default(),
            scaled_for: None,
        })
    }

    pub fn info(&self) -> &PluginInfo {
        &self.plugin.info
    }

    fn eff(&self) -> &Effect {
        unsafe { &*self.effect }
    }

    /// Sets a numeric parameter (all components) or string value.
    pub fn set_value(&mut self, name: &str, v: &[f64]) {
        self.user_set.insert(name.to_string());
        if let Some(p) = self.eff().param(name) {
            let mut cur = p.value.lock();
            for (c, x) in cur.iter_mut().zip(v) {
                *c = *x;
            }
        }
    }

    pub fn set_text(&mut self, name: &str, v: &str) {
        if let Some(p) = self.eff().param(name) {
            *p.text.lock() = v.to_string();
        }
    }

    /// Gives a parameter keyframes the plug-in can read at any time.
    pub fn set_animation(&mut self, name: &str, anim: Option<AnimatedParam>, fps: f64) {
        self.user_set.insert(name.to_string());
        if let Some(p) = self.eff().param(name) {
            *p.anim.lock() = anim;
            *p.fps.lock() = fps;
        }
    }

    pub fn param_value(&self, name: &str) -> Option<Vec<f64>> {
        self.eff().param(name).map(|p| p.value.lock().clone())
    }

    /// Spatial parameters whose default is given in normalized coordinates are converted to the
    /// project's canonical (pixel) coordinates, until the host sets them explicitly.
    fn scale_normalised_defaults(&mut self, w: u32, h: u32) {
        if self.scaled_for == Some((w, h)) {
            return;
        }
        self.scaled_for = Some((w, h));
        let e = self.eff();
        for p in e.params.lock().iter().map(|p| unsafe { &**p }) {
            if self.user_set.contains(&p.name) || !matches!(p.ty, ParamType::Double2D | ParamType::Double3D | ParamType::Double) {
                continue;
            }
            let props = unsafe { &*p.props };
            if props.get_str(kOfxParamPropDefaultCoordinateSystem, 0).as_deref() != Some(kOfxParamCoordinatesNormalised) {
                continue;
            }
            let mut v = p.value.lock();
            for (i, x) in v.iter_mut().enumerate() {
                let d = props.get_f(kOfxParamPropDefault, i).unwrap_or(0.0);
                *x = d * if i == 0 { w as f64 } else { h as f64 };
            }
        }
    }

    fn fire_changes(&mut self, time: f64) {
        let names: Vec<(String, Vec<f64>, String)> = self
            .eff()
            .params
            .lock()
            .iter()
            .map(|p| unsafe { &**p })
            .map(|p| (p.name.clone(), p.value.lock().clone(), p.text.lock().clone()))
            .collect();
        for (name, v, t) in names {
            let changed = self.applied.get(&name) != Some(&v) || self.texts.get(&name) != Some(&t);
            if changed && self.applied.contains_key(&name) {
                let args = PropSet::new_ptr();
                let a = unsafe { &*args };
                a.set_str(kOfxPropType, 0, kOfxTypeParameter);
                a.set_str(kOfxPropName, 0, &name);
                a.set_str(kOfxPropChangeReason, 0, kOfxChangeUserEdited);
                a.set_dbl(kOfxPropTime, 0, time);
                a.set_dbl(kOfxImageEffectPropRenderScale, 0, 1.0);
                a.set_dbl(kOfxImageEffectPropRenderScale, 1, 1.0);
                unsafe { call(self.plugin.entry, kOfxActionInstanceChanged, self.effect as Handle, args, std::ptr::null_mut()) };
                drop(unsafe { Box::from_raw(args) });
            }
            self.applied.insert(name.clone(), v);
            self.texts.insert(name, t);
        }
    }

    /// Renders one frame. `src` is premultiplied RGBA f32 (rows top to bottom) or `None` for generators.
    pub fn render(&mut self, w: u32, h: u32, src: Option<&[f32]>, time_frames: f64, fps: f64) -> Result<Vec<f32>, OfxError> {
        let (wu, hu) = (w as usize, h as usize);
        let depth = self.depth;
        {
            let e = self.eff();
            *e.time.lock() = (time_frames, 0.0, 1.0e6);
            for p in e.params.lock().iter() {
                *unsafe { &**p }.fps.lock() = fps;
            }
            let mut bufs = e.buffers.lock();
            bufs.src = match src {
                Some(s) => pack(s, wu, hu, depth),
                None => Buf::default(),
            };
            let row = wu * 4 * depth.bytes();
            bufs.out = Buf { data: vec![0u8; row * hu], w, h, depth: Some(depth) };
            let p = e.props();
            p.set_int(kOfxImageEffectPropProjectSize, 0, w as i32);
            p.set_int(kOfxImageEffectPropProjectSize, 1, h as i32);
            p.set_dbl(kOfxImageEffectPropProjectSize, 0, w as f64);
            p.set_dbl(kOfxImageEffectPropProjectSize, 1, h as f64);
            p.set_dbl(kOfxImageEffectPropProjectOffset, 0, 0.0);
            p.set_dbl(kOfxImageEffectPropProjectOffset, 1, 0.0);
            p.set_dbl(kOfxImageEffectPropProjectExtent, 0, w as f64);
            p.set_dbl(kOfxImageEffectPropProjectExtent, 1, h as f64);
            p.set_dbl(kOfxImageEffectPropProjectPixelAspectRatio, 0, 1.0);
            p.set_dbl(kOfxImageEffectInstancePropEffectDuration, 0, 1.0e6);
            p.set_dbl(kOfxImageEffectPropFrameRate, 0, fps);
            for c in e.clips.lock().iter() {
                let c = unsafe { &**c };
                let cp = unsafe { &*c.props };
                let connected = c.name == kOfxImageEffectOutputClipName || src.is_some() && c.name == kOfxImageEffectSimpleSourceClipName;
                cp.set_int(kOfxImageClipPropConnected, 0, connected as i32);
                cp.set_str(kOfxImageEffectPropPixelDepth, 0, if connected { depth.name() } else { kOfxBitDepthNone });
                cp.set_str(kOfxImageEffectPropComponents, 0, if connected { kOfxImageComponentRGBA } else { kOfxImageComponentNone });
                cp.set_str(kOfxImageClipPropUnmappedComponents, 0, kOfxImageComponentRGBA);
                cp.set_str(kOfxImageClipPropUnmappedPixelDepth, 0, depth.name());
                cp.set_str(kOfxImageEffectPropPreMultiplication, 0, kOfxImagePreMultiplied);
                cp.set_dbl(kOfxImagePropPixelAspectRatio, 0, 1.0);
                cp.set_dbl(kOfxImageEffectPropFrameRate, 0, fps);
                cp.set_dbl(kOfxImageEffectPropFrameRange, 0, 0.0);
                cp.set_dbl(kOfxImageEffectPropFrameRange, 1, 1.0e6);
                cp.set_int(kOfxImageClipPropContinuousSamples, 0, 0);
                cp.set_str(kOfxImageClipPropFieldOrder, 0, kOfxImageFieldNone);
            }
        }
        self.scale_normalised_defaults(w, h);
        self.fire_changes(time_frames);
        let entry = self.plugin.entry;
        let handle = self.effect as Handle;
        let window = |a: &PropSet| {
            for (i, v) in [0, 0, w as i32, h as i32].iter().enumerate() {
                a.set_int(kOfxImageEffectPropRenderWindow, i, *v);
            }
            a.set_dbl(kOfxImageEffectPropRenderScale, 0, 1.0);
            a.set_dbl(kOfxImageEffectPropRenderScale, 1, 1.0);
            a.set_str(kOfxImageEffectPropFieldToRender, 0, kOfxImageFieldNone);
            a.set_int(kOfxImageEffectPropSequentialRenderStatus, 0, 0);
            a.set_int(kOfxImageEffectPropInteractiveRenderStatus, 0, 0);
        };
        let run = |action: &str, with_time: bool, window_args: bool| -> Status {
            let args = PropSet::new_ptr();
            let a = unsafe { &*args };
            if with_time {
                a.set_dbl(kOfxPropTime, 0, time_frames);
            }
            if window_args {
                window(a);
            } else {
                a.set_dbl(kOfxImageEffectPropFrameRange, 0, time_frames);
                a.set_dbl(kOfxImageEffectPropFrameRange, 1, time_frames);
                a.set_dbl(kOfxImageEffectPropFrameStep, 0, 1.0);
                a.set_int(kOfxPropIsInteractive, 0, 0);
                a.set_dbl(kOfxImageEffectPropRenderScale, 0, 1.0);
                a.set_dbl(kOfxImageEffectPropRenderScale, 1, 1.0);
                a.set_int(kOfxImageEffectPropSequentialRenderStatus, 0, 0);
                a.set_int(kOfxImageEffectPropInteractiveRenderStatus, 0, 0);
            }
            let st = unsafe { call(entry, action, handle, args, std::ptr::null_mut()) };
            drop(unsafe { Box::from_raw(args) });
            st
        };
        let st = run(kOfxImageEffectActionBeginSequenceRender, false, false);
        if !ok(st) && st != kOfxStatErrUnknown {
            log::debug!("OFX BeginSequenceRender -> {st}");
        }
        let st = run(kOfxImageEffectActionRender, true, true);
        let _ = run(kOfxImageEffectActionEndSequenceRender, false, false);
        if st != kOfxStatOK {
            return Err(OfxError::Action { action: kOfxImageEffectActionRender.into(), status: st });
        }
        let bufs = self.eff().buffers.lock();
        Ok(unpack(&bufs.out, wu, hu))
    }
}

impl Drop for OfxInstance {
    fn drop(&mut self) {
        if self.created {
            unsafe {
                call(self.plugin.entry, kOfxActionDestroyInstance, self.effect as Handle, std::ptr::null_mut(), std::ptr::null_mut())
            };
        }
        live().lock().retain(|x| *x != self.effect as usize);
        // The instance's property sets are small; they are reclaimed with the process.
    }
}

// ---------------------------------------------------------------------------
// pixel conversion (rows flipped: OFX images are bottom-up)
// ---------------------------------------------------------------------------

fn pack(src: &[f32], w: usize, h: usize, depth: Depth) -> Buf {
    let row = w * 4 * depth.bytes();
    let mut data = vec![0u8; row * h];
    for y in 0..h {
        let s = &src[(h - 1 - y) * w * 4..(h - y) * w * 4];
        let d = &mut data[y * row..(y + 1) * row];
        match depth {
            Depth::Float => {
                for (i, v) in s.iter().enumerate() {
                    d[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
                }
            }
            Depth::Short => {
                for (i, v) in s.iter().enumerate() {
                    d[i * 2..i * 2 + 2].copy_from_slice(&((v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).to_ne_bytes());
                }
            }
            Depth::Byte => {
                for (i, v) in s.iter().enumerate() {
                    d[i] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
    }
    Buf { data, w: w as u32, h: h as u32, depth: Some(depth) }
}

fn unpack(buf: &Buf, w: usize, h: usize) -> Vec<f32> {
    let Some(depth) = buf.depth else { return vec![0.0; w * h * 4] };
    let row = buf.row_bytes();
    let mut out = vec![0.0f32; w * h * 4];
    for y in 0..h {
        let s = &buf.data[y * row..(y + 1) * row];
        let d = &mut out[(h - 1 - y) * w * 4..(h - y) * w * 4];
        match depth {
            Depth::Float => {
                for (i, v) in d.iter_mut().enumerate() {
                    *v = f32::from_ne_bytes([s[i * 4], s[i * 4 + 1], s[i * 4 + 2], s[i * 4 + 3]]);
                }
            }
            Depth::Short => {
                for (i, v) in d.iter_mut().enumerate() {
                    *v = u16::from_ne_bytes([s[i * 2], s[i * 2 + 1]]) as f32 / 65535.0;
                }
            }
            Depth::Byte => {
                for (i, v) in d.iter_mut().enumerate() {
                    *v = s[i] as f32 / 255.0;
                }
            }
        }
    }
    out
}
