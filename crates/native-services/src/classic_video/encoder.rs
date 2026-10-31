use opencut_contracts::EditorError;
use opencut_editor_application::classic_visual::ClassicVisualRgbaSink;
use opencut_editor_application::media_video_jobs::{MAX_VIDEO_FRAMES, VideoExecutionLimit};
use opencut_media::{OutputAdmission, OutputProcess};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
fn err(message: &str) -> EditorError {
    EditorError::new("VIDEO_ENCODER_FAILED", message)
}
fn io(error: std::io::Error) -> EditorError {
    err(&error.to_string())
}
fn wait_input_writable(input: &ChildStdin) -> Result<(), EditorError> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mut descriptor = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // The encoder owns this live stdin handle throughout the wait. Keep
        // the original 10ms cancellation/deadline upper bound, but resume as
        // soon as FFmpeg drains the pipe instead of sleeping a whole tick.
        let ready = unsafe { libc::poll(&mut descriptor, 1, 10) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(io(error));
            }
        } else if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(err("Encoder input closed"));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = input;
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
fn media(error: opencut_media::MediaError) -> EditorError {
    match error {
        opencut_media::MediaError::Admission { code, message } => EditorError::new(code, message),
        other => err(&other.to_string()),
    }
}
/// Resolve once, execute and hash the exact file, and rehash after the attempt.
/// PATH resolution never delegates to a shell or executes an internal CLI.
pub fn resolve_tool(value: PathBuf) -> Result<PathBuf, EditorError> {
    if value.is_absolute() || value.components().count() != 1 {
        return checked_executable(&value);
    }
    let paths = std::env::var_os("PATH").ok_or_else(tool_unavailable)?;
    resolve_in_paths(&value, &paths)
}
fn resolve_in_paths(value: &Path, paths: &std::ffi::OsStr) -> Result<PathBuf, EditorError> {
    for directory in std::env::split_paths(paths) {
        if let Ok(executable) = checked_executable(&directory.join(value)) {
            return Ok(executable);
        }
    }
    Err(tool_unavailable())
}
fn tool_unavailable() -> EditorError {
    EditorError::new(
        "CAPABILITY_UNAVAILABLE",
        "Required video encoder/probe is not an executable regular file",
    )
}
fn checked_executable(path: &Path) -> Result<PathBuf, EditorError> {
    let resolved = std::fs::canonicalize(path).map_err(|_| tool_unavailable())?;
    let metadata = std::fs::metadata(&resolved).map_err(|_| tool_unavailable())?;
    if !metadata.is_file() {
        return Err(tool_unavailable());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(tool_unavailable());
        }
    }
    Ok(resolved)
}

