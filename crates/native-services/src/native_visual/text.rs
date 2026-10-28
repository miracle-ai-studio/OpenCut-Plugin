use super::{media::callback, plan::Plan, *};
use crate::classic_visual::VisualProcessSession;
use opencut_contracts::{
    caption_frame::{CaptionLayoutReceipt, CaptionRasterPlan, CaptionWrapMeasurement},
    scene_frame::SceneFontSynthesisOwner,
    text_frame::*,
};
use opencut_editor_application::{
    classic_text_frame::ClassicTextFontHandle, classic_visual::ClassicVisualResources,
};
use opencut_renderer::raster::{RasterImage, RasterLimits, TextRasterizer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub(super) struct TextSources {
    pub images: BTreeMap<String, Arc<RasterImage>>,
    pub rasters: Vec<TextRasterReceipt>,
    pub captions: Vec<CaptionLayoutReceipt>,
}
pub(super) fn raster_error(error: opencut_renderer::Error) -> EditorError {
    EditorError::new(error.code, error.message)
}
fn resource(font: &ValidatedTextFont) -> TextFontResource {
    TextFontResource {
        font_id: font.font_id.clone(),
        content_hash: font.content_hash.clone(),
        size: font.size.clone(),
        weight: font.weight,
        style: font.style,
        face_index: font.face_index,
    }
}

pub(super) fn authorize_fonts<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
    session: &mut S,
    resources: &mut R,
    plan: &Plan,
    handles: &[ClassicTextFontHandle],
    rasterizer: &mut TextRasterizer,
) -> Result<(), EditorError> {
    let mut fonts = BTreeMap::new();
    for font in plan
        .text()
        .into_iter()
        .flat_map(|p| p.texts.iter().map(|t| &t.font))
        .chain(
            plan.scene()
                .into_iter()
                .flat_map(|p| p.mask_fonts.iter().map(|m| &m.font)),
        )
    {
        if let Some(previous) = fonts.insert(font.font_id.clone(), font) {
            if resource(previous) != resource(font) {
                return Err(invalid("Native frame changed a shared font identity"));
            }
        }
    }
    let active: BTreeSet<_> = fonts.values().map(|f| f.content_hash.clone()).collect();
    rasterizer.retain(&active);
    for font in fonts.values() {
        resources.checkpoint()?;
        let handle = handles
            .iter()
            .find(|h| h.font_id == font.font_id)
            .ok_or_else(|| invalid("Native frame font handle is missing"))?;
        let resolved = callback(
            session,
            resources,
            "resolveVerifiedFontBytes",
            json!({"font":resource(font),"handle":handle.handle}),
        )?;
        let transfer = resolved
            .get("transferId")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Native font transfer is missing"))?;
        let length = resolved
            .get("byteLength")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Native font byte count is missing"))?;
        let result = (|| {
            if length != font.size {
                return Err(invalid("Native font length differs from its fixed proof"));
            }
            if rasterizer.contains(font) {
                return Ok(());
            }
            let size = number(length)?;
            if size == 0 || size > opencut_editor_application::font_import::MAX_FONT_BYTES {
                return Err(invalid("Native font exceeds static face admission"));
            }
            let mut bytes = Vec::with_capacity(size as usize);
            while (bytes.len() as u64) < size {
                resources.checkpoint()?;
                let offset = bytes.len() as u64;
                let count = (size - offset).min(65536) as u32;
                let (chunk, eof) = resources.read(transfer, offset, count)?;
                if chunk.len() != count as usize || eof != (offset + u64::from(count) == size) {
                    return Err(invalid("Native font returned a short verified range"));
                }
                bytes.extend_from_slice(&chunk);
            }
            rasterizer.load(font, bytes.into()).map_err(raster_error)
        })();
        let closed = resources.release(transfer);
        result?;
        closed?;
    }
    Ok(())
}

