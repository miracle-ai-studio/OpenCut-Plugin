//! Host assembly of source preparation; application owns selection and jobs.
use opencut_contracts::{EditorError, source_preview::*};
use opencut_native_services::source_preview::{resolve_runtime, runtime_fingerprint};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
mod resources;
pub(crate) use resources::{PreviewResources, SourceCache};

#[derive(Clone)]
pub(crate) struct SourcePlanJob {
    pub identity: SourcePreviewIdentity,
    pub authority: opencut_job_execution::scene_audio::TrustedJobAuthority,
    pub job_id: String,
}
#[derive(Clone)]
pub(crate) struct SourcePlan {
    pub runtime_fingerprint: String,
    pub jobs: Vec<SourcePlanJob>,
}
pub(crate) fn prepare_plan(
    host: &mut crate::Automation,
    input: SourcePreviewPrepare,
    runtime: Runtime,
) -> Result<SourcePlan, EditorError> {
    let jobs = opencut_sqlite_store::sqlite_source_preview::prepare(
        &mut host.app,
        input,
        &runtime.fingerprint,
    )?;
    let jobs = jobs
        .into_iter()
        .map(|job| {
            let identity: SourcePreviewIdentity =
                serde_json::from_value(job.input["identity"].clone()).map_err(|_| {
                    EditorError::new(
                        "INVALID_STORED_RESULT",
                        "Source task omitted captured identity",
                    )
                })?;
            let authority =
                serde_json::from_value(job.input["authority"].clone()).map_err(|_| {
                    EditorError::new(
                        "INVALID_STORED_RESULT",
                        "Source task omitted captured caller scope",
                    )
                })?;
            Ok(SourcePlanJob {
                identity,
                authority,
                job_id: job.job_id,
            })
        })
        .collect::<Result<Vec<_>, EditorError>>()?;
    Ok(SourcePlan {
        runtime_fingerprint: runtime.fingerprint,
        jobs,
    })
}

#[derive(Clone)]
pub(crate) struct Runtime {
    pub worker: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub identity: SourcePreviewRuntimeIdentity,
    pub fingerprint: String,
}
struct Captured {
    observation: String,
    runtime: Runtime,
}
fn observation(worker: &Path, ffmpeg: &Path, ffprobe: &Path) -> Result<String, EditorError> {
    let mut digest = Sha256::new();
    digest.update(opencut_native_services::source_art::runtime_fingerprint(worker)?.as_bytes());
    let host = std::env::current_exe().map_err(|_| {
        EditorError::new(
            "CAPABILITY_UNAVAILABLE",
            "Source preparation host unavailable",
        )
    })?;
    for path in [host.as_path(), ffmpeg, ffprobe] {
        let metadata = std::fs::metadata(path).map_err(|_| {
            EditorError::new(
                "SOURCE_PREVIEW_ENGINE_CHANGED",
                "Source preparation runtime unavailable",
            )
        })?;
        digest.update(path.as_os_str().as_encoded_bytes());
        digest.update(metadata.len().to_le_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            for value in [
                metadata.ino() as i128,
                metadata.mtime() as i128,
                metadata.mtime_nsec() as i128,
                metadata.ctime() as i128,
                metadata.ctime_nsec() as i128,
            ] {
                digest.update(value.to_le_bytes());
            }
        }
        #[cfg(not(unix))]
        digest.update(format!("{:?}", metadata.modified()).as_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub(crate) fn configured() -> Result<Runtime, EditorError> {
    let worker = crate::native_worker_runtime::worker()?;
    let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
    let ffprobe = crate::media_tool_runtime::ffprobe()?;
    let before = observation(&worker, &ffmpeg, &ffprobe)?;
    static CAPTURED: OnceLock<Mutex<Option<Captured>>> = OnceLock::new();
    let mut capture = CAPTURED.get_or_init(Mutex::default).lock().map_err(|_| {
        EditorError::new(
            "INTERNAL_ERROR",
            "Source preparation runtime lock unavailable",
        )
    })?;
    // These paths, inodes, sizes, modification and change times are observed
    // on every admission. Rehash only when an actual dependency changes;
    // expiring an unchanged identity stalls the foreground preview on a timer.
    if !capture
        .as_ref()
        .is_some_and(|entry| entry.observation == before)
    {
        let identity = resolve_runtime(&worker, &ffmpeg, &ffprobe)?;
        let fingerprint = runtime_fingerprint(&identity)?;
        if observation(&worker, &ffmpeg, &ffprobe)? != before {
            return Err(EditorError::new(
                "SOURCE_PREVIEW_ENGINE_CHANGED",
                "Source preparation runtime changed during capture",
            ));
        }
        *capture = Some(Captured {
            observation: before,
            runtime: Runtime {
                worker,
                ffmpeg,
                ffprobe,
                identity,
                fingerprint,
            },
        });
    }
    Ok(capture
        .as_ref()
        .expect("captured source runtime")
        .runtime
        .clone())
}
pub(crate) fn prepare(
    host: &mut crate::Automation,
    input: SourcePreviewPrepare,
) -> Result<opencut_api_contracts::source_preview::SourcePreviewPreparation, EditorError> {
    use opencut_editor_application::source_preview::SourcePreviewSubmissionStore;
    host.app
        .set_execution_context(crate::ExecutionContext::for_caller(
            host.source,
            host.caller.clone(),
        ));
    opencut_editor_application::source_preview::validate_submission(&input)?;
    let fixed = host
        .app
        .fixed_source_preview_snapshot(&input.project_id, &input.revision)?;
    if opencut_editor_application::source_preview::source_targets(&fixed.snapshot, &input)?
        .is_empty()
    {
        return Ok(
            opencut_api_contracts::source_preview::SourcePreviewPreparation { jobs: Vec::new() },
        );
    }
    let runtime = configured()?;
    let jobs = opencut_sqlite_store::sqlite_source_preview::prepare(
        &mut host.app,
        input,
        &runtime.fingerprint,
    )?;
    let jobs = jobs
        .iter()
        .map(|job| host.app.job_view(&job.job_id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(opencut_api_contracts::source_preview::SourcePreviewPreparation { jobs })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs::{self, File, FileTimes};

    #[test]
    fn dependency_observation_detects_same_size_rewrites_replacements_and_removal() {
        let directory = tempfile::tempdir().unwrap();
        let paths = ["worker", "ffmpeg", "ffprobe"].map(|name| directory.path().join(name));
        for path in &paths {
            fs::write(path, b"original").unwrap();
        }
        let observe = || observation(&paths[0], &paths[1], &paths[2]);
        let mut previous = observe().unwrap();
        assert_eq!(observe().unwrap(), previous);
        for path in &paths {
            let modified = fs::metadata(path).unwrap().modified().unwrap();
            fs::write(path, b"modified").unwrap();
            File::open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(modified))
                .unwrap();
            let current = observe().unwrap();
            assert_ne!(current, previous);
            previous = current;
            let replacement = directory.path().join("replacement");
            fs::write(&replacement, b"modified").unwrap();
            fs::rename(replacement, path).unwrap();
            let current = observe().unwrap();
            assert_ne!(current, previous);
            previous = current;
        }
        fs::remove_file(&paths[2]).unwrap();
        assert!(observe().is_err());
    }
}
