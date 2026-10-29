//! Shared trusted fixed-revision job authorization; policy remains typed per executor.
use super::*;
use opencut_contracts::scene::EditorSnapshot;
use opencut_editor_application::{
    access::{Caller, VerifiedCredential},
    audit::ExecutionContext,
    scene::StoredProjectDocument,
};
use opencut_job_execution::scene_audio::TrustedJobAuthority;
pub(super) fn authority(context: &ExecutionContext) -> TrustedJobAuthority {
    match context.caller() {
        Caller::LocalOwner => TrustedJobAuthority::LocalOwner,
        Caller::Credential(c) => TrustedJobAuthority::Credential {
            principal_id: c.principal_id().into(),
            credential_id: c.credential_id().into(),
        },
    }
}
pub(super) fn context(value: &TrustedJobAuthority) -> ExecutionContext {
    ExecutionContext::for_caller(
        opencut_editor_application::audit::CallSource::Worker,
        match value {
            TrustedJobAuthority::LocalOwner => Caller::LocalOwner,
            TrustedJobAuthority::Credential {
                principal_id,
                credential_id,
            } => Caller::Credential(VerifiedCredential::from_verified(
                principal_id.clone(),
                credential_id.clone(),
            )),
        },
    )
}
pub(super) fn fixed(
    connection: &Connection,
    project: &str,
    revision: &str,
) -> Result<EditorSnapshot, EditorError> {
    let stored = sqlite_scene::load_source(connection, project, Some(revision))?;
    let StoredProjectDocument::Editable(document) = stored.document else {
        return Err(EditorError::new(
            "PROJECT_PROMOTION_REQUIRED",
            "Promote this historical revision before submitting new renders",
        ));
    };
    Ok(EditorSnapshot {
        project: *document,
        revision: stored.revision,
    })
}
pub(super) fn require_scope(
    connection: &Connection,
    caller: &ExecutionContext,
    project: &str,
) -> Result<(), EditorError> {
    sqlite_access::require_execution_scope(connection, caller, project, None).map(|_| ())
}
/// A retained job keeps its exact, stricter pre-font-reserve policy. Accept only
/// that complete historical policy or the complete current policy, never an
/// arbitrary caller-selected subset or a change to another resource limit.
pub(super) fn matches_policy(
    captured: &impl serde::Serialize,
    current: &impl serde::Serialize,
) -> Result<bool, EditorError> {
    let captured = serde_json::to_value(captured).map_err(|_| access::denied())?;
    let current = serde_json::to_value(current).map_err(|_| access::denied())?;
    if captured == current {
        return Ok(true);
    }
    let text = serde_json::to_value(sqlite_scene_text_frame_jobs::policy())
        .map_err(|_| access::denied())?;
    let base =
        serde_json::to_value(sqlite_media_frame_jobs::policy()).map_err(|_| access::denied())?;
    let mut old_base = base.clone();
    old_base["max_rgba_bytes"] = serde_json::json!(64u64 * 1024 * 1024);
    let mut old_text = text.clone();
    old_text["max_rgba_bytes"] = serde_json::json!(160u64 * 1024 * 1024);
    let visual_policies = [&base, &text, &old_base, &old_text];
    let matches_visual = |candidate: &Value| {
        if candidate == &text {
            return visual_policies.iter().any(|policy| captured == **policy);
        }
        if candidate == &base {
            return captured == old_base;
        }
        if candidate.get("visual") == Some(&text) || candidate.get("visual") == Some(&base) {
            let policies: &[&Value] = if candidate.get("visual") == Some(&text) {
                &visual_policies
            } else {
                &[&base, &old_base]
            };
            return policies.iter().any(|policy| {
                let mut exact = candidate.clone();
                exact["visual"] = (*policy).clone();
                captured == exact
            });
        }
        false
    };
    if matches_visual(&current) {
        return Ok(true);
    }
    // Exact retained video policies predate progress-based lifetime. Preserve
    // their original total deadline and fixed quotas;never infer a looser
    // policy from a subset of matching fields or rewrite the stored input.
    if current.get("stalled_after_ms") == Some(&serde_json::json!(300_000))
        && current.get("deadline_ms").is_none()
        && current.get("max_frames")
            == Some(&serde_json::json!(
                opencut_job_execution::media_video::MAX_VIDEO_FRAMES
            ))
        && current.get("max_output_bytes") == Some(&serde_json::json!(i64::MAX as u64))
    {
        let mut historical = current.clone();
        let fields = historical.as_object_mut().ok_or_else(access::denied)?;
        fields.remove("stalled_after_ms");
        fields.insert("deadline_ms".into(), serde_json::json!(300_000));
        fields.insert("max_frames".into(), serde_json::json!(1800));
        fields.insert(
            "max_output_bytes".into(),
            serde_json::json!(256u64 * 1024 * 1024),
        );
        if captured == historical {
            return Ok(true);
        }
        if matches_visual(&historical) {
            return Ok(true);
        }
    }
    Ok(false)
}

