mod asset_analysis;
mod export_video;
mod historical_visual;
mod media_frame;
#[cfg(unix)]
mod media_output;
mod media_video;
mod output_attempt;
mod scene_audio;
mod scene_caption_video;
mod scene_text_frame;
mod scene_transcription;
pub(super) fn sequence_transcribe_capability()
-> opencut_job_contracts::scene_transcription::SceneTranscriptionCapability {
    scene_transcription::capability()
}
pub(crate) mod receipts;
use opencut_job_execution::{
    AnalysisOptions, PreparedAnalysisInput, PreparedProjectFrameInput, PreparedProjectRangeInput,
    PreparedProjectRenderInput, PreparedSpeakerTurnsInput, PreparedTranscribeInput,
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use opencut_asset_store::{AssetLockManifest, AssetStore};
use opencut_contracts::{EditorError, ProjectSnapshot};
use opencut_job_contracts::{
    ArtifactDescriptor, AssetLockingMode, CompositedProjectFrameJobResult,
    CompositedProjectFrameQuality, ExportJobInput, FrameJobInput, MediaExportJobResult,
    MediaFrameJobResult, ProjectFrameJobResult, ProjectFrameQuality, ProjectRangeJobResult,
    ProjectRangeQuality, ProjectRenderJobResult, SpeakerTurnsJobResult, TranscribeJobResult,
};
use opencut_media::{
    CancellationToken, Container, ExportRequest, FfmpegAnalysisBackend, FfmpegBackend,
    FrameAlignment, FrameExtractRequest, MediaTime, ProbeRequest, ProgressStage, ProgressUpdate,
    ShotsRequest, SilenceRequest, SpeakerTurnsRequest, TinydiarizeBackend, TranscribeRequest,
    VerifyRequest, VideoCodec, WaveformRequest, WhisperBackend,
};
use opencut_project_render::{
    ProjectRenderAdapter, ResolvedProjectFrameRequest, ResolvedProjectRangeRequest,
    ResolvedProjectRenderRequest, ResolvedProjectSnapshot, ResolvedVisualProjectSnapshot,
    select_project_frame_time,
};
use opencut_sqlite_store::{EditorApplication, JobProgress, JobRecord, JobState};
use opencut_time::RationalTime;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// A busy SQLite operation can consume its full five-second timeout. Keep
// retrying a transient lock while there is still one timeout of lease runway,
// instead of failing a healthy writer halfway through its durable lease.
const HEARTBEAT_BUSY_RUNWAY_MS: u64 = 5_000;

use opencut_job_runtime::{ExecutorPort, JobStore, JobStoreFactory, LEASE_MS};
struct SqliteJobStore(
    EditorApplication,
    PathBuf,
    opencut_editor_application::preview_cache::PreviewClaimBudget,
);
struct SqliteFactory<'a>(&'a Path);
impl JobStoreFactory for SqliteFactory<'_> {
    fn open(&self) -> Result<Box<dyn JobStore>, EditorError> {
        Ok(Box::new(SqliteJobStore(
            EditorApplication::open_managed(self.0)?,
            self.0.to_path_buf(),
            Default::default(),
        )))
    }
}
impl JobStore for SqliteJobStore {
    fn recover(&mut self) -> Result<usize, EditorError> {
        self.0.live_jobs().recover()
    }
    fn claim(&mut self, worker: &str, lease_ms: i64) -> Result<Option<JobRecord>, EditorError> {
        let preferred = if self.2.allows_preference() {
            crate::audio_preview::cache::preferred_jobs(&self.1, &self.0)?
        } else {
            Vec::new()
        };
        let job = self
            .0
            .live_jobs()
            .claim_preferred(worker, lease_ms, &preferred)?;
        self.2.claimed(
            job.as_ref()
                .is_some_and(|job| preferred.contains(&job.job_id)),
        );
        Ok(job)
    }
    fn get_job(&self, id: &str) -> Result<JobRecord, EditorError> {
        self.0.get_job(id)
    }
    fn complete(
        &mut self,
        id: &str,
        worker: &str,
        attempt: u32,
        value: Value,
    ) -> Result<JobRecord, EditorError> {
        let original = self.0.get_job(id)?;
        match self.0.live_jobs().complete(id, worker, attempt, value) {
            Err(error)
                if matches!(
                    original.kind.as_str(),
                    "render.project_audio.v4"
                        | "render.project_audio_range.v4"
                        | "render.project_frame.v4"
                        | "render.project_text_frame.v4"
                        | "render.project_caption_frame.v4"
                        | "render.project_graphic_frame.v4"
                        | "render.project_mask_frame.v4"
                        | "render.project_scene_frame.v4"
                        | "render.project_blend_frame.v4"
                        | "render.project_video.v4"
                        | "render.project_caption_video.v4"
                        | "render.project_graphic_video.v4"
                        | "render.project_mask_video.v4"
                        | "render.project_scene_video.v4"
                        | "render.export_video.v2"
                        | "render.export_video_range.v1"
                        | "render.project_blend_video.v4"
                        | "analysis.sequence_transcribe.v4"
                        | "analysis.asset.v4"
                ) && !matches!(error.code.as_str(), "DATABASE_BUSY" | "JOB_LEASE_LOST") =>
            {
                // Completion revalidates the original caller inside its SQL
                // transaction. A refusal leaves the lease unchanged; fail with
                // that same fence rather than abandoning a running attempt.
                self.0.live_jobs().fail(
                    id,
                    worker,
                    attempt,
                    json!({"code":error.code,"message":error.message}),
                )
            }
            result => result,
        }
    }
    fn fail(
        &mut self,
        id: &str,
        worker: &str,
        attempt: u32,
        error: Value,
    ) -> Result<JobRecord, EditorError> {
        self.0.live_jobs().fail(id, worker, attempt, error)
    }
    fn finish_cancelled(
        &mut self,
        id: &str,
        worker: &str,
        attempt: u32,
    ) -> Result<JobRecord, EditorError> {
        self.0.live_jobs().finish_cancelled(id, worker, attempt)
    }
}
struct NativeExecutor<'a> {
    database: &'a Path,
    output_root: PathBuf,
    publication_attempt: Mutex<Option<opencut_storage_budget::AttemptLease>>,
}
impl ExecutorPort for NativeExecutor<'_> {
    fn publication_finished(&self) {
        self.publication_attempt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }
    fn clean_abandoned_inputs(&self) -> Result<(), EditorError> {
        clean_abandoned_inputs(
            &EditorApplication::open_managed(self.database)?,
            &self.output_root,
        )
    }
    fn execute(
        &self,
        worker: &str,
        job: &JobRecord,
        stop: Arc<AtomicBool>,
    ) -> Result<Value, EditorError> {
        self.publication_attempt
            .lock()
            .map_err(|_| EditorError::new("INTERNAL_ERROR", "Attempt guard lock failed"))?
            .take();
        execute_monitored(
            self.database,
            &self.output_root,
            worker,
            job,
            stop,
            &self.publication_attempt,
        )
    }
}
pub fn run(database: &Path, once: bool, stop: Arc<AtomicBool>) -> Result<(), EditorError> {
    let executor = NativeExecutor {
        database,
        publication_attempt: Mutex::new(None),
        output_root: database
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("artifacts"),
    };
    opencut_job_runtime::run(&SqliteFactory(database), &executor, once, stop)
}

