//! Local, bounded output analysis. A verified private copy and child admission
//! remain held until the native processes actually exit, including cancellation.
use opencut_contracts::EditorError;
use opencut_editor_application::quality::{MeasuredQuality, PreparedQuality};
use opencut_media::{
    CancellationToken, FfmpegAnalysisBackend, MediaError, MediaResult, OutputAdmission,
    OutputProcess,
};
use opencut_storage_budget::{AttemptLease, Budget, ReservedChild};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
fn error(message: &str) -> EditorError {
    EditorError::new("QUALITY_ANALYSIS_FAILED", message)
}
fn io(err: impl std::fmt::Display) -> EditorError {
    error(&err.to_string())
}
fn media(error: EditorError) -> MediaError {
    MediaError::Admission {
        code: error.code,
        message: error.message,
    }
}
#[derive(Debug)]
struct Admission {
    budget: Budget,
    attempt: AttemptLease,
    directory: PathBuf,
    pids: Arc<Mutex<BTreeSet<u32>>>,
}
struct Process {
    inner: Option<ReservedChild>,
    pid: u32,
    pids: Arc<Mutex<BTreeSet<u32>>>,
    limit: Option<u64>,
}
impl Drop for Process {
    fn drop(&mut self) {
        drop(self.inner.take());
        if let Ok(mut pids) = self.pids.lock() {
            pids.remove(&self.pid);
        }
    }
}
impl OutputProcess for Process {
    fn child(&mut self) -> &mut Child {
        self.inner.as_mut().expect("owned process").process()
    }
    fn output_limit(&self) -> Option<u64> {
        self.limit
    }
}
impl OutputAdmission for Admission {
    fn spawn(
        &self,
        command: Command,
        output: Option<&Path>,
    ) -> MediaResult<Box<dyn OutputProcess>> {
        let (mut inner, limit) = if let Some(output) = output {
            if output.parent() != Some(self.directory.as_path()) || output.exists() {
                return Err(MediaError::InvalidRequest(
                    "Quality metadata must be a new private child".into(),
                ));
            }
            let reserved = self
                .budget
                .reserve("media_metadata", 16_000_000)
                .map_err(media)?;
            let limit = reserved.reserved_bytes();
            (
                reserved.spawn_in_attempt(command, self.attempt.clone())?,
                Some(limit),
            )
        } else {
            (self.attempt.clone().spawn_readonly(command)?, None)
        };
        let pid = inner.process().id();
        self.pids
            .lock()
            .map_err(|_| MediaError::InvalidRequest("Quality process registry unavailable".into()))?
            .insert(pid);
        Ok(Box::new(Process {
            inner: Some(inner),
            pid,
            pids: self.pids.clone(),
            limit,
        }))
    }
    fn write(&self, _: &Path, _: &[u8]) -> MediaResult<()> {
        Err(MediaError::Unsupported(
            "Quality analysis does not publish arbitrary output".into(),
        ))
    }
}
fn hash_file(path: &Path) -> Result<String, EditorError> {
    let mut file = std::fs::File::open(path).map_err(io)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(io)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
#[cfg(unix)]
pub fn measure(
    database: &Path,
    artifact_root: &Path,
    input: &PreparedQuality,
    resolve_tools: impl FnOnce() -> Result<(PathBuf, PathBuf), EditorError>,
    mut checkpoint: impl FnMut() -> Result<(), EditorError> + Send,
) -> Result<MeasuredQuality, EditorError> {
    checkpoint()?;
    let started = Instant::now();
    let policy = &input.source.policy;
    // The published file stays on disk and is verified in 64KiB blocks below.
    // Its length is admitted by the existing disk reservation, not the visual
    // decoder's encoded-cache allowance. Deadline/RSS/metadata limits remain.
    let directory = tempfile::Builder::new()
        .prefix(".quality-")
        .tempdir_in(artifact_root)
        .map_err(io)?;
    let attempt = AttemptLease::acquire(directory.path()).map_err(io)?;
    let path = directory.path().join("verified-input");
    let mut source = crate::artifacts::ArtifactSource::open(
        artifact_root,
        &input.job_id,
        input.attempt,
        Path::new(&input.artifact.path),
    )?;
    if source.metadata().map_err(io)?.len() != input.artifact.size {
        return Err(error("Published output size differs"));
    }
    let mut copy = crate::budget::AdmittedFile::create(
        database,
        &path,
        "media_inspection",
        input.artifact.size,
        false,
    )?;
    let mut digest = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0; 65536];
    loop {
        checkpoint()?;
        if started.elapsed() >= Duration::from_millis(policy.deadline_ms) {
            return Err(error("Quality deadline exceeded during verification"));
        }
        let n = source.read(&mut buffer).map_err(io)?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or_else(|| error("Output length overflow"))?;
        if count > input.artifact.size {
            return Err(error("Published output grew"));
        }
        digest.update(&buffer[..n]);
        copy.append(&buffer[..n])?;
    }
    if count != input.artifact.size
        || format!("{:x}", digest.finalize()) != input.artifact.content_hash
    {
        return Err(error("Published output hash differs"));
    }
    copy.sync()?;
    drop(source);
    let (ffmpeg, ffprobe) = resolve_tools()?;
    let ffmpeg = crate::classic_video::resolve_tool(ffmpeg)?;
    let ffprobe = crate::classic_video::resolve_tool(ffprobe)?;
    let ffmpeg_path = ffmpeg.clone();
    let ffprobe_path = ffprobe.clone();
    let ffmpeg_sha256 = hash_file(&ffmpeg)?;
    let ffprobe_sha256 = hash_file(&ffprobe)?;
    let cancel = CancellationToken::new();
    let pids = Arc::new(Mutex::new(BTreeSet::new()));
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let outcome = std::thread::scope(|scope| {
        let cancellation = cancel.clone();
        let tracked = pids.clone();
        let monitor = scope.spawn(move || -> Result<(), EditorError> {
            loop {
                let state = (|| {
                    checkpoint()?;
                    if started.elapsed() >= Duration::from_millis(policy.deadline_ms) {
                        return Err(error("Quality analysis exceeded captured deadline"));
                    }
                    for pid in tracked
                        .lock()
                        .map_err(|_| error("Quality process registry unavailable"))?
                        .iter()
                    {
                        if crate::classic_video::resident_bytes(*pid)?
                            .is_some_and(|rss| rss > policy.max_rss_bytes)
                        {
                            return Err(error("Quality process exceeded captured RSS budget"));
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = state {
                    cancellation.cancel();
                    return Err(error);
                }
                match stopped.recv_timeout(Duration::from_millis(100)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        let admission = Arc::new(Admission {
            budget: Budget::new(
                database
                    .parent()
                    .ok_or_else(|| error("Workspace root unavailable"))?,
            ),
            attempt: attempt.clone(),
            directory: directory.path().into(),
            pids,
        });
        let result = FfmpegAnalysisBackend::new(ffmpeg, ffprobe)
            .with_output_admission(admission, directory.path().into())
            .output_quality(&path, &cancel)
            .map_err(|e| EditorError::new(e.code(), e.to_string()));
        let _ = stop.send(());
        let monitored = monitor
            .join()
            .map_err(|_| error("Quality monitor stopped unexpectedly"))?;
        monitored?;
        result
    })?;
    if hash_file(&ffmpeg_path)? != ffmpeg_sha256 || hash_file(&ffprobe_path)? != ffprobe_sha256 {
        return Err(error("Quality executable identity changed during analysis"));
    }
    drop(copy);
    drop(attempt);
    drop(directory);
    Ok(MeasuredQuality {
        measurements: outcome,
        ffmpeg_sha256,
        ffprobe_sha256,
    })
}
#[cfg(not(unix))]
pub fn measure(
    _: &Path,
    _: &Path,
    _: &PreparedQuality,
    _: impl FnOnce() -> Result<(PathBuf, PathBuf), EditorError>,
    _: impl FnMut() -> Result<(), EditorError> + Send,
) -> Result<MeasuredQuality, EditorError> {
    Err(EditorError::new(
        "CAPABILITY_UNAVAILABLE",
        "Quality process admission requires Unix",
    ))
}

/// Native process observation shared by host-owned analysis monitors.
pub fn observed_process_rss(pid: u32) -> Result<Option<u64>, EditorError> {
    crate::classic_video::resident_bytes(pid)
}
