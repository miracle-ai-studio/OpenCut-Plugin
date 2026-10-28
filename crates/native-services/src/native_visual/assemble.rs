use super::{engine::Bindings, plan::Plan, text::raster_error, *};
use opencut_contracts::{
    scene::{BlendMode, EffectKind, FontSize, LayerMaskShape, SceneEffect, TextDecoration},
    scene_frame::{SceneBackground, SceneFontSynthesisOwner, SceneStep},
    text_frame::{TextLayerPlan, TextRasterBackground, TextRasterInput, TextRasterTransform},
    visual_frame::VisualQuad,
};
use opencut_media::native_decode::{DecodedVideoFrame, VideoColorMatrix};
use opencut_renderer::{
    self as gpu,
    raster::{RasterImage, RasterLimits, SceneMaskRaster, TextRasterizer},
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
enum Source {
    Media(Arc<DecodedVideoFrame>),
    Raster(Arc<RasterImage>),
}
impl Source {
    fn raster(&self) -> gpu::Raster<'_> {
        match self {
            Self::Media(frame) => gpu::Raster {
                width: frame.width,
                height: frame.height,
                rgba: &frame.rgba,
            },
            Self::Raster(image) => image.as_raster(),
        }
    }
    fn rotation(&self) -> u16 {
        match self {
            Self::Media(frame) => frame.rotation_degrees,
            Self::Raster(_) => 0,
        }
    }
    fn matrix(&self) -> gpu::ColorMatrix {
        match self {
            Self::Media(frame) => match frame.matrix {
                VideoColorMatrix::Bt709 => gpu::ColorMatrix::Bt709,
                VideoColorMatrix::Bt601 => gpu::ColorMatrix::Bt601,
                _ => gpu::ColorMatrix::Unspecified,
            },
            _ => gpu::ColorMatrix::Unspecified,
        }
    }
    fn display_size(&self) -> (u32, u32) {
        let raster = self.raster();
        if self.rotation() % 180 == 0 {
            (raster.width, raster.height)
        } else {
            (raster.height, raster.width)
        }
    }
}
struct Layer {
    source: Source,
    quad: VisualQuad,
    blend: BlendMode,
    masks: Vec<SceneMaskRaster>,
    effects: Vec<SceneEffect>,
    effect_size: [u32; 2],
}
enum Step {
    Layer(usize),
    Effects(Vec<SceneEffect>),
}
pub(super) struct Prepared {
    layers: Vec<Layer>,
    steps: Vec<Step>,
    background: Option<Layer>,
}
fn full(width: u32, height: u32) -> VisualQuad {
    VisualQuad {
        center_x: width as f32 / 2.0,
        center_y: height as f32 / 2.0,
        width: width as f32,
        height: height as f32,
        rotation_radians: 0.0,
        opacity: 1.0,
        flip_x: false,
        flip_y: false,
    }
}

