//! Ordered Classic stacks retain the source graph while delegating raw rasters.
use super::*;
use opencut_contracts::{blend_frame::BoundBlendFrame, mask_frame::MaskQuad, scene_frame::*};
pub const CLASSIC_SCENE_FRAME_PROFILE: &str = "classic-v31-gpu-scene-frame-v1";
#[derive(Clone, Debug)]
pub struct PreparedSceneFrame {
    source_graph: SceneComposition,
    blend: PreparedBlendFrame,
    stacks: Vec<SceneLayerStack>,
    steps: Vec<SceneStep>,
    background: SceneBackground,
    mask_texts: Vec<PreparedSceneMaskText>,
    font_synthesis: Vec<PreparedSceneFontSynthesis>,
}
#[derive(Clone, Debug)]
pub struct PreparedSceneMaskText {
    pub element_id: String,
    pub mask_id: String,
    pub font: FontResource,
    pub mask: TextMask,
}
#[derive(Clone, Debug)]
pub struct PreparedSceneFontSynthesis {
    pub element_id: String,
    pub owner_id: String,
    pub owner_kind: SceneFontSynthesisOwner,
    pub font: FontResource,
    pub requested_weight: u16,
    pub requested_style: FontStyle,
}
impl PreparedSceneFrame {
    pub fn font_synthesis(&self) -> &[PreparedSceneFontSynthesis] {
        &self.font_synthesis
    }
    pub fn mask_texts(&self) -> &[PreparedSceneMaskText] {
        &self.mask_texts
    }