fn execute_monitored(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    stop: Arc<AtomicBool>,
    publication_attempt: &Mutex<Option<opencut_storage_budget::AttemptLease>>,
) -> Result<Value, EditorError> {
    let cancel = CancellationToken::new();
    let done = Arc::new(AtomicBool::new(false));
    let heartbeat_pending = Arc::new(AtomicBool::new(false));
    let monitor_app = EditorApplication::open_managed(database)?;
    let progress_app = EditorApplication::open_managed(database)?;
    let import_input: Option<opencut_editor_application::uploads::PreparedMediaImportInput> =
        if job.kind == "media.import" {
            Some(parse(job.input.clone())?)
        } else {
            None
        };
    std::thread::scope(|scope| {
        let monitor_cancel = cancel.clone();
        let monitor_done = done.clone();
        let monitor_heartbeat_pending = heartbeat_pending.clone();
        let monitor = scope.spawn(move || {
            let mut app = Some(monitor_app);
            let mut last_heartbeat = Instant::now();
            loop {
                if monitor_done.load(Ordering::SeqCst) {
                    monitor_heartbeat_pending.store(false, Ordering::SeqCst);
                    break None;
                }
                if stop.load(Ordering::SeqCst) {
                    monitor_cancel.cancel();
                    monitor_heartbeat_pending.store(false, Ordering::SeqCst);
                    break None;
                }
                monitor_heartbeat_pending.store(true, Ordering::SeqCst);
                let heartbeat: Result<JobRecord, EditorError> = (|| {
                    if app.is_none() {
                        app = Some(EditorApplication::open_managed(database)?);
                    }
                    let app = app.as_mut().expect("monitor connection was opened");
                    let state =
                        app.live_jobs()
                            .heartbeat(&job.job_id, worker, job.attempt, LEASE_MS)?;
                    if let Some(input) = &import_input {
                        use opencut_editor_application::uploads::SealedUploadRead;
                        app.authorize_sealed_import(input, &job.job_id, worker, job.attempt)?;
                    }
                    Ok(state)
                })();
                if heartbeat.is_ok() {
                    monitor_heartbeat_pending.store(false, Ordering::SeqCst);
                }
                match heartbeat {
                    Ok(state) if state.cancel_requested => {
                        monitor_cancel.cancel();
                        break None;
                    }
                    Ok(_) => last_heartbeat = Instant::now(),
                    Err(error)
                        if error.code == "DATABASE_BUSY"
                            && last_heartbeat.elapsed()
                                < Duration::from_millis(
                                    LEASE_MS as u64 - HEARTBEAT_BUSY_RUNWAY_MS,
                                ) =>
                    {
                        // A VFS write may have returned BUSY after a SQLite
                        // transaction began. Drop its pager before retrying.
                        app.take();
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    Err(error) => {
                        monitor_heartbeat_pending.store(false, Ordering::SeqCst);
                        eprintln!(
                            "Job {} attempt {} heartbeat failed: {error}",
                            job.job_id, job.attempt
                        );
                        monitor_cancel.cancel();
                        break Some(error);
                    }
                }
                for _ in 0..5 {
                    if monitor_done.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        let mut reporter =
            ProgressReporter::new(progress_app, Some(database), worker, job, &cancel)
                .with_heartbeat_priority(heartbeat_pending);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if matches!(
                job.kind.as_str(),
                opencut_job_contracts::scene_audio::AUDIO_WAV_KIND
                    | opencut_job_contracts::scene_audio::AUDIO_RANGE_WAV_KIND
            ) {
                scene_audio::execute(database, root, worker, job, &cancel, publication_attempt)
            } else if job.kind
                == opencut_job_contracts::scene_transcription::SCENE_TRANSCRIPTION_KIND
            {
                scene_transcription::execute(
                    database,
                    root,
                    worker,
                    job,
                    &cancel,
                    publication_attempt,
                )
            } else if job.kind == "analysis.asset.v4" {
                asset_analysis::execute(database, root, worker, job, &cancel, publication_attempt)
            } else if matches!(
                job.kind.as_str(),
                opencut_job_contracts::export_video::EXPORT_VIDEO_KIND
                    | opencut_job_contracts::export_video::EXPORT_VIDEO_RANGE_KIND
            ) {
                export_video::execute(
                    database,
                    root,
                    worker,
                    job,
                    &cancel,
                    publication_attempt,
                    &mut |update| reporter.report(update),
                )
            } else if job.kind == opencut_job_contracts::scene_video::SCENE_VIDEO_MP4_KIND {
                media_video::execute_with_profile(
                    database,
                    root,
                    worker,
                    job,
                    &cancel,
                    publication_attempt,
                    opencut_editor_application::media_video_jobs::VideoFrameProfile::SceneV1,
                    &mut |update| reporter.report(update),
                )
            } else if job.kind == opencut_job_contracts::media_video::MEDIA_VIDEO_MP4_KIND {
                media_video::execute(
                    database,
                    root,
                    worker,
                    job,
                    &cancel,
                    publication_attempt,
                    &mut |update| reporter.report(update),
                )
            } else if job.kind == opencut_job_contracts::scene_caption_video::CAPTION_VIDEO_MP4_KIND
            {
                scene_caption_video::execute(
                    database,
                    root,
                    worker,
                    job,
                    &cancel,
                    publication_attempt,
                    &mut |update| reporter.report(update),
                )
            } else if job.kind == opencut_job_contracts::scene_graphic_video::GRAPHIC_VIDEO_MP4_KIND
            {
                media_video::execute_with_profile(
                    database,
                    root,
                    worker,
                    job,
                    &cancel,
                    publication_attempt,
                    opencut_editor_application::media_video_jobs::VideoFrameProfile::GraphicV1,
                    &mut |update| reporter.report(update),
                )
            } else if job.kind == opencut_job_contracts::scene_mask_video::MASK_VIDEO_MP4_KIND {
                media_video::execute_with_profile(
                    database,
                    root,
                    worker,
                    job,
                    &cancel,
                    publication_attempt,
                    opencut_editor_application::media_video_jobs::VideoFrameProfile::MaskV1,
                    &mut |update| reporter.report(update),
                )
            } else if job.kind == opencut_job_contracts::scene_blend_video::BLEND_VIDEO_MP4_KIND {
                media_video::execute_with_profile(
                    database,
                    root,
                    worker,
                    job,
                    &cancel,
                    publication_attempt,
                    opencut_editor_application::media_video_jobs::VideoFrameProfile::BlendV1,
                    &mut |update| reporter.report(update),
                )
            } else if matches!(
                job.kind.as_str(),
                opencut_job_contracts::scene_text_frame::TEXT_FRAME_PNG_KIND
                    | opencut_job_contracts::scene_caption_frame::CAPTION_FRAME_PNG_KIND
                    | opencut_job_contracts::scene_graphic_frame::GRAPHIC_FRAME_PNG_KIND
                    | opencut_job_contracts::scene_mask_frame::MASK_FRAME_PNG_KIND
                    | opencut_job_contracts::scene_frame::SCENE_FRAME_PNG_KIND
                    | opencut_job_contracts::scene_blend_frame::BLEND_FRAME_PNG_KIND
            ) {
                scene_text_frame::execute(database, root, worker, job, &cancel, publication_attempt)
            } else if job.kind == opencut_job_contracts::media_frame::MEDIA_FRAME_PNG_KIND {
                media_frame::execute(database, root, worker, job, &cancel, publication_attempt)
            } else {
                execute(database, worker, root, job, &cancel, &mut |update| {
                    reporter.report(update)
                })
            }
        }));
        done.store(true, Ordering::SeqCst);
        let monitor_error = monitor
            .join()
            .unwrap_or_else(|_| Some(EditorError::new("INTERNAL_ERROR", "Job monitor panicked")));
        if let Some(error) = monitor_error {
            Err(error)
        } else if let Some(error) = reporter.error {
            Err(error)
        } else {
            outcome.unwrap_or_else(|_| {
                Err(EditorError::new("INTERNAL_ERROR", "Media worker panicked"))
            })
        }
    })
}

/// The fraction belongs to the reported stage, not to total job completion.
/// Stage changes and the stage's final sample bypass the write-rate limit.
struct ProgressReporter<'a> {
    app: Option<EditorApplication>,
    database: Option<&'a Path>,
    worker: &'a str,
    job: &'a JobRecord,
    cancel: &'a CancellationToken,
    heartbeat_pending: Option<Arc<AtomicBool>>,
    last: Option<(JobProgress, Instant)>,
    error: Option<EditorError>,
}

impl<'a> ProgressReporter<'a> {
    fn new(
        app: EditorApplication,
        database: Option<&'a Path>,
        worker: &'a str,
        job: &'a JobRecord,
        cancel: &'a CancellationToken,
    ) -> Self {
        Self {
            app: Some(app),
            database,
            worker,
            job,
            cancel,
            heartbeat_pending: None,
            last: None,
            error: None,
        }
    }

    fn with_heartbeat_priority(mut self, pending: Arc<AtomicBool>) -> Self {
        self.heartbeat_pending = Some(pending);
        self
    }

    fn report(&mut self, update: ProgressUpdate) {
        self.report_at(update, Instant::now());
    }

    fn report_at(&mut self, update: ProgressUpdate, observed_at: Instant) {
        if self.error.is_some()
            || self.cancel.is_cancelled()
            || self
                .heartbeat_pending
                .as_ref()
                .is_some_and(|pending| pending.load(Ordering::SeqCst))
        {
            return;
        }
        let progress = JobProgress {
            stage: match update.stage {
                ProgressStage::ResolvingAssets => "resolving_assets",
                ProgressStage::Probing => "probing",
                ProgressStage::SelectingFrame => "selecting_frame",
                ProgressStage::Decoding => "decoding",
                ProgressStage::Encoding => "encoding",
                ProgressStage::Verifying => "verifying",
            }
            .into(),
            fraction: update.fraction.map(f64::from),
            media_time: update.media_time.map(|time| opencut_contracts::Time {
                ticks: time.ticks.to_string(),
                timebase: time.timebase,
            }),
        };
        if let Some((last, at)) = &self.last {
            if last == &progress {
                return;
            }
            if last.stage == progress.stage
                && !(progress.fraction == Some(1.0) && last.fraction != Some(1.0))
                && observed_at.saturating_duration_since(*at) < Duration::from_millis(250)
            {
                return;
            }
        }
        if self.app.is_none() {
            let Some(database) = self.database else {
                self.cancel.cancel();
                self.error = Some(EditorError::new(
                    "INTERNAL_ERROR",
                    "Progress connection lost",
                ));
                return;
            };
            match EditorApplication::open_managed(database) {
                Ok(app) => self.app = Some(app),
                Err(error) if error.code == "DATABASE_BUSY" => return,
                Err(error) => {
                    self.cancel.cancel();
                    self.error = Some(error);
                    return;
                }
            }
        }
        let result = self
            .app
            .as_mut()
            .expect("progress connection was opened")
            .live_jobs()
            .progress(
                &self.job.job_id,
                self.worker,
                self.job.attempt,
                progress.clone(),
            );
        match result {
            Ok(state) => {
                self.last = Some((progress, observed_at));
                if state.cancel_requested {
                    self.cancel.cancel();
                }
            }
            Err(error) if error.code == "DATABASE_BUSY" => {
                // Progress is advisory. The monitor will retry the heartbeat;
                // final publication still checks the current lease in SQLite.
                if self.database.is_some() {
                    self.app.take();
                }
            }
            Err(error) => {
                // Do not keep rendering or publish a successful result after losing
                // the lease or failing to durably report the attempt's state.
                self.cancel.cancel();
                self.error = Some(error);
            }
        }
    }
}

struct PrivateInputs(PathBuf);
impl Drop for PrivateInputs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Attempts increase monotonically. A queued attempt can only be claimed under
/// a NEW number, so deleting its old inputs remains safe if another worker claims
/// concurrently. Never remove current running inputs or public artifacts.
fn clean_abandoned_inputs(app: &EditorApplication, root: &Path) -> Result<(), EditorError> {
    let io = |e: std::io::Error| EditorError::new("PRIVATE_INPUT_CLEANUP_FAILED", e.to_string());
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io(error)),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(EditorError::new(
                "PRIVATE_INPUT_CLEANUP_FAILED",
                "Artifact root must be a real directory",
            ));
        }
        Ok(_) => (),
    }
    // Execution publishes canonical attempt paths. Parent aliases such as
    // /var and /private/var must identify the same publication during recovery.
    let root = fs::canonicalize(root).map_err(io)?;
    for job_directory in fs::read_dir(root).map_err(io)? {
        let job_directory = job_directory.map_err(io)?;
        if !job_directory.file_type().map_err(io)?.is_dir() {
            continue;
        }
        let Ok(job) = app.get_job(&job_directory.file_name().to_string_lossy()) else {
            continue;
        };
        for attempt_directory in fs::read_dir(job_directory.path()).map_err(io)? {
            let attempt_directory = attempt_directory.map_err(io)?;
            if !attempt_directory.file_type().map_err(io)?.is_dir() {
                continue;
            }
            let name = attempt_directory.file_name();
            let Some(attempt) = name
                .to_str()
                .and_then(|name| name.strip_prefix("attempt-"))
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if attempt < job.attempt || (attempt == job.attempt && job.state != JobState::Running) {
                #[cfg(unix)]
                let _cleanup = {
                    let Some(guard) = opencut_storage_budget::AttemptLease::try_cleanup(
                        &attempt_directory.path(),
                    )
                    .map_err(io)?
                    else {
                        continue;
                    };
                    if !guard.still_names(&attempt_directory.path()).map_err(io)? {
                        continue;
                    }
                    guard
                };
                let published = attempt == job.attempt
                    && job.state == JobState::Succeeded
                    && job
                        .result
                        .as_ref()
                        .and_then(|result| result.pointer("/artifact/path"))
                        .and_then(Value::as_str)
                        .is_some_and(|path| Path::new(path).starts_with(attempt_directory.path()));
                // An expired or terminal unsuccessful attempt has no published
                // references and is entirely private. A successful attempt keeps
                // only the directory containing its referenced public artifact.
                if !published {
                    fs::remove_dir_all(attempt_directory.path()).map_err(io)?;
                    continue;
                }
                let inputs = attempt_directory.path().join("inputs");
                match fs::symlink_metadata(&inputs) {
                    Ok(metadata) if metadata.is_dir() => {
                        fs::remove_dir_all(inputs).map_err(io)?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) => {}
                    Err(error) => return Err(io(error)),
                }
            }
        }
    }
    Ok(())
}

