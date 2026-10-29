//! Native source-only preparation. Application ports own fixed source facts,
//! current authority and publication; these adapters own bounded bytes/codecs.
mod index;
mod process;
mod protocol;
pub(crate) mod raster;
pub(crate) mod worker;
use crate::native_process;
pub use index::{SourcePreviewIndex, SourcePreviewIndexBuilder};
use opencut_contracts::{EditorError, source_preview::*};
pub use opencut_editor_application::source_preview::runtime_fingerprint;
use opencut_editor_application::source_preview::{
    PreparedSourcePreview, SourcePreviewCheckpoint, SourcePreviewExecutor,
    StoredSourcePreviewResult,
};
use opencut_media::OutputAdmission;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn decoder_identity(worker: &Path) -> Result<SourcePreviewDecoderIdentity, EditorError> {
    Ok(SourcePreviewDecoderIdentity {
        worker_sha256: native_process::file_hash(worker)?,
        decoder_libraries: native_process::library_identities()?,
        platform: native_process::system_identity()?,
    })
}
pub fn resolve_runtime(
    worker: &Path,
    ffmpeg: &Path,
    ffprobe: &Path,
) -> Result<SourcePreviewRuntimeIdentity, EditorError> {
    Ok(SourcePreviewRuntimeIdentity {
        host_sha256: native_process::file_hash(
            &std::env::current_exe()
                .map_err(|_| protocol("Cannot observe source preparation host"))?,
        )?,
        decoder: decoder_identity(worker)?,
        ffmpeg_sha256: native_process::file_hash(ffmpeg)?,
        ffprobe_sha256: native_process::file_hash(ffprobe)?,
    })
}
pub struct SourcePreviewProcessConfig {
    pub worker: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub strict_cgroup: Option<PathBuf>,
    pub asset_root: PathBuf,
    pub video_output: PathBuf,
    pub index_output: PathBuf,
    pub runtime: SourcePreviewRuntimeIdentity,
}
pub struct SourcePreviewProcessExecutor<'a> {
    pub config: SourcePreviewProcessConfig,
    pub admission: &'a dyn OutputAdmission,
}
impl SourcePreviewExecutor for SourcePreviewProcessExecutor<'_> {
    fn execute(
        &self,
        prepared: &PreparedSourcePreview,
        checkpoint: &mut dyn SourcePreviewCheckpoint,
    ) -> Result<StoredSourcePreviewResult, EditorError> {
        #[cfg(unix)]
        {
            process::execute(self, prepared, checkpoint)
        }
        #[cfg(not(unix))]
        {
            let _ = (prepared, checkpoint);
            Err(error(
                "CAPABILITY_UNAVAILABLE",
                "Source preparation process isolation requires Unix",
            ))
        }
    }
}
fn error(code: &str, message: &str) -> EditorError {
    EditorError::new(code, message)
}
fn protocol(message: &str) -> EditorError {
    error("SOURCE_PREVIEW_PROTOCOL_ERROR", message)
}

pub const INDEX_MAGIC: [u8; 8] = *b"OCSPIDX\0";
#[cfg(test)]
mod tests;

fn invalid(message: &str) -> opencut_contracts::EditorError {
    opencut_contracts::EditorError::new("INVALID_STORED_RESULT", message)
}
