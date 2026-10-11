//! Color correction data model (Color page). Processing lives in
//! `kompo-color` (math, LUTs, OCIO) and `kompo-gpu` (shaders).

use crate::animation::AnimatedParam;
use crate::id::NodeId;
use crate::time::Time;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// RGB + master (luma) triple used by the color wheels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WheelValue {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    /// Master (Y) adjustment applied equally to all channels.
    pub master: f64,
}

impl WheelValue {
    pub const fn splat(v: f64) -> Self {
        Self { r: v, g: v, b: v, master: v }
    }
    pub fn rgb(&self) -> [f64; 3] {
        [self.r, self.g, self.b]
    }
}

/// Primary correction. Lift/offset are additive (neutral 0), gamma/gain are
/// multiplicative (neutral 1). Operations run in the grading (log) space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Primaries {
    pub lift: WheelValue,
    pub gamma: WheelValue,
    pub gain: WheelValue,
    pub offset: WheelValue,
    pub contrast: f64,
    pub pivot: f64,
    pub saturation: f64,
    /// Kelvin shift relative to 6500 K (negative = cooler).
    pub temperature: f64,
    /// Green/magenta tint (-1..1).
    pub tint: f64,
    /// Hue rotation in degrees.
    pub hue: f64,
}

impl Default for Primaries {
    fn default() -> Self {
        Self {
            lift: WheelValue::splat(0.0),
            gamma: WheelValue::splat(1.0),
            gain: WheelValue::splat(1.0),
            offset: WheelValue::splat(0.0),
            contrast: 1.0,
            pivot: 0.336,
            saturation: 1.0,
            temperature: 0.0,
            tint: 0.0,
            hue: 0.0,
        }
    }
}

impl Primaries {
    pub fn is_identity(&self) -> bool {
        *self == Primaries::default()
    }
}

/// A spline curve through control points `(x, y)` in `[0, 1]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    pub points: Vec<[f64; 2]>,
}

impl Curve {
    pub fn identity() -> Self {
        Self { points: vec![[0.0, 0.0], [1.0, 1.0]] }
    }
    /// Flat curve at 0.5 — neutral for hue-vs-X curves (0.5 = no change).
    pub fn flat() -> Self {
        Self { points: vec![[0.0, 0.5], [1.0, 0.5]] }
    }
    pub fn is_identity(&self) -> bool {
        self.points.len() == 2 && self.points[0] == [0.0, 0.0] && self.points[1] == [1.0, 1.0]
    }
    pub fn is_flat(&self) -> bool {
        self.points.iter().all(|p| (p[1] - 0.5).abs() < 1e-9)
    }
}

