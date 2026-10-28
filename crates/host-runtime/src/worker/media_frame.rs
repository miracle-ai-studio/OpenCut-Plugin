//! Fixed Classic scene-frame execution. Private PNG publication uses the queue fence.
use super::output_attempt::{
    attempt_directory, cancelled, check_attempt, check_output_name, error, file_hash, io_error,
};
use crate::classic_visual_runtime;
use opencut_contracts::EditorError;
use opencut_editor_application::{
    budget::Writer,
    classic_visual::{ClassicVisualExecutor, ClassicVisualResources, ClassicVisualSession},
    media_frame_jobs::{self as frame, PreparedMediaFramePng, StoredFramePngResult},
};
use opencut_job_contracts::{ArtifactDescriptor, media_frame::*};
use opencut_media::CancellationToken;
use opencut_native_services::classic_visual::{
    ClassicVisualPngSink, ClassicVisualProcess, VerifiedClassicVisualResources,
};
use opencut_sqlite_store::{EditorApplication, JobRecord};
use opencut_storage_budget::{AttemptLease, Budget};
use serde_json::Value;
use std::{ffi::OsStr, path::Path, sync::Mutex};

pub(super) fn execute(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
) -> Result<Value, EditorError> {
    cancelled(cancel)?;
    let input: PreparedMediaFramePng = serde_json::from_value(job.input.clone())
        .map_err(|_| error("INVALID_ARGUMENT", "Invalid private V4 frame preparation"))?;
    let prepared = frame::validate_prepared(&input)?;
    let app = EditorApplication::open_managed(database)?;
    app.revalidate_scene_frame_job(&job.job_id, worker, job.attempt, &input.authority)?;
    let config = classic_visual_runtime::configured(&input.processing_policy)?;
    let identity = prepared.identity();
    let ceiling = ClassicVisualPngSink::output_ceiling(identity.width, identity.height)?;
    if ceiling > input.processing_policy.max_output_bytes {
        return Err(error(
            "VISUAL_BUDGET_EXCEEDED",
            "PNG ceiling exceeds captured output admission",
        ));
    }
    let workspace = root
        .parent()
        .ok_or_else(|| error("STORAGE_BUDGET_ERROR", "Missing managed output workspace"))?;
    let _reservation = Budget::new(workspace).reserve_output_bytes(
        ceiling,
        Writer {
            job_id: job.job_id.clone(),
            attempt: job.attempt,
        },
    )?;
    let directory = attempt_directory(root, job)?;
    let attempt = AttemptLease::acquire(&directory).map_err(io_error)?;
    check_attempt(&attempt, &directory)?;
    {
        let mut guard = held
            .lock()
            .map_err(|_| error("INTERNAL_ERROR", "Attempt lifetime guard unavailable"))?;
        if guard.is_some() {
            return Err(error("INTERNAL_ERROR", "Output attempt already held"));
        }
        *guard = Some(attempt.clone());
    }
    let mut resources =
        VerifiedClassicVisualResources::new(app.asset_store_root(), &prepared, || {
            cancelled(cancel)?;
            check_attempt(&attempt, &directory)?;
            app.revalidate_scene_frame_job(&job.job_id, worker, job.attempt, &input.authority)
        })?;
    let handles = resources.handles();
    let mut session = ClassicVisualSession::new(&prepared, &input.processing_policy)?;
    resources.checkpoint()?;
    let filename = OsStr::new("frame.png");
    let file = attempt.create_readwrite_child(filename).map_err(io_error)?;
    let mut sink = ClassicVisualPngSink::new(file, identity.width, identity.height, ceiling)?;
    let host_sha256 =
        opencut_native_services::reproducibility::execution_identity(vec![])?.process_binary_sha256;
    let execution = ClassicVisualProcess { config }.execute(
        &mut session,
        &mut resources,
        &handles,
        &mut sink,
    )?;
    resources.checkpoint()?;
    let mut file = sink.finalize()?;
    let size = file.metadata().map_err(io_error)?.len();
    if size == 0 || size > ceiling {
        return Err(error(
            "ARTIFACT_VERIFICATION_FAILED",
            "PNG exceeds captured byte admission",
        ));
    }
    let content_hash = file_hash(&mut file, size, cancel, || Ok(()))?;
    attempt.sync_directory().map_err(io_error)?;
    check_output_name(&attempt, filename, &file)?;
    resources.checkpoint()?;
    let runtime = execution.runtime_identity.identity;
    let stored = StoredFramePngResult {
        public: MediaFramePngResult {
            schema_version: 1,
            identity: frame::public_identity(&prepared),
            asset_lock: input.asset_lock.clone(),
            render_input_sha256: frame::render_input_sha256(
                &input.snapshot,
                &input.sequence_id,
                &input.time,
            )?,
            execution: MediaFrameExecutionSummary {
                host_sha256,
                runtime: opencut_job_contracts::media_frame::VisualRuntimeIdentity::Native(runtime),
            },
            sources: frame::public_sources(&prepared, &execution.receipt.bound.decoded)?,
            artifact: MediaFrameArtifactMetadata {
                mime_type: "image/png".into(),
                size_bytes: size.to_string(),
                content_hash: content_hash.clone(),
            },
        },
        artifact: ArtifactDescriptor {
            path: directory
                .join(filename)
                .into_os_string()
                .into_string()
                .map_err(|_| {
                    error(
                        "ARTIFACT_VERIFICATION_FAILED",
                        "Private output path is not UTF-8",
                    )
                })?,
            mime_type: "image/png".into(),
            size,
            content_hash,
        },
    };
    frame::validate_stored_result(&input, &stored)?;
    resources.checkpoint()?;
    if opencut_native_services::reproducibility::execution_identity(vec![])?.process_binary_sha256
        != stored.public.execution.host_sha256
    {
        return Err(error(
            "EXECUTION_IDENTITY_CHANGED",
            "Executing host changed during frame processing",
        ));
    }
    check_output_name(&attempt, filename, &file)?;
    serde_json::to_value(stored).map_err(|_| {
        error(
            "SERIALIZATION_ERROR",
            "Could not serialize private frame result",
        )
    })
}
