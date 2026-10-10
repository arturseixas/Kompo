//! Still images: PNG and JPEG via `image`, OpenEXR via `exr` (both pure Rust).

use crate::MediaError;
use crate::frame::{FrameStorage, PixelLayout, SampleType, VideoFrame, next_serial};
use kompo_core::media::ColorDescription;
use kompo_core::time::Time;
use std::path::Path;

pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "exr"];

pub fn is_image_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).map(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false)
}

/// Image dimensions without decoding the pixels.
pub fn image_dimensions(path: &Path) -> Result<(u32, u32, u8, bool), MediaError> {
    if is_exr(path) {
        let meta = exr::meta::MetaData::read_from_file(path, false).map_err(|e| MediaError::Image(e.to_string()))?;
        let h = meta.headers.first().ok_or_else(|| MediaError::Image("empty EXR".into()))?;
        let size = h.layer_size;
        let alpha = h.channels.list.iter().any(|c| c.name.eq("A"));
        return Ok((size.width() as u32, size.height() as u32, 32, alpha));
    }
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let png = reader.format() == Some(image::ImageFormat::Png);
    let (w, h) = reader.into_dimensions().map_err(|e| MediaError::Image(e.to_string()))?;
    Ok((w, h, 8, png))
}

fn is_exr(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("exr")).unwrap_or(false)
}

/// Loads an image as a frame. PNG/JPEG are sRGB-encoded; EXR is linear float.
pub fn load_image(path: &Path) -> Result<VideoFrame, MediaError> {
    if is_exr(path) {
        return load_exr(path);
    }
    let img = image::ImageReader::open(path)?.with_guessed_format()?.decode().map_err(|e| MediaError::Image(e.to_string()))?;
    let (w, h) = (img.width(), img.height());
    let deep = matches!(img.color(), image::ColorType::Rgb16 | image::ColorType::Rgba16 | image::ColorType::L16 | image::ColorType::La16);
    let color = ColorDescription::SRGB;
    if deep {
        let buf = img.to_rgba16();
        let bytes: Vec<u8> = buf.as_raw().iter().flat_map(|v| v.to_le_bytes()).collect();
        Ok(VideoFrame {
            width: w,
            height: h,
            layout: PixelLayout::Rgba,
            sample: SampleType::U16,
            bits: 16,
            shift: 0,
            color,
            pts: Time::ZERO,
            duration: Time::ZERO,
            strides: vec![w as usize * 8],
            storage: FrameStorage::Owned(vec![bytes]),
            serial: next_serial(),
        })
    } else {
        Ok(VideoFrame::from_rgba8(w, h, img.to_rgba8().into_raw(), color))
    }
}

fn load_exr(path: &Path) -> Result<VideoFrame, MediaError> {
    use exr::prelude::*;
    let image = read_first_rgba_layer_from_file(
        path,
        |resolution, _| {
            let (w, h) = (resolution.width(), resolution.height());
            (w, vec![0f32; w * h * 4])
        },
        |(w, buf), pos, (r, g, b, a): (f32, f32, f32, f32)| {
            let i = (pos.y() * *w + pos.x()) * 4;
            buf[i] = r;
            buf[i + 1] = g;
            buf[i + 2] = b;
            buf[i + 3] = a;
        },
    )
    .map_err(|e| MediaError::Image(e.to_string()))?;
    let size = image.layer_data.size;
    let (_, data) = image.layer_data.channel_data.pixels;
    Ok(VideoFrame::from_rgba_f32(size.width() as u32, size.height() as u32, &data, ColorDescription::LINEAR_REC709))
}

/// Writes an RGBA f32 linear buffer as OpenEXR (half float).
pub fn write_exr(path: &Path, width: u32, height: u32, rgba: &[f32]) -> Result<(), MediaError> {
    use exr::prelude::*;
    let w = width as usize;
    let get = |pos: Vec2<usize>| {
        let i = (pos.y() * w + pos.x()) * 4;
        (f16::from_f32(rgba[i]), f16::from_f32(rgba[i + 1]), f16::from_f32(rgba[i + 2]), f16::from_f32(rgba[i + 3]))
    };
    let layer = Layer::new(
        (width as usize, height as usize),
        LayerAttributes::named("rgba"),
        Encoding::SMALL_LOSSLESS,
        SpecificChannels::rgba(get),
    );
    Image::from_layer(layer).write().to_file(path).map_err(|e| MediaError::Image(e.to_string()))
}

/// Writes 8-bit RGBA (sRGB encoded) as PNG or JPEG depending on the extension.
pub fn write_rgba8(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), MediaError> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or_else(|| MediaError::Image("bad buffer".into()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    if ext == "jpg" || ext == "jpeg" {
        image::DynamicImage::ImageRgba8(img).to_rgb8().save(path).map_err(|e| MediaError::Image(e.to_string()))
    } else {
        img.save(path).map_err(|e| MediaError::Image(e.to_string()))
    }
}

/// Writes 16-bit RGBA PNG.
pub fn write_rgba16_png(path: &Path, width: u32, height: u32, rgba: &[u16]) -> Result<(), MediaError> {
    let img: image::ImageBuffer<image::Rgba<u16>, Vec<u16>> =
        image::ImageBuffer::from_raw(width, height, rgba.to_vec()).ok_or_else(|| MediaError::Image("bad buffer".into()))?;
    img.save(path).map_err(|e| MediaError::Image(e.to_string()))
}
