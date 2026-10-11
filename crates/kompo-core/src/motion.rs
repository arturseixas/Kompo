//! Motion composition data model (Motion page): an After Effects-like stack
//! of layers with parenting, shapes, animated text, footage, precomps and
//! expressions. Evaluation and rasterization live in `kompo-motion`.
//!
//! Every animatable value is an [`AnimatedParam`] addressed by a dotted
//! property name (`transform.position`, `text.size`, `shape.0.fill.color`,
//! ...), which is how the UI, expressions and the graph editor refer to it.

use crate::animation::AnimatedParam;
use crate::effects::{BlendMode, EffectInstance};
use crate::id::{CompId, LayerId, MediaId};
use crate::time::{Rational, Time};
use crate::timeline::Transform2D;
use serde::{Deserialize, Serialize};

/// A layered composition. `layers[0]` is the bottom layer.
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
    #[serde(default)]
    pub layers: Vec<Layer>,
}

impl Composition {
    pub fn new(name: impl Into<String>, width: u32, height: u32, frame_rate: Rational, duration: Time) -> Self {
        Self { id: CompId::new(), name: name.into(), width, height, frame_rate, duration, background: [0.0; 4], layers: Vec::new() }
    }

    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    pub fn layer_by_name(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    /// Adds a layer on top.
    pub fn add_layer(&mut self, layer: Layer) -> LayerId {
        let id = layer.id;
        self.layers.push(layer);
        id
    }

    /// `true` if making `parent` the parent of `child` would create a cycle.
    pub fn parent_would_cycle(&self, child: LayerId, parent: LayerId) -> bool {
        let mut cur = Some(parent);
        let mut guard = 0;
        while let Some(id) = cur {
            if id == child {
                return true;
            }
            cur = self.layer(id).and_then(|l| l.parent);
            guard += 1;
            if guard > self.layers.len() + 1 {
                return true;
            }
        }
        false
    }

    /// Compositions reachable from this one through precomp layers (including itself).
    pub fn references(&self) -> Vec<CompId> {
        self.layers.iter().filter_map(|l| if let LayerKind::Precomp { comp } = l.kind { Some(comp) } else { None }).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum LayerKind {
    Solid {
        color: [f64; 4],
        size: [f64; 2],
    },
    Shape(Vec<ShapeItem>),
    Text(TextLayer),
    /// A video, image or image sequence from the media pool.
    Footage {
        media: MediaId,
    },
    Precomp {
        comp: CompId,
    },
    /// Invisible: a parent for other layers.
    Null,
    /// Applies its effects to everything below it.
    Adjustment,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub kind: LayerKind,
    /// In point in the composition.
    pub start: Time,
    pub duration: Time,
    /// Time of the source shown at `start` (footage and precomps).
    #[serde(default)]
    pub source_offset: Time,
    #[serde(default)]
    pub parent: Option<LayerId>,
    pub transform: Transform2D,
    pub opacity: AnimatedParam,
    #[serde(default)]
    pub blend: BlendMode,
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
}

fn yes() -> bool {
    true
}

impl Layer {
    pub fn new(name: impl Into<String>, kind: LayerKind, start: Time, duration: Time) -> Self {
        Self {
            id: LayerId::new(),
            name: name.into(),
            enabled: true,
            kind,
            start,
            duration,
            source_offset: Time::ZERO,
            parent: None,
            transform: Transform2D::default(),
            opacity: AnimatedParam::float(1.0),
            blend: BlendMode::Normal,
            effects: Vec::new(),
        }
    }

    pub fn end(&self) -> Time {
        self.start + self.duration
    }

    /// `true` if the layer is active at comp time `t`.
    pub fn active_at(&self, t: Time) -> bool {
        self.enabled && t >= self.start && t < self.end()
    }

    /// Layer-local time at comp time `t`.
    pub fn local_time(&self, t: Time) -> Time {
        t - self.start
    }

    /// Names of every animatable property, in UI order.
    pub fn property_names(&self) -> Vec<String> {
        let mut v: Vec<String> = ["transform.anchor", "transform.position", "transform.scale", "transform.rotation", "transform.opacity"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        match &self.kind {
            LayerKind::Shape(items) => {
                for (i, it) in items.iter().enumerate() {
                    v.push(format!("shape.{i}.position"));
                    v.push(format!("shape.{i}.rotation"));
                    v.push(format!("shape.{i}.scale"));
                    match &it.geometry {
                        ShapeGeometry::Rect { .. } => {
                            v.push(format!("shape.{i}.size"));
                            v.push(format!("shape.{i}.roundness"));
                        }
                        ShapeGeometry::Ellipse { .. } => v.push(format!("shape.{i}.size")),
                        ShapeGeometry::Star { .. } => {
                            for p in ["points", "outer", "inner", "round"] {
                                v.push(format!("shape.{i}.{p}"));
                            }
                        }
                        ShapeGeometry::Path { .. } => {}
                    }
                    if it.fill.is_some() {
                        v.push(format!("shape.{i}.fill.color"));
                        v.push(format!("shape.{i}.fill.opacity"));
                    }
                    if it.stroke.is_some() {
                        v.push(format!("shape.{i}.stroke.color"));
                        v.push(format!("shape.{i}.stroke.width"));
                        v.push(format!("shape.{i}.stroke.opacity"));
                    }
                    v.push(format!("shape.{i}.trim.start"));
                    v.push(format!("shape.{i}.trim.end"));
                }
            }
            LayerKind::Text(t) => {
                for p in ["size", "color", "tracking"] {
                    v.push(format!("text.{p}"));
                }
                for (i, _) in t.animators.iter().enumerate() {
                    for p in ["start", "end", "offset", "opacity", "position", "scale", "rotation"] {
                        v.push(format!("text.anim.{i}.{p}"));
                    }
                }
            }
            _ => {}
        }
        v
    }
}

impl Layer {
    /// Looks a property up by its dotted name.
    pub fn prop(&self, key: &str) -> Option<&AnimatedParam> {
        let parts: Vec<&str> = key.split('.').collect();
        match parts.as_slice() {
            ["transform", p] => match *p {
                "position" => Some(&self.transform.position),
                "anchor" => Some(&self.transform.anchor),
                "scale" => Some(&self.transform.scale),
                "rotation" => Some(&self.transform.rotation),
                "opacity" => Some(&self.opacity),
                _ => None,
            },
            ["text", "anim", i, p] => {
                let LayerKind::Text(t) = &self.kind else { return None };
                let a = t.animators.get(i.parse::<usize>().ok()?)?;
                match *p {
                    "start" => Some(&a.start),
                    "end" => Some(&a.end),
                    "offset" => Some(&a.offset),
                    "opacity" => Some(&a.opacity),
                    "position" => Some(&a.position),
                    "scale" => Some(&a.scale),
                    "rotation" => Some(&a.rotation),
                    _ => None,
                }
            }
            ["text", p] => {
                let LayerKind::Text(t) = &self.kind else { return None };
                match *p {
                    "size" => Some(&t.size),
                    "color" => Some(&t.color),
                    "tracking" => Some(&t.tracking),
                    _ => None,
                }
            }
            ["shape", i, rest @ ..] => {
                let LayerKind::Shape(items) = &self.kind else { return None };
                let it = items.get(i.parse::<usize>().ok()?)?;
                match rest {
                    ["position"] => Some(&it.position),
                    ["rotation"] => Some(&it.rotation),
                    ["scale"] => Some(&it.scale),
                    ["trim", "start"] => Some(&it.trim_start),
                    ["trim", "end"] => Some(&it.trim_end),
                    ["fill", "color"] => it.fill.as_ref().map(|f| &f.color),
                    ["fill", "opacity"] => it.fill.as_ref().map(|f| &f.opacity),
                    ["stroke", "color"] => it.stroke.as_ref().map(|s| &s.color),
                    ["stroke", "width"] => it.stroke.as_ref().map(|s| &s.width),
                    ["stroke", "opacity"] => it.stroke.as_ref().map(|s| &s.opacity),
                    [g] => match (&it.geometry, *g) {
                        (ShapeGeometry::Rect { size, .. }, "size") | (ShapeGeometry::Ellipse { size }, "size") => Some(size),
                        (ShapeGeometry::Rect { roundness, .. }, "roundness") => Some(roundness),
                        (ShapeGeometry::Star { points, .. }, "points") => Some(points),
                        (ShapeGeometry::Star { outer, .. }, "outer") => Some(outer),
                        (ShapeGeometry::Star { inner, .. }, "inner") => Some(inner),
                        (ShapeGeometry::Star { round, .. }, "round") => Some(round),
                        _ => None,
                    },
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Looks a property up by its dotted name.
    pub fn prop_mut(&mut self, key: &str) -> Option<&mut AnimatedParam> {
        let parts: Vec<&str> = key.split('.').collect();
        match parts.as_slice() {
            ["transform", p] => match *p {
                "position" => Some(&mut self.transform.position),
                "anchor" => Some(&mut self.transform.anchor),
                "scale" => Some(&mut self.transform.scale),
                "rotation" => Some(&mut self.transform.rotation),
                "opacity" => Some(&mut self.opacity),
                _ => None,
            },
            ["text", "anim", i, p] => {
                let LayerKind::Text(t) = &mut self.kind else { return None };
                let a = t.animators.get_mut(i.parse::<usize>().ok()?)?;
                match *p {
                    "start" => Some(&mut a.start),
                    "end" => Some(&mut a.end),
                    "offset" => Some(&mut a.offset),
                    "opacity" => Some(&mut a.opacity),
                    "position" => Some(&mut a.position),
                    "scale" => Some(&mut a.scale),
                    "rotation" => Some(&mut a.rotation),
                    _ => None,
                }
            }
            ["text", p] => {
                let LayerKind::Text(t) = &mut self.kind else { return None };
                match *p {
                    "size" => Some(&mut t.size),
                    "color" => Some(&mut t.color),
                    "tracking" => Some(&mut t.tracking),
                    _ => None,
                }
            }
            ["shape", i, rest @ ..] => {
                let LayerKind::Shape(items) = &mut self.kind else { return None };
                let it = items.get_mut(i.parse::<usize>().ok()?)?;
                match rest {
                    ["position"] => Some(&mut it.position),
                    ["rotation"] => Some(&mut it.rotation),
                    ["scale"] => Some(&mut it.scale),
                    ["trim", "start"] => Some(&mut it.trim_start),
                    ["trim", "end"] => Some(&mut it.trim_end),
                    ["fill", "color"] => it.fill.as_mut().map(|f| &mut f.color),
                    ["fill", "opacity"] => it.fill.as_mut().map(|f| &mut f.opacity),
                    ["stroke", "color"] => it.stroke.as_mut().map(|s| &mut s.color),
                    ["stroke", "width"] => it.stroke.as_mut().map(|s| &mut s.width),
                    ["stroke", "opacity"] => it.stroke.as_mut().map(|s| &mut s.opacity),
                    [g] => match (&mut it.geometry, *g) {
                        (ShapeGeometry::Rect { size, .. }, "size") | (ShapeGeometry::Ellipse { size }, "size") => Some(size),
                        (ShapeGeometry::Rect { roundness, .. }, "roundness") => Some(roundness),
                        (ShapeGeometry::Star { points, .. }, "points") => Some(points),
                        (ShapeGeometry::Star { outer, .. }, "outer") => Some(outer),
                        (ShapeGeometry::Star { inner, .. }, "inner") => Some(inner),
                        (ShapeGeometry::Star { round, .. }, "round") => Some(round),
                        _ => None,
                    },
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

// ---- shapes ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathNode {
    pub point: [f64; 2],
    /// Tangent handles relative to the point.
    pub in_tangent: [f64; 2],
    pub out_tangent: [f64; 2],
}

impl PathNode {
    pub fn corner(x: f64, y: f64) -> Self {
        Self { point: [x, y], in_tangent: [0.0; 2], out_tangent: [0.0; 2] }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ShapeGeometry {
    Rect {
        size: AnimatedParam,
        /// Corner radius in pixels.
        roundness: AnimatedParam,
    },
    Ellipse {
        size: AnimatedParam,
    },
    /// Polygon (`star == false`) or star.
    Star {
        star: bool,
        points: AnimatedParam,
        outer: AnimatedParam,
        inner: AnimatedParam,
        round: AnimatedParam,
    },
    Path {
        nodes: Vec<PathNode>,
        closed: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeFill {
    pub color: AnimatedParam,
    pub opacity: AnimatedParam,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeStroke {
    pub color: AnimatedParam,
    pub width: AnimatedParam,
    pub opacity: AnimatedParam,
}

/// One shape: a geometry with its own transform, fill and stroke.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeItem {
    pub name: String,
    pub geometry: ShapeGeometry,
    pub position: AnimatedParam,
    pub rotation: AnimatedParam,
    pub scale: AnimatedParam,
    pub fill: Option<ShapeFill>,
    pub stroke: Option<ShapeStroke>,
    /// Trim paths, percent of the outline.
    pub trim_start: AnimatedParam,
    pub trim_end: AnimatedParam,
}

impl ShapeItem {
    fn base(name: &str, geometry: ShapeGeometry) -> Self {
        Self {
            name: name.into(),
            geometry,
            position: AnimatedParam::vec2(0.0, 0.0),
            rotation: AnimatedParam::float(0.0),
            scale: AnimatedParam::vec2(1.0, 1.0),
            fill: Some(ShapeFill {
                color: AnimatedParam::new(crate::animation::ParamValue::Color([0.95, 0.64, 0.23, 1.0])),
                opacity: AnimatedParam::float(1.0),
            }),
            stroke: None,
            trim_start: AnimatedParam::float(0.0),
            trim_end: AnimatedParam::float(100.0),
        }
    }

    pub fn rect(w: f64, h: f64) -> Self {
        Self::base("Rectangle", ShapeGeometry::Rect { size: AnimatedParam::vec2(w, h), roundness: AnimatedParam::float(0.0) })
    }

    pub fn ellipse(w: f64, h: f64) -> Self {
        Self::base("Ellipse", ShapeGeometry::Ellipse { size: AnimatedParam::vec2(w, h) })
    }

    pub fn polygon(points: f64, radius: f64) -> Self {
        Self::base(
            "Polygon",
            ShapeGeometry::Star {
                star: false,
                points: AnimatedParam::float(points),
                outer: AnimatedParam::float(radius),
                inner: AnimatedParam::float(radius * 0.5),
                round: AnimatedParam::float(0.0),
            },
        )
    }

    pub fn star(points: f64, outer: f64, inner: f64) -> Self {
        Self::base(
            "Star",
            ShapeGeometry::Star {
                star: true,
                points: AnimatedParam::float(points),
                outer: AnimatedParam::float(outer),
                inner: AnimatedParam::float(inner),
                round: AnimatedParam::float(0.0),
            },
        )
    }

    pub fn path(nodes: Vec<PathNode>, closed: bool) -> Self {
        Self::base("Path", ShapeGeometry::Path { nodes, closed })
    }

    pub fn with_stroke(mut self, color: [f64; 4], width: f64) -> Self {
        self.stroke = Some(ShapeStroke {
            color: AnimatedParam::new(crate::animation::ParamValue::Color(color)),
            width: AnimatedParam::float(width),
            opacity: AnimatedParam::float(1.0),
        });
        self
    }

    pub fn filled(mut self, color: [f64; 4]) -> Self {
        self.fill =
            Some(ShapeFill { color: AnimatedParam::new(crate::animation::ParamValue::Color(color)), opacity: AnimatedParam::float(1.0) });
        self
    }

    pub fn no_fill(mut self) -> Self {
        self.fill = None;
        self
    }
}

// ---- text -----------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Justify {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimatorBasis {
    #[default]
    Characters,
    Words,
    Lines,
}

/// A text animator: properties applied to the part of the text selected by
/// a range (After Effects' Range Selector, "Square" shape with softness).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextAnimator {
    pub name: String,
    pub basis: AnimatorBasis,
    /// Range start/end/offset in percent of the text.
    pub start: AnimatedParam,
    pub end: AnimatedParam,
    pub offset: AnimatedParam,
    /// Softness of the range edges, percent.
    #[serde(default)]
    pub smoothness: f64,
    /// Value reached by selected characters: percent (100 = unchanged).
    pub opacity: AnimatedParam,
    /// Offset in pixels.
    pub position: AnimatedParam,
    /// Percent (100 = unchanged).
    pub scale: AnimatedParam,
    /// Degrees.
    pub rotation: AnimatedParam,
}

impl TextAnimator {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            basis: AnimatorBasis::Characters,
            start: AnimatedParam::float(0.0),
            end: AnimatedParam::float(100.0),
            offset: AnimatedParam::float(0.0),
            smoothness: 0.0,
            opacity: AnimatedParam::float(100.0),
            position: AnimatedParam::vec2(0.0, 0.0),
            scale: AnimatedParam::float(100.0),
            rotation: AnimatedParam::float(0.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextLayer {
    pub text: String,
    pub font: String,
    pub size: AnimatedParam,
    pub color: AnimatedParam,
    pub tracking: AnimatedParam,
    pub justify: Justify,
    #[serde(default)]
    pub animators: Vec<TextAnimator>,
}

impl TextLayer {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            font: "Inter".into(),
            size: AnimatedParam::float(96.0),
            color: AnimatedParam::new(crate::animation::ParamValue::Color([1.0, 1.0, 1.0, 1.0])),
            tracking: AnimatedParam::float(0.0),
            justify: Justify::Center,
            animators: Vec::new(),
        }
    }
}
