//! Ephemeral fixed-revision audio playback. Counts are canonical decimal strings;
//! producer progress is never evidence of presentation or audible output.
use opencut_contracts::Time;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AudioPreviewProfile {
    ClassicRealtimeV31,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewIdentity {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub profile: AudioPreviewProfile,
    pub sample_rate: u32,
    pub channels: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewCreate {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub sample_rate: u32,
    /// Nonnegative presentation time, floored once to the output sample grid.
    pub start_time: Time,
    pub idempotency_key: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewInspect {
    pub session_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewAcquire {
    pub session_id: String,
    pub expected_generation: String,
    pub control_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewStream {
    pub session_id: String,
    pub generation: String,
    pub overlay_epoch: String,
    pub owner_id: String,
    pub lease_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TemporaryAudioGain {
    pub track_id: String,
    pub element_id: String,
    /// Static element dB only: -600..200 tenths. Mute/source/track are unchanged.
    pub decibels_tenths: i32,
    pub gesture_id: String,
    pub update_sequence: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewControl {
    pub session_id: String,
    pub generation: String,
    pub owner_id: String,
    pub lease_id: String,
    pub control_id: String,
    pub command: AudioPreviewCommand,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioPreviewClockSource {
    DeviceTimestamp,
}
/// Owner-reported device output estimate; never a physical audibility proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewOutputObservation {
    /// Content frames output since this generation's anchor, excluding underrun silence.
    pub frames: String,
    /// None when the device API does not quantify its physical output uncertainty.
    pub uncertainty_frames: Option<u32>,
    pub source: AudioPreviewClockSource,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioPreviewCommand {
    Play,
    Pause,
    Seek {
        sequence_time: Time,
    },
    Stop,
    Heartbeat,
    ReleaseOwner,
    SetTemporaryGain {
        gain: TemporaryAudioGain,
    },
    ClearTemporaryGain {
        gesture_id: String,
    },
    ReportConsumed {
        sequence: String,
        /// Content frames submitted to the output graph; releases FIFO credit.
        consumed_frames: String,
        playing: bool,
        stalled: bool,
        /// None means no valid device output observation, not zero output latency.
        output: Option<AudioPreviewOutputObservation>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioPreviewState {
    Ready,
    Playing,
    Paused,
    Ended,
    Failed,
    Stopped,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewSession {
    pub session_id: String,
    pub identity: AudioPreviewIdentity,
    pub generation: String,
    pub overlay_epoch: String,
    pub owner_id: String,
    pub lease_id: String,
    pub state: AudioPreviewState,
    pub actual_start_sample: String,
    pub total_frames: String,
    /// Confirmed submission cursor, used for producer credit, not presentation.
    pub submitted_time: Time,
    /// Last owner-confirmed device output time; remains at the anchor until observed.
    pub presentation_time: Time,
    pub output: Option<AudioPreviewOutputObservation>,
    pub consumed_frames: String,
    pub produced_through_sample: String,
    pub buffered_frames: String,
    pub presentation_fresh: bool,
    pub stalled: bool,
    pub lease_remaining_ms: String,
    pub expires_in_ms: String,
    pub temporary_gain: Option<TemporaryAudioGain>,
    pub failure_code: Option<String>,
    pub next_event_cursor: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewEventsInput {
    pub session_id: String,
    pub cursor: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewEvent {
    pub cursor: String,
    pub generation: String,
    pub overlay_epoch: String,
    pub kind: String,
    pub server_monotonic_ms: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewEvents {
    pub events: Vec<AudioPreviewEvent>,
    pub next_cursor: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewCapability {
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub profile: AudioPreviewProfile,
    /// Completed scene PCM. Consumers present it directly without a second
    /// compressor, gain or look-ahead delay.
    pub pcm_stage: AudioPreviewPcmStage,
    pub sample_rates: Vec<u32>,
    pub stream_path: String,
    pub max_sessions_per_database: u32,
    pub max_block_frames: u32,
    pub max_buffered_frames: u32,
    pub owner_lease_ms: u32,
    pub authorization_heartbeat_ms: u32,
    pub session_ttl_ms: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioPreviewPcmStage {
    Mastered,
}
/// One header precedes one interleaved stereo Float32 little-endian payload.
/// One bounded PCM envelope, independent from project storage formats.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioPreviewPcmHeader {
    pub pcm_stage: AudioPreviewPcmStage,
    pub session_id: String,
    pub identity: AudioPreviewIdentity,
    pub generation: String,
    pub overlay_epoch: String,
    pub sequence: String,
    pub start_sample: String,
    pub frame_count: u32,
    pub content_hash: String,
}
/// Binary transport types come from these same reviewed Rust declarations.
pub fn stream_schemas() -> serde_json::Value {
    serde_json::json!({
        "input": schemars::schema_for!(AudioPreviewStream),
        "header": schemars::schema_for!(AudioPreviewPcmHeader),
    })
}
