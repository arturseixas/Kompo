//! The OpenFX host side: property sets, parameters, clips and the suites
//! plug-ins fetch with `fetchSuite`.
//!
//! Handles passed across the C boundary are raw pointers to leaked boxes
//! (`PropSet`, `Param`, `Clip`, `Effect`); descriptors live as long as the
//! process, instances are freed when destroyed.

#![allow(clippy::missing_safety_doc, non_snake_case)]

use crate::consts::*;
use kompo_core::animation::AnimatedParam;
use kompo_core::time::Time;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};

pub type Status = c_int;
pub type Handle = *mut c_void;

// ---------------------------------------------------------------------------
// property sets
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum PV {
    I(i32),
    D(f64),
    S(CString),
    P(usize),
}

#[derive(Default)]
pub struct PropSet {
    map: Mutex<HashMap<String, Vec<PV>>>,
}

impl PropSet {
    pub fn new_ptr() -> *mut PropSet {
        Box::into_raw(Box::default())
    }

    pub fn clone_ptr(&self) -> *mut PropSet {
        Box::into_raw(Box::new(PropSet { map: Mutex::new(self.map.lock().clone()) }))
    }

    fn set_at(&self, name: &str, index: usize, v: PV) -> Status {
        let mut m = self.map.lock();
        let e = m.entry(name.to_string()).or_default();
        if index > e.len() {
            return kOfxStatErrBadIndex;
        }
        if index == e.len() {
            e.push(v);
        } else {
            e[index] = v;
        }
        kOfxStatOK
    }

    pub fn set_str(&self, name: &str, index: usize, v: &str) {
        self.set_at(name, index, PV::S(CString::new(v).unwrap_or_default()));
    }

    pub fn set_int(&self, name: &str, index: usize, v: i32) {
        self.set_at(name, index, PV::I(v));
    }

    pub fn set_dbl(&self, name: &str, index: usize, v: f64) {
        self.set_at(name, index, PV::D(v));
    }

    pub fn set_ptr(&self, name: &str, index: usize, v: usize) {
        self.set_at(name, index, PV::P(v));
    }

    pub fn dimension(&self, name: &str) -> usize {
        self.map.lock().get(name).map(|v| v.len()).unwrap_or(0)
    }

    pub fn has(&self, name: &str) -> bool {
        self.map.lock().contains_key(name)
    }

    pub fn get_str(&self, name: &str, index: usize) -> Option<String> {
        match self.map.lock().get(name)?.get(index)? {
            PV::S(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        }
    }

    pub fn strings(&self, name: &str) -> Vec<String> {
        (0..self.dimension(name)).filter_map(|i| self.get_str(name, i)).collect()
    }

    pub fn get_f(&self, name: &str, index: usize) -> Option<f64> {
        match self.map.lock().get(name)?.get(index)? {
            PV::D(d) => Some(*d),
            PV::I(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn get_i(&self, name: &str, index: usize) -> Option<i32> {
        match self.map.lock().get(name)?.get(index)? {
            PV::I(i) => Some(*i),
            PV::D(d) => Some(*d as i32),
            _ => None,
        }
    }

    pub fn get_p(&self, name: &str, index: usize) -> Option<usize> {
        match self.map.lock().get(name)?.get(index)? {
            PV::P(p) => Some(*p),
            _ => None,
        }
    }
}

unsafe fn ps<'a>(h: Handle) -> Option<&'a PropSet> {
    unsafe { (h as *const PropSet).as_ref() }
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

macro_rules! with_ps {
    ($h:expr, $p:expr, $body:expr) => {{
        match unsafe { ps($h) } {
            Some(set) => {
                let name = unsafe { cstr($p) };
                #[allow(clippy::redundant_closure_call)]
                ($body)(set, name)
            }
            None => kOfxStatErrBadHandle,
        }
    }};
}

unsafe extern "C" fn prop_set_pointer(h: Handle, p: *const c_char, i: c_int, v: *mut c_void) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| s.set_at(&n, i as usize, PV::P(v as usize)))
}
unsafe extern "C" fn prop_set_string(h: Handle, p: *const c_char, i: c_int, v: *const c_char) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| s.set_at(&n, i as usize, PV::S(CString::new(unsafe { cstr(v) }).unwrap_or_default())))
}
unsafe extern "C" fn prop_set_double(h: Handle, p: *const c_char, i: c_int, v: f64) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| s.set_at(&n, i as usize, PV::D(v)))
}
unsafe extern "C" fn prop_set_int(h: Handle, p: *const c_char, i: c_int, v: c_int) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| s.set_at(&n, i as usize, PV::I(v)))
}
unsafe extern "C" fn prop_set_pointer_n(h: Handle, p: *const c_char, c: c_int, v: *const *mut c_void) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| {
        for i in 0..c as usize {
            s.set_at(&n, i, PV::P(unsafe { *v.add(i) } as usize));
        }
        kOfxStatOK
    })
}
unsafe extern "C" fn prop_set_string_n(h: Handle, p: *const c_char, c: c_int, v: *const *const c_char) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| {
        for i in 0..c as usize {
            s.set_at(&n, i, PV::S(CString::new(unsafe { cstr(*v.add(i)) }).unwrap_or_default()));
        }
        kOfxStatOK
    })
}
unsafe extern "C" fn prop_set_double_n(h: Handle, p: *const c_char, c: c_int, v: *const f64) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| {
        for i in 0..c as usize {
            s.set_at(&n, i, PV::D(unsafe { *v.add(i) }));
        }
        kOfxStatOK
    })
}
unsafe extern "C" fn prop_set_int_n(h: Handle, p: *const c_char, c: c_int, v: *const c_int) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| {
        for i in 0..c as usize {
            s.set_at(&n, i, PV::I(unsafe { *v.add(i) }));
        }
        kOfxStatOK
    })
}

fn get_pv(s: &PropSet, name: &str, i: c_int) -> Result<PV, Status> {
    let m = s.map.lock();
    let Some(v) = m.get(name) else { return Err(kOfxStatErrUnknown) };
    v.get(i as usize).cloned().ok_or(kOfxStatErrBadIndex)
}

