//! Fixed-revision configurable video export; previous MP4 result layouts frozen.
use super::{
    media_frame::{MediaFrameArtifactMetadata, MediaFrameExecutionSummary},
    scene_audio::AudioExecutionSummary,
};
pub use opencut_contracts::export_video::*;
use opencut_contracts::{AssetLockManifest, FrameRate, Time, text_frame::FontLock};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub const EXPORT_VIDEO_KIND: &str = "render.export_video.v2";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitExportVideo {
    pub idempotency_key: String,
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub options: ExportVideoOptions,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVideoCapability {
    pub schema_version: u32,
    pub available: bool,
    /// Audio-off export remains available without an audio renderer runtime.
    pub audio_available: bool,
    pub submit_tool: String,
    pub formats: Vec<ExportFormat>,
    pub qualities: Vec<ExportQuality>,
    pub unavailable_reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVideoIdentity {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub visual_profile: ExportVisualProfile,
    pub options: ExportVideoOptions,
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
    pub frame_count: String,
    pub timeline_duration: Time,
    pub video_duration: Time,
    /// None means there is no output audio stream, not a silent encoded stream.
    pub audio_frame_count: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VideoEncoderBackend {
    FfmpegLibx264,
    FfmpegH264Videotoolbox,
    FfmpegLibvpxVp9,
    WebcodecsAvc,
}
/// Admission-time hardware capability observation, before any project frames.
/// A failed real encode is never retried through another backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVideoHardwareProbe {
    pub available: bool,
    pub exit_code: i32,
    /// Hash of the retained first 16 KiB of capability stderr, never local paths.
    pub diagnostics_sha256: String,
    pub system: opencut_contracts::renderer_identity::NativeSystemIdentity,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVideoEncoderSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<VideoEncoderBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_probe: Option<ExportVideoHardwareProbe>,
    pub ffmpeg_sha256: String,
    pub ffprobe_sha256: String,
    pub video_codec: String,
    pub audio_codec: Option<String>,
    pub pixel_format: String,
    pub rates: ExportVideoRates,
    pub observed_video_frames: String,
    pub observed_video_duration_seconds: f64,
    pub observed_audio_duration_seconds: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVideoExecution {
    pub visual: MediaFrameExecutionSummary,
    pub audio: Option<AudioExecutionSummary>,
    pub encoder: ExportVideoEncoderSummary,
    pub frame_receipts_sha256: String,
    pub frames_rendered: String,
    pub rgba_bytes: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVideoResult {
    pub schema_version: u32,
    pub identity: ExportVideoIdentity,
    pub asset_lock: AssetLockManifest,
    pub font_lock: Vec<FontLock>,
    pub render_input_sha256: String,
    pub execution: ExportVideoExecution,
    pub artifact: MediaFrameArtifactMetadata,
}

/// Exact half-open interval on the original scene's output frame grid.
/// The first encoded frame is start_frame; timestamps in the output start at zero.
pub use crate::render_requests::VideoFrameRange;
pub const EXPORT_VIDEO_RANGE_KIND: &str = "render.export_video_range.v1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitExportVideoRange {
    pub idempotency_key: String,
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub options: ExportVideoOptions,
    pub range: VideoFrameRange,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportRangeMapping {
    pub frames: VideoFrameRange,
    pub source_start: Time,
    pub source_end: Time,
    /// 48 kHz sample boundaries use floor(frame * fps_den * 48000 / fps_num).
    /// End is clipped to the full scene PCM length; mastering precedes cropping.
    pub audio: Option<super::scene_audio::AudioSampleRange>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVideoRangeResult {
    pub schema_version: u32,
    pub mapping: ExportRangeMapping,
    /// Output frame/sample counts and duration; timeline_duration is the original scene extent.
    pub output: ExportVideoResult,
}
