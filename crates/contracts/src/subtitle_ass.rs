//! Style-preserving ASS authoring into ordinary text elements.
use crate::{
    TimeRange,
    project_public::ApplyProject,
    scene::{FontStyle, FontWeight},
    text_authoring::TextTrackTarget,
    text_frame::{TextLayerPlan, TextRasterReceipt},
    timeline::TimelineTarget,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleAssFontBinding {
    pub family: String,
    pub weight: FontWeight,
    pub style: FontStyle,
    pub font_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleAssFontRequirement {
    pub family: String,
    pub weight: FontWeight,
    pub style: FontStyle,
    pub resolved_font_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleAssPlanRequest {
    pub project_id: String,
    pub expected_revision: String,
    pub sequence_id: String,
    pub idempotency_key: String,
    pub track: TextTrackTarget,
    pub content: String,
    /// Explicit aliases; otherwise exact pinned family/weight/style is required.
    pub font_bindings: Vec<SubtitleAssFontBinding>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleAssCuePlan {
    pub timeline_range: TimeRange,
    pub layer: TextLayerPlan,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtitleAssPlanResult {
    pub measurement: Option<crate::renderer_identity::NativeTextRuntimeIdentity>,
    pub revision: String,
    pub width: u32,
    pub height: u32,
    /// Unresolved fonts yield no cues or transaction; callers may provide aliases.
    pub font_requirements: Vec<SubtitleAssFontRequirement>,
    pub cues: Vec<SubtitleAssCuePlan>,
    pub transaction: Option<ApplyProject>,
    pub selection: Option<TimelineTarget>,
    pub warnings: Vec<String>,
}