unsafe extern "C" fn prop_get_pointer(h: Handle, p: *const c_char, i: c_int, out: *mut *mut c_void) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| match get_pv(s, &n, i) {
        Ok(PV::P(v)) => {
            unsafe { *out = v as *mut c_void };
            kOfxStatOK
        }
        Ok(_) => kOfxStatErrValue,
        Err(e) => e,
    })
}
unsafe extern "C" fn prop_get_string(h: Handle, p: *const c_char, i: c_int, out: *mut *const c_char) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| {
        let m = s.map.lock();
        match m.get(&n) {
            None => kOfxStatErrUnknown,
            Some(v) => match v.get(i as usize) {
                // the CString lives in the property set; its heap buffer does not move
                Some(PV::S(c)) => {
                    unsafe { *out = c.as_ptr() };
                    kOfxStatOK
                }
                Some(_) => kOfxStatErrValue,
                None => kOfxStatErrBadIndex,
            },
        }
    })
}
unsafe extern "C" fn prop_get_double(h: Handle, p: *const c_char, i: c_int, out: *mut f64) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| match get_pv(s, &n, i) {
        Ok(PV::D(v)) => {
            unsafe { *out = v };
            kOfxStatOK
        }
        Ok(PV::I(v)) => {
            unsafe { *out = v as f64 };
            kOfxStatOK
        }
        Ok(_) => kOfxStatErrValue,
        Err(e) => e,
    })
}
unsafe extern "C" fn prop_get_int(h: Handle, p: *const c_char, i: c_int, out: *mut c_int) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| match get_pv(s, &n, i) {
        Ok(PV::I(v)) => {
            unsafe { *out = v };
            kOfxStatOK
        }
        Ok(PV::D(v)) => {
            unsafe { *out = v as i32 };
            kOfxStatOK
        }
        Ok(_) => kOfxStatErrValue,
        Err(e) => e,
    })
}
unsafe extern "C" fn prop_get_pointer_n(h: Handle, p: *const c_char, c: c_int, out: *mut *mut c_void) -> Status {
    for i in 0..c {
        let st = unsafe { prop_get_pointer(h, p, i, out.add(i as usize)) };
        if st != kOfxStatOK {
            return st;
        }
    }
    kOfxStatOK
}
unsafe extern "C" fn prop_get_string_n(h: Handle, p: *const c_char, c: c_int, out: *mut *const c_char) -> Status {
    for i in 0..c {
        let st = unsafe { prop_get_string(h, p, i, out.add(i as usize)) };
        if st != kOfxStatOK {
            return st;
        }
    }
    kOfxStatOK
}
unsafe extern "C" fn prop_get_double_n(h: Handle, p: *const c_char, c: c_int, out: *mut f64) -> Status {
    for i in 0..c {
        let st = unsafe { prop_get_double(h, p, i, out.add(i as usize)) };
        if st != kOfxStatOK {
            return st;
        }
    }
    kOfxStatOK
}
unsafe extern "C" fn prop_get_int_n(h: Handle, p: *const c_char, c: c_int, out: *mut c_int) -> Status {
    for i in 0..c {
        let st = unsafe { prop_get_int(h, p, i, out.add(i as usize)) };
        if st != kOfxStatOK {
            return st;
        }
    }
    kOfxStatOK
}
unsafe extern "C" fn prop_reset(h: Handle, p: *const c_char) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| {
        s.map.lock().remove(&n);
        kOfxStatOK
    })
}
unsafe extern "C" fn prop_get_dimension(h: Handle, p: *const c_char, out: *mut c_int) -> Status {
    with_ps!(h, p, |s: &PropSet, n: String| {
        unsafe { *out = s.dimension(&n) as c_int };
        kOfxStatOK
    })
}

#[repr(C)]
pub struct PropertySuite {
    set_pointer: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> Status,
    set_string: unsafe extern "C" fn(Handle, *const c_char, c_int, *const c_char) -> Status,
    set_double: unsafe extern "C" fn(Handle, *const c_char, c_int, f64) -> Status,
    set_int: unsafe extern "C" fn(Handle, *const c_char, c_int, c_int) -> Status,
    set_pointer_n: unsafe extern "C" fn(Handle, *const c_char, c_int, *const *mut c_void) -> Status,
    set_string_n: unsafe extern "C" fn(Handle, *const c_char, c_int, *const *const c_char) -> Status,
    set_double_n: unsafe extern "C" fn(Handle, *const c_char, c_int, *const f64) -> Status,
    set_int_n: unsafe extern "C" fn(Handle, *const c_char, c_int, *const c_int) -> Status,
    get_pointer: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut *mut c_void) -> Status,
    get_string: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut *const c_char) -> Status,
    get_double: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut f64) -> Status,
    get_int: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_int) -> Status,
    get_pointer_n: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut *mut c_void) -> Status,
    get_string_n: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut *const c_char) -> Status,
    get_double_n: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut f64) -> Status,
    get_int_n: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_int) -> Status,
    reset: unsafe extern "C" fn(Handle, *const c_char) -> Status,
    get_dimension: unsafe extern "C" fn(Handle, *const c_char, *mut c_int) -> Status,
}

static PROPERTY_SUITE: PropertySuite = PropertySuite {
    set_pointer: prop_set_pointer,
    set_string: prop_set_string,
    set_double: prop_set_double,
    set_int: prop_set_int,
    set_pointer_n: prop_set_pointer_n,
    set_string_n: prop_set_string_n,
    set_double_n: prop_set_double_n,
    set_int_n: prop_set_int_n,
    get_pointer: prop_get_pointer,
    get_string: prop_get_string,
    get_double: prop_get_double,
    get_int: prop_get_int,
    get_pointer_n: prop_get_pointer_n,
    get_string_n: prop_get_string_n,
    get_double_n: prop_get_double_n,
    get_int_n: prop_get_int_n,
    reset: prop_reset,
    get_dimension: prop_get_dimension,
};

// ---------------------------------------------------------------------------
// parameters
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    Int,
    Double,
    Boolean,
    Choice,
    Rgba,
    Rgb,
    Double2D,
    Int2D,
    Double3D,
    Int3D,
    String,
    Custom,
    PushButton,
    Group,
    Page,
}

impl ParamType {
    pub fn from_name(n: &str) -> Option<Self> {
        Some(match n {
            x if x == kOfxParamTypeInteger => Self::Int,
            x if x == kOfxParamTypeDouble => Self::Double,
            x if x == kOfxParamTypeBoolean => Self::Boolean,
            x if x == kOfxParamTypeChoice => Self::Choice,
            x if x == kOfxParamTypeRGBA => Self::Rgba,
            x if x == kOfxParamTypeRGB => Self::Rgb,
            x if x == kOfxParamTypeDouble2D => Self::Double2D,
            x if x == kOfxParamTypeInteger2D => Self::Int2D,
            x if x == kOfxParamTypeDouble3D => Self::Double3D,
            x if x == kOfxParamTypeInteger3D => Self::Int3D,
            x if x == kOfxParamTypeString => Self::String,
            x if x == kOfxParamTypeCustom => Self::Custom,
            x if x == kOfxParamTypePushButton => Self::PushButton,
            x if x == kOfxParamTypeGroup => Self::Group,
            x if x == kOfxParamTypePage => Self::Page,
            _ => return None,
        })
    }

    /// Number of numeric components.
    pub fn dims(self) -> usize {
        match self {
            Self::Int | Self::Double | Self::Boolean | Self::Choice => 1,
            Self::Rgba => 4,
            Self::Rgb | Self::Double3D | Self::Int3D => 3,
            Self::Double2D | Self::Int2D => 2,
            _ => 0,
        }
    }

    pub fn is_int(self) -> bool {
        matches!(self, Self::Int | Self::Int2D | Self::Int3D | Self::Boolean | Self::Choice)
    }

    pub fn is_string(self) -> bool {
        matches!(self, Self::String | Self::Custom)
    }
}

pub struct Param {
    pub name: String,
    pub ty: ParamType,
    pub props: *mut PropSet,
    pub value: Mutex<Vec<f64>>,
    pub text: Mutex<String>,
    /// Animation provided by the host (keyframes in clip time, seconds).
    pub anim: Mutex<Option<AnimatedParam>>,
    pub fps: Mutex<f64>,
}

unsafe impl Send for Param {}
unsafe impl Sync for Param {}

impl Param {
    pub fn new(name: &str, ty: ParamType, props: *mut PropSet) -> *mut Param {
        Box::into_raw(Box::new(Param {
            name: name.to_string(),
            ty,
            props,
            value: Mutex::new(vec![0.0; ty.dims()]),
            text: Mutex::new(String::new()),
            anim: Mutex::new(None),
            fps: Mutex::new(25.0),
        }))
    }

    fn props(&self) -> &PropSet {
        unsafe { &*self.props }
    }

