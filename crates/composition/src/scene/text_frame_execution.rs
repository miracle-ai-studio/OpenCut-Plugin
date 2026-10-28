//! New normal-frame assembly for media and free text. The frozen media-only
//! frame preparer/binder remains authoritative for every delegated media node.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub const CLASSIC_TEXT_FRAME_PROFILE: &str = "classic-v31-gpu-normal-text-frame-v2";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextFrameIdentity {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub time: Time,
    pub width: u32,
    pub height: u32,
    pub profile: String,
}

#[derive(Clone, Debug)]
pub struct PreparedTextFrame {
    identity: TextFrameIdentity,
    graph: SceneComposition,
    pub(super) media: PreparedVisualFrame,
    texts: Vec<TextRasterPlan>,
    order: Vec<LayerOrder>,
}

#[derive(Clone, Debug)]
enum LayerOrder {
    Media(usize),
    Text(usize),
}

impl PreparedTextFrame {
    /// Caption layout may change text, never the already-admitted media transfer.
    pub fn reflow_text(&self, graph: &SceneComposition) -> Result<Self, CompositionError> {
        let mut reflowed = prepare_text_frame(graph)?;
        if reflowed.media.graph() != self.media.graph() {
            return Err(invalid("Text reflow cannot change fixed media sources"));
        }
        Ok(reflowed)
    }
    pub fn identity(&self) -> &TextFrameIdentity {
        &self.identity
    }
    pub fn graph(&self) -> &SceneComposition {
        &self.graph
    }
    pub fn layers(&self) -> &[CompiledLayer] {
        &self.graph.layers
    }
    /// Internal delegated media subgraph, not the complete V2 frame receipt.
    pub fn media(&self) -> &PreparedVisualFrame {
        &self.media
    }
    pub fn texts(&self) -> &[TextRasterPlan] {
        &self.texts
    }
}

/// Constructed only by the native caller after authorized, hash-verified font
/// inspection proves the fixed face covers this complete text. This internal
/// fact is never accepted from a browser/public request as a coverage claim.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeTextCoverage {
    pub element_id: String,
    pub font: FontResource,
    pub content: String,
}

