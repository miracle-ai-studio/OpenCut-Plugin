//! Classic ASS compatibility import. The strict external ASS profile is frozen;
//! this operation preserves Classic's mapped styles, fallback and loss warnings.
use crate::{
    project_public::ApplyProject,
    subtitle_ass::{SubtitleAssFontBinding, SubtitleAssFontRequirement},
    subtitle_text::SubtitleTextObservation,
    text_authoring::TextTrackTarget,
    timeline::TimelineTarget,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleClassicAssPlanRequest {
    pub project_id: String,
    pub expected_revision: String,
    pub sequence_id: String,
    pub idempotency_key: String,
    pub track: TextTrackTarget,
    pub content: String,
    pub font_bindings: Vec<SubtitleAssFontBinding>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleClassicVerticalAlign {
    Top,
    Middle,
    Bottom,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleClassicPlacement {
    pub left_margin_px: f64,
    pub right_margin_px: f64,
    pub vertical_margin_px: f64,
    pub vertical_align: SubtitleClassicVerticalAlign,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleClassicAssCuePlan {
    pub timeline_range: crate::TimeRange,
    pub layer: crate::text_frame::TextLayerPlan,
    pub directive: crate::caption_frame::CaptionRasterPlan,
    pub placement: SubtitleClassicPlacement,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleClassicAssPlanResult {
    pub measurement: Option<crate::renderer_identity::NativeTextRuntimeIdentity>,
    pub revision: String,
    pub width: u32,
    pub height: u32,
    pub font_requirements: Vec<SubtitleAssFontRequirement>,
    pub cues: Vec<SubtitleClassicAssCuePlan>,
    pub transaction: Option<ApplyProject>,
    pub selection: Option<TimelineTarget>,
    pub warnings: Vec<String>,
    pub skipped_cue_count: u32,
}