    /// Takes the value from the `OfxParamPropDefault` property.
    pub fn apply_default(&self) {
        let p = self.props();
        if self.ty.is_string() {
            *self.text.lock() = p.get_str(kOfxParamPropDefault, 0).unwrap_or_default();
        } else {
            let mut v = self.value.lock();
            for (i, x) in v.iter_mut().enumerate() {
                *x = p.get_f(kOfxParamPropDefault, i).unwrap_or(0.0);
            }
        }
    }

    pub fn value_at(&self, t_frames: f64) -> Vec<f64> {
        if let Some(a) = self.anim.lock().as_ref() {
            let t = Time::from_seconds(t_frames / (*self.fps.lock()).max(1e-6));
            let v = a.eval(t);
            let mut out = match v {
                kompo_core::animation::ParamValue::Vec2(x) => x.to_vec(),
                kompo_core::animation::ParamValue::Vec3(x) => x.to_vec(),
                kompo_core::animation::ParamValue::Color(x) => x.to_vec(),
                other => vec![other.as_f64()],
            };
            out.resize(self.ty.dims().max(1), *out.last().unwrap_or(&0.0));
            return out;
        }
        self.value.lock().clone()
    }
}

unsafe fn param<'a>(h: Handle) -> Option<&'a Param> {
    unsafe { (h as *const Param).as_ref() }
}

unsafe extern "C" fn param_define(set: Handle, ty: *const c_char, name: *const c_char, props: *mut Handle) -> Status {
    let Some(e) = (unsafe { (set as *const Effect).as_ref() }) else { return kOfxStatErrBadHandle };
    let (ty_s, name) = unsafe { (cstr(ty), cstr(name)) };
    let Some(t) = ParamType::from_name(&ty_s) else { return kOfxStatErrUnsupported };
    if e.params.lock().iter().any(|p| unsafe { &**p }.name == name) {
        return kOfxStatErrExists;
    }
    let pr = PropSet::new_ptr();
    let set_props = unsafe { &*pr };
    set_props.set_str(kOfxPropType, 0, kOfxTypeParameter);
    set_props.set_str(kOfxPropName, 0, &name);
    set_props.set_str(kOfxParamPropType, 0, &ty_s);
    set_props.set_int(kOfxParamPropEnabled, 0, 1);
    set_props.set_int(kOfxParamPropSecret, 0, 0);
    set_props.set_int(kOfxParamPropAnimates, 0, 1);
    set_props.set_int(kOfxParamPropCanUndo, 0, 1);
    set_props.set_int(kOfxParamPropEvaluateOnChange, 0, 1);
    set_props.set_int(kOfxParamPropPersistant, 0, 1);
    let prm = Param::new(&name, t, pr);
    e.params.lock().push(prm);
    if !props.is_null() {
        unsafe { *props = pr as Handle };
    }
    kOfxStatOK
}

unsafe extern "C" fn param_get_handle(set: Handle, name: *const c_char, out: *mut Handle, props: *mut Handle) -> Status {
    let Some(e) = (unsafe { (set as *const Effect).as_ref() }) else { return kOfxStatErrBadHandle };
    let name = unsafe { cstr(name) };
    let found = e.params.lock().iter().copied().find(|p| unsafe { &**p }.name == name);
    match found {
        Some(p) => {
            unsafe {
                *out = p as Handle;
                if !props.is_null() {
                    *props = (*p).props as Handle;
                }
            }
            kOfxStatOK
        }
        None => kOfxStatErrUnknown,
    }
}

unsafe extern "C" fn param_set_get_property_set(set: Handle, out: *mut Handle) -> Status {
    let Some(e) = (unsafe { (set as *const Effect).as_ref() }) else { return kOfxStatErrBadHandle };
    unsafe { *out = e.param_set_props as Handle };
    kOfxStatOK
}

unsafe extern "C" fn param_get_property_set(h: Handle, out: *mut Handle) -> Status {
    let Some(p) = (unsafe { param(h) }) else { return kOfxStatErrBadHandle };
    unsafe { *out = p.props as Handle };
    kOfxStatOK
}

/// Raw variadic arguments of the parameter getters/setters. The C functions
/// are variadic, which Rust cannot define on stable; instead each platform
/// ABI's register/stack assignment is mirrored with fixed parameters.
#[derive(Clone, Copy, Default)]
struct VArgs {
    /// integer-class arguments (pointers, ints) in order
    ints: [usize; 4],
    /// floating-point arguments in order
    dbls: [f64; 4],
}

unsafe fn write_value(p: &Param, t_frames: Option<f64>, a: &VArgs) -> Status {
    let v = match t_frames {
        Some(t) => p.value_at(t),
        None => p.value.lock().clone(),
    };
    unsafe {
        if p.ty.is_string() {
            let out = a.ints[0] as *mut *const c_char;
            if out.is_null() {
                return kOfxStatErrBadHandle;
            }
            // keep the returned string alive in the parameter's property set
            let s = p.text.lock().clone();
            p.props().set_str("kompo.text", 0, &s);
            let mut ptr: *const c_char = std::ptr::null();
            prop_get_string(p.props as Handle, c"kompo.text".as_ptr(), 0, &mut ptr);
            *out = ptr;
            return kOfxStatOK;
        }
        for (i, x) in v.iter().enumerate().take(4) {
            let dst = a.ints[i];
            if dst == 0 {
                return kOfxStatErrBadHandle;
            }
            if p.ty.is_int() {
                *(dst as *mut c_int) = x.round() as c_int;
            } else {
                *(dst as *mut f64) = *x;
            }
        }
    }
    kOfxStatOK
}

fn store_value(p: &Param, a: &VArgs, ints_raw: bool) -> Status {
    if p.ty.is_string() {
        *p.text.lock() = unsafe { cstr(a.ints[0] as *const c_char) };
        return kOfxStatOK;
    }
    let mut v = p.value.lock();
    for i in 0..v.len().min(4) {
        v[i] = if p.ty.is_int() {
            // ints are passed as 32-bit values in the low half of an integer register/slot
            (if ints_raw { a.ints[i] as u32 as i32 } else { 0 }) as f64
        } else {
            a.dbls[i]
        };
    }
    kOfxStatOK
}

unsafe fn do_get(h: Handle, time: Option<f64>, a: VArgs) -> Status {
    let Some(p) = (unsafe { param(h) }) else { return kOfxStatErrBadHandle };
    unsafe { write_value(p, time, &a) }
}

fn do_set(h: Handle, time: Option<f64>, a: VArgs) -> Status {
    let Some(p) = (unsafe { param(h) }) else { return kOfxStatErrBadHandle };
    let _ = time; // host-side keyframing from plug-ins is not supported: set the current value
    store_value(p, &a, true)
}

// --- the variadic functions, one signature per ABI ---------------------------------------------

#[cfg(all(unix, not(all(target_os = "macos", target_arch = "aarch64"))))]
mod variadic {
    use super::*;
    // System V (x86-64 Linux/macOS) and AAPCS64 Linux: integer and floating point arguments are
    // assigned to separate register files in order.
    pub unsafe extern "C" fn get_value(h: Handle, i0: usize, i1: usize, i2: usize, i3: usize) -> Status {
        unsafe { do_get(h, None, VArgs { ints: [i0, i1, i2, i3], dbls: [0.0; 4] }) }
    }
    pub unsafe extern "C" fn get_value_at(h: Handle, i0: usize, i1: usize, i2: usize, i3: usize, t: f64) -> Status {
        unsafe { do_get(h, Some(t), VArgs { ints: [i0, i1, i2, i3], dbls: [0.0; 4] }) }
    }
    pub unsafe extern "C" fn set_value(
        h: Handle,
        i0: usize,
        i1: usize,
        i2: usize,
        i3: usize,
        d0: f64,
        d1: f64,
        d2: f64,
        d3: f64,
    ) -> Status {
        do_set(h, None, VArgs { ints: [i0, i1, i2, i3], dbls: [d0, d1, d2, d3] })
    }
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn set_value_at(
        h: Handle,
        i0: usize,
        i1: usize,
        i2: usize,
        i3: usize,
        t: f64,
        d0: f64,
        d1: f64,
        d2: f64,
        d3: f64,
    ) -> Status {
        do_set(h, Some(t), VArgs { ints: [i0, i1, i2, i3], dbls: [d0, d1, d2, d3] })
    }
}

