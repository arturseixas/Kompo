//! Motion composition data model (Motion page). Evaluation (parenting,
//! expressions, text and shape rasterization) lives in `kompo-motion`.

use crate::id::CompId;
use crate::time::{Rational, Time};
use serde::{Deserialize, Serialize};

/// A layered composition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    pub id: CompId,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub duration: Time,
    #[serde(default)]
    pub background: [f64; 4],
}

impl Composition {
    pub fn new(name: impl Into<String>, width: u32, height: u32, frame_rate: Rational, duration: Time) -> Self {
        Self { id: CompId::new(), name: name.into(), width, height, frame_rate, duration, background: [0.0; 4] }
    }
}
