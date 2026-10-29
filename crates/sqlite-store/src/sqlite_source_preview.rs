//! Source-only tasks retain the existing queue, caller and publication fences.
use super::sqlite_scene_jobs::{authority, context, fixed, require_scope};
use super::*;
use opencut_editor_application::source_preview::{self as source, *};
#[cfg(test)]
mod tests;

pub fn policy() -> SourcePreviewPolicy {
    SourcePreviewPolicy {
        max_source_pixels: 16_777_216,
        max_decoded_bytes: 512 * 1024 * 1024,
        max_source_rss_bytes: 768 * 1024 * 1024,
        max_encoder_rss_bytes: 256 * 1024 * 1024,
        max_output_bytes: 4 * 1024 * 1024 * 1024,
        max_frames: MAX_SOURCE_FRAMES,
        max_decode_threads: 2,
        deadline_ms: 3_600_000,
    }
}
impl SourcePreviewSubmissionStore for EditorApplication {
    fn fixed_source_preview_snapshot(
        &self,
        project: &str,
        revision: &str,
    ) -> Result<AuthorizedSourcePreview, EditorError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        require_scope(&tx, &self.connection.1, project)?;
        Ok(AuthorizedSourcePreview {
            snapshot: fixed(&tx, project, revision)?,
            authority: authority(&self.connection.1),
            processing_policy: policy(),
        })
    }
    fn find_source_preview(&self, request: &JobRequest) -> Result<Option<JobRecord>, EditorError> {
        self.find_existing_job(request)
    }
    fn accept_source_preview(
        &mut self,
        request: JobRequest,
        prepared: PreparedSourcePreview,
    ) -> Result<JobRecord, EditorError> {
        self.create_prepared_job(
            request,
            serde_json::to_value(prepared).map_err(|_| {
                EditorError::new("SERIALIZATION_ERROR", "Cannot persist source preparation")
            })?,
        )
    }
}
pub fn prepare(
    app: &mut EditorApplication,
    input: SourcePreviewPrepare,
    runtime_fingerprint: &str,
) -> Result<Vec<JobRecord>, EditorError> {
    let assets = sqlite_media_frame_jobs::Assets(opencut_asset_store::AssetStore::read_only(
        app.asset_store_root(),
    )?);
    source::prepare(app, &assets, input, runtime_fingerprint)
}
pub(super) fn verified_target(
    connection: &Connection,
    input: &Value,
) -> Result<Option<(String, String)>, EditorError> {
    let Ok(prepared) = serde_json::from_value::<PreparedSourcePreview>(input.clone()) else {
        return Ok(None);
    };
    source::validate_prepared(&prepared)?;
    if fixed(
        connection,
        &prepared.snapshot.project.id,
        &prepared.snapshot.revision,
    )? != prepared.snapshot
    {
        return Ok(None);
    }
    Ok(Some((
        prepared.snapshot.project.id,
        prepared.snapshot.revision,
    )))
}
pub(super) fn validate_acceptance(
    connection: &Connection,
    caller: &opencut_editor_application::audit::ExecutionContext,
    request: &JobRequest,
    input: &Value,
) -> Result<(), EditorError> {
    let prepared: PreparedSourcePreview = serde_json::from_value(input.clone())
        .map_err(|_| EditorError::new("INVALID_ARGUMENT", "Invalid private source preparation"))?;
    source::validate_request(request, &prepared)?;
    require_scope(connection, caller, &prepared.snapshot.project.id)?;
    if prepared.authority != authority(caller)
        || prepared.processing_policy != policy()
        || fixed(
            connection,
            &prepared.snapshot.project.id,
            &prepared.snapshot.revision,
        )? != prepared.snapshot
    {
        return Err(access::denied());
    }
    Ok(())
}
pub(super) fn require_completion(
    connection: &Connection,
    job: &JobRecord,
) -> Result<(), EditorError> {
    let prepared: PreparedSourcePreview =
        decode_json(&job.input.to_string(), "source preparation input")?;
    require_scope(
        connection,
        &context(&prepared.authority),
        &prepared.snapshot.project.id,
    )?;
    if prepared.processing_policy != policy()
        || fixed(
            connection,
            &prepared.snapshot.project.id,
            &prepared.snapshot.revision,
        )? != prepared.snapshot
    {
        return Err(access::denied());
    }
    let result: StoredSourcePreviewResult =
        serde_json::from_value(job.result.clone().ok_or_else(|| {
            EditorError::new("INVALID_STORED_RESULT", "Missing source preparation result")
        })?)
        .map_err(|_| {
            EditorError::new("INVALID_STORED_RESULT", "Invalid source preparation result")
        })?;
    source::validate_result(&prepared, &result)
}
impl EditorApplication {
    pub fn capture_source_preview_job(
        &self,
        job_id: &str,
    ) -> Result<CapturedSourcePreviewJob, EditorError> {
        let job = self.get_job(job_id)?;
        let captured = source::capture_job(&job)?;
        require_scope(
            &self.connection,
            &self.connection.1,
            &captured.prepared.identity.project_id,
        )?;
        if captured.prepared.authority != authority(&self.connection.1)
            || captured.prepared.processing_policy != policy()
            || fixed(
                &self.connection,
                &captured.prepared.snapshot.project.id,
                &captured.prepared.snapshot.revision,
            )? != captured.prepared.snapshot
        {
            return Err(access::denied());
        }
        Ok(captured)
    }
    pub fn source_preview_job_status(
        &self,
        captured: &mut CapturedSourcePreviewJob,
    ) -> Result<(JobState, u32, bool), EditorError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        require_scope(
            &tx,
            &self.connection.1,
            &captured.prepared.identity.project_id,
        )?;
        if captured.prepared.authority != authority(&self.connection.1)
            || captured.prepared.processing_policy != policy()
        {
            return Err(access::denied());
        }
        let (state,attempt,cancelled,input_same,result_same,changed):(String,u32,bool,bool,bool,Option<String>) = tx.prepare_cached(
            "SELECT j.state,j.attempt,j.cancel_requested,j.input_json=?3,j.result_json IS ?4,CASE WHEN j.result_json IS ?4 THEN NULL ELSE j.result_json END FROM jobs j JOIN job_projects p USING(job_id) JOIN revisions r ON r.project_id=p.project_id AND r.revision=p.revision WHERE j.job_id=?1 AND j.kind=?2 AND p.project_id=?5 AND p.revision=?6"
        ).map_err(storage_error)?.query_row(params![captured.job_id,source::KIND,captured.input_text,captured.result_text,captured.prepared.snapshot.project.id,captured.prepared.snapshot.revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).map_err(storage_error)?;
        let state = match state.as_str() {
            "queued" => JobState::Queued,
            "running" => JobState::Running,
            "succeeded" => JobState::Succeeded,
            "failed" => JobState::Failed,
            "cancelled" => JobState::Cancelled,
            _ => {
                return Err(EditorError::new(
                    "INVALID_STORED_RESULT",
                    "Unknown source preparation state",
                ));
            }
        };
        if !input_same
            || captured.published_attempt.is_some_and(|published| {
                published != attempt || state != JobState::Succeeded || !result_same
            })
        {
            return Err(EditorError::new(
                "ARTIFACT_VERIFICATION_FAILED",
                "Source preparation publication changed",
            ));
        }
        if state == JobState::Succeeded && captured.result.is_none() {
            let text = changed.ok_or_else(|| {
                EditorError::new("INVALID_STORED_RESULT", "Source publication omitted result")
            })?;
            let stored: StoredSourcePreviewResult =
                decode_json(&text, "source preparation result")?;
            source::validate_result(&captured.prepared, &stored)?;
            captured.result = Some(stored);
            captured.result_text = Some(text);
            captured.published_attempt = Some(attempt);
        }
        Ok((state, attempt, cancelled))
    }
    pub fn require_source_preview_read(
        &self,
        captured: &CapturedSourcePreviewJob,
    ) -> Result<(), EditorError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        require_scope(
            &tx,
            &self.connection.1,
            &captured.prepared.identity.project_id,
        )?;
        sqlite_access::require_job(
            &tx,
            &self.connection.1,
            &captured.job_id,
            ProjectAction::ReadArtifacts,
        )?;
        let valid: bool = tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM jobs j JOIN job_projects p USING(job_id) JOIN revisions r ON r.project_id=p.project_id AND r.revision=p.revision WHERE j.job_id=?1 AND j.kind=?2 AND j.state='succeeded' AND j.attempt=?3 AND j.cancel_requested=0 AND j.input_json=?4 AND j.result_json=?5 AND p.project_id=?6 AND p.revision=?7)").map_err(storage_error)?.query_row(params![captured.job_id,source::KIND,captured.published_attempt,captured.input_text,captured.result_text,captured.prepared.snapshot.project.id,captured.prepared.snapshot.revision],|row|row.get(0)).map_err(storage_error)?;
        if !valid {
            return Err(EditorError::new(
                "ARTIFACT_VERIFICATION_FAILED",
                "Source preview is no longer published",
            ));
        }
        Ok(())
    }
    pub fn revalidate_source_preview_job(
        &self,
        job_id: &str,
        worker: &str,
        attempt: u32,
        selected: &TrustedJobAuthority,
    ) -> Result<(), EditorError> {
        sqlite_scene_jobs::revalidate(
            self,
            source::KIND,
            job_id,
            worker,
            attempt,
            selected,
            &policy(),
        )
    }
}
