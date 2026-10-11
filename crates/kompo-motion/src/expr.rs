//! A small After Effects-flavored expression language.
//!
//! ```text
//! wiggle(2, 30)                         // random motion
//! [value[0] + Math.sin(time * 3) * 50, value[1]]
//! loopOut("pingpong")                   // repeat the keyframes
//! linear(time, 0, 2, 0, 100)            // map a time range to a value range
//! thisComp.layer("Ball").transform.position + [100, 0]
//! var a = time * 2; a > 3 ? 100 : a * 30
//! ```
//!
//! Values are numbers or vectors; arithmetic broadcasts a number over a
//! vector. Layer property references read the other layer's *evaluated*
//! property through [`ExprEnv::property_at`].

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Num(f64),
    Vec(Vec<f64>),
    Str(String),
    Layer(String),
    /// A property path being built (`transform` of a layer, ...).
    Path(String, String),
}

impl V {
    fn vec(&self) -> Result<Vec<f64>, String> {
        match self {
            V::Num(n) => Ok(vec![*n]),
            V::Vec(v) => Ok(v.clone()),
            other => Err(format!("expected a number or vector, got {other:?}")),
        }
    }
    fn num(&self) -> Result<f64, String> {
        match self {
            V::Num(n) => Ok(*n),
            V::Vec(v) if !v.is_empty() => Ok(v[0]),
            other => Err(format!("expected a number, got {other:?}")),
        }
    }
}

/// What an expression can see.
pub trait ExprEnv {
    /// Current time in seconds (layer-local).
    fn time(&self) -> f64;
    /// The property's keyframed value at the current time.
    fn value(&self) -> Vec<f64>;
    /// The property's keyframed value at another layer time.
    fn value_at(&self, t: f64) -> Vec<f64>;
    /// First and last keyframe times (None without keyframes).
    fn key_range(&self) -> Option<(f64, f64)>;
    fn index(&self) -> f64 {
        1.0
    }
    fn comp_size(&self) -> [f64; 2] {
        [1920.0, 1080.0]
    }
    fn layer_name(&self) -> String;
    /// Evaluated property `prop` (dotted, e.g. `transform.position`) of layer `layer` at comp time `t`.
    fn property_at(&self, layer: &str, prop: &str, t: f64) -> Option<Vec<f64>>;
    /// Seed for noise/random (stable per property).
    fn seed(&self) -> u64;
}

// ---- lexer -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Id(String),
    Op(&'static str),
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    const OPS: [&str; 23] =
        ["&&", "||", "==", "!=", "<=", ">=", "!", "+", "-", "*", "/", "%", "(", ")", "[", "]", ",", ".", "?", ":", "<", ">", ";"];
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch.is_ascii_digit() || (ch == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let s = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                i += 1;
            }
            let t: String = c[s..i].iter().collect();
            out.push(Tok::Num(t.parse().map_err(|_| format!("bad number {t}"))?));
        } else if ch == '"' || ch == '\'' {
            let q = ch;
            i += 1;
            let s = i;
            while i < c.len() && c[i] != q {
                i += 1;
            }
            if i >= c.len() {
                return Err("unterminated string".into());
            }
            out.push(Tok::Str(c[s..i].iter().collect()));
            i += 1;
        } else if ch.is_alphabetic() || ch == '_' || ch == '$' {
            let s = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '$') {
                i += 1;
            }
            out.push(Tok::Id(c[s..i].iter().collect()));
        } else {
            let rest: String = c[i..(i + 2).min(c.len())].iter().collect();
            let op = OPS
                .iter()
                .find(|o| rest.starts_with(**o))
                .or(if ch == '=' { Some(&"=") } else { None })
                .ok_or_else(|| format!("unexpected '{ch}'"))?;
            out.push(Tok::Op(op));
            i += op.len();
        }
    }
    Ok(out)
}

// ---- parser ----------------------------------------------------------------

