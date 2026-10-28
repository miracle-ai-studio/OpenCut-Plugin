//! Canvas assembly reuses the fixed-snapshot font inspection and live fences.
use super::*;
use opencut_contracts::text_canvas::{TextCanvasPlanRequest, TextCanvasPlanResult};
use opencut_editor_application::{
    classic_text_frame::admit_text_frame,
    font_import::FontImportPolicy,
    scene_text_frame_jobs::TextFrameJobSubmissionStore,
    text_canvas::{canvas_plan_result, prepare_canvas_plan},
};
use opencut_native_services::classic_visual::VerifiedClassicTextResources;

pub(super) fn plan(
    host: &Automation,
    request: &TextCanvasPlanRequest,
) -> Result<TextCanvasPlanResult, EditorError> {
    let selected = host
        .app
        .fixed_scene_text_frame_snapshot(&request.project_id, &request.revision)?;
    let prepared = prepare_canvas_plan(&selected.snapshot, request)?;
    admit_text_frame(prepared.execution(), &selected.processing_policy)?;
    let started = std::time::Instant::now();
    let checkpoint = || {
        let current = host
            .app
            .fixed_scene_text_frame_snapshot(&request.project_id, &request.revision)?;
        if current.processing_policy != selected.processing_policy {
            return Err(EditorError::new(
                "PERMISSION_DENIED",
                "Canvas execution policy changed",
            ));
        }
        if started.elapsed().as_millis() > u128::from(selected.processing_policy.deadline_ms) {
            return Err(EditorError::new(
                "VISUAL_BUDGET_EXCEEDED",
                "Canvas font inspection exceeded its deadline",
            ));
        }
        Ok(())
    };
    let mut resources = VerifiedClassicTextResources::new_fonts(
        host.app.asset_store_root(),
        prepared.execution(),
        &FontImportPolicy::default(),
        &checkpoint,
    )?;
    let facts = canvas_plan_result(
        &prepared,
        request,
        &selected.processing_policy,
        resources.coverage(),
    )?;
    let mut session = opencut_editor_application::classic_text_frame::ClassicTextFrameSession::new(
        prepared.execution(),
        &selected.processing_policy,
        resources.coverage(),
    )?;
    let options = opencut_native_services::native_visual::NativeTextMeasureOptions::Plain {
        synthesis: prepared.font_synthesis().cloned(),
    };
    let fonts = resources.font_handles();
    let mut metrics = opencut_native_services::native_visual::NativeTextMeasurements::new(
        crate::classic_visual_runtime::configured_text(&selected.processing_policy)?,
        &mut || checkpoint(),
    )?;
    let measured = metrics.measure(&mut session, &mut resources, &fonts, &options)?;
    let identity = metrics.finish(&mut || checkpoint())?;
    let result = opencut_editor_application::text_canvas::complete_canvas_plan(
        &prepared,
        facts,
        measured.raster,
        identity,
    )?;
    checkpoint()?;
    Ok(result)
}