fn hash(
    path: &Path,
    checkpoint: &mut dyn FnMut(bool) -> Result<(), EditorError>,
) -> Result<String, EditorError> {
    let mut file = File::open(path).map_err(io)?;
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        checkpoint(false)?;
        let n = file.read(&mut bytes).map_err(io)?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
        checkpoint(true)?;
    }
    Ok(format!("{:x}", digest.finalize()))
}
#[derive(Clone)]
pub struct ClassicVideoEncoderConfig {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub output: PathBuf,
    pub audio: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps_numerator: u32,
    pub fps_denominator: u32,
    pub frame_count: u64,
    pub max_output_bytes: u64,
    pub max_rss_bytes: u64,
    pub execution_limit: VideoExecutionLimit,
}
pub struct ClassicVideoEncoderObservation {
    pub backend: opencut_editor_application::export_video::VideoEncoderBackend,
    pub ffmpeg_sha256: String,
    pub ffprobe_sha256: String,
    pub video_frames: u64,
    pub video_duration_seconds: f64,
    pub audio_duration_seconds: f64,
}
pub struct SourcePreviewEncoderObservation {
    pub ffmpeg_sha256: String,
    pub ffprobe_sha256: String,
    pub time_base: opencut_contracts::source_preview::SourcePreviewTimeBase,
}
pub struct ClassicVideoEncoder<F: FnMut(bool) -> Result<(), EditorError>> {
    process: Option<Box<dyn OutputProcess>>,
    input: Option<ChildStdin>,
    stderr: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    config: ClassicVideoEncoderConfig,
    checkpoint: F,
    started: Instant,
    last_rss: Instant,
    observed_output_bytes: u64,
    ffmpeg_hash: String,
    ffprobe_hash: String,
    next_frame: u64,
    offset: u64,
    export_options: Option<opencut_contracts::export_video::ExportVideoOptions>,
    source_preview: bool,
}
impl<F: FnMut(bool) -> Result<(), EditorError>> ClassicVideoEncoder<F> {
    #[cfg(unix)]
    pub fn start(
        config: ClassicVideoEncoderConfig,
        admission: &dyn OutputAdmission,
        checkpoint: F,
    ) -> Result<Self, EditorError> {
        Self::start_internal(config, admission, checkpoint, None, false)
    }
    #[cfg(unix)]
    pub fn start_export(
        config: ClassicVideoEncoderConfig,
        options: opencut_contracts::export_video::ExportVideoOptions,
        admission: &dyn OutputAdmission,
        checkpoint: F,
    ) -> Result<Self, EditorError> {
        Self::start_internal(config, admission, checkpoint, Some(options), false)
    }
    /// Source preparation supplies a maximum frame count first and seals the
    /// independently observed complete count at EOF. The output is lossless
    /// RGBA FFV1; it never consumes a project, original source path or audio.
    #[cfg(unix)]
    pub fn start_source_preview(
        config: ClassicVideoEncoderConfig,
        admission: &dyn OutputAdmission,
        checkpoint: F,
    ) -> Result<Self, EditorError> {
        Self::start_internal(config, admission, checkpoint, None, true)
    }
    #[cfg(unix)]
    fn start_internal(
        config: ClassicVideoEncoderConfig,
        admission: &dyn OutputAdmission,
        mut checkpoint: F,
        export_options: Option<opencut_contracts::export_video::ExportVideoOptions>,
        source_preview: bool,
    ) -> Result<Self, EditorError> {
        if config.width == 0
            || config.height == 0
            || (!source_preview && (config.width % 2 != 0 || config.height % 2 != 0))
            || config.fps_numerator == 0
            || config.fps_denominator == 0
            || config.frame_count == 0
            || config.frame_count > MAX_VIDEO_FRAMES
            || config.max_output_bytes == 0
            || config.max_output_bytes > i64::MAX as u64
            || config.execution_limit.milliseconds() == 0
        {
            return Err(err("Invalid basic CFR encoder admission"));
        }
        let ffmpeg_hash = hash(&config.ffmpeg, &mut checkpoint)?;
        let ffprobe_hash = hash(&config.ffprobe, &mut checkpoint)?;
        let mut command = Command::new(&config.ffmpeg);
        if source_preview {
            command
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-n",
                    "-protocol_whitelist",
                    "file,pipe",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgba",
                    "-video_size",
                ])
                .arg(format!("{}x{}", config.width, config.height))
                .arg("-framerate")
                .arg(format!(
                    "{}/{}",
                    config.fps_numerator, config.fps_denominator
                ))
                .args([
                    "-i",
                    "pipe:0",
                    "-an",
                    "-sn",
                    "-dn",
                    "-c:v",
                    "ffv1",
                    "-level",
                    "3",
                    "-g",
                    "1",
                    "-coder",
                    "0",
                    "-context",
                    "0",
                    "-slicecrc",
                    "1",
                    "-slices",
                    "4",
                    "-threads",
                    "2",
                    "-filter_threads",
                    "1",
                    "-pix_fmt",
                    "bgra",
                    "-f",
                    "matroska",
                ])
                .arg(&config.output);
        } else if let Some(options) = &export_options {
            crate::export_video::append_arguments(&mut command, &config, options)?;
        } else {
            command
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-n",
                    "-protocol_whitelist",
                    "file,pipe",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgb24",
                    "-video_size",
                ])
                .arg(format!("{}x{}", config.width, config.height))
                .arg("-framerate")
                .arg(format!(
                    "{}/{}",
                    config.fps_numerator, config.fps_denominator
                ))
                .args(["-i", "pipe:0", "-i"])
                .arg(&config.audio)
                .args([
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-crf",
                    "18",
                    "-preset",
                    "medium",
                    "-threads",
                    "2",
                    "-filter_threads",
                    "1",
                    "-filter_complex_threads",
                    "1",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "192000",
                    "-ar",
                    "48000",
                    "-ac",
                    "2",
                    "-movflags",
                    "+faststart",
                    "-f",
                    "mp4",
                ]);
            crate::export_video::append_sdr_color_arguments(&mut command);
            command.arg(&config.output);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let started = Instant::now();
        let mut process = admission
            .spawn(command, Some(&config.output))
            .map_err(media)?;
        let input = process
            .child()
            .stdin
            .take()
            .ok_or_else(|| err("Missing encoder input"))?;
        nonblocking(&input)?;
        let stderr = process
            .child()
            .stderr
            .take()
            .ok_or_else(|| err("Missing encoder diagnostics"))?;
        let stderr = thread::spawn(move || bounded_read(stderr, 16384));
        Ok(Self {
            process: Some(process),
            input: Some(input),
            stderr: Some(stderr),
            config,
            checkpoint,
            started,
            last_rss: started,
            observed_output_bytes: 0,
            ffmpeg_hash,
            ffprobe_hash,
            next_frame: 0,
            offset: 0,
            export_options,
            source_preview,
        })
    }
    fn check(&mut self) -> Result<(), EditorError> {
        (self.checkpoint)(false)?;
        if !self.config.execution_limit.renews_on_progress()
            && self.started.elapsed()
                >= Duration::from_millis(self.config.execution_limit.milliseconds())
        {
            return Err(EditorError::new(
                "VIDEO_TIME_LIMIT",
                "Video encoding exceeded captured deadline",
            ));
        }
        if let Ok(metadata) = std::fs::metadata(&self.config.output) {
            if metadata.len() > self.config.max_output_bytes {
                return Err(EditorError::new(
                    "VIDEO_BUDGET_EXCEEDED",
                    "MP4 exceeded captured output admission",
                ));
            }
            if metadata.len() > self.observed_output_bytes {
                self.observed_output_bytes = metadata.len();
                self.completed_work()?;
            }
        }
        if self.last_rss.elapsed() >= Duration::from_millis(100) {
            self.last_rss = Instant::now();
            if let Some(process) = self.process.as_mut() {
                if let Some(rss) = resident_bytes(process.child().id())? {
                    if rss > self.config.max_rss_bytes {
                        return Err(EditorError::new(
                            "VIDEO_BUDGET_EXCEEDED",
                            "Encoder RSS exceeded captured admission",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    fn completed_work(&mut self) -> Result<(), EditorError> {
        (self.checkpoint)(true)?;
        Ok(())
    }
    pub fn finish_frame(&mut self) -> Result<(), EditorError> {
        self.check()?;
        if self.next_frame >= self.config.frame_count
            || self.offset != u64::from(self.config.width) * u64::from(self.config.height) * 4
        {
            return Err(err("Encoder frame lacks complete validated RGBA"));
        }
        self.next_frame += 1;
        self.offset = 0;
        self.completed_work()?;
        Ok(())
    }
    #[cfg(unix)]
    fn finish_probe(
        &mut self,
        admission: &dyn OutputAdmission,
        frame_observer: Option<&mut dyn FnMut(u64, i64, i64) -> Result<(), EditorError>>,
    ) -> Result<Value, EditorError> {
        if self.next_frame != self.config.frame_count || self.offset != 0 {
            return Err(err("Encoder lacks all complete CFR frames"));
        }
        drop(self.input.take());
        let status = loop {
            self.check()?;
            if let Some(status) = self
                .process
                .as_mut()
                .unwrap()
                .child()
                .try_wait()
                .map_err(io)?
            {
                break status;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let _diagnostics = self
            .stderr
            .take()
            .unwrap()
            .join()
            .map_err(|_| err("Encoder diagnostic reader failed"))?
            .map_err(io)?;
        let ceiling = self.process.as_ref().unwrap().output_limit();
        if !status.success() {
            use std::os::unix::process::ExitStatusExt;
            if status.signal() == Some(libc::SIGXFSZ)
                || ceiling.is_some_and(|limit| {
                    std::fs::metadata(&self.config.output).is_ok_and(|m| m.len() >= limit)
                })
            {
                return Err(EditorError::new(
                    "VIDEO_BUDGET_EXCEEDED",
                    "Encoder exhausted admitted output reservation",
                ));
            }
            return Err(err("FFmpeg encoding failed"));
        }
        drop(self.process.take());
        self.check()?;
        self.completed_work()?;
        let probe = probe_observed(
            &self.config.ffprobe,
            &self.config.output,
            admission,
            self.config
                .execution_limit
                .remaining_after(self.started.elapsed()),
            self.config.max_rss_bytes,
            &mut self.checkpoint,
            frame_observer,
        )?;
        self.completed_work()?;
        self.check()?;
        if hash(&self.config.ffmpeg, &mut self.checkpoint)? != self.ffmpeg_hash
            || hash(&self.config.ffprobe, &mut self.checkpoint)? != self.ffprobe_hash
        {
            return Err(EditorError::new(
                "EXECUTION_IDENTITY_CHANGED",
                "Encoder/probe binary changed during execution",
            ));
        }
        Ok(probe)
    }
    #[cfg(unix)]
    pub fn finish_source_preview(
        mut self,
        actual_frames: u64,
        admission: &dyn OutputAdmission,
        frame_observer: &mut dyn FnMut(u64, i64, i64) -> Result<(), EditorError>,
    ) -> Result<SourcePreviewEncoderObservation, EditorError> {
        if !self.source_preview
            || actual_frames == 0
            || actual_frames > self.config.frame_count
            || actual_frames != self.next_frame
            || self.offset != 0
        {
            return Err(err(
                "Source encoder lacks its complete actual frame sequence",
            ));
        }
        self.config.frame_count = actual_frames;
        let observed = self.finish_probe(admission, Some(frame_observer))?;
        let streams = observed["streams"]
            .as_array()
            .ok_or_else(|| err("Missing source proxy stream"))?;
        if streams.len() != 1 {
            return Err(err("Source proxy must contain only one video stream"));
        }
        let video = &streams[0];
        if video["codec_type"] != "video"
            || video["codec_name"] != "ffv1"
            || video["pix_fmt"] != "bgra"
            || video["width"].as_u64() != Some(u64::from(self.config.width))
            || video["height"].as_u64() != Some(u64::from(self.config.height))
            || video["nb_read_frames"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                != Some(actual_frames)
        {
            return Err(err(
                "Lossless source proxy changed its observed format or count",
            ));
        }
        let base = video["time_base"]
            .as_str()
            .and_then(|s| s.split_once('/'))
            .ok_or_else(|| err("Source proxy has no observed time base"))?;
        let numerator = base
            .0
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| err("Invalid source proxy time base"))?;
        let denominator = base
            .1
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| err("Invalid source proxy time base"))?;
        Ok(SourcePreviewEncoderObservation {
            ffmpeg_sha256: self.ffmpeg_hash.clone(),
            ffprobe_sha256: self.ffprobe_hash.clone(),
            time_base: opencut_contracts::source_preview::SourcePreviewTimeBase {
                numerator,
                denominator,
            },
        })
    }
    #[cfg(unix)]
    pub fn finish(
        mut self,
        admission: &dyn OutputAdmission,
    ) -> Result<ClassicVideoEncoderObservation, EditorError> {
        if self.source_preview {
            return Err(err("Source proxy requires observed source completion"));
        }
        let probe = self.finish_probe(admission, None)?;
        if let Some(options) = &self.export_options {
            let observed = crate::export_video::verify_probe(&probe, &self.config, options)?;
            return Ok(ClassicVideoEncoderObservation {
                backend: match options.format {
                    opencut_contracts::export_video::ExportFormat::Mp4 => {
                        opencut_editor_application::export_video::VideoEncoderBackend::FfmpegLibx264
                    }
                    opencut_contracts::export_video::ExportFormat::Webm => {
                        opencut_editor_application::export_video::VideoEncoderBackend::FfmpegLibvpxVp9
                    }
                },
                ffmpeg_sha256: self.ffmpeg_hash.clone(),
                ffprobe_sha256: self.ffprobe_hash.clone(),
                video_frames: observed.frames,
                video_duration_seconds: observed.video_seconds,
                audio_duration_seconds: observed.audio_seconds.unwrap_or(0.0),
            });
        }
        let streams = probe
            .get("streams")
            .and_then(Value::as_array)
            .ok_or_else(|| err("Missing observed MP4 streams"))?;
        if streams.len() != 2 {
            return Err(err("MP4 must have exactly shared video and audio streams"));
        }
        let video = streams
            .iter()
            .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))
            .ok_or_else(|| err("Missing MP4 video"))?;
        let audio = streams
            .iter()
            .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio"))
            .ok_or_else(|| err("Missing MP4 audio"))?;
        crate::export_video::verify_sdr_color(video)?;
        let count = video
            .get("nb_read_frames")
            .and_then(Value::as_str)
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| err("Missing observed decoded frame count"))?;
        let duration = |s: &Value| {
            s.get("duration")
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|n| n.is_finite() && *n > 0.0)
                .ok_or_else(|| err("Missing observed stream duration"))
        };
        if count != self.config.frame_count
            || video.get("codec_name").and_then(Value::as_str) != Some("h264")
            || video.get("pix_fmt").and_then(Value::as_str) != Some("yuv420p")
            || video.get("width").and_then(Value::as_u64) != Some(u64::from(self.config.width))
            || video.get("height").and_then(Value::as_u64) != Some(u64::from(self.config.height))
            || audio.get("codec_name").and_then(Value::as_str) != Some("aac")
            || audio.get("sample_rate").and_then(Value::as_str) != Some("48000")
            || audio.get("channels").and_then(Value::as_u64) != Some(2)
        {
            return Err(err(
                "Observed MP4 stream format differs from fixed encoder profile",
            ));
        }
        Ok(ClassicVideoEncoderObservation {
            backend: opencut_editor_application::export_video::VideoEncoderBackend::FfmpegLibx264,
            ffmpeg_sha256: self.ffmpeg_hash.clone(),
            ffprobe_sha256: self.ffprobe_hash.clone(),
            video_frames: count,
            video_duration_seconds: duration(video)?,
            audio_duration_seconds: duration(audio)?,
        })
    }
}
impl<F: FnMut(bool) -> Result<(), EditorError>> ClassicVisualRgbaSink for ClassicVideoEncoder<F> {
    fn chunk(&mut self, offset: u64, bytes: &[u8]) -> Result<(), EditorError> {
        self.check()?;
        let frame_bytes = u64::from(self.config.width) * u64::from(self.config.height) * 4;
        if self.next_frame >= self.config.frame_count
            || offset != self.offset
            || bytes.is_empty()
            || bytes.len() > 65536
            || !bytes.len().is_multiple_of(4)
            || offset
                .checked_add(bytes.len() as u64)
                .is_none_or(|end| end > frame_bytes)
        {
            return Err(err("RGBA encoder stream changed frame order"));
        }
        if self.source_preview {
            self.write_input(bytes)?;
        } else {
            // All video outputs use opaque yuv420p. Preserve the original RGB
            // bytes while avoiding alpha in FFmpeg's queued raw frames/packets.
            // Source proxies keep their complete lossless RGBA input above.
            let mut rgb = [0u8; 65536 / 4 * 3];
            for (pixel, packed) in bytes.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
                packed.copy_from_slice(&pixel[..3]);
            }
            self.write_input(&rgb[..bytes.len() / 4 * 3])?;
        }
        self.offset += bytes.len() as u64;
        Ok(())
    }
}
impl<F: FnMut(bool) -> Result<(), EditorError>> ClassicVideoEncoder<F> {
    fn write_input(&mut self, mut remaining: &[u8]) -> Result<(), EditorError> {
        while !remaining.is_empty() {
            match self
                .input
                .as_mut()
                .ok_or_else(|| err("Encoder input closed"))?
                .write(remaining)
            {
                Ok(0) => return Err(err("Encoder input closed")),
                Ok(n) => remaining = &remaining[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    wait_input_writable(
                        self.input
                            .as_ref()
                            .ok_or_else(|| err("Encoder input closed"))?,
                    )?;
                    // The chunk was checked before entering this bounded
                    // nonblocking write. Recheck after every wait; successful
                    // partial writes need no duplicate database round trip.
                    self.check()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => self.check()?,
                Err(e) => return Err(io(e)),
            }
        }
        Ok(())
    }
}
impl<F: FnMut(bool) -> Result<(), EditorError>> Drop for ClassicVideoEncoder<F> {
    fn drop(&mut self) {
        drop(self.input.take());
        drop(self.process.take());
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
    }
}
fn bounded_read(mut reader: impl Read, max: usize) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            return Ok(retained);
        }
        let take = n.min(max.saturating_sub(retained.len()));
        retained.extend_from_slice(&chunk[..take]);
    }
}
#[cfg(unix)]
fn nonblocking(input: &ChildStdin) -> Result<(), EditorError> {
    use std::os::fd::AsRawFd;
    let fd = input.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok(())
}
// Compact ffprobe sections are one bounded line each. Decode every frame as
// before;discard frame rows after counting them and retain only final metadata.
// See https://ffmpeg.org/ffprobe.html#compact_002c-csv and -count_frames.
fn read_probe_output(
    reader: impl Read,
    progress: mpsc::SyncSender<u64>,
) -> Result<Value, EditorError> {
    read_probe_output_observed(reader, progress, None)
}
fn read_probe_output_observed(
    reader: impl Read,
    progress: mpsc::SyncSender<u64>,
    frames: Option<mpsc::SyncSender<(u64, i64, i64)>>,
) -> Result<Value, EditorError> {
    let mut reader = BufReader::new(reader);
    let mut counts = BTreeMap::<u64, (String, u64)>::new();
    let mut streams = BTreeMap::<u64, Value>::new();
    let mut format = None;
    let mut decoded = 0u64;
    loop {
        let mut line = Vec::new();
        let length = reader
            .by_ref()
            .take(16_385)
            .read_until(b'\n', &mut line)
            .map_err(io)?;
        if length == 0 {
            break;
        }
        if length > 16_384 || line.last() != Some(&b'\n') {
            return Err(err("Probe section exceeds bounded framing"));
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| err("Invalid probe text"))?
            .trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('|');
        let section = parts.next().ok_or_else(|| err("Missing probe section"))?;
        let mut fields = BTreeMap::new();
        for part in parts {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            // Only scalar codec/count/clock fields and the existing WebM
            // duration tag are consumed. No arbitrary escaped metadata is used.
            if !matches!(
                key,
                "index"
                    | "stream_index"
                    | "media_type"
                    | "codec_type"
                    | "codec_name"
                    | "width"
                    | "height"
                    | "pix_fmt"
                    | "color_range"
                    | "color_space"
                    | "color_primaries"
                    | "color_transfer"
                    | "r_frame_rate"
                    | "avg_frame_rate"
                    | "sample_rate"
                    | "channels"
                    | "nb_read_frames"
                    | "duration"
                    | "tag:DURATION"
                    | "format_name"
                    | "pts"
                    | "time_base"
            ) {
                continue;
            }
            if value.contains('\\') || fields.insert(key, value).is_some() {
                return Err(err("Ambiguous probe field"));
            }
        }
        let integer = |key: &str| {
            fields
                .get(key)
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| err("Invalid probe integer"))
        };
        if section == "frame" {
            let index = integer("stream_index")?;
            let kind = *fields
                .get("media_type")
                .ok_or_else(|| err("Missing decoded frame type"))?;
            if !matches!(kind, "video" | "audio")
                || (counts.len() >= 2 && !counts.contains_key(&index))
            {
                return Err(err("Unexpected decoded stream"));
            }
            let counter = counts.entry(index).or_insert_with(|| (kind.into(), 0));
            if counter.0 != kind {
                return Err(err("Decoded stream type changed"));
            }
            counter.1 = counter
                .1
                .checked_add(1)
                .ok_or_else(|| err("Probe frame count overflow"))?;
            if let Some(frames) = &frames {
                if kind != "video" {
                    return Err(err("Source proxy contains a non-video frame"));
                }
                let pts = fields
                    .get("pts")
                    .and_then(|v| v.parse::<i64>().ok())
                    .ok_or_else(|| err("Missing observed encoded frame PTS"))?;
                let duration = fields
                    .get("duration")
                    .and_then(|v| v.parse::<i64>().ok())
                    .filter(|v| *v > 0)
                    .ok_or_else(|| err("Missing observed encoded frame duration"))?;
                // Unlike coalescible progress, exact frame observations cannot
                // be dropped. The owning loop drains this bounded queue and
                // drops its receiver before any reader join on failure.
                frames
                    .send((counter.1 - 1, pts, duration))
                    .map_err(|_| err("Source frame observer retired"))?;
            }
            decoded = decoded
                .checked_add(1)
                .ok_or_else(|| err("Probe progress overflow"))?;
            // Bounded,coalescible observations. A full queue never blocks the
            // reader or accumulates output;polling alone cannot send progress.
            let _ = progress.try_send(decoded);
        } else if section == "stream" || section == "format" {
            let mut value = serde_json::Map::new();
            for (key, field) in &fields {
                if *key == "tag:DURATION" {
                    value.insert("tags".into(), serde_json::json!({"DURATION":field}));
                } else if matches!(*key, "index" | "width" | "height" | "channels") {
                    value.insert((*key).into(), integer(key)?.into());
                } else {
                    value.insert((*key).into(), Value::String((*field).into()));
                }
            }
            if section == "stream" {
                let index = integer("index")?;
                if streams.len() >= 2 || streams.insert(index, Value::Object(value)).is_some() {
                    return Err(err("Unexpected probe stream inventory"));
                }
            } else if format.replace(Value::Object(value)).is_some() {
                return Err(err("Repeated probe container"));
            }
        } else if section != "side_data" {
            return Err(err("Unexpected probe section"));
        }
    }
    if streams.is_empty() || streams.len() != counts.len() {
        return Err(err("Decoded frame inventory is incomplete"));
    }
    for (index, stream) in &streams {
        let (kind, count) = counts
            .get(index)
            .ok_or_else(|| err("Unobserved output stream"))?;
        if stream["codec_type"].as_str() != Some(kind.as_str())
            || stream["nb_read_frames"]
                .as_str()
                .and_then(|v| v.parse::<u64>().ok())
                != Some(*count)
        {
            return Err(err("Probe decoded counts differ from observed frame rows"));
        }
    }
    Ok(
        serde_json::json!({"streams":streams.into_values().collect::<Vec<_>>(),"format":format.ok_or_else(|| err("Missing probe container"))?}),
    )
}
#[cfg(unix)]
fn probe(
    path: &Path,
    output: &Path,
    admission: &dyn OutputAdmission,
    limit: VideoExecutionLimit,
    max_rss_bytes: u64,
    checkpoint: &mut dyn FnMut(bool) -> Result<(), EditorError>,
) -> Result<Value, EditorError> {
    probe_observed(
        path,
        output,
        admission,
        limit,
        max_rss_bytes,
        checkpoint,
        None,
    )
}
#[cfg(unix)]
fn probe_observed(
    path: &Path,
    output: &Path,
    admission: &dyn OutputAdmission,
    limit: VideoExecutionLimit,
    max_rss_bytes: u64,
    checkpoint: &mut dyn FnMut(bool) -> Result<(), EditorError>,
    mut frame_observer: Option<&mut dyn FnMut(u64, i64, i64) -> Result<(), EditorError>>,
) -> Result<Value, EditorError> {
    checkpoint(false)?;
    let mut command = Command::new(path);
    command.args([
        "-v", "error", "-protocol_whitelist", "file,pipe", "-count_frames",
        "-show_frames", "-show_streams", "-show_format", "-show_entries",
        "frame=media_type,stream_index,pts,duration:stream=index,codec_type,codec_name,width,height,pix_fmt,color_range,color_space,color_primaries,color_transfer,r_frame_rate,avg_frame_rate,sample_rate,channels,nb_read_frames,duration,time_base:stream_tags=DURATION:format=format_name,duration",
        "-of", "compact=p=1:nk=0",
    ]).arg(output).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut process = admission.spawn(command, None).map_err(media)?;
    let stdout = process
        .child()
        .stdout
        .take()
        .ok_or_else(|| err("Missing probe output"))?;
    let stderr = process
        .child()
        .stderr
        .take()
        .ok_or_else(|| err("Missing probe diagnostics"))?;
    let (sender, observations) = mpsc::sync_channel(4);
    let (frame_sender, frame_rows) = if frame_observer.is_some() {
        let (sender, rows) = mpsc::sync_channel(4);
        (Some(sender), Some(rows))
    } else {
        (None, None)
    };
    let mut out = Some(thread::spawn(move || {
        read_probe_output_observed(stdout, sender, frame_sender)
    }));
    let mut pending_frame = None;
    let mut source_frames = 0u64;
    let mut parsed = None;
    let errors = thread::spawn(move || bounded_read(stderr, 16384));
    let start = Instant::now();
    let mut last_progress = start;
    let mut last_rss = start;
    let mut decoded = 0;
    let status = (|| {
        loop {
            let mut advanced = false;
            if let Some(rows) = &frame_rows {
                for _ in 0..256 {
                    let row = pending_frame.take().or_else(|| rows.try_recv().ok());
                    let Some((ordinal, pts, duration)) = row else {
                        break;
                    };
                    if ordinal != source_frames {
                        return Err(err("Encoded frame observer order changed"));
                    }
                    frame_observer.as_mut().expect("source observer")(ordinal, pts, duration)?;
                    source_frames += 1;
                    advanced = true;
                }
            }
            for _ in 0..4 {
                let Ok(count) = observations.try_recv() else {
                    break;
                };
                if count <= decoded {
                    return Err(err("Repeated probe progress"));
                }
                decoded = count;
                advanced = true;
            }
            if advanced {
                let elapsed = if limit.renews_on_progress() {
                    last_progress.elapsed()
                } else {
                    start.elapsed()
                };
                if elapsed >= Duration::from_millis(limit.milliseconds()) {
                    return Err(EditorError::new(
                        "VIDEO_TIME_LIMIT",
                        "Probe progress arrived after expiration",
                    ));
                }
                checkpoint(true)?;
                if limit.renews_on_progress() {
                    last_progress = Instant::now();
                }
            }
            checkpoint(false)?;
            let elapsed = if limit.renews_on_progress() {
                last_progress.elapsed()
            } else {
                start.elapsed()
            };
            if elapsed >= Duration::from_millis(limit.milliseconds()) {
                return Err(EditorError::new(
                    "VIDEO_TIME_LIMIT",
                    "Video probe exceeded captured lifetime",
                ));
            }
            if out.as_ref().is_some_and(|reader| reader.is_finished()) {
                parsed = Some(
                    out.take()
                        .unwrap()
                        .join()
                        .map_err(|_| err("Probe output reader failed"))??,
                );
            }
            if last_rss.elapsed() >= Duration::from_millis(100) {
                last_rss = Instant::now();
                if resident_bytes(process.child().id())?.is_some_and(|rss| rss > max_rss_bytes) {
                    return Err(EditorError::new(
                        "VIDEO_BUDGET_EXCEEDED",
                        "Video probe RSS exceeded captured admission",
                    ));
                }
            }
            if let Some(status) = process.child().try_wait().map_err(io)? {
                if frame_rows.is_none() {
                    return Ok(status);
                }
                if parsed.is_some() {
                    // A finished reader can leave its final bounded rows in
                    // the queue after this loop's first drain. Consume them
                    // before accepting process exit and sealing the index.
                    while let Some((ordinal, pts, duration)) =
                        frame_rows.as_ref().and_then(|rows| rows.try_recv().ok())
                    {
                        if ordinal != source_frames {
                            return Err(err("Final source frame order changed"));
                        }
                        frame_observer.as_mut().expect("source observer")(ordinal, pts, duration)?;
                        source_frames += 1;
                    }
                    return Ok(status);
                }
            }
            if let Some(rows) = &frame_rows {
                match rows.recv_timeout(Duration::from_millis(5)) {
                    Ok(row) => pending_frame = Some(row),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => thread::yield_now(),
                }
            } else {
                thread::sleep(Duration::from_millis(20));
            }
        }
    })();
    drop(process);
    drop(observations);
    drop(frame_rows);
    let observed = if let Some(reader) = out {
        reader
            .join()
            .map_err(|_| err("Probe output reader failed"))
            .and_then(|result| result)
    } else {
        parsed.ok_or_else(|| err("Missing probe observation"))
    };
    let _ = errors.join();
    if !status?.success() {
        return Err(err("Bounded video probe failed"));
    }
    observed
}
#[cfg(target_os = "linux")]
pub(crate) fn resident_bytes(pid: u32) -> Result<Option<u64>, EditorError> {
    let content = match std::fs::read_to_string(format!("/proc/{pid}/statm")) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let resident = content
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| err("Encoder RSS observation unavailable"))?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return Err(err("Encoder RSS page size unavailable"));
    }
    Ok(Some(resident.saturating_mul(page as u64)))
}
#[cfg(target_os = "macos")]
pub(crate) fn resident_bytes(pid: u32) -> Result<Option<u64>, EditorError> {
    #[repr(C)]
    struct TaskInfo {
        values: [u64; 6],
        counters: [i32; 12],
    }
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut std::ffi::c_void,
            size: i32,
        ) -> i32;
    }
    let mut info = TaskInfo {
        values: [0; 6],
        counters: [0; 12],
    };
    let size = std::mem::size_of::<TaskInfo>();
    let n = unsafe {
        proc_pidinfo(
            pid as i32,
            4,
            0,
            (&mut info as *mut TaskInfo).cast(),
            size as i32,
        )
    };
    if n == 0 {
        return Ok(None);
    }
    if n != size as i32 {
        return Err(err("Encoder RSS observation unavailable"));
    }
    Ok(Some(info.values[1]))
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn resident_bytes(_: u32) -> Result<Option<u64>, EditorError> {
    Err(EditorError::new(
        "CAPABILITY_UNAVAILABLE",
        "Video encoder RSS monitoring unavailable",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn compact_probe_progress_preserves_decoded_counts_and_bounded_stream_facts() {
        let rows = concat!(
            "frame|media_type=video|stream_index=0|side_data|side_data_type=fixture\n",
            "frame|stream_index=1|media_type=audio\n",
            "frame|stream_index=0|media_type=video\n",
            "stream|index=0|codec_type=video|codec_name=vp9|width=2|height=2|pix_fmt=yuv420p|color_range=tv|color_space=bt709|color_primaries=bt709|color_transfer=bt709|r_frame_rate=30/1|nb_read_frames=2|tag:DURATION=00:00:00.066666667\n",
            "stream|index=1|codec_type=audio|codec_name=opus|sample_rate=48000|channels=2|nb_read_frames=1|tag:DURATION=00:00:00.066666667\n",
            "format|format_name=matroska,webm|duration=0.066667\n",
        );
        let (sender, receiver) = mpsc::sync_channel(4);
        let observed = read_probe_output(rows.as_bytes(), sender).unwrap();
        assert_eq!(receiver.into_iter().collect::<Vec<_>>(), [1, 2, 3]);
        let config = ClassicVideoEncoderConfig {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            output: "output.webm".into(),
            audio: "private.wav".into(),
            width: 2,
            height: 2,
            fps_numerator: 30,
            fps_denominator: 1,
            frame_count: 2,
            max_output_bytes: 1 << 24,
            max_rss_bytes: 1 << 26,
            execution_limit: VideoExecutionLimit::Progress {
                milliseconds: 300_000,
            },
        };
        let options = opencut_contracts::export_video::ExportVideoOptions {
            runtime_fingerprint: None,
            resolution: opencut_contracts::export_video::RasterResolution::Source,
            format: opencut_contracts::export_video::ExportFormat::Webm,
            quality: opencut_contracts::export_video::ExportQuality::High,
            include_audio: true,
        };
        let verified = crate::export_video::verify_probe(&observed, &config, &options).unwrap();
        assert_eq!(verified.frames, 2);
        assert!((verified.video_seconds - 2.0 / 30.0).abs() < 1e-8);
        assert!((verified.audio_seconds.unwrap() - 2.0 / 30.0).abs() < 1e-8);
        for invalid in [
            rows.replace("nb_read_frames=2", "nb_read_frames=3"),
            rows.replace("media_type=audio", "media_type=subtitle"),
            format!("frame|ignored={}\n", "x".repeat(16_385)),
        ] {
            let (sender, _receiver) = mpsc::sync_channel(4);
            assert!(read_probe_output(invalid.as_bytes(), sender).is_err());
        }
    }
    use std::process::Child;
    #[derive(Debug)]
    struct Admission;
    struct Process(Child);
    impl OutputProcess for Process {
        fn child(&mut self) -> &mut Child {
            &mut self.0
        }
        fn output_limit(&self) -> Option<u64> {
            Some(1048576)
        }
    }
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    impl OutputAdmission for Admission {
        fn spawn(
            &self,
            mut command: Command,
            _: Option<&Path>,
        ) -> opencut_media::MediaResult<Box<dyn OutputProcess>> {
            Ok(Box::new(Process(command.spawn()?)))
        }
        fn write(&self, _: &Path, _: &[u8]) -> opencut_media::MediaResult<()> {
            unreachable!()
        }
    }
    #[derive(Debug)]
    struct CaptureInput {
        path: PathBuf,
        arguments: std::sync::Mutex<Vec<std::ffi::OsString>>,
    }
    impl OutputAdmission for CaptureInput {
        fn spawn(
            &self,
            command: Command,
            _: Option<&Path>,
        ) -> opencut_media::MediaResult<Box<dyn OutputProcess>> {
            *self.arguments.lock().unwrap() = command.get_args().map(|s| s.to_owned()).collect();
            let child = Command::new("/bin/sh")
                .args(["-c", "exec /bin/cat > \"$1\"", "capture-encoder-input"])
                .arg(&self.path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?;
            Ok(Box::new(Process(child)))
        }
        fn write(&self, _: &Path, _: &[u8]) -> opencut_media::MediaResult<()> {
            unreachable!()
        }
    }
    #[test]
    fn opaque_video_input_packs_rgb_while_source_proxy_keeps_alpha_and_hidden_rgb() {
        let directory = tempfile::tempdir().unwrap();
        let rgba = [9, 17, 31, 0, 255, 0, 0, 1, 0, 255, 0, 128, 0, 0, 255, 255];
        let rgb = [9, 17, 31, 255, 0, 0, 0, 255, 0, 0, 0, 255];
        for (width, height, rgba, rgb) in [
            (2, 2, rgba.to_vec(), rgb.to_vec()),
            (
                128,
                128,
                [13, 21, 34, 0].repeat(16384),
                [13, 21, 34].repeat(16384),
            ),
        ] {
            for source in [false, true] {
                let admission = CaptureInput {
                    path: directory.path().join(format!("{width}-{source}")),
                    arguments: Default::default(),
                };
                let config = ClassicVideoEncoderConfig {
                    ffmpeg: "/bin/cat".into(),
                    ffprobe: "/bin/cat".into(),
                    output: directory.path().join("unused-output"),
                    audio: directory.path().join("unused-audio"),
                    width,
                    height,
                    fps_numerator: 30,
                    fps_denominator: 1,
                    frame_count: 2,
                    max_output_bytes: 1048576,
                    max_rss_bytes: 67108864,
                    execution_limit: VideoExecutionLimit::Total { milliseconds: 5000 },
                };
                let checkpoint: fn(bool) -> Result<(), EditorError> = |_| Ok(());
                let mut encoder = if source {
                    ClassicVideoEncoder::start_source_preview(config, &admission, checkpoint)
                        .unwrap()
                } else {
                    ClassicVideoEncoder::start(config, &admission, checkpoint).unwrap()
                };
                let arguments = admission.arguments.lock().unwrap();
                let at = arguments.iter().position(|a| a == "-pixel_format").unwrap();
                assert_eq!(arguments[at + 1], if source { "rgba" } else { "rgb24" });
                drop(arguments);
                for _ in 0..2 {
                    if rgba.len() == 16 {
                        encoder.chunk(0, &rgba[..4]).unwrap();
                        encoder.chunk(4, &rgba[4..]).unwrap();
                    } else {
                        encoder.chunk(0, &rgba).unwrap();
                    }
                    encoder.finish_frame().unwrap();
                }
                drop(encoder.input.take());
                assert!(
                    encoder
                        .process
                        .as_mut()
                        .unwrap()
                        .child()
                        .wait()
                        .unwrap()
                        .success()
                );
                drop(encoder);
                let expected = if source { rgba.clone() } else { rgb.clone() };
                assert_eq!(std::fs::read(&admission.path).unwrap(), expected.repeat(2));
            }
        }
    }
    #[test]
    fn tool_resolution_requires_executable_regular_files_for_absolute_relative_and_path() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let executable = directory.path().join("encoder");
        std::fs::write(&executable, "fixture bytes need not execute").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
        let current = std::env::current_dir().unwrap();
        let relative = executable.strip_prefix(&current).unwrap().to_path_buf();
        let paths = std::env::join_paths([directory.path()]).unwrap();
        for rejected in [
            resolve_tool(executable.clone()),
            resolve_tool(relative.clone()),
            resolve_in_paths(Path::new("encoder"), &paths),
            resolve_tool(directory.path().into()),
        ] {
            assert_eq!(rejected.unwrap_err().code, "CAPABILITY_UNAVAILABLE");
        }
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let expected = std::fs::canonicalize(&executable).unwrap();
        assert_eq!(resolve_tool(executable).unwrap(), expected);
        assert_eq!(resolve_tool(relative).unwrap(), expected);
        assert_eq!(
            resolve_in_paths(Path::new("encoder"), &paths).unwrap(),
            expected
        );
        let shadow = directory.path().join("shadow");
        std::fs::create_dir(&shadow).unwrap();
        std::fs::write(shadow.join("encoder"), "non executable shadow").unwrap();
        std::fs::set_permissions(
            shadow.join("encoder"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let paths = std::env::join_paths([shadow.as_path(), directory.path()]).unwrap();
        assert_eq!(
            resolve_in_paths(Path::new("encoder"), &paths).unwrap(),
            expected
        );
    }
    #[test]
    fn encoder_backpressure_remains_cancellable_and_reaps_child() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("blocked-encoder");
        std::fs::write(&executable, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let start = Instant::now();
        let cancel = opencut_media::CancellationToken::new();
        let mut encoder = ClassicVideoEncoder::start(
            ClassicVideoEncoderConfig {
                ffmpeg: executable,
                ffprobe: "/bin/sh".into(),
                output: directory.path().join("video.mp4"),
                audio: directory.path().join("audio.wav"),
                width: 256,
                height: 256,
                fps_numerator: 24,
                fps_denominator: 1,
                frame_count: 1,
                max_output_bytes: 1048576,
                max_rss_bytes: 67108864,
                execution_limit: VideoExecutionLimit::Total { milliseconds: 5000 },
            },
            &Admission,
            |_| {
                if cancel.is_cancelled() {
                    Err(EditorError::new(
                        "JOB_CANCELLED",
                        "Test cancelled during full pipe",
                    ))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        let pid = encoder.process.as_mut().unwrap().child().id();
        let trigger = cancel.clone();
        let cancellation = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            trigger.cancel();
        });
        let mut failure = None;
        for index in 0..4 {
            if let Err(cause) = encoder.chunk(index * 65536, &vec![0u8; 65536]) {
                failure = Some(cause);
                break;
            }
        }
        cancellation.join().unwrap();
        assert_eq!(
            failure.expect("Unconsumed pipe must backpressure").code,
            "JOB_CANCELLED"
        );
        drop(encoder);
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    }
}
