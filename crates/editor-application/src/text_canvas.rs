//! Canvas editing observes the same sampled IR and native font coverage as the
//! actual renderer. It creates neither a second composition nor a durable job.
use crate::classic_text_frame::ClassicTextFrameSession;
use crate::font_synthesis::binding_for_resource;
use opencut_composition::{
    CompositionScope,
    scene::{
        NativeTextCoverage, PreparedTextFrame, SampledContent, SceneComposition,
        compile_frame_scene, prepare_text_frame, prepare_text_raster,
    },
};
use opencut_contracts::font_synthesis::FontSynthesisBinding;
use opencut_contracts::{EditorError, scene::*, text_canvas::*};
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;

#[derive(Clone, Debug)]
pub struct TextCanvasFacts {
    pub identity: opencut_contracts::text_frame::TextFrameIdentity,
    pub layer: opencut_contracts::text_frame::TextLayerPlan,
    pub font_synthesis: Option<FontSynthesisBinding>,
}

fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", message)
}
fn bounded_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

pub(crate) fn validate_canvas_request(
    snapshot: &EditorSnapshot,
    request: &TextCanvasPlanRequest,
) -> Result<(), EditorError> {
    if ![
        &request.project_id,
        &request.revision,
        &request.sequence_id,
        &request.element_id,
    ]
    .into_iter()
    .all(|id| bounded_id(id))
        || opencut_time::RationalTime::try_from(&request.time)?.ticks() < 0
    {
        return Err(invalid(
            "Canvas text requires bounded fixed identities and nonnegative exact time",
        ));
    }
    if snapshot.project.id != request.project_id || snapshot.revision != request.revision {
        return Err(invalid(
            "Canvas text snapshot differs from the requested fixed revision",
        ));
    }
    let sequence = snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == request.sequence_id)
        .ok_or_else(|| EditorError::new("NOT_FOUND", "Canvas text scene is missing"))?;
    let (track, element) = sequence
        .tracks
        .iter()
        .find_map(|track| {
            track
                .elements
                .iter()
                .find(|element| element.id == request.element_id)
                .map(|element| (track, element))
        })
        .ok_or_else(|| EditorError::new("NOT_FOUND", "Canvas text element is missing"))?;
    if track.locked {
        return Err(EditorError::new(
            "TRACK_LOCKED",
            "Canvas text track is locked",
        ));
    }
    if !matches!(
        element.content,
        opencut_contracts::scene::ElementContent::Text { .. }
    ) {
        return Err(invalid("Canvas editing requires a free-text element"));
    }
    Ok(())
}

pub(crate) fn sample_canvas_graph(
    snapshot: &EditorSnapshot,
    request: &TextCanvasPlanRequest,
) -> Result<SceneComposition, EditorError> {
    // Sample the authorized complete source exactly once. Canvas input observes
    // one target, so unrelated captions/graphics need not be executable by the
    // free-text frame backend. This is an execution partition of sampled IR,
    // never a rewritten/projected source document or another scene clock.
    let sampled = compile_frame_scene(
        snapshot,
        &request.sequence_id,
        &request.time,
        CompositionScope::Visual,
    )
    .map_err(|error| EditorError::new(error.code, error.message))?;
    if !sampled
        .layers
        .iter()
        .any(|layer| layer.element_id == request.element_id)
    {
        return Err(EditorError::new(
            "NOT_FOUND",
            "Canvas text is inactive at the requested exact time",
        ));
    }
    Ok(sampled)
}

pub(crate) fn prepare_canvas_partition(
    mut sampled: SceneComposition,
    request: &TextCanvasPlanRequest,
) -> Result<PreparedTextFrame, EditorError> {
    // This operation observes a text input layout, before downstream compositing.
    // Preserve the sampled transform/opacity and reject transitions in the pure
    // preparer; blend/effect stacks do not change the intrinsic glyph geometry.
    if let Some(style) = sampled
        .layers
        .iter_mut()
        .find(|layer| layer.element_id == request.element_id)
        .and_then(|layer| layer.visual.as_mut())
    {
        style.blend_mode = opencut_contracts::scene::BlendMode::Normal;
        style.effects.clear();
        style.masks.clear();
    }
    let target = prepare_text_raster(&sampled, &request.element_id)
        .map_err(|error| EditorError::new(error.code, error.message))?;
    let mut partition = sampled;
    partition
        .layers
        .retain(|layer| layer.element_id == request.element_id);
    partition.fonts.retain(|font| font.id == target.font.id);
    partition.resources.clear();
    partition.transitions.clear();
    let prepared = prepare_text_frame(&partition)
        .map_err(|error| EditorError::new(error.code, error.message))?;
    Ok(prepared)
}

/// Fixed font capture and coverage prepare the same text execution facts.
/// The native measurement port supplies glyph metrics after these checks.
fn canvas_layout(
    prepared: &PreparedTextFrame,
    request: &TextCanvasPlanRequest,
    policy: &LocalVisualExecutionPolicy,
    coverage: &[NativeTextCoverage],
) -> Result<TextCanvasFacts, EditorError> {
    let plan = ClassicTextFrameSession::new(prepared, policy, coverage)?.typed_plan()?;
    if plan.identity.project_id != request.project_id
        || plan.identity.revision != request.revision
        || plan.identity.sequence_id != request.sequence_id
        || plan.identity.time != request.time
    {
        return Err(invalid(
            "Canvas plan differs from the requested fixed scope",
        ));
    }
    let layer = plan
        .texts
        .into_iter()
        .find(|layer| layer.element_id == request.element_id)
        .ok_or_else(|| EditorError::new("NOT_FOUND", "Canvas text is inactive"))?;
    Ok(TextCanvasFacts {
        font_synthesis: None,
        identity: plan.identity,
        layer,
    })
}

