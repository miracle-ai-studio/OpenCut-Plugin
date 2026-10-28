use super::*;
use opencut_contracts::{font_synthesis::FontSynthesisBinding, scene::FontStyle, text_frame::*};
use sha2::{Digest, Sha256};
use skia_safe::{self as skia, FontMgr, Shaper};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

struct Font {
    byte_len: usize,
    retained_bytes: u64,
    typeface: skia::Typeface,
    typo_ascent: f32,
    typo_descent: f32,
    weight: u16,
    style: FontStyle,
}
pub struct TextRasterizer {
    fonts: BTreeMap<String, Font>,
    max_font_bytes: u64,
    retained_bytes: u64,
}
struct Line {
    width: f64,
    ascent: f64,
    descent: f64,
    baseline: f32,
    font: skia::Font,
    glyphs: Vec<skia::GlyphId>,
    positions: Vec<skia::Point>,
}

impl TextRasterizer {
    pub fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }
    pub fn contains(&self, font: &ValidatedTextFont) -> bool {
        self.fonts.get(&font.content_hash).is_some_and(|entry| {
            entry.byte_len.to_string() == font.size
                && entry.weight == font.weight
                && entry.style == font.style
        })
    }
    pub fn new(max_font_bytes: u64) -> Self {
        Self {
            fonts: BTreeMap::new(),
            max_font_bytes,
            retained_bytes: 0,
        }
    }
    /// The caller checks current font authority even on a cache hit. This
    /// cache retains immutable face data, never grants or project snapshots.
    pub fn retain(&mut self, hashes: &BTreeSet<String>) {
        self.fonts.retain(|hash, font| {
            if hashes.contains(hash) {
                true
            } else {
                self.retained_bytes -= font.retained_bytes;
                false
            }
        });
    }
    pub fn load(&mut self, font: &ValidatedTextFont, bytes: Arc<[u8]>) -> Result<()> {
        if font.face_index != 0
            || !font.glyph_coverage_validated
            || ![400, 700].contains(&font.weight)
            || bytes.is_empty()
            || font.size != bytes.len().to_string()
            || format!("{:x}", Sha256::digest(&bytes)) != font.content_hash
        {
            return Err(invalid(
                "Native font bytes differ from their validated identity",
            ));
        }
        if let Some(existing) = self.fonts.get(&font.content_hash) {
            if existing.weight != font.weight || existing.style != font.style {
                return Err(invalid("Native font metadata changed"));
            }
            return Ok(());
        }
        // The owned SkData and platform font backend may each retain the
        // verified bytes. Their native caches are additionally bounded by RSS.
        let font_bytes = (bytes.len() as u64) * 2;
        let retained = self
            .retained_bytes
            .checked_add(font_bytes)
            .ok_or_else(|| budget("Native font cache overflow"))?;
        if retained > self.max_font_bytes || self.fonts.len() >= 32 {
            return Err(budget("Native fonts exceed admission"));
        }
        let face =
            ttf_parser::Face::parse(&bytes, 0).map_err(|_| invalid("Invalid native font face"))?;
        if face.weight().to_number() != font.weight
            || face.is_italic() != (font.style == FontStyle::Italic)
        {
            return Err(invalid("Native font style differs from real face metadata"));
        }
        let typo_ascent = face
            .tables()
            .os2
            .map_or(face.ascender(), |t| t.typographic_ascender());
        let typo_descent = face
            .tables()
            .os2
            .map_or(face.descender(), |t| t.typographic_descender());
        // Only decode these verified bytes. The manager is never used for font
        // lookup; shaping below has a single fixed face and an empty fallback
        // manager. No host font can enter an accepted glyph run.
        let typeface = FontMgr::new()
            .new_from_data(skia::Data::new_copy(&bytes), 0)
            .ok_or_else(|| invalid("Native font backend rejected the verified face"))?;
        self.fonts.insert(
            font.content_hash.clone(),
            Font {
                byte_len: bytes.len(),
                retained_bytes: font_bytes,
                typeface,
                typo_ascent: f32::from(typo_ascent),
                typo_descent: -f32::from(typo_descent),
                weight: font.weight,
                style: font.style,
            },
        );
        self.retained_bytes = retained;
        Ok(())
    }

    pub fn measure_width(
        &mut self,
        font: &ValidatedTextFont,
        text: &TextRasterInput,
        content: &str,
    ) -> Result<f64> {
        validate(text)?;
        let face = self
            .fonts
            .get_mut(&font.content_hash)
            .ok_or_else(|| invalid("Native font was not loaded"))?;
        Ok(shape(face, text, content, false)?.width)
    }

    /// Metrics and rendering share one shaping/layout implementation. This
    /// path allocates glyph/path work only, without a dummy raster or GPU.
    pub fn measure_layout(
        &mut self,
        layer: &TextLayerPlan,
        synthesis: Option<&FontSynthesisBinding>,
        max_bytes: u64,
    ) -> Result<TextLayout> {
        validate(&layer.text)?;
        if !layer.font.glyph_coverage_validated
            || format!("{:x}", Sha256::digest(layer.text.content.as_bytes()))
                != layer.font.validated_text_hash
        {
            return Err(invalid(
                "Native measurement differs from its glyph-coverage proof",
            ));
        }
        self.layout_lines(layer, synthesis, max_bytes)
            .map(|(_, layout)| layout)
    }
    fn layout_lines(
        &mut self,
        layer: &TextLayerPlan,
        synthesis: Option<&FontSynthesisBinding>,
        max_bytes: u64,
    ) -> Result<(Vec<Line>, TextLayout)> {
        let text = &layer.text;
        let shaping = (text.content.len() as u64 + 1)
            .checked_mul(8192)
            .ok_or_else(|| budget("Native shaping admission overflow"))?;
        if shaping > max_bytes {
            return Err(budget("Native text shaping exceeds admission"));
        }
        let italic = validate_synthesis(synthesis, &layer.font)?;
        let face = self
            .fonts
            .get_mut(&layer.font.content_hash)
            .ok_or_else(|| invalid("Native font was not loaded"))?;
        if face.weight != layer.font.weight || face.style != layer.font.style {
            return Err(invalid("Native text font metadata changed"));
        }
        let mut lines = Vec::new();
        for content in text.content.split('\n') {
            lines.push(shape(face, text, content, italic)?);
        }
        let block = TextBlockMetrics {
            max_width: lines.iter().map(|line| line.width).fold(0.0, f64::max),
            height: lines.len() as f64 * text.line_height_px,
            visual_center_offset: (lines.len() - 1) as f64 * text.line_height_px / 2.0,
        };
        let background = if text.background.enabled {
            let bg = &text.background;
            let left = aligned(&text.align, block.max_width);
            let width = block.max_width + 2.0 * bg.padding_x_px;
            let height = block.height + 2.0 * bg.padding_y_px;
            if width < 0.0 || height < 0.0 {
                return Err(invalid("Native text background has negative extent"));
            }
            Some(TextBackgroundRect {
                left: left - bg.padding_x_px + bg.offset_x_px,
                top: -block.height / 2.0 - bg.padding_y_px + bg.offset_y_px,
                width,
                height,
                radius: width.min(height) / 2.0 * bg.corner_radius_percent.clamp(0.0, 100.0)
                    / 100.0,
            })
        } else {
            None
        };
        let thickness = (text.font_size_px * 0.07).max(1.0);
        let metrics: Vec<_> = text
            .content
            .split('\n')
            .zip(&lines)
            .enumerate()
            .map(|(index, (content, line))| {
                let baseline_y = index as f64 * text.line_height_px - block.visual_center_offset;
                TextLineMetrics {
                    content: content.into(),
                    baseline_y,
                    width: line.width,
                    ascent: line.ascent,
                    descent: line.descent,
                    decoration: if text.decoration == "none" {
                        None
                    } else {
                        Some(TextRect {
                            left: aligned(&text.align, line.width),
                            top: if text.decoration == "underline" {
                                baseline_y + line.descent + thickness
                            } else {
                                baseline_y - (line.ascent - line.descent) * 0.35
                            },
                            width: line.width,
                            height: thickness,
                        })
                    },
                }
            })
            .collect();
        let layout = TextLayout {
            font_id: layer.font.font_id.clone(),
            font_content_hash: layer.font.content_hash.clone(),
            validated_text_hash: layer.font.validated_text_hash.clone(),
            font_weight: layer.font.weight,
            font_style: layer.font.style,
            face_index: layer.font.face_index,
            font_size_px: text.font_size_px,
            line_height_px: text.line_height_px,
            letter_spacing_px: text.letter_spacing_px,
            align: text.align.clone(),
            baseline: "middle".into(),
            block,
            background,
            lines: metrics,
            transform: text.transform.clone(),
        };
        Ok((lines, layout))
    }

    pub fn rasterize(
        &mut self,
        layer: &TextLayerPlan,
        width: u32,
        height: u32,
        synthesis: Option<&FontSynthesisBinding>,
        limits: RasterLimits,
    ) -> Result<(RasterImage, TextRasterReceipt)> {
        self.render_text(layer, width, height, synthesis, limits, None)
    }
    pub fn mask_stroke(
        &mut self,
        layer: &TextLayerPlan,
        body: &RasterImage,
        stroke: &opencut_contracts::scene::Stroke,
        synthesis: Option<&FontSynthesisBinding>,
        limits: RasterLimits,
    ) -> Result<RasterImage> {
        self.render_text(
            layer,
            body.width,
            body.height,
            synthesis,
            limits,
            Some((stroke, body)),
        )
        .map(|(image, _)| image)
    }
    fn render_text(
        &mut self,
        layer: &TextLayerPlan,
        width: u32,
        height: u32,
        synthesis: Option<&FontSynthesisBinding>,
        limits: RasterLimits,
        stroke: Option<(&opencut_contracts::scene::Stroke, &RasterImage)>,
    ) -> Result<(RasterImage, TextRasterReceipt)> {
        let text = &layer.text;
        validate(text)?;
        if !layer.font.glyph_coverage_validated
            || format!("{:x}", Sha256::digest(text.content.as_bytes()))
                != layer.font.validated_text_hash
        {
            return Err(invalid("Native text differs from its glyph-coverage proof"));
        }
        let raster = admit(width, height, 4, limits)?;
        // Account bounded shaping/path work before allocating a glyph buffer.
        let shaping = (text.content.len() as u64 + 1)
            .checked_mul(8192)
            .ok_or_else(|| budget("Native shaping admission overflow"))?;
        let peak = raster * if stroke.is_some() { 2 } else { 1 };
        let peak = peak
            .checked_add(shaping)
            .ok_or_else(|| budget("Native text admission overflow"))?;
        if peak > limits.max_bytes {
            return Err(budget("Native text shaping exceeds admission"));
        }
        let (lines, layout) = self.layout_lines(
            layer,
            synthesis,
            limits.max_bytes - raster * if stroke.is_some() { 2 } else { 1 },
        )?;
        let mut image = pixmap(width, height)?;
        let info = skia::ImageInfo::new(
            (width as i32, height as i32),
            skia::ColorType::RGBA8888,
            skia::AlphaType::Premul,
            skia::ColorSpace::new_srgb(),
        );
        {
            let mut surface =
                skia::surfaces::wrap_pixels(&info, image.data_mut(), width as usize * 4, None)
                    .ok_or_else(|| budget("Native text surface allocation failed"))?;
            let canvas = surface.canvas();
            let t = &text.transform;
            // Preserve the authored T * S * R order, including mirrored text.
            canvas.translate((t.center_x as f32, t.center_y as f32));
            canvas.scale((t.scale_x as f32, t.scale_y as f32));
            canvas.rotate(t.rotation_radians.to_degrees() as f32, None);
            if let Some(bg) = &layout.background {
                if bg.width > 0.0 && bg.height > 0.0 {
                    canvas.draw_round_rect(
                        skia::Rect::from_xywh(
                            bg.left as f32,
                            bg.top as f32,
                            bg.width as f32,
                            bg.height as f32,
                        ),
                        bg.radius as f32,
                        bg.radius as f32,
                        &text_paint(text.background.color)?,
                    );
                }
            }
            let mut fill = text_paint(text.color)?;
            if let Some((stroke, _)) = stroke {
                let c = &stroke.color;
                if !stroke.width_px.is_finite() || stroke.width_px < 0.0 {
                    return Err(invalid("Invalid native text stroke"));
                }
                fill = text_paint([c.red, c.green, c.blue, c.alpha])?;
                fill.set_style(skia::paint::Style::Stroke)
                    .set_stroke_width(stroke.width_px as f32)
                    .set_stroke_cap(skia::paint::Cap::Round)
                    .set_stroke_join(skia::paint::Join::Round);
            }
            for (line, metric) in lines.iter().zip(&layout.lines) {
                canvas.draw_glyphs_at(
                    &line.glyphs,
                    line.positions.as_slice(),
                    (
                        aligned(&text.align, line.width) as f32,
                        metric.baseline_y as f32 + line.baseline,
                    ),
                    &line.font,
                    &fill,
                );
                if let Some(rect) = &metric.decoration {
                    if stroke.is_none() {
                        canvas.draw_rect(
                            skia::Rect::from_xywh(
                                rect.left as f32,
                                rect.top as f32,
                                rect.width as f32,
                                rect.height as f32,
                            ),
                            &fill,
                        );
                    }
                }
            }
        }
        if let Some((stroke, body)) = stroke
            && stroke.align != opencut_contracts::scene::StrokeAlign::Center
        {
            if body.rgba.len() != image.data().len() {
                return Err(invalid("Native text stroke body changed extent"));
            }
            for (input, pixel) in body
                .rgba
                .chunks_exact(4)
                .zip(image.data_mut().chunks_exact_mut(4))
            {
                let alpha = if stroke.align == opencut_contracts::scene::StrokeAlign::Inside {
                    input[3]
                } else {
                    255 - input[3]
                };
                for channel in pixel {
                    *channel = ((u16::from(*channel) * u16::from(alpha) + 127) / 255) as u8;
                }
            }
        }
        Ok((
            finish(image, peak),
            TextRasterReceipt {
                element_id: layer.element_id.clone(),
                width,
                height,
                layout,
            },
        ))
    }
}

