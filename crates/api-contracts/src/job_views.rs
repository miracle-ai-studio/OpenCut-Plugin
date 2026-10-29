//! Sole public task views;execution records and historical payload tags stay private.
use crate::JobState;
use opencut_contracts::{AssetLockManifest, Time, TimeRange};
use opencut_job_contracts::{
    AssetLockingMode, CompositedProjectFrameQuality, ProjectFrameQuality, ProjectRangeQuality,
};
use opencut_media_contracts::{
    DecodedSample, FontResolutionPolicy, FrameAlignment, FrameQuality, FrameRate, MediaStream,
    MediaTime, PixelBounds, TextClusterRange,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<JobPurpose>,
    pub job_id: String,
    pub kind: String,
    pub target: Option<ProjectTaskTarget>,
    pub state: JobState,
    pub cancel_requested: bool,
    pub progress: Option<ProgressView>,
    pub result: Option<JobResultView>,
    #[serde(default)]
    pub resource_target: Option<JobResourceTarget>,
    pub result_unavailable: Option<PublicJobFailure>,
    pub failure: Option<PublicJobFailure>,
    pub artifacts: Vec<ArtifactReference>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobPurpose {
    PlaybackOptimization,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobResourceTarget {
    pub project_id: String,
    pub resource_kind: String,
    pub resource_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectTaskTarget {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProgressView {
    pub stage: PublicJobStage,
    pub fraction: Option<f64>,
    pub media_time: Option<Time>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicJobStage {
    ResolvingAssets,
    Probing,
    SelectingFrame,
    Decoding,
    Encoding,
    Verifying,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PublicJobFailure {
    pub code: String,
    pub message: String,
    pub diagnostic_job_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactReference {
    pub artifact_id: String,
    pub job_id: String,
    pub role: ArtifactRole,
    pub content_hash: String,
    pub size_bytes: String,
    pub mime_type: String,
    pub availability: ArtifactAvailability,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    Primary,
    FrameIndex,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAvailability {
    Unchecked,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MediaInfo {
    pub format_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_brand: Option<String>,
    pub duration: Option<MediaTime>,
    pub bit_rate: Option<u64>,
    pub streams: Vec<MediaStream>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VerificationView {
    pub passed: bool,
    pub checks: Vec<VerificationCheckView>,
    pub metadata: MediaInfo,
    pub decoded_samples: Vec<DecodedSample>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VerificationCheckView {
    pub name: String,
    pub passed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TextLayoutView {
    pub requested_family: String,
    pub policy: FontResolutionPolicy,
    pub used_faces: Vec<FontFaceView>,
    pub line_count: u32,
    pub glyph_count: u32,
    pub rtl_runs: u32,
    pub cluster_ranges: Vec<TextClusterRange>,
    pub pixel_bounds: PixelBounds,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FontFaceView {
    pub family: String,
    pub postscript_name: String,
    pub face_index: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExportView {
    pub metadata: MediaInfo,
    pub verification: VerificationView,
    pub text_layouts: Vec<TextLayoutView>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceFrameView {
    pub requested_time: MediaTime,
    pub actual_time: MediaTime,
    pub source_pts: String,
    pub source_time_base: FrameRate,
    pub alignment: FrameAlignment,
    pub exact: bool,
    pub quality: FrameQuality,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectRenderView {
    pub revision: String,
    pub asset_lock: Option<AssetLockManifest>,
    pub asset_locking: AssetLockingMode,
    pub media: ExportView,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectFrameView {
    pub revision: String,
    pub sequence_id: String,
    pub requested_time: Time,
    pub actual_sequence_time: Time,
    pub alignment: FrameAlignment,
    pub quality: ProjectFrameQuality,
    pub asset_lock: Option<AssetLockManifest>,
    pub asset_locking: AssetLockingMode,
    pub width: u32,
    pub height: u32,
    pub text_layouts: Vec<TextLayoutView>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CompositedFrameView {
    pub revision: String,
    pub sequence_id: String,
    pub requested_time: Time,
    pub actual_sequence_time: Time,
    pub alignment: FrameAlignment,
    pub quality: CompositedProjectFrameQuality,
    pub asset_lock: Option<AssetLockManifest>,
    pub asset_locking: AssetLockingMode,
    pub width: u32,
    pub height: u32,
    pub text_layouts: Vec<TextLayoutView>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectRangeView {
    pub revision: String,
    pub sequence_id: String,
    pub requested_range: TimeRange,
    pub actual_sequence_range: TimeRange,
    pub alignment: FrameAlignment,
    pub quality: ProjectRangeQuality,
    pub asset_lock: Option<AssetLockManifest>,
    pub asset_locking: AssetLockingMode,
    pub media: ExportView,
}
pub use crate::job_results::JobResultView;
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobQueryInput {
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobPage {
    pub jobs: Vec<JobView>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobChangesInput {
    pub cursor: Option<String>,
    pub job_id: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}
fn default_limit() -> usize {
    50
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobChangesPage {
    pub events: Vec<JobChange>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobChange {
    pub event_id: String,
    pub job_id: String,
    pub event_type: JobChangeKind,
    pub state: JobState,
    pub progress: Option<ProgressView>,
    pub created_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobChangeKind {
    Queued,
    Started,
    Progress,
    Succeeded,
    Failed,
    CancelRequested,
    Cancelled,
    Recovered,
    Unknown,
}
