//! V4 frames joins the existing durable queue with the original trusted caller.
#[cfg(test)]
mod tests;
use super::sqlite_scene_jobs::{authority, context, fixed, require_scope};
use super::*;
use opencut_contracts::{AssetLockManifest, LockedAsset, scene::EditorSnapshot};
use opencut_editor_application::{
    audit::ExecutionContext,
    media_frame_jobs::{
        self as frame, AuthorizedFrameSnapshot, MediaFrameAssetCapture,
        MediaFrameJobSubmissionStore,
    },
};
use opencut_job_contracts::media_frame::{FrameRenderInput, MEDIA_FRAME_PNG_KIND};
use opencut_job_execution::{
    media_frame::{LocalVisualExecutionPolicy, PreparedMediaFramePng},
    scene_audio::TrustedJobAuthority,
};

pub(crate) fn policy() -> LocalVisualExecutionPolicy {
    LocalVisualExecutionPolicy {
        max_frame_pixels: 16_777_216,
        max_decoded_pixels: 16_777_216,
        max_gpu_bytes: 512 * 1024 * 1024,
        // Native codec reference frames, output and text share this explicit
        // CPU ceiling. GPU and process limits remain independently enforced;
        // each decoder receives a disjoint share before allocating frames.
        max_rgba_bytes: 512 * 1024 * 1024,
        max_encoded_bytes: 32 * 1024 * 1024,
        max_output_bytes: 129 * 1024 * 1024,
        max_rss_bytes: 2 * 1024 * 1024 * 1024,
        deadline_ms: 120_000,
    }
}
impl MediaFrameJobSubmissionStore for EditorApplication {
    fn fixed_scene_frame_snapshot(
        &self,
        project_id: &str,
        revision: &str,
    ) -> Result<AuthorizedFrameSnapshot, EditorError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        require_scope(&tx, &self.connection.1, project_id)?;
        let snapshot = fixed(&tx, project_id, revision)?;
        Ok(AuthorizedFrameSnapshot {
            snapshot,
            authority: authority(&self.connection.1),
            processing_policy: policy(),
        })
    }
    fn find_accepted_scene_frame_job(
        &self,
        request: &JobRequest,
    ) -> Result<Option<JobRecord>, EditorError> {
        self.find_existing_job(request)
    }
    fn accept_scene_frame_job(
        &mut self,
        request: JobRequest,
        prepared: PreparedMediaFramePng,
    ) -> Result<JobRecord, EditorError> {
        self.create_prepared_job(
            request,
            serde_json::to_value(prepared).map_err(|_| {
                EditorError::new("SERIALIZATION_ERROR", "Could not persist frame input")
            })?,
        )
    }
}
struct Assets(opencut_asset_store::AssetStore);
impl MediaFrameAssetCapture for Assets {
    fn capture(
        &self,
        snapshot: &EditorSnapshot,
        sequence_id: &str,
        ids: &[String],
    ) -> Result<AssetLockManifest, EditorError> {
        let mut assets = Vec::with_capacity(ids.len());
        for id in ids {
            let asset = snapshot
                .project
                .assets
                .iter()
                .find(|asset| &asset.id == id)
                .ok_or_else(|| {
                    EditorError::new("ASSET_LOCK_INVALID", "Fixed frame asset is missing")
                })?;
            self.0.open_version_verified(&asset.version)?;
            assets.push(LockedAsset {
                asset_id: id.clone(),
                content_hash: asset.version.content_hash.clone(),
                size: asset.version.size,
            });
        }
        Ok(AssetLockManifest {
            version: 1,
            revision: snapshot.revision.clone(),
            sequence_id: sequence_id.into(),
            assets,
        })
    }
}
pub fn start_frame_png(
    app: &mut EditorApplication,
    input: FrameRenderInput,
) -> Result<JobRecord, EditorError> {
    let assets = Assets(opencut_asset_store::AssetStore::read_only(
        app.asset_store_root(),
    )?);
    frame::submit_frame_png(app, &assets, input)
}

/// Select either the public request or trusted private snapshot, without V3 projection.
pub(super) fn verified_target(
    connection: &Connection,
    input: &Value,
) -> Result<Option<(String, String)>, EditorError> {
    let (project, revision, snapshot) = if let Ok(public) =
        serde_json::from_value::<FrameRenderInput>(input.clone())
    {
        frame::validate_submission(&public)?;
        (public.project_id, public.revision, None)
    } else if let Ok(prepared) = serde_json::from_value::<PreparedMediaFramePng>(input.clone()) {
        (
            prepared.snapshot.project.id.clone(),
            prepared.snapshot.revision.clone(),
            Some(prepared.snapshot),
        )
    } else {
        return Ok(None);
    };
    let actual = fixed(connection, &project, &revision)?;
    if snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot != &actual)
    {
        return Ok(None);
    }
    Ok(Some((project, revision)))
}
/// Called inside the same IMMEDIATE transaction as acceptance, including owners.
pub(super) fn validate_acceptance(
    connection: &Connection,
    caller: &ExecutionContext,
    request: &JobRequest,
    input: &Value,
) -> Result<(), EditorError> {
    let prepared: PreparedMediaFramePng = serde_json::from_value(input.clone())
        .map_err(|_| EditorError::new("INVALID_ARGUMENT", "Invalid prepared frame input"))?;
    frame::validate_prepared_request(request, &prepared)?;
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
    frame::validate_prepared(&prepared)?;
    Ok(())
}
pub(super) fn require_completion(
    connection: &Connection,
    job: &JobRecord,
) -> Result<(), EditorError> {
    let prepared: PreparedMediaFramePng = serde_json::from_value(job.input.clone())
        .map_err(|_| EditorError::new("INVALID_ARGUMENT", "Invalid persisted frame input"))?;
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
    let result: frame::StoredFramePngResult = serde_json::from_value(
        job.result
            .clone()
            .ok_or_else(|| EditorError::new("INVALID_STORED_RESULT", "Missing frame result"))?,
    )
    .map_err(|_| EditorError::new("INVALID_STORED_RESULT", "Invalid frame result"))?;
    frame::validate_stored_result(&prepared, &result)?;
    Ok(())
}
impl EditorApplication {
    /// Small live checks share the fixed job authorization boundary.
    pub fn revalidate_scene_frame_job(
        &self,
        job_id: &str,
        worker: &str,
        attempt: u32,
        selected_authority: &TrustedJobAuthority,
    ) -> Result<(), EditorError> {
        sqlite_scene_jobs::revalidate(
            self,
            MEDIA_FRAME_PNG_KIND,
            job_id,
            worker,
            attempt,
            selected_authority,
            &policy(),
        )
    }
}