fn resolve_inputs(
    root: &Path,
    directory: &Path,
    snapshot: &ProjectSnapshot,
    sequence_id: &str,
    lock: Option<&AssetLockManifest>,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<ResolvedProjectSnapshot, EditorError> {
    let verified = prepare_inputs(
        root,
        directory,
        snapshot,
        InputScope::Full(sequence_id),
        lock,
        cancel,
        progress,
    )?;
    resolve_verified_inputs(verified, sequence_id)
}

fn resolve_visual_inputs(
    root: &Path,
    directory: &Path,
    snapshot: &ProjectSnapshot,
    sequence_id: &str,
    lock: Option<&AssetLockManifest>,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<ResolvedVisualProjectSnapshot, EditorError> {
    let verified = prepare_inputs(
        root,
        directory,
        snapshot,
        InputScope::VisualFrame(sequence_id),
        lock,
        cancel,
        progress,
    )?;
    ProjectRenderAdapter::resolve_visual_assets(verified, sequence_id, |asset| {
        Ok(PathBuf::from(&asset.uri))
    })
    .map_err(|error| EditorError::new(error.code, error.message))
}

enum InputScope<'a> {
    Full(&'a str),
    VisualFrame(&'a str),
}

/// The private preparation path verifies exactly the dependency set selected by
/// the job kind. Callers wrap these bytes in a purpose-specific resolved type.
fn prepare_inputs(
    root: &Path,
    directory: &Path,
    snapshot: &ProjectSnapshot,
    scope: InputScope<'_>,
    lock: Option<&AssetLockManifest>,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<ProjectSnapshot, EditorError> {
    let (sequence_id, ids) = match scope {
        InputScope::Full(sequence_id) => (
            sequence_id,
            ProjectRenderAdapter::required_asset_ids(snapshot, sequence_id),
        ),
        InputScope::VisualFrame(sequence_id) => (
            sequence_id,
            ProjectRenderAdapter::required_visual_asset_ids(snapshot, sequence_id),
        ),
    };
    let ids = ids.map_err(|error| EditorError::new(error.code, error.message))?;
    let Some(lock) = lock else {
        if snapshot
            .project
            .assets
            .iter()
            .any(|asset| asset.version.is_some() && ids.contains(&asset.id))
        {
            return Err(EditorError::new(
                "ASSET_LOCK_REQUIRED",
                "A versioned project job requires a verified media lock",
            ));
        }
        return Ok(snapshot.clone());
    };
    progress(ProgressUpdate {
        stage: ProgressStage::ResolvingAssets,
        fraction: None,
        media_time: None,
    });
    let store = AssetStore::open_managed(
        root.parent()
            .unwrap_or_else(|| Path::new("."))
            .join("asset-store"),
    )?;
    store.materialize(snapshot, sequence_id, lock, &ids, directory, &|| {
        cancel.is_cancelled()
    })
}

/// The caller supplies legacy unversioned inputs, or private copies whose lock
/// and bytes were checked by AssetStore. Resolving preserves version provenance.
fn resolve_verified_inputs(
    verified: ProjectSnapshot,
    sequence_id: &str,
) -> Result<ResolvedProjectSnapshot, EditorError> {
    ProjectRenderAdapter::resolve_assets(verified, sequence_id, |asset| {
        Ok(PathBuf::from(&asset.uri))
    })
    .map_err(|error| EditorError::new(error.code, error.message))
}

/// Also runs while unwinding a backend panic, before execute_monitored turns it
/// into a failed job. Only a successful frame survives this private workspace.
struct PreviewFiles {
    intermediate: Option<PathBuf>,
    output: PathBuf,
    keep_output: bool,
}

impl Drop for PreviewFiles {
    fn drop(&mut self) {
        if let Some(intermediate) = &self.intermediate {
            let _ = fs::remove_file(intermediate);
        }
        if !self.keep_output {
            let _ = fs::remove_file(&self.output);
        }
    }
}

fn execute(
    database: &Path,
    worker: &str,
    root: &Path,
    job: &JobRecord,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<Value, EditorError> {
    progress(ProgressUpdate {
        stage: ProgressStage::Probing,
        fraction: None,
        media_time: None,
    });
    if cancel.is_cancelled() {
        return Err(EditorError::new(
            "JOB_CANCELLED",
            "Job was cancelled before media execution",
        ));
    }
    let directory = root
        .join(&job.job_id)
        .join(format!("attempt-{}", job.attempt));
    for path in [
        root.to_path_buf(),
        root.join(&job.job_id),
        directory.clone(),
    ] {
        fs::create_dir_all(&path).map_err(io_error)?;
        if !fs::symlink_metadata(&path).map_err(io_error)?.is_dir() {
            return Err(EditorError::new(
                "STORAGE_BUDGET_ERROR",
                "Private attempt roots must be real directories",
            ));
        }
    }
    let directory = fs::canonicalize(directory).map_err(io_error)?;
    let backend = crate::media_tool_runtime::backend()?;
    #[cfg(unix)]
    let admission = {
        let attempt =
            opencut_storage_budget::AttemptLease::acquire(&directory).map_err(io_error)?;
        std::sync::Arc::new(media_output::TaskOutput {
            budget: opencut_storage_budget::Budget::new(
                root.parent().unwrap_or_else(|| Path::new(".")),
            ),
            attempt,
            directory: directory.clone(),
            writer: opencut_editor_application::budget::Writer {
                job_id: job.job_id.clone(),
                attempt: job.attempt,
            },
        })
    };
    #[cfg(unix)]
    let backend = backend.with_output_admission(admission.clone());
    let receipt = receipts::prepare(job, &backend)?;
    let result = (|| {
        match job.kind.as_str() {
            "render.project.v3"
            | "preview.project_frame.v3"
            | "preview.project_range.v3"
            | "analysis.sequence_transcribe" => {
                historical_visual::execute(root, &directory, &backend, job, cancel, progress)
            }
            "analysis.silence" | "analysis.shots" | "analysis.waveform" => {
                let input: PreparedAnalysisInput = parse(job.input.clone())?;
                let matching = matches!(
                    (&input.options, job.kind.as_str()),
                    (AnalysisOptions::Silence { .. }, "analysis.silence")
                        | (AnalysisOptions::Shots { .. }, "analysis.shots")
                        | (AnalysisOptions::Waveform { .. }, "analysis.waveform")
                );
                if !matching
                    || input.asset_lock.assets.len() != 1
                    || input.asset_lock.assets[0].asset_id != input.asset_id
                {
                    return Err(EditorError::new(
                        "ASSET_LOCK_INVALID",
                        "Analysis options or asset lock are invalid",
                    ));
                }
                let private = PrivateInputs(directory.join("inputs"));
                let store = AssetStore::open_managed(
                    root.parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join("asset-store"),
                )?;
                let snapshot = store.materialize(
                    &input.snapshot,
                    &input.sequence_id,
                    &input.asset_lock,
                    std::slice::from_ref(&input.asset_id),
                    &private.0,
                    &|| cancel.is_cancelled(),
                )?;
                let source = snapshot
                    .project
                    .assets
                    .iter()
                    .find(|asset| asset.id == input.asset_id)
                    .ok_or_else(|| {
                        EditorError::new("ASSET_LOCK_INVALID", "Analysis asset missing")
                    })?;
                let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
                let ffprobe = crate::media_tool_runtime::ffprobe()?;
                let engine = FfmpegAnalysisBackend::new(ffmpeg, ffprobe);
                #[cfg(unix)]
                let engine = engine.with_output_admission(admission.clone(), directory.clone());
                let source = PathBuf::from(&source.uri);
                let analysis = match input.options {
                    AnalysisOptions::Silence {
                        noise_db,
                        min_duration_ms,
                    } => json!(
                        engine
                            .silence(
                                &SilenceRequest {
                                    input: source,
                                    noise_db,
                                    min_duration_ms
                                },
                                cancel
                            )
                            .map_err(media_error)?
                    ),
                    AnalysisOptions::Shots { threshold } => json!(
                        engine
                            .shots(
                                &ShotsRequest {
                                    input: source,
                                    threshold
                                },
                                cancel
                            )
                            .map_err(media_error)?
                    ),
                    AnalysisOptions::Waveform { buckets } => json!(
                        engine
                            .waveform(
                                &WaveformRequest {
                                    input: source,
                                    buckets
                                },
                                cancel
                            )
                            .map_err(media_error)?
                    ),
                };
                if cancel.is_cancelled() {
                    return Err(EditorError::new("JOB_CANCELLED", "Analysis was cancelled"));
                }
                Ok(json!({
                    "project_id": input.snapshot.project.id,
                    "revision": input.snapshot.revision,
                    "asset_id": input.asset_id,
                    "asset_version": input.asset_lock.assets.into_iter().next().expect("checked above"),
                    "analysis": analysis,
                }))
            }
            "analysis.speaker_turns" => {
                let input: PreparedSpeakerTurnsInput = parse(job.input.clone())?;
                if input.language != "en"
                    || input.asset_lock.assets.len() != 1
                    || input.asset_lock.assets[0].asset_id != input.asset_id
                    || input.asset_lock.revision != input.snapshot.revision
                    || input.asset_lock.sequence_id != input.sequence_id
                {
                    return Err(EditorError::new(
                        "ASSET_LOCK_INVALID",
                        "Speaker-turn asset lock is invalid",
                    ));
                }
                let private = PrivateInputs(directory.join("inputs"));
                let store = AssetStore::open_managed(
                    root.parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join("asset-store"),
                )?;
                let snapshot = store.materialize(
                    &input.snapshot,
                    &input.sequence_id,
                    &input.asset_lock,
                    std::slice::from_ref(&input.asset_id),
                    &private.0,
                    &|| cancel.is_cancelled(),
                )?;
                let source = snapshot
                    .project
                    .assets
                    .iter()
                    .find(|asset| asset.id == input.asset_id)
                    .ok_or_else(|| {
                        EditorError::new("ASSET_LOCK_INVALID", "Speaker-turn asset missing")
                    })?;
                let model = std::env::var_os("OPENCUT_WHISPER_TDRZ_MODEL")
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        EditorError::new(
                            "CAPABILITY_UNAVAILABLE",
                            "Host English speaker-turn model is not configured",
                        )
                    })?;
                let executable = std::env::var_os("OPENCUT_WHISPER_CLI")
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "whisper-cli".into());
                let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
                let engine =
                    TinydiarizeBackend::new(executable, model, ffmpeg).map_err(media_error)?;
                #[cfg(unix)]
                let engine = engine.with_output_admission(admission.clone(), directory.clone());
                let analysis = engine
                    .speaker_turns(
                        &SpeakerTurnsRequest {
                            input: PathBuf::from(&source.uri),
                        },
                        cancel,
                    )
                    .map_err(media_error)?;
                if cancel.is_cancelled() {
                    return Err(EditorError::new(
                        "JOB_CANCELLED",
                        "Speaker-turn analysis was cancelled",
                    ));
                }
                Ok(json!(SpeakerTurnsJobResult {
                    project_id: input.snapshot.project.id,
                    revision: input.snapshot.revision,
                    asset_id: input.asset_id,
                    asset_version: input
                        .asset_lock
                        .assets
                        .into_iter()
                        .next()
                        .expect("checked above"),
                    analysis,
                }))
            }
            "analysis.transcribe" => {
                let input: PreparedTranscribeInput = parse(job.input.clone())?;
                if input.asset_lock.assets.len() != 1
                    || input.asset_lock.assets[0].asset_id != input.asset_id
                {
                    return Err(EditorError::new(
                        "ASSET_LOCK_INVALID",
                        "Analysis asset lock is invalid",
                    ));
                }
                let private = PrivateInputs(directory.join("inputs"));
                let store = AssetStore::open_managed(
                    root.parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join("asset-store"),
                )?;
                let snapshot = store.materialize(
                    &input.snapshot,
                    &input.sequence_id,
                    &input.asset_lock,
                    std::slice::from_ref(&input.asset_id),
                    &private.0,
                    &|| cancel.is_cancelled(),
                )?;
                let source = snapshot
                    .project
                    .assets
                    .iter()
                    .find(|asset| asset.id == input.asset_id)
                    .ok_or_else(|| {
                        EditorError::new("ASSET_LOCK_INVALID", "Analysis asset missing")
                    })?;
                let model = std::env::var_os("OPENCUT_WHISPER_MODEL")
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        EditorError::new(
                            "CAPABILITY_UNAVAILABLE",
                            "Host transcription model is not configured",
                        )
                    })?;
                let executable = std::env::var_os("OPENCUT_WHISPER_CLI")
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "whisper-cli".into());
                let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
                let engine = WhisperBackend::new(executable, model, ffmpeg).map_err(media_error)?;
                #[cfg(unix)]
                let engine = engine.with_output_admission(admission.clone(), directory.clone());
                let transcription = engine
                    .transcribe(
                        &TranscribeRequest {
                            input: PathBuf::from(&source.uri),
                            language: input.language,
                            word_timestamps: input.word_timestamps,
                        },
                        cancel,
                    )
                    .map_err(media_error)?;
                if cancel.is_cancelled() {
                    return Err(EditorError::new("JOB_CANCELLED", "Analysis was cancelled"));
                }
                Ok(json!(TranscribeJobResult {
                    project_id: input.snapshot.project.id,
                    revision: input.snapshot.revision,
                    asset_id: input.asset_id,
                    asset_version: input
                        .asset_lock
                        .assets
                        .into_iter()
                        .next()
                        .expect("checked above"),
                    transcription,
                }))
            }
            "media.import" => {
                use opencut_editor_application::uploads::{self, PreparedMediaImportInput};
                let input: PreparedMediaImportInput = parse(job.input.clone())?;
                uploads::validate_manifest(&input)?;
                let private = PrivateInputs(directory.join("inputs"));
                fs::create_dir_all(&private.0).map_err(io_error)?;
                let file = private.0.join("upload.media");
                let workspace = root
                    .parent()
                    .ok_or_else(|| EditorError::new("INTERNAL_ERROR", "Missing workspace root"))?;
                use opencut_editor_application::uploads::SealedUploadRead;
                let app = EditorApplication::open_managed(database)?;
                app.authorize_sealed_import(&input, &job.job_id, worker, job.attempt)?;
                opencut_asset_store::uploads::UploadChunks::managed(workspace.join("uploads"))
                    .assemble(
                        &input,
                        &file,
                        &mut |offset| {
                            app.sealed_import_chunks(
                                &input,
                                &job.job_id,
                                worker,
                                job.attempt,
                                offset,
                            )
                        },
                        &|| cancel.is_cancelled(),
                    )?;
                let file = fs::canonicalize(&file).map_err(io_error)?;
                let metadata = backend.probe_upload(&file, cancel).map_err(media_error)?;
                let mut asset = opencut_editor_application::media::describe(
                    &metadata,
                    &public_path(file.clone())?,
                    &input.name,
                    &input.content_hash,
                    input.size_bytes,
                )?;
                asset.id = input.media_id.clone();
                // This project resource does not grant access to the owner-only catalog.
                asset.metadata.remove("media_url");
                if asset.kind == opencut_contracts::AssetKind::Video {
                    let observed = opencut_native_services::packet_rate::probe(
                        &crate::native_worker_runtime::worker()?,
                        &file,
                        input.size_bytes,
                        &mut || {
                            if cancel.is_cancelled() {
                                return Err(EditorError::new(
                                    "JOB_CANCELLED",
                                    "Import was cancelled",
                                ));
                            }
                            app.authorize_sealed_import(&input, &job.job_id, worker, job.attempt)
                        },
                    )?;
                    asset.metadata.insert(
                        opencut_media::CLASSIC_PACKET_RATE_METADATA_KEY.into(),
                        serde_json::to_value(observed).map_err(|_| {
                            EditorError::new(
                                "MEDIA_INVALID_TOOL_OUTPUT",
                                "Could not encode Classic packet-rate observation",
                            )
                        })?,
                    );
                }
                if cancel.is_cancelled() {
                    return Err(EditorError::new("JOB_CANCELLED", "Import was cancelled"));
                }
                let mut asset =
                    AssetStore::open_managed(workspace.join("asset-store"))?.pin_asset(&asset)?;
                asset.uri = format!("opencut-media://{}", input.media_id);
                let result = opencut_job_contracts::MediaImportJobResult {
                    media_id: input.media_id.clone(),
                    project_id: input.project_id.clone(),
                    upload_id: input.upload_id.clone(),
                    job_id: job.job_id.clone(),
                    asset,
                };
                uploads::validate_import_result(&input, &job.job_id, &result)?;
                Ok(json!(result))
            }
            "media.probe" => {
                let request: ProbeRequest = parse(job.input.clone())?;
                Ok(json!(
                    backend
                        .probe_cancellable(&request, cancel)
                        .map_err(media_error)?
                ))
            }
            "preview.frame" => {
                let request: FrameJobInput = parse(job.input.clone())?;
                let output = directory.join("frame.png");
                let result = backend
                    .extract_frame(
                        &FrameExtractRequest {
                            input: request.input.into(),
                            output: output.clone(),
                            requested_time: request.requested_time,
                            video_stream: request.video_stream,
                            alignment: request.alignment,
                            overwrite: false,
                        },
                        cancel,
                        progress,
                    )
                    .map_err(media_error)?;
                Ok(json!(MediaFrameJobResult {
                    media: result.try_map_paths(public_path)?,
                    artifact: artifact(&output, "image/png")?,
                }))
            }
            "preview.project_frame" => {
                let input: PreparedProjectFrameInput = parse(job.input.clone())?;
                let private_inputs = PrivateInputs(directory.join("inputs"));
                let snapshot = resolve_inputs(
                    root,
                    &private_inputs.0,
                    &input.snapshot,
                    &input.sequence_id,
                    input.asset_lock.as_ref(),
                    cancel,
                    progress,
                )?;
                let sequence = input
                    .snapshot
                    .project
                    .sequences
                    .iter()
                    .find(|sequence| sequence.id == input.sequence_id)
                    .ok_or_else(|| {
                        EditorError::new(
                            "NOT_FOUND",
                            "Sequence does not exist in the fixed revision",
                        )
                    })?;
                let fps = sequence.fps.clone();
                let (width, height) = (sequence.width, sequence.height);
                let revision = input.snapshot.revision.clone();
                let output = directory.join("frame.png");
                // This first project preview uses the exact same composition/export
                // path as a full render. It is deliberately a job, not a real-time
                // seek API. The intermediate encoded movie is never published.
                let intermediate = directory.join("preview-source.mp4");
                let mut private_files = PreviewFiles {
                    intermediate: Some(intermediate.clone()),
                    output: output.clone(),
                    keep_output: false,
                };
                let plan = ProjectRenderAdapter::plan_resolved(&ResolvedProjectRenderRequest {
                    resolved: &snapshot,
                    output: intermediate.clone(),
                    container: Container::Mp4,
                    video_codec: VideoCodec::H264,
                    overwrite: false,
                })
                .map_err(|error| EditorError::new(error.code, error.message))?;
                let actual_time = select_project_frame_time(
                    &input.requested_time,
                    &fps,
                    input.alignment,
                    plan.media_request.duration,
                )
                .map_err(|error| EditorError::new(error.code, error.message))?;
                let result = (|| {
                    let rendered = ProjectRenderAdapter::execute(&plan, &backend, cancel, progress)
                        .map_err(|error| EditorError::new(error.code, error.message))?;
                    let selected = RationalTime::try_from(&actual_time)?;
                    let extracted = backend
                        .extract_frame(
                            &FrameExtractRequest {
                                input: intermediate.clone(),
                                output: output.clone(),
                                requested_time: MediaTime {
                                    ticks: selected.ticks(),
                                    timebase: selected.timebase(),
                                },
                                video_stream: None,
                                alignment: FrameAlignment::Exact,
                                overwrite: false,
                            },
                            cancel,
                            progress,
                        )
                        .map_err(media_error)?;
                    let observed = RationalTime::new(
                        extracted.actual_time.ticks,
                        extracted.actual_time.timebase,
                    )?;
                    if observed.cmp_exact(selected) != std::cmp::Ordering::Equal {
                        return Err(EditorError::new(
                            "ARTIFACT_VERIFICATION_FAILED",
                            "Rendered preview frame timestamp differs from the selected sequence time",
                        ));
                    }
                    if cancel.is_cancelled() {
                        return Err(EditorError::new(
                            "JOB_CANCELLED",
                            "Project preview was cancelled",
                        ));
                    }
                    let result = ProjectFrameJobResult {
                        revision,
                        sequence_id: input.sequence_id,
                        requested_time: input.requested_time,
                        actual_sequence_time: actual_time,
                        alignment: input.alignment,
                        quality: ProjectFrameQuality::EncodedProjectFrame,
                        asset_locking: locking_mode(input.asset_lock.as_ref()),
                        asset_lock: input.asset_lock,
                        width,
                        height,
                        text_layouts: rendered.text_layouts,
                        artifact: artifact(&output, "image/png")?,
                    };
                    result.validate()?;
                    Ok(json!(result))
                })();
                private_files.keep_output = result.is_ok();
                result
            }
            "preview.composited_frame" => {
                let input: PreparedProjectFrameInput = parse(job.input.clone())?;
                let private_inputs = PrivateInputs(directory.join("inputs"));
                let snapshot = resolve_visual_inputs(
                    root,
                    &private_inputs.0,
                    &input.snapshot,
                    &input.sequence_id,
                    input.asset_lock.as_ref(),
                    cancel,
                    progress,
                )?;
                let output = directory.join("frame.png");
                let mut private_files = PreviewFiles {
                    intermediate: None,
                    output: output.clone(),
                    keep_output: false,
                };
                let plan =
                    ProjectRenderAdapter::plan_frame_resolved(&ResolvedProjectFrameRequest {
                        resolved: &snapshot,
                        requested_time: input.requested_time.clone(),
                        alignment: input.alignment,
                        output: output.clone(),
                        overwrite: false,
                    })
                    .map_err(|error| EditorError::new(error.code, error.message))?;
                let rendered = backend
                    .render_timeline_frame(&plan.media_request, cancel, progress)
                    .map_err(media_error)?;
                let observed = RationalTime::new(
                    rendered.actual_sequence_time.ticks,
                    rendered.actual_sequence_time.timebase,
                )?;
                if observed.cmp_exact(RationalTime::try_from(&plan.actual_sequence_time)?)
                    != std::cmp::Ordering::Equal
                {
                    return Err(EditorError::new(
                        "ARTIFACT_VERIFICATION_FAILED",
                        "Composited frame timestamp differs from the selected sequence time",
                    ));
                }
                if cancel.is_cancelled() {
                    return Err(EditorError::new(
                        "JOB_CANCELLED",
                        "Project preview was cancelled",
                    ));
                }
                let result = CompositedProjectFrameJobResult {
                    revision: plan.revision,
                    sequence_id: plan.sequence_id,
                    requested_time: input.requested_time,
                    actual_sequence_time: plan.actual_sequence_time,
                    alignment: input.alignment,
                    quality: CompositedProjectFrameQuality::CompositedProjectFrame,
                    asset_locking: locking_mode(input.asset_lock.as_ref()),
                    asset_lock: input.asset_lock,
                    width: rendered.width,
                    height: rendered.height,
                    text_layouts: rendered.text_layouts,
                    artifact: artifact(&output, "image/png")?,
                };
                result.validate()?;
                private_files.keep_output = true;
                Ok(json!(result))
            }
            "preview.project_range" => {
                let input: PreparedProjectRangeInput = parse(job.input.clone())?;
                let private_inputs = PrivateInputs(directory.join("inputs"));
                let snapshot = resolve_inputs(
                    root,
                    &private_inputs.0,
                    &input.snapshot,
                    &input.sequence_id,
                    input.asset_lock.as_ref(),
                    cancel,
                    progress,
                )?;
                let output = directory.join("preview.mp4");
                let mut private_files = PreviewFiles {
                    intermediate: None,
                    output: output.clone(),
                    keep_output: false,
                };
                let plan =
                    ProjectRenderAdapter::plan_range_resolved(&ResolvedProjectRangeRequest {
                        resolved: &snapshot,
                        requested_range: input.requested_range.clone(),
                        alignment: input.alignment,
                        output: output.clone(),
                        overwrite: false,
                    })
                    .map_err(|error| EditorError::new(error.code, error.message))?;
                let rendered = backend
                    .render_timeline_range(&plan.media_request, cancel, progress)
                    .map_err(media_error)?;
                if cancel.is_cancelled() {
                    return Err(EditorError::new(
                        "JOB_CANCELLED",
                        "Project range preview was cancelled",
                    ));
                }
                let result = ProjectRangeJobResult {
                    revision: plan.revision,
                    sequence_id: plan.sequence_id,
                    requested_range: input.requested_range,
                    actual_sequence_range: plan.actual_sequence_range,
                    alignment: input.alignment,
                    quality: ProjectRangeQuality::EncodedProjectRange,
                    asset_locking: locking_mode(input.asset_lock.as_ref()),
                    asset_lock: input.asset_lock,
                    media: rendered.try_map_paths(public_path)?,
                    artifact: artifact(&output, "video/mp4")?,
                };
                result.validate()?;
                private_files.keep_output = true;
                Ok(json!(result))
            }
            "render.export" => {
                let request: ExportJobInput = parse(job.input.clone())?;
                let output = directory.join(output_name(request.container));
                let result = backend
                    .export(
                        &ExportRequest {
                            segments: request
                                .segments
                                .into_iter()
                                .map(|segment| segment.map_paths(PathBuf::from))
                                .collect(),
                            output: output.clone(),
                            container: request.container,
                            video: request.video,
                            audio: request.audio,
                            requested_features: vec![],
                            overwrite: false,
                        },
                        cancel,
                        progress,
                    )
                    .map_err(media_error)?;
                Ok(json!(MediaExportJobResult {
                    media: result.try_map_paths(public_path)?,
                    artifact: artifact(&output, output_mime(request.container))?,
                }))
            }
            "render.project" => {
                let input: PreparedProjectRenderInput = parse(job.input.clone())?;
                let private_inputs = PrivateInputs(directory.join("inputs"));
                let snapshot = resolve_inputs(
                    root,
                    &private_inputs.0,
                    &input.snapshot,
                    &input.sequence_id,
                    input.asset_lock.as_ref(),
                    cancel,
                    progress,
                )?;
                let output = directory.join(output_name(input.container));
                let revision = input.snapshot.revision.clone();
                let request = ResolvedProjectRenderRequest {
                    resolved: &snapshot,
                    output: output.clone(),
                    container: input.container,
                    video_codec: input.video_codec,
                    overwrite: false,
                };
                let plan = ProjectRenderAdapter::plan_resolved(&request)
                    .map_err(|e| EditorError::new(e.code, e.message))?;
                let result = ProjectRenderAdapter::execute(&plan, &backend, cancel, progress)
                    .map_err(|e| EditorError::new(e.code, e.message))?;
                let result = ProjectRenderJobResult {
                    revision,
                    asset_locking: locking_mode(input.asset_lock.as_ref()),
                    asset_lock: input.asset_lock,
                    media: result.try_map_paths(public_path)?,
                    artifact: artifact(&output, output_mime(input.container))?,
                };
                result.validate()?;
                Ok(json!(result))
            }
            "artifact.verify" => {
                let input: VerifyRequest = parse(job.input.clone())?;
                Ok(json!(
                    backend
                        .verify(&input, cancel, progress)
                        .map_err(media_error)?
                ))
            }
            _ => Err(EditorError::new(
                "CAPABILITY_UNAVAILABLE",
                "Worker cannot execute this job kind",
            )),
        }
    })()?;
    if let Some(receipt) = receipt {
        let bytes = receipts::finish(receipt, &backend, &result)?;
        if cancel.is_cancelled() {
            return Err(EditorError::new(
                "JOB_CANCELLED",
                "Cancelled before execution receipt",
            ));
        }
        let receipt_path = directory.join("execution.json");
        #[cfg(unix)]
        opencut_media::OutputAdmission::write(admission.as_ref(), &receipt_path, &bytes)
            .map_err(media_error)?;
        #[cfg(not(unix))]
        {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(receipt_path)
                .map_err(io_error)?;
            file.write_all(&bytes).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
        }
        #[cfg(unix)]
        fs::File::open(&directory)
            .and_then(|f| f.sync_all())
            .map_err(io_error)?;
    }
    Ok(result)
}

