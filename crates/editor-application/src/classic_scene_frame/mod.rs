//! Versioned Classic scene stacks reuse fixed-source blend sessions.
pub use opencut_api_contracts::scene_frame_plan::SceneFramePlan;
use opencut_composition::scene::*;
use opencut_contracts::text_frame::{TextFontResource, ValidatedTextFont};
use opencut_contracts::{EditorError, blend_frame::BoundBlendFrame, scene_frame::*};
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
mod session;
pub use session::*;
#[cfg(test)]
mod synthesis_tests;
fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_VISUAL_PLAN", message)
}
pub(crate) fn scene_execution_policy(
    prepared: &PreparedSceneFrame,
    policy: &LocalVisualExecutionPolicy,
) -> Result<LocalVisualExecutionPolicy, EditorError> {
    crate::media_frame_jobs::validate_policy(policy)?;
    let text = prepared.execution().execution().execution();
    let mut resources: BTreeMap<&str, u64> = text
        .media()
        .resources()
        .iter()
        .map(|r| {
            (
                r.asset_id.as_str(),
                if r.kind == opencut_contracts::AssetKind::Video {
                    0
                } else {
                    r.version.size
                },
            )
        })
        .collect();
    let mut fonts: BTreeMap<&str, u64> = text
        .texts()
        .iter()
        .map(|p| (p.font.id.as_str(), p.font.content.size))
        .collect();
    let original_fonts: BTreeSet<_> = fonts.keys().copied().collect();
    for mask in prepared.mask_texts() {
        if mask.font.content.size == 0
            || mask.font.content.size > crate::font_import::MAX_FONT_BYTES
        {
            return Err(invalid("Scene mask font exceeds static face admission"));
        }
        if let Some(previous) = fonts.insert(mask.font.id.as_str(), mask.font.content.size) {
            if previous != mask.font.content.size {
                return Err(invalid("Scene font identity changed"));
            }
        }
    }
    for (id, size) in &fonts {
        if resources.insert(id, size.to_owned()).is_some() {
            return Err(invalid("Scene font and media identities collide"));
        }
    }
    let encoded = resources.values().try_fold(0u64, |sum, size| {
        sum.checked_add(*size)
            .ok_or_else(|| invalid("Scene encoded accounting overflow"))
    })?;
    if resources.len() > 32 || encoded > policy.max_encoded_bytes {
        return Err(EditorError::new(
            "VISUAL_BUDGET_EXCEEDED",
            "Scene media and mask fonts exceed original encoded admission",
        ));
    }
    let extra_font_bytes = fonts
        .iter()
        .filter(|(id, _)| !original_fonts.contains(**id))
        .try_fold(0u64, |sum, (_, size)| {
            sum.checked_add(
                size.checked_mul(3)
                    .ok_or_else(|| invalid("Scene font accounting overflow"))?,
            )
            .ok_or_else(|| invalid("Scene font accounting overflow"))
        })?;
    let masks = prepared
        .stacks()
        .iter()
        .flat_map(|s| &s.masks)
        .filter(|m| m.enabled)
        .count() as u64;
    let i = prepared.execution().execution().execution().identity();
    let pixels = u64::from(i.width)
        .checked_mul(u64::from(i.height))
        .ok_or_else(|| invalid("Scene accounting overflow"))?;
    let budget = || {
        EditorError::new(
            "VISUAL_BUDGET_EXCEEDED",
            "Scene stacks exceed the original visual admission",
        )
    };
    // Inert scene stacks allocate no extra CPU Canvas. Output/readback, text
    // rasters and media decode capacity already belong to their child budgets.
    // Keep the existing conservative scratch reserve for active scene work.
    let scratch = masks != 0
        || matches!(prepared.background(), SceneBackground::MainTrackBlur { .. })
        || prepared
            .stacks()
            .iter()
            .any(|stack| stack.effects.iter().any(|effect| effect.enabled && matches!(effect.effect, opencut_contracts::scene::EffectKind::ClassicBlur { .. })))
        || prepared.steps().iter().any(|step| {
            matches!(step,
            SceneStep::Effect { effects, .. } if effects.iter().any(|effect| effect.enabled && matches!(effect.effect, opencut_contracts::scene::EffectKind::ClassicBlur { .. })))
        });
    let cpu = pixels
        .checked_mul(
            masks
                .checked_mul(12)
                .and_then(|n| n.checked_add(if scratch { 16 } else { 0 }))
                .ok_or_else(budget)?,
        )
        .ok_or_else(budget)?;
    // The child visual budget already reserves eight full-canvas GPU textures
    // plus readback (three targets, two effect ping/pong targets, backdrop and
    // preview/capture). Charge only additional mask storage here, once.
    let gpu = pixels
        .checked_mul(masks.checked_mul(48).ok_or_else(budget)?)
        .ok_or_else(budget)?;
    let mut result = policy.clone();
    result.max_encoded_bytes = result
        .max_encoded_bytes
        .checked_sub(extra_font_bytes / 3)
        .ok_or_else(budget)?;
    if text.texts().is_empty() && text.media().resources().is_empty() {
        result.max_encoded_bytes = result.max_encoded_bytes.max(1);
    }
    result.max_rgba_bytes = result
        .max_rgba_bytes
        .checked_sub(cpu.checked_add(extra_font_bytes).ok_or_else(budget)?)
        .ok_or_else(budget)?;
    result.max_gpu_bytes = result.max_gpu_bytes.checked_sub(gpu).ok_or_else(budget)?;
    Ok(result)
}
pub fn admit_scene_frame(
    prepared: &PreparedSceneFrame,
    policy: &LocalVisualExecutionPolicy,
) -> Result<(), EditorError> {
    crate::classic_blend_frame::admit_blend_frame(
        prepared.blend(),
        &scene_execution_policy(prepared, policy)?,
    )
}
pub fn plan_frame(
    prepared: &PreparedSceneFrame,
    policy: &LocalVisualExecutionPolicy,
    coverage: &[NativeTextCoverage],
) -> Result<SceneFramePlan, EditorError> {
    for font in &prepared.source_graph().fonts {
        crate::font_synthesis::binding_for_resource(font)?;
    }
    let font_synthesis = prepared
        .font_synthesis()
        .iter()
        .map(|entry| {
            let binding = crate::font_synthesis::binding_for_resource(&entry.font)?
                .ok_or_else(|| invalid("Scene synthesis owner has no approved binding"))?;
            if binding.requested.weight != entry.requested_weight
                || binding.requested.style != entry.requested_style
            {
                return Err(invalid(
                    "Scene requested typography differs from the approved binding",
                ));
            }
            Ok(SceneFontSynthesis {
                element_id: entry.element_id.clone(),
                owner_id: entry.owner_id.clone(),
                owner_kind: entry.owner_kind,
                font_id: entry.font.id.clone(),
                binding,
            })
        })
        .collect::<Result<Vec<_>, EditorError>>()?;
    admit_scene_frame(prepared, policy)?;
    let title_coverage = title_coverage(prepared, coverage)?;
    let execution =
        crate::classic_blend_frame::plan_frame(prepared.blend(), policy, &title_coverage)?;
    let mut identity = execution.identity.clone();
    identity.profile = CLASSIC_SCENE_FRAME_PROFILE.into();
    Ok(SceneFramePlan {
        identity,
        execution,
        mask_fonts: mask_fonts(prepared),
        font_synthesis,
        stacks: prepared.stacks().to_vec(),
        steps: prepared.steps().to_vec(),
        background: prepared.background().clone(),
    })
}
pub fn bind_scene_frame(
    prepared: &PreparedSceneFrame,
    child: &BoundBlendFrame,
) -> Result<BoundSceneFrame, EditorError> {
    let expected =
        crate::classic_blend_frame::bind_blend_frame(prepared.blend(), &child.execution)?;
    if child != &expected {
        return Err(invalid("Scene child differs from fixed blend binding"));
    }
    opencut_composition::scene::bind_scene_frame(prepared, child)
        .map_err(|e| EditorError::new(e.code, e.message))
}

