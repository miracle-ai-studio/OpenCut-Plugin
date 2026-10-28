//! Additive, path-free fixed-revision Classic audio jobs.
use opencut_contracts::AssetLockManifest;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const AUDIO_WAV_KIND: &str = "render.project_audio.v4";
pub const AUDIO_RANGE_WAV_KIND: &str = "render.project_audio_range.v4";

/// Half-open sample interval at the request's target sample rate. No timeline
/// quantization or decoder seek is implied; mastering precedes this crop.
pub use crate::render_requests::AudioSampleRange;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitAudioRangeWav {
    pub idempotency_key: String,
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub sample_rate: u32,
    pub range: AudioSampleRange,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioRangeWavCapability {
    pub schema_version: u32,
    pub available: bool,
    pub submit_tool: String,
    pub sample_rates: Vec<u32>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitAudioWav {
    pub idempotency_key: String,
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    /// Hertz; the application accepts only 44100 or 48000.
    pub sample_rate: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioWavCapability {
    pub schema_version: u32,
    pub available: bool,
    pub submit_tool: String,
    pub sample_rates: Vec<u32>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AudioProfile {
    #[serde(rename = "classic-export-v31-web-audio")]
    ClassicExportV31WebAudio,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioSampleFormat {
    IeeeFloat32Le,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioWavIdentity {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub profile: AudioProfile,
    pub sample_rate: u32,
    pub channels: u32,
    /// Exact output frame count, canonical unsigned decimal across JS boundaries.
    pub frame_count: String,
    pub sample_format: AudioSampleFormat,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum AudioDecoderProfile {
    #[serde(rename = "web-audio-decode-at-target-rate")]
    WebAudioDecodeAtTargetRate,
    #[serde(rename = "mediabunny-1.41.0-concatenate-offline-resample")]
    Mediabunny141ConcatenateOfflineResample,
    #[serde(rename = "native-libav-sinc-at-target-rate")]
    NativeLibavSincAtTargetRate,
    #[serde(rename = "native-libav-concatenate-linear-resample")]
    NativeLibavConcatenateLinearResample,
    #[serde(rename = "native-libav-windowed-concatenate-linear-resample")]
    NativeLibavWindowedConcatenateLinearResample,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum AudioPitchProfile {
    #[serde(rename = "unprepared-linear")]
    UnpreparedLinear,
    #[serde(rename = "prepared-linear")]
    PreparedLinear,
    #[serde(rename = "prepared-soundtouch")]
    PreparedSoundtouch,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AudioMasteringProfile {
    #[serde(rename = "classic-export-whole-scene-web-audio-v31")]
    ClassicExportWholeSceneWebAudioV31,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AudioExecutionSummary {
    pub host_sha256: String,
    #[serde(flatten)]
    pub runtime: AudioRuntimeIdentity,
    /// Unique profiles used by this fixed scene, not claims about unused engines.
    pub decoder_profiles: Vec<AudioDecoderProfile>,
    pub pitch_profiles: Vec<AudioPitchProfile>,
    pub mastering_profile: AudioMasteringProfile,
    pub mastering_applied: bool,
}
/// A union for immutable proof reads, never an execution backend selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AudioRuntimeIdentity {
    Native(opencut_contracts::renderer_identity::NativeAudioIdentity),
    HistoricalBrowser(HistoricalBrowserAudioIdentity),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoricalBrowserAudioIdentity {
    pub browser_version: String,
    pub browser_sha256: String,
    pub module_sha256: String,
    pub playwright_version: String,
    pub mediabunny_version: String,
    pub soundtouch_version: String,
}
#[derive(JsonSchema)]
struct AudioSummaryFields {
    host_sha256: String,
    decoder_profiles: Vec<AudioDecoderProfile>,
    pitch_profiles: Vec<AudioPitchProfile>,
    mastering_profile: AudioMasteringProfile,
    mastering_applied: bool,
}
impl JsonSchema for AudioExecutionSummary {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AudioExecutionSummary".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let common = AudioSummaryFields::json_schema(generator);
        let mut variants = [
            opencut_contracts::renderer_identity::NativeAudioIdentity::json_schema(generator),
            HistoricalBrowserAudioIdentity::json_schema(generator),
        ];
        for variant in &mut variants {
            let object = variant.ensure_object();
            object
                .get_mut("properties")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .extend(common.as_value()["properties"].as_object().unwrap().clone());
            object
                .get_mut("required")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .extend(
                    common.as_value()["required"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .cloned(),
                );
        }
        schemars::json_schema!({"anyOf":variants})
    }
}
impl<'de> Deserialize<'de> for AudioExecutionSummary {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            host_sha256: String,
            decoder_profiles: Vec<AudioDecoderProfile>,
            pitch_profiles: Vec<AudioPitchProfile>,
            mastering_profile: AudioMasteringProfile,
            mastering_applied: bool,
            #[serde(flatten)]
            runtime: serde_json::Map<String, serde_json::Value>,
        }
        let f = Fields::deserialize(deserializer)?;
        let runtime = serde_json::from_value(serde_json::Value::Object(f.runtime))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            host_sha256: f.host_sha256,
            runtime,
            decoder_profiles: f.decoder_profiles,
            pitch_profiles: f.pitch_profiles,
            mastering_profile: f.mastering_profile,
            mastering_applied: f.mastering_applied,
        })
    }
}
/* Profiles and runtime fields above deliberately retain the exact historical
 * serialized object while native proofs declare only the executing engine. */
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioArtifactMetadata {
    pub mime_type: String,
    pub size_bytes: String,
    pub content_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioWavResult {
    pub schema_version: u32,
    pub identity: AudioWavIdentity,
    pub asset_lock: AssetLockManifest,
    /// SHA-256 of the complete fixed snapshot plus sequence, rate and profile.
    /// This is separate from the public submission/idempotency identity.
    pub render_input_sha256: String,
    pub execution: AudioExecutionSummary,
    pub artifact: AudioArtifactMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioRangeWavIdentity {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub profile: AudioProfile,
    pub sample_rate: u32,
    pub channels: u32,
    pub start_sample: String,
    pub frame_count: String,
    pub whole_scene_frame_count: String,
    pub sample_format: AudioSampleFormat,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioRangeWavResult {
    pub schema_version: u32,
    pub identity: AudioRangeWavIdentity,
    pub asset_lock: AssetLockManifest,
    /// Complete fixed scene, execution profile and exact output sample range.
    pub render_input_sha256: String,
    pub execution: AudioExecutionSummary,
    pub artifact: AudioArtifactMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_runtime_proofs_preserve_history_and_reject_mixed_or_invented_engine_fields() {
        let mut historical = serde_json::json!({"host_sha256":"a".repeat(64),"browser_version":"pinned","browser_sha256":"b".repeat(64),"module_sha256":"c".repeat(64),
            "playwright_version":"1.63.0","mediabunny_version":"1.41.0","soundtouch_version":"0.3.0","decoder_profiles":["web-audio-decode-at-target-rate"],"pitch_profiles":["unprepared-linear"],
            "mastering_profile":"classic-export-whole-scene-web-audio-v31","mastering_applied":false});
        let decoded: AudioExecutionSummary = serde_json::from_value(historical.clone()).unwrap();
        assert!(matches!(
            decoded.runtime,
            AudioRuntimeIdentity::HistoricalBrowser(_)
        ));
        assert_eq!(serde_json::to_value(decoded).unwrap(), historical);
        let native = opencut_contracts::renderer_identity::NativeAudioIdentity {
            worker_sha256: "d".repeat(64),
            decoder_libraries: vec![],
            decoder_profile: "libav-authorized-audio".into(),
            dsp_profile: "classic-audio-f32".into(),
            dsp_sha256: "e".repeat(64),
            soundtouch_source_sha256: "f".repeat(64),
            platform: opencut_contracts::renderer_identity::NativeSystemIdentity {
                os: "macos".into(),
                architecture: "x86_64".into(),
                kernel_release: "darwin".into(),
                system_build: "test".into(),
            },
        };
        let value = AudioExecutionSummary {
            host_sha256: "a".repeat(64),
            runtime: AudioRuntimeIdentity::Native(native),
            decoder_profiles: vec![AudioDecoderProfile::NativeLibavSincAtTargetRate],
            pitch_profiles: vec![AudioPitchProfile::UnpreparedLinear],
            mastering_profile: AudioMasteringProfile::ClassicExportWholeSceneWebAudioV31,
            mastering_applied: false,
        };
        let native = serde_json::to_value(&value).unwrap();
        assert!(serde_json::from_value::<AudioExecutionSummary>(native.clone()).is_ok());
        for field in ["browser_version", "gpu_backend", "font_backend"] {
            let mut mixed = native.clone();
            mixed[field] = serde_json::json!("invented");
            assert!(serde_json::from_value::<AudioExecutionSummary>(mixed).is_err());
        }
        historical["worker_sha256"] = native["worker_sha256"].clone();
        assert!(serde_json::from_value::<AudioExecutionSummary>(historical).is_err());
        let schema = schemars::schema_for!(AudioExecutionSummary);
        for variant in schema.as_value()["anyOf"].as_array().unwrap() {
            assert_eq!(variant["additionalProperties"], false);
            assert!(
                variant["required"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("host_sha256"))
            );
        }
    }
    #[test]
    fn submission_cannot_supply_execution_authority_or_native_locations() {
        let input = serde_json::json!({"idempotency_key":"key", "project_id":"p", "revision":"r", "sequence_id":"s", "sample_rate":48000});
        assert!(serde_json::from_value::<SubmitAudioWav>(input.clone()).is_ok());
        for field in [
            "snapshot",
            "path",
            "authority",
            "processing_policy",
            "asset_lock",
        ] {
            let mut attempt = input.clone();
            attempt[field] = serde_json::json!("injected");
            assert!(serde_json::from_value::<SubmitAudioWav>(attempt).is_err());
        }
        let mut range = input;
        range["range"] = serde_json::json!({"start_sample":"17", "frame_count":"1024"});
        assert!(serde_json::from_value::<SubmitAudioRangeWav>(range.clone()).is_ok());
        assert!(serde_json::from_value::<SubmitAudioWav>(range.clone()).is_err());
        range["range"]["path"] = serde_json::json!("/injected");
        assert!(serde_json::from_value::<SubmitAudioRangeWav>(range).is_err());
    }
}