#[cfg(windows)]
mod variadic {
    use super::*;
    // Win64: arguments are positional; variadic doubles are also passed in integer slots.
    pub unsafe extern "C" fn get_value(h: Handle, a0: usize, a1: usize, a2: usize, a3: usize) -> Status {
        unsafe { do_get(h, None, VArgs { ints: [a0, a1, a2, a3], dbls: [0.0; 4] }) }
    }
    pub unsafe extern "C" fn get_value_at(h: Handle, t: f64, a0: usize, a1: usize, a2: usize, a3: usize) -> Status {
        unsafe { do_get(h, Some(t), VArgs { ints: [a0, a1, a2, a3], dbls: [0.0; 4] }) }
    }
    pub unsafe extern "C" fn set_value(h: Handle, a0: usize, a1: usize, a2: usize, a3: usize) -> Status {
        let d = [f64::from_bits(a0 as u64), f64::from_bits(a1 as u64), f64::from_bits(a2 as u64), f64::from_bits(a3 as u64)];
        do_set(h, None, VArgs { ints: [a0, a1, a2, a3], dbls: d })
    }
    pub unsafe extern "C" fn set_value_at(h: Handle, t: f64, a0: usize, a1: usize, a2: usize, a3: usize) -> Status {
        let d = [f64::from_bits(a0 as u64), f64::from_bits(a1 as u64), f64::from_bits(a2 as u64), f64::from_bits(a3 as u64)];
        do_set(h, Some(t), VArgs { ints: [a0, a1, a2, a3], dbls: d })
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod variadic {
    use super::*;
    // Apple arm64: all variadic arguments live on the stack, after the 7 unused integer registers.
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn get_value(
        h: Handle,
        _r1: usize,
        _r2: usize,
        _r3: usize,
        _r4: usize,
        _r5: usize,
        _r6: usize,
        _r7: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
    ) -> Status {
        unsafe { do_get(h, None, VArgs { ints: [a0, a1, a2, a3], dbls: [0.0; 4] }) }
    }
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn get_value_at(
        h: Handle,
        t: f64,
        _r1: usize,
        _r2: usize,
        _r3: usize,
        _r4: usize,
        _r5: usize,
        _r6: usize,
        _r7: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
    ) -> Status {
        unsafe { do_get(h, Some(t), VArgs { ints: [a0, a1, a2, a3], dbls: [0.0; 4] }) }
    }
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn set_value(
        h: Handle,
        _r1: usize,
        _r2: usize,
        _r3: usize,
        _r4: usize,
        _r5: usize,
        _r6: usize,
        _r7: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
    ) -> Status {
        let d = [f64::from_bits(a0 as u64), f64::from_bits(a1 as u64), f64::from_bits(a2 as u64), f64::from_bits(a3 as u64)];
        do_set(h, None, VArgs { ints: [a0, a1, a2, a3], dbls: d })
    }
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn set_value_at(
        h: Handle,
        t: f64,
        _r1: usize,
        _r2: usize,
        _r3: usize,
        _r4: usize,
        _r5: usize,
        _r6: usize,
        _r7: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
    ) -> Status {
        let d = [f64::from_bits(a0 as u64), f64::from_bits(a1 as u64), f64::from_bits(a2 as u64), f64::from_bits(a3 as u64)];
        do_set(h, Some(t), VArgs { ints: [a0, a1, a2, a3], dbls: d })
    }
}

unsafe extern "C" fn param_get_derivative(_: Handle, _: f64) -> Status {
    kOfxStatErrUnsupported
}
unsafe extern "C" fn param_get_integral(_: Handle, _: f64, _: f64) -> Status {
    kOfxStatErrUnsupported
}
unsafe extern "C" fn param_get_num_keys(h: Handle, out: *mut c_uint) -> Status {
    let Some(p) = (unsafe { param(h) }) else { return kOfxStatErrBadHandle };
    unsafe { *out = p.anim.lock().as_ref().map(|a| a.keyframes.len()).unwrap_or(0) as c_uint };
    kOfxStatOK
}
unsafe extern "C" fn param_get_key_time(h: Handle, n: c_uint, out: *mut f64) -> Status {
    let Some(p) = (unsafe { param(h) }) else { return kOfxStatErrBadHandle };
    let fps = *p.fps.lock();
    match p.anim.lock().as_ref().and_then(|a| a.keyframes.get(n as usize)) {
        Some(k) => {
            unsafe { *out = k.time.seconds() * fps };
            kOfxStatOK
        }
        None => kOfxStatErrBadIndex,
    }
}
unsafe extern "C" fn param_get_key_index(h: Handle, t: f64, dir: c_int, out: *mut c_int) -> Status {
    let Some(p) = (unsafe { param(h) }) else { return kOfxStatErrBadHandle };
    let fps = *p.fps.lock();
    let g = p.anim.lock();
    let Some(a) = g.as_ref() else { return kOfxStatFailed };
    let times: Vec<f64> = a.keyframes.iter().map(|k| k.time.seconds() * fps).collect();
    let idx = match dir.cmp(&0) {
        std::cmp::Ordering::Equal => times.iter().position(|x| (x - t).abs() < 1e-6),
        std::cmp::Ordering::Greater => times.iter().position(|x| *x > t),
        std::cmp::Ordering::Less => times.iter().rposition(|x| *x < t),
    };
    match idx {
        Some(i) => {
            unsafe { *out = i as c_int };
            kOfxStatOK
        }
        None => kOfxStatFailed,
    }
}
unsafe extern "C" fn param_delete_key(_: Handle, _: f64) -> Status {
    kOfxStatErrUnsupported
}
unsafe extern "C" fn param_delete_all_keys(_: Handle) -> Status {
    kOfxStatErrUnsupported
}
unsafe extern "C" fn param_copy(_: Handle, _: Handle, _: f64, _: *const c_void) -> Status {
    kOfxStatErrUnsupported
}
unsafe extern "C" fn param_edit_begin(_: Handle, _: *const c_char) -> Status {
    kOfxStatOK
}
unsafe extern "C" fn param_edit_end(_: Handle) -> Status {
    kOfxStatOK
}

#[repr(C)]
pub struct ParameterSuite {
    define: unsafe extern "C" fn(Handle, *const c_char, *const c_char, *mut Handle) -> Status,
    get_handle: unsafe extern "C" fn(Handle, *const c_char, *mut Handle, *mut Handle) -> Status,
    set_get_property_set: unsafe extern "C" fn(Handle, *mut Handle) -> Status,
    get_property_set: unsafe extern "C" fn(Handle, *mut Handle) -> Status,
    get_value: *const c_void,
    get_value_at_time: *const c_void,
    get_derivative: unsafe extern "C" fn(Handle, f64) -> Status,
    get_integral: unsafe extern "C" fn(Handle, f64, f64) -> Status,
    set_value: *const c_void,
    set_value_at_time: *const c_void,
    get_num_keys: unsafe extern "C" fn(Handle, *mut c_uint) -> Status,
    get_key_time: unsafe extern "C" fn(Handle, c_uint, *mut f64) -> Status,
    get_key_index: unsafe extern "C" fn(Handle, f64, c_int, *mut c_int) -> Status,
    delete_key: unsafe extern "C" fn(Handle, f64) -> Status,
    delete_all_keys: unsafe extern "C" fn(Handle) -> Status,
    copy: unsafe extern "C" fn(Handle, Handle, f64, *const c_void) -> Status,
    edit_begin: unsafe extern "C" fn(Handle, *const c_char) -> Status,
    edit_end: unsafe extern "C" fn(Handle) -> Status,
}

unsafe impl Sync for ParameterSuite {}
unsafe impl Send for ParameterSuite {}

fn parameter_suite() -> &'static ParameterSuite {
    static S: std::sync::OnceLock<ParameterSuite> = std::sync::OnceLock::new();
    S.get_or_init(|| ParameterSuite {
        define: param_define,
        get_handle: param_get_handle,
        set_get_property_set: param_set_get_property_set,
        get_property_set: param_get_property_set,
        get_value: variadic::get_value as *const c_void,
        get_value_at_time: variadic::get_value_at as *const c_void,
        get_derivative: param_get_derivative,
        get_integral: param_get_integral,
        set_value: variadic::set_value as *const c_void,
        set_value_at_time: variadic::set_value_at as *const c_void,
        get_num_keys: param_get_num_keys,
        get_key_time: param_get_key_time,
        get_key_index: param_get_key_index,
        delete_key: param_delete_key,
        delete_all_keys: param_delete_all_keys,
        copy: param_copy,
        edit_begin: param_edit_begin,
        edit_end: param_edit_end,
    })
}

// ---------------------------------------------------------------------------
// effects, clips and images
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    Byte,
    Short,
    Float,
}

