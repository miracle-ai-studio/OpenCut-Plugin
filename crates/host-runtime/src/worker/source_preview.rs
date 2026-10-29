//! One owned attempt for source decoding, lossless encoding and index output.
use super::{media_output::TaskOutput, output_attempt::*};
use opencut_contracts::EditorError;
use opencut_editor_application::{
    budget::Writer,
    source_preview::{self as source, *},
};
use opencut_media::{CancellationToken, ProgressStage, ProgressUpdate};
use opencut_native_services::source_preview::{
    SourcePreviewProcessConfig, SourcePreviewProcessExecutor,
};
use opencut_sqlite_store::{EditorApplication, JobRecord};
use opencut_storage_budget::{AttemptLease, Budget};
use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

#[cfg(unix)]
pub(super) fn execute(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<serde_json::Value, EditorError> {
    let input: PreparedSourcePreview = serde_json::from_value(job.input.clone())
        .map_err(|_| error("INVALID_ARGUMENT", "Invalid private source preparation"))?;
    source::validate_prepared(&input)?;
    let directory = attempt_directory(root, job)?;
    let attempt = AttemptLease::acquire(&directory).map_err(io_error)?;
    check_attempt(&attempt, &directory)?;
    {
        let mut guard = held
            .lock()
            .map_err(|_| error("INTERNAL_ERROR", "Source attempt guard unavailable"))?;
        if guard.is_some() {
            return Err(error(
                "INTERNAL_ERROR",
                "Source output attempt already held",
            ));
        }
        *guard = Some(attempt.clone());
    }
    let app = EditorApplication::open_managed(database)?;
    let started = Instant::now();
    let mut check = |_: bool| {
        cancelled(cancel)?;
        if started.elapsed() >= Duration::from_millis(input.processing_policy.deadline_ms) {
            return Err(error(
                "SOURCE_PREVIEW_TIME_LIMIT",
                "Source preparation deadline exceeded",
            ));
        }
        check_attempt(&attempt, &directory)?;
        app.revalidate_source_preview_job(&job.job_id, worker, job.attempt, &input.authority)
    };
    check(false)?;
    let runtime = crate::source_preview_runtime::configured()?;
    if runtime.fingerprint != input.identity.runtime_fingerprint {
        return Err(error(
            "SOURCE_PREVIEW_ENGINE_CHANGED",
            "Captured source preparation runtime changed",
        ));
    }
    let output = TaskOutput {
        budget: Budget::new(
            root.parent()
                .ok_or_else(|| error("STORAGE_BUDGET_ERROR", "Source workspace unavailable"))?,
        ),
        attempt: attempt.clone(),
        directory: directory.clone(),
        writer: Writer {
            job_id: job.job_id.clone(),
            attempt: job.attempt,
        },
    };
    let executor = SourcePreviewProcessExecutor {
        config: SourcePreviewProcessConfig {
            worker: runtime.worker,
            ffmpeg: runtime.ffmpeg,
            ffprobe: runtime.ffprobe,
            strict_cgroup: None,
            asset_root: app.asset_store_root().into(),
            video_output: directory.join("source-preview.mkv"),
            index_output: directory.join("source-preview.idx"),
            runtime: runtime.identity,
        },
        admission: &output,
    };
    progress(ProgressUpdate {
        stage: ProgressStage::Decoding,
        fraction: None,
        media_time: None,
    });
    let result = source::execute(&executor, &input, &mut check)?;
    for descriptor in [&result.artifact, &result.frame_index] {
        let name = Path::new(&descriptor.path).file_name().ok_or_else(|| {
            error(
                "INVALID_STORED_RESULT",
                "Source output filename unavailable",
            )
        })?;
        let mut file = attempt.open_readonly_child(name).map_err(io_error)?;
        check_output_name(&attempt, name, &file)?;
        if file_hash(&mut file, descriptor.size, cancel, || check(false))?
            != descriptor.content_hash
        {
            return Err(error(
                "ARTIFACT_VERIFICATION_FAILED",
                "Source output changed before publication",
            ));
        }
        check_output_name(&attempt, name, &file)?;
    }
    check(false)?;
    serde_json::to_value(result).map_err(|_| {
        error(
            "SERIALIZATION_ERROR",
            "Cannot encode source preparation result",
        )
    })
}
