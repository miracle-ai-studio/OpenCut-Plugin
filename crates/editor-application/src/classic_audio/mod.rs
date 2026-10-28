//! Internal Classic browser execution ports. No public DTO, files or process I/O.
mod protocol;
mod receipt;
mod session;
mod wav_layout;
use opencut_contracts::EditorError;
pub use protocol::*;
pub use receipt::*;
pub use session::*;
pub use wav_layout::ClassicAudioWavLayout;

/// One codec declaration shared by PCM admission and stored-proof validation.
pub(crate) fn native_pcm_sample_bytes(codec: &str) -> Option<u64> {
    match codec {
        "pcm-s16" => Some(2),
        "pcm-f32" => Some(4),
        _ => None,
    }
}

/// A host holds verified CAS descriptors and the shared collection gate for the
/// entire execution. Every method must recheck cancellation and live policy.
pub trait ClassicAudioResources {
    fn resolve(
        &mut self,
        resource: &AudioResource,
        handle: &str,
    ) -> Result<(String, u64), EditorError>;
    fn read(
        &mut self,
        transfer: &str,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), EditorError>;
    fn release(&mut self, transfer: &str) -> Result<(), EditorError>;
    fn checkpoint(&mut self) -> Result<(), EditorError>;
}

/// PCM is private attempt output. A sink must not publish on a chunk or receipt;
/// publication follows actual process termination and the worker attempt fence.
pub trait ClassicAudioPcmSink {
    fn chunk(&mut self, channel: u32, offset: u64, samples: &[f32]) -> Result<(), EditorError>;
}

fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_AUDIO_PLAN", message)
}
