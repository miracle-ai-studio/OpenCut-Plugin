//! One parent-owned process, pipe, deadline/RSS monitor and teardown for native
//! media execution. Typed audio/visual adapters own their admission and receipts.
pub(crate) mod monitor;
mod platform;
pub(crate) mod wire;
use opencut_contracts::EditorError;
pub(crate) use platform::system_identity;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fmt::Write as _,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};
#[derive(Clone, Copy)]
pub(crate) enum Domain {
    Visual,
    Audio,
    SourceArt,
    SourcePreview,
    Probe,
}
impl Domain {
    pub fn protocol(self, e: EditorError) -> EditorError {
        if e.code == "NATIVE_PROTOCOL_ERROR" {
            EditorError::new(
                match self {
                    Self::Visual => "VISUAL_PROTOCOL_ERROR",
                    Self::Audio => "AUDIO_PROTOCOL_ERROR",
                    Self::SourceArt => "SOURCE_ART_PROTOCOL_ERROR",
                    Self::SourcePreview => "SOURCE_PREVIEW_PROTOCOL_ERROR",
                    Self::Probe => "MEDIA_INVALID_TOOL_OUTPUT",
                },
                e.message,
            )
        } else if e.code == "NATIVE_PROCESS_FAILED" {
            EditorError::new(
                match self {
                    Self::Visual => "INVALID_VISUAL_PLAN",
                    Self::Audio => "AUDIO_HOST_FAILED",
                    Self::SourceArt => "SOURCE_ART_DECODE_FAILED",
                    Self::SourcePreview => "SOURCE_PREVIEW_DECODE_FAILED",
                    Self::Probe => "MEDIA_PROCESS_FAILED",
                },
                e.message,
            )
        } else {
            e
        }
    }
    pub fn time_code(self) -> &'static str {
        match self {
            Self::Visual => "VISUAL_TIME_LIMIT",
            Self::Audio => "AUDIO_TIME_LIMIT",
            Self::SourceArt => "SOURCE_ART_TIME_LIMIT",
            Self::SourcePreview => "SOURCE_PREVIEW_TIME_LIMIT",
            Self::Probe => "MEDIA_BACKPRESSURE",
        }
    }
    pub fn rss_code(self) -> &'static str {
        match self {
            Self::Visual => "VISUAL_BUDGET_EXCEEDED",
            Self::Audio => "AUDIO_BUDGET_EXCEEDED",
            Self::SourceArt => "SOURCE_ART_BUDGET_EXCEEDED",
            Self::SourcePreview => "SOURCE_PREVIEW_BUDGET_EXCEEDED",
            Self::Probe => "MEDIA_OUTPUT_LIMIT_EXCEEDED",
        }
    }
    pub fn changed(self) -> EditorError {
        EditorError::new(
            match self {
                Self::Visual => "VISUAL_ENGINE_CHANGED",
                Self::Audio => "AUDIO_ENGINE_CHANGED",
                Self::SourceArt => "SOURCE_ART_ENGINE_CHANGED",
                Self::SourcePreview => "SOURCE_PREVIEW_ENGINE_CHANGED",
                Self::Probe => "ARTIFACT_VERIFICATION_FAILED",
            },
            "Configured native worker changed during execution",
        )
    }
}
pub(crate) fn invalid(message: impl Into<String>) -> EditorError {
    EditorError::new("NATIVE_PROCESS_FAILED", message)
}
pub(crate) fn file_hash(path: &Path) -> Result<String, EditorError> {
    let mut input =
        std::fs::File::open(path).map_err(|_| invalid("Cannot open native runtime identity"))?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut bytes = [0; 65536];
    loop {
        let n = input
            .read(&mut bytes)
            .map_err(|_| invalid("Cannot hash native runtime identity"))?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    let digest = hash.finish();
    let mut hex = String::with_capacity(64);
    for byte in digest.as_ref() {
        write!(&mut hex, "{byte:02x}").expect("Writing a digest to String cannot fail");
    }
    Ok(hex)
}

#[cfg(test)]
mod file_hash_tests;
pub(crate) fn library_identities()
-> Result<Vec<opencut_contracts::renderer_identity::NativeLibraryIdentity>, EditorError> {
    opencut_media::native_decode::identity::libraries()
        .map_err(crate::media::execution_error)?
        .into_iter()
        .map(|lib| {
            Ok(
                opencut_contracts::renderer_identity::NativeLibraryIdentity {
                    name: lib.name.into(),
                    version: lib.version,
                    configuration_sha256: format!(
                        "{:x}",
                        Sha256::digest(lib.configuration.as_bytes())
                    ),
                    binary_sha256: file_hash(&lib.path)?,
                },
            )
        })
        .collect()
}
pub(crate) struct NativeQuota {
    pub profile: String,
    pub max_rss_bytes: String,
    pub observed_peak_rss_bytes: String,
    pub monitor_interval_ms: u32,
    pub elapsed_ms: u64,
    pub hard_memory_enforced: bool,
}
macro_rules! quota_conversion {
    ($target:path) => {
        impl From<NativeQuota> for $target {
            fn from(q: NativeQuota) -> Self {
                Self {
                    profile: q.profile,
                    max_rss_bytes: q.max_rss_bytes,
                    observed_peak_rss_bytes: q.observed_peak_rss_bytes,
                    monitor_interval_ms: q.monitor_interval_ms,
                    elapsed_ms: q.elapsed_ms,
                    hard_memory_enforced: q.hard_memory_enforced,
                }
            }
        }
    };
}
quota_conversion!(opencut_editor_application::classic_audio::AudioQuotaReceipt);
quota_conversion!(opencut_editor_application::classic_visual::VisualQuotaReceipt);

pub(crate) struct ProcessConfig {
    pub worker: PathBuf,
    pub max_rss_bytes: u64,
    pub lifetime_deadline: Duration,
    pub operation_deadline: Duration,
    pub progress: Option<Arc<Mutex<Instant>>>,
    pub strict_cgroup: Option<PathBuf>,
    pub domain: Domain,
}
pub(crate) struct WorkerProcess {
    config: ProcessConfig,
    input: Option<ChildStdin>,
    pub monitor: monitor::Monitor,
    receiver: Option<mpsc::Receiver<Result<wire::Packet, EditorError>>>,
    reader: Option<JoinHandle<()>>,
    diagnostics: Option<JoinHandle<()>>,
    pub expected_hash: String,
}
impl WorkerProcess {
    #[cfg(not(unix))]
    pub fn start(_: ProcessConfig) -> Result<Self, EditorError> {
        Err(EditorError::new(
            "CAPABILITY_UNAVAILABLE",
            "Native process lifecycle requires a supported Unix host",
        ))
    }
    #[cfg(unix)]
    pub fn start(config: ProcessConfig) -> Result<Self, EditorError> {
        use std::os::unix::process::CommandExt;
        if !config.worker.is_absolute()
            || config.max_rss_bytes == 0
            || config.lifetime_deadline.is_zero()
            || config.operation_deadline.is_zero()
        {
            return Err(config
                .domain
                .protocol(invalid("Invalid native process admission")));
        }
        let expected_hash = file_hash(&config.worker).map_err(|e| config.domain.protocol(e))?;
        let mut child = Command::new(&config.worker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|_| {
                EditorError::new(
                    "CAPABILITY_UNAVAILABLE",
                    "Cannot start configured native worker",
                )
            })?;
        let input = child.stdin.take().expect("piped native input");
        let stdout = child.stdout.take().expect("piped native output");
        let stderr = child.stderr.take().expect("piped native diagnostics");
        let monitor = monitor::Monitor::new_for(
            child,
            config.max_rss_bytes,
            config.lifetime_deadline,
            config.progress.clone(),
            config.domain,
        );
        monitor.frame(Some(Instant::now() + config.operation_deadline));
        if let Some(path) = &config.strict_cgroup {
            attach_cgroup(path, monitor.pid(), config.max_rss_bytes)
                .map_err(|e| config.domain.protocol(e))?;
        }
        let (sender, receiver) = mpsc::sync_channel(2);
        let reader = std::thread::spawn(move || {
            let mut input = BufReader::new(stdout);
            loop {
                let packet = wire::read(&mut input);
                let failed = packet.is_err();
                if sender.send(packet).is_err() || failed {
                    break;
                }
            }
        });
        let diagnostics = std::thread::spawn(move || {
            let mut input = stderr;
            let mut bytes = [0; 4096];
            while input.read(&mut bytes).is_ok_and(|n| n != 0) {}
        });
        Ok(Self {
            config,
            input: Some(input),
            monitor,
            receiver: Some(receiver),
            reader: Some(reader),
            diagnostics: Some(diagnostics),
            expected_hash,
        })
    }
    pub fn pid(&self) -> u32 {
        self.monitor.pid()
    }
    pub fn check(&self) -> Result<(), EditorError> {
        self.monitor
            .check()
            .map_err(|e| self.config.domain.protocol(e))
    }
    pub fn send(&mut self, meta: Value, data: &[u8]) -> Result<(), EditorError> {
        self.check()?;
        let result = wire::send(
            self.input
                .as_mut()
                .ok_or_else(|| wire::error("Closed native input"))?,
            meta,
            data,
        )
        .map_err(|e| self.config.domain.protocol(e));
        self.check()?;
        result
    }
    pub fn receive(&self) -> Result<Option<wire::Packet>, EditorError> {
        self.check()?;
        match self
            .receiver
            .as_ref()
            .ok_or_else(|| wire::error("Closed native output"))?
            .recv_timeout(Duration::from_millis(25))
        {
            Ok(packet) => {
                self.check()?;
                packet.map(Some).map_err(|e| self.config.domain.protocol(e))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(_) => Err(self
                .config
                .domain
                .protocol(wire::error("Native output closed"))),
        }
    }
    pub fn unchanged(&self) -> Result<(), EditorError> {
        if file_hash(&self.config.worker).map_err(|e| self.config.domain.protocol(e))?
            != self.expected_hash
        {
            return Err(self.config.domain.changed());
        }
        Ok(())
    }
    pub fn quota(&self) -> NativeQuota {
        self.monitor.quota(
            self.config.max_rss_bytes,
            self.config.strict_cgroup.is_some(),
        )
    }
    pub fn complete(&mut self) -> Result<NativeQuota, EditorError> {
        self.input.take();
        if !self
            .monitor
            .reap()
            .map_err(|e| self.config.domain.protocol(e))?
            .success()
        {
            return Err(self
                .config
                .domain
                .protocol(wire::error("Native worker exited unsuccessfully")));
        }
        self.unchanged()?;
        Ok(self.quota())
    }
}
impl Drop for WorkerProcess {
    fn drop(&mut self) {
        self.input.take();
        self.monitor.stop();
        self.receiver.take();
        if let Some(t) = self.reader.take() {
            let _ = t.join();
        }
        if let Some(t) = self.diagnostics.take() {
            let _ = t.join();
        }
    }
}
fn attach_cgroup(path: &Path, pid: u32, max_rss: u64) -> Result<(), EditorError> {
    #[cfg(target_os = "linux")]
    {
        if !path.is_absolute() {
            return Err(invalid("Native cgroup must be absolute"));
        }
        let maximum = std::fs::read_to_string(path.join("memory.max"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .ok_or_else(|| invalid("Native cgroup lacks a hard memory limit"))?;
        if maximum == 0 || maximum > max_rss {
            return Err(invalid("Native cgroup exceeds memory admission"));
        }
        std::fs::write(path.join("cgroup.procs"), pid.to_string())
            .map_err(|_| invalid("Cannot attach native worker to cgroup"))?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, pid, max_rss);
        Err(EditorError::new(
            "CAPABILITY_UNAVAILABLE",
            "Hard cgroup quota requires Linux",
        ))
    }
}
