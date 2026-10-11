//! LUT file formats: Adobe/Resolve `.cube` (1D and 3D), Autodesk `.3dl`,
//! Sony/Imageworks `.spi1d` and `.spi3d`.

use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum LutKind {
    /// One curve per channel, `size` entries each.
    Curve1D,
    /// `size^3` lattice, red varying fastest.
    Cube3D,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lut {
    pub title: String,
    pub kind: LutKind,
    pub size: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub data: Vec<[f32; 3]>,
}

impl Lut {
    /// Looks up `rgb` (trilinear for 3D, linear per channel for 1D).
    pub fn eval(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mut t = [0.0f32; 3];
        for k in 0..3 {
            let span = (self.domain_max[k] - self.domain_min[k]).max(1e-12);
            t[k] = ((rgb[k] - self.domain_min[k]) / span).clamp(0.0, 1.0);
        }
        let n = self.size;
        match self.kind {
            LutKind::Curve1D => {
                let mut out = [0.0; 3];
                for k in 0..3 {
                    let p = t[k] * (n - 1) as f32;
                    let i = (p.floor() as usize).min(n - 2);
                    let f = p - i as f32;
                    out[k] = self.data[i][k] * (1.0 - f) + self.data[i + 1][k] * f;
                }
                out
            }
            LutKind::Cube3D => {
                let mut i0 = [0usize; 3];
                let mut fr = [0.0f32; 3];
                for k in 0..3 {
                    let p = t[k] * (n - 1) as f32;
                    i0[k] = (p.floor() as usize).min(n - 2);
                    fr[k] = p - i0[k] as f32;
                }
                let at = |r: usize, g: usize, b: usize| self.data[(b * n + g) * n + r];
                let mut out = [0.0f32; 3];
                for dz in 0..2 {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let w = (if dx == 1 { fr[0] } else { 1.0 - fr[0] })
                                * (if dy == 1 { fr[1] } else { 1.0 - fr[1] })
                                * (if dz == 1 { fr[2] } else { 1.0 - fr[2] });
                            let v = at(i0[0] + dx, i0[1] + dy, i0[2] + dz);
                            for k in 0..3 {
                                out[k] += w * v[k];
                            }
                        }
                    }
                }
                out
            }
        }
    }
}

fn floats(line: &str) -> Option<Vec<f32>> {
    line.split_whitespace().map(|t| t.parse::<f32>().ok()).collect()
}

pub fn parse_cube(text: &str) -> Result<Lut, String> {
    let mut title = String::new();
    let (mut size1, mut size3) = (0usize, 0usize);
    let (mut dmin, mut dmax) = ([0.0f32; 3], [1.0f32; 3]);
    let mut data = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();
        match key {
            "TITLE" => title = rest.trim_matches('"').to_string(),
            "LUT_1D_SIZE" => size1 = rest.parse().map_err(|_| "bad LUT_1D_SIZE")?,
            "LUT_3D_SIZE" => size3 = rest.parse().map_err(|_| "bad LUT_3D_SIZE")?,
            "DOMAIN_MIN" => dmin = floats(rest).and_then(|v| v.try_into().ok()).ok_or("bad DOMAIN_MIN")?,
            "DOMAIN_MAX" => dmax = floats(rest).and_then(|v| v.try_into().ok()).ok_or("bad DOMAIN_MAX")?,
            k if k.starts_with("LUT_") || k.starts_with("DOMAIN_") => {}
            _ => {
                let v = floats(line).ok_or_else(|| format!("bad row: {line}"))?;
                if v.len() != 3 {
                    return Err(format!("expected 3 values, got: {line}"));
                }
                data.push([v[0], v[1], v[2]]);
            }
        }
    }
    let (kind, size) = match (size1, size3) {
        (n, 0) if n >= 2 => (LutKind::Curve1D, n),
        (0, n) if n >= 2 => (LutKind::Cube3D, n),
        _ => return Err("missing LUT_1D_SIZE / LUT_3D_SIZE".into()),
    };
    let expected = if kind == LutKind::Cube3D { size * size * size } else { size };
    if data.len() != expected {
        return Err(format!("expected {expected} entries, found {}", data.len()));
    }
    Ok(Lut { title, kind, size, domain_min: dmin, domain_max: dmax, data })
}

/// Autodesk/Lustre `.3dl`: optional input-mesh line, then rows with blue varying fastest.
pub fn parse_3dl(text: &str) -> Result<Lut, String> {
    let mut rows: Vec<Vec<f32>> = Vec::new();
    let mut mesh = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("3DMESH") || line.starts_with("Mesh") || line.starts_with("LUT") {
            continue;
        }
        let Some(v) = floats(line) else { continue };
        if v.len() == 3 {
            rows.push(v);
        } else if v.len() > 3 && mesh == 0 {
            mesh = v.len();
        }
    }
    let n = (rows.len() as f64).cbrt().round() as usize;
    if n < 2 || n * n * n != rows.len() {
        return Err(format!("{} rows is not a cube", rows.len()));
    }
    let max = rows.iter().flatten().cloned().fold(0.0f32, f32::max);
    let scale = if max <= 1.0 { 1.0 } else { ((max + 1.0).log2().ceil().exp2() - 1.0).max(1.0) };
    // file order: r slowest, b fastest -> our order: r fastest
    let mut data = vec![[0.0f32; 3]; n * n * n];
    for (i, r) in rows.iter().enumerate() {
        let (ri, gi, bi) = (i / (n * n), (i / n) % n, i % n);
        data[(bi * n + gi) * n + ri] = [r[0] / scale, r[1] / scale, r[2] / scale];
    }
    Ok(Lut { title: String::new(), kind: LutKind::Cube3D, size: n, domain_min: [0.0; 3], domain_max: [1.0; 3], data })
}

