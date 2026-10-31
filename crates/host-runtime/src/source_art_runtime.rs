//! Trusted native source-art installation and live dependency cache fencing.
use opencut_asset_store::verification::FileIdentity;
use opencut_contracts::{EditorError, source_art::*};
use opencut_editor_application::source_art::*;
use opencut_native_services::source_art::{
    SourceArtProcessConfig, resolve_identity, runtime_fingerprint,
};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};
#[derive(PartialEq, Eq)]
struct Observation {
    fingerprint: String,
    files: Vec<(PathBuf, FileIdentity)>,
}
struct Captured {
    observation: Observation,
    identity: SourceArtIdentity,
}
fn observation(worker: &Path) -> Result<Observation, EditorError> {
    let fingerprint = runtime_fingerprint(worker)?;
    let unavailable = || {
        EditorError::new(
            "SOURCE_ART_PROTOCOL_ERROR",
            "Cannot observe native source runtime",
        )
    };
    let libraries =
        opencut_media::native_decode::identity::libraries().map_err(|_| unavailable())?;
    let mut paths = vec![worker.to_owned()];
    paths.extend(libraries.into_iter().map(|library| library.path));
    paths.sort();
    paths.dedup();
    let files = paths
        .into_iter()
        .map(|path| {
            let identity = FileIdentity::of(std::fs::metadata(&path).map_err(|_| unavailable())?)
                .map_err(|_| unavailable())?;
            Ok((path, identity))
        })
        .collect::<Result<_, EditorError>>()?;
    Ok(Observation { fingerprint, files })
}