impl Depth {
    pub fn name(self) -> &'static str {
        match self {
            Depth::Byte => kOfxBitDepthByte,
            Depth::Short => kOfxBitDepthShort,
            Depth::Float => kOfxBitDepthFloat,
        }
    }
    pub fn bytes(self) -> usize {
        match self {
            Depth::Byte => 1,
            Depth::Short => 2,
            Depth::Float => 4,
        }
    }
}

/// A pixel buffer handed to the plug-in: RGBA, rows bottom-up.
#[derive(Default)]
pub struct Buf {
    pub data: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub depth: Option<Depth>,
}

impl Buf {
    pub fn row_bytes(&self) -> usize {
        self.w as usize * 4 * self.depth.map(|d| d.bytes()).unwrap_or(4)
    }
}

#[derive(Default)]
pub struct RenderBuffers {
    pub src: Buf,
    pub out: Buf,
}

pub struct Clip {
    pub name: String,
    pub props: *mut PropSet,
    pub effect: *mut Effect,
}

unsafe impl Send for Clip {}
unsafe impl Sync for Clip {}

/// An image-effect descriptor or instance.
pub struct Effect {
    pub props: *mut PropSet,
    pub param_set_props: *mut PropSet,
    pub params: Mutex<Vec<*mut Param>>,
    pub clips: Mutex<Vec<*mut Clip>>,
    pub plugin_id: String,
    pub is_instance: bool,
    pub buffers: Mutex<RenderBuffers>,
    /// Image property sets handed out and not yet released.
    pub images: Mutex<Vec<usize>>,
    pub time: Mutex<(f64, f64, f64)>,
    pub persistent_message: Mutex<String>,
}

unsafe impl Send for Effect {}
unsafe impl Sync for Effect {}

impl Effect {
    pub fn new(plugin_id: &str, is_instance: bool) -> *mut Effect {
        let props = PropSet::new_ptr();
        let e = Box::new(Effect {
            props,
            param_set_props: PropSet::new_ptr(),
            params: Mutex::new(Vec::new()),
            clips: Mutex::new(Vec::new()),
            plugin_id: plugin_id.to_string(),
            is_instance,
            buffers: Mutex::new(RenderBuffers::default()),
            images: Mutex::new(Vec::new()),
            time: Mutex::new((0.0, 0.0, 1.0)),
            persistent_message: Mutex::new(String::new()),
        });
        let p = unsafe { &*props };
        p.set_str(kOfxPropType, 0, kOfxTypeImageEffect);
        Box::into_raw(e)
    }

    pub fn props(&self) -> &PropSet {
        unsafe { &*self.props }
    }

    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params.lock().iter().map(|p| unsafe { &**p }).find(|p| p.name == name)
    }

    pub fn clip(&self, name: &str) -> Option<*mut Clip> {
        self.clips.lock().iter().copied().find(|c| unsafe { &**c }.name == name)
    }

    pub fn define_clip(&self, name: &str) -> *mut Clip {
        let props = PropSet::new_ptr();
        let p = unsafe { &*props };
        p.set_str(kOfxPropType, 0, kOfxTypeClip);
        p.set_str(kOfxPropName, 0, name);
        p.set_int(kOfxImageClipPropOptional, 0, 0);
        let c = Box::into_raw(Box::new(Clip { name: name.to_string(), props, effect: std::ptr::null_mut() }));
        self.clips.lock().push(c);
        c
    }

    /// Instance copy of a descriptor: same clips and parameters with default values.
    pub fn instantiate(&self) -> *mut Effect {
        let inst = Effect::new(&self.plugin_id, true);
        let e = unsafe { &mut *inst };
        for c in self.clips.lock().iter() {
            let c = unsafe { &**c };
            let ip = unsafe { (*c.props).clone_ptr() };
            let nc = Box::into_raw(Box::new(Clip { name: c.name.clone(), props: ip, effect: inst }));
            e.clips.lock().push(nc);
        }
        for p in self.params.lock().iter() {
            let p = unsafe { &**p };
            let ip = unsafe { (*p.props).clone_ptr() };
            let np = Param::new(&p.name, p.ty, ip);
            unsafe { (*np).apply_default() };
            e.params.lock().push(np);
        }
        inst
    }
}

unsafe fn effect<'a>(h: Handle) -> Option<&'a Effect> {
    unsafe { (h as *const Effect).as_ref() }
}

unsafe extern "C" fn ie_get_property_set(h: Handle, out: *mut Handle) -> Status {
    let Some(e) = (unsafe { effect(h) }) else { return kOfxStatErrBadHandle };
    unsafe { *out = e.props as Handle };
    kOfxStatOK
}

unsafe extern "C" fn ie_get_param_set(h: Handle, out: *mut Handle) -> Status {
    if h.is_null() {
        return kOfxStatErrBadHandle;
    }
    // the effect doubles as its parameter set
    unsafe { *out = h };
    kOfxStatOK
}

unsafe extern "C" fn ie_clip_define(h: Handle, name: *const c_char, props: *mut Handle) -> Status {
    let Some(e) = (unsafe { effect(h) }) else { return kOfxStatErrBadHandle };
    let name = unsafe { cstr(name) };
    if e.clip(&name).is_some() {
        return kOfxStatErrExists;
    }
    let c = e.define_clip(&name);
    unsafe {
        (*c).effect = h as *mut Effect;
        if !props.is_null() {
            *props = (*c).props as Handle;
        }
    }
    kOfxStatOK
}