// Canvas uses the normalized OS/2 typographic ascent/descent for the
// middle baseline, rounded to 1/64 px. Raw hhea units are not an em box.
fn middle_baseline(face: &Font, font: &skia::Font) -> f32 {
    let (mut ascent, mut descent) = (face.typo_ascent, face.typo_descent);
    if ascent <= 0.0 || descent < 0.0 || ascent + descent <= 0.0 {
        let metrics = font.metrics().1;
        ascent = -metrics.ascent;
        descent = metrics.descent;
    }
    let size = font.size();
    let normalized = (ascent * size / (ascent + descent) * 64.0).round() / 64.0;
    normalized - (size * 64.0).round() / 128.0
}

fn shape(face: &mut Font, text: &TextRasterInput, content: &str, italic: bool) -> Result<Line> {
    let mut font = skia::Font::from_typeface_with_params(
        face.typeface.clone(),
        text.font_size_px as f32,
        1.0,
        if italic { -0.25 } else { 0.0 },
    );
    font.set_embedded_bitmaps(false)
        .set_subpixel(true)
        .set_linear_metrics(true)
        .set_edging(skia::font::Edging::SubpixelAntiAlias);
    let baseline = middle_baseline(face, &font);
    let mut output = GlyphRuns::default();
    if !content.is_empty() {
        let shaper = skia::shapers::hb::shape_then_wrap(Some(FontMgr::empty()))
            .ok_or_else(|| invalid("Native text shaper is unavailable"))?;
        for (range, level) in directional_words(content)? {
            let word = &content[range.clone()];
            let mut fonts = Shaper::new_trivial_font_run_iterator(&font, word.len());
            let mut bidi = skia::shapers::primitive::trivial_bidi_run_iterator(level, word.len());
            let mut script = Shaper::new_hb_icu_script_run_iterator(word);
            let mut language = Shaper::new_trivial_language_run_iterator("en", word.len());
            let disabled = if text.letter_spacing_px == 0.0 {
                &[][..]
            } else {
                &[*b"liga", *b"clig", *b"calt"][..]
            };
            let features: Vec<_> = disabled
                .iter()
                .map(|tag| skia::shaper::Feature {
                    tag: u32::from_be_bytes(*tag),
                    value: 0,
                    start: 0,
                    end: word.len(),
                })
                .collect();
            // Default Canvas shaping retains kerning within a word but does
            // not kern across its separately shaped space items.
            let previous_lines = output.lines;
            output.cluster_base = range.start as u32;
            shaper.shape_with_iterators_and_features(
                word,
                &mut fonts,
                &mut bidi,
                &mut script,
                &mut language,
                &features,
                f32::MAX,
                &mut output,
            );
            if output.lines - previous_lines > 1 {
                return Err(invalid("Native shaping unexpectedly wrapped a line"));
            }
        }
    }
    if output
        .font_ids
        .iter()
        .any(|id| *id != face.typeface.unique_id())
        || output.glyphs.contains(&0)
    {
        return Err(invalid("Native shaping changed the verified font or line"));
    }
    // Letter spacing applies to clusters, never to individual marks in a
    // grapheme. HarfBuzz's fixed-point advances remain authoritative.
    let mut spacing = 0.0f32;
    let mut previous = None;
    for (position, cluster) in output.positions.iter_mut().zip(&output.clusters) {
        if previous.is_some_and(|last| last != *cluster) {
            spacing += text.letter_spacing_px as f32;
        }
        position.x += spacing;
        previous = Some(*cluster);
    }
    if previous.is_some() {
        spacing += text.letter_spacing_px as f32;
    }
    let mut bounds = vec![skia::Rect::default(); output.glyphs.len()];
    font.get_bounds(&output.glyphs, &mut bounds, None);
    // Canvas reports precise outline bounds, not the integer raster-cache
    // bounds (which include antialias padding). Bitmap glyphs have no outline.
    for (glyph, bounds) in output.glyphs.iter().zip(&mut bounds) {
        if let Some(path) = font.get_path(*glyph) {
            *bounds = *path.bounds();
        }
    }
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for (rect, point) in bounds.iter().zip(&output.positions) {
        if !rect.is_empty() {
            top = top.min(rect.top + point.y);
            bottom = bottom.max(rect.bottom + point.y);
        }
    }
    Ok(Line {
        width: f64::from(output.advance + spacing),
        ascent: if top.is_finite() {
            f64::from(-top - baseline)
        } else {
            0.0
        },
        descent: if bottom.is_finite() {
            f64::from(bottom + baseline)
        } else {
            0.0
        },
        baseline,
        font,
        glyphs: output.glyphs,
        positions: output.positions,
    })
}