#[derive(Clone, Debug)]
pub struct PreparedCanvasPlan {
    execution: PreparedTextFrame,
    font_synthesis: Option<FontSynthesisBinding>,
}
impl PreparedCanvasPlan {
    pub fn execution(&self) -> &PreparedTextFrame {
        &self.execution
    }
    pub fn font_synthesis(&self) -> Option<&FontSynthesisBinding> {
        self.font_synthesis.as_ref()
    }
}
pub fn prepare_canvas_plan(
    snapshot: &EditorSnapshot,
    request: &TextCanvasPlanRequest,
) -> Result<PreparedCanvasPlan, EditorError> {
    validate_canvas_request(snapshot, request)?;
    // A new consumer must not accept a caller-forged provider/hash, including
    // unused resources that sampling will intentionally omit.
    for font in &snapshot.project.fonts {
        binding_for_resource(font)?;
    }
    let mut sampled = sample_canvas_graph(snapshot, request)?;
    let target = sampled
        .layers
        .iter()
        .find(|layer| layer.element_id == request.element_id)
        .ok_or_else(|| invalid("Canvas text target is missing"))?;
    let SampledContent::Text { text } = &target.content else {
        return Err(invalid("Canvas requires free text"));
    };
    let FontBinding::Locked { font_id } = &text.font else {
        return Err(invalid("Canvas requires a locked font"));
    };
    let font_id = font_id.clone();
    let actual = sampled
        .fonts
        .iter()
        .find(|font| font.id == font_id)
        .ok_or_else(|| invalid("Canvas font is missing from the fixed graph"))?;
    let font_synthesis = binding_for_resource(actual)?;
    if let Some(binding) = &font_synthesis {
        let weight = match text.font_weight {
            FontWeight::Normal => 400,
            FontWeight::Bold => 700,
        };
        if weight != binding.requested.weight || text.font_style != binding.requested.style {
            return Err(invalid(
                "Canvas text request differs from the explicit synthesis binding",
            ));
        }
    }
    // Isolate only the observed target, preserving its transition facts until
    // the original pure preparer has rejected any unsupported target transition.
    // The source snapshot and sampled authored request are never persisted back.
    sampled
        .layers
        .retain(|layer| layer.element_id == request.element_id);
    sampled.fonts.retain(|font| font.id == font_id);
    if let Some(binding) = &font_synthesis {
        let SampledContent::Text { text } = &mut sampled.layers[0].content else {
            unreachable!()
        };
        text.font_style = binding.actual.style;
        sampled.fonts[0].origin = ResourceOrigin::Generated {
            provider: "opencut-canvas-execution".into(),
            generation_id: binding.binding_id.clone(),
        };
    }
    let execution = prepare_canvas_partition(sampled, request)?;
    Ok(PreparedCanvasPlan {
        execution,
        font_synthesis,
    })
}

pub fn canvas_plan_result(
    prepared: &PreparedCanvasPlan,
    request: &TextCanvasPlanRequest,
    policy: &LocalVisualExecutionPolicy,
    coverage: &[NativeTextCoverage],
) -> Result<TextCanvasFacts, EditorError> {
    let mut actual = canvas_layout(&prepared.execution, request, policy, coverage)?;
    if let Some(binding) = &prepared.font_synthesis {
        let font = &actual.layer.font;
        if font.content_hash != binding.actual.content_hash
            || font.size != binding.actual.size_bytes
            || font.weight != binding.actual.weight
            || font.style != binding.actual.style
            || font.face_index != 0
        {
            return Err(invalid(
                "Canvas result differs from the bound actual source",
            ));
        }
    }
    actual.identity.profile = CLASSIC_TEXT_CANVAS_PROFILE.into();
    actual.font_synthesis = prepared.font_synthesis.clone();
    Ok(actual)
}

/// Bind CPU-only metrics through the same strict layout validator used by
/// rendered text. The public result contains completed native evidence only.
pub fn complete_canvas_plan(
    prepared: &PreparedCanvasPlan,
    facts: TextCanvasFacts,
    raster: opencut_contracts::text_frame::TextRasterReceipt,
    measurement: opencut_contracts::renderer_identity::NativeTextRuntimeIdentity,
) -> Result<TextCanvasPlanResult, EditorError> {
    let frame = prepared.execution();
    if raster.width != frame.identity().width
        || raster.height != frame.identity().height
        || frame.texts().len() != 1
        || raster.element_id != facts.layer.element_id
    {
        return Err(invalid(
            "Canvas measurement differs from its prepared target",
        ));
    }
    crate::classic_text_frame::validate_layout(&frame.texts()[0], &raster)?;
    crate::classic_visual::validate_text_identity(&measurement)?;
    Ok(TextCanvasPlanResult {
        identity: facts.identity,
        layer: facts.layer,
        font_synthesis: facts.font_synthesis,
        layout: raster.layout,
        measurement,
    })
}

#[cfg(test)]
pub(crate) mod tests;