    pub fn source_graph(&self) -> &SceneComposition {
        &self.source_graph
    }
    pub fn blend(&self) -> &PreparedBlendFrame {
        &self.blend
    }
    pub fn execution(&self) -> &PreparedGraphicFrame {
        self.blend.execution()
    }
    pub fn blends(&self) -> &[opencut_contracts::blend_frame::LayerBlend] {
        self.blend.blends()
    }
    pub fn stacks(&self) -> &[SceneLayerStack] {
        &self.stacks
    }
    pub fn steps(&self) -> &[SceneStep] {
        &self.steps
    }
    pub fn background(&self) -> &SceneBackground {
        &self.background
    }
}
fn effect(effect: &SceneEffect) -> Result<(), CompositionError> {
    if !effect.enabled {
        return Ok(());
    }
    match effect.effect {
        EffectKind::ClassicBlur { intensity } if intensity.is_finite() && intensity >= 0. => Ok(()),
        EffectKind::LegacyColorAdjust {
            exposure_ev,
            contrast,
            saturation,
        } if exposure_ev.is_finite()
            && (-8.0..=8.0).contains(&exposure_ev)
            && contrast.is_finite()
            && (0.0..=4.0).contains(&contrast)
            && saturation.is_finite()
            && (0.0..=4.0).contains(&saturation) =>
        {
            Ok(())
        }
        _ => Err(unsupported(
            "Scene execution does not support this effect profile",
        )),
    }
}
fn mask(mask: &Mask) -> Result<(), CompositionError> {
    if !mask.enabled {
        return Ok(());
    }
    match mask.feather {
        MaskFeather::ClassicPixels { value }
            if value.is_finite()
                && value >= 0.
                && (value as f32).is_finite()
                && ((value as f32) == 0. || (value as f32) / 2. > 0.) => {}
        _ => {
            return Err(unsupported(
                "Scene mask requires finite Classic pixel feather",
            ));
        }
    }
    if !mask.stroke.width_px.is_finite()
        || mask.stroke.width_px < 0.
        || mask.stroke.color.encoding != ColorEncoding::Srgb
        || [
            mask.stroke.color.red,
            mask.stroke.color.green,
            mask.stroke.color.blue,
            mask.stroke.color.alpha,
        ]
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err(invalid(
            "Scene mask stroke requires finite width and normalized sRGB",
        ));
    }
    match &mask.shape {
        LayerMaskShape::LegacyRectangle { .. } | LayerMaskShape::LegacyEllipse { .. } => {
            return Err(unsupported(
                "Legacy canvas masks retain their original renderer",
            ));
        }

        _ => {}
    }
    Ok(())
}
pub fn prepare_scene_frame(
    graph: &SceneComposition,
) -> Result<PreparedSceneFrame, CompositionError> {
    let mut raw = graph.clone();
    let mut font_synthesis = vec![];
    let mut synthesis_ids = std::collections::BTreeSet::new();
    for font in &graph.fonts {
        if opencut_editor_core::scene::is_font_synthesis_resource(font) {
            opencut_editor_core::scene::validate_font_synthesis_resource(font)
                .map_err(|e| invalid(&e.message))?;
            synthesis_ids.insert(font.id.clone());
        }
    }
    // Only this explicit Scene profile can delegate an actual normal raster
    // while preserving the authored italic request in a separate typed map.
    for font in &mut raw.fonts {
        if synthesis_ids.contains(&font.id) {
            font.origin = ResourceOrigin::Generated {
                provider: "opencut-scene-native-font-execution-v1".into(),
                generation_id: font.content.content_hash.clone(),
            };
        }
    }
    for layer in &mut raw.layers {
        // A Classic sticker is an image asset. Preserve its authored node in
        // source_graph; only this Scene execution partition delegates decoding
        // to the existing image path, with actual decoded dimensions authoritative.
        if let SampledContent::Sticker { asset_id, .. } = &layer.content {
            layer.content = SampledContent::Image {
                asset_id: asset_id.clone(),
            };
        }
        match &mut layer.content {
            SampledContent::Text { text } => {
                if let FontBinding::Locked { font_id } = &text.font {
                    if synthesis_ids.contains(font_id) {
                        let requested_weight = match text.font_weight {
                            FontWeight::Normal => 400,
                            FontWeight::Bold => 700,
                        };
                        if text.font_style != FontStyle::Italic
                            || graph
                                .fonts
                                .iter()
                                .find(|f| &f.id == font_id)
                                .is_none_or(|f| f.weight != requested_weight)
                        {
                            return Err(unsupported(
                                "Scene synthesis requires explicit matching same-weight italic intent",
                            ));
                        }
                        text.font_style = FontStyle::Normal;
                    }
                }
            }
            SampledContent::Caption { style, .. } => {
                if matches!(&style.font, FontBinding::Locked { font_id } if synthesis_ids.contains(font_id))
                {
                    return Err(unsupported(
                        "Caption style cannot encode an explicit synthesis request",
                    ));
                }
            }
            _ => {}
        }
    }
    let mut stacks = vec![];
    let mut steps = vec![];
    let mut mask_texts = vec![];
    for layer in &graph.layers {
        if let SampledContent::EffectLayer {
            effect: layer_effect,
        } = &layer.content
        {
            effect(layer_effect)?;
            let mut effects = vec![layer_effect.clone()];
            if let Some(style) = &layer.visual {
                for extra in &style.effects {
                    effect(extra)?;
                }
                if style.masks.iter().any(|m| m.enabled) {
                    return Err(unsupported("Adjustment layers do not have a mask source"));
                }
                effects.extend(style.effects.clone());
            }
            steps.push(SceneStep::Effect {
                element_id: layer.element_id.clone(),
                effects,
            });
            continue;
        }
        let visual = layer
            .visual
            .as_ref()
            .ok_or_else(|| unsupported("Scene visual style missing"))?;
        let enabled_effects: Vec<_> = visual.effects.iter().filter(|e| e.enabled).collect();
        if enabled_effects
            .iter()
            .any(|e| matches!(e.effect, EffectKind::LegacyColorAdjust { .. }))
            && enabled_effects
                .iter()
                .any(|e| !matches!(e.effect, EffectKind::LegacyColorAdjust { .. }))
        {
            return Err(unsupported(
                "Mixed source-space legacy and canvas-space Classic effects require explicit conversion",
            ));
        }
        for entry in &visual.effects {
            effect(entry)?;
        }
        let enabled: Vec<_> = visual.masks.iter().filter(|m| m.enabled).collect();
        if !enabled.is_empty()
            && !matches!(
                layer.content,
                SampledContent::Video { .. }
                    | SampledContent::Image { .. }
                    | SampledContent::Sticker { .. }
                    | SampledContent::Graphic { .. }
            )
        {
            return Err(unsupported("Only Classic media and graphics accept masks"));
        }
        for m in &visual.masks {
            mask(m)?;
        }
        for entry in visual.masks.iter().filter(|m| m.enabled) {
            if let LayerMaskShape::Text { mask } = &entry.shape {
                if mask.content.chars().all(|c|matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')) {continue;}
                let FontBinding::Locked { font_id } = &mask.font else {
                    return Err(unsupported("Scene text mask requires a locked static font"));
                };
                let mut fonts = graph.fonts.iter().filter(|f| &f.id == font_id);
                let font = fonts
                    .next()
                    .ok_or_else(|| invalid("Scene text-mask font is missing"))?;
                let weight = match mask.font_weight {
                    FontWeight::Normal => 400,
                    FontWeight::Bold => 700,
                };
                if fonts.next().is_some()
                    || font.face_index != 0
                    || font.weight != weight
                    || (font.style != mask.font_style
                        && !(synthesis_ids.contains(font_id)
                            && mask.font_style == FontStyle::Italic))
                {
                    return Err(unsupported(
                        "Scene text mask requires its matching static face without synthesis",
                    ));
                }
                if synthesis_ids.contains(font_id) {
                    if mask.font_style != FontStyle::Italic {
                        return Err(unsupported(
                            "Scene text mask synthesis requires explicit italic intent",
                        ));
                    }
                    font_synthesis.push(PreparedSceneFontSynthesis {
                        element_id: layer.element_id.clone(),
                        owner_id: entry.id.clone(),
                        owner_kind: SceneFontSynthesisOwner::TextMask,
                        font: font.clone(),
                        requested_weight: weight,
                        requested_style: mask.font_style,
                    });
                }
                mask_texts.push(PreparedSceneMaskText {
                    element_id: layer.element_id.clone(),
                    mask_id: entry.id.clone(),
                    font: raw
                        .fonts
                        .iter()
                        .find(|candidate| candidate.id == font.id)
                        .ok_or_else(|| invalid("Scene mask execution font missing"))?
                        .clone(),
                    mask: mask.clone(),
                });
            }
        }

        stacks.push(SceneLayerStack {
            element_id: layer.element_id.clone(),
            masks: visual.masks.clone(),
            effects: visual.effects.clone(),
        });
        steps.push(SceneStep::Layer {
            element_id: layer.element_id.clone(),
        });
    }
    raw.layers
        .retain(|layer| !matches!(layer.content, SampledContent::EffectLayer { .. }));
    for layer in &mut raw.layers {
        let v = layer.visual.as_mut().expect("validated");
        v.masks.clear();
        v.effects.clear();
    }
    // The compiler already sampled transition handles and progress. Lower the
    // supported alpha-over crossfade into the delegated incoming layer only;
    // preserve the complete authored transition in source_graph for receipts.
    let mut incoming = std::collections::BTreeSet::new();
    for transition in &graph.transitions {
        if transition.profile != TransitionKind::LegacyCrossfadeV3
            || transition.outgoing_weight != 1.0
            || !(0.0..=1.0).contains(&transition.incoming_weight)
            || !incoming.insert(&transition.incoming_element_id)
        {
            return Err(unsupported(
                "Scene transition profile or weights are unsupported",
            ));
        }
        for id in [
            &transition.outgoing_element_id,
            &transition.incoming_element_id,
        ] {
            if !raw.layers.iter().any(|layer| {
                layer.element_id == *id && matches!(layer.content, SampledContent::Video { .. })
            }) {
                return Err(unsupported(
                    "Scene transition requires both video participants",
                ));
            }
        }
        let layer = raw
            .layers
            .iter_mut()
            .find(|layer| layer.element_id == transition.incoming_element_id)
            .expect("validated incoming participant");
        layer
            .visual
            .as_mut()
            .expect("validated visual")
            .effective_opacity *= transition.incoming_weight;
    }
    raw.transitions.clear();
    let background = match &graph.background {
        CompiledBackground::Color { color } => SceneBackground::Color {
            color: color.clone(),
        },
        CompiledBackground::MainTrackBlur {
            intensity,
            source_layers,
            ..
        } => {
            if !intensity.is_finite() || *intensity < 0. {
                return Err(invalid(
                    "Background blur intensity must be finite and nonnegative",
                ));
            }
            SceneBackground::MainTrackBlur {
                intensity: *intensity,
                source_element_ids: source_layers.clone(),
            }
        }
    };
    raw.background = CompiledBackground::Color {
        color: Color {
            encoding: ColorEncoding::Srgb,
            red: 0.,
            green: 0.,
            blue: 0.,
            alpha: 1.,
        },
    };
    let text_fonts: std::collections::BTreeSet<_> = raw
        .layers
        .iter()
        .filter_map(|layer| match &layer.content {
            SampledContent::Text { text } => Some(&text.font),
            SampledContent::Caption { style, .. } => Some(&style.font),
            _ => None,
        })
        .filter_map(|font| match font {
            FontBinding::Locked { font_id } => Some(font_id.clone()),
            _ => None,
        })
        .collect();
    raw.fonts.retain(|font| text_fonts.contains(&font.id));
    let mut blend = prepare_blend_frame(&raw)?;
    // Colour backgrounds can consume opaque VideoFrames directly on the GPU.
    // CPU backdrop painting keeps a bounded canvas transfer; frozen narrow
    // profiles retain their original four-surface contract.
    blend.execution.execution.execution.media.decoder_surfaces =
        if matches!(&background, SceneBackground::Color { .. }) {
            1
        } else {
            2
        };
    for title in blend.execution().execution().execution().texts() {
        if synthesis_ids.contains(&title.font.id) {
            let font = graph
                .fonts
                .iter()
                .find(|f| f.id == title.font.id)
                .ok_or_else(|| invalid("Scene synthesis font missing"))?;
            font_synthesis.push(PreparedSceneFontSynthesis {
                element_id: title.identity.element_id.clone(),
                owner_id: title.identity.element_id.clone(),
                owner_kind: SceneFontSynthesisOwner::Text,
                font: font.clone(),
                requested_weight: font.weight,
                requested_style: FontStyle::Italic,
            });
        }
    }
    if !blend
        .execution()
        .order()
        .iter()
        .eq(stacks.iter().map(|s| &s.element_id))
    {
        return Err(invalid(
            "Scene stack order differs from delegated layer order",
        ));
    }
    Ok(PreparedSceneFrame {
        source_graph: graph.clone(),
        blend,
        stacks,
        steps,
        background,
        mask_texts,
        font_synthesis,
    })
}
pub fn bind_scene_frame(
    prepared: &PreparedSceneFrame,
    child: &BoundBlendFrame,
) -> Result<BoundSceneFrame, CompositionError> {
    let graphic = &child.execution;
    let text = prepared.execution().execution().execution();
    let media = bind_visual_sources_scene(text.media(), &graphic.decoded)?;
    if !prepared
        .stacks
        .iter()
        .map(|s| &s.element_id)
        .eq(graphic.layers.iter().map(|l| &l.element_id))
    {
        return Err(invalid("Scene binding changed layer inventory"));
    }
    let mut layers = vec![];
    for (stack, layer) in prepared.stacks.iter().zip(&graphic.layers) {
        let source = prepared
            .source_graph
            .layers
            .iter()
            .find(|s| s.element_id == stack.element_id)
            .ok_or_else(|| invalid("Scene source missing"))?;
        let size = if let Some(g) = prepared
            .execution()
            .graphics()
            .iter()
            .find(|g| g.element_id == stack.element_id)
        {
            if layer.quad != g.quad || layer.asset_id != format!("graphic:{}", stack.element_id) {
                return Err(invalid("Scene graphic quad changed"));
            }
            Some((g.graphic.source_width, g.graphic.source_height))
        } else if let Some(expected) = media
            .layers
            .iter()
            .find(|l| l.element_id == stack.element_id)
        {
            if layer != expected {
                return Err(invalid("Scene media geometry changed"));
            }
            graphic.decoded.iter().find_map(|d| match d {
                DecodedVisualSource::ReadyImage {
                    element_id,
                    width,
                    height,
                }
                | DecodedVisualSource::ReadyVideo {
                    element_id,
                    width,
                    height,
                    ..
                } if element_id == &stack.element_id => Some((*width, *height)),
                _ => None,
            })
        } else {
            None
        };
        let quad = if layer.quad.is_none() {
            None
        } else if let Some((w, h)) = size {
            super::mask_frame_execution::logical_quad(&prepared.source_graph, source, w, h)?
        } else {
            Some(MaskQuad {
                center_x: f64::from(text.identity().width) / 2.,
                center_y: f64::from(text.identity().height) / 2.,
                width: f64::from(text.identity().width),
                height: f64::from(text.identity().height),
                rotation_radians: 0.,
                flip_x: false,
                flip_y: false,
                opacity: 1.,
            })
        };
        layers.push(BoundSceneLayer {
            stack: stack.clone(),
            quad,
        });
    }
    let mut identity = child.identity.clone();
    identity.profile = CLASSIC_SCENE_FRAME_PROFILE.into();
    Ok(BoundSceneFrame {
        identity,
        execution: child.clone(),
        layers,
        steps: prepared.steps.clone(),
        background: prepared.background.clone(),
    })
}
