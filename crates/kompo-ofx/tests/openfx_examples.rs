//! Runs the open-source OpenFX example plug-ins through the Kompo host.
//!
//! Build them first with `scripts/build-ofx-examples.sh` (or point `KOMPO_OFX_TEST_DIR` at a
//! folder of `.ofx.bundle`s). Without plug-ins the tests print a notice and pass.

use kompo_ofx::{OfxInstance, scan};
use std::path::PathBuf;

fn dir() -> Option<PathBuf> {
    let d = std::env::var_os("KOMPO_OFX_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ofx-examples"));
    if d.is_dir() {
        Some(d)
    } else {
        eprintln!("OFX example plug-ins not built ({}); run scripts/build-ofx-examples.sh", d.display());
        None
    }
}

const W: usize = 48;
const H: usize = 32;

fn image() -> Vec<f32> {
    let mut v = Vec::new();
    for y in 0..H {
        for x in 0..W {
            v.extend_from_slice(&[0.1 + 0.8 * x as f32 / W as f32, 0.2, 0.1 + 0.6 * y as f32 / H as f32, 1.0]);
        }
    }
    v
}

fn px(img: &[f32], x: usize, y: usize) -> [f32; 4] {
    let i = (y * W + x) * 4;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.02
}

#[test]
fn every_example_loads_and_most_render() {
    let Some(d) = dir() else { return };
    let found = scan(&[d]);
    assert!(!found.is_empty(), "no plug-ins found");
    let mut rendered = 0;
    for p in &found {
        if p.unsupported.is_some() {
            eprintln!("skipped {}: {}", p.id, p.unsupported.clone().unwrap_or_default());
            continue;
        }
        let generator = p.context.as_deref() == Some("OfxImageEffectContextGenerator");
        let src = image();
        let mut inst = OfxInstance::new(&p.id).unwrap_or_else(|e| panic!("{}: {e}", p.id));
        let out = inst
            .render(W as u32, H as u32, if generator { None } else { Some(&src) }, 0.0, 25.0)
            .unwrap_or_else(|e| panic!("{}: {e}", p.id));
        assert_eq!(out.len(), W * H * 4);
        assert!(out.iter().all(|v| v.is_finite()), "{}: finite output", p.id);
        rendered += 1;
    }
    assert!(rendered >= 6, "at least six example plug-ins render ({rendered})");
}

#[test]
fn gain_scales_rgb() {
    let Some(d) = dir() else { return };
    scan(&[d]);
    let mut inst = OfxInstance::new("uk.co.thefoundry.BasicGainPlugin").expect("gain plug-in");
    inst.set_value("scale", &[0.5]);
    let src = image();
    let out = inst.render(W as u32, H as u32, Some(&src), 0.0, 25.0).unwrap();
    let (a, b) = (px(&src, 20, 10), px(&out, 20, 10));
    assert!(close(b[0], a[0] * 0.5) && close(b[2], a[2] * 0.5), "gain 0.5: {a:?} -> {b:?}");
}

#[test]
fn invert_is_an_involution() {
    let Some(d) = dir() else { return };
    scan(&[d]);
    let mut inst = OfxInstance::new("uk.co.thefoundry.OfxInvertExample").expect("invert plug-in");
    let src = image();
    let once = inst.render(W as u32, H as u32, Some(&src), 0.0, 25.0).unwrap();
    assert!(close(px(&once, 5, 5)[0], 1.0 - px(&src, 5, 5)[0]), "inverted: {:?} vs {:?}", px(&once, 5, 5), px(&src, 5, 5));
    let twice = inst.render(W as u32, H as u32, Some(&once), 0.0, 25.0).unwrap();
    assert!(close(px(&twice, 30, 20)[1], px(&src, 30, 20)[1]) && close(px(&twice, 30, 20)[0], px(&src, 30, 20)[0]), "round trip");
}

#[test]
fn rectangle_draws_with_its_colour_parameter() {
    let Some(d) = dir() else { return };
    scan(&[d]);
    let mut inst = OfxInstance::new("uk.co.thefoundry.GeneratorExample").expect("rectangle plug-in");
    // corners are in canonical (pixel) coordinates; fill a centered box with pure green
    inst.set_value("corner1", &[W as f64 * 0.25, H as f64 * 0.25]);
    inst.set_value("corner2", &[W as f64 * 0.75, H as f64 * 0.75]);
    inst.set_value("colour", &[0.0, 1.0, 0.0, 1.0]);
    let src = image();
    let out = inst.render(W as u32, H as u32, Some(&src), 0.0, 25.0).unwrap();
    let inside = px(&out, W / 2, H / 2);
    assert!(inside[1] > 0.8 && inside[0] < 0.3, "green inside the box: {inside:?}");
    let outside = px(&out, 2, 2);
    assert!(close(outside[0], px(&src, 2, 2)[0]), "outside is the source: {outside:?}");
}

#[test]
fn choice_params_and_noise_generator() {
    let Some(d) = dir() else { return };
    let found = scan(&[d]);
    let mut inst = OfxInstance::new("org.openfx.ChoiceParamPlugin").expect("choice plug-in");
    let ci = found.iter().find(|p| p.id == "org.openfx.ChoiceParamPlugin").unwrap();
    assert!(ci.params.iter().any(|p| p.name == "red_choice" && p.choices.len() >= 2), "choices exposed to the UI: {:?}", ci.params);
    inst.set_value("red_choice", &[1.0]);
    let src = image();
    let a = inst.render(W as u32, H as u32, Some(&src), 0.0, 25.0).unwrap();
    inst.set_value("red_choice", &[0.0]);
    let b = inst.render(W as u32, H as u32, Some(&src), 0.0, 25.0).unwrap();
    assert!(a != b, "a different choice produces a different image");
    let mut noise = OfxInstance::new("net.sf.openfx.noisePlugin").expect("noise plug-in");
    let n = noise.render(W as u32, H as u32, None, 0.0, 25.0).unwrap();
    let first = px(&n, 0, 0);
    assert!(n.chunks(4).any(|p| (p[0] - first[0]).abs() > 0.05), "the generator makes varying pixels");
}

#[test]
fn normalised_defaults_become_canonical_coordinates() {
    let Some(d) = dir() else { return };
    scan(&[d]);
    // the Rectangle plug-in's defaults are 0.4..0.6 of the frame, normalized
    let mut inst = OfxInstance::new("uk.co.thefoundry.GeneratorExample").expect("rectangle plug-in");
    inst.set_value("colour", &[1.0, 0.0, 1.0, 1.0]);
    let src = image();
    let out = inst.render(W as u32, H as u32, Some(&src), 0.0, 25.0).unwrap();
    let c = px(&out, W / 2, H / 2);
    assert!(c[0] > 0.8 && c[2] > 0.8 && c[1] < 0.3, "default centered box is magenta: {c:?}");
    assert!(close(px(&out, 2, 2)[0], px(&src, 2, 2)[0]), "outside untouched");
}