pub(super) fn prepare(
    plan: &Plan,
    bound: &Bindings,
    media: &BTreeMap<String, Arc<DecodedVideoFrame>>,
    rasters: &mut BTreeMap<String, Arc<RasterImage>>,
    text: &mut TextRasterizer,
    limits: RasterLimits,
    checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
) -> Result<Prepared, EditorError> {
    let identity = &plan.media().identity;
    let (width, height) = (identity.width, identity.height);
    let mut sources: BTreeMap<String, Source> = media
        .iter()
        .map(|(id, source)| (id.clone(), Source::Media(source.clone())))
        .collect();
    for (id, raster) in rasters.iter() {
        if sources
            .insert(id.clone(), Source::Raster(raster.clone()))
            .is_some()
        {
            return Err(invalid("Native source partitions overlap"));
        }
    }
    let mut result = Prepared {
        layers: Vec::new(),
        steps: Vec::new(),
        background: None,
    };
    let mut indices = BTreeMap::new();
    let mut retained = sources
        .values()
        .map(|s| s.raster().rgba.len() as u64)
        .sum::<u64>();
    for layer in &bound.layers {
        checkpoint()?;
        let Some(quad) = &layer.quad else { continue };
        let source = sources
            .get(&layer.element_id)
            .ok_or_else(|| invalid("Bound native quad has no prepared source"))?
            .clone();
        let blend = bound
            .blend
            .as_ref()
            .and_then(|b| b.blends.iter().find(|b| b.element_id == layer.element_id))
            .map_or(BlendMode::Normal, |b| b.blend_mode);
        let scene = bound.scene.as_ref().and_then(|s| {
            s.layers
                .iter()
                .find(|s| s.stack.element_id == layer.element_id)
        });
        let simple = bound
            .masks
            .as_ref()
            .and_then(|s| s.masks.iter().find(|s| s.element_id == layer.element_id));
        let effects = scene.map_or_else(Vec::new, |s| s.stack.effects.clone());
        if effects
            .iter()
            .any(|e| e.enabled && matches!(e.effect, EffectKind::LegacyColorAdjust { .. }))
        {
            if let Source::Media(frame) = &source {
                // Images are encoded RGB, while legacy video grades require a
                // proved limited-range matrix. Never silently guess HDR/range.
                let video = plan.media().layers.iter().any(|p| {
                    p.element_id == layer.element_id
                        && matches!(
                            p.decode,
                            opencut_api_contracts::visual_preview::VisualDecodeRequest::Video { .. }
                        )
                });
                if video
                    && (frame.full_range
                        || !matches!(
                            frame.matrix,
                            VideoColorMatrix::Bt601 | VideoColorMatrix::Bt709
                        ))
                {
                    return Err(EditorError::new(
                        "UNSUPPORTED_RENDER_FEATURE",
                        "Native legacy grades require limited-range BT.601 or BT.709 video",
                    ));
                }
            }
        }
        let logical = scene
            .and_then(|s| s.quad.as_ref())
            .or_else(|| simple.and_then(|s| s.quad.as_ref()));
        let effect_size = logical.map_or([width, height], |q| {
            [q.width.abs().round() as u32, q.height.abs().round() as u32]
        });
        let mut masks = Vec::new();
        let mut strokes = Vec::new();
        if let Some(logical) = logical {
            let authored: Vec<_> = if let Some(scene) = scene {
                scene.stack.masks.iter().collect()
            } else {
                simple.and_then(|s| s.mask.as_ref()).into_iter().collect()
            };
            for mask in authored {
                checkpoint()?;
                if !mask.enabled {
                    continue;
                }
                let available = RasterLimits {
                    max_pixels: limits.max_pixels,
                    max_bytes: limits
                        .max_bytes
                        .checked_sub(retained)
                        .ok_or_else(|| invalid("Native scene rasters exceed admission"))?,
                };
                let local = if matches!(&mask.shape,LayerMaskShape::Text{mask}if !mask.content.trim().is_empty())
                {
                    let local_width = logical.width.round() as u32;
                    let local_height = logical.height.round() as u32;
                    if local_width != 0 && local_height != 0 {
                        Some(text_mask(
                            plan,
                            &layer.element_id,
                            mask,
                            local_width,
                            local_height,
                            text,
                            available,
                        )?)
                    } else {
                        None
                    }
                } else {
                    None
                };
                let text_source = local.as_ref().map(|(body, stroke)| (body, stroke.as_ref()));
                if let Some(mut raster) =
                    gpu::raster::scene_mask(mask, logical, width, height, text_source, available)
                        .map_err(raster_error)?
                {
                    retained += raster.body.rgba.len() as u64;
                    if let Some(stroke) = raster.stroke.take() {
                        retained += stroke.rgba.len() as u64;
                        strokes.push(Arc::new(stroke));
                    }
                    masks.push(raster);
                }
            }
        }
        let mut owned = vec![result.layers.len()];
        result.layers.push(Layer {
            source,
            quad: quad.clone(),
            blend,
            masks,
            effects,
            effect_size,
        });
        for stroke in strokes {
            owned.push(result.layers.len());
            result.layers.push(Layer {
                source: Source::Raster(stroke),
                quad: full(width, height),
                blend: BlendMode::Normal,
                masks: Vec::new(),
                effects: Vec::new(),
                effect_size: [width, height],
            });
        }
        indices.insert(layer.element_id.clone(), owned);
    }
    if let Some(scene) = &bound.scene {
        for step in &scene.steps {
            match step {
                SceneStep::Layer { element_id } => {
                    if let Some(indices) = indices.get(element_id) {
                        result.steps.extend(indices.iter().map(|n| Step::Layer(*n)));
                    }
                }
                SceneStep::Effect { effects, .. } => {
                    result.steps.push(Step::Effects(effects.clone()))
                }
            }
        }
        if let SceneBackground::MainTrackBlur {
            intensity,
            source_element_ids,
        } = &scene.background
        {
            if let Some(source) = source_element_ids.iter().find_map(|id| sources.get(id)) {
                let (w, h) = source.display_size();
                let scale = (f64::from(width) / f64::from(w)).max(f64::from(height) / f64::from(h));
                let mut quad = full(width, height);
                quad.width = (f64::from(w) * scale) as f32;
                quad.height = (f64::from(h) * scale) as f32;
                result.background = Some(Layer {
                    source: source.clone(),
                    quad,
                    blend: BlendMode::Normal,
                    masks: Vec::new(),
                    effect_size: [width, height],
                    effects: vec![SceneEffect {
                        id: "background".into(),
                        enabled: true,
                        effect: EffectKind::ClassicBlur {
                            intensity: *intensity,
                        },
                    }],
                });
            }
        }
    } else {
        for layer in &bound.layers {
            if let Some(indices) = indices.get(&layer.element_id) {
                result.steps.extend(indices.iter().map(|n| Step::Layer(*n)));
            }
        }
    }
    checkpoint()?;
    Ok(result)
}

