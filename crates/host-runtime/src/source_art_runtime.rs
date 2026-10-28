//! Trusted source-art installation discovery. Runtime identity is captured by
//! the native adapter and refreshed when any captured file changes or TTL ends.
use opencut_contracts::{
    EditorError,
    source_art::{SourceArtIdentity, SourceArtQuery},
};
use opencut_editor_application::source_art::{
    SOURCE_PEAKS_ALGORITHM, SourceArtPolicy, THUMBNAIL_ALGORITHM,
};
use opencut_native_services::source_art::{SourceArtProcessConfig, resolve_identity};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
struct Captured {
    fingerprint: String,
    identity: SourceArtIdentity,
    captured: Instant,
}
fn unavailable() -> EditorError {
    EditorError::new(
        "CAPABILITY_UNAVAILABLE",
        "Source-art decoder runtime is not configured or unavailable",
    )
}
pub(crate) fn configured(
    policy: &SourceArtPolicy,
    query: &SourceArtQuery,
) -> Result<SourceArtProcessConfig, EditorError> {
    opencut_editor_application::source_art::validate_policy(policy)?;
    let runtime = crate::classic_audio_runtime::source_art_dependencies()?;
    let audio = runtime.audio_package.as_path();
    let package = std::env::var_os("OPENCUT_SOURCE_ART_PACKAGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            audio
                .parent()
                .unwrap_or(audio)
                .join("classic-source-art-renderer")
        });
    let package = crate::browser_runtime::absolute(&package).map_err(|_| unavailable())?;
    let shared = package
        .parent()
        .ok_or_else(unavailable)?
        .join("classic-audio-renderer");
    if fs::canonicalize(shared).map_err(|_| unavailable())? != audio {
        return Err(unavailable());
    }
    let manifest = crate::browser_runtime::metadata(&package.join("package.json"))
        .map_err(|_| unavailable())?;
    if manifest["name"] != "@opencut/classic-source-art-renderer" || manifest["version"] != "0.1.0"
    {
        return Err(unavailable());
    }
    let runner = crate::browser_runtime::package_file(&package, "host/runner.mjs")
        .map_err(|_| unavailable())?;
    let fingerprint = fingerprint(&package, audio, &runtime.node, &runtime.chromium)?;
    static CAPTURED: OnceLock<Mutex<Option<Captured>>> = OnceLock::new();
    let mut captured = CAPTURED
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| unavailable())?;
    if !captured.as_ref().is_some_and(|value| {
        value.fingerprint == fingerprint && value.captured.elapsed() < Duration::from_secs(30)
    }) {
        let identity = resolve_identity(&runtime.node, &runner, &runtime.chromium)?;
        // A mutation during capture cannot authorize a previously cached value.
        if fingerprint != self::fingerprint(&package, audio, &runtime.node, &runtime.chromium)? {
            return Err(unavailable());
        }
        *captured = Some(Captured {
            fingerprint,
            identity,
            captured: Instant::now(),
        });
    }
    let mut identity = captured.as_ref().ok_or_else(unavailable)?.identity.clone();
    identity.algorithm = match query {
        SourceArtQuery::Waveform { .. } => SOURCE_PEAKS_ALGORITHM,
        SourceArtQuery::Thumbnail {} => THUMBNAIL_ALGORITHM,
    }
    .into();
    Ok(SourceArtProcessConfig {
        node: runtime.node,
        runner,
        chromium: runtime.chromium,
        deadline: Duration::from_millis(policy.deadline_ms),
        max_pcm_bytes: policy.max_pcm_bytes,
        max_encoded_bytes: policy.max_encoded_bytes,
        max_rss_bytes: policy.max_rss_bytes,
        identity,
    })
}
fn collect(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), EditorError> {
    if files.len() > 2048 {
        return Err(unavailable());
    }
    for entry in fs::read_dir(root).map_err(|_| unavailable())? {
        let path = entry.map_err(|_| unavailable())?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|e| e == "mjs" || e == "js" || e == "json")
        {
            files.push(path);
        }
    }
    Ok(())
}
fn fingerprint(
    package: &Path,
    audio: &Path,
    node: &Path,
    chromium: &Path,
) -> Result<String, EditorError> {
    let mut files = vec![node.into(), chromium.into(), package.join("package.json")];
    for root in [package.join("src"), package.join("host")] {
        collect(&root, &mut files)?;
    }
    for file in [
        "host/shared.mjs",
        "host/browser.mjs",
        "host/page.mjs",
        "host/runner.mjs",
        "host/modules.mjs",
        "host/limits.mjs",
        "host/protocol.mjs",
        "src/validation.mjs",
        "src/verified-range-source.mjs",
        "src/windowed-decoder.mjs",
        "src/runtime.mjs",
        "src/pcm-source.mjs",
    ] {
        files.push(audio.join(file));
    }
    let modules = audio.join("node_modules");
    for file in [
        "mediabunny/package.json",
        "mediabunny/dist/bundles/mediabunny.mjs",
        "playwright-core/package.json",
        "playwright-core/index.js",
        "playwright-core/index.mjs",
    ] {
        files.push(modules.join(file));
    }
    collect(&modules.join("playwright-core"), &mut files)?;
    files.sort();
    files.dedup();
    let mut digest = Sha256::new();
    for file in files {
        let canonical = fs::canonicalize(&file).map_err(|_| unavailable())?;
        let stat = fs::metadata(&canonical).map_err(|_| unavailable())?;
        if !stat.is_file() {
            return Err(unavailable());
        }
        digest.update(canonical.as_os_str().as_encoded_bytes());
        digest.update(stat.len().to_le_bytes());
        let modified = stat
            .modified()
            .map_err(|_| unavailable())?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| unavailable())?;
        digest.update(modified.as_nanos().to_le_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            digest.update(stat.ino().to_le_bytes());
            digest.update(stat.ctime().to_le_bytes());
            digest.update(stat.ctime_nsec().to_le_bytes());
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}
