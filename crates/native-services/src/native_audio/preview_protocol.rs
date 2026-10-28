//! Captured native realtime process policy and receipts. These private types
//! carry execution facts, never authored project state or native resource paths.
use super::{preview::InputBinding, protocol::error};
use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::ClassicAudioIdentity;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    pub preparation_deadline_ms: u64,
    pub operation_deadline_ms: u64,
    pub lifetime_deadline_ms: u64,
    pub max_pcm_bytes: u64,
    pub max_encoded_bytes: u64,
    pub max_rss_bytes: u64,
}
impl Policy {
    pub fn validate(&self) -> Result<(), EditorError> {
        if !(1..=120000).contains(&self.preparation_deadline_ms)
            || !(1..=120000).contains(&self.operation_deadline_ms)
            || self.lifetime_deadline_ms != 3600000
            || self.max_pcm_bytes == 0
            || self.max_pcm_bytes > 256 * 1024 * 1024
            || self.max_encoded_bytes == 0
            || self.max_encoded_bytes > 32 * 1024 * 1024
            || self.max_rss_bytes == 0
        {
            return Err(error("Invalid captured native audio preview policy"));
        }
        Ok(())
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BlockReceipt {
    pub identity: ClassicAudioIdentity,
    pub engine_identity: String,
    pub generation: u64,
    pub overlay_epoch: u64,
    pub start_sample: String,
    pub frame_count: u32,
    pub lookahead_frames: u32,
    pub inputs: Vec<InputBinding>,
    pub peak_accounted_bytes: u64,
}