// One fixed input per application/worker connection, capped independently of
// project size. Larger inputs still take the same uncached validation path.
// This stores immutable facts only, never a successful lease/access decision.
const MAX_FIXED_JOB_INPUT_BYTES: usize = 1024 * 1024;
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub(super) enum CurrentPolicy {
    Visual(opencut_job_execution::media_frame::LocalVisualExecutionPolicy),
    Audio(opencut_job_execution::scene_audio::LocalAudioExecutionPolicy),
    Video(opencut_job_execution::media_video::LocalVideoExecutionPolicy),
    SourceArt(opencut_editor_application::source_art::SourceArtPolicy),
    SourcePreview(opencut_job_execution::source_preview::SourcePreviewPolicy),
}
impl From<&opencut_job_execution::media_frame::LocalVisualExecutionPolicy> for CurrentPolicy {
    fn from(value: &opencut_job_execution::media_frame::LocalVisualExecutionPolicy) -> Self {
        Self::Visual(value.clone())
    }
}
impl From<&opencut_job_execution::scene_audio::LocalAudioExecutionPolicy> for CurrentPolicy {
    fn from(value: &opencut_job_execution::scene_audio::LocalAudioExecutionPolicy) -> Self {
        Self::Audio(value.clone())
    }
}
impl From<&opencut_job_execution::media_video::LocalVideoExecutionPolicy> for CurrentPolicy {
    fn from(value: &opencut_job_execution::media_video::LocalVideoExecutionPolicy) -> Self {
        Self::Video(value.clone())
    }
}
impl From<&opencut_editor_application::source_art::SourceArtPolicy> for CurrentPolicy {
    fn from(value: &opencut_editor_application::source_art::SourceArtPolicy) -> Self {
        Self::SourceArt(value.clone())
    }
}
impl From<&opencut_job_execution::source_preview::SourcePreviewPolicy> for CurrentPolicy {
    fn from(value: &opencut_job_execution::source_preview::SourcePreviewPolicy) -> Self {
        Self::SourcePreview(value.clone())
    }
}
pub(super) struct FixedJobInput {
    job_id: String,
    kind: String,
    input: String,
    project: String,
    revision: String,
    authority: TrustedJobAuthority,
    current_policy: CurrentPolicy,
    processing: Value,
}
struct LiveJob {
    project: String,
    revision: String,
    state: String,
    attempt: u32,
    owner: Option<String>,
    until: Option<i64>,
    cancelled: bool,
}
fn live_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<LiveJob> {
    Ok(LiveJob {
        project: row.get(0)?,
        revision: row.get(1)?,
        state: row.get(2)?,
        attempt: row.get(3)?,
        owner: row.get(4)?,
        until: row.get(5)?,
        cancelled: row.get(6)?,
    })
}
fn require_live(job: &LiveJob, worker: &str, attempt: u32) -> Result<(), EditorError> {
    let now = lifecycle::JobClock::now_ms(&sqlite_jobs::SystemClock)?;
    if job.state != "running"
        || job.attempt != attempt
        || job.owner.as_deref() != Some(worker)
        || job.until.is_none_or(|until| until <= now)
    {
        return Err(EditorError::new(
            "JOB_LEASE_LOST",
            "V4 render attempt is no longer current",
        ));
    }
    if job.cancelled {
        return Err(EditorError::new(
            "JOB_CANCELLED",
            "V4 render task was cancelled",
        ));
    }
    Ok(())
}
pub(super) fn revalidate(
    app: &EditorApplication,
    kind: &str,
    job_id: &str,
    worker: &str,
    attempt: u32,
    selected_authority: &TrustedJobAuthority,
    policy: impl Into<CurrentPolicy>,
) -> Result<(), EditorError> {
    let tx = app
        .connection
        .unchecked_transaction()
        .map_err(storage_error)?;
    // Compare every typed field before materializing JSON. No lease, caller,
    // access, input or publication result is memoized by this immutable fact.
    let current_policy = policy.into();
    let mut fixed = app.scene_job_input.borrow_mut();
    if let Some(prior) = fixed
        .as_mut()
        .filter(|p| p.job_id == job_id && p.kind == kind)
    {
        if &prior.authority != selected_authority {
            return Err(access::denied());
        }
        if prior.current_policy != current_policy {
            let json = serde_json::to_value(&current_policy).map_err(|_| access::denied())?;
            if !matches_policy(&prior.processing, &json)? {
                return Err(access::denied());
            }
        }
        // Read all mutable fences in a fresh transaction. SQL compares the
        // entire original TEXT, not a revision/hash/mtime approximation. Any
        // input or target rewrite fails closed instead of reusing its facts.
        let (job,unchanged):(LiveJob,bool)=tx.prepare_cached(
            "SELECT p.project_id,p.revision,j.state,j.attempt,j.lease_owner,j.lease_until,j.cancel_requested,j.input_json=?3 FROM jobs j JOIN job_projects p USING(job_id) JOIN revisions r ON r.project_id=p.project_id AND r.revision=p.revision WHERE j.job_id=?1 AND j.kind=?2"
        ).map_err(storage_error)?.query_row(params![job_id,kind,&prior.input],|row|Ok((live_job(row)?,row.get(7)?))).map_err(storage_error)?;
        require_live(&job, worker, attempt)?;
        if !unchanged || job.project != prior.project || job.revision != prior.revision {
            return Err(access::denied());
        }
        require_scope(&tx, &context(selected_authority), &job.project)?;
        prior.current_policy = current_policy;
        return Ok(());
    }
    let (job,authority,processing,input):(LiveJob,String,String,String)=tx.prepare_cached(
        "SELECT p.project_id,p.revision,j.state,j.attempt,j.lease_owner,j.lease_until,j.cancel_requested,json_extract(j.input_json,CASE WHEN j.kind IN ('render.export_video.v2','render.export_video_range.v1') THEN '$.execution.authority' ELSE '$.authority' END),json_extract(j.input_json,CASE WHEN j.kind IN ('render.export_video.v2','render.export_video_range.v1') THEN '$.execution.processing_policy' ELSE '$.processing_policy' END),j.input_json FROM jobs j JOIN job_projects p USING(job_id) JOIN revisions r ON r.project_id=p.project_id AND r.revision=p.revision WHERE j.job_id=?1 AND j.kind=?2"
    ).map_err(storage_error)?.query_row(params![job_id,kind],|row|Ok((live_job(row)?,row.get(7)?,row.get(8)?,row.get(9)?))).map_err(storage_error)?;
    require_live(&job, worker, attempt)?;
    let original: TrustedJobAuthority = decode_json(&authority, "V4 authority")?;
    let processing: Value = decode_json(&processing, "V4 processing policy")?;
    let policy_json = serde_json::to_value(&current_policy).map_err(|_| access::denied())?;
    if &original != selected_authority || !matches_policy(&processing, &policy_json)? {
        return Err(access::denied());
    }
    require_scope(&tx, &context(&original), &job.project)?;
    if input.len() <= MAX_FIXED_JOB_INPUT_BYTES {
        *fixed = Some(FixedJobInput {
            job_id: job_id.into(),
            kind: kind.into(),
            input,
            project: job.project,
            revision: job.revision,
            authority: original,
            current_policy,
            processing,
        });
    } else {
        *fixed = None;
    }
    Ok(())
}

