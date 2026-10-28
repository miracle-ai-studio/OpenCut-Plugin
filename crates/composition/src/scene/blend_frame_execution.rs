//! Blend composition wraps the unchanged D0 raster/decode partition. Its source
//! graph and complete ordered blend identity remain fixed until final binding.
use super::*;
use opencut_contracts::blend_frame::LayerBlend;

pub const CLASSIC_BLEND_FRAME_PROFILE: &str = "classic-v31-gpu-blend-frame-v1";

#[derive(Clone, Debug)]
pub struct PreparedBlendFrame {
    source_graph: SceneComposition,
    pub(super) execution: PreparedGraphicFrame,
    blends: Vec<LayerBlend>,
}
impl PreparedBlendFrame {
    pub fn source_graph(&self) -> &SceneComposition {
        &self.source_graph
    }
    pub fn execution(&self) -> &PreparedGraphicFrame {
        &self.execution
    }
    pub fn blends(&self) -> &[LayerBlend] {
        &self.blends
    }
}

pub fn prepare_blend_frame(
    graph: &SceneComposition,
) -> Result<PreparedBlendFrame, CompositionError> {
    reject_legacy_synthesis_fonts(&graph.fonts)?;
    let mut rasterization_graph = graph.clone();
    let mut blends = Vec::with_capacity(graph.layers.len());
    for layer in &mut rasterization_graph.layers {
        let visual = layer
            .visual
            .as_mut()
            .ok_or_else(|| unsupported("Blend frame requires a visual style for every layer"))?;
        blends.push(LayerBlend {
            element_id: layer.element_id.clone(),
            blend_mode: visual.blend_mode,
        });
        // The delegated preparation computes geometry, resource requests and
        // individual rasters only. The full GPU frame MUST use the original
        // blend table; this copy is never accepted as the final blend identity.
        visual.blend_mode = BlendMode::Normal;
    }
    // Existing validation still rejects unsupported effects, masks, profiles,
    // resources, captions, transitions and duplicate/missing element identities.
    let execution = prepare_graphic_frame(&rasterization_graph)?;
    if !execution
        .order()
        .iter()
        .eq(blends.iter().map(|blend| &blend.element_id))
    {
        return Err(invalid(
            "Blend table differs from the complete execution order",
        ));
    }
    Ok(PreparedBlendFrame {
        source_graph: graph.clone(),
        execution,
        blends,
    })
}

pub fn validate_blend_layers(
    prepared: &PreparedBlendFrame,
    blends: &[LayerBlend],
) -> Result<(), CompositionError> {
    if prepared.blends != blends {
        return Err(invalid(
            "Blend facts differ from fixed layer identities, order or modes",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
