//! Typed private execution receipts; these never extend a frozen public job DTO.
use super::*;
use opencut_composition::scene::ClassicAudioPreparationFacts;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioIdentity {
    pub project_id: String,
    pub sequence_id: String,
    pub revision: String,
    pub profile: String,
    pub sample_rate: u32,
    pub channels: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioDecoderReceipt {
    pub asset_id: String,
    pub profile: String,
    pub sample_rate: u32,
    pub frame_count: String,
    pub channels: u32,
    #[serde(default)]
    pub primary: Option<ClassicAudioPrimarySelection>,
}
/// Private persisted provenance for negotiated standalone PCM decoding. Public
/// algorithm families stay frozen; each receipt is bound to its fixed CAS source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassicAudioDecoderEvidence {
    pub sources: Vec<ClassicAudioDecoderSource>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassicAudioDecoderSource {
    pub resource: AudioResource,
    pub decoder: ClassicAudioDecoderReceipt,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioPreparationReceipt {
    pub element_id: String,
    pub facts: ClassicAudioPreparationFacts,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioMasteringReceipt {
    pub profile: String,
    pub whole_scene_frames: String,
    pub input_peak: f64,
    pub applied: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioRange {
    pub start_sample: String,
    pub frame_count: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioRenderReceipt {
    pub identity: ClassicAudioIdentity,
    pub engine_identity: String,
    pub decoders: Vec<ClassicAudioDecoderReceipt>,
    pub preparation: Vec<ClassicAudioPreparationReceipt>,
    pub mastering: ClassicAudioMasteringReceipt,
    pub range: Option<ClassicAudioRange>,
    pub peak_accounted_bytes: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioEngineIdentity {
    pub engine: String,
    pub browser_version: String,
    pub browser_sha256: String,
    pub module_sha256: String,
    pub playwright_version: String,
    pub mediabunny_version: String,
}
#[derive(Debug)]
pub struct ClassicAudioRuntimeReceipt {
    pub browser_pid: u32,
    pub identity: ClassicAudioEngineIdentity,
}
#[derive(Debug)]
pub struct ClassicAudioExecutionReceipt {
    pub receipt: ClassicAudioRenderReceipt,
    pub quota: AudioQuotaReceipt,
    pub runtime_identity: ClassicAudioRuntimeReceipt,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioPreviewDecoderReceipt {
    pub asset_id: String,
    pub primary: ClassicAudioPrimarySelection,
    pub sample_rate: u32,
    pub frame_count: Option<String>,
    pub channels: u32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioPreviewReceipt {
    pub identity: ClassicAudioIdentity,
    pub engine_identity: String,
    pub decoders: Vec<ClassicAudioPreviewDecoderReceipt>,
    pub source_formats: Vec<opencut_composition::scene::PreviewSourceFormat>,
    pub preparation: Vec<ClassicAudioPreparationReceipt>,
    pub peak_accounted_bytes: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioResourceHandle {
    pub asset_id: String,
    pub handle: String,
}

pub trait ClassicAudioExecutor {
    fn execute(
        &self,
        session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
        handles: &[ClassicAudioResourceHandle],
        sink: &mut dyn ClassicAudioPcmSink,
    ) -> Result<ClassicAudioExecutionReceipt, EditorError>;
}
