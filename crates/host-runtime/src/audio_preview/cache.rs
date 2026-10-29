//! Assemble application cache scheduling with native runtime and fixed-session authority.
use super::*;
use opencut_api_contracts::visual_preview::*;
use opencut_editor_application::media_video_jobs::MediaVideoJobSubmissionStore;
pub(crate) fn prepare(
    host: &mut Automation,
    input: VisualPreviewCachePrepare,
) -> Result<VisualPreviewCache, EditorError> {
    let e = entry(&host.database, &input.session.session_id)?;
    authorize(host, &e, true)?;
    let checkpoint = || {
        lock(&e.state)?.machine.verify_visual_frame(
            &input.session,
            host.caller.principal_id(),
            now(),
        )
    };
    checkpoint()?;
    let fixed = host
        .app
        .fixed_scene_video_snapshot(&e.request.project_id, &e.request.revision)?;
    let config = crate::classic_visual_runtime::configured_scene(&fixed.processing_policy.visual)?;
    let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
    let ffprobe = crate::media_tool_runtime::ffprobe()?;
    let runtime = opencut_native_services::preview_cache::fingerprint(&config, &ffmpeg, &ffprobe)?;
    let priority_time = lock(&e.state)?.machine.snapshot(now()).presentation_time;
    let result = opencut_editor_application::preview_cache::prepare(
        &mut host.app,
        &fixed.snapshot,
        &e.request.sequence_id,
        &fixed.processing_policy,
        &runtime,
        &priority_time,
        input.reuse_only,
        checkpoint,
    )?;
    authorize(host, &e, true)?;
    let mut state = lock(&e.state)?;
    state
        .machine
        .verify_visual_frame(&input.session, host.caller.principal_id(), now())?;
    state.cache_plan = Some(result.clone());
    Ok(result)
}

pub(crate) fn preferred_jobs(
    database: &Path,
    app: &opencut_sqlite_store::EditorApplication,
) -> Result<Vec<String>, EditorError> {
    let entries: Vec<_> = lock(registries())?
        .get(&database_key(database)?)
        .map(|r| r.entries.values().cloned().collect())
        .unwrap_or_default();
    let mut jobs = Vec::new();
    for entry in entries {
        if entry.checkpoint(app).is_err() {
            continue;
        }
        let state = lock(&entry.state)?;
        let view = state.machine.snapshot(now());
        if view.state != AudioPreviewState::Playing {
            continue;
        }
        if let Some(plan) = &state.cache_plan {
            jobs.extend(opencut_editor_application::preview_cache::preferred_jobs(
                plan,
                &view.presentation_time,
            )?);
        }
        if let Some(plan) = &state.source_plan {
            jobs.extend(plan.jobs.iter().map(|job| job.job_id.clone()));
        }
    }
    jobs.sort();
    jobs.dedup();
    Ok(jobs)
}
