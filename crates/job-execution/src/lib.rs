//! Internal persisted worker inputs; never imported by a client SDK.
pub mod scene_mask_video;
use opencut_contracts::{AssetLockManifest, ProjectSnapshot, Time, TimeRange};
pub mod historical_visual;
pub mod media_frame;
pub mod media_video;
pub mod scene_audio;
pub mod scene_blend_video;
pub mod scene_caption_video;
pub mod scene_graphic_video;
pub mod scene_text_frame;
pub mod source_preview;
use opencut_media_contracts::{Container, FrameAlignment, VideoCodec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedProjectRenderInput {
    pub snapshot: ProjectSnapshot,
    pub sequence_id: String,
    pub container: Container,
    pub video_codec: VideoCodec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_lock: Option<AssetLockManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedProjectFrameInput {
    pub snapshot: ProjectSnapshot,
    pub sequence_id: String,
    pub requested_time: Time,
    pub alignment: FrameAlignment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_lock: Option<AssetLockManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedProjectRangeInput {
    pub snapshot: ProjectSnapshot,
    pub sequence_id: String,
    pub requested_range: TimeRange,
    pub alignment: FrameAlignment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_lock: Option<AssetLockManifest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedTranscribeInput {
    pub snapshot: ProjectSnapshot,
    pub sequence_id: String,
    pub asset_id: String,
    pub language: Option<String>,
    #[serde(default)]
    pub word_timestamps: bool,
    pub asset_lock: AssetLockManifest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedAnalysisInput {
    pub snapshot: ProjectSnapshot,
    pub sequence_id: String,
    pub asset_id: String,
    pub options: AnalysisOptions,
    pub asset_lock: AssetLockManifest,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedSpeakerTurnsInput {
    pub snapshot: ProjectSnapshot,
    pub sequence_id: String,
    pub asset_id: String,
    pub language: String,
    pub asset_lock: AssetLockManifest,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnalysisOptions {
    Silence { noise_db: i16, min_duration_ms: u32 },
    Shots { threshold: f32 },
    Waveform { buckets: u16 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn snapshot_json() -> Value {
        json!({
            "project": {
                "id": "project-1", "name": "Fixture", "schema_version": 2,
                "default_sequence_id": "sequence-1", "assets": [],
                "sequences": [{
                    "id": "sequence-1", "name": "Main", "width": 1920, "height": 1080,
                    "fps": {"numerator": 24, "denominator": 1}, "tracks": [], "caption_cues": []
                }]
            },
            "revision": "rev-1"
        })
    }

    fn lock_json() -> Value {
        json!({
            "version": 1, "revision": "rev-1", "sequence_id": "sequence-1",
            "assets": [{"asset_id": "asset-1", "content_hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "size": 44}]
        })
    }

    #[test]
    fn prepared_render_without_lock_matches_historical_execution_shape() {
        let historical = json!({
            "snapshot": snapshot_json(), "sequence_id": "sequence-1",
            "container": "mp4", "video_codec": "h264"
        });
        let prepared: PreparedProjectRenderInput =
            serde_json::from_value(historical.clone()).unwrap();
        assert_eq!(prepared.asset_lock, None);
        assert_eq!(serde_json::to_value(prepared).unwrap(), historical);
    }

    #[test]
    fn prepared_frame_with_lock_preserves_manifest_shape() {
        let fixture = json!({
            "snapshot": snapshot_json(), "sequence_id": "sequence-1",
            "requested_time": {"ticks": "12", "timebase": 24}, "alignment": "nearest",
            "asset_lock": lock_json()
        });
        let prepared: PreparedProjectFrameInput = serde_json::from_value(fixture.clone()).unwrap();
        assert_eq!(prepared.asset_lock.as_ref().unwrap().assets[0].size, 44);
        assert_eq!(serde_json::to_value(prepared).unwrap(), fixture);
    }

    #[test]
    fn requests_reject_unknown_fields() {
        let mut input = json!({
            "snapshot": snapshot_json(), "sequence_id": "sequence-1",
            "container": "mp4", "video_codec": "h264"
        });
        input["unexpected"] = json!(true);
        assert!(serde_json::from_value::<PreparedProjectRenderInput>(input).is_err());
    }
}

/// Authoritative sealed upload, persisted by the host rather than supplied by a caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedMediaImportInput {
    pub upload_id: String,
    pub project_id: String,
    pub media_id: String,
    pub name: String,
    pub size_bytes: u64,
    pub content_hash: String,
    /// Captured by the host; absent only on exact retained historical inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<scene_audio::TrustedJobAuthority>,
    /// Historical jobs captured a bounded inline manifest. Current jobs read
    /// immutable acknowledged rows through the application paging port.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<PreparedUploadChunk>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedUploadChunk {
    pub offset: u64,
    pub size_bytes: u64,
    pub content_hash: String,
}

pub mod scene_video;
