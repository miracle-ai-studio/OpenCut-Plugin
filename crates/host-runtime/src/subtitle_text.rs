//! Native assembly for read-only subtitle authoring. Public apply owns writes.
use super::*;
use opencut_contracts::{project_public::ApplyProject, subtitle_text::*};
use opencut_editor_application::{
    classic_text_frame::{ClassicTextFrameSession, admit_text_frame},
    font_import::FontImportPolicy,
    scene::{SceneCommitIntent, SceneRead},
    scene_text_frame_jobs::TextFrameJobSubmissionStore,
};
use opencut_native_services::classic_visual::VerifiedClassicTextResources;

pub(super) fn require_head(
    host: &Automation,
    project_id: &str,
    expected_revision: &str,
    idempotency_key: &str,
) -> Result<(), EditorError> {
    let intent = ApplyProject {
        branch_name: None,
        project_id: project_id.into(),
        expected_revision: expected_revision.into(),
        idempotency_key: idempotency_key.into(),
        label: "Create editable subtitles".into(),
        operations: vec![],
    };
    host.app
        .authorize_project(SceneCommitIntent::PublicApply(&intent))?;
    let head = host.app.head_scene_source(project_id)?;
    if head.revision != expected_revision {
        return Err(EditorError::new(
            "REVISION_CONFLICT",
            "Subtitle authoring requires its original V4 head",
        ));
    }
    Ok(())
}

pub(super) fn plan(
    host: &Automation,
    input: &SubtitleTextPlanRequest,
) -> Result<SubtitleTextPlanResult, EditorError> {
    let started = std::time::Instant::now();
    require_head(
        host,
        &input.project_id,
        &input.expected_revision,
        &input.idempotency_key,
    )?;
    let selected = host
        .app
        .fixed_scene_text_frame_snapshot(&input.project_id, &input.expected_revision)?;
    let transcript = host.app.subtitle_text_transcript(input)?;
    let checkpoint = || {
        require_head(
            host,
            &input.project_id,
            &input.expected_revision,
            &input.idempotency_key,
        )?;
        let current = host
            .app
            .fixed_scene_text_frame_snapshot(&input.project_id, &input.expected_revision)?;
        if current.processing_policy != selected.processing_policy {
            return Err(EditorError::new(
                "PERMISSION_DENIED",
                "Subtitle execution policy changed",
            ));
        }
        if started.elapsed().as_millis() > u128::from(selected.processing_policy.deadline_ms) {
            return Err(EditorError::new(
                "VISUAL_BUDGET_EXCEEDED",
                "Subtitle preparation exceeded its deadline",
            ));
        }
        Ok(())
    };
    let mut measured_request =
        opencut_editor_application::subtitle_text::SubtitleTextPlanRequest::from(input.clone());
    let prepared = opencut_editor_application::subtitle_text::prepare_with_checkpoint(
        &selected.snapshot,
        &measured_request,
        transcript.as_ref(),
        &mut || checkpoint(),
    )?;
    let mut layers = Vec::with_capacity(prepared.frames().len());
    let mut metrics: Option<opencut_native_services::native_visual::NativeTextMeasurements> = None;
    let mut observations = Vec::with_capacity(prepared.frames().len());
    for (index, frame) in prepared.frames().iter().enumerate() {
        checkpoint()?;
        admit_text_frame(frame, &selected.processing_policy)?;
        let mut resources = VerifiedClassicTextResources::new_subtitle_authoring_fonts(
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
                "Subtitle measurement must have one target",
            ));
        }
        let synthesis = opencut_editor_application::font_synthesis::binding_for_resource(
            &frame.texts()[0].font,
        )?;
        let directive = prepared.measurement_directive(index)?;
        let placement = None;
        let options = opencut_native_services::native_visual::NativeTextMeasureOptions::Caption {
            directive,
            placement,
            synthesis,
        };
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
        observations.push(opencut_contracts::subtitle_text::SubtitleTextObservation {
            caption: measured.caption.ok_or_else(|| {
                EditorError::new(
                    "INVALID_VISUAL_PLAN",
                    "Native caption omitted its wrap observation",
                )
            })?,
            raster: measured.raster,
        });
        layers.extend(plan.texts);
    }
    let measurement = metrics
        .map(|metrics| metrics.finish(&mut || checkpoint()))
        .transpose()?;
    if measurement.is_some() {
        measured_request.observations = Some(observations);
    }
    let mut result =
        opencut_editor_application::subtitle_text::finish(&prepared, &measured_request, &layers)?;
    result.measurement = measurement;
    // Original transcript authority can be revoked independently of project edit.
    host.app.subtitle_text_transcript(input)?;
    checkpoint()?;
    Ok(result)
}

pub(super) fn export(
    host: &Automation,
    input: &SubtitleTextExport,
) -> Result<opencut_api_contracts::subtitle_exchange::SubtitleExportReceipt, EditorError> {
    let fixed = opencut_editor_application::scene::inspect(
        &host.app,
        &opencut_contracts::project_inspection::InspectProject {
            project_id: input.project_id.clone(),
            revision: Some(input.revision.clone()),
        },
    )?;
    opencut_editor_application::subtitle_text_export::export(&fixed, input)
}
