//! Transport-neutral contracts for durable native media jobs.
pub mod asset_analysis;
pub mod export_video;
pub mod historical_visual;
pub mod media_frame;
pub mod media_video;
pub mod render_requests;
pub mod scene_audio;
pub mod scene_blend_frame;
pub mod scene_blend_video;
pub mod scene_caption_video;
pub mod scene_frame;
pub mod scene_graphic_video;
pub mod scene_mask_frame;
pub mod scene_mask_video;
pub mod scene_text_frame;
pub mod scene_transcription;

use opencut_contracts::AssetLockManifest;
use opencut_contracts::{EditorError, Time, TimeRange};
use opencut_media_contracts::{
    AudioOutput, Container, ExportResult, ExportSegment, ExtractedFrame, FrameAlignment,
    MediaMetadata, ProbeRequest, TextLayoutReport, VerificationReport, VerifyRequest, VideoCodec,
    VideoOutput,
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameJobInput {
    pub input: String,
    pub requested_time: opencut_media_contracts::MediaTime,
    #[serde(default)]
    pub video_stream: Option<u32>,
    pub alignment: FrameAlignment,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportJobInput {
    pub segments: Vec<ExportSegment>,
    pub container: Container,
    pub video: VideoOutput,
    pub audio: AudioOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectExportInput {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub container: Container,
    pub video_codec: VideoCodec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectFrameInput {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub requested_time: Time,
    pub alignment: FrameAlignment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectRangeInput {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub requested_range: TimeRange,
    pub alignment: FrameAlignment,
}

/// Public analysis request. The host chooses the executable, model and source path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranscribeJobInput {
    pub idempotency_key: String,
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub word_timestamps: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TranscribeJobResult {
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    pub asset_version: opencut_contracts::LockedAsset,
    pub transcription: opencut_media_contracts::TranscriptionResult,
}

/// Transcribe the audible mix of one fixed visual-project scene.
pub use scene_transcription::SceneTranscriptionRequest;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SequenceTranscribeResult {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub duration: Time,
    pub asset_lock: opencut_contracts::AssetLockManifest,
    pub transcription: opencut_media_contracts::TranscriptionResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SilenceJobInput {
    pub idempotency_key: String,
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    pub noise_db: i16,
    pub min_duration_ms: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShotsJobInput {
    pub idempotency_key: String,
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    pub threshold: f32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaveformJobInput {
    pub idempotency_key: String,
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    pub buckets: u16,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisJobResult<R> {
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    pub asset_version: opencut_contracts::LockedAsset,
    pub analysis: R,
}
pub type SilenceJobResult = AnalysisJobResult<opencut_media_contracts::SilenceResult>;
pub type ShotsJobResult = AnalysisJobResult<opencut_media_contracts::ShotsResult>;
pub type WaveformJobResult = AnalysisJobResult<opencut_media_contracts::WaveformResult>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpeakerTurnsJobInput {
    pub idempotency_key: String,
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    /// Only English is supported by the verified local tinydiarize model.
    pub language: String,
}
pub type SpeakerTurnsJobResult = AnalysisJobResult<opencut_media_contracts::SpeakerTurnsResult>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum JobStartInput {
    #[serde(rename = "media.probe")]
    Probe {
        idempotency_key: String,
        input: ProbeRequest,
    },
    #[serde(rename = "preview.frame")]
    Frame {
        idempotency_key: String,
        input: FrameJobInput,
    },
    #[serde(rename = "preview.project_frame")]
    ProjectFrame {
        idempotency_key: String,
        input: ProjectFrameInput,
    },
    #[serde(rename = "preview.composited_frame")]
    CompositedFrame {
        idempotency_key: String,
        input: ProjectFrameInput,
    },
    #[serde(rename = "preview.project_range")]
    ProjectRange {
        idempotency_key: String,
        input: ProjectRangeInput,
    },
    #[serde(rename = "render.export")]
    Export {
        idempotency_key: String,
        input: ExportJobInput,
    },
    #[serde(rename = "render.project")]
    Project {
        idempotency_key: String,
        input: ProjectExportInput,
    },
    #[serde(rename = "artifact.verify")]
    Verify {
        idempotency_key: String,
        input: VerifyRequest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobInput {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactDescriptor {
    pub path: String,
    pub mime_type: String,
    pub size: u64,
    pub content_hash: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetLockingMode {
    MediaContentLocked,
    #[default]
    LegacyUnlocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFrameQuality {
    EncodedProjectFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompositedProjectFrameQuality {
    CompositedProjectFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRangeQuality {
    EncodedProjectRange,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MediaFrameJobResult {
    pub media: ExtractedFrame,
    pub artifact: ArtifactDescriptor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MediaExportJobResult {
    pub media: ExportResult,
    pub artifact: ArtifactDescriptor,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ProjectRenderJobResult {
    pub revision: String,
    #[serde(default)]
    pub asset_lock: Option<AssetLockManifest>,
    #[serde(default)]
    pub asset_locking: AssetLockingMode,
    pub media: ExportResult,
    pub artifact: ArtifactDescriptor,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ProjectFrameJobResult {
    pub revision: String,
    pub sequence_id: String,
    pub requested_time: Time,
    pub actual_sequence_time: Time,
    pub alignment: FrameAlignment,
    pub quality: ProjectFrameQuality,
    #[serde(default)]
    pub asset_lock: Option<AssetLockManifest>,
    #[serde(default)]
    pub asset_locking: AssetLockingMode,
    pub width: u32,
    pub height: u32,
    pub text_layouts: Vec<TextLayoutReport>,
    pub artifact: ArtifactDescriptor,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CompositedProjectFrameJobResult {
    pub revision: String,
    pub sequence_id: String,
    pub requested_time: Time,
    pub actual_sequence_time: Time,
    pub alignment: FrameAlignment,
    pub quality: CompositedProjectFrameQuality,
    #[serde(default)]
    pub asset_lock: Option<AssetLockManifest>,
    #[serde(default)]
    pub asset_locking: AssetLockingMode,
    pub width: u32,
    pub height: u32,
    pub text_layouts: Vec<TextLayoutReport>,
    pub artifact: ArtifactDescriptor,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ProjectRangeJobResult {
    pub revision: String,
    pub sequence_id: String,
    pub requested_range: TimeRange,
    pub actual_sequence_range: TimeRange,
    pub alignment: FrameAlignment,
    pub quality: ProjectRangeQuality,
    #[serde(default)]
    pub asset_lock: Option<AssetLockManifest>,
    #[serde(default)]
    pub asset_locking: AssetLockingMode,
    pub media: ExportResult,
    pub artifact: ArtifactDescriptor,
}

pub fn validate_asset_locking(
    asset_lock: Option<&AssetLockManifest>,
    mode: AssetLockingMode,
) -> Result<(), EditorError> {
    match (asset_lock, mode) {
        (Some(_), AssetLockingMode::MediaContentLocked)
        | (None, AssetLockingMode::LegacyUnlocked) => Ok(()),
        (Some(_), AssetLockingMode::LegacyUnlocked) => Err(invalid_result(
            "asset_lock is present but asset_locking is legacy_unlocked",
        )),
        (None, AssetLockingMode::MediaContentLocked) => Err(invalid_result(
            "asset_locking is media_content_locked but asset_lock is absent",
        )),
    }
}

impl ProjectRenderJobResult {
    pub fn validate(&self) -> Result<(), EditorError> {
        validate_asset_locking(self.asset_lock.as_ref(), self.asset_locking)?;
        if let Some(asset_lock) = &self.asset_lock
            && asset_lock.revision != self.revision
        {
            return Err(invalid_result(
                "asset_lock revision does not match the rendered revision",
            ));
        }
        Ok(())
    }
}

impl ProjectFrameJobResult {
    pub fn validate(&self) -> Result<(), EditorError> {
        validate_asset_locking(self.asset_lock.as_ref(), self.asset_locking)?;
        if let Some(asset_lock) = &self.asset_lock {
            if asset_lock.revision != self.revision {
                return Err(invalid_result(
                    "asset_lock revision does not match the rendered revision",
                ));
            }
            if asset_lock.sequence_id != self.sequence_id {
                return Err(invalid_result(
                    "asset_lock sequence_id does not match the rendered sequence",
                ));
            }
        }
        Ok(())
    }
}

impl CompositedProjectFrameJobResult {
    pub fn validate(&self) -> Result<(), EditorError> {
        validate_frame_result(
            &self.revision,
            &self.sequence_id,
            self.asset_lock.as_ref(),
            self.asset_locking,
        )
    }
}

impl ProjectRangeJobResult {
    pub fn validate(&self) -> Result<(), EditorError> {
        validate_frame_result(
            &self.revision,
            &self.sequence_id,
            self.asset_lock.as_ref(),
            self.asset_locking,
        )
    }
}

fn validate_frame_result(
    revision: &str,
    sequence_id: &str,
    asset_lock: Option<&AssetLockManifest>,
    asset_locking: AssetLockingMode,
) -> Result<(), EditorError> {
    validate_asset_locking(asset_lock, asset_locking)?;
    if let Some(asset_lock) = asset_lock {
        if asset_lock.revision != revision {
            return Err(invalid_result(
                "asset_lock revision does not match the rendered revision",
            ));
        }
        if asset_lock.sequence_id != sequence_id {
            return Err(invalid_result(
                "asset_lock sequence_id does not match the rendered sequence",
            ));
        }
    }
    Ok(())
}

fn invalid_result(message: impl Into<String>) -> EditorError {
    EditorError::new("INVALID_JOB_RESULT", message)
}

#[derive(Deserialize)]
struct ProjectRenderJobResultWire {
    revision: String,
    #[serde(default)]
    asset_lock: Option<AssetLockManifest>,
    #[serde(default)]
    asset_locking: AssetLockingMode,
    media: ExportResult,
    artifact: ArtifactDescriptor,
}

impl<'de> Deserialize<'de> for ProjectRenderJobResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ProjectRenderJobResultWire::deserialize(deserializer)?;
        let result = Self {
            revision: wire.revision,
            asset_lock: wire.asset_lock,
            asset_locking: wire.asset_locking,
            media: wire.media,
            artifact: wire.artifact,
        };
        result.validate().map_err(serde::de::Error::custom)?;
        Ok(result)
    }
}

#[derive(Deserialize)]
struct ProjectFrameJobResultWire {
    revision: String,
    sequence_id: String,
    requested_time: Time,
    actual_sequence_time: Time,
    alignment: FrameAlignment,
    quality: ProjectFrameQuality,
    #[serde(default)]
    asset_lock: Option<AssetLockManifest>,
    #[serde(default)]
    asset_locking: AssetLockingMode,
    width: u32,
    height: u32,
    text_layouts: Vec<TextLayoutReport>,
    artifact: ArtifactDescriptor,
}

impl<'de> Deserialize<'de> for ProjectFrameJobResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ProjectFrameJobResultWire::deserialize(deserializer)?;
        let result = Self {
            revision: wire.revision,
            sequence_id: wire.sequence_id,
            requested_time: wire.requested_time,
            actual_sequence_time: wire.actual_sequence_time,
            alignment: wire.alignment,
            quality: wire.quality,
            asset_lock: wire.asset_lock,
            asset_locking: wire.asset_locking,
            width: wire.width,
            height: wire.height,
            text_layouts: wire.text_layouts,
            artifact: wire.artifact,
        };
        result.validate().map_err(serde::de::Error::custom)?;
        Ok(result)
    }
}

#[derive(Deserialize)]
struct CompositedProjectFrameJobResultWire {
    revision: String,
    sequence_id: String,
    requested_time: Time,
    actual_sequence_time: Time,
    alignment: FrameAlignment,
    quality: CompositedProjectFrameQuality,
    #[serde(default)]
    asset_lock: Option<AssetLockManifest>,
    #[serde(default)]
    asset_locking: AssetLockingMode,
    width: u32,
    height: u32,
    text_layouts: Vec<TextLayoutReport>,
    artifact: ArtifactDescriptor,
}

impl<'de> Deserialize<'de> for CompositedProjectFrameJobResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = CompositedProjectFrameJobResultWire::deserialize(deserializer)?;
        let result = Self {
            revision: wire.revision,
            sequence_id: wire.sequence_id,
            requested_time: wire.requested_time,
            actual_sequence_time: wire.actual_sequence_time,
            alignment: wire.alignment,
            quality: wire.quality,
            asset_lock: wire.asset_lock,
            asset_locking: wire.asset_locking,
            width: wire.width,
            height: wire.height,
            text_layouts: wire.text_layouts,
            artifact: wire.artifact,
        };
        result.validate().map_err(serde::de::Error::custom)?;
        Ok(result)
    }
}

#[derive(Deserialize)]
struct ProjectRangeJobResultWire {
    revision: String,
    sequence_id: String,
    requested_range: TimeRange,
    actual_sequence_range: TimeRange,
    alignment: FrameAlignment,
    quality: ProjectRangeQuality,
    #[serde(default)]
    asset_lock: Option<AssetLockManifest>,
    #[serde(default)]
    asset_locking: AssetLockingMode,
    media: ExportResult,
    artifact: ArtifactDescriptor,
}

impl<'de> Deserialize<'de> for ProjectRangeJobResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ProjectRangeJobResultWire::deserialize(deserializer)?;
        let result = Self {
            revision: wire.revision,
            sequence_id: wire.sequence_id,
            requested_range: wire.requested_range,
            actual_sequence_range: wire.actual_sequence_range,
            alignment: wire.alignment,
            quality: wire.quality,
            asset_lock: wire.asset_lock,
            asset_locking: wire.asset_locking,
            media: wire.media,
            artifact: wire.artifact,
        };
        result.validate().map_err(serde::de::Error::custom)?;
        Ok(result)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobKindDescriptor {
    pub name: String,
    pub input_schema: Value,
    pub result_schema: Value,
}

mod sealed {
    pub trait Sealed {}
}
pub trait JobKind: sealed::Sealed {
    type Input: Serialize + serde::de::DeserializeOwned + JsonSchema;
    type Result: Serialize + serde::de::DeserializeOwned + JsonSchema;
    const NAME: &'static str;
}
macro_rules! job_kinds {
    ($( $marker:ident: ($input:ty, $result:ty, $name:literal ) ),* $(,)?) => {
        pub mod kinds {
            use super::*;
            $(pub struct $marker;
            impl sealed::Sealed for $marker {}
            impl JobKind for $marker {
                type Input = $input;
                type Result = $result;
                const NAME: &'static str = $name;
            })*
        }
        pub fn job_kind_descriptors() -> Vec<JobKindDescriptor> {
            vec![$(descriptor::<$input, $result>($name)),*]
        }
    }
}
pub mod source_preview;

job_kinds! {
    SourcePreview: (source_preview::SourcePreviewIdentity, source_preview::SourcePreviewResult, "media.preview_source"),
    MediaProbe: (ProbeRequest, MediaMetadata, "media.probe"),
    PreviewFrame: (FrameJobInput, MediaFrameJobResult, "preview.frame"),
    PreviewProjectFrame: (ProjectFrameInput, ProjectFrameJobResult, "preview.project_frame"),
    PreviewCompositedFrame: (ProjectFrameInput, CompositedProjectFrameJobResult, "preview.composited_frame"),
    PreviewProjectRange: (ProjectRangeInput, ProjectRangeJobResult, "preview.project_range"),
    RenderExport: (ExportJobInput, MediaExportJobResult, "render.export"),
    RenderProject: (ProjectExportInput, ProjectRenderJobResult, "render.project"),
    ArtifactVerify: (VerifyRequest, VerificationReport, "artifact.verify"),
    MediaImport: (MediaImportInput, MediaImportJobResult, "media.import"),
    AnalysisTranscribe: (TranscribeJobInput, TranscribeJobResult, "analysis.transcribe"),
    AnalysisSequenceTranscribe: (SceneTranscriptionRequest, SequenceTranscribeResult, "analysis.sequence_transcribe"),
    SceneTranscribe: (scene_transcription::SceneTranscriptionRequest, scene_transcription::SceneTranscriptionResult, "analysis.sequence_transcribe.v4"),
    AnalysisSilence: (SilenceJobInput, SilenceJobResult, "analysis.silence"),
    AnalysisShots: (ShotsJobInput, ShotsJobResult, "analysis.shots"),
    AnalysisWaveform: (WaveformJobInput, WaveformJobResult, "analysis.waveform"),
    AnalysisSpeakerTurns: (SpeakerTurnsJobInput, SpeakerTurnsJobResult, "analysis.speaker_turns"),
    RenderProjectV3: (historical_visual::HistoricalVisualSubmitRender, ProjectRenderJobResult, "render.project.v3"),
    PreviewProjectFrameV3: (historical_visual::HistoricalVisualSubmitFrame, CompositedProjectFrameJobResult, "preview.project_frame.v3"),
    PreviewProjectRangeV3: (historical_visual::HistoricalVisualSubmitRange, ProjectRangeJobResult, "preview.project_range.v3"),
    RenderProjectAudioV4: (scene_audio::SubmitAudioWav, scene_audio::AudioWavResult, "render.project_audio.v4"),
    RenderProjectAudioRangeV4: (scene_audio::SubmitAudioRangeWav, scene_audio::AudioRangeWavResult, "render.project_audio_range.v4"),
    RenderProjectTextFrameV4: (scene_text_frame::FrameRenderInput, scene_text_frame::TextFramePngResult, "render.project_text_frame.v4"),
    RenderProjectVideoV4: (media_video::FixedVideoInput, media_video::MediaVideoMp4Result, "render.project_video.v4"),
    RenderProjectCaptionVideoV4: (scene_caption_video::FixedVideoInput, scene_caption_video::CaptionVideoMp4Result, "render.project_caption_video.v4"),
    RenderProjectGraphicVideoV4: (scene_graphic_video::FixedVideoInput, scene_graphic_video::GraphicVideoMp4Result, "render.project_graphic_video.v4"),
    RenderProjectMaskVideoV4: (scene_mask_video::FixedVideoInput, scene_mask_video::MaskVideoMp4Result, "render.project_mask_video.v4"),
    RenderProjectBlendVideoV4: (scene_blend_video::FixedVideoInput, scene_blend_video::BlendVideoMp4Result, "render.project_blend_video.v4"),
    RenderProjectGraphicFrameV4: (scene_graphic_frame::FrameRenderInput, scene_graphic_frame::GraphicFramePngResult, "render.project_graphic_frame.v4"),
    RenderProjectSceneFrameV4: (scene_frame::FrameRenderInput, scene_frame::SceneFramePngResult, "render.project_scene_frame.v4"),
    RenderProjectSceneVideoV4: (scene_video::FixedVideoInput, scene_video::SceneVideoMp4Result, "render.project_scene_video.v4"),
    AssetAnalysis: (asset_analysis::SubmitAssetAnalysis, asset_analysis::AssetAnalysisResult, "analysis.asset.v4"),
    ExportVideoRangeV1: (export_video::SubmitExportVideoRange, export_video::ExportVideoRangeResult, "render.export_video_range.v1"),
    ExportVideoV2: (export_video::SubmitExportVideo, export_video::ExportVideoResult, "render.export_video.v2"),
    RenderProjectMaskFrameV4: (scene_mask_frame::FrameRenderInput, scene_mask_frame::MaskFramePngResult, "render.project_mask_frame.v4"),
    RenderProjectBlendFrameV4: (scene_blend_frame::FrameRenderInput, scene_blend_frame::BlendFramePngResult, "render.project_blend_frame.v4"),
    RenderProjectCaptionFrameV4: (scene_caption_frame::FrameRenderInput, scene_caption_frame::CaptionFramePngResult, "render.project_caption_frame.v4"),
    RenderProjectFrameV4: (media_frame::FrameRenderInput, media_frame::MediaFramePngResult, "render.project_frame.v4"),
}

fn descriptor<I: JsonSchema, O: JsonSchema>(name: &str) -> JobKindDescriptor {
    JobKindDescriptor {
        name: name.into(),
        input_schema: serde_json::to_value(schema_for!(I)).expect("JSON Schema is serializable"),
        result_schema: serde_json::to_value(schema_for!(O)).expect("JSON Schema is serializable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn transcription_request_exposes_only_project_asset_and_bounded_options() {
        let request = json!({
            "idempotency_key":"analysis-1", "project_id":"project-1",
            "revision":"rev-1", "asset_id":"asset-1", "language":"en"
        });
        assert!(serde_json::from_value::<TranscribeJobInput>(request.clone()).is_ok());
        for injected in ["input", "path", "executable", "model", "ffmpeg"] {
            let mut attempt = request.clone();
            attempt[injected] = json!("/private/tool");
            assert!(serde_json::from_value::<TranscribeJobInput>(attempt).is_err());
        }
        let old = serde_json::to_string(&schemars::schema_for!(JobStartInput)).unwrap();
        assert!(!old.contains("analysis.transcribe"));
        assert!(
            job_kind_descriptors()
                .iter()
                .any(|kind| kind.name == "analysis.transcribe")
        );
    }

    fn artifact_json() -> Value {
        json!({
            "path": "/tmp/frame.png", "mime_type": "image/png", "size": 123,
            "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        })
    }

    fn metadata_json() -> Value {
        json!({
            "input": "/tmp/render.mp4", "format_name": "mov,mp4", "duration": null,
            "bit_rate": null, "streams": []
        })
    }

    fn verification_json() -> Value {
        json!({"passed": true, "checks": [], "metadata": metadata_json(), "decoded_samples": []})
    }

    fn export_result_json() -> Value {
        json!({
            "output": "/tmp/render.mp4", "metadata": metadata_json(),
            "verification": verification_json(), "text_layouts": []
        })
    }

    fn lock_json() -> Value {
        json!({
            "version": 1, "revision": "rev-1", "sequence_id": "sequence-1",
            "assets": [{"asset_id": "asset-1", "content_hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "size": 44}]
        })
    }

    #[test]
    fn results_ignore_new_fields_while_validating_known_content() {
        let mut result = project_render_fixture();
        result["future_result_field"] = json!({"version": 2});
        result["artifact"]["future_artifact_field"] = json!(true);
        let decoded: ProjectRenderJobResult = serde_json::from_value(result).unwrap();
        assert_eq!(decoded.revision, "rev-1");
        assert_eq!(decoded.asset_locking, AssetLockingMode::MediaContentLocked);
        assert_eq!(decoded.artifact.size, 123);
    }

    fn project_render_fixture() -> Value {
        json!({
            "revision": "rev-1", "asset_lock": lock_json(),
            "asset_locking": "media_content_locked", "media": export_result_json(),
            "artifact": artifact_json()
        })
    }

    #[test]
    fn historical_success_results_decode_to_typed_values() {
        let render: ProjectRenderJobResult =
            serde_json::from_value(project_render_fixture()).unwrap();
        assert_eq!(render.asset_locking, AssetLockingMode::MediaContentLocked);
        assert_eq!(render.asset_lock.unwrap().assets[0].size, 44);

        let frame_fixture = json!({
            "revision": "rev-1", "sequence_id": "sequence-1",
            "requested_time": {"ticks": "13", "timebase": 24},
            "actual_sequence_time": {"ticks": "14", "timebase": 24},
            "alignment": "nearest", "quality": "encoded_project_frame",
            "width": 1920, "height": 1080, "text_layouts": [], "artifact": artifact_json()
        });
        let frame: ProjectFrameJobResult = serde_json::from_value(frame_fixture).unwrap();
        assert_eq!(frame.quality, ProjectFrameQuality::EncodedProjectFrame);
        assert_eq!(frame.asset_locking, AssetLockingMode::LegacyUnlocked);
        assert_eq!(frame.actual_sequence_time.ticks, "14");

        let mut composited_fixture = json!({
            "revision": "rev-1", "sequence_id": "sequence-1",
            "requested_time": {"ticks": "13", "timebase": 24},
            "actual_sequence_time": {"ticks": "13", "timebase": 24},
            "alignment": "nearest", "quality": "composited_project_frame",
            "width": 1920, "height": 1080, "text_layouts": [], "artifact": artifact_json()
        });
        composited_fixture["future_field"] = json!(true);
        let composited: CompositedProjectFrameJobResult =
            serde_json::from_value(composited_fixture).unwrap();
        assert_eq!(
            composited.quality,
            CompositedProjectFrameQuality::CompositedProjectFrame
        );
        assert_eq!(composited.asset_locking, AssetLockingMode::LegacyUnlocked);

        let mut range_fixture = json!({
            "revision": "rev-1", "sequence_id": "sequence-1",
            "requested_range": {
                "start": {"ticks": "24", "timebase": 24},
                "duration": {"ticks": "48", "timebase": 24}
            },
            "actual_sequence_range": {
                "start": {"ticks": "24", "timebase": 24},
                "duration": {"ticks": "48", "timebase": 24}
            },
            "alignment": "exact", "quality": "encoded_project_range",
            "media": export_result_json(), "artifact": artifact_json()
        });
        range_fixture["future_field"] = json!({"version": 2});
        let range: ProjectRangeJobResult = serde_json::from_value(range_fixture).unwrap();
        assert_eq!(range.quality, ProjectRangeQuality::EncodedProjectRange);
        assert_eq!(range.asset_locking, AssetLockingMode::LegacyUnlocked);
    }

    #[test]
    fn locked_result_missing_asset_locking_is_rejected() {
        let mut fixture = project_render_fixture();
        fixture.as_object_mut().unwrap().remove("asset_locking");
        assert!(serde_json::from_value::<ProjectRenderJobResult>(fixture).is_err());
    }

    #[test]
    fn historical_result_without_lock_fields_is_conservatively_legacy_unlocked() {
        let mut fixture = project_render_fixture();
        fixture.as_object_mut().unwrap().remove("asset_lock");
        fixture.as_object_mut().unwrap().remove("asset_locking");
        let result: ProjectRenderJobResult = serde_json::from_value(fixture).unwrap();
        assert_eq!(result.asset_lock, None);
        assert_eq!(result.asset_locking, AssetLockingMode::LegacyUnlocked);
        let serialized = serde_json::to_value(result).unwrap();
        assert_eq!(serialized["asset_lock"], Value::Null);
        assert_eq!(serialized["asset_locking"], "legacy_unlocked");
    }

    #[test]
    fn claimed_lock_without_manifest_is_rejected() {
        let mut fixture = project_render_fixture();
        fixture.as_object_mut().unwrap().remove("asset_lock");
        assert!(serde_json::from_value::<ProjectRenderJobResult>(fixture).is_err());
    }

    #[test]
    fn locked_results_reject_mismatched_revision_and_sequence() {
        let mut render = project_render_fixture();
        render["asset_lock"]["revision"] = json!("other-revision");
        assert!(serde_json::from_value::<ProjectRenderJobResult>(render).is_err());

        let frame = json!({
            "revision": "rev-1", "sequence_id": "sequence-1",
            "requested_time": {"ticks": "0", "timebase": 24},
            "actual_sequence_time": {"ticks": "0", "timebase": 24},
            "alignment": "exact", "quality": "encoded_project_frame",
            "asset_lock": {
                "version": 1, "revision": "rev-1", "sequence_id": "other-sequence",
                "assets": []
            },
            "asset_locking": "media_content_locked", "width": 1920, "height": 1080,
            "text_layouts": [], "artifact": artifact_json()
        });
        assert!(serde_json::from_value::<ProjectFrameJobResult>(frame).is_err());

        let composited = json!({
            "revision": "rev-1", "sequence_id": "sequence-1",
            "requested_time": {"ticks": "0", "timebase": 24},
            "actual_sequence_time": {"ticks": "0", "timebase": 24},
            "alignment": "exact", "quality": "composited_project_frame",
            "asset_lock": {
                "version": 1, "revision": "other-revision", "sequence_id": "sequence-1",
                "assets": []
            },
            "asset_locking": "media_content_locked", "width": 1920, "height": 1080,
            "text_layouts": [], "artifact": artifact_json()
        });
        assert!(serde_json::from_value::<CompositedProjectFrameJobResult>(composited).is_err());
    }

    #[test]
    fn schemas_keep_exact_time_and_quality_contracts() {
        let schema = serde_json::to_value(schema_for!(ProjectFrameJobResult)).unwrap();
        let encoded = serde_json::to_string(&schema).unwrap();
        assert!(encoded.contains("encoded_project_frame"));
        let time = &schema["$defs"]["Time"]["properties"]["ticks"];
        assert_eq!(time["type"], "string");

        let descriptors = job_kind_descriptors();
        assert_eq!(descriptors.len(), 37);
        let audio_range = descriptors
            .iter()
            .find(|d| d.name == scene_audio::AUDIO_RANGE_WAV_KIND)
            .unwrap();
        assert_eq!(
            audio_range.input_schema["$defs"]["AudioSampleRange"]["properties"]["start_sample"]["type"],
            "string"
        );
        assert!(
            audio_range
                .result_schema
                .to_string()
                .contains("whole_scene_frame_count")
        );
        let classic_frame = descriptors
            .iter()
            .find(|d| d.name == media_frame::MEDIA_FRAME_PNG_KIND)
            .unwrap();
        assert!(
            classic_frame
                .result_schema
                .to_string()
                .contains("classic-v31-gpu-normal-frame")
        );
        assert_eq!(
            classic_frame.input_schema["$defs"]["Time"]["properties"]["ticks"]["type"],
            "string"
        );
        assert_eq!(descriptors[2].name, "preview.project_frame");
        assert!(
            descriptors[2]
                .result_schema
                .to_string()
                .contains("encoded_project_frame")
        );
        assert_eq!(descriptors[3].name, "preview.composited_frame");
        assert!(
            descriptors[3]
                .result_schema
                .to_string()
                .contains("composited_project_frame")
        );
        assert!(
            !descriptors[3]
                .result_schema
                .to_string()
                .contains("encoded_project_frame")
        );
        assert_eq!(descriptors[4].name, "preview.project_range");
        assert!(
            descriptors[4]
                .result_schema
                .to_string()
                .contains("encoded_project_range")
        );
    }
}

/// Public import intent. Only the upload authority can prepare worker input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaImportInput {
    pub upload_id: String,
}
/// A project resource, not a global CAS capability or a host filesystem location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MediaImportJobResult {
    pub media_id: String,
    pub project_id: String,
    pub upload_id: String,
    pub job_id: String,
    pub asset: opencut_contracts::Asset,
}

pub mod scene_caption_frame;
pub mod scene_graphic_frame;

pub mod scene_video;
