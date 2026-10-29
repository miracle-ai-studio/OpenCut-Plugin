//! V4 audio joins the existing durable queue with the original trusted caller.
#[cfg(test)]
mod tests;
use super::sqlite_scene_jobs::{authority, context, fixed, require_scope};
use super::*;
use opencut_contracts::{AssetLockManifest, LockedAsset, scene::EditorSnapshot};
use opencut_editor_application::{
    audit::ExecutionContext,
    scene_audio_jobs::{
        self as audio, AudioAssetCapture, AudioJobSubmissionStore, AuthorizedAudioSnapshot,
    },
};
use opencut_job_contracts::scene_audio::{
    AUDIO_RANGE_WAV_KIND, AUDIO_WAV_KIND, SubmitAudioRangeWav, SubmitAudioWav,
};
use opencut_job_execution::scene_audio::{
    LocalAudioExecutionPolicy, LocalAudioProcessingProfile, PreparedAudioWav, TrustedJobAuthority,
};

pub(crate) fn policy() -> LocalAudioExecutionPolicy {
    LocalAudioExecutionPolicy {
        profile: LocalAudioProcessingProfile::LocalClassicAudioV1,
        max_pcm_bytes: 256 * 1024 * 1024,
        max_encoded_bytes: 32 * 1024 * 1024,
        max_rss_bytes: 2 * 1024 * 1024 * 1024,
        deadline_ms: 120_000,
    }
}
impl opencut_editor_application::audio_preview::AudioPreviewRead for EditorApplication {
    fn fixed_audio_preview_snapshot(
        &self,
        project_id: &str,
        revision: &str,
    ) -> Result<opencut_editor_application::audio_preview::AuthorizedAudioPreview, EditorError>
    {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        require_scope(&tx, &self.connection.1, project_id)?;
        let limits = policy();
        Ok(
            opencut_editor_application::audio_preview::AuthorizedAudioPreview {
                snapshot: fixed(&tx, project_id, revision)?,
                authority: authority(&self.connection.1),
                policy: opencut_editor_application::audio_preview::AudioPreviewPolicy {
                    max_pcm_bytes: limits.max_pcm_bytes,
                    max_encoded_bytes: limits.max_encoded_bytes,
                    max_rss_bytes: limits.max_rss_bytes,
                },
            },
        )
    }
    fn revalidate_audio_preview(
        &self,
        project_id: &str,
        revision: &str,
        selected: &TrustedJobAuthority,
    ) -> Result<(), EditorError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        require_scope(&tx, &context(selected), project_id)?;
        let exists: bool = tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM revisions WHERE project_id=?1 AND revision=?2)",
            )
            .map_err(storage_error)?
            .query_row(params![project_id, revision], |r| r.get(0))
            .map_err(storage_error)?;
        if !exists {
            return Err(EditorError::new(
                "NOT_FOUND",
                "Fixed audio revision is unavailable",
            ));
        }
        Ok(())
    }
}
impl AudioJobSubmissionStore for EditorApplication {
    fn fixed_scene_audio_snapshot(
        &self,
        project_id: &str,
        revision: &str,
    ) -> Result<AuthorizedAudioSnapshot, EditorError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        require_scope(&tx, &self.connection.1, project_id)?;
        let snapshot = fixed(&tx, project_id, revision)?;
        Ok(AuthorizedAudioSnapshot {
            snapshot,
            authority: authority(&self.connection.1),
            processing_policy: policy(),
        })
    }
    fn find_accepted_scene_audio_job(
        &self,
        request: &JobRequest,
    ) -> Result<Option<JobRecord>, EditorError> {
        self.find_existing_job(request)
    }
    fn accept_scene_audio_job(
        &mut self,
        request: JobRequest,
        prepared: PreparedAudioWav,
    ) -> Result<JobRecord, EditorError> {
        self.create_prepared_job(
            request,
            serde_json::to_value(prepared).map_err(|_| {
                EditorError::new("SERIALIZATION_ERROR", "Could not persist audio input")
            })?,
        )
    }
}
struct Assets(opencut_asset_store::AssetStore);
impl AudioAssetCapture for Assets {
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
                    EditorError::new("ASSET_LOCK_INVALID", "Fixed audio asset is missing")
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
pub fn start_audio_wav(
    app: &mut EditorApplication,
    input: SubmitAudioWav,
) -> Result<JobRecord, EditorError> {
    let assets = Assets(opencut_asset_store::AssetStore::read_only(
        app.asset_store_root(),
    )?);
    audio::submit_audio_wav(app, &assets, input)
}

pub fn start_audio_range_wav(
    app: &mut EditorApplication,
    input: SubmitAudioRangeWav,
) -> Result<JobRecord, EditorError> {
    let assets = Assets(opencut_asset_store::AssetStore::read_only(
        app.asset_store_root(),
    )?);
    audio::submit_audio_range_wav(app, &assets, input)
}

impl EditorApplication {
    /// Ephemeral preview checks use the captured authenticated caller, not a
    /// worker owner. Recheck scope and revision existence without reparsing the
    /// immutable snapshot for every small PCM block.
    pub fn revalidate_scene_audio_preview(
        &self,
        project_id: &str,
        revision: &str,
    ) -> Result<(), EditorError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        require_scope(&tx, &self.connection.1, project_id)?;
        let present: bool = tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM revisions WHERE project_id=?1 AND revision=?2)",
            )
            .map_err(storage_error)?
            .query_row(params![project_id, revision], |row| row.get(0))
            .map_err(storage_error)?;
        if !present {
            return Err(EditorError::new(
                "NOT_FOUND",
                "Fixed preview revision is unavailable",
            ));
        }
        Ok(())
    }
}

