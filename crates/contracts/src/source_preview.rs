//! Immutable source preparation facts. Project time and encoded proxy time
//! remain separate; these values carry neither native paths nor authority.
use crate::{
    AssetVersion,
    renderer_identity::{NativeLibraryIdentity, NativeSystemIdentity},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewRaster {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewTimeBase {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourcePreviewMatrix {
    Bt601,
    Bt709,
    Bt2020,
    Rgb,
    Unspecified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewFrameVideo {
    pub source_width: u32,
    pub source_height: u32,
    pub raster_width: u32,
    pub raster_height: u32,
    pub rotation_degrees: u16,
    pub pixel_aspect_numerator: i32,
    pub pixel_aspect_denominator: i32,
    pub matrix: SourcePreviewMatrix,
    pub full_range: bool,
    pub has_alpha: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewIdentity {
    pub project_id: String,
    pub asset_id: String,
    pub source_version: AssetVersion,
    pub selected_video_stream: Option<u32>,
    pub raster_bound: SourcePreviewRaster,
    pub runtime_fingerprint: String,
    pub policy_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewDecoderIdentity {
    pub worker_sha256: String,
    pub decoder_libraries: Vec<NativeLibraryIdentity>,
    pub platform: NativeSystemIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewRuntimeIdentity {
    pub host_sha256: String,
    pub decoder: SourcePreviewDecoderIdentity,
    pub ffmpeg_sha256: String,
    pub ffprobe_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourcePreviewEncoding {
    Ffv1Rgba,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewArtifact {
    pub content_hash: String,
    pub size_bytes: String,
    pub mime_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewResult {
    pub identity: SourcePreviewIdentity,
    /// The immutable producer is not relabelled when another revision reuses
    /// the same original bytes, selected track, raster and runtime.
    pub producer_revision: String,
    pub actual_stream_index: u32,
    pub actual_track_id: i32,
    pub source_time_base: SourcePreviewTimeBase,
    pub proxy_time_base: SourcePreviewTimeBase,
    pub encoding: SourcePreviewEncoding,
    pub frame_count: String,
    pub first_source_pts: String,
    pub last_source_pts: String,
    pub last_source_duration: String,
    pub video: SourcePreviewArtifact,
    pub frame_index: SourcePreviewArtifact,
    pub runtime: SourcePreviewRuntimeIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewFrameSelection {
    pub ordinal: String,
    pub source_pts: String,
    pub source_duration: String,
    pub proxy_pts: String,
    pub proxy_duration: String,
    pub source_video: SourcePreviewFrameVideo,
    pub last_frame: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourcePreviewState {
    NotRequested,
    Queued,
    Running,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourcePreviewUse {
    Original {
        element_id: String,
        asset_id: String,
        state: SourcePreviewState,
        job_id: Option<String>,
    },
    Prepared {
        element_id: String,
        job_id: String,
        source: Box<SourcePreviewResult>,
        /// None is an observed source gap before the first frame, rather than
        /// a fabricated timestamp or an alternate rendering implementation.
        frame: Option<SourcePreviewFrameSelection>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewPrepare {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    /// Omitted to prepare the sequence's participating video assets.
    pub asset_id: Option<String>,
    pub selected_video_stream: Option<u32>,
}
