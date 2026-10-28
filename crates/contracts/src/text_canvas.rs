//! Fixed-revision Canvas facts, with actual source and requested typography separate.
use crate::{
    Time,
    font_synthesis::FontSynthesisBinding,
    text_frame::{TextFrameIdentity, TextLayerPlan},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub const CLASSIC_TEXT_CANVAS_PROFILE: &str = "classic-canvas-text";
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextCanvasPlanRequest {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub element_id: String,
    pub time: Time,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextCanvasPlanResult {
    pub layout: crate::text_frame::TextLayout,
    pub measurement: crate::renderer_identity::NativeTextRuntimeIdentity,
    pub identity: TextFrameIdentity,
    /// Font metadata and coverage always describe the actual source bytes.
    pub layer: TextLayerPlan,
    /// Canvas measure/draw uses this requested style; FontFace still uses the
    /// actual descriptor above. None retains ordinary exact-face behavior.
    pub font_synthesis: Option<FontSynthesisBinding>,
}