fn locking_mode(lock: Option<&AssetLockManifest>) -> AssetLockingMode {
    if lock.is_some() {
        AssetLockingMode::MediaContentLocked
    } else {
        AssetLockingMode::LegacyUnlocked
    }
}

// The native boundary interprets a wire location as a local path.
fn public_path(path: PathBuf) -> Result<String, EditorError> {
    path.into_os_string().into_string().map_err(|_| {
        EditorError::new(
            "INVALID_ARGUMENT",
            "A public media location must be valid UTF-8",
        )
    })
}

fn artifact(path: &Path, mime: &str) -> Result<ArtifactDescriptor, EditorError> {
    let mut file = fs::File::open(path).map_err(io_error)?;
    let size = file.metadata().map_err(io_error)?.len();
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(ArtifactDescriptor {
        path: public_path(path.to_path_buf())?,
        mime_type: mime.into(),
        size,
        content_hash: format!("{:x}", hash.finalize()),
    })
}
fn output_name(container: Container) -> &'static str {
    match container {
        Container::Mp4 => "render.mp4",
        Container::Matroska => "render.mkv",
    }
}
fn output_mime(container: Container) -> &'static str {
    match container {
        Container::Mp4 => "video/mp4",
        Container::Matroska => "video/x-matroska",
    }
}
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, EditorError> {
    serde_json::from_value(value).map_err(|e| EditorError::new("INVALID_ARGUMENT", e.to_string()))
}
fn media_error(error: opencut_media::MediaError) -> EditorError {
    EditorError::new(error.code(), error.to_string())
}
fn io_error(error: std::io::Error) -> EditorError {
    EditorError::new("MEDIA_IO_ERROR", error.to_string())
}
#[cfg(test)]
mod tests;
