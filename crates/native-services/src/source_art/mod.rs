//! Isolated, bounded fixed-CAS source decoding. Cache and projection live above
//! this adapter; no authoring duration or gain participates in source peaks.
use opencut_contracts::{EditorError, source_art::*};
use opencut_editor_application::source_art::*;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
mod identity;
mod process;
mod protocol;
mod streaming;
#[cfg(test)]
mod tests;
pub(crate) mod worker;
pub use identity::{resolve_identity, runtime_fingerprint};

#[derive(Clone)]
pub struct SourceArtProcessConfig {
    pub worker: PathBuf,
    pub strict_cgroup: Option<PathBuf>,
    pub deadline: Duration,
    pub max_encoded_bytes: u64,
    pub max_pcm_bytes: u64,
    pub max_rss_bytes: u64,
    pub identity: SourceArtIdentity,
}
pub struct SourceArtProcessExecutor {
    config: SourceArtProcessConfig,
    asset_root: PathBuf,
}
fn error(code: &str, message: &str) -> EditorError {
    EditorError::new(code, message)
}
fn protocol(message: &str) -> EditorError {
    error("SOURCE_ART_PROTOCOL_ERROR", message)
}
fn trusted_file(path: &Path) -> Result<(), EditorError> {
    if !path.is_absolute() || !path.is_file() {
        return Err(error(
            "CAPABILITY_UNAVAILABLE",
            "Source-art runtime file is unavailable",
        ));
    }
    Ok(())
}
fn validate_identity(identity: &SourceArtIdentity) -> Result<(), EditorError> {
    validate_runtime_identity(identity)?;
    if !matches!(identity, SourceArtIdentity::Native(_)) {
        return Err(protocol(
            "New source-art execution requires the configured native runtime",
        ));
    }
    Ok(())
}
impl SourceArtProcessExecutor {
    pub fn new(config: SourceArtProcessConfig, asset_root: PathBuf) -> Result<Self, EditorError> {
        trusted_file(&config.worker)?;
        validate_identity(&config.identity)?;
        let policy = SourceArtPolicy {
            max_encoded_bytes: config.max_encoded_bytes,
            max_pcm_bytes: config.max_pcm_bytes,
            max_rss_bytes: config.max_rss_bytes,
            deadline_ms: u64::try_from(config.deadline.as_millis())
                .map_err(|_| protocol("Invalid source-art deadline"))?,
        };
        validate_policy(&policy)?;
        if config.deadline.as_millis() != u128::from(policy.deadline_ms)
            || config.deadline != Duration::from_millis(policy.deadline_ms)
        {
            return Err(protocol("Source-art deadline must be whole milliseconds"));
        }
        Ok(Self { config, asset_root })
    }
}
impl SourceArtExecutor for SourceArtProcessExecutor {
    fn execute(
        &self,
        prepared: &PreparedSourceArt,
        checkpoint: &mut dyn SourceArtCheckpoint,
    ) -> Result<DecodedSourceArt, EditorError> {
        #[cfg(unix)]
        {
            process::execute(self, prepared, checkpoint)
        }
        #[cfg(not(unix))]
        {
            let _ = (prepared, checkpoint);
            Err(error(
                "CAPABILITY_UNAVAILABLE",
                "Source-art process isolation requires Unix",
            ))
        }
    }
}