unsafe extern "C" fn ie_clip_get_handle(h: Handle, name: *const c_char, clip: *mut Handle, props: *mut Handle) -> Status {
    let Some(e) = (unsafe { effect(h) }) else { return kOfxStatErrBadHandle };
    let name = unsafe { cstr(name) };
    match e.clip(&name) {
        Some(c) => {
            unsafe {
                *clip = c as Handle;
                if !props.is_null() {
                    *props = (*c).props as Handle;
                }
            }
            kOfxStatOK
        }
        None => kOfxStatErrUnknown,
    }
}

unsafe extern "C" fn ie_clip_get_property_set(h: Handle, out: *mut Handle) -> Status {
    let Some(c) = (unsafe { (h as *const Clip).as_ref() }) else { return kOfxStatErrBadHandle };
    unsafe { *out = c.props as Handle };
    kOfxStatOK
}

#[repr(C)]
pub struct RectD {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

unsafe extern "C" fn ie_clip_get_image(h: Handle, _time: f64, _region: *const RectD, out: *mut Handle) -> Status {
    let Some(c) = (unsafe { (h as *const Clip).as_ref() }) else { return kOfxStatErrBadHandle };
    let Some(e) = (unsafe { c.effect.as_ref() }) else { return kOfxStatErrBadHandle };
    let cp = unsafe { &*c.props };
    if cp.get_i(kOfxImageClipPropConnected, 0) != Some(1) {
        return kOfxStatFailed;
    }
    let mut bufs = e.buffers.lock();
    let buf = if c.name == kOfxImageEffectOutputClipName { &mut bufs.out } else { &mut bufs.src };
    let Some(depth) = buf.depth else { return kOfxStatFailed };
    if buf.data.is_empty() {
        return kOfxStatFailed;
    }
    let img = PropSet::new_ptr();
    let p = unsafe { &*img };
    p.set_str(kOfxPropType, 0, kOfxTypeImage);
    p.set_ptr(kOfxImagePropData, 0, buf.data.as_mut_ptr() as usize);
    for (i, v) in [0, 0, buf.w as i32, buf.h as i32].iter().enumerate() {
        p.set_int(kOfxImagePropBounds, i, *v);
        p.set_int(kOfxImagePropRegionOfDefinition, i, *v);
    }
    p.set_int(kOfxImagePropRowBytes, 0, buf.row_bytes() as i32);
    p.set_dbl(kOfxImagePropPixelAspectRatio, 0, 1.0);
    p.set_str(kOfxImageEffectPropPixelDepth, 0, depth.name());
    p.set_str(kOfxImageEffectPropComponents, 0, kOfxImageComponentRGBA);
    p.set_str(kOfxImageEffectPropPreMultiplication, 0, kOfxImagePreMultiplied);
    p.set_str(kOfxImagePropField, 0, kOfxImageFieldNone);
    p.set_dbl(kOfxImageEffectPropRenderScale, 0, 1.0);
    p.set_dbl(kOfxImageEffectPropRenderScale, 1, 1.0);
    p.set_str(kOfxImagePropUniqueIdentifier, 0, &format!("{}-{:p}", c.name, img));
    e.images.lock().push(img as usize);
    unsafe { *out = img as Handle };
    kOfxStatOK
}

unsafe extern "C" fn ie_clip_release_image(h: Handle) -> Status {
    // find the owning effect through the registry of live images
    let key = h as usize;
    let mut found = false;
    for e in crate::plugin::live_effects() {
        let e = unsafe { &*e };
        let mut imgs = e.images.lock();
        if let Some(pos) = imgs.iter().position(|p| *p == key) {
            imgs.remove(pos);
            found = true;
            break;
        }
    }
    if found {
        drop(unsafe { Box::from_raw(h as *mut PropSet) });
        kOfxStatOK
    } else {
        kOfxStatErrBadHandle
    }
}

unsafe extern "C" fn ie_clip_get_rod(h: Handle, _time: f64, out: *mut RectD) -> Status {
    let Some(c) = (unsafe { (h as *const Clip).as_ref() }) else { return kOfxStatErrBadHandle };
    let Some(e) = (unsafe { c.effect.as_ref() }) else { return kOfxStatErrBadHandle };
    let bufs = e.buffers.lock();
    let buf = if c.name == kOfxImageEffectOutputClipName { &bufs.out } else { &bufs.src };
    unsafe {
        *out = RectD { x1: 0.0, y1: 0.0, x2: buf.w as f64, y2: buf.h as f64 };
    }
    kOfxStatOK
}

unsafe extern "C" fn ie_abort(_: Handle) -> c_int {
    0
}

unsafe fn alloc(n: usize) -> *mut u8 {
    let layout = std::alloc::Layout::from_size_align(n + 32, 32).unwrap_or(std::alloc::Layout::new::<u8>());
    let p = unsafe { std::alloc::alloc_zeroed(layout) };
    if p.is_null() {
        return p;
    }
    unsafe { *(p as *mut usize) = n };
    unsafe { p.add(32) }
}

unsafe fn dealloc(p: *mut u8) {
    if p.is_null() {
        return;
    }
    let base = unsafe { p.sub(32) };
    let n = unsafe { *(base as *const usize) };
    let layout = std::alloc::Layout::from_size_align(n + 32, 32).unwrap_or(std::alloc::Layout::new::<u8>());
    unsafe { std::alloc::dealloc(base, layout) };
}

unsafe extern "C" fn ie_image_memory_alloc(_: Handle, n: usize, out: *mut Handle) -> Status {
    let p = unsafe { alloc(n) };
    if p.is_null() {
        return kOfxStatErrMemory;
    }
    unsafe { *out = p as Handle };
    kOfxStatOK
}
unsafe extern "C" fn ie_image_memory_free(h: Handle) -> Status {
    unsafe { dealloc(h as *mut u8) };
    kOfxStatOK
}
unsafe extern "C" fn ie_image_memory_lock(h: Handle, out: *mut *mut c_void) -> Status {
    unsafe { *out = h };
    kOfxStatOK
}
unsafe extern "C" fn ie_image_memory_unlock(_: Handle) -> Status {
    kOfxStatOK
}

#[repr(C)]
pub struct ImageEffectSuite {
    get_property_set: unsafe extern "C" fn(Handle, *mut Handle) -> Status,
    get_param_set: unsafe extern "C" fn(Handle, *mut Handle) -> Status,
    clip_define: unsafe extern "C" fn(Handle, *const c_char, *mut Handle) -> Status,
    clip_get_handle: unsafe extern "C" fn(Handle, *const c_char, *mut Handle, *mut Handle) -> Status,
    clip_get_property_set: unsafe extern "C" fn(Handle, *mut Handle) -> Status,
    clip_get_image: unsafe extern "C" fn(Handle, f64, *const RectD, *mut Handle) -> Status,
    clip_release_image: unsafe extern "C" fn(Handle) -> Status,
    clip_get_rod: unsafe extern "C" fn(Handle, f64, *mut RectD) -> Status,
    abort: unsafe extern "C" fn(Handle) -> c_int,
    image_memory_alloc: unsafe extern "C" fn(Handle, usize, *mut Handle) -> Status,
    image_memory_free: unsafe extern "C" fn(Handle) -> Status,
    image_memory_lock: unsafe extern "C" fn(Handle, *mut *mut c_void) -> Status,
    image_memory_unlock: unsafe extern "C" fn(Handle) -> Status,
}

static IMAGE_EFFECT_SUITE: ImageEffectSuite = ImageEffectSuite {
    get_property_set: ie_get_property_set,
    get_param_set: ie_get_param_set,
    clip_define: ie_clip_define,
    clip_get_handle: ie_clip_get_handle,
    clip_get_property_set: ie_clip_get_property_set,
    clip_get_image: ie_clip_get_image,
    clip_release_image: ie_clip_release_image,
    clip_get_rod: ie_clip_get_rod,
    abort: ie_abort,
    image_memory_alloc: ie_image_memory_alloc,
    image_memory_free: ie_image_memory_free,
    image_memory_lock: ie_image_memory_lock,
    image_memory_unlock: ie_image_memory_unlock,
};

// ---------------------------------------------------------------------------
// memory, threads, messages, progress, timeline
// ---------------------------------------------------------------------------

unsafe extern "C" fn mem_alloc(_: Handle, n: usize, out: *mut *mut c_void) -> Status {
    let p = unsafe { alloc(n) };
    if p.is_null() {
        return kOfxStatErrMemory;
    }
    unsafe { *out = p as *mut c_void };
    kOfxStatOK
}
unsafe extern "C" fn mem_free(p: *mut c_void) -> Status {
    unsafe { dealloc(p as *mut u8) };
    kOfxStatOK
}

#[repr(C)]
pub struct MemorySuite {
    alloc: unsafe extern "C" fn(Handle, usize, *mut *mut c_void) -> Status,
    free: unsafe extern "C" fn(*mut c_void) -> Status,
}
static MEMORY_SUITE: MemorySuite = MemorySuite { alloc: mem_alloc, free: mem_free };

type ThreadFn = unsafe extern "C" fn(c_uint, c_uint, *mut c_void);

thread_local! {
    static THREAD_INDEX: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

unsafe extern "C" fn mt_multi_thread(f: ThreadFn, n: c_uint, arg: *mut c_void) -> Status {
    let arg = arg as usize;
    let n = n.max(1);
    rayon::scope(|s| {
        for i in 0..n {
            s.spawn(move |_| {
                THREAD_INDEX.with(|t| t.set(Some(i)));
                unsafe { f(i, n, arg as *mut c_void) };
                THREAD_INDEX.with(|t| t.set(None));
            });
        }
    });
    kOfxStatOK
}
unsafe extern "C" fn mt_num_cpus(out: *mut c_uint) -> Status {
    unsafe { *out = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as c_uint };
    kOfxStatOK
}
unsafe extern "C" fn mt_index(out: *mut c_uint) -> Status {
    match THREAD_INDEX.with(|t| t.get()) {
        Some(i) => {
            unsafe { *out = i };
            kOfxStatOK
        }
        None => kOfxStatFailed,
    }
}
unsafe extern "C" fn mt_is_spawned() -> c_int {
    THREAD_INDEX.with(|t| t.get().is_some()) as c_int
}

/// Recursive mutex with an explicit owner (OFX mutexes lock and unlock across calls).
struct OfxMutex {
    state: std::sync::Mutex<(Option<std::thread::ThreadId>, u32)>,
    cv: std::sync::Condvar,
}

unsafe extern "C" fn mt_mutex_create(out: *mut Handle, _lock_count: c_int) -> Status {
    let m = Box::new(OfxMutex { state: std::sync::Mutex::new((None, 0)), cv: std::sync::Condvar::new() });
    unsafe { *out = Box::into_raw(m) as Handle };
    kOfxStatOK
}
unsafe extern "C" fn mt_mutex_destroy(h: Handle) -> Status {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h as *mut OfxMutex) });
    }
    kOfxStatOK
}
unsafe extern "C" fn mt_mutex_lock(h: Handle) -> Status {
    let Some(m) = (unsafe { (h as *const OfxMutex).as_ref() }) else { return kOfxStatErrBadHandle };
    let me = std::thread::current().id();
    let mut g = m.state.lock().unwrap_or_else(|e| e.into_inner());
    while g.0.is_some() && g.0 != Some(me) {
        g = m.cv.wait(g).unwrap_or_else(|e| e.into_inner());
    }
    g.0 = Some(me);
    g.1 += 1;
    kOfxStatOK
}
unsafe extern "C" fn mt_mutex_unlock(h: Handle) -> Status {
    let Some(m) = (unsafe { (h as *const OfxMutex).as_ref() }) else { return kOfxStatErrBadHandle };
    let mut g = m.state.lock().unwrap_or_else(|e| e.into_inner());
    if g.1 > 0 {
        g.1 -= 1;
        if g.1 == 0 {
            g.0 = None;
            m.cv.notify_one();
        }
    }
    kOfxStatOK
}
unsafe extern "C" fn mt_mutex_try_lock(h: Handle) -> Status {
    let Some(m) = (unsafe { (h as *const OfxMutex).as_ref() }) else { return kOfxStatErrBadHandle };
    let me = std::thread::current().id();
    let mut g = m.state.lock().unwrap_or_else(|e| e.into_inner());
    if g.0.is_some() && g.0 != Some(me) {
        return kOfxStatFailed;
    }
    g.0 = Some(me);
    g.1 += 1;
    kOfxStatOK
}

