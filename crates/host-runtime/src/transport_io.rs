//! Native data-plane handles owned by host assembly. Transport code consumes
//! bounded IO and typed results without constructing media or storage adapters.
use opencut_contracts::{Asset, EditorError};
use opencut_editor_application::{audit::ExecutionContext, budget::*};
use opencut_native_services::budget as native;
use std::{io::Read, path::Path, time::Duration};

pub struct AdmittedFile(native::AdmittedFile);
impl AdmittedFile {
    pub fn create(
        database: &Path,
        path: &Path,
        category: &str,
        ceiling: u64,
        anonymous: bool,
    ) -> Result<Self, EditorError> {
        native::AdmittedFile::create(database, path, category, ceiling, anonymous).map(Self)
    }
    pub fn append(&mut self, bytes: &[u8]) -> Result<(), EditorError> {
        self.0.append(bytes)
    }
    pub fn sync(&self) -> Result<(), EditorError> {
        self.0.sync()
    }
    pub fn rewind(&mut self) -> Result<(), EditorError> {
        self.0.rewind()
    }
    pub fn read_chunk(&mut self) -> Result<Vec<u8>, EditorError> {
        self.0.read_chunk()
    }
}
pub struct WorkspaceBudget<'a>(pub &'a Path);
impl BudgetNative for WorkspaceBudget<'_> {
    fn inspect(&self, recover: bool) -> Result<Report, EditorError> {
        native::NativeBudget(self.0).inspect(recover)
    }
    fn configure(
        &self,
        input: Configure,
        context: &ExecutionContext,
    ) -> Result<PolicyChange, EditorError> {
        native::NativeBudget(self.0).configure(input, context)
    }
    fn repair_transition(
        &self,
        action: RepairAction,
        input: RepairRequest,
        context: &ExecutionContext,
    ) -> Result<RepairChange, EditorError> {
        native::NativeBudget(self.0).repair_transition(action, input, context)
    }
}
pub struct ArtifactReader(opencut_native_services::artifacts::ArtifactSource);
impl ArtifactReader {
    pub fn open(root: &Path, job: &str, attempt: u32, path: &Path) -> Result<Self, EditorError> {
        opencut_native_services::artifacts::ArtifactSource::open(root, job, attempt, path).map(Self)
    }
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.0.metadata()
    }
}
impl Read for ArtifactReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(bytes)
    }
}

pub fn import_uploaded_media(
    database: &Path,
    context: ExecutionContext,
    root: &Path,
    staging: &Path,
    name: &str,
    hash: &str,
    size: u64,
) -> Result<Asset, EditorError> {
    use opencut_media::ProbeRequest;
    let backend = crate::media_tool_runtime::backend()
        .map_err(|e| EditorError::new("INVALID_ARGUMENT", e.message))?;
    let metadata = backend
        .probe(&ProbeRequest {
            input: staging.into(),
        })
        .map_err(|e| EditorError::new("INVALID_ARGUMENT", e.to_string()))?;
    let canonical = std::fs::canonicalize(root)
        .map_err(|e| EditorError::new("IO_ERROR", e.to_string()))?
        .join(hash);
    let asset = opencut_native_services::media::describe(&metadata, &canonical, name, hash, size)?;
    let mut app = opencut_sqlite_store::EditorApplication::open_managed(database)?;
    app.set_execution_context(context);
    opencut_native_services::media::import_observed(root, staging, hash, &asset, &app)?;
    Ok(asset)
}
pub fn open_verified_media(
    database: &Path,
    asset: &Asset,
) -> Result<(std::fs::File, u64), EditorError> {
    let root = database
        .parent()
        .ok_or_else(|| EditorError::new("INTERNAL_ERROR", "Missing workspace root"))?
        .join("asset-store");
    let store = opencut_asset_store::AssetStore::read_only(root).map_err(|_| {
        EditorError::new(
            "ARTIFACT_VERIFICATION_FAILED",
            "Project media is unavailable",
        )
    })?;
    store.open_verified(asset).map_err(|_| {
        EditorError::new(
            "ARTIFACT_VERIFICATION_FAILED",
            "Project media content is missing or corrupt",
        )
    })
}
/// Native host opens bytes only after application resource authorization.
pub fn open_verified_project_resource(
    database: &Path,
    version: &opencut_contracts::AssetVersion,
) -> Result<(std::fs::File, u64), EditorError> {
    let root = database
        .parent()
        .ok_or_else(|| EditorError::new("INTERNAL_ERROR", "Missing workspace root"))?
        .join("asset-store");
    let store = opencut_asset_store::AssetStore::read_only(root).map_err(|_| {
        EditorError::new(
            "ARTIFACT_VERIFICATION_FAILED",
            "Project resource is unavailable",
        )
    })?;
    store.open_version_verified(version).map_err(|_| {
        EditorError::new(
            "ARTIFACT_VERIFICATION_FAILED",
            "Project resource content is missing or corrupt",
        )
    })
}
pub fn release_upload_in_repair(
    database: &Path,
    context: ExecutionContext,
    upload_id: &str,
) -> Result<opencut_editor_application::uploads::UploadRetentionView, EditorError> {
    let mut app = opencut_sqlite_store::EditorApplication::open_managed_repair(database)?;
    app.set_execution_context(context);
    let root = app
        .asset_store_root()
        .parent()
        .ok_or_else(|| EditorError::new("STORAGE_ERROR", "Invalid asset root"))?
        .join("uploads");
    opencut_native_services::uploads::release(
        &root,
        &mut app,
        opencut_editor_application::uploads::UploadInput {
            upload_id: upload_id.into(),
        },
    )
}
pub fn recover_previews(database: &Path) -> Result<(), EditorError> {
    opencut_native_services::preview::recover_workspace(database).map(|_| ())
}
pub fn shutdown_previews(database: &Path) -> Result<(), EditorError> {
    let audio = crate::audio_preview::shutdown(database);
    let art = crate::source_art::shutdown(database);
    let preview = opencut_native_services::preview::shutdown_workspace(database);
    audio.and(art).and(preview)
}
pub struct PreviewStream(pub(crate) opencut_native_services::preview::Stream);
pub enum StreamRead {
    Chunk(Vec<u8>),
    Timeout,
    End,
}
impl PreviewStream {
    pub fn read(&self, timeout: Duration) -> Result<StreamRead, EditorError> {
        use opencut_native_services::preview::StreamRead as Native;
        self.0.read(timeout).map(|v| match v {
            Native::Chunk(bytes) => StreamRead::Chunk(bytes),
            Native::Timeout => StreamRead::Timeout,
            Native::End => StreamRead::End,
        })
    }
}
