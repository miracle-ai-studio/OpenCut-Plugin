//! Native raster adapters for sampled graphics, text and mask geometry.
mod graphics;
mod masks;
mod text;
use crate::{Raster, Result, budget, invalid};
pub use graphics::graphic;
pub use masks::{SceneMaskRaster, scene_mask};
pub use text::TextRasterizer;
use tiny_skia::{Paint, Path, PathBuilder, Pixmap};

#[derive(Clone, Copy)]
pub struct RasterLimits {
    pub max_pixels: u64,
    pub max_bytes: u64,
}
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub peak_bytes: u64,
}
impl RasterImage {
    pub fn as_raster(&self) -> Raster<'_> {
        Raster {
            width: self.width,
            height: self.height,
            rgba: &self.rgba,
        }
    }
}
fn admit(width: u32, height: u32, bytes_per_pixel: u64, limits: RasterLimits) -> Result<u64> {
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(budget("Native raster extent is outside admission"));
    }
    let pixels = u64::from(width) * u64::from(height);
    let bytes = pixels
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| budget("Native raster byte overflow"))?;
    if pixels > limits.max_pixels || bytes > limits.max_bytes {
        return Err(budget("Native raster exceeds admission"));
    }
    Ok(bytes)
}
fn pixmap(width: u32, height: u32) -> Result<Pixmap> {
    Pixmap::new(width, height).ok_or_else(|| budget("Native raster allocation failed"))
}
fn finish(image: Pixmap, peak_bytes: u64) -> RasterImage {
    let (width, height) = (image.width(), image.height());
    let mut rgba = image.take();
    for p in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(p[3]);
        for channel in &mut p[..3] {
            *channel = if alpha == 0 {
                0
            } else {
                ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8
            };
        }
    }
    RasterImage {
        width,
        height,
        rgba,
        peak_bytes,
    }
}
fn paint(color: [f64; 4]) -> Result<Paint<'static>> {
    if color
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err(invalid("Invalid native raster color"));
    }
    let mut paint = Paint::default();
    paint.set_color_rgba8(
        (color[0] * 255.0).round() as u8,
        (color[1] * 255.0).round() as u8,
        (color[2] * 255.0).round() as u8,
        (color[3] * 255.0).round() as u8,
    );
    paint.anti_alias = true;
    Ok(paint)
}
fn rounded_rect(x: f64, y: f64, width: f64, height: f64, radius: f64) -> Result<Path> {
    if [x, y, width, height, radius].iter().any(|v| !v.is_finite())
        || width <= 0.0
        || height <= 0.0
        || radius < 0.0
    {
        return Err(invalid("Invalid native rounded rectangle"));
    }
    let r = radius.min(width / 2.0).min(height / 2.0);
    let k = 0.5522847498307936;
    let mut p = PathBuilder::new();
    p.move_to((x + r) as f32, y as f32);
    p.line_to((x + width - r) as f32, y as f32);
    p.cubic_to(
        (x + width - r + k * r) as f32,
        y as f32,
        (x + width) as f32,
        (y + r - k * r) as f32,
        (x + width) as f32,
        (y + r) as f32,
    );
    p.line_to((x + width) as f32, (y + height - r) as f32);
    p.cubic_to(
        (x + width) as f32,
        (y + height - r + k * r) as f32,
        (x + width - r + k * r) as f32,
        (y + height) as f32,
        (x + width - r) as f32,
        (y + height) as f32,
    );
    p.line_to((x + r) as f32, (y + height) as f32);
    p.cubic_to(
        (x + r - k * r) as f32,
        (y + height) as f32,
        x as f32,
        (y + height - r + k * r) as f32,
        x as f32,
        (y + height - r) as f32,
    );
    p.line_to(x as f32, (y + r) as f32);
    p.cubic_to(
        x as f32,
        (y + r - k * r) as f32,
        (x + r - k * r) as f32,
        y as f32,
        (x + r) as f32,
        y as f32,
    );
    p.close();
    p.finish()
        .ok_or_else(|| invalid("Native rectangle path is degenerate"))
}
