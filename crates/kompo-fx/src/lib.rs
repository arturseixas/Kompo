//! # kompo-fx
//!
//! The effects and transitions library. Two kinds of effects share one
//! catalog and one data model (`kompo_core::effects::EffectInstance`, whose
//! parameters are keyframable [`AnimatedParam`]s):
//!
//! * **native** effects (`kompo.*`) run in the render graph on the GPU
//!   (Gaussian blur, color via baked 3D LUT) and are what realtime playback
//!   relies on;
//! * **library** effects (`ec.*`, 300+ of them: blurs, distortions, keying,
//!   stylize, noise, generators, ...) come from the vendored `effectcraft`
//!   effects crate (MIT OR Apache-2.0, see `third_party/effectcraft`) and run
//!   on the CPU through [`kompo_gpu::CpuOp`].

pub mod apply;
pub mod catalog;
pub mod transition;

pub use apply::{FxContext, apply_effects};
pub use catalog::{FxInfo, FxParam, FxUi, catalog, categories, find, new_instance};
pub use transition::{TransitionInfo, TransitionPlan, transitions};
