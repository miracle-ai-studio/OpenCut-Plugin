//! Revision-scoped source art. Peaks describe decoded source samples, without
//! element gain, mastering or the Classic waveform's burn/display curve.
use crate::{AssetVersion, Time, TimeRange};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceArtRequest {
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    /// A scoped waiter identity; it is neither an edit nor a persistent job.
    pub request_id: String,
    pub query: SourceArtQuery,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceArtQuery {
    Waveform {
        /// Explicit waveform rate, 44100 or 48000. Classic's device-default
        /// choice is made explicit so summaries cannot cross decoding profiles.
        sample_rate: u32,
        projection: WaveformProjection,
    },
    /// Primary video sample at one second, or a static image; Classic's
    /// sequential fit to 1280 x 720, no upscale, canvas JPEG quality 0.8.
    Thumbnail {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WaveformProjection {
    Source {
        /// None covers the actual decoded extent, rather than nominal metadata.
        range: Option<TimeRange>,
        bins: u32,
    },
    Element {
        sequence_id: String,
        track_id: String,
        element_id: String,
        /// Placement-local window. Rust resolves trim and effective source rate.
        local_range: TimeRange,
        bins: u32,
        /// Positive placement-local step; bins = ceil(duration / step).
        /// The last bin ends at the requested local window end.
        bin_duration: Time,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceArtCancel {
    pub project_id: String,
    pub revision: String,
    pub request_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceArtCancelResponse {
    pub cancel_requested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SourceArtIdentity {
    Native(NativeSourceArtIdentity),
    Browser(BrowserSourceArtIdentity),
}

/// Exact historical source-art proofs remain readable. New native execution
/// never constructs a browser identity or selects this runtime as a fallback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserSourceArtIdentity {
    /// source-peaks128-v1 or classic-thumbnail-1s-jpeg08-v1.
    pub algorithm: String,
    /// Actual source decoding and resampling path; bound with the module/runtime hashes.
    pub decode_profile: String,
    pub browser_version: String,
    pub browser_sha256: String,
    pub node_sha256: String,
    /// Hash of the complete pinned source-art renderer module manifest.
    pub module_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeSourceArtIdentity {
    pub algorithm: String,
    pub decode_profile: String,
    pub worker_sha256: String,
    pub module_sha256: String,
    pub decoder_libraries: Vec<crate::renderer_identity::NativeLibraryIdentity>,
    /// CPU raster/encoding dependency; no GPU or font execution is claimed.
    pub rasterizer: String,
    pub platform: crate::renderer_identity::NativeSystemIdentity,
}
impl SourceArtIdentity {
    pub fn algorithm(&self) -> &str {
        match self {
            Self::Native(v) => &v.algorithm,
            Self::Browser(v) => &v.algorithm,
        }
    }
    pub fn decode_profile(&self) -> &str {
        match self {
            Self::Native(v) => &v.decode_profile,
            Self::Browser(v) => &v.decode_profile,
        }
    }
    pub fn set_algorithm(&mut self, algorithm: &str) {
        match self {
            Self::Native(v) => v.algorithm = algorithm.into(),
            Self::Browser(v) => v.algorithm = algorithm.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceArtResponse {
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    pub version: AssetVersion,
    pub identity: SourceArtIdentity,
    pub data: SourceArtData,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceArtData {
    Waveform {
        sample_rate: u32,
        channels: u32,
        /// Canonical unsigned decimal, independent of JavaScript's safe integer.
        total_samples: String,
        bucket_size: u32,
        projection: WaveformProjection,
        /// Max of intersecting 128-sample source buckets across all channels.
        amplitudes: Vec<f32>,
    },
    ThumbnailReady {
        width: u32,
        height: u32,
        content_hash: String,
        /// Lowercase hexadecimal of at most 1 MiB of verified JPEG bytes.
        jpeg_bytes_hex: String,
        observation: SourceArtThumbnailObservation,
    },
    ThumbnailNoFrame {
        observation: SourceArtThumbnailObservation,
        reason: SourceArtNoFrameReason,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceArtThumbnailObservation {
    StaticImage {
        source_width: u32,
        source_height: u32,
    },
    Video {
        track_id: u32,
        source_width: u32,
        source_height: u32,
        requested_seconds: f64,
        /// Observed decoder seconds, not exact project ticks. Present only
        /// when getSample(1) returned a real sample; no synthetic hold frame.
        actual_timestamp_seconds: Option<f64>,
        frame_duration_seconds: Option<f64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceArtNoFrameReason {
    NoSampleAtOneSecond,
}