fn text_mask(
    plan: &Plan,
    element: &str,
    mask: &opencut_contracts::scene::Mask,
    width: u32,
    height: u32,
    rasterizer: &mut TextRasterizer,
    limits: RasterLimits,
) -> Result<(RasterImage, Option<RasterImage>), EditorError> {
    let LayerMaskShape::Text { mask: input } = &mask.shape else {
        return Err(invalid("Native text mask shape changed"));
    };
    let scene = plan
        .scene()
        .ok_or_else(|| invalid("Native text mask requires a complete scene"))?;
    let font = scene
        .mask_fonts
        .iter()
        .find(|f| f.mask_id == mask.id)
        .ok_or_else(|| invalid("Native text mask omitted its font proof"))?
        .font
        .clone();
    let size = match input.font_size {
        FontSize::ClassicHeight90 { value } => value * f64::from(height) / 90.0,
        FontSize::CanvasPixels { value } => value,
    };
    let layer = TextLayerPlan {
        element_id: mask.id.clone(),
        font,
        opacity: 1.0,
        text: TextRasterInput {
            content: input.content.clone(),
            font_size_px: size,
            line_height_px: size * input.line_height_multiplier,
            letter_spacing_px: input.letter_spacing_px,
            align: "center".into(),
            decoration: match input.decoration {
                TextDecoration::None => "none",
                TextDecoration::Underline => "underline",
                TextDecoration::LineThrough => "lineThrough",
            }
            .into(),
            color: [1.0; 4],
            background: TextRasterBackground {
                enabled: false,
                color: [0.0; 4],
                padding_x_px: 0.0,
                padding_y_px: 0.0,
                offset_x_px: 0.0,
                offset_y_px: 0.0,
                corner_radius_percent: 0.0,
            },
            transform: TextRasterTransform {
                center_x: f64::from(width) / 2.0 + input.center_x * f64::from(width),
                center_y: f64::from(height) / 2.0 + input.center_y * f64::from(height),
                scale_x: input.scale,
                scale_y: input.scale,
                rotation_radians: input.rotation_degrees.to_radians(),
            },
        },
    };
    let synthesis = scene
        .font_synthesis
        .iter()
        .find(|s| {
            s.owner_kind == SceneFontSynthesisOwner::TextMask
                && s.element_id == element
                && s.owner_id == mask.id
                && s.font_id == layer.font.font_id
        })
        .map(|s| &s.binding);
    let (body, _) = rasterizer
        .rasterize(&layer, width, height, synthesis, limits)
        .map_err(raster_error)?;
    let stroke = if mask.stroke.width_px > 0.0 {
        Some(
            rasterizer
                .mask_stroke(&layer, &body, &mask.stroke, synthesis, limits)
                .map_err(raster_error)?,
        )
    } else {
        None
    };
    Ok((body, stroke))
}

pub(super) fn render(
    renderer: &mut gpu::Renderer,
    plan: &Plan,
    bound: &Bindings,
    prepared: &Prepared,
    execution: &mut dyn gpu::Execution,
) -> Result<gpu::Receipt, EditorError> {
    let masks: Vec<Vec<_>> = prepared
        .layers
        .iter()
        .map(|layer| {
            layer
                .masks
                .iter()
                .map(|mask| gpu::Mask {
                    source: mask.body.as_raster(),
                    feather: mask.feather,
                    inverted: mask.inverted,
                })
                .collect()
        })
        .collect();
    let layers: Vec<_> = prepared
        .layers
        .iter()
        .zip(&masks)
        .map(|(layer, masks)| view(layer, masks))
        .collect();
    let background = prepared.background.as_ref().map(|layer| view(layer, &[]));
    let steps: Vec<_> = prepared
        .steps
        .iter()
        .map(|step| match step {
            Step::Layer(index) => gpu::Step::Layer(*index),
            Step::Effects(effects) => gpu::Step::Effects(effects),
        })
        .collect();
    renderer
        .render(
            &gpu::Frame {
                width: plan.media().identity.width,
                height: plan.media().identity.height,
                clear: bound.clear,
                background: background.map_or(
                    gpu::Background::Color(bound.background),
                    gpu::Background::Layer,
                ),
                layers: &layers,
                steps: &steps,
            },
            execution,
        )
        .map_err(raster_error)
}
fn view<'a>(layer: &'a Layer, masks: &'a [gpu::Mask<'a>]) -> gpu::Layer<'a> {
    gpu::Layer {
        source: layer.source.raster(),
        quad: layer.quad.clone(),
        blend: layer.blend,
        masks,
        effects: &layer.effects,
        effect_size: layer.effect_size,
        color_matrix: layer.source.matrix(),
        rotation_degrees: layer.source.rotation(),
    }
}
