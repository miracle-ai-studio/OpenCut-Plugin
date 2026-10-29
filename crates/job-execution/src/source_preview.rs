//! Private captured source preparation. No native path or executable selector
//! is accepted from a client or persisted as an application capability.
use crate::scene_audio::TrustedJobAuthority;
use opencut_contracts::{scene::EditorSnapshot, source_preview::SourcePreviewIdentity};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewPolicy {
    pub max_source_pixels: u64,
    pub max_decoded_bytes: u64,
    pub max_source_rss_bytes: u64,
    pub max_encoder_rss_bytes: u64,
    pub max_output_bytes: u64,
    pub max_frames: u64,
    pub max_decode_threads: u32,
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedSourcePreview {
    pub snapshot: EditorSnapshot,
    pub sequence_id: String,
    pub identity: SourcePreviewIdentity,
    pub authority: TrustedJobAuthority,
    pub processing_policy: SourcePreviewPolicy,
}
