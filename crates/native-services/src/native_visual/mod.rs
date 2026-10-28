//! Native execution of existing application-bound frame facts. Host assembly
//! owns process isolation and publication; this module owns no public command.
mod assemble;
mod engine;
mod identity;
pub(crate) use identity::platform as platform_identity;
mod monitor;
mod process;
pub(crate) use process::NativeProcess;
mod wire;
mod worker;
pub use worker::serve;
mod measurement;
mod media;
mod plan;
mod preview;
#[cfg(test)]
mod tests;
mod text;
pub use engine::NativeFrameEngine;
pub use measurement::{NativeTextMeasureOptions, NativeTextMeasurement, NativeTextMeasurements};
pub use media::{MediaFrameSources, NativeMediaDecoders};
use opencut_contracts::EditorError;
pub use preview::{NativePreviewFrame, NativeScenePreview};

/// Native adapter observation. Public preview DTOs bind this to the original
/// session/frame identity; this value grants no resource authority.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeVisualHit {
    pub element_id: String,
    pub alpha: u8,
}

pub fn platform_backend() -> Result<opencut_renderer::Backend, EditorError> {
    if cfg!(target_os = "macos") {
        Ok(opencut_renderer::Backend::Metal)
    } else if cfg!(target_os = "linux") {
        Ok(opencut_renderer::Backend::Vulkan)
    } else {
        Err(EditorError::new(
            "CAPABILITY_UNAVAILABLE",
            "Native worker lifecycle is unavailable on this platform",
        ))
    }
}

fn invalid(message: impl Into<String>) -> EditorError {
    EditorError::new("INVALID_VISUAL_PLAN", message)
}
fn media_error(error: opencut_media::MediaError) -> EditorError {
    EditorError::new(error.code(), error.to_string())
}
fn number(value: &str) -> Result<u64, EditorError> {
    let number = value
        .parse::<u64>()
        .map_err(|_| invalid("Invalid native byte admission"))?;
    if number.to_string() != value {
        return Err(invalid("Noncanonical native byte admission"));
    }
    Ok(number)
}
