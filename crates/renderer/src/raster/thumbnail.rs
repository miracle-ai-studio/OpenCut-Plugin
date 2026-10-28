//! CPU source-picture decode/transform/JPEG. The caller provides the already
//! prepared output size; no project timing or thumbnail sizing policy is here.
use super::*;
use skia_safe::{self as skia, EncodedOrigin};
use std::io::{self, Write};
pub const SOURCE_THUMBNAIL_RASTERIZER: &str = "skia-safe-0.153.3-srgb-jpeg80";

#[derive(Clone, Copy)]
pub struct SourceThumbnailLimits {
    pub max_pixels: u64,
    pub max_bytes: u64,
    /// Other buffers owned by the source operation, including encoded range
    /// transport and codec reservations. Never omitted from peak admission.
    pub retained_bytes: u64,
    pub max_jpeg_bytes: usize,
}
pub struct SourceThumbnailImage {
    image: skia::Image,
    width: u32,
    height: u32,
    origin: EncodedOrigin,
    admitted: u64,
}
fn dimensions(width: u32, height: u32, limits: SourceThumbnailLimits) -> Result<u64> {
    if width == 0
        || height == 0
        || width > i32::MAX as u32
        || height > i32::MAX as u32
        || u64::from(width) * u64::from(height) > limits.max_pixels
        || limits.max_bytes == 0
        || limits.max_bytes > 256 * 1024 * 1024
        || limits.max_jpeg_bytes == 0
        || limits.max_jpeg_bytes > 1024 * 1024
    {
        return Err(budget("Native source image dimensions exceed admission"));
    }
    // Codec, pixel owner, Data copy and decode/transform workspace share the
    // retained source's 16-byte-per-pixel guard. Bound both JPEG buffers too.
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(16))
        .and_then(|n| n.checked_add(limits.retained_bytes))
        .and_then(|n| n.checked_add(limits.max_jpeg_bytes as u64 * 2))
        .ok_or_else(|| budget("Native source picture memory overflow"))?;
    if bytes > limits.max_bytes {
        return Err(budget("Native source picture exceeds memory admission"));
    }
    Ok(bytes)
}
fn pixels(bytes: usize) -> Result<Vec<u8>> {
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(bytes)
        .map_err(|_| budget("Cannot allocate admitted source pixels"))?;
    pixels.resize(bytes, 0);
    Ok(pixels)
}
impl SourceThumbnailImage {
    pub fn encoded(
        encoded: &[u8],
        limits: SourceThumbnailLimits,
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        checkpoint()?;
        if encoded.is_empty()
            || (encoded.len() as u64)
                .checked_mul(3)
                .is_none_or(|bytes| bytes > limits.retained_bytes || bytes > limits.max_bytes)
        {
            return Err(budget("Encoded source image exceeds captured admission"));
        }
        let mut codec = skia::Codec::from_data(skia::Data::new_copy(encoded)).ok_or_else(|| {
            crate::Error::new(
                "UNSUPPORTED_SOURCE_ART_IMAGE",
                "Native source image has no supported bounded header",
            )
        })?;
        // Preserve the reviewed bounded-image format set. Accepting an extra
        // platform codec is not an implicit expansion of the source contract.
        if !matches!(
            codec.encoded_format(),
            skia::EncodedImageFormat::PNG
                | skia::EncodedImageFormat::JPEG
                | skia::EncodedImageFormat::GIF
                | skia::EncodedImageFormat::BMP
                | skia::EncodedImageFormat::WEBP
        ) {
            return Err(crate::Error::new(
                "UNSUPPORTED_SOURCE_ART_IMAGE",
                "Native source image format is unsupported",
            ));
        }
        let info = codec.info();
        let width = u32::try_from(info.width())
            .map_err(|_| invalid("Invalid native source image width"))?;
        let height = u32::try_from(info.height())
            .map_err(|_| invalid("Invalid native source image height"))?;
        let admitted = dimensions(width, height, limits)?;
        let origin = codec.origin();
        // The previous image header/natural-dimension comparison rejected a
        // nonsquare 90-degree EXIF image. Preserve that explicit boundary.
        if origin.swaps_width_height() && width != height {
            return Err(invalid(
                "Oriented source image dimensions differ from the captured header",
            ));
        }
        let rgba_info = skia::ImageInfo::new(
            (width as i32, height as i32),
            skia::ColorType::RGBA8888,
            skia::AlphaType::Premul,
            skia::ColorSpace::new_srgb(),
        );
        let row_bytes = width as usize * 4;
        let mut rgba = pixels(
            row_bytes
                .checked_mul(height as usize)
                .ok_or_else(|| budget("Source image rows overflow"))?,
        )?;
        let options = skia::codec::Options {
            max_decode_memory: Some(
                usize::try_from(u64::from(width) * u64::from(height) * 8)
                    .map_err(|_| budget("Source image codec admission overflow"))?,
            ),
            ..Default::default()
        };
        if codec.get_pixels_with_options(&rgba_info, &mut rgba, row_bytes, Some(&options))
            != skia::codec::Result::Success
        {
            return Err(invalid("Native source image pixels could not be decoded"));
        }
        checkpoint()?;
        let image =
            skia::images::raster_from_data(&rgba_info, skia::Data::new_copy(&rgba), row_bytes)
                .ok_or_else(|| budget("Cannot retain admitted source image pixels"))?;
        Ok(Self {
            image,
            width,
            height,
            origin,
            admitted,
        })
    }
    pub fn rgba(
        width: u32,
        height: u32,
        rgba: &[u8],
        rotation_degrees: u16,
        limits: SourceThumbnailLimits,
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        checkpoint()?;
        let admitted = dimensions(width, height, limits)?;
        let expected = u64::from(width) * u64::from(height) * 4;
        if expected != rgba.len() as u64 {
            return Err(invalid("Native source frame changed its RGBA extent"));
        }
        let origin = match rotation_degrees {
            0 => EncodedOrigin::TopLeft,
            90 => EncodedOrigin::RightTop,
            180 => EncodedOrigin::BottomRight,
            270 => EncodedOrigin::LeftBottom,
            _ => return Err(invalid("Native source frame has unsupported rotation")),
        };
        let info = skia::ImageInfo::new(
            (width as i32, height as i32),
            skia::ColorType::RGBA8888,
            skia::AlphaType::Unpremul,
            skia::ColorSpace::new_srgb(),
        );
        let image =
            skia::images::raster_from_data(&info, skia::Data::new_copy(rgba), width as usize * 4)
                .ok_or_else(|| budget("Cannot retain admitted source frame pixels"))?;
        checkpoint()?;
        Ok(Self {
            image,
            width,
            height,
            origin,
            admitted,
        })
    }
    pub fn source_size(&self) -> (u32, u32) {
        if self.origin.swaps_width_height() {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
    pub fn jpeg(
        &self,
        target_width: u32,
        target_height: u32,
        limits: SourceThumbnailLimits,
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        checkpoint()?;
        if target_width == 0
            || target_height == 0
            || target_width > 1280
            || target_height > 720
            || self.admitted != dimensions(self.width, self.height, limits)?
            || self
                .admitted
                .checked_add(u64::from(target_width) * u64::from(target_height) * 4)
                .is_none_or(|n| n > limits.max_bytes)
        {
            return Err(budget("Prepared native source thumbnail exceeds admission"));
        }
        let info = skia::ImageInfo::new(
            (target_width as i32, target_height as i32),
            skia::ColorType::RGBA8888,
            skia::AlphaType::Opaque,
            skia::ColorSpace::new_srgb(),
        );
        let row_bytes = target_width as usize * 4;
        let mut output = pixels(row_bytes * target_height as usize)?;
        {
            let mut surface = skia::surfaces::wrap_pixels(&info, &mut output, row_bytes, None)
                .ok_or_else(|| budget("Cannot allocate admitted thumbnail canvas"))?;
            let canvas = surface.canvas();
            canvas.clear(skia::Color::BLACK);
            let (width, height) = self.source_size();
            canvas.scale((
                target_width as f32 / width as f32,
                target_height as f32 / height as f32,
            ));
            // Use Skia's encoded-origin matrix with the oriented destination
            // extent, matching its clockwise EXIF transform definitions.
            canvas.concat(&self.origin.to_matrix((width as i32, height as i32)));
            canvas.draw_image_rect_with_sampling_options(
                &self.image,
                None,
                skia::Rect::from_wh(self.width as f32, self.height as f32),
                skia::SamplingOptions::new(skia::FilterMode::Linear, skia::MipmapMode::None),
                &skia::Paint::default(),
            );
        }
        checkpoint()?;
        let pixmap = skia::Pixmap::new(&info, &mut output, row_bytes)
            .ok_or_else(|| invalid("Native thumbnail pixels changed their layout"))?;
        let mut encoded = BoundedJpeg {
            bytes: Vec::new(),
            limit: limits.max_jpeg_bytes,
            exceeded: false,
        };
        if !skia::jpeg_encoder::encode(
            &pixmap,
            &mut encoded,
            &skia::jpeg_encoder::Options {
                quality: 80,
                ..Default::default()
            },
        ) {
            return Err(if encoded.exceeded {
                budget("Native source JPEG exceeds output admission")
            } else {
                invalid("Native source JPEG encoding failed")
            });
        }
        checkpoint()?;
        if encoded.bytes.is_empty() {
            return Err(invalid("Native source JPEG is empty"));
        }
        Ok(encoded.bytes)
    }
}
struct BoundedJpeg {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl Write for BoundedJpeg {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > self.limit)
        {
            self.exceeded = true;
            return Err(io::Error::other("Admitted source JPEG is full"));
        }
        self.bytes
            .try_reserve_exact(bytes.len())
            .map_err(|_| io::Error::other("Cannot allocate admitted JPEG"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "thumbnail_tests.rs"]
mod tests;
