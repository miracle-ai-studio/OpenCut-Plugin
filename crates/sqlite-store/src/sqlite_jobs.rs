//! SQLite adapter for atomic task lifecycle transitions.
use super::lifecycle::{JobAction, JobClock, JobLifecycleStore, Transition, transition};
use super::*;
use opencut_editor_application::audit::ExecutionContext;

pub(super) struct SystemClock;
impl JobClock for SystemClock {
    fn now_ms(&self) -> Result<i64, EditorError> {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| EditorError::new("INTERNAL_ERROR", "system clock precedes Unix epoch"))?;
        i64::try_from(elapsed.as_millis())
            .map_err(|_| EditorError::new("INTERNAL_ERROR", "system clock exceeds supported range"))
    }
}
impl JobLifecycleStore for SqliteConnection {
    fn claim_atomic(
        &mut self,
        worker: &str,
        lease_ms: i64,
        clock: &dyn JobClock,
        preferred: &[String],
    ) -> Result<Option<JobRecord>, EditorError> {
        sqlite_access::owner(&self.1)?;
        let context = ExecutionContext::worker(Some(worker));
        let tx = self
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let observed_at = clock.now_ms()?;
        super::lifecycle::deadline(worker, observed_at, lease_ms)?;
        let job_id:Option<String>=tx.query_row(
            "SELECT job_id FROM jobs WHERE state='queued' AND cancel_requested=0 ORDER BY CASE WHEN job_id IN (SELECT value FROM json_each(?1)) THEN 0 ELSE 1 END,created_at,rowid LIMIT 1",
            [serde_json::to_string(preferred).map_err(|_| EditorError::new("INTERNAL_ERROR","Cannot encode scheduling preference"))?],|row|row.get(0)).optional().map_err(storage_error)?;
        let Some(job_id) = job_id else {
            tx.commit().map_err(storage_error)?;
            return Ok(None);
        };
        let current = load_job(&tx, &job_id)?;
        let next = transition(&current, JobAction::Claim { worker, lease_ms }, observed_at)?;
        let result = persist(&tx, &context, &current, next)?;
        tx.commit().map_err(storage_error)?;
        Ok(Some(result))
    }
    fn update_atomic(
        &mut self,
        job_id: &str,
        action: JobAction<'_>,
        clock: &dyn JobClock,
    ) -> Result<JobRecord, EditorError> {
        if !matches!(action, JobAction::RequestCancel) {
            sqlite_access::owner(&self.1)?;
        }
        let context = match &action {
            JobAction::RequestCancel => self.1.clone(),
            JobAction::RepairCancel => self.1.clone(),
            JobAction::Claim { worker, .. } => ExecutionContext::worker(Some(worker)),
            JobAction::Worker { lease, .. } => ExecutionContext::worker(Some(lease.worker)),
            JobAction::Recover => ExecutionContext::worker(None),
        };
        let tx = self
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        if matches!(action, JobAction::RequestCancel) {
            sqlite_access::require_job(&tx, &context, job_id, ProjectAction::CancelJobs)?;
        }
        let current = load_job(&tx, job_id)?;
        let next = transition(&current, action, clock.now_ms()?)?;
        let result = persist(&tx, &context, &current, next)?;
        tx.commit().map_err(storage_error)?;
        Ok(result)
    }
    fn recover_atomic(&mut self, clock: &dyn JobClock) -> Result<usize, EditorError> {
        sqlite_access::owner(&self.1)?;
        let context = ExecutionContext::worker(None);
        let tx = self
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let observed_at = clock.now_ms()?;
        let expired: Vec<String> = {
            let mut statement = tx
                .prepare("SELECT job_id FROM jobs WHERE state='running' AND lease_until<=?1")
                .map_err(storage_error)?;
            statement
                .query_map([observed_at], |row| row.get(0))
                .map_err(storage_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage_error)?
        };
        for id in &expired {
            let current = load_job(&tx, id)?;
            persist(
                &tx,
                &context,
                &current,
                transition(&current, JobAction::Recover, observed_at)?,
            )?;
        }
        tx.commit().map_err(storage_error)?;
        Ok(expired.len())
    }
}
fn persist(
    tx: &Connection,
    context: &ExecutionContext,
    previous: &JobRecord,
    change: Transition,
) -> Result<JobRecord, EditorError> {
    if !change.changed {
        return Ok(change.record);
    }
    let next = change.record;
    if next.kind == opencut_editor_application::source_preview::KIND
        && next.state == JobState::Succeeded
    {
        sqlite_source_preview::require_completion(tx, &next)?;
    }
    if next.kind == "analysis.sequence_transcribe.v4" && next.state == JobState::Succeeded {
        sqlite_scene_transcription::require_completion(tx, &next)?;
    }
    if matches!(
        next.kind.as_str(),
        "render.project_audio.v4" | "render.project_audio_range.v4"
    ) && next.state == JobState::Succeeded
    {
        sqlite_scene_audio_jobs::require_completion(tx, &next)?;
    }
    if next.kind == "render.project_video.v4" && next.state == JobState::Succeeded {
        sqlite_media_video_jobs::require_completion(tx, &next)?;
    }
    if next.kind == "render.project_caption_video.v4" && next.state == JobState::Succeeded {
        sqlite_scene_caption_video_jobs::require_completion(tx, &next)?;
    }
    if next.kind == "render.project_graphic_video.v4" && next.state == JobState::Succeeded {
        sqlite_scene_graphic_video_jobs::require_completion(tx, &next)?;
    }
    if matches!(
        next.kind.as_str(),
        "render.export_video.v2" | "render.export_video_range.v1"
    ) && next.state == JobState::Succeeded
    {
        export_video::require_completion(tx, &next)?;
    }
    if next.kind == "analysis.asset.v4" && next.state == JobState::Succeeded {
        asset_analysis::require_completion(tx, &next)?;
    }
    if next.kind == "render.project_scene_video.v4" && next.state == JobState::Succeeded {
        sqlite_scene_video_jobs::require_completion(tx, &next)?;
    }
    if next.kind == "render.project_mask_video.v4" && next.state == JobState::Succeeded {
        sqlite_scene_mask_video_jobs::require_completion(tx, &next)?;
    }
    if next.kind == "render.project_blend_video.v4" && next.state == JobState::Succeeded {
        sqlite_scene_blend_video_jobs::require_completion(tx, &next)?;
    }
    if matches!(
        next.kind.as_str(),
        "render.project_text_frame.v4"
            | "render.project_caption_frame.v4"
            | "render.project_graphic_frame.v4"
            | "render.project_mask_frame.v4"
            | "render.project_scene_frame.v4"
            | "render.project_blend_frame.v4"
    ) && next.state == JobState::Succeeded
    {
        sqlite_scene_text_frame_jobs::require_completion(tx, &next)?;
    }
    if next.kind == "render.project_frame.v4" && next.state == JobState::Succeeded {
        sqlite_media_frame_jobs::require_completion(tx, &next)?;
    }
    let result = next
        .result
        .as_ref()
        .map(|value| encode_json(value, "job result"))
        .transpose()?;
    let error = next
        .error
        .as_ref()
        .map(|value| encode_json(value, "job error"))
        .transpose()?;
    let progress = next
        .progress
        .as_ref()
        .map(|value| encode_json(value, "job progress"))
        .transpose()?;
    let changed=tx.execute(
        "UPDATE jobs SET state=?1,attempt=?2,result_json=?3,error_json=?4,lease_owner=?5,
         lease_until=?6,cancel_requested=?7,progress_json=?8
         WHERE job_id=?9 AND state=?10 AND attempt=?11 AND lease_owner IS ?12 AND lease_until IS ?13",
        params![job_state_name(next.state),next.attempt,result,error,next.lease_owner,next.lease_until,
            next.cancel_requested,progress,next.job_id,job_state_name(previous.state),previous.attempt,previous.lease_owner,previous.lease_until],
    ).map_err(storage_error)?;
    if changed != 1 {
        return Err(EditorError::new("JOB_LEASE_LOST", "job update was fenced"));
    }
    sqlite_uploads::publish_import(tx, context, &next)?;
    if let Some(event) = change.event {
        insert_job_event(tx, &next, event.event_type, event.payload)?;
        sqlite_audit::job(
            tx,
            context,
            &next,
            &format!("job.{}", event.event_type),
            false,
        )?;
    }
    Ok(next)
}
