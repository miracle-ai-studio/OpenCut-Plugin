//! Private source facts only. No project, path, credentials or output authority.
use opencut_contracts::{EditorError, source_preview::*};
use opencut_editor_application::source_preview::{
    SourcePreviewPolicy, validate_policy, validate_raster,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub identity: SourcePreviewIdentity,
    pub decoder: SourcePreviewDecoderIdentity,
    pub policy: SourcePreviewPolicy,
}
impl Plan {
    pub fn validate(&self) -> Result<(), EditorError> {
        validate_policy(&self.policy)?;
        validate_raster(self.identity.raster_bound)?;
        if self.identity.source_version.size == 0
            || self.identity.source_version.size > (1u64 << 53) - 1
            || self.identity.selected_video_stream.is_some_and(|n| n > 63)
            || self.decoder.worker_sha256.len() != 64
            || self.decoder.decoder_libraries.len() != 5
        {
            return Err(super::protocol("Invalid captured source decoder plan"));
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Track {
    pub stream_index: u32,
    pub track_id: i32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Frame {
    pub ordinal: u64,
    pub pts: i64,
    pub duration: i64,
    pub time_base: SourcePreviewTimeBase,
    pub video: SourcePreviewFrameVideo,
}