/// Select either the public request or trusted private snapshot, without V3 projection.
pub(super) fn verified_target(
    connection: &Connection,
    input: &Value,
) -> Result<Option<(String, String)>, EditorError> {
    let (project, revision, snapshot) =
        if let Ok(public) = serde_json::from_value::<SubmitAudioWav>(input.clone()) {
            audio::validate_submission(&public)?;
            (public.project_id, public.revision, None)
        } else if let Ok(public) = serde_json::from_value::<SubmitAudioRangeWav>(input.clone()) {
            audio::validate_range_submission(&public)?;
            (public.project_id, public.revision, None)
        } else if let Ok(prepared) = serde_json::from_value::<PreparedAudioWav>(input.clone()) {
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
    let prepared: PreparedAudioWav = serde_json::from_value(input.clone())
        .map_err(|_| EditorError::new("INVALID_ARGUMENT", "Invalid prepared audio input"))?;
    audio::validate_prepared_request(request, &prepared)?;
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
    audio::validate_prepared(&prepared)?;
    Ok(())
}
pub(super) fn require_completion(
    connection: &Connection,
    job: &JobRecord,
) -> Result<(), EditorError> {
    let prepared: PreparedAudioWav = serde_json::from_value(job.input.clone())
        .map_err(|_| EditorError::new("INVALID_ARGUMENT", "Invalid persisted audio input"))?;
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
    let result = job
        .result
        .clone()
        .ok_or_else(|| EditorError::new("INVALID_STORED_RESULT", "Missing audio result"))?;
    if job.kind == AUDIO_RANGE_WAV_KIND {
        let result: audio::StoredAudioRangeWavResult = serde_json::from_value(result)
            .map_err(|_| EditorError::new("INVALID_STORED_RESULT", "Invalid range audio result"))?;
        audio::validate_stored_range_result(&prepared, &result)?;
    } else {
        let result: audio::StoredAudioWavResult = serde_json::from_value(result)
            .map_err(|_| EditorError::new("INVALID_STORED_RESULT", "Invalid audio result"))?;
        audio::validate_stored_result(&prepared, &result)?;
    }
    Ok(())
}
impl EditorApplication {
    /// Small live checks share the fixed job authorization boundary.
    pub fn revalidate_scene_audio_job(
        &self,
        job_id: &str,
        worker: &str,
        attempt: u32,
        selected_authority: &TrustedJobAuthority,
    ) -> Result<(), EditorError> {
        let kind: String = self
            .connection
            .query_row(
                "SELECT kind FROM jobs WHERE job_id=?1",
                params![job_id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if !matches!(kind.as_str(), AUDIO_WAV_KIND | AUDIO_RANGE_WAV_KIND) {
            return Err(EditorError::new(
                "INVALID_ARGUMENT",
                "Task is not a V4 audio job",
            ));
        }
        sqlite_scene_jobs::revalidate(
            self,
            &kind,
            job_id,
            worker,
            attempt,
            selected_authority,
            &policy(),
        )
    }
}
