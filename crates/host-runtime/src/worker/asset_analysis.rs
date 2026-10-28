//! Native V4 source analyses share existing engines, queue leases and budgets.
use super::{
    media_output::TaskOutput,
    output_attempt::{attempt_directory, io_error},
};
use opencut_contracts::EditorError;
use opencut_editor_application::{asset_analysis::*, budget::Writer};
use opencut_media::*;
use opencut_sqlite_store::{EditorApplication, JobRecord};
use opencut_storage_budget::{AttemptLease, Budget};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
fn error(message: &str) -> EditorError {
    EditorError::new("ANALYSIS_EXECUTION_FAILED", message)
}
fn media(e: MediaError) -> EditorError {
    EditorError::new(e.code(), e.to_string())
}
#[cfg(unix)]
pub(super) fn execute(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
) -> Result<serde_json::Value, EditorError> {
    let input: PreparedAssetAnalysis = serde_json::from_value(job.input.clone())
        .map_err(|_| error("Invalid private V4 analysis"))?;
    let lock = prepare(&input)?;
    let directory = attempt_directory(root, job)?;
    let attempt = AttemptLease::acquire(&directory).map_err(io_error)?;
    {
        let mut guard = held
            .lock()
            .map_err(|_| error("Attempt guard unavailable"))?;
        if guard.is_some() {
            return Err(error("Attempt already held"));
        }
        *guard = Some(attempt.clone());
    }
    let task_output = Arc::new(TaskOutput {
        budget: Budget::new(
            root.parent()
                .ok_or_else(|| error("Workspace root unavailable"))?,
        ),
        attempt: attempt.clone(),
        directory: directory.clone(),
        writer: Writer {
            job_id: job.job_id.clone(),
            attempt: job.attempt,
        },
    });
    let pids = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let admission = Arc::new(super::media_output::TrackedTaskOutput {
        inner: task_output,
        pids: pids.clone(),
    });
    let app = EditorApplication::open_managed(database)?;
    app.revalidate_asset_analysis_scene(&job.job_id, worker, job.attempt, &input.authority)?;
    let started = Instant::now();
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let local_cancel = CancellationToken::new();
    let result = std::thread::scope(|scope| {
        let signal = local_cancel.clone();
        let input_ref = &input;
        let monitor_database = database.to_path_buf();
        let monitor = scope.spawn(move || -> Result<(), EditorError> {
            let app = match EditorApplication::open_managed(monitor_database) {
                Ok(app) => app,
                Err(error) => {
                    signal.cancel();
                    return Err(error);
                }
            };
            loop {
                if cancel.is_cancelled() {
                    signal.cancel();
                    return Err(EditorError::new("JOB_CANCELLED", "Analysis cancelled"));
                }
                if started.elapsed()
                    >= Duration::from_millis(input_ref.processing_policy.deadline_ms)
                {
                    signal.cancel();
                    return Err(error("Captured analysis deadline exceeded"));
                }
                if let Err(error) = app.revalidate_asset_analysis_scene(
                    &job.job_id,
                    worker,
                    job.attempt,
                    &input_ref.authority,
                ) {
                    signal.cancel();
                    return Err(error);
                }
                for pid in pids
                    .lock()
                    .map_err(|_| {
                        signal.cancel();
                        error("Analysis process registry unavailable")
                    })?
                    .iter()
                {
                    match opencut_native_services::quality::observed_process_rss(*pid) {
                        Ok(Some(rss)) if rss > input_ref.processing_policy.max_rss_bytes => {
                            signal.cancel();
                            return Err(error("Analysis child exceeded captured RSS budget"));
                        }
                        Err(error) => {
                            signal.cancel();
                            return Err(error);
                        }
                        _ => {}
                    }
                }
                match stopped.recv_timeout(Duration::from_millis(100)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        let run = (|| {
            let asset = input
                .snapshot
                .project
                .assets
                .iter()
                .find(|a| a.id == input.asset_id)
                .ok_or_else(|| error("Fixed asset missing"))?;
            let store = opencut_asset_store::AssetStore::read_only(app.asset_store_root())?;
            let (mut file, size) = store.open_version_verified(&asset.version)?;
            if size != lock.size {
                return Err(error("Fixed asset size changed"));
            }
            let path = directory.join("analysis-source");
            let mut copy = opencut_native_services::budget::AdmittedFile::create(
                database,
                &path,
                "media_inspection",
                size,
                false,
            )?;
            let mut hash = Sha256::new();
            let mut copied = 0u64;
            let mut buffer = [0; 65536];
            loop {
                if local_cancel.is_cancelled() {
                    return Err(EditorError::new(
                        "JOB_CANCELLED",
                        "Analysis cancelled while preparing input",
                    ));
                }
                let n = file.read(&mut buffer).map_err(io_error)?;
                if n == 0 {
                    break;
                }
                copied = copied
                    .checked_add(n as u64)
                    .ok_or_else(|| error("Input length overflow"))?;
                if copied > size {
                    return Err(error("Fixed source grew"));
                }
                hash.update(&buffer[..n]);
                copy.append(&buffer[..n])?;
            }
            if copied != size || format!("{:x}", hash.finalize()) != lock.content_hash {
                return Err(error("Fixed asset hash changed"));
            }
            copy.sync()?;
            let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
            let ffprobe = crate::media_tool_runtime::ffprobe()?;
            let engine = FfmpegAnalysisBackend::new(ffmpeg.clone(), ffprobe)
                .with_output_admission(admission.clone(), directory.clone());
            let data = match &input.options {
                AssetAnalysisOptions::Silence {
                    noise_db,
                    min_duration_ms,
                } => AssetAnalysisData::Silence(
                    engine
                        .silence(
                            &SilenceRequest {
                                input: path.clone(),
                                noise_db: *noise_db,
                                min_duration_ms: *min_duration_ms,
                            },
                            &local_cancel,
                        )
                        .map_err(media)?,
                ),
                AssetAnalysisOptions::Shots { threshold } => AssetAnalysisData::Shots(
                    engine
                        .shots(
                            &ShotsRequest {
                                input: path.clone(),
                                threshold: *threshold,
                            },
                            &local_cancel,
                        )
                        .map_err(media)?,
                ),
                AssetAnalysisOptions::Waveform { buckets } => AssetAnalysisData::Waveform(
                    engine
                        .waveform(
                            &WaveformRequest {
                                input: path.clone(),
                                buckets: *buckets,
                            },
                            &local_cancel,
                        )
                        .map_err(media)?,
                ),
                AssetAnalysisOptions::Transcribe {
                    language,
                    word_timestamps,
                } => {
                    let model = std::env::var_os("OPENCUT_WHISPER_MODEL").ok_or_else(|| {
                        EditorError::new(
                            "CAPABILITY_UNAVAILABLE",
                            "Local transcription model is not configured",
                        )
                    })?;
                    let executable = std::env::var_os("OPENCUT_WHISPER_CLI")
                        .unwrap_or_else(|| "whisper-cli".into());
                    let engine = WhisperBackend::new(executable, model, ffmpeg)
                        .map_err(media)?
                        .with_output_admission(admission.clone(), directory.clone());
                    AssetAnalysisData::Transcribe(
                        engine
                            .transcribe(
                                &TranscribeRequest {
                                    input: path.clone(),
                                    language: language.clone(),
                                    word_timestamps: *word_timestamps,
                                },
                                &local_cancel,
                            )
                            .map_err(media)?,
                    )
                }
                AssetAnalysisOptions::SpeakerTurns {} => {
                    let model =
                        std::env::var_os("OPENCUT_WHISPER_TDRZ_MODEL").ok_or_else(|| {
                            EditorError::new(
                                "CAPABILITY_UNAVAILABLE",
                                "Local English speaker-turn model is not configured",
                            )
                        })?;
                    let executable = std::env::var_os("OPENCUT_WHISPER_CLI")
                        .unwrap_or_else(|| "whisper-cli".into());
                    let engine = TinydiarizeBackend::new(executable, model, ffmpeg)
                        .map_err(media)?
                        .with_output_admission(admission.clone(), directory.clone());
                    AssetAnalysisData::SpeakerTurns(
                        engine
                            .speaker_turns(
                                &SpeakerTurnsRequest {
                                    input: path.clone(),
                                },
                                &local_cancel,
                            )
                            .map_err(media)?,
                    )
                }
            };
            let result = AssetAnalysisResult {
                schema_version: 4,
                project_id: input.snapshot.project.id.clone(),
                revision: input.snapshot.revision.clone(),
                asset_id: input.asset_id.clone(),
                asset_version: lock.clone(),
                options: input.options.clone(),
                data,
            };
            validate_result(&input, &result)?;
            Ok(result)
        })();
        let _ = stop.send(());
        monitor
            .join()
            .map_err(|_| error("Analysis authority monitor stopped"))??;
        run
    })?;
    app.revalidate_asset_analysis_scene(&job.job_id, worker, job.attempt, &input.authority)?;
    serde_json::to_value(result).map_err(|_| error("Cannot encode analysis result"))
}
#[cfg(not(unix))]
pub(super) fn execute(
    _: &Path,
    _: &Path,
    _: &str,
    _: &JobRecord,
    _: &CancellationToken,
    _: &Mutex<Option<AttemptLease>>,
) -> Result<serde_json::Value, EditorError> {
    Err(EditorError::new(
        "CAPABILITY_UNAVAILABLE",
        "V4 analysis process admission requires Unix",
    ))
}