#[cfg(test)]
mod policy_continuity_tests {
    use super::*;

    #[test]
    fn typed_current_policies_keep_exact_existing_json_and_all_fields() {
        let visual = sqlite_scene_text_frame_jobs::policy();
        let audio = sqlite_scene_audio_jobs::policy();
        let video = sqlite_media_video_jobs::policy();
        let source_art = sqlite_source_art::policy();
        assert_eq!(
            serde_json::to_value(CurrentPolicy::from(&source_art)).unwrap(),
            serde_json::to_value(&source_art).unwrap()
        );
        let mut changed = source_art.clone();
        changed.max_encoded_bytes -= 1;
        assert!(CurrentPolicy::from(&source_art) != CurrentPolicy::from(&changed));
        assert!(!matches_policy(&source_art, &CurrentPolicy::from(&changed)).unwrap());
        assert_eq!(
            serde_json::to_value(CurrentPolicy::from(&visual)).unwrap(),
            serde_json::to_value(&visual).unwrap()
        );
        assert_eq!(
            serde_json::to_value(CurrentPolicy::from(&audio)).unwrap(),
            serde_json::to_value(&audio).unwrap()
        );
        assert_eq!(
            serde_json::to_value(CurrentPolicy::from(&video)).unwrap(),
            serde_json::to_value(&video).unwrap()
        );
        let mut changed = video.clone();
        changed.audio.max_pcm_bytes -= 1;
        assert!(CurrentPolicy::from(&video) != CurrentPolicy::from(&changed));
        assert!(!matches_policy(&video, &CurrentPolicy::from(&changed)).unwrap());
        let mut changed = video.clone();
        changed.visual.max_gpu_bytes -= 1;
        assert!(CurrentPolicy::from(&video) != CurrentPolicy::from(&changed));
        assert!(!matches_policy(&video, &CurrentPolicy::from(&changed)).unwrap());
    }