/// ICU supplies embedding levels on the whole line; word boundaries never
/// change paragraph direction. Reorder those runs using the resolved levels,
/// then reverse the word items within each RTL run.
fn directional_words(content: &str) -> Result<Vec<(std::ops::Range<usize>, u8)>> {
    use skia::shaper::RunIterator;
    let mut bidi = Shaper::new_icu_bidi_run_iterator(content, 0)
        .ok_or_else(|| invalid("Native text direction is invalid"))?;
    let mut runs = Vec::new();
    let mut start = 0;
    while !bidi.at_end() {
        bidi.consume();
        let end = bidi.end_of_current_run();
        if end <= start || end > content.len() || !content.is_char_boundary(end) {
            return Err(invalid("Native bidi boundary is invalid"));
        }
        runs.push((start..end, bidi.current_level()));
        start = end;
    }
    if start != content.len() {
        return Err(invalid("Native bidi run omitted text"));
    }
    if let Some(minimum) = runs
        .iter()
        .map(|r| r.1)
        .filter(|level| level % 2 == 1)
        .min()
    {
        let maximum = runs.iter().map(|r| r.1).max().unwrap();
        for level in (minimum..=maximum).rev() {
            let mut index = 0;
            while index < runs.len() {
                if runs[index].1 < level {
                    index += 1;
                    continue;
                }
                let start = index;
                while index < runs.len() && runs[index].1 >= level {
                    index += 1;
                }
                runs[start..index].reverse();
            }
        }
    }
    let mut result = Vec::new();
    for (run, level) in runs {
        let insertion = result.len();
        let mut start = run.start;
        for (offset, ch) in content[run.clone()].char_indices() {
            if matches!(ch, ' ' | '\t' | '\u{200b}') {
                let at = run.start + offset;
                if at > start {
                    result.push((start..at, level));
                }
                start = at + ch.len_utf8();
                result.push((at..start, level));
            }
        }
        if start < run.end {
            result.push((start..run.end, level));
        }
        if level % 2 == 1 {
            result[insertion..].reverse();
        }
    }
    Ok(result)
}