#[repr(C)]
pub struct MultiThreadSuite {
    multi_thread: unsafe extern "C" fn(ThreadFn, c_uint, *mut c_void) -> Status,
    num_cpus: unsafe extern "C" fn(*mut c_uint) -> Status,
    index: unsafe extern "C" fn(*mut c_uint) -> Status,
    is_spawned: unsafe extern "C" fn() -> c_int,
    mutex_create: unsafe extern "C" fn(*mut Handle, c_int) -> Status,
    mutex_destroy: unsafe extern "C" fn(Handle) -> Status,
    mutex_lock: unsafe extern "C" fn(Handle) -> Status,
    mutex_unlock: unsafe extern "C" fn(Handle) -> Status,
    mutex_try_lock: unsafe extern "C" fn(Handle) -> Status,
}
static MULTI_THREAD_SUITE: MultiThreadSuite = MultiThreadSuite {
    multi_thread: mt_multi_thread,
    num_cpus: mt_num_cpus,
    index: mt_index,
    is_spawned: mt_is_spawned,
    mutex_create: mt_mutex_create,
    mutex_destroy: mt_mutex_destroy,
    mutex_lock: mt_mutex_lock,
    mutex_unlock: mt_mutex_unlock,
    mutex_try_lock: mt_mutex_try_lock,
};

// The message functions are variadic in C; only their named parameters are read, so a fixed
// signature is ABI compatible on every platform.
unsafe extern "C" fn msg_message(_: Handle, ty: *const c_char, _id: *const c_char, fmt: *const c_char) -> Status {
    let (ty, fmt) = unsafe { (cstr(ty), cstr(fmt)) };
    log::info!("OFX {ty}: {fmt}");
    if ty == kOfxMessageQuestion { kOfxStatReplyYes } else { kOfxStatOK }
}
unsafe extern "C" fn msg_set_persistent(h: Handle, _ty: *const c_char, _id: *const c_char, fmt: *const c_char) -> Status {
    if let Some(e) = unsafe { effect(h) } {
        *e.persistent_message.lock() = unsafe { cstr(fmt) };
    }
    kOfxStatOK
}
unsafe extern "C" fn msg_clear_persistent(h: Handle) -> Status {
    if let Some(e) = unsafe { effect(h) } {
        e.persistent_message.lock().clear();
    }
    kOfxStatOK
}

