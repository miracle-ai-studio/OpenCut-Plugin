//! Trusted native source-art installation and live dependency cache fencing.
use opencut_contracts::{EditorError, source_art::*};
use opencut_editor_application::source_art::*;
use opencut_native_services::source_art::{
    SourceArtProcessConfig, resolve_identity, runtime_fingerprint,
};
use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
struct Captured {
    fingerprint: String,
    identity: SourceArtIdentity,
    at: Instant,
}
pub(crate) fn configured(
    policy: &SourceArtPolicy,
    query: &SourceArtQuery,
) -> Result<SourceArtProcessConfig, EditorError> {
    validate_policy(policy)?;
    let worker = crate::native_worker_runtime::worker()?;
    let fingerprint = runtime_fingerprint(&worker)?;
    static CAPTURED: OnceLock<Mutex<Option<Captured>>> = OnceLock::new();
    let mut captured = CAPTURED
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| EditorError::new("INTERNAL_ERROR", "Source runtime capture lock failed"))?;
    if !captured
        .as_ref()
        .is_some_and(|v| v.fingerprint == fingerprint && v.at.elapsed() < Duration::from_secs(30))
    {
        let identity = resolve_identity(&worker)?;
        if runtime_fingerprint(&worker)? != fingerprint {
            return Err(EditorError::new(
                "SOURCE_ART_ENGINE_CHANGED",
                "Native source dependencies changed during capture",
            ));
        }
        *captured = Some(Captured {
            fingerprint,
            identity,
            at: Instant::now(),
        });
    }
    let mut identity = captured
        .as_ref()
        .expect("captured native source identity")
        .identity
        .clone();
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
