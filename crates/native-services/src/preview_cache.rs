//! Trusted installed-runtime fingerprint for derived preview cache admission.
//! No media, native paths or executable selectors cross the public boundary.
use crate::classic_visual::ClassicVisualProcessConfig;
use opencut_contracts::EditorError;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};
type Stamp = (PathBuf, u64, SystemTime);
type CachedIdentity = (Vec<Stamp>, Vec<u8>, String);
static LAST: OnceLock<Mutex<Option<CachedIdentity>>> = OnceLock::new();
fn failure() -> EditorError {
    EditorError::new(
        "PREVIEW_RUNTIME_UNAVAILABLE",
        "Cannot identify the installed preview renderer",
    )
}
pub fn fingerprint(
    config: &ClassicVisualProcessConfig,
    ffmpeg: &Path,
    ffprobe: &Path,
) -> Result<String, EditorError> {
    let platform =
        serde_json::to_vec(&crate::native_visual::platform_identity()?).map_err(|_| failure())?;
    let mut paths = vec![
        config.worker.clone(),
        std::env::current_exe().map_err(|_| failure())?,
        ffmpeg.into(),
        ffprobe.into(),
    ];
    paths.extend(
        opencut_media::native_decode::identity::libraries()
            .map_err(|_| failure())?
            .into_iter()
            .map(|library| library.path),
    );
    paths.sort();
    paths.dedup();
    let stamps = paths
        .iter()
        .map(|path| {
            let stat = fs::metadata(path).map_err(|_| failure())?;
            Ok((
                path.clone(),
                stat.len(),
                stat.modified().map_err(|_| failure())?,
            ))
        })
        .collect::<Result<Vec<_>, EditorError>>()?;
    let mut last = LAST
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| failure())?;
    if let Some((previous, previous_platform, key)) = &*last {
        if previous == &stamps && previous_platform == &platform {
            return Ok(key.clone());
        }
    }
    let mut hash = Sha256::new();
    hash.update(b"opencut-native-preview-runtime\0");
    hash.update(&platform);
    hash.update([0]);
    let mut buffer = [0u8; 65536];
    for path in paths {
        // File names/order participate as well as actual content. Absolute paths
        // are private and affect reuse only, never source timeline semantics.
        hash.update(path.to_string_lossy().as_bytes());
        hash.update([0]);
        let mut file = File::open(path).map_err(|_| failure())?;
        loop {
            let count = file.read(&mut buffer).map_err(|_| failure())?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        hash.update([0]);
    }
    let key = format!("{:x}", hash.finalize());
    *last = Some((stamps, platform, key.clone()));
    Ok(key)
}