#[derive(Default)]
struct GlyphRuns {
    glyphs: Vec<skia::GlyphId>,
    positions: Vec<skia::Point>,
    offsets: Vec<skia::Point>,
    clusters: Vec<u32>,
    font_ids: Vec<u32>,
    advance: f32,
    lines: usize,
    run_start: usize,
    cluster_base: u32,
}
impl skia::shaper::RunHandler for GlyphRuns {
    fn begin_line(&mut self) {
        self.lines += 1;
    }
    fn run_info(&mut self, _: &skia::shaper::run_handler::RunInfo) {}
    fn commit_run_info(&mut self) {}
    fn run_buffer(
        &mut self,
        info: &skia::shaper::run_handler::RunInfo,
    ) -> skia::shaper::run_handler::Buffer<'_> {
        let start = self.glyphs.len();
        self.run_start = start;
        self.glyphs.resize(start + info.glyph_count, 0);
        self.positions
            .resize(start + info.glyph_count, skia::Point::default());
        self.offsets
            .resize(start + info.glyph_count, skia::Point::default());
        self.clusters.resize(start + info.glyph_count, 0);
        self.font_ids.push(info.font.typeface().unique_id());
        let mut buffer = skia::shaper::run_handler::Buffer::new(
            &mut self.glyphs[start..],
            &mut self.positions[start..],
            skia::Point::new(self.advance, 0.0),
        );
        buffer.clusters = Some(&mut self.clusters[start..]);
        buffer.offsets = Some(&mut self.offsets[start..]);
        buffer
    }
    fn commit_run_buffer(&mut self, info: &skia::shaper::run_handler::RunInfo) {
        let start = self.run_start;
        let end = self.glyphs.len();
        for cluster in &mut self.clusters[start..] {
            *cluster += self.cluster_base;
        }
        let mut widths = vec![0.0; end - start];
        info.font.get_widths(&self.glyphs[start..], &mut widths);
        let mut correction = 0.0;
        for index in start..end {
            let next = if index + 1 == end {
                self.advance + info.advance.x
            } else {
                self.positions[index + 1].x
            };
            let advance = next - self.positions[index].x;
            self.positions[index].x += correction + self.offsets[index].x;
            self.positions[index].y += self.offsets[index].y;
            // Both backends use 16.16 HarfBuzz positions, but Canvas truncates
            // native glyph widths while SkShaper rounds them. Correct the
            // advance conversion; do not round the final measured line or
            // move non-advancing combining marks independently.
            if advance != 0.0 {
                let fixed = widths[index - start] * 65536.0;
                correction += (fixed.trunc() - fixed.round()) / 65536.0;
            }
        }
        self.advance += info.advance.x + correction;
    }
    fn commit_line(&mut self) {}
}
fn text_paint(color: [f64; 4]) -> Result<skia::Paint> {
    paint(color)?;
    let mut paint = skia::Paint::default();
    paint.set_anti_alias(true).set_color4f(
        skia::Color4f::new(
            color[0] as f32,
            color[1] as f32,
            color[2] as f32,
            color[3] as f32,
        ),
        None,
    );
    Ok(paint)
}
fn aligned(align: &str, width: f64) -> f64 {
    match align {
        "left" => 0.0,
        "right" => -width,
        _ => -width / 2.0,
    }
}
fn validate(text: &TextRasterInput) -> Result<()> {
    let bg = &text.background;
    let t = &text.transform;
    if !["left", "center", "right"].contains(&text.align.as_str())
        || !["none", "underline", "lineThrough"].contains(&text.decoration.as_str())
        || text.font_size_px <= 0.0
        || text.line_height_px <= 0.0
        || text.content.len() > 262144
        || [
            text.font_size_px,
            text.line_height_px,
            text.letter_spacing_px,
            bg.padding_x_px,
            bg.padding_y_px,
            bg.offset_x_px,
            bg.offset_y_px,
            bg.corner_radius_percent,
            t.center_x,
            t.center_y,
            t.scale_x,
            t.scale_y,
            t.rotation_radians,
        ]
        .iter()
        .any(|v| !v.is_finite() || !(*v as f32).is_finite())
    {
        return Err(invalid("Invalid prepared native text"));
    }
    paint(text.color)?;
    paint(bg.color)?;
    Ok(())
}
fn validate_synthesis(
    binding: Option<&FontSynthesisBinding>,
    font: &ValidatedTextFont,
) -> Result<bool> {
    let Some(binding) = binding else {
        return Ok(false);
    };
    if binding.actual.content_hash != font.content_hash
        || binding.actual.size_bytes != font.size
        || binding.actual.weight != font.weight
        || binding.actual.style != FontStyle::Normal
        || font.style != FontStyle::Normal
        || binding.requested.style != FontStyle::Italic
        || binding.requested.weight != font.weight
    {
        return Err(invalid(
            "Native italic request differs from the fixed real font",
        ));
    }
    Ok(true)
}
