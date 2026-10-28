pub use opencut_api_contracts::media::{InspectMediaInput, RepairMediaInput};
use opencut_asset_store::catalog::MediaCatalog;
use opencut_contracts::{Asset, CatalogEntry, EditorError};
use opencut_editor_application::maintenance::{
    PublicationJournal, ReconcileInput, ReconcileResult, UntrackedPublication,
};
use opencut_editor_application::media::{self as application, DetectedMedia};
use opencut_media::{FfmpegBackend, MediaMetadata, ProbeRequest};
use std::path::Path;
/// AVIO borrows application authority on every call. Those errors are already
/// typed by the owning session; do not erase cancellation/revocation codes via
/// MediaError's storage-admission compatibility mapping.
pub(crate) fn execution_error(error: opencut_media::MediaError) -> EditorError {
    match error {
        opencut_media::MediaError::Admission { code, message } => EditorError::new(code, message),
        other => EditorError::new(other.code(), other.to_string()),
    }
}
pub type ProbeFactory = dyn Fn() -> Result<FfmpegBackend, EditorError>;
pub struct NativeMediaCatalog<'a>(
    pub &'a Path,
    pub Option<&'a dyn PublicationJournal>,
    pub Option<&'a ProbeFactory>,
);
fn utf8(path: &Path) -> Result<String, EditorError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| EditorError::new("INVALID_ARGUMENT", "Media path is not valid UTF-8"))
}
impl application::MediaCatalog for NativeMediaCatalog<'_> {
    fn inspect(&self, hash: &str) -> Result<CatalogEntry, EditorError> {
        MediaCatalog::read_only(self.0).inspect(hash)
    }
    fn repair(
        &self,
        input: &RepairMediaInput,
        classify: &dyn Fn(DetectedMedia) -> Result<Asset, EditorError>,
    ) -> Result<CatalogEntry, EditorError> {
        MediaCatalog::open_managed(self.0)?.repair_observed(
            &input.content_hash,
            input.expected_metadata_hash.as_deref(),
            |path, name, size| {
                let backend = self.2.ok_or_else(|| {
                    EditorError::new("CAPABILITY_UNAVAILABLE", "Media probe is not configured")
                })?()?;
                let metadata = backend
                    .probe(&ProbeRequest { input: path.into() })
                    .map_err(|e| EditorError::new("CAPABILITY_UNAVAILABLE", e.to_string()))?;
                let uri = utf8(path)?;
                let metadata = metadata.try_map_paths(|path| utf8(&path))?;
                classify(DetectedMedia {
                    metadata,
                    uri,
                    name: name.into(),
                    content_hash: input.content_hash.clone(),
                    size,
                })
            },
            self.1.unwrap_or(&UntrackedPublication),
        )
    }
    fn publish(&self, hash: &str, asset: &Asset) -> Result<(), EditorError> {
        MediaCatalog::open_managed(self.0)?.publish_observed(
            hash,
            asset,
            self.1.unwrap_or(&UntrackedPublication),
        )
    }
}
pub fn inspect(root: &Path, input: InspectMediaInput) -> Result<CatalogEntry, EditorError> {
    application::inspect(&NativeMediaCatalog(root, None, None), input)
}
pub fn repair(
    root: &Path,
    input: RepairMediaInput,
    probe: &ProbeFactory,
) -> Result<CatalogEntry, EditorError> {
    application::repair(&NativeMediaCatalog(root, None, Some(probe)), input)
}
pub fn describe(
    metadata: &MediaMetadata,
    path: &Path,
    name: &str,
    hash: &str,
    size: u64,
) -> Result<Asset, EditorError> {
    application::describe(metadata, &utf8(path)?, name, hash, size)
}
pub fn publish(root: &Path, hash: &str, asset: &Asset) -> Result<(), EditorError> {
    application::publish(&NativeMediaCatalog(root, None, None), hash, asset)
}
pub fn repair_observed(
    root: &Path,
    input: RepairMediaInput,
    journal: &dyn PublicationJournal,
    probe: &ProbeFactory,
) -> Result<CatalogEntry, EditorError> {
    application::repair(&NativeMediaCatalog(root, Some(journal), Some(probe)), input)
}
pub fn import_observed(
    root: &Path,
    staging: &Path,
    hash: &str,
    asset: &Asset,
    journal: &dyn PublicationJournal,
) -> Result<(), EditorError> {
    MediaCatalog::open_managed(root)?.import_observed(staging, hash, asset, journal)
}
pub fn reconcile(
    root: &Path,
    input: ReconcileInput,
    journal: &dyn PublicationJournal,
) -> Result<ReconcileResult, EditorError> {
    opencut_editor_application::maintenance::validate_object(&input.object)?;
    MediaCatalog::open_managed(root)?.reconcile(input.object, journal)
}
/// Verify immutable media before publishing an owner-authorized project conversion.
pub fn verify_asset_versions(
    root: &std::path::Path,
    assets: &[opencut_contracts::Asset],
) -> Result<(), opencut_contracts::EditorError> {
    let store = opencut_asset_store::AssetStore::read_only(root)?;
    for asset in assets {
        let _verified = store.open_verified(asset)?;
    }
    Ok(())
}
