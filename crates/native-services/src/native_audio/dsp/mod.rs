//! One native implementation of the retained Classic sample operations.
mod compressor;
mod memory;
mod resample;
mod soundtouch;
pub use compressor::{ClassicCompressor, clip_export, mastering_required};
pub use memory::{PcmArray, PcmMemory, PcmReservation};
use opencut_contracts::EditorError;
pub use resample::{DecodeResampler, resample_preparation};
pub use soundtouch::{SoundTouch, render_preserved_pitch};
type Result<T> = std::result::Result<T, EditorError>;
fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_AUDIO_PLAN", message)
}
fn budget(message: &str) -> EditorError {
    EditorError::new("AUDIO_BUDGET_EXCEEDED", message)
}

#[cfg(test)]
mod tests;