#[derive(Clone, Debug)]
enum E {
    Num(f64),
    Str(String),
    Arr(Vec<E>),
    Var(String),
    Neg(Box<E>),
    Not(Box<E>),
    Bin(&'static str, Box<E>, Box<E>),
    Cond(Box<E>, Box<E>, Box<E>),
    Call(Box<E>, Vec<E>),
    Index(Box<E>, Box<E>),
    Member(Box<E>, String),
}

#[derive(Clone, Debug)]
enum Stmt {
    Let(String, E),
    Expr(E),
}

struct Parser {
    t: Vec<Tok>,
    p: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.p)
    }
    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Op(o)) if *o == op) {
            self.p += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, op: &str) -> Result<(), String> {
        if self.eat_op(op) { Ok(()) } else { Err(format!("expected '{op}'")) }
    }

    fn program(&mut self) -> Result<Vec<Stmt>, String> {
        let mut out = Vec::new();
        while self.peek().is_some() {
            if self.eat_op(";") {
                continue;
            }
            if matches!(self.peek(), Some(Tok::Id(k)) if k == "var" || k == "let" || k == "const") {
                self.p += 1;
                let Some(Tok::Id(name)) = self.peek().cloned() else { return Err("expected a name after var".into()) };
                self.p += 1;
                self.expect("=")?;
                let e = self.expr()?;
                out.push(Stmt::Let(name, e));
            } else {
                out.push(Stmt::Expr(self.expr()?));
            }
        }
        Ok(out)
    }

    fn expr(&mut self) -> Result<E, String> {
        let c = self.binary(0)?;
        if self.eat_op("?") {
            let a = self.expr()?;
            self.expect(":")?;
            let b = self.expr()?;
            return Ok(E::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binary(&mut self, min: u8) -> Result<E, String> {
        const LEVELS: [&[&str]; 6] = [&["||"], &["&&"], &["==", "!="], &["<", ">", "<=", ">="], &["+", "-"], &["*", "/", "%"]];
        if min as usize >= LEVELS.len() {
            return self.unary();
        }
        let mut l = self.binary(min + 1)?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(o)) if LEVELS[min as usize].contains(o) => *o,
                _ => break,
            };
            self.p += 1;
            let r = self.binary(min + 1)?;
            l = E::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<E, String> {
        if self.eat_op("-") {
            return Ok(E::Neg(Box::new(self.unary()?)));
        }
        if self.eat_op("+") {
            return self.unary();
        }
        if self.eat_op("!") {
            return Ok(E::Not(Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<E, String> {
        let mut e = self.primary()?;
        loop {
            if self.eat_op("(") {
                let mut args = Vec::new();
                if !self.eat_op(")") {
                    loop {
                        args.push(self.expr()?);
                        if self.eat_op(")") {
                            break;
                        }
                        self.expect(",")?;
                    }
                }
                e = E::Call(Box::new(e), args);
            } else if self.eat_op("[") {
                let i = self.expr()?;
                self.expect("]")?;
                e = E::Index(Box::new(e), Box::new(i));
            } else if self.eat_op(".") {
                let Some(Tok::Id(n)) = self.peek().cloned() else { return Err("expected a name after '.'".into()) };
                self.p += 1;
                e = E::Member(Box::new(e), n);
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> Result<E, String> {
        match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.p += 1;
                Ok(E::Num(n))
            }
            Some(Tok::Str(s)) => {
                self.p += 1;
                Ok(E::Str(s))
            }
            Some(Tok::Id(n)) => {
                self.p += 1;
                Ok(E::Var(n))
            }
            Some(Tok::Op("(")) => {
                self.p += 1;
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Some(Tok::Op("[")) => {
                self.p += 1;
                let mut items = Vec::new();
                if !self.eat_op("]") {
                    loop {
                        items.push(self.expr()?);
                        if self.eat_op("]") {
                            break;
                        }
                        self.expect(",")?;
                    }
                }
                Ok(E::Arr(items))
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
}

// ---- evaluation --------------------------------------------------------------

fn hash(seed: u64, k: i64) -> f64 {
    let mut h = seed ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    (h >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}

/// Smooth 1D value noise in -1..1.
pub fn noise(seed: u64, x: f64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let u = f * f * (3.0 - 2.0 * f);
    hash(seed, i as i64) * (1.0 - u) + hash(seed, i as i64 + 1) * u
}

/// Three-octave noise like After Effects' wiggle.
fn fbm(seed: u64, x: f64) -> f64 {
    (noise(seed, x) + 0.5 * noise(seed ^ 0x55, x * 2.0) + 0.25 * noise(seed ^ 0xAA, x * 4.0)) / 1.75
}

fn broadcast(a: &[f64], b: &[f64], f: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    let n = a.len().max(b.len());
    (0..n)
        .map(|i| {
            let x = if a.len() == 1 { a[0] } else { a.get(i).copied().unwrap_or(0.0) };
            let y = if b.len() == 1 { b[0] } else { b.get(i).copied().unwrap_or(0.0) };
            f(x, y)
        })
        .collect()
}

fn out(v: Vec<f64>) -> V {
    if v.len() == 1 { V::Num(v[0]) } else { V::Vec(v) }
}

fn ease_curve(t: f64, kind: &str) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match kind {
        "easeIn" => t * t * t,
        "easeOut" => 1.0 - (1.0 - t).powi(3),
        _ => t * t * (3.0 - 2.0 * t),
    }
}

struct Interp<'a> {
    env: &'a dyn ExprEnv,
    vars: HashMap<String, V>,
    steps: usize,
}

impl Interp<'_> {
    fn tick(&mut self) -> Result<(), String> {
        self.steps += 1;
        if self.steps > 20_000 { Err("expression too complex".into()) } else { Ok(()) }
    }

    fn eval(&mut self, e: &E) -> Result<V, String> {
        self.tick()?;
        match e {
            E::Num(n) => Ok(V::Num(*n)),
            E::Str(s) => Ok(V::Str(s.clone())),
            E::Arr(items) => {
                let mut v = Vec::new();
                for i in items {
                    v.extend(self.eval(i)?.vec()?);
                }
                Ok(V::Vec(v))
            }
            E::Var(n) => self.var(n),
            E::Neg(x) => Ok(out(self.eval(x)?.vec()?.iter().map(|v| -v).collect())),
            E::Not(x) => Ok(V::Num((self.eval(x)?.num()? == 0.0) as i32 as f64)),
            E::Bin(op, a, b) => {
                let (a, b) = (self.eval(a)?, self.eval(b)?);
                if matches!(*op, "&&" | "||") {
                    let (x, y) = (a.num()? != 0.0, b.num()? != 0.0);
                    return Ok(V::Num(if *op == "&&" { x && y } else { x || y } as i32 as f64));
                }
                if let (V::Str(x), V::Str(y)) = (&a, &b) {
                    return match *op {
                        "==" => Ok(V::Num((x == y) as i32 as f64)),
                        "!=" => Ok(V::Num((x != y) as i32 as f64)),
                        "+" => Ok(V::Str(format!("{x}{y}"))),
                        _ => Err("bad string operator".into()),
                    };
                }
                let (x, y) = (self.numeric(a)?, self.numeric(b)?);
                let f: fn(f64, f64) -> f64 = match *op {
                    "+" => |a, b| a + b,
                    "-" => |a, b| a - b,
                    "*" => |a, b| a * b,
                    "/" => |a, b| if b == 0.0 { 0.0 } else { a / b },
                    "%" => |a, b| if b == 0.0 { 0.0 } else { a.rem_euclid(b) },
                    "<" => |a, b| (a < b) as i32 as f64,
                    ">" => |a, b| (a > b) as i32 as f64,
                    "<=" => |a, b| (a <= b) as i32 as f64,
                    ">=" => |a, b| (a >= b) as i32 as f64,
                    "==" => |a, b| ((a - b).abs() < 1e-12) as i32 as f64,
                    "!=" => |a, b| ((a - b).abs() >= 1e-12) as i32 as f64,
                    _ => return Err(format!("bad operator {op}")),
                };
                Ok(out(broadcast(&x, &y, f)))
            }
            E::Cond(c, a, b) => {
                if self.eval(c)?.num()? != 0.0 {
                    self.eval(a)
                } else {
                    self.eval(b)
                }
            }
            E::Index(a, i) => {
                let v = self.numeric_v(a)?;
                let i = self.eval(i)?.num()? as usize;
                v.get(i).copied().map(V::Num).ok_or_else(|| format!("index {i} out of range"))
            }
            E::Member(base, name) => self.member(base, name),
            E::Call(f, args) => self.call(f, args),
        }
    }

    /// Resolves property paths to values.
    fn numeric(&mut self, v: V) -> Result<Vec<f64>, String> {
        match v {
            V::Path(layer, prop) => {
                self.env.property_at(&layer, &prop, self.env.time()).ok_or_else(|| format!("no property '{prop}' on layer '{layer}'"))
            }
            other => other.vec(),
        }
    }

    fn numeric_v(&mut self, e: &E) -> Result<Vec<f64>, String> {
        let v = self.eval(e)?;
        self.numeric(v)
    }

    fn var(&mut self, n: &str) -> Result<V, String> {
        if let Some(v) = self.vars.get(n) {
            return Ok(v.clone());
        }
        Ok(match n {
            "time" => V::Num(self.env.time()),
            "value" => out(self.env.value()),
            "index" => V::Num(self.env.index()),
            "PI" => V::Num(std::f64::consts::PI),
            "thisLayer" => V::Layer(self.env.layer_name()),
            "thisComp" | "Math" => V::Path(String::new(), n.to_string()),
            "width" => V::Num(self.env.comp_size()[0]),
            "height" => V::Num(self.env.comp_size()[1]),
            "transform" | "position" | "scale" | "rotation" | "opacity" | "anchorPoint" => V::Path(self.env.layer_name(), canonical(n)),
            _ => V::Path(String::new(), n.to_string()),
        })
    }

    fn member(&mut self, base: &E, name: &str) -> Result<V, String> {
        let b = self.eval(base)?;
        match b {
            V::Path(layer, path) if path == "Math" => match name {
                "PI" => Ok(V::Num(std::f64::consts::PI)),
                "E" => Ok(V::Num(std::f64::consts::E)),
                _ => Ok(V::Path(layer, format!("Math.{name}"))),
            },
            V::Layer(l) => Ok(V::Path(l, canonical(name))),
            V::Path(layer, path) => {
                let joined = if path.is_empty() { canonical(name) } else { format!("{path}.{}", canonical(name)) };
                Ok(V::Path(layer, joined))
            }
            other => {
                // `.length` of a vector
                if name == "length" {
                    return Ok(V::Num(other.vec()?.len() as f64));
                }
                Err(format!("cannot read .{name}"))
            }
        }
    }

    fn call(&mut self, f: &E, args: &[E]) -> Result<V, String> {
        // method call on a path/layer: `thisComp.layer("x")`, `.valueAtTime(t)`, `Math.sin(x)`
        let (target, name) = match f {
            E::Member(base, n) => (Some(self.eval(base)?), n.as_str()),
            E::Var(n) => (None, n.as_str()),
            _ => return Err("not callable".into()),
        };
        let mut a: Vec<V> = Vec::new();
        for x in args {
            a.push(self.eval(x)?);
        }
        if let Some(t) = target {
            return match (&t, name) {
                (V::Path(_, p), "layer") if p == "thisComp" => match a.first() {
                    Some(V::Str(n)) => Ok(V::Layer(n.clone())),
                    Some(V::Num(i)) => Ok(V::Layer(format!("#{}", *i as usize))),
                    _ => Err("layer() takes a name".into()),
                },
                (V::Path(_, p), m) if p == "Math" => self.func(m, a),
                (V::Path(layer, prop), "valueAtTime") => {
                    let at = a.first().ok_or("valueAtTime needs a time")?.num()?;
                    self.env.property_at(layer, prop, at).map(out).ok_or_else(|| format!("no property '{prop}'"))
                }
                (V::Layer(_), "valueAtTime") => Err("valueAtTime needs a property".into()),
                _ => Err(format!("unknown method {name}")),
            };
        }
        self.func(name, a)
    }

    fn func(&mut self, name: &str, a: Vec<V>) -> Result<V, String> {
        let n = |i: usize| -> Result<f64, String> { a.get(i).ok_or_else(|| format!("{name}: missing argument {i}"))?.num() };
        let unary = |f: fn(f64) -> f64| -> Result<V, String> { Ok(V::Num(f(n(0)?))) };
        match name {
            "sin" => unary(f64::sin),
            "cos" => unary(f64::cos),
            "tan" => unary(f64::tan),
            "asin" => unary(f64::asin),
            "acos" => unary(f64::acos),
            "atan" => unary(f64::atan),
            "atan2" => Ok(V::Num(n(0)?.atan2(n(1)?))),
            "abs" => unary(f64::abs),
            "sqrt" => unary(f64::sqrt),
            "exp" => unary(f64::exp),
            "log" => unary(f64::ln),
            "floor" => unary(f64::floor),
            "ceil" => unary(f64::ceil),
            "round" => unary(f64::round),
            "sign" => unary(f64::signum),
            "pow" => Ok(V::Num(n(0)?.powf(n(1)?))),
            "min" => Ok(V::Num(n(0)?.min(n(1)?))),
            "max" => Ok(V::Num(n(0)?.max(n(1)?))),
            "degreesToRadians" | "radians" => unary(f64::to_radians),
            "radiansToDegrees" | "degrees" => unary(f64::to_degrees),
            "clamp" => {
                let v = self.numeric(a[0].clone())?;
                let (lo, hi) = (n(1)?, n(2)?);
                Ok(out(v.iter().map(|x| x.clamp(lo, hi)).collect()))
            }
            "length" => {
                let x = self.numeric(a.first().cloned().ok_or("length needs an argument")?)?;
                let y = match a.get(1) {
                    Some(v) => self.numeric(v.clone())?,
                    None => vec![0.0; x.len()],
                };
                Ok(V::Num(broadcast(&x, &y, |p, q| (p - q) * (p - q)).iter().sum::<f64>().sqrt()))
            }
            "add" | "sub" | "mul" | "div" => {
                let (x, y) = (self.numeric(a[0].clone())?, self.numeric(a[1].clone())?);
                let f: fn(f64, f64) -> f64 = match name {
                    "add" => |p, q| p + q,
                    "sub" => |p, q| p - q,
                    "mul" => |p, q| p * q,
                    _ => |p, q| if q == 0.0 { 0.0 } else { p / q },
                };
                Ok(out(broadcast(&x, &y, f)))
            }
            "linear" | "ease" | "easeIn" | "easeOut" => {
                let (t, t0, t1) = (n(0)?, n(1)?, n(2)?);
                let k = if (t1 - t0).abs() < 1e-12 { 1.0 } else { ((t - t0) / (t1 - t0)).clamp(0.0, 1.0) };
                let s = if name == "linear" { k } else { ease_curve(k, name) };
                let (v0, v1) = match a.len() {
                    5 => (self.numeric(a[3].clone())?, self.numeric(a[4].clone())?),
                    _ => (vec![0.0], vec![1.0]),
                };
                Ok(out(broadcast(&v0, &v1, |p, q| p + (q - p) * s)))
            }
            "random" => {
                let t = self.env.time();
                let r = hash(self.env.seed(), (t * 1000.0) as i64) * 0.5 + 0.5;
                match a.len() {
                    0 => Ok(V::Num(r)),
                    1 => Ok(V::Num(r * n(0)?)),
                    _ => Ok(V::Num(n(0)? + r * (n(1)? - n(0)?))),
                }
            }
            "noise" => Ok(V::Num(fbm(self.env.seed(), n(0)?))),
            "wiggle" => {
                let (freq, amp) = (n(0)?, n(1)?);
                let t = self.env.time();
                let base = self.env.value();
                Ok(out(base
                    .iter()
                    .enumerate()
                    .map(|(i, v)| v + amp * fbm(self.env.seed().wrapping_add(i as u64 * 7919), t * freq))
                    .collect()))
            }
            "loopOut" | "loopIn" => {
                let mode = match a.first() {
                    Some(V::Str(s)) => s.clone(),
                    _ => "cycle".into(),
                };
                let Some((t0, t1)) = self.env.key_range() else { return Ok(out(self.env.value())) };
                let t = self.env.time();
                let period = (t1 - t0).max(1e-9);
                let before = name == "loopIn" && t < t0;
                let after = name == "loopOut" && t > t1;
                if !(before || after) {
                    return Ok(out(self.env.value()));
                }
                let phase = if after { (t - t0) / period } else { (t1 - t) / period };
                let wrapped = match mode.as_str() {
                    "pingpong" => {
                        let k = phase.rem_euclid(2.0);
                        if k <= 1.0 { k } else { 2.0 - k }
                    }
                    "offset" | "continue" => phase.rem_euclid(1.0),
                    _ => phase.rem_euclid(1.0),
                };
                let tt = if after { t0 + wrapped * period } else { t1 - wrapped * period };
                let v = self.env.value_at(tt);
                if mode == "offset" {
                    let (a0, a1) = (self.env.value_at(t0), self.env.value_at(t1));
                    let cycles = phase.floor();
                    let dir = if after { 1.0 } else { -1.0 };
                    return Ok(out(v.iter().zip(a0.iter().zip(&a1)).map(|(x, (p, q))| x + (q - p) * cycles * dir).collect()));
                }
                Ok(out(v))
            }
            "valueAtTime" => {
                let t = n(0)?;
                Ok(out(self.env.value_at(t)))
            }
            "seedRandom" => Ok(V::Num(0.0)),
            _ => Err(format!("unknown function '{name}'")),
        }
    }
}

fn canonical(name: &str) -> String {
    match name {
        "anchorPoint" => "anchor".into(),
        other => other.to_string(),
    }
}

/// Evaluates `src`. The result is a vector (1 element for numbers).
pub fn eval(src: &str, env: &dyn ExprEnv) -> Result<Vec<f64>, String> {
    let toks = lex(src)?;
    let prog = Parser { t: toks, p: 0 }.program()?;
    let mut it = Interp { env, vars: HashMap::new(), steps: 0 };
    let mut last = None;
    for s in &prog {
        match s {
            Stmt::Let(n, e) => {
                let v = it.eval(e)?;
                let v = match v {
                    V::Path(..) => out(it.numeric(v)?),
                    other => other,
                };
                it.vars.insert(n.clone(), v);
            }
            Stmt::Expr(e) => last = Some(it.eval(e)?),
        }
    }
    let v = last.ok_or("empty expression")?;
    it.numeric(v)
}

/// Checks that an expression parses (for the editor).
pub fn check(src: &str) -> Result<(), String> {
    Parser { t: lex(src)?, p: 0 }.program().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Env {
        time: f64,
        value: Vec<f64>,
    }

    impl ExprEnv for Env {
        fn time(&self) -> f64 {
            self.time
        }
        fn value(&self) -> Vec<f64> {
            self.value.clone()
        }
        fn value_at(&self, t: f64) -> Vec<f64> {
            self.value.iter().map(|v| v + t * 10.0).collect()
        }
        fn key_range(&self) -> Option<(f64, f64)> {
            Some((0.0, 2.0))
        }
        fn layer_name(&self) -> String {
            "self".into()
        }
        fn property_at(&self, layer: &str, prop: &str, t: f64) -> Option<Vec<f64>> {
            (layer == "Ball" && prop == "transform.position").then(|| vec![100.0 + t, 50.0])
        }
        fn seed(&self) -> u64 {
            42
        }
    }

    fn ev(src: &str, time: f64, value: &[f64]) -> Vec<f64> {
        eval(src, &Env { time, value: value.to_vec() }).unwrap_or_else(|e| panic!("{src}: {e}"))
    }

    #[test]
    fn arithmetic_vectors_and_conditions() {
        assert_eq!(ev("1 + 2 * 3", 0.0, &[0.0]), vec![7.0]);
        assert_eq!(ev("[value[0] + 10, value[1] * 2]", 0.0, &[1.0, 2.0]), vec![11.0, 4.0]);
        assert_eq!(ev("value + 5", 0.0, &[1.0, 2.0]), vec![6.0, 7.0]);
        assert_eq!(ev("time > 1 ? 100 : 0", 2.0, &[0.0]), vec![100.0]);
        assert_eq!(ev("var a = time * 2; a + 1", 3.0, &[0.0]), vec![7.0]);
        assert!((ev("Math.sin(PI / 2)", 0.0, &[0.0])[0] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn interpolation_helpers() {
        assert_eq!(ev("linear(time, 0, 2, 0, 100)", 1.0, &[0.0]), vec![50.0]);
        assert_eq!(ev("linear(time, 0, 2, [0,0], [10,20])", 1.0, &[0.0]), vec![5.0, 10.0]);
        let e = ev("ease(time, 0, 1, 0, 100)", 0.5, &[0.0])[0];
        assert!((e - 50.0).abs() < 1e-9);
        assert_eq!(ev("clamp(value, 0, 10)", 0.0, &[-5.0]), vec![0.0]);
    }

    #[test]
    fn wiggle_is_deterministic_and_bounded() {
        let a = ev("wiggle(2, 30)", 1.234, &[100.0, 100.0]);
        let b = ev("wiggle(2, 30)", 1.234, &[100.0, 100.0]);
        assert_eq!(a, b);
        assert!(a.iter().all(|v| (v - 100.0).abs() <= 30.0));
        assert!(ev("wiggle(2, 30)", 1.234, &[100.0, 100.0]) != ev("wiggle(2, 30)", 1.5, &[100.0, 100.0]));
    }

    #[test]
    fn loops_repeat_keyframes() {
        // keys span 0..2 s; at 3 s "cycle" shows the value at 1 s
        assert_eq!(ev("loopOut(\"cycle\")", 3.0, &[0.0]), vec![10.0]);
        // ping-pong at 3 s mirrors to 1 s as well; at 2.5 s it is at 1.5 s
        assert_eq!(ev("loopOut(\"pingpong\")", 2.5, &[0.0]), vec![15.0]);
        assert_eq!(ev("loopOut()", 1.0, &[7.0]), vec![7.0], "inside the keyframes nothing changes");
    }

    #[test]
    fn layer_references() {
        assert_eq!(ev("thisComp.layer(\"Ball\").transform.position + [10, 0]", 2.0, &[0.0, 0.0]), vec![112.0, 50.0]);
        assert_eq!(ev("thisComp.layer(\"Ball\").transform.position.valueAtTime(0)", 2.0, &[0.0]), vec![100.0, 50.0]);
    }

    #[test]
    fn errors_are_reported() {
        assert!(eval("1 +", &Env { time: 0.0, value: vec![0.0] }).is_err());
        assert!(eval("nope(1)", &Env { time: 0.0, value: vec![0.0] }).is_err());
        assert!(check("[1, 2").is_err());
        assert!(check("wiggle(1, 2)").is_ok());
    }
}
