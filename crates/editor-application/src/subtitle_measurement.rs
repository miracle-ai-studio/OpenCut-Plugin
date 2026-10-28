//! Application measurement facts remain separate from public planning intent.
use opencut_contracts::{
    scene::Text,
    subtitle_ass::{SubtitleAssFontBinding, SubtitleAssPlanRequest},
    subtitle_classic_ass::SubtitleClassicAssPlanRequest,
    subtitle_text::{SubtitleTextObservation, SubtitleTextPlanRequest, SubtitleTextSource},
    text_authoring::TextTrackTarget,
    text_frame::TextRasterReceipt,
};

#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredSubtitleTextRequest {
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
    /// None prepares measurement inputs. Some validates them and returns one edit.
    pub observations: Option<Vec<SubtitleTextObservation>>,
}
impl From<SubtitleTextPlanRequest> for MeasuredSubtitleTextRequest {
    fn from(input: SubtitleTextPlanRequest) -> Self {
        Self {
            project_id: input.project_id,
            expected_revision: input.expected_revision,
            sequence_id: input.sequence_id,
            idempotency_key: input.idempotency_key,
            track: input.track,
            source: input.source,
            style: input.style,
            max_width_px: input.max_width_px,
            observations: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredSubtitleAssRequest {
    pub project_id: String,
    pub expected_revision: String,
    pub sequence_id: String,
    pub idempotency_key: String,
    pub track: TextTrackTarget,
    pub content: String,
    /// Explicit aliases; otherwise exact pinned family/weight/style is required.
    pub font_bindings: Vec<SubtitleAssFontBinding>,
    pub observations: Option<Vec<TextRasterReceipt>>,
}
impl From<SubtitleAssPlanRequest> for MeasuredSubtitleAssRequest {
    fn from(input: SubtitleAssPlanRequest) -> Self {
        Self {
            project_id: input.project_id,
            expected_revision: input.expected_revision,
            sequence_id: input.sequence_id,
            idempotency_key: input.idempotency_key,
            track: input.track,
            content: input.content,
            font_bindings: input.font_bindings,
            observations: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredSubtitleClassicAssRequest {
    pub project_id: String,
    pub expected_revision: String,
    pub sequence_id: String,
    pub idempotency_key: String,
    pub track: TextTrackTarget,
    pub content: String,
    pub font_bindings: Vec<SubtitleAssFontBinding>,
    pub observations: Option<Vec<SubtitleTextObservation>>,
}
impl From<SubtitleClassicAssPlanRequest> for MeasuredSubtitleClassicAssRequest {
    fn from(input: SubtitleClassicAssPlanRequest) -> Self {
        Self {
            project_id: input.project_id,
            expected_revision: input.expected_revision,
            sequence_id: input.sequence_id,
            idempotency_key: input.idempotency_key,
            track: input.track,
            content: input.content,
            font_bindings: input.font_bindings,
            observations: None,
        }
    }
}
