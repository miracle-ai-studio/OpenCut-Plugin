//! The sole public task-result registry. Stored native kinds and payload
//! formats are private execution facts,never a client protocol selector.
pub use opencut_job_contracts::asset_analysis::{AssetAnalysisData, AssetAnalysisOptions};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;
mod receipts;
pub use receipts::*;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobResultDescriptor {
    pub name: String,
    pub result_schema: Value,
}
mod sealed {
    pub trait Sealed {}
}
pub trait JobResultKind: sealed::Sealed {
    type Result: Clone + Serialize + serde::de::DeserializeOwned + JsonSchema;
    const NAME: &'static str;
    fn result(view: &JobResultView) -> Option<&Self::Result>;
}
macro_rules! job_results {
    ($( $variant:ident:($result:ty,$name:literal)),* $(,)?)=>{
        #[derive(Debug,Clone,Serialize,Deserialize,JsonSchema)]
        #[serde(tag="kind",content="data",deny_unknown_fields)]
        pub enum JobResultView {$( #[serde(rename=$name)] $variant($result), )*}
        impl JobResultView {
            pub fn kind(&self)->&'static str {match self {$( Self::$variant(_)=>$name, )*}}
        }
        pub mod kinds {
            use super::*;
            $(pub struct $variant;
            impl sealed::Sealed for $variant {}
            impl JobResultKind for $variant {
                type Result=$result;
                const NAME:&'static str=$name;
                fn result(view:&JobResultView)->Option<&Self::Result>{
                    if let JobResultView::$variant(value)=view {Some(value)}else{None}
                }
            })*
        }
        pub fn descriptors()->Vec<JobResultDescriptor>{
            vec![$(JobResultDescriptor{name:$name.into(),result_schema:serde_json::to_value(schema_for!($result)).expect("shared public result schema")}),*]
        }
    }
}
job_results! {
    SourcePreview:(opencut_contracts::source_preview::SourcePreviewResult,"media.preview_source"),
    Probe:(crate::job_views::MediaInfo,"media.probe"),
    SourceFrame:(crate::job_views::SourceFrameView,"preview.frame"),
    ProjectFrame:(crate::job_views::ProjectFrameView,"preview.project_frame"),
    CompositedFrame:(crate::job_views::CompositedFrameView,"preview.composited_frame"),
    ProjectRange:(crate::job_views::ProjectRangeView,"preview.project_range"),
    Export:(crate::job_views::ExportView,"render.export"),
    ProjectRender:(crate::job_views::ProjectRenderView,"render.project"),
    Verification:(crate::job_views::VerificationView,"artifact.verify"),
    MediaImport:(opencut_job_contracts::MediaImportJobResult,"media.import"),
    AssetAnalysis:(AssetAnalysisResult,"analysis.asset"),
    SceneTranscription:(SceneTranscriptionResult,"analysis.sequence_transcribe"),
    Audio:(AudioRenderResult,"render.audio"),
    MediaFrame:(MediaFrameResult,"render.media_frame"),
    TextFrame:(TextFrameResult,"render.text_frame"),
    CaptionFrame:(CaptionFrameResult,"render.caption_frame"),
    GraphicFrame:(GraphicFrameResult,"render.graphic_frame"),
    MaskFrame:(MaskFrameResult,"render.mask_frame"),
    BlendFrame:(BlendFrameResult,"render.blend_frame"),
    SceneFrame:(SceneFrameResult,"render.frame"),
    MediaVideo:(MediaVideoResult,"render.media_video"),
    CaptionVideo:(CaptionVideoResult,"render.caption_video"),
    GraphicVideo:(GraphicVideoResult,"render.graphic_video"),
    MaskVideo:(MaskVideoResult,"render.mask_video"),
    BlendVideo:(BlendVideoResult,"render.blend_video"),
    SceneVideo:(SceneVideoResult,"render.scene_video"),
    VideoExport:(VideoExportResult,"render.video"),
    VideoRange:(VideoRangeResult,"render.video_range"),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetAnalysisResult {
    pub project_id: String,
    pub revision: String,
    pub asset_id: String,
    pub asset_version: opencut_contracts::LockedAsset,
    pub options: opencut_job_contracts::asset_analysis::AssetAnalysisOptions,
    pub data: opencut_job_contracts::asset_analysis::AssetAnalysisData,
}
impl From<opencut_job_contracts::asset_analysis::AssetAnalysisResult> for AssetAnalysisResult {
    fn from(value: opencut_job_contracts::asset_analysis::AssetAnalysisResult) -> Self {
        Self {
            project_id: value.project_id,
            revision: value.revision,
            asset_id: value.asset_id,
            asset_version: value.asset_version,
            options: value.options,
            data: value.data,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioRenderResult {
    pub identity: opencut_job_contracts::scene_audio::AudioRangeWavIdentity,
    pub asset_lock: opencut_contracts::AssetLockManifest,
    pub render_input_sha256: String,
    pub execution: opencut_job_contracts::scene_audio::AudioExecutionSummary,
    pub artifact: opencut_job_contracts::scene_audio::AudioArtifactMetadata,
}
impl From<opencut_job_contracts::scene_audio::AudioRangeWavResult> for AudioRenderResult {
    fn from(v: opencut_job_contracts::scene_audio::AudioRangeWavResult) -> Self {
        Self {
            identity: v.identity,
            asset_lock: v.asset_lock,
            render_input_sha256: v.render_input_sha256,
            execution: v.execution,
            artifact: v.artifact,
        }
    }
}
impl From<opencut_job_contracts::scene_audio::AudioWavResult> for AudioRenderResult {
    fn from(v: opencut_job_contracts::scene_audio::AudioWavResult) -> Self {
        let i = v.identity;
        Self {
            identity: opencut_job_contracts::scene_audio::AudioRangeWavIdentity {
                project_id: i.project_id,
                revision: i.revision,
                sequence_id: i.sequence_id,
                profile: i.profile,
                sample_rate: i.sample_rate,
                channels: i.channels,
                start_sample: "0".into(),
                whole_scene_frame_count: i.frame_count.clone(),
                frame_count: i.frame_count,
                sample_format: i.sample_format,
            },
            asset_lock: v.asset_lock,
            render_input_sha256: v.render_input_sha256,
            execution: v.execution,
            artifact: v.artifact,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneTranscriptionResult {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub duration: opencut_contracts::Time,
    pub asset_lock: opencut_contracts::AssetLockManifest,
    /// Historical FFmpeg mixes have no Classic PCM receipt. None is an absence of
    /// evidence,not a silent mix or a claim that mastering did not run.
    pub audio: Option<AudioRenderResult>,
    pub transcription: opencut_media_contracts::TranscriptionResult,
}
impl From<opencut_job_contracts::scene_transcription::SceneTranscriptionResult>
    for SceneTranscriptionResult
{
    fn from(v: opencut_job_contracts::scene_transcription::SceneTranscriptionResult) -> Self {
        Self {
            project_id: v.project_id,
            revision: v.revision,
            sequence_id: v.sequence_id,
            duration: v.duration,
            asset_lock: v.audio.asset_lock.clone(),
            audio: Some(v.audio.into()),
            transcription: v.transcription,
        }
    }
}
impl From<opencut_job_contracts::SequenceTranscribeResult> for SceneTranscriptionResult {
    fn from(v: opencut_job_contracts::SequenceTranscribeResult) -> Self {
        Self {
            project_id: v.project_id,
            revision: v.revision,
            sequence_id: v.sequence_id,
            duration: v.duration,
            asset_lock: v.asset_lock,
            audio: None,
            transcription: v.transcription,
        }
    }
}
