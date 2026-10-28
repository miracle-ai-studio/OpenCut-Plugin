//! One complete scene preview bound to the existing sound consumer clock.
use crate::{audio_preview::AudioPreviewStream, scene_frame_plan::SceneFramePlan};
use opencut_contracts::{
    Time,
    blend_frame::BoundBlendFrame,
    caption_frame::CaptionLayoutReceipt,
    graphic_frame::{BoundGraphicFrame, GraphicRasterReceipt},
    scene_frame::BoundSceneFrame,
    text_frame::{BoundTextFrame, TextRasterReceipt},
    visual_frame::*,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualResource {
    pub asset_id: String,
    pub content_hash: String,
    pub size: String,
    pub kind: VisualResourceKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicVisualLayerPlan {
    pub element_id: String,
    pub resource: VisualResource,
    pub decode: VisualDecodeRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicVisualPlan {
    pub identity: VisualFrameIdentity,
    pub resources: Vec<VisualResource>,
    pub layers: Vec<ClassicVisualLayerPlan>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualDecodeAdmission {
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: String,
    pub max_decoded_bytes: String,
    /// Native-admitted working surfaces: direct video (1), canvas transfer (2),
    /// or the frozen four-surface decoder (4).
    /// Missing values retain the frozen four-surface decoder contract.
    #[serde(
        default = "default_decoder_surfaces",
        skip_serializing_if = "is_default_decoder_surfaces"
    )]
    pub decoder_surfaces: u8,
}

fn default_decoder_surfaces() -> u8 {
    4
}
fn is_default_decoder_surfaces(value: &u8) -> bool {
    *value == 4
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualPreviewAdmission {
    pub max_frame_pixels: String,
    pub max_decoded_pixels: String,
    pub max_gpu_bytes: String,
    pub max_rgba_bytes: String,
    pub max_encoded_bytes: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VisualResourceKind {
    Video,
    Image,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum VisualDecodeRequest {
    Image,
    Video {
        #[serde(rename = "requestedTime")]
        requested_time: Time,
        /// Decoder API projection supplied by Rust; the browser never divides
        /// cross-process integer ticks or applies retime itself.
        #[serde(rename = "requestedTimeSeconds")]
        requested_time_seconds: f64,
        #[serde(rename = "pointProfile")]
        point_profile: String,
        #[serde(rename = "selectedVideoStream")]
        selected_video_stream: Option<u32>,
        #[serde(rename = "eofPolicy")]
        eof_policy: VisualVideoEofPolicy,
        #[serde(rename = "noFrameReason")]
        no_frame_reason: Option<VisualNoFrameReason>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewFrame {
    #[serde(
        default,
        skip_serializing_if = "opencut_contracts::export_video::RasterResolution::is_source"
    )]
    pub resolution: opencut_contracts::export_video::RasterResolution,
    pub session: AudioPreviewStream,
    /// Prepare a bounded future picture without claiming it was presented.
    /// The client must still present against actual audio consumption.
    #[serde(default)]
    pub prepare_ahead: bool,
    /// Resident presentation and queued picture bytes, deducted before decode admission.
    #[serde(default)]
    pub retained_picture_bytes: u32,
    pub time: Option<Time>,
    pub decoded: Option<Vec<DecodedVisualSource>>,
    pub rasters: Option<Vec<TextRasterReceipt>>,
    pub captions: Option<Vec<CaptionLayoutReceipt>>,
    pub graphics: Option<Vec<GraphicRasterReceipt>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewFrameResult {
    pub prepare_ahead: bool,
    /// Next sampling time, computed by the application for sequential preparation.
    pub next_time: Time,
    pub plan: SceneFramePlan,
    pub admission: VisualPreviewAdmission,
    pub decode_admissions: BTreeMap<String, VisualDecodeAdmission>,
    pub bound_media: Option<BoundVisualFrame>,
    pub bound_execution: Option<BoundTextFrame>,
    pub bound_graphic: Option<BoundGraphicFrame>,
    pub bound_blend: Option<BoundBlendFrame>,
    pub bound: Option<BoundSceneFrame>,
    pub time_seconds: f64,
    pub consumer_time_seconds: f64,
    pub max_lag_ms: u32,
}

/// Prepare immutable derived pictures through the shared durable render port.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewCachePrepare {
    pub session: AudioPreviewStream,
    /// Inspect already admitted derived pictures without creating render work.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reuse_only: bool,
}
fn is_false(value: &bool) -> bool {
    !*value
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewCache {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub cache_key: String,
    pub width: u32,
    pub height: u32,
    pub duration: Time,
    pub duration_seconds: f64,
    pub frame_step: Time,
    pub frame_step_seconds: f64,
    pub max_encoded_bytes: u32,
    pub segments: Vec<VisualPreviewCacheSegment>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewCacheSegment {
    pub start: Time,
    pub end: Time,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub job_id: String,
    pub frames: Vec<VisualPreviewCachedFrame>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewCachedFrame {
    pub time: Time,
    pub next_time: Time,
    pub time_seconds: f64,
    /// Timestamp in the encoded segment, projected from its exact CFR index.
    pub segment_time_seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewFont {
    pub session: AudioPreviewStream,
    pub font_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewFontAuthorized {
    pub font_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewFontBatchAuthorize {
    pub session: AudioPreviewStream,
    pub font_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewFontBatchAuthorized {
    pub font_ids: Vec<String>,
}

/// A session-owned immutable media capability. It never exposes a native path.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewResourceOpen {
    pub session: AudioPreviewStream,
    pub resource: VisualResource,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualPreviewResourceOpened {
    pub transfer_id: String,
    pub content_hash: String,
    pub byte_length: String,
    pub max_chunk_bytes: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewResourceRead {
    pub session: AudioPreviewStream,
    pub transfer_id: String,
    pub offset: String,
    pub count: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualPreviewResourceChunk {
    pub offset: String,
    pub bytes_hex: String,
    pub eof: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewResourceRelease {
    pub session: AudioPreviewStream,
    pub transfer_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisualPreviewResourceReleased {
    pub released: bool,
}
