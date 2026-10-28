//! Host assembly for pure ASS plans with authorized pinned-font observations.
use super::*;
use opencut_contracts::subtitle_ass::*;
use opencut_editor_application::{
    classic_text_frame::{ClassicTextFrameSession, admit_text_frame},
    font_import::FontImportPolicy,
    scene_text_frame_jobs::TextFrameJobSubmissionStore,
};
use opencut_native_services::classic_visual::VerifiedClassicTextResources;

pub(super) fn plan(
    host: &Automation,
    input: &SubtitleAssPlanRequest,
) -> Result<SubtitleAssPlanResult, EditorError> {
    let started = std::time::Instant::now();
    let head = || {
        super::subtitle_text::require_head(
            host,
            &input.project_id,
            &input.expected_revision,
            &input.idempotency_key,
        )
    };
    head()?;
    let selected = host
        .app
        .fixed_scene_text_frame_snapshot(&input.project_id, &input.expected_revision)?;
    let checkpoint = || {
        head()?;
        let current = host
            .app
            .fixed_scene_text_frame_snapshot(&input.project_id, &input.expected_revision)?;
        if current.processing_policy != selected.processing_policy {
            return Err(EditorError::new(
                "PERMISSION_DENIED",
                "ASS execution policy changed",
            ));
        }
        if started.elapsed().as_millis() > u128::from(selected.processing_policy.deadline_ms) {
            return Err(EditorError::new(
                "VISUAL_BUDGET_EXCEEDED",
                "ASS preparation exceeded its deadline",
            ));
        }
        Ok(())
    };
    let mut measured_request =
        opencut_editor_application::subtitle_ass::SubtitleAssPlanRequest::from(input.clone());
    let prepared = opencut_editor_application::subtitle_ass::prepare_with_checkpoint(
        &selected.snapshot,
        &measured_request,
        &mut || checkpoint(),
    )?;
    let mut layers = Vec::with_capacity(prepared.frames().len());
    let mut metrics: Option<opencut_native_services::native_visual::NativeTextMeasurements> = None;
    let mut observations = Vec::with_capacity(prepared.frames().len());
    for (index, frame) in prepared.frames().iter().enumerate() {
        checkpoint()?;
        admit_text_frame(frame, &selected.processing_policy)?;
        let mut resources = VerifiedClassicTextResources::new_fonts(
            host.app.asset_store_root(),
            frame,
            &FontImportPolicy::default(),
            &checkpoint,
        )?;
        let mut session =
            ClassicTextFrameSession::new(frame, &selected.processing_policy, resources.coverage())?;
        let plan = session.typed_plan()?;
        if plan.texts.len() != 1 {
            return Err(EditorError::new(
                "INVALID_VISUAL_PLAN",
                "ASS measurement must have one target",
            ));
        }
        let synthesis = opencut_editor_application::font_synthesis::binding_for_resource(
            &frame.texts()[0].font,
        )?;
        let _ = index;
        let options =
            opencut_native_services::native_visual::NativeTextMeasureOptions::Plain { synthesis };
        if metrics.is_none() {
            metrics = Some(
                opencut_native_services::native_visual::NativeTextMeasurements::new(
                    crate::classic_visual_runtime::configured_text(&selected.processing_policy)?,
                    &mut || checkpoint(),
                )?,
            );
        }
        let fonts = resources.font_handles();
        let measured = metrics
            .as_mut()
            .expect("native text measurement owner")
            .measure(&mut session, &mut resources, &fonts, &options)?;
        observations.push(measured.raster);
        layers.extend(plan.texts);
    }
    let measurement = metrics
        .map(|metrics| metrics.finish(&mut || checkpoint()))
        .transpose()?;
    if measurement.is_some() {
        measured_request.observations = Some(observations);
    }
    let mut result =
        opencut_editor_application::subtitle_ass::finish(&prepared, &measured_request, &layers)?;
    result.measurement = measurement;
    checkpoint()?;
    Ok(result)
}
