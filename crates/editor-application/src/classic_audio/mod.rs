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
/// Existing private PCM sink bound, independent of the binary source/fact
/// packet size. A larger transport packet never widens output admission.
pub const AUDIO_PCM_CHUNK_FRAMES: usize = 4096;
pub const NATIVE_AUDIO_DECODER: &str = "libav-authorized-audio";
pub const NATIVE_AUDIO_DSP: &str = "classic-audio-f32";
pub const SOUNDTOUCH_SOURCE_SHA256: &str =
    "5143b5e14251a1e2068c83a7851ffe4bee27a0628cb0e587e2d941e0f06bf9c0";
pub fn validate_native_identity(
    identity: &opencut_contracts::renderer_identity::NativeAudioIdentity,
) -> Result<(), EditorError> {
    fn hash(s: &str) -> bool {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
    fn text(s: &str) -> bool {
        !s.trim().is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control)
    }
    let mut names = std::collections::BTreeSet::new();
    if !hash(&identity.worker_sha256)
        || !hash(&identity.dsp_sha256)
        || identity.decoder_profile != NATIVE_AUDIO_DECODER
        || identity.dsp_profile != NATIVE_AUDIO_DSP
        || identity.soundtouch_source_sha256 != SOUNDTOUCH_SOURCE_SHA256
        || identity.decoder_libraries.len() != 5
        || [
            &identity.platform.os,
            &identity.platform.architecture,
            &identity.platform.kernel_release,
            &identity.platform.system_build,
        ]
        .iter()
        .any(|s| !text(s))
    {
        return Err(invalid("Invalid native audio execution identity"));
    }
    for lib in &identity.decoder_libraries {
        if !names.insert(lib.name.as_str())
            || !matches!(
                lib.name.as_str(),
                "avcodec" | "avformat" | "avutil" | "swscale" | "swresample"
            )
            || !text(&lib.version)
            || !hash(&lib.configuration_sha256)
            || !hash(&lib.binary_sha256)
        {
            return Err(invalid("Invalid native audio decoder library identity"));
        }
    }
    Ok(())
}

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
/// Chunks contain 1..=AUDIO_PCM_CHUNK_FRAMES finite frames, ordered per channel.
pub trait ClassicAudioPcmSink {
    fn chunk(&mut self, channel: u32, offset: u64, samples: &[f32]) -> Result<(), EditorError>;
}

fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_AUDIO_PLAN", message)
}
