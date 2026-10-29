pub use opencut_api_contracts::visual_preview::{
    ClassicVisualLayerPlan, ClassicVisualPlan, VisualDecodeAdmission, VisualDecodeRequest,
    VisualResource, VisualResourceKind,
};
use opencut_composition::scene::{BoundVisualFrame, DecodedVisualSource, VisualFrameIdentity};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicVisualResourceHandle {
    pub asset_id: String,
    pub handle: String,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "op", content = "args", deny_unknown_fields)]
pub enum VisualCallback {
    #[serde(rename = "resolveVerifiedBytes")]
    Resolve(VisualResolveBytes),
    #[serde(rename = "readVerifiedBytes")]
    Read(VisualReadBytes),
    #[serde(rename = "releaseVerifiedBytes")]
    Release(VisualReleaseBytes),
    #[serde(rename = "admitDecode")]
    Admit(VisualAdmitDecode),
    #[serde(rename = "resolvePreviewSource")]
    PreviewSource(VisualPreviewSource),
    #[serde(rename = "bindFrame")]
    Bind(VisualBindFrame),
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualResolveBytes {
    pub resource: VisualResource,
    pub handle: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualReadBytes {
    pub transfer_id: String,
    pub offset: String,
    pub count: u32,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualReleaseBytes {
    pub transfer_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualAdmitDecode {
    pub element_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualPreviewSource {
    pub element_id: String,
}

#[derive(Clone, Debug)]
pub struct VisualPreviewSourceRequest {
    pub project_id: String,
    pub revision: String,
    pub element_id: String,
    pub asset_id: String,
    pub source_version: opencut_contracts::AssetVersion,
    pub selected_video_stream: Option<u32>,
    pub requested_time: opencut_contracts::Time,
    pub raster_bound: opencut_contracts::source_preview::SourcePreviewRaster,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualPreviewSourceResolution {
    pub source_preparation: opencut_contracts::source_preview::SourcePreviewUse,
    pub transfer_id: Option<String>,
    pub byte_length: Option<String>,
}
impl VisualPreviewSourceResolution {
    pub fn original(
        request: &VisualPreviewSourceRequest,
        state: opencut_contracts::source_preview::SourcePreviewState,
        job_id: Option<String>,
    ) -> Self {
        Self {
            source_preparation: opencut_contracts::source_preview::SourcePreviewUse::Original {
                element_id: request.element_id.clone(),
                asset_id: request.asset_id.clone(),
                state,
                job_id,
            },
            transfer_id: None,
            byte_length: None,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualBindFrame {
    pub decoded: Vec<DecodedVisualSource>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassicVisualRenderReceipt {
    pub identity: VisualFrameIdentity,
    pub engine_identity: String,
    pub bound: BoundVisualFrame,
    pub rgba_bytes: String,
    pub peak_accounted_bytes: String,
}
pub use opencut_contracts::renderer_identity::NativeVisualIdentity as ClassicVisualEngineIdentity;
#[derive(Clone, Debug)]
pub struct ClassicVisualRuntimeReceipt {
    pub worker_pid: u32,
    pub identity: ClassicVisualEngineIdentity,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualQuotaReceipt {
    pub profile: String,
    pub max_rss_bytes: String,
    pub observed_peak_rss_bytes: String,
    pub monitor_interval_ms: u32,
    pub elapsed_ms: u64,
    pub hard_memory_enforced: bool,
}
#[derive(Debug)]
pub struct ClassicVisualExecutionReceipt {
    pub receipt: ClassicVisualRenderReceipt,
    pub quota: VisualQuotaReceipt,
    pub runtime_identity: ClassicVisualRuntimeReceipt,
}
