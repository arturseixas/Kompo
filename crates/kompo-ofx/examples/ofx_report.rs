//! Loads every OFX plug-in under the given folders (or the standard ones), describes it and
//! renders a test ramp through it, printing a pass/fail line per plug-in.
//!
//! `cargo run -p kompo-ofx --example ofx_report -- /path/to/plugins`

use kompo_ofx::{OfxInstance, ParamType};

fn ramp(w: usize, h: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let a = 1.0;
            v.extend_from_slice(&[x as f32 / (w - 1) as f32 * a, y as f32 / (h - 1) as f32 * a, 0.25 * a, a]);
        }
    }
    v
}

fn main() {
    let dirs: Vec<std::path::PathBuf> = std::env::args().skip(1).map(Into::into).collect();
    let plugins = kompo_ofx::scan(&dirs);
    let (mut pass, mut fail) = (0, 0);
    for p in &plugins {
        print!("{:<46} {:<28} v{}.{} ", p.id, p.label, p.version.0, p.version.1);
        if let Some(why) = &p.unsupported {
            println!("SKIP  {why}");
            fail += 1;
            continue;
        }
        let params: Vec<String> =
            p.params.iter().filter(|q| !matches!(q.ty, ParamType::Group | ParamType::Page)).map(|q| q.name.clone()).collect();
        let generator = p.context.as_deref() == Some("OfxImageEffectContextGenerator");
        let result = OfxInstance::new(&p.id).and_then(|mut inst| {
            let src = ramp(64, 32);
            let out = inst.render(64, 32, if generator { None } else { Some(&src) }, 0.0, 25.0)?;
            let changed = out.iter().zip(&src).any(|(a, b)| (a - b).abs() > 1e-4);
            Ok((out.iter().all(|v| v.is_finite()), changed))
        });
        match result {
            Ok((finite, changed)) => {
                println!(
                    "PASS  context={} params=[{}] finite={finite} modifies_image={changed}",
                    p.context.as_deref().unwrap_or("?").trim_start_matches("OfxImageEffectContext"),
                    params.join(",")
                );
                pass += 1;
            }
            Err(e) => {
                println!("FAIL  {e}");
                fail += 1;
            }
        }
    }
    println!("\n{pass} rendered, {fail} not run or failed, {} total", plugins.len());
}
