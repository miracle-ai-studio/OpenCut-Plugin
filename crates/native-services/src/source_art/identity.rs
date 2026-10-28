use super::*;
use crate::native_process;
use sha2::{Digest, Sha256};
pub(super) fn module_hash() -> String {
    let mut hash = Sha256::new();
    for (name, bytes) in [
        ("protocol", include_bytes!("protocol.rs").as_slice()),
        ("worker", include_bytes!("worker.rs").as_slice()),
        ("waveform", include_bytes!("streaming.rs").as_slice()),
        (
            "resample",
            include_bytes!("../native_audio/dsp/resample.rs").as_slice(),
        ),
        (
            "thumbnail",
            include_bytes!("../../../renderer/src/raster/thumbnail.rs").as_slice(),
        ),
    ] {
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}
pub fn resolve_identity(worker: &Path) -> Result<SourceArtIdentity, EditorError> {
    trusted_file(worker)?;
    let value = SourceArtIdentity::Native(NativeSourceArtIdentity {
        algorithm: SOURCE_PEAKS_ALGORITHM.into(),
        decode_profile: NATIVE_SOURCE_ART_DECODE_PROFILE.into(),
        worker_sha256: native_process::file_hash(worker)?,
        module_sha256: module_hash(),
        decoder_libraries: native_process::library_identities()?,
        rasterizer: opencut_renderer::raster::SOURCE_THUMBNAIL_RASTERIZER.into(),
        platform: native_process::system_identity()?,
    });
    validate_identity(&value)?;
    Ok(value)
}
/// Cheap dependency-stat fence before cache admission. Hash the actual files
/// only when these facts change or the host's bounded capture TTL expires.
pub fn runtime_fingerprint(worker: &Path) -> Result<String, EditorError> {
    trusted_file(worker)?;
    let libraries = opencut_media::native_decode::identity::libraries()
        .map_err(crate::media::execution_error)?;
    let mut paths = vec![worker.to_owned()];
    paths.extend(libraries.into_iter().map(|l| l.path));
    paths.sort();
    paths.dedup();
    let mut hash = Sha256::new();
    hash.update(module_hash().as_bytes());
    for path in paths {
        let meta = std::fs::metadata(&path)
            .map_err(|_| protocol("Cannot observe native source runtime"))?;
        hash.update(path.as_os_str().as_encoded_bytes());
        hash.update(meta.len().to_le_bytes());
        hash.update(
            meta.modified()
                .map_err(|_| protocol("Cannot observe source runtime modification"))?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| protocol("Invalid runtime modification time"))?
                .as_nanos()
                .to_le_bytes(),
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            hash.update(meta.ino().to_le_bytes());
            hash.update(meta.ctime().to_le_bytes());
            hash.update(meta.ctime_nsec().to_le_bytes());
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}