impl Default for Curve {
    fn default() -> Self {
        Self::identity()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curves {
    pub master: Curve,
    pub red: Curve,
    pub green: Curve,
    pub blue: Curve,
    pub hue_vs_hue: Curve,
    pub hue_vs_sat: Curve,
    pub hue_vs_lum: Curve,
    pub lum_vs_sat: Curve,
}

impl Default for Curves {
    fn default() -> Self {
        Self {
            master: Curve::identity(),
            red: Curve::identity(),
            green: Curve::identity(),
            blue: Curve::identity(),
            hue_vs_hue: Curve::flat(),
            hue_vs_sat: Curve::flat(),
            hue_vs_lum: Curve::flat(),
            lum_vs_sat: Curve::flat(),
        }
    }
}

impl Curves {
    pub fn is_identity(&self) -> bool {
        self.master.is_identity()
            && self.red.is_identity()
            && self.green.is_identity()
            && self.blue.is_identity()
            && self.hue_vs_hue.is_flat()
            && self.hue_vs_sat.is_flat()
            && self.hue_vs_lum.is_flat()
            && self.lum_vs_sat.is_flat()
    }
}

/// HSL qualifier (keyer) for secondary corrections. Hue in degrees, others 0..1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Qualifier {
    pub enabled: bool,
    pub hue_center: f64,
    pub hue_width: f64,
    pub hue_softness: f64,
    pub sat_low: f64,
    pub sat_high: f64,
    pub sat_softness: f64,
    pub lum_low: f64,
    pub lum_high: f64,
    pub lum_softness: f64,
    /// Matte blur radius in pixels (at sequence resolution).
    pub blur: f64,
}

impl Default for Qualifier {
    fn default() -> Self {
        Self {
            enabled: false,
            hue_center: 30.0,
            hue_width: 40.0,
            hue_softness: 20.0,
            sat_low: 0.15,
            sat_high: 1.0,
            sat_softness: 0.1,
            lum_low: 0.05,
            lum_high: 0.95,
            lum_softness: 0.1,
            blur: 2.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WindowShape {
    Ellipse,
    Rectangle,
    /// Polygon vertices in normalized window space (-0.5..0.5).
    Polygon(Vec<[f64; 2]>),
}

/// A power window (mask). Center/size are normalized to the frame
/// (0..1, origin top-left); animated so tracking can write keyframes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PowerWindow {
    pub enabled: bool,
    pub shape: WindowShape,
    pub center: AnimatedParam,
    pub size: AnimatedParam,
    pub rotation: AnimatedParam,
    /// Feather as a fraction of the window size.
    pub softness: f64,
    pub invert: bool,
}

impl Default for PowerWindow {
    fn default() -> Self {
        Self {
            enabled: true,
            shape: WindowShape::Ellipse,
            center: AnimatedParam::vec2(0.5, 0.5),
            size: AnimatedParam::vec2(0.4, 0.4),
            rotation: AnimatedParam::float(0.0),
            softness: 0.15,
            invert: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LutRef {
    pub path: PathBuf,
    /// Mix between the input and the LUT result (0..1).
    pub strength: f64,
    /// The LUT expects DaVinci-Intermediate log input instead of Rec.709-encoded video.
    #[serde(default)]
    pub input_log: bool,
}

/// Where an OCIO transform leads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OcioTarget {
    /// Another color space of the config.
    Space(String),
    /// A display and view (display-referred output).
    Display { display: String, view: String },
}

/// An OpenColorIO transform applied at the head of a node: values are taken
/// as being in `source` and converted to `target`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OcioRef {
    pub config: PathBuf,
    pub source: String,
    pub target: OcioTarget,
}

/// One correction node. Nodes run in series; a node limited by a qualifier
/// and/or window affects only the matte area (secondary correction).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradeNode {
    pub id: NodeId,
    pub name: String,
    pub enabled: bool,
    pub primaries: Primaries,
    pub curves: Curves,
    pub lut: Option<LutRef>,
    #[serde(default)]
    pub ocio: Option<OcioRef>,
    pub qualifier: Qualifier,
    pub windows: Vec<PowerWindow>,
    /// Inverts the combined matte (qualifier x windows).
    pub invert_matte: bool,
}

impl GradeNode {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: NodeId::new(),
            name: name.into(),
            enabled: true,
            primaries: Primaries::default(),
            curves: Curves::default(),
            lut: None,
            ocio: None,
            qualifier: Qualifier::default(),
            windows: Vec::new(),
            invert_matte: false,
        }
    }

    pub fn is_identity(&self) -> bool {
        !self.enabled || (self.primaries.is_identity() && self.curves.is_identity() && self.lut.is_none() && self.ocio.is_none())
    }

    pub fn has_matte(&self) -> bool {
        self.qualifier.enabled || self.windows.iter().any(|w| w.enabled)
    }
}

/// Complete grade of a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorGrade {
    pub enabled: bool,
    pub nodes: Vec<GradeNode>,
}

impl Default for ColorGrade {
    fn default() -> Self {
        Self { enabled: true, nodes: Vec::new() }
    }
}

impl ColorGrade {
    pub fn is_identity(&self) -> bool {
        !self.enabled || self.nodes.iter().all(|n| n.is_identity())
    }

    /// The first node, created on demand (the "primary" node of the Color page).
    pub fn primary_node_mut(&mut self) -> &mut GradeNode {
        if self.nodes.is_empty() {
            self.nodes.push(GradeNode::new("Primary"));
        }
        &mut self.nodes[0]
    }

    pub fn is_animated(&self) -> bool {
        self.nodes.iter().any(|n| n.windows.iter().any(|w| w.center.is_animated() || w.size.is_animated() || w.rotation.is_animated()))
    }

    pub fn window_center(w: &PowerWindow, t: Time) -> [f64; 2] {
        w.center.eval(t).as_vec2()
    }
}
