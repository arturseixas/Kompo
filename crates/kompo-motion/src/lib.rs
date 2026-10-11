//! # kompo-motion
//!
//! The Motion page's engine: expressions, layer property evaluation with
//! parenting, vector shape and animated text rasterization, and the
//! conversion of a composition at a given time into a render graph.

pub mod eval;
pub mod expr;
pub mod raster;
pub mod render;

pub use render::{Footage, NoFootage, RenderOpts, render_comp};
