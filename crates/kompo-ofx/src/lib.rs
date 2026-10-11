//! # kompo-ofx
//!
//! An OpenFX 1.4 *host*: discovers `.ofx` bundles in the standard folders,
//! loads them with `libloading`, implements the host suites plug-ins fetch
//! (property, parameter, image effect, memory, multi-thread, message,
//! progress, time line) and runs image-effect instances on CPU buffers.
//!
//! Not implemented: OpenGL/GPU rendering (plug-ins that offer it are run on
//! the CPU), custom overlays/interacts, parametric parameters, and the
//! transition/retimer/paint contexts. See `docs/OFX.md`.

pub mod consts;
pub mod host;
pub mod plugin;

pub use host::ParamType;
pub use plugin::{OfxError, OfxInstance, ParamInfo, PluginInfo, default_search_paths, find, plugins, scan};