pub(super) fn prepare_text(
    plan: &Plan,
    rasterizer: &mut TextRasterizer,
    limits: RasterLimits,
    checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
) -> Result<TextSources, EditorError> {
    let mut result = TextSources {
        images: BTreeMap::new(),
        rasters: Vec::new(),
        captions: Vec::new(),
    };
    let Some(text) = plan.text() else {
        return Ok(result);
    };
    let mut retained = 0u64;
    for original in &text.texts {
        checkpoint()?;
        let mut layer = original.clone();
        if let Some(directive) = plan
            .caption()
            .and_then(|p| p.captions.iter().find(|c| c.element_id == layer.element_id))
        {
            let receipt = caption(
                &mut layer,
                directive,
                text.identity.width,
                text.identity.height,
                rasterizer,
            )?;
            result.captions.push(receipt);
        }
        let synthesis = plan
            .scene()
            .and_then(|p| {
                p.font_synthesis.iter().find(|s| {
                    s.owner_kind == SceneFontSynthesisOwner::Text
                        && s.element_id == layer.element_id
                        && s.owner_id == layer.element_id
                        && s.font_id == layer.font.font_id
                })
            })
            .map(|s| &s.binding);
        let available = RasterLimits {
            max_pixels: limits.max_pixels,
            max_bytes: limits
                .max_bytes
                .checked_sub(retained)
                .ok_or_else(|| invalid("Native text exceeded frame admission"))?,
        };
        let (image, receipt) = rasterizer
            .rasterize(
                &layer,
                text.identity.width,
                text.identity.height,
                synthesis,
                available,
            )
            .map_err(raster_error)?;
        retained += image.rgba.len() as u64;
        result
            .images
            .insert(layer.element_id.clone(), Arc::new(image));
        result.rasters.push(receipt);
    }
    Ok(result)
}

fn caption(
    layer: &mut TextLayerPlan,
    directive: &CaptionRasterPlan,
    width: u32,
    height: u32,
    rasterizer: &mut TextRasterizer,
) -> Result<CaptionLayoutReceipt, EditorError> {
    if directive.wrap_profile != "classic-v31-greedy-whitespace-v1"
        || directive.max_width_px <= 0.0
        || directive.max_width_px > f64::from(width)
        || directive.bottom_margin_px != f64::from(height) * 0.05
    {
        return Err(invalid(
            "Native caption directive differs from the fixed canvas",
        ));
    }
    let original_hash = format!("{:x}", Sha256::digest(layer.text.content.as_bytes()));
    if original_hash != layer.font.validated_text_hash {
        return Err(invalid(
            "Native caption differs from its original glyph proof",
        ));
    }
    let mut measurements = Vec::new();
    let mut measure = |content: &str| -> Result<f64, EditorError> {
        let value = rasterizer
            .measure_width(&layer.font, &layer.text, content)
            .map_err(raster_error)?;
        measurements.push(CaptionWrapMeasurement {
            content: content.into(),
            width: value,
        });
        Ok(value)
    };
    let mut paragraphs = Vec::new();
    for paragraph in layer.text.content.trim().replace("\r\n", "\n").split('\n') {
        let mut words = paragraph.split_whitespace();
        let Some(first) = words.next() else {
            paragraphs.push(String::new());
            continue;
        };
        let mut current = first.to_owned();
        let mut lines = Vec::new();
        for word in words {
            let candidate = format!("{current} {word}");
            if measure(&candidate)? <= directive.max_width_px {
                current = candidate
            } else {
                lines.push(current);
                current = word.into();
            }
        }
        lines.push(current);
        paragraphs.push(lines.join("\n"));
    }
    let wrapped = paragraphs.join("\n");
    let mut maximum = 0f64;
    for line in wrapped.split('\n') {
        maximum = maximum.max(measure(line)?);
    }
    drop(measure);
    let block_height = wrapped.split('\n').count() as f64 * layer.text.line_height_px;
    let left = match layer.text.align.as_str() {
        "left" => 0.0,
        "right" => -maximum,
        _ => -maximum / 2.0,
    };
    let bg = &layer.text.background;
    let (left, top, box_width, box_height) = if bg.enabled {
        (
            left - bg.padding_x_px + bg.offset_x_px,
            -block_height / 2.0 - bg.padding_y_px + bg.offset_y_px,
            maximum + 2.0 * bg.padding_x_px,
            block_height + 2.0 * bg.padding_y_px,
        )
    } else {
        (left, -block_height / 2.0, maximum, block_height)
    };
    layer.text.transform.center_x = match layer.text.align.as_str() {
        "left" => -left,
        "right" => f64::from(width) - (left + box_width),
        _ => f64::from(width) / 2.0 - (left + box_width / 2.0),
    };
    layer.text.transform.center_y =
        f64::from(height) - directive.bottom_margin_px - (top + box_height);
    layer.text.content = wrapped.clone();
    layer.font.validated_text_hash = format!("{:x}", Sha256::digest(wrapped.as_bytes()));
    Ok(CaptionLayoutReceipt {
        element_id: layer.element_id.clone(),
        original_text_hash: original_hash,
        wrapped_text: wrapped,
        measurements,
    })
}
