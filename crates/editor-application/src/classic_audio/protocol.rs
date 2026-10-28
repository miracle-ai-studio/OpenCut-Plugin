use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioResource {
    pub asset_id: String,
    pub content_hash: String,
    pub size: u64,
    pub kind: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioQuotaReceipt {
    pub profile: String,
    pub max_rss_bytes: String,
    pub observed_peak_rss_bytes: String,
    pub monitor_interval_ms: u32,
    pub elapsed_ms: u64,
    pub hard_memory_enforced: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", content = "args", deny_unknown_fields)]
pub enum AudioCallback {
    #[serde(rename = "resolveVerifiedBytes")]
    Resolve(ResolveBytes),
    #[serde(rename = "openVerifiedRange")]
    OpenRange(ResolveBytes),
    #[serde(rename = "readVerifiedBytes")]
    Read(ReadBytes),
    #[serde(rename = "releaseVerifiedBytes")]
    Release(ReleaseBytes),
    #[serde(rename = "releaseDecodedSource")]
    ReleaseDecoded(DecodedRelease),
    #[serde(rename = "admitDecode")]
    Admit(DecodeAdmission),
    #[serde(rename = "admitContainerDecode")]
    AdmitContainer(DecodeAdmission),
    #[serde(rename = "admitPcmAudioDecode")]
    AdmitPcmAudioWindowed(DecodeAdmission),
    #[serde(rename = "admitPreviewDecode")]
    AdmitPreviewDecode(DecodeAdmission),
    #[serde(rename = "compilePitchPreparation")]
    Pitch(PitchRequest),
    #[serde(rename = "compileSourcePositions")]
    Positions(PositionRequest),
    #[serde(rename = "compileBlock")]
    Block(BlockRequest),
    #[serde(rename = "bindPreviewSources")]
    BindPreview(BindPreviewSources),
    #[serde(rename = "previewSourceWindows")]
    PreviewWindows(PreviewBlockRequest),
    #[serde(rename = "admitPreviewBlock")]
    AdmitPreviewBlock(PreviewBlockRequest),
    #[serde(rename = "compilePreviewBlock")]
    PreviewBlock(PreviewBlockRequest),
    #[serde(rename = "observePreviewDecoded")]
    ObservePreviewDecoded(PreviewDecodedObservation),
    #[serde(rename = "observeWindowedPreviewDecoded")]
    ObserveWindowedPreview(WindowedPreviewObservation),
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowedPreviewObservation {
    pub decoded: PreviewDecodedObservation,
    pub resident_frame_count: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewDecodedObservation {
    pub resource: AudioResource,
    pub sample_rate: u32,
    pub frame_count: String,
    pub channels: u32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindPreviewSources {
    pub source_formats: Vec<opencut_composition::scene::PreviewSourceFormat>,
    pub window: Option<PreviewBlockRequest>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewBlockRequest {
    pub start_sample: String,
    pub frame_count: u32,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveBytes {
    pub resource: AudioResource,
    pub handle: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadBytes {
    pub transfer_id: String,
    pub offset: String,
    pub count: u32,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseBytes {
    pub transfer_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecodedRelease {
    pub resource: AudioResource,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecodeAdmission {
    pub resource: AudioResource,
    #[serde(default)]
    pub primary: Option<ClassicAudioPrimarySelection>,
}

/// Residency for an ordinary primary track. No complete-source PCM length is
/// known or admitted; every command supplies fresh bounded PTS observations.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollingPreviewAdmission {
    pub working_frames: u64,
    pub max_channels: u32,
    pub max_chunk_frames: u32,
    pub source_sample_rate: u32,
    pub target_sample_rate: u32,
}

/// One source may feed both ordinary PTS windows and prepared concat PCM.
/// Allocation choices describe consumers,not protocol generations.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewDecodeAllocation {
    pub stream: Option<RollingPreviewAdmission>,
    pub preparation: Option<WindowDecodeAdmission>,
}

/// Private decoder admission. Logical source frames never authorize
/// a full PCM allocation; working_frames retains the original four-copy bound.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowDecodeAdmission {
    pub mode: String,
    pub max_frames: u64,
    pub max_channels: u32,
    pub working_frames: u64,
    pub source_sample_rate: u32,
    pub target_sample_rate: u32,
    pub max_native_frames: String,
    pub windows: Vec<DecodePcmWindow>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecodePcmWindow {
    pub source_start_frame: String,
    pub source_end_frame: String,
    pub target_start_frame: String,
}
/// Observed by the pinned container decoder before PCM allocation. track_id is
/// that decoder's track identity, never an FFmpeg stream ordinal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicAudioPrimarySelection {
    pub track_id: u32,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PitchRequest {
    pub element_id: String,
    pub decoded_sample_rate: u32,
    pub decoded_frame_count: String,
    pub decoded_channels: u32,
    /// Sparse decoded PCM reports its actual residency; dense PCM uses null.
    pub resident_frame_count: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PositionRequest {
    pub element_id: String,
    pub local_start_sample: String,
    pub frame_count: u32,
    pub source_sample_rate: u32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlockRequest {
    pub start_sample: String,
    pub frame_count: u32,
    pub source_formats: Vec<FinalSourceFormat>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalSourceFormat {
    pub element_id: String,
    pub sample_rate: u32,
    pub frame_count: String,
    pub prepared: bool,
    pub channels: u32,
}
pub fn decimal(value: &str) -> Result<u64, EditorError> {
    if value.is_empty()
        || value.len() > 20
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid("Expected a canonical unsigned decimal string"));
    }
    value.parse().map_err(|_| invalid("Decimal exceeds u64"))
}
