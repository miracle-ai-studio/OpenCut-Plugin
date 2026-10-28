//! Editable subtitles use ordinary text elements and the shared atomic edit.
//! Metrics are observations; the application owns wrapping, timing and placement.
use crate::{
    TimeRange,
    caption_frame::{CaptionLayoutReceipt, CaptionRasterPlan},
    project_public::ApplyProject,
    scene::Text,
    subtitles::SubtitleFormat,
    text_authoring::TextTrackTarget,
    text_frame::{TextLayerPlan, TextRasterReceipt},
    timeline::TimelineTarget,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubtitleTextSource {
    Transcript {
        analysis_job_id: String,
        words_per_caption: u8,
    },
    File {
        format: SubtitleFormat,
        content: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleTextObservation {
    pub caption: CaptionLayoutReceipt,
    pub raster: TextRasterReceipt,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleTextPlanRequest {
    pub project_id: String,
    pub expected_revision: String,
    pub sequence_id: String,
    pub idempotency_key: String,
    pub track: TextTrackTarget,
    pub source: SubtitleTextSource,
    /// Content must be empty. Cue content comes exclusively from the source.
    pub style: Text,
    /// None selects the Classic 80% canvas width. Placement uses its 5% bottom margin.
    pub max_width_px: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleTextCuePlan {
    pub timeline_range: TimeRange,
    pub layer: TextLayerPlan,
    pub directive: CaptionRasterPlan,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleTextPlanResult {
    pub measurement: Option<crate::renderer_identity::NativeTextRuntimeIdentity>,
    pub revision: String,
    pub width: u32,
    pub height: u32,
    pub cues: Vec<SubtitleTextCuePlan>,
    pub transaction: Option<ApplyProject>,
    pub selection: Option<TimelineTarget>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleTextExport {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    /// Explicit scope prevents ordinary titles on other tracks entering subtitles.
    pub track_id: String,
    pub format: SubtitleFormat,
}