/// Browser raster facts after the native caller has matched its observed text
/// digest to the fixed UTF-8 digest. `content` comes from the complete observed
/// line sequence; it cannot be substituted with unrelated expected text.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRasterObservation {
    pub element_id: String,
    pub font_id: String,
    pub font_content_hash: String,
    pub font_weight: u16,
    pub font_style: FontStyle,
    pub face_index: u32,
    pub content: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextRasterBinding {
    pub coverage: NativeTextCoverage,
    pub raster: TextRasterObservation,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BoundTextFrameLayer {
    Media(BoundVisualLayer),
    Text {
        plan: TextRasterPlan,
        observation: TextRasterObservation,
        quad: VisualQuad,
    },
}

impl BoundTextFrameLayer {
    pub fn element_id(&self) -> &str {
        match self {
            Self::Media(layer) => &layer.element_id,
            Self::Text { plan, .. } => &plan.identity.element_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoundTextFrame {
    pub identity: TextFrameIdentity,
    pub clear: [f32; 4],
    pub background: [f32; 4],
    /// Complete original bottom-to-top order, including no-frame media nodes.
    pub layers: Vec<BoundTextFrameLayer>,
    /// Actual media observations, ordered exactly as the delegated media nodes.
    /// Text raster observations remain attached to their respective layers.
    pub decoded: Vec<DecodedVisualSource>,
}

pub fn prepare_text_frame(graph: &SceneComposition) -> Result<PreparedTextFrame, CompositionError> {
    reject_legacy_synthesis_fonts(&graph.fonts)?;
    let mut ids = BTreeSet::new();
    let mut required_fonts = BTreeSet::new();
    let mut media_layers = Vec::new();
    let mut texts = Vec::new();
    let mut order = Vec::new();
    for layer in &graph.layers {
        if layer.element_id.is_empty() || !ids.insert(layer.element_id.as_str()) {
            return Err(invalid(
                "V2 frame requires distinct nonempty layer identities",
            ));
        }
        match &layer.content {
            SampledContent::Image { .. } | SampledContent::Video { .. } => {
                order.push(LayerOrder::Media(media_layers.len()));
                media_layers.push(layer.clone());
            }
            SampledContent::Text { .. } => {
                let plan = prepare_text_raster(graph, &layer.element_id)?;
                required_fonts.insert(plan.font.id.clone());
                order.push(LayerOrder::Text(texts.len()));
                texts.push(plan);
            }
            SampledContent::Caption { .. }
            | SampledContent::Sticker { .. }
            | SampledContent::Graphic { .. }
            | SampledContent::EffectLayer { .. } => {
                return Err(unsupported(format!(
                    "V2 normal frame cannot execute layer {} of this kind",
                    layer.element_id
                )));
            }
        }
    }
    let mut present_fonts = BTreeSet::new();
    for font in &graph.fonts {
        if !present_fonts.insert(font.id.clone()) || !required_fonts.contains(&font.id) {
            return Err(invalid(
                "V2 frame fonts must exactly cover participating text layers",
            ));
        }
        if graph
            .resources
            .iter()
            .any(|resource| resource.asset_id == font.id)
        {
            return Err(invalid(
                "Font and media resource identities must be distinct",
            ));
        }
    }
    if present_fonts != required_fonts {
        return Err(invalid("V2 frame font binding is incomplete"));
    }

    // This is an explicit delegated execution partition, never another compile
    // or projection to the old project model. All removed nodes have a prepared
    // text plan and an order slot above. Do not filter media resources: the V1
    // exact-coverage check must still reject extra or missing CAS identities.
    let mut media_graph = graph.clone();
    media_graph.layers = media_layers;
    media_graph.fonts.clear();
    let media = prepare_visual_frame_scene(&media_graph)?;
    let identity = media.identity();
    Ok(PreparedTextFrame {
        identity: TextFrameIdentity {
            project_id: identity.project_id.clone(),
            revision: identity.revision.clone(),
            sequence_id: identity.sequence_id.clone(),
            time: identity.time.clone(),
            width: identity.width,
            height: identity.height,
            profile: CLASSIC_TEXT_FRAME_PROFILE.into(),
        },
        graph: graph.clone(),
        media,
        texts,
        order,
    })
}

fn bind_text(
    plan: &TextRasterPlan,
    binding: &TextRasterBinding,
) -> Result<BoundTextFrameLayer, CompositionError> {
    let proof = &binding.coverage;
    let observed = &binding.raster;
    if proof.element_id != plan.identity.element_id
        || proof.font != plan.font
        || proof.content != plan.text.content
        || observed.element_id != plan.identity.element_id
        || observed.font_id != plan.font.id
        || observed.font_content_hash != plan.font.content.content_hash
        || observed.font_weight != plan.font.weight
        || observed.font_style != plan.font.style
        || observed.face_index != plan.font.face_index
        || observed.content != plan.text.content
        || observed.width != plan.identity.width
        || observed.height != plan.identity.height
    {
        return Err(invalid(
            "Native text coverage or raster facts differ from the fixed text plan",
        ));
    }
    Ok(BoundTextFrameLayer::Text {
        plan: plan.clone(),
        observation: observed.clone(),
        quad: VisualQuad {
            center_x: plan.identity.width as f32 / 2.0,
            center_y: plan.identity.height as f32 / 2.0,
            width: plan.identity.width as f32,
            height: plan.identity.height as f32,
            rotation_radians: 0.0,
            flip_x: false,
            flip_y: false,
            opacity: plan.opacity as f32,
        },
    })
}

pub fn bind_text_frame_sources(
    prepared: &PreparedTextFrame,
    decoded: &[DecodedVisualSource],
    texts: &[TextRasterBinding],
) -> Result<BoundTextFrame, CompositionError> {
    let media = bind_visual_sources_scene(&prepared.media, decoded)?;
    let mut observations = BTreeMap::new();
    for binding in texts {
        if binding.coverage.element_id != binding.raster.element_id
            || observations
                .insert(binding.raster.element_id.as_str(), binding)
                .is_some()
        {
            return Err(invalid(
                "Text bindings must have distinct, matching native/raster identities",
            ));
        }
    }
    if observations.len() != prepared.texts.len() {
        return Err(invalid(
            "Text bindings must cover every prepared text layer exactly once",
        ));
    }
    let bound_texts = prepared
        .texts
        .iter()
        .map(|plan| {
            let binding = observations
                .get(plan.identity.element_id.as_str())
                .ok_or_else(|| invalid("Prepared text raster observation missing"))?;
            bind_text(plan, binding)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let layers = prepared
        .order
        .iter()
        .map(|entry| match *entry {
            LayerOrder::Media(index) => BoundTextFrameLayer::Media(media.layers[index].clone()),
            LayerOrder::Text(index) => bound_texts[index].clone(),
        })
        .collect();
    Ok(BoundTextFrame {
        identity: prepared.identity.clone(),
        clear: media.clear,
        background: media.background,
        layers,
        decoded: media.decoded,
    })
}

#[cfg(test)]
mod tests;