    #[test]
    fn font_reserve_preserves_only_exact_historical_visual_policies() {
        let current = sqlite_scene_text_frame_jobs::policy();
        let legacy = sqlite_media_frame_jobs::policy();
        assert!(matches_policy(&current, &current).unwrap());
        assert!(matches_policy(&legacy, &current).unwrap());
        let mut old_base = legacy.clone();
        old_base.max_rgba_bytes = 64 * 1024 * 1024;
        let mut old_text = current.clone();
        old_text.max_rgba_bytes = 160 * 1024 * 1024;
        assert!(matches_policy(&old_base, &legacy).unwrap());
        assert!(matches_policy(&old_base, &current).unwrap());
        assert!(matches_policy(&old_text, &current).unwrap());
        assert!(!matches_policy(&old_text, &legacy).unwrap());
        let mut bad_old = old_text.clone();
        bad_old.max_encoded_bytes += 1;
        assert!(!matches_policy(&bad_old, &current).unwrap());
        let mut forged = legacy.clone();
        forged.max_encoded_bytes += 1;
        assert!(!matches_policy(&forged, &current).unwrap());
        let mut intermediate = current.clone();
        intermediate.max_rgba_bytes -= 1;
        assert!(!matches_policy(&intermediate, &current).unwrap());
        let video = sqlite_media_video_jobs::policy();
        let mut old_video = video.clone();
        old_video.visual = legacy;
        assert!(matches_policy(&old_video, &video).unwrap());
        old_video.audio.max_encoded_bytes += 1;
        assert!(!matches_policy(&old_video, &video).unwrap());
        let mut retained = video.clone();
        retained.deadline_ms = Some(300_000);
        retained.stalled_after_ms = None;
        retained.max_frames = 1800;
        retained.max_output_bytes = 256 * 1024 * 1024;
        assert!(matches_policy(&retained, &video).unwrap());
        let raw = serde_json::to_value(&retained).unwrap();
        assert!(raw.get("stalled_after_ms").is_none());
        assert_eq!(raw["deadline_ms"], 300_000);
        assert_eq!(
            serde_json::to_value(
                serde_json::from_value::<
                    opencut_job_execution::media_video::LocalVideoExecutionPolicy,
                >(raw.clone())
                .unwrap()
            )
            .unwrap(),
            raw
        );
        retained.deadline_ms = Some(300_001);
        assert!(!matches_policy(&retained, &video).unwrap());
        retained.stalled_after_ms = Some(300_000);
        assert!(retained.execution_limit().is_none());
    }
}