fn capture(
    cached: &mut Option<Captured>,
    mut observe: impl FnMut() -> Result<Observation, EditorError>,
    resolve: impl FnOnce() -> Result<SourceArtIdentity, EditorError>,
) -> Result<SourceArtIdentity, EditorError> {
    let before = observe()?;
    // Reuse byte facts only with a change-time identity. No access grant is
    // retained here; application admission and worker checks still run.
    if cfg!(unix)
        && let Some(value) = cached.as_ref().filter(|value| value.observation == before)
    {
        return Ok(value.identity.clone());
    }
    let identity = resolve()?;
    if observe()? != before {
        return Err(EditorError::new(
            "SOURCE_ART_ENGINE_CHANGED",
            "Native source dependencies changed during capture",
        ));
    }
    *cached = Some(Captured {
        observation: before,
        identity: identity.clone(),
    });
    Ok(identity)
}
pub(crate) fn configured(
    policy: &SourceArtPolicy,
    query: &SourceArtQuery,
) -> Result<SourceArtProcessConfig, EditorError> {
    validate_policy(policy)?;
    let worker = crate::native_worker_runtime::worker()?;
    static CAPTURED: OnceLock<Mutex<Option<Captured>>> = OnceLock::new();
    let mut captured = CAPTURED
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| EditorError::new("INTERNAL_ERROR", "Source runtime capture lock failed"))?;
    let mut identity = capture(
        &mut captured,
        || observation(&worker),
        || resolve_identity(&worker),
    )?;
    identity.set_algorithm(match query {
        SourceArtQuery::Waveform { .. } => SOURCE_PEAKS_ALGORITHM,
        SourceArtQuery::Thumbnail {} => THUMBNAIL_ALGORITHM,
    });
    Ok(SourceArtProcessConfig {
        worker,
        strict_cgroup: None,
        deadline: Duration::from_millis(policy.deadline_ms),
        max_encoded_bytes: policy.max_encoded_bytes,
        max_pcm_bytes: policy.max_pcm_bytes,
        max_rss_bytes: policy.max_rss_bytes,
        identity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        fs::{self, File, FileTimes},
    };

    fn identity() -> SourceArtIdentity {
        SourceArtIdentity::Browser(BrowserSourceArtIdentity {
            algorithm: SOURCE_PEAKS_ALGORITHM.into(),
            decode_profile: SOURCE_ART_DECODE_PROFILE.into(),
            browser_version: "fixture".into(),
            browser_sha256: "b".repeat(64),
            node_sha256: "c".repeat(64),
            module_sha256: "d".repeat(64),
        })
    }
    fn observed(paths: &[PathBuf], fingerprint: &str) -> Result<Observation, EditorError> {
        let files = paths
            .iter()
            .map(|path| {
                let metadata = fs::metadata(path).map_err(|_| {
                    EditorError::new("SOURCE_ART_PROTOCOL_ERROR", "Missing dependency")
                })?;
                Ok((path.clone(), FileIdentity::of(metadata)?))
            })
            .collect::<Result<_, EditorError>>()?;
        Ok(Observation {
            fingerprint: fingerprint.into(),
            files,
        })
    }

    #[test]
    fn live_observation_includes_worker_and_each_loaded_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let worker = dir.path().join("worker");
        fs::write(&worker, b"fixture").unwrap();
        let observed = observation(&worker).unwrap();
        assert!(observed.files.iter().any(|(path, _)| path == &worker));
        for library in opencut_media::native_decode::identity::libraries().unwrap() {
            assert!(observed.files.iter().any(|(path, _)| path == &library.path));
        }
        fs::remove_file(worker.clone()).unwrap();
        assert!(observation(&worker).is_err());
    }

    #[test]
    fn unchanged_dependencies_reuse_bytes_but_any_library_or_module_change_recaptures() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [dir.path().join("worker"), dir.path().join("decoder")];
        for path in &paths {
            fs::write(path, b"initial").unwrap();
        }
        let mut cache = None;
        let count = Cell::new(0);
        let resolve = || {
            count.set(count.get() + 1);
            Ok(identity())
        };
        for _ in 0..3 {
            capture(&mut cache, || observed(&paths, "module"), resolve).unwrap();
        }
        assert_eq!(count.get(), if cfg!(unix) { 1 } else { 3 });
        let before = count.get();
        for path in &paths {
            // A same-size rewrite with its mtime restored still changes ctime.
            let modified = fs::metadata(path).unwrap().modified().unwrap();
            std::thread::sleep(Duration::from_millis(2));
            fs::write(path, b"changed").unwrap();
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(modified))
                .unwrap();
            capture(&mut cache, || observed(&paths, "module"), resolve).unwrap();
        }
        assert_eq!(count.get(), before + 2);
        capture(&mut cache, || observed(&paths, "new-module"), resolve).unwrap();
        assert_eq!(count.get(), before + 3);
        fs::remove_file(&paths[1]).unwrap();
        assert!(capture(&mut cache, || observed(&paths, "new-module"), resolve).is_err());
        assert_eq!(count.get(), before + 3);
    }

    #[test]
    fn replacement_with_identical_size_and_mtime_cannot_reuse_old_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worker");
        fs::write(&path, b"old").unwrap();
        let paths = [path.clone()];
        let mut cache = None;
        capture(&mut cache, || observed(&paths, "module"), || Ok(identity())).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let replacement = dir.path().join("replacement");
        fs::write(&replacement, b"new").unwrap();
        File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        fs::rename(replacement, path).unwrap();
        let count = Cell::new(0);
        capture(
            &mut cache,
            || observed(&paths, "module"),
            || {
                count.set(count.get() + 1);
                Ok(identity())
            },
        )
        .unwrap();
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn changing_dependency_during_capture_or_resolver_failure_never_publishes_a_fact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worker");
        fs::write(&path, b"before").unwrap();
        let paths = [path.clone()];
        let mut cache = None;
        let result = capture(
            &mut cache,
            || observed(&paths, "module"),
            || {
                fs::write(&path, b"changed-during-capture").unwrap();
                Ok(identity())
            },
        );
        assert_eq!(result.unwrap_err().code, "SOURCE_ART_ENGINE_CHANGED");
        assert!(cache.is_none());
        let result = capture(
            &mut cache,
            || observed(&paths, "module"),
            || Err(EditorError::new("IO_ERROR", "fixture")),
        );
        assert_eq!(result.unwrap_err().code, "IO_ERROR");
        assert!(cache.is_none());
        capture(&mut cache, || observed(&paths, "module"), || Ok(identity())).unwrap();
        assert!(cache.is_some());
    }
}
