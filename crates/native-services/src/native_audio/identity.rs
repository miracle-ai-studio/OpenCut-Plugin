//! Actual CPU/DSP/libav facts; no browser, font or graphics identity is invented.
use super::*;
use crate::native_process;
use opencut_contracts::renderer_identity::NativeAudioIdentity;
use opencut_editor_application::classic_audio::{
    NATIVE_AUDIO_DECODER, NATIVE_AUDIO_DSP, SOUNDTOUCH_SOURCE_SHA256,
};
use sha2::{Digest, Sha256};
pub(crate) fn dsp_hash() -> String {
    let mut digest = Sha256::new();
    for (name, bytes) in [
        ("compressor", include_bytes!("dsp/compressor.rs").as_slice()),
        ("resample", include_bytes!("dsp/resample.rs").as_slice()),
        ("soundtouch", include_bytes!("dsp/soundtouch.rs").as_slice()),
        (
            "sources",
            include_bytes!("../../vendor/audio/source.json").as_slice(),
        ),
    ] {
        digest.update(name.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn observe() -> Result<NativeAudioIdentity, EditorError> {
    Ok(NativeAudioIdentity {
        worker_sha256: native_process::file_hash(
            &std::env::current_exe().map_err(|_| invalid("Cannot identify native audio worker"))?,
        )?,
        decoder_libraries: native_process::library_identities()?,
        decoder_profile: NATIVE_AUDIO_DECODER.into(),
        dsp_profile: NATIVE_AUDIO_DSP.into(),
        dsp_sha256: dsp_hash(),
        soundtouch_source_sha256: SOUNDTOUCH_SOURCE_SHA256.into(),
        platform: native_process::system_identity()?,
    })
}
