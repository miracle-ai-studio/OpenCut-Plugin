//! Additive, path-free fixed-revision Classic GPU scene frames.
use opencut_contracts::{AssetLockManifest, Time};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MEDIA_FRAME_PNG_KIND: &str = "render.project_frame.v4";

pub use crate::render_requests::FrameRenderInput;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaFramePngCapability {
    pub schema_version: u32,
    /// Configured process availability; each job verifies its actual GPU/backend.
    pub available: bool,
    pub submit_tool: String,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MediaFrameProfile {
    #[serde(rename = "classic-v31-gpu-normal-frame")]
    ClassicV31GpuNormalFrame,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaFrameIdentity {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    /// Exact requested scene time; actual video observations are separate.
    pub time: Time,
    pub width: u32,
    pub height: u32,
    pub profile: MediaFrameProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaFrameNoSourceReason {
    OutsideReadWindow,
    Eof,
    TimestampGap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaFrameSourceObservation {
    pub element_id: String,
    pub asset_id: String,
    pub requested_time: Option<Time>,
    pub selected_video_stream: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Observed presentation seconds. Exact demux ticks remain decoder facts.
    pub actual_timestamp_seconds: Option<f64>,
    pub frame_duration_seconds: Option<f64>,
    /// True only for an observed terminal video frame held by the authored
    /// layout. Omission retains the exact historical non-hold result shape.
    #[serde(default, skip_serializing_if = "is_false")]
    pub eof_held: bool,
    pub no_frame_reason: Option<MediaFrameNoSourceReason>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MediaFrameExecutionSummary {
    pub host_sha256: String,
    #[serde(flatten)]
    pub runtime: VisualRuntimeIdentity,
}

impl JsonSchema for MediaFrameExecutionSummary {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MediaFrameExecutionSummary".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Each closed object must include host_sha256 before joining the union.
        // A flattened union of closed $refs would reject the host on both arms.
        let mut variants = [
            opencut_contracts::renderer_identity::NativeVisualIdentity::json_schema(generator),
            HistoricalBrowserVisualIdentity::json_schema(generator),
        ];
        for variant in &mut variants {
            let object = variant.ensure_object();
            object
                .get_mut("properties")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("host_sha256".into(), serde_json::json!({"type":"string"}));
            object
                .get_mut("required")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!("host_sha256"));
        }
        schemars::json_schema!({"anyOf": variants})
    }
}

impl<'de> Deserialize<'de> for MediaFrameExecutionSummary {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Serde's deny_unknown_fields cannot consume a flattened untagged enum.
        // Capture the host separately, then strictly decode every remaining key
        // as one complete runtime proof. Historical bytes keep their exact shape.
        #[derive(Deserialize)]
        struct Fields {
            host_sha256: String,
            #[serde(flatten)]
            runtime: serde_json::Map<String, serde_json::Value>,
        }
        let fields = Fields::deserialize(deserializer)?;
        let runtime = serde_json::from_value(serde_json::Value::Object(fields.runtime))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            host_sha256: fields.host_sha256,
            runtime,
        })
    }
}

/// Historical proofs remain readable without retaining their execution backend.
/// Every newly executed frame produces Native; this is not a runtime selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum VisualRuntimeIdentity {
    Native(opencut_contracts::renderer_identity::NativeVisualIdentity),
    HistoricalBrowser(HistoricalBrowserVisualIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoricalBrowserVisualIdentity {
    pub browser_version: String,
    pub browser_sha256: String,
    pub module_sha256: String,
    pub shader_sha256: String,
    pub playwright_version: String,
    pub mediabunny_version: String,
    pub gpu_backend: String,
    pub gpu_device: String,
    pub texture_format: String,
    pub decoder_profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaFrameArtifactMetadata {
    pub mime_type: String,
    pub size_bytes: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaFramePngResult {
    pub schema_version: u32,
    pub identity: MediaFrameIdentity,
    pub asset_lock: AssetLockManifest,
    pub render_input_sha256: String,
    pub execution: MediaFrameExecutionSummary,
    pub sources: Vec<MediaFrameSourceObservation>,
    pub artifact: MediaFrameArtifactMetadata,
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn stored_browser_identity_round_trips_without_relabeling_and_rejects_mixed_proofs() {
        let original = json!({"host_sha256":"a".repeat(64),"browser_version":"153.0.8010.12","browser_sha256":"b".repeat(64),
            "module_sha256":"c".repeat(64),"shader_sha256":"d".repeat(64),"playwright_version":"1.63.0","mediabunny_version":"1.41.0",
            "gpu_backend":"webgpu","gpu_device":"old device","texture_format":"bgra8unorm","decoder_profile":"historical exact value"});
        let parsed: MediaFrameExecutionSummary = serde_json::from_value(original.clone()).unwrap();
        assert!(matches!(
            parsed.runtime,
            VisualRuntimeIdentity::HistoricalBrowser(_)
        ));
        assert_eq!(serde_json::to_value(parsed).unwrap(), original);
        let mut mixed = original;
        mixed["renderer_sha256"] = json!("e".repeat(64));
        assert!(serde_json::from_value::<MediaFrameExecutionSummary>(mixed).is_err());
    }

    #[test]
    fn native_identity_round_trips_and_rejects_unknown_or_partial_proofs() {
        let native = json!({"host_sha256":"a".repeat(64),"renderer_sha256":"b".repeat(64),
            "shader_sha256":"c".repeat(64),"decoder_libraries":[],"wgpu_version":"30.0.1",
            "rasterizer":"fixed native rasterizer","gpu_backend":"metal","gpu_device":"test GPU",
            "gpu_driver":"test driver","gpu_vendor_id":1,"gpu_device_id":2,"texture_format":"bgra8unorm",
            "decoder_profile":"libav-pts-half-open;verified-avio"});
        let parsed: MediaFrameExecutionSummary = serde_json::from_value(native.clone()).unwrap();
        assert!(matches!(parsed.runtime, VisualRuntimeIdentity::Native(_)));
        assert_eq!(serde_json::to_value(parsed).unwrap(), native);
        let mut unknown = native.clone();
        unknown["unproved"] = json!(true);
        assert!(serde_json::from_value::<MediaFrameExecutionSummary>(unknown).is_err());
        let mut partial = native;
        partial.as_object_mut().unwrap().remove("renderer_sha256");
        assert!(serde_json::from_value::<MediaFrameExecutionSummary>(partial).is_err());
    }
}