/// Imageworks `.spi3d`: header with `size x y z`, then `i j k r g b` rows.
pub fn parse_spi3d(text: &str) -> Result<Lut, String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if !lines.next().is_some_and(|l| l.starts_with("SPILUT")) {
        return Err("not an SPI LUT".into());
    }
    lines.next(); // "3 3"
    let size: Vec<usize> = lines.next().ok_or("missing size")?.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    let n = *size.first().ok_or("bad size")?;
    if size.len() != 3 || size.iter().any(|s| *s != n) {
        return Err("only cubic spi3d supported".into());
    }
    let mut data = vec![[0.0f32; 3]; n * n * n];
    for l in lines {
        let v = floats(l).ok_or("bad row")?;
        if v.len() != 6 {
            return Err("bad spi3d row".into());
        }
        let (r, g, b) = (v[0] as usize, v[1] as usize, v[2] as usize);
        if r >= n || g >= n || b >= n {
            return Err("index out of range".into());
        }
        data[(b * n + g) * n + r] = [v[3], v[4], v[5]];
    }
    Ok(Lut { title: String::new(), kind: LutKind::Cube3D, size: n, domain_min: [0.0; 3], domain_max: [1.0; 3], data })
}

/// Imageworks `.spi1d`: `From min max`, `Length n`, `Components c`, then `{ values }`.
pub fn parse_spi1d(text: &str) -> Result<Lut, String> {
    let (mut from, mut length, mut comps) = ((0.0f32, 1.0f32), 0usize, 1usize);
    let mut vals: Vec<Vec<f32>> = Vec::new();
    let mut in_body = false;
    for l in text.lines().map(str::trim) {
        if let Some(rest) = l.strip_prefix("From") {
            let v = floats(rest).ok_or("bad From")?;
            from = (*v.first().ok_or("bad From")?, *v.get(1).ok_or("bad From")?);
        } else if let Some(v) = l.strip_prefix("Length") {
            length = v.trim().parse().map_err(|_| "bad Length")?;
        } else if let Some(v) = l.strip_prefix("Components") {
            comps = v.trim().parse().map_err(|_| "bad Components")?;
        } else if l == "{" {
            in_body = true;
        } else if l == "}" {
            in_body = false;
        } else if in_body && let Some(v) = floats(l) {
            vals.push(v);
        }
    }
    if length < 2 || vals.len() != length {
        return Err("bad spi1d length".into());
    }
    let data = vals.iter().map(|v| if comps >= 3 { [v[0], v[1], v[2]] } else { [v[0]; 3] }).collect();
    Ok(Lut { title: String::new(), kind: LutKind::Curve1D, size: length, domain_min: [from.0; 3], domain_max: [from.1; 3], data })
}

pub fn parse(path: &Path, text: &str) -> Result<Lut, String> {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("cube") => parse_cube(text),
        Some("3dl") => parse_3dl(text),
        Some("spi3d") => parse_spi3d(text),
        Some("spi1d") => parse_spi1d(text),
        other => Err(format!("unsupported LUT format: {}", other.unwrap_or("none"))),
    }
}

pub fn load(path: &Path) -> Result<Lut, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_3d_identity_and_1d() {
        let mut s = String::from("TITLE \"id\"\nLUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    s += &format!("{r}.0 {g}.0 {b}.0\n");
                }
            }
        }
        let l = parse_cube(&s).unwrap();
        let o = l.eval([0.25, 0.5, 0.75]);
        assert!((o[0] - 0.25).abs() < 1e-6 && (o[2] - 0.75).abs() < 1e-6);
        let one = parse_cube("LUT_1D_SIZE 2\n0 0 0\n0.5 0.5 0.5\n").unwrap();
        assert!((one.eval([1.0, 0.5, 0.0])[0] - 0.5).abs() < 1e-6);
        assert!(parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err());
    }

    #[test]
    fn spi_and_3dl() {
        let spi = "SPILUT 1.0\n3 3\n2 2 2\n0 0 0 0 0 0\n0 0 1 0 0 1\n0 1 0 0 1 0\n0 1 1 0 1 1\n1 0 0 1 0 0\n1 0 1 1 0 1\n1 1 0 1 1 0\n1 1 1 1 1 1\n";
        let l = parse_spi3d(spi).unwrap();
        assert!((l.eval([1.0, 0.0, 0.0])[0] - 1.0).abs() < 1e-6);
        let mut t = String::new();
        for r in 0..2 {
            for g in 0..2 {
                for b in 0..2 {
                    t += &format!("{} {} {}\n", r * 1023, g * 1023, b * 1023);
                }
            }
        }
        let l = parse_3dl(&t).unwrap();
        assert!((l.eval([0.0, 0.0, 1.0])[2] - 1.0).abs() < 1e-4);
        let d1 = parse_spi1d("Version 1\nFrom 0.0 1.0\nLength 2\nComponents 1\n{\n0.0\n1.0\n}\n").unwrap();
        assert!((d1.eval([0.5; 3])[1] - 0.5).abs() < 1e-6);
    }
}