pub(crate) fn title_coverage(
    prepared: &PreparedSceneFrame,
    coverage: &[NativeTextCoverage],
) -> Result<Vec<NativeTextCoverage>, EditorError> {
    let titles = prepared.execution().execution().execution().texts();
    let mut seen = BTreeSet::new();
    let mut output = vec![];
    for proof in coverage {
        if !seen.insert(proof.element_id.as_str()) {
            return Err(invalid("Scene native coverage repeats an owner"));
        }
        if titles
            .iter()
            .any(|p| p.identity.element_id == proof.element_id)
        {
            output.push(proof.clone());
        } else if let Some(mask) = prepared
            .mask_texts()
            .iter()
            .find(|m| m.mask_id == proof.element_id)
        {
            if proof.font != mask.font || proof.content != mask.mask.content {
                return Err(invalid(
                    "Scene mask coverage differs from fixed font or content",
                ));
            }
        } else {
            return Err(invalid(
                "Scene native coverage contains an unknown text owner",
            ));
        }
    }
    if coverage.len() != titles.len() + prepared.mask_texts().len() {
        return Err(invalid("Scene native font coverage is incomplete"));
    }
    Ok(output)
}
fn mask_fonts(prepared: &PreparedSceneFrame) -> Vec<SceneTextMaskFont> {
    prepared
        .mask_texts()
        .iter()
        .map(|p| SceneTextMaskFont {
            mask_id: p.mask_id.clone(),
            font: ValidatedTextFont {
                font_id: p.font.id.clone(),
                content_hash: p.font.content.content_hash.clone(),
                size: p.font.content.size.to_string(),
                weight: p.font.weight,
                style: p.font.style,
                face_index: p.font.face_index,
                glyph_coverage_validated: true,
                validated_text_hash: format!("{:x}", Sha256::digest(p.mask.content.as_bytes())),
            },
        })
        .collect()
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