#[repr(C)]
pub struct MessageSuiteV1 {
    message: unsafe extern "C" fn(Handle, *const c_char, *const c_char, *const c_char) -> Status,
}
#[repr(C)]
pub struct MessageSuiteV2 {
    message: unsafe extern "C" fn(Handle, *const c_char, *const c_char, *const c_char) -> Status,
    set_persistent: unsafe extern "C" fn(Handle, *const c_char, *const c_char, *const c_char) -> Status,
    clear_persistent: unsafe extern "C" fn(Handle) -> Status,
}
static MESSAGE_V1: MessageSuiteV1 = MessageSuiteV1 { message: msg_message };
static MESSAGE_V2: MessageSuiteV2 =
    MessageSuiteV2 { message: msg_message, set_persistent: msg_set_persistent, clear_persistent: msg_clear_persistent };

unsafe extern "C" fn prog_start(_: Handle, _label: *const c_char) -> Status {
    kOfxStatOK
}
unsafe extern "C" fn prog_update(_: Handle, _p: f64) -> Status {
    kOfxStatOK
}
unsafe extern "C" fn prog_end(_: Handle) -> Status {
    kOfxStatOK
}
#[repr(C)]
pub struct ProgressSuite {
    start: unsafe extern "C" fn(Handle, *const c_char) -> Status,
    update: unsafe extern "C" fn(Handle, f64) -> Status,
    end: unsafe extern "C" fn(Handle) -> Status,
}
static PROGRESS_SUITE: ProgressSuite = ProgressSuite { start: prog_start, update: prog_update, end: prog_end };

unsafe extern "C" fn tl_get_time(h: Handle, out: *mut f64) -> Status {
    let Some(e) = (unsafe { effect(h) }) else { return kOfxStatErrBadHandle };
    unsafe { *out = e.time.lock().0 };
    kOfxStatOK
}
unsafe extern "C" fn tl_goto_time(_: Handle, _t: f64) -> Status {
    kOfxStatOK
}
unsafe extern "C" fn tl_bounds(h: Handle, a: *mut f64, b: *mut f64) -> Status {
    let Some(e) = (unsafe { effect(h) }) else { return kOfxStatErrBadHandle };
    let (_, first, last) = *e.time.lock();
    unsafe {
        *a = first;
        *b = last;
    }
    kOfxStatOK
}
#[repr(C)]
pub struct TimeLineSuite {
    get_time: unsafe extern "C" fn(Handle, *mut f64) -> Status,
    goto_time: unsafe extern "C" fn(Handle, f64) -> Status,
    bounds: unsafe extern "C" fn(Handle, *mut f64, *mut f64) -> Status,
}
static TIMELINE_SUITE: TimeLineSuite = TimeLineSuite { get_time: tl_get_time, goto_time: tl_goto_time, bounds: tl_bounds };

// ---------------------------------------------------------------------------
// the host struct
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct OfxHost {
    pub host: Handle,
    pub fetch_suite: unsafe extern "C" fn(Handle, *const c_char, c_int) -> *const c_void,
}

unsafe impl Sync for OfxHost {}
unsafe impl Send for OfxHost {}

unsafe extern "C" fn fetch_suite(_: Handle, name: *const c_char, version: c_int) -> *const c_void {
    let name = unsafe { cstr(name) };
    let p: *const c_void = match (name.as_str(), version) {
        (n, 1) if n == kOfxPropertySuite => &PROPERTY_SUITE as *const _ as *const c_void,
        (n, 1) if n == kOfxParameterSuite => parameter_suite() as *const _ as *const c_void,
        (n, 1) if n == kOfxImageEffectSuite => &IMAGE_EFFECT_SUITE as *const _ as *const c_void,
        (n, 1) if n == kOfxMemorySuite => &MEMORY_SUITE as *const _ as *const c_void,
        (n, 1) if n == kOfxMultiThreadSuite => &MULTI_THREAD_SUITE as *const _ as *const c_void,
        (n, 1) if n == kOfxMessageSuite => &MESSAGE_V1 as *const _ as *const c_void,
        (n, 2) if n == kOfxMessageSuite => &MESSAGE_V2 as *const _ as *const c_void,
        (n, 1) if n == kOfxProgressSuite => &PROGRESS_SUITE as *const _ as *const c_void,
        (n, 1) if n == kOfxTimeLineSuite => &TIMELINE_SUITE as *const _ as *const c_void,
        _ => std::ptr::null(),
    };
    if p.is_null() {
        log::debug!("OFX: host does not provide suite {name} v{version}");
    }
    p
}

fn host_props() -> *mut PropSet {
    let p = PropSet::new_ptr();
    let s = unsafe { &*p };
    s.set_str(kOfxPropType, 0, kOfxTypeImageEffectHost);
    s.set_str(kOfxPropName, 0, "net.kompo.app");
    s.set_str(kOfxPropLabel, 0, "Kompo");
    for (i, v) in [0, 1, 0].iter().enumerate() {
        s.set_int(kOfxPropVersion, i, *v);
    }
    s.set_str(kOfxPropVersionLabel, 0, "0.1.0");
    s.set_int(kOfxImageEffectHostPropIsBackground, 0, 0);
    s.set_int(kOfxImageEffectPropSupportsOverlays, 0, 0);
    s.set_int(kOfxImageEffectPropSupportsMultiResolution, 0, 0);
    s.set_int(kOfxImageEffectPropSupportsTiles, 0, 0);
    s.set_int(kOfxImageEffectPropTemporalClipAccess, 0, 0);
    s.set_str(kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
    s.set_str(kOfxImageEffectPropSupportedContexts, 0, kOfxImageEffectContextFilter);
    s.set_str(kOfxImageEffectPropSupportedContexts, 1, kOfxImageEffectContextGenerator);
    s.set_str(kOfxImageEffectPropSupportedContexts, 2, kOfxImageEffectContextGeneral);
    s.set_int(kOfxImageEffectPropSupportsMultipleClipDepths, 0, 0);
    s.set_int(kOfxImageEffectPropSupportsMultipleClipPARs, 0, 0);
    s.set_int(kOfxImageEffectPropSetableFrameRate, 0, 0);
    s.set_int(kOfxImageEffectPropSetableFielding, 0, 0);
    s.set_int(kOfxParamHostPropSupportsCustomInteract, 0, 0);
    s.set_int(kOfxParamHostPropSupportsStringAnimation, 0, 0);
    s.set_int(kOfxParamHostPropSupportsChoiceAnimation, 0, 0);
    s.set_int(kOfxParamHostPropSupportsStrChoiceAnimation, 0, 0);
    s.set_int(kOfxParamHostPropSupportsBooleanAnimation, 0, 0);
    for (i, d) in [kOfxBitDepthFloat, kOfxBitDepthShort, kOfxBitDepthByte].iter().enumerate() {
        s.set_str(kOfxImageEffectPropSupportedPixelDepths, i, d);
    }
    s.set_int(kOfxParamHostPropSupportsCustomAnimation, 0, 0);
    s.set_int(kOfxParamHostPropSupportsParametricAnimation, 0, 0);
    s.set_int(kOfxParamHostPropMaxParameters, 0, -1);
    s.set_int(kOfxParamHostPropMaxPages, 0, 0);
    s.set_int(kOfxParamHostPropPageRowColumnCount, 0, 0);
    s.set_int(kOfxParamHostPropPageRowColumnCount, 1, 0);
    s.set_str(kOfxImageEffectHostPropNativeOrigin, 0, kOfxHostNativeOriginBottomLeft);
    p
}

pub fn host() -> &'static OfxHost {
    static H: std::sync::OnceLock<OfxHost> = std::sync::OnceLock::new();
    H.get_or_init(|| OfxHost { host: host_props() as Handle, fetch_suite })
}
