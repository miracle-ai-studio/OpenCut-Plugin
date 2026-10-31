//! Immutable, content-addressed media for project revisions and accepted jobs.
//! Versions and job manifests identify bytes independently of provenance paths.
pub mod catalog;
pub mod font_import;
pub mod gate;
pub mod uploads;
pub mod verification;
use opencut_contracts::{Asset, AssetVersion, EditorError, ProjectSnapshot};
use sha2::{Digest, Sha256};
use std::sync::{Mutex, OnceLock};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use verification::{FileIdentity, VerificationFacts};

pub use opencut_contracts::{AssetLockManifest, LockedAsset};

pub struct AssetStore {
    root: PathBuf,
    writable: bool,
    budget: Option<opencut_storage_budget::Budget>,
    _gate: gate::Gate,
}

impl AssetStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, EditorError> {
        fs::create_dir_all(root.as_ref()).map_err(io_error)?;
        let root = fs::canonicalize(root).map_err(io_error)?;
        if let Some(parent) = root.parent() {
            sync_directory(parent)?;
        }
        let guard = gate::Gate::shared(&root)?;
        Ok(Self {
            root,
            _gate: guard,
            writable: true,
            budget: None,
        })
    }

    /// Host-managed CAS: private bytes share the workspace admission ledger.
    /// Standalone embedders using open() must supply their own resource policy.
    pub fn open_managed(root: impl AsRef<Path>) -> Result<Self, EditorError> {
        let mut store = Self::open(root)?;
        #[cfg(unix)]
        {
            store.budget = Some(opencut_storage_budget::Budget::new(
                store
                    .root
                    .parent()
                    .ok_or_else(|| error("INVALID_ARGUMENT", "Missing managed workspace"))?,
            ));
        }
        Ok(store)
    }
    fn reserve(
        &self,
        category: &str,
        bytes: u64,
    ) -> Result<Option<opencut_storage_budget::Reservation>, EditorError> {
        self.budget
            .as_ref()
            .map(|b| b.reserve(category, bytes))
            .transpose()
    }

    pub fn capture(
        &self,
        snapshot: &ProjectSnapshot,
        sequence_id: &str,
        asset_ids: &[String],
    ) -> Result<AssetLockManifest, EditorError> {
        validate_sequence(snapshot, sequence_id)?;
        let mut assets = Vec::new();
        for id in asset_ids.iter().collect::<BTreeSet<_>>() {
            let asset = snapshot
                .project
                .assets
                .iter()
                .find(|asset| &asset.id == id)
                .ok_or_else(|| {
                    error(
                        "ASSET_NOT_FOUND",
                        format!("Asset {id} is absent from the revision"),
                    )
                })?;
            let pinned = self.pin_asset(asset)?;
            let version = pinned.version.expect("pin_asset establishes a version");
            assets.push(LockedAsset {
                asset_id: id.clone(),
                content_hash: version.content_hash,
                size: version.size,
            });
        }
        Ok(AssetLockManifest {
            version: 1,
            revision: snapshot.revision.clone(),
            sequence_id: sequence_id.into(),
            assets,
        })
    }

    /// Open a read-only view, including when the store is absent. Resolution
    /// then reports missing content without creating directories during queries.
    pub fn read_only(root: impl AsRef<Path>) -> Result<Self, EditorError> {
        let root = std::path::absolute(root).map_err(io_error)?;
        let guard = gate::Gate::shared(&root)?;
        Ok(Self {
            root,
            _gate: guard,
            writable: false,
            budget: None,
        })
    }

    /// Pin a new binding, or validate an explicitly reused immutable version.
    /// A versioned asset never falls back to its provenance URI.
    pub fn pin_asset(&self, asset: &Asset) -> Result<Asset, EditorError> {
        if asset.version.is_some() {
            let resolved = self.resolve_asset(asset)?;
            if self.writable {
                regular_file(Path::new(&resolved.uri))?
                    .sync_all()
                    .map_err(io_error)?;
                sync_directory(
                    Path::new(&resolved.uri)
                        .parent()
                        .expect("CAS object parent"),
                )?;
                sync_directory(&self.root)?;
            }
            return Ok(asset.clone());
        }
        if !self.writable {
            return Err(error(
                "ASSET_STORE_READ_ONLY",
                "Read-only asset resolution cannot capture new media",
            ));
        }
        let id = &asset.id;
        let raw = asset.uri.strip_prefix("file://").unwrap_or(&asset.uri);
        if (asset.uri.contains("://") && !asset.uri.starts_with("file://")) || raw.contains('%') {
            return Err(error(
                "CAPABILITY_UNAVAILABLE",
                format!("Asset {id} requires a supported local file URI"),
            ));
        }
        let mut source = regular_file(Path::new(raw))
            .map_err(|e| error("ASSET_UNAVAILABLE", format!("Asset {id}: {}", e.message)))?;
        let before = source.metadata().map_err(io_error)?;
        let mut reservation = self.reserve("cas_capture", before.len())?;
        let temporary = Temporary(self.root.join(format!(".capture-{}", uuid::Uuid::new_v4())));
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary.0)
            .map_err(io_error)?;
        let (content_hash, size) = copy_hash_admitted(
            &mut source,
            &mut destination,
            &|| false,
            reservation.as_mut(),
        )?;
        let after = source.metadata().map_err(io_error)?;
        if size != before.len()
            || size != after.len()
            || before.modified().ok() != after.modified().ok()
        {
            return Err(error(
                "ASSET_CHANGED",
                format!("Asset {id} changed while being captured; retry with stable input"),
            ));
        }
        destination.sync_all().map_err(io_error)?;
        make_read_only(&destination)?;
        drop(destination);
        let locked = LockedAsset {
            asset_id: id.clone(),
            content_hash,
            size,
        };
        let path = self.object_path(&locked.content_hash)?;
        fs::create_dir_all(path.parent().expect("object parent")).map_err(io_error)?;
        sync_directory(&self.root)?;
        // A hard link publishes a complete object without replacing a winner.
        match fs::hard_link(&temporary.0, &path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_file(&path, &locked)?;
            }
            Err(e) => return Err(io_error(e)),
        }
        // A concurrent publisher may not have synced its directory yet.
        // Every successful capture establishes durability before revision or Job commit.
        sync_directory(path.parent().expect("object parent"))?;
        let mut pinned = asset.clone();
        pinned.version = Some(AssetVersion {
            id: format!("av_{}", locked.content_hash),
            content_hash: locked.content_hash,
            size: locked.size,
        });
        Ok(pinned)
    }

    /// Return a temporary execution view for read-only media inspection. The
    /// project revision and version provenance remain unchanged in the caller.
    pub fn resolve_asset(&self, asset: &Asset) -> Result<Asset, EditorError> {
        let Some(version) = &asset.version else {
            return Ok(asset.clone());
        };
        if version.id != format!("av_{}", version.content_hash) {
            return Err(error(
                "ASSET_VERSION_INVALID",
                "Asset version id does not match its content hash",
            ));
        }
        let path = self
            .object_path(&version.content_hash)
            .map_err(|e| error("ASSET_VERSION_INVALID", e.message))?;
        let locked = LockedAsset {
            asset_id: asset.id.clone(),
            content_hash: version.content_hash.clone(),
            size: version.size,
        };
        verify_file(&path, &locked).map_err(|e| {
            error(
                "ASSET_INTEGRITY_ERROR",
                format!("Asset {} version {}: {}", asset.id, version.id, e.message),
            )
        })?;
        let mut resolved = asset.clone();
        resolved.uri = path.to_string_lossy().into_owned();
        Ok(resolved)
    }

    /// Open the exact versioned bytes for an authorized reader. Verify the
    /// opened descriptor before returning it so a later path replacement
    /// cannot retarget the response.
    pub fn open_verified(&self, asset: &Asset) -> Result<(File, u64), EditorError> {
        let version = asset.version.as_ref().ok_or_else(|| {
            error(
                "ASSET_VERSION_INVALID",
                "Project media has no fixed content version",
            )
        })?;
        self.open_version_verified(version)
    }

    /// The caller must authorize this version through a project resource port.
    /// Verification identifies immutable bytes; it does not grant project access.
    pub fn open_version_verified(
        &self,
        version: &AssetVersion,
    ) -> Result<(File, u64), EditorError> {
        let (file, size, _) = self.open_version_checked(version, &mut || Ok(()))?;
        Ok((file, size))
    }

    /// Hash once through the opened descriptor, checking cancellation/authority
    /// between bounded reads. A verified descriptor never grants access itself.
    pub fn open_version_checked(
        &self,
        version: &AssetVersion,
        checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
    ) -> Result<(File, u64, std::time::SystemTime), EditorError> {
        let (file, size, identity) = self.open_version_with_identity(version, checkpoint)?;
        Ok((file, size, identity.modified()))
    }

    /// Return the identity observed during verification, not a later observation
    /// that could accidentally bind changed bytes to the earlier content hash.
    pub fn open_version_with_identity(
        &self,
        version: &AssetVersion,
        checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
    ) -> Result<(File, u64, FileIdentity), EditorError> {
        checkpoint()?;
        if version.id != format!("av_{}", version.content_hash) {
            return Err(error(
                "ASSET_VERSION_INVALID",
                "Asset version id does not match its content hash",
            ));
        }
        let path = self.object_path(&version.content_hash)?;
        if !fs::symlink_metadata(&path)
            .map_err(io_error)?
            .file_type()
            .is_file()
        {
            return Err(error(
                "ASSET_UNAVAILABLE",
                "Content-addressed object is not a regular file",
            ));
        }
        let mut file = regular_file(&path)?;
        let before = file.metadata().map_err(io_error)?;
        if before.len() != version.size {
            return Err(error(
                "ASSET_INTEGRITY_ERROR",
                "Project media length does not match the fixed version",
            ));
        }
        let identity = FileIdentity::of(before.clone())?;
        static VERIFIED: OnceLock<Mutex<VerificationFacts<(PathBuf, AssetVersion)>>> =
            OnceLock::new();
        let verified = VERIFIED.get_or_init(Mutex::default);
        let key = (self.root.clone(), version.clone());
        let known = verified
            .lock()
            .map_err(|_| error("ASSET_INTEGRITY_ERROR", "Verification facts unavailable"))?
            .contains(&key, &identity);
        if known {
            if FileIdentity::of(file.metadata().map_err(io_error)?)? != identity {
                return Err(error(
                    "ASSET_INTEGRITY_ERROR",
                    "Project media changed during verification",
                ));
            }
            checkpoint()?;
            if FileIdentity::of(file.metadata().map_err(io_error)?)? != identity {
                return Err(error(
                    "ASSET_INTEGRITY_ERROR",
                    "Project media changed during admission",
                ));
            }
            return Ok((file, version.size, identity));
        }
        // A changing file cannot turn verification into an endless read. This
        // is the exact declared identity plus one sentinel byte,not a media cap.
        let (hash, size) = {
            let maximum = version
                .size
                .checked_add(1)
                .ok_or_else(|| error("ASSET_VERSION_INVALID", "Asset length is not addressable"))?;
            let mut bounded = (&mut file).take(maximum);
            copy_hash_checked(&mut bounded, &mut std::io::sink(), checkpoint, None)?
        };
        let after = file.metadata().map_err(io_error)?;
        if FileIdentity::of(after.clone())? != identity {
            return Err(error(
                "ASSET_INTEGRITY_ERROR",
                "Project media changed during verification",
            ));
        }
        checkpoint()?;
        if hash != version.content_hash || size != version.size {
            return Err(error(
                "ASSET_INTEGRITY_ERROR",
                "Project media bytes do not match the fixed version",
            ));
        }
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        if FileIdentity::of(file.metadata().map_err(io_error)?)? != identity {
            return Err(error(
                "ASSET_INTEGRITY_ERROR",
                "Project media changed during admission",
            ));
        }
        verified
            .lock()
            .map_err(|_| error("ASSET_INTEGRITY_ERROR", "Verification facts unavailable"))?
            .remember(key, identity.clone());
        Ok((file, size, identity))
    }

    /// Copy and verify immutable inputs into an attempt-private directory. Never
    /// fall back to the original URI when a locked object is absent or corrupt.
    pub fn materialize(
        &self,
        snapshot: &ProjectSnapshot,
        sequence_id: &str,
        manifest: &AssetLockManifest,
        required_ids: &[String],
        directory: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ProjectSnapshot, EditorError> {
        validate_sequence(snapshot, sequence_id)?;
        let expected: BTreeSet<_> = required_ids.iter().collect();
        let observed: BTreeSet<_> = manifest.assets.iter().map(|a| &a.asset_id).collect();
        if manifest.version != 1
            || manifest.revision != snapshot.revision
            || manifest.sequence_id != sequence_id
            || expected != observed
            || observed.len() != manifest.assets.len()
        {
            return Err(error(
                "ASSET_LOCK_INVALID",
                "Asset lock does not match the fixed revision and required inputs",
            ));
        }
        fs::create_dir_all(directory).map_err(io_error)?;
        let directory = fs::canonicalize(directory).map_err(io_error)?;
        let mut resolved = snapshot.clone();
        for (index, locked) in manifest.assets.iter().enumerate() {
            let asset = resolved
                .project
                .assets
                .iter_mut()
                .find(|asset| asset.id == locked.asset_id)
                .ok_or_else(|| {
                    error(
                        "ASSET_LOCK_INVALID",
                        "Locked asset is absent from the fixed revision",
                    )
                })?;
            if let Some(version) = &asset.version
                && (version.id != format!("av_{}", version.content_hash)
                    || version.content_hash != locked.content_hash
                    || version.size != locked.size)
            {
                return Err(error(
                    "ASSET_LOCK_INVALID",
                    format!(
                        "Job lock differs from the immutable version of asset {}",
                        asset.id
                    ),
                ));
            }
            let path = self.object_path(&locked.content_hash)?;
            let mut source = regular_file(&path).map_err(|e| {
                error(
                    "ASSET_INTEGRITY_ERROR",
                    format!(
                        "Locked asset {} is unavailable: {}",
                        locked.asset_id, e.message
                    ),
                )
            })?;
            let mut reservation = self.reserve("private_asset_copy", locked.size)?;
            let private =
                Temporary(directory.join(format!("input-{index}-{}", uuid::Uuid::new_v4())));
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&private.0)
                .map_err(io_error)?;
            let (hash, size) =
                copy_hash_admitted(&mut source, &mut output, cancelled, reservation.as_mut())?;
            if hash != locked.content_hash || size != locked.size {
                return Err(error(
                    "ASSET_INTEGRITY_ERROR",
                    format!(
                        "Locked asset {} failed content verification",
                        locked.asset_id
                    ),
                ));
            }
            output.sync_all().map_err(io_error)?;
            make_read_only(&output)?;
            asset.uri = private.0.to_string_lossy().into_owned();
            // Attempt owner removes materialized files, including on failure.
            let _ = private.persist();
        }
        Ok(resolved)
    }

    fn object_path(&self, hash: &str) -> Result<PathBuf, EditorError> {
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(error("ASSET_LOCK_INVALID", "Invalid SHA-256 content hash"));
        }
        Ok(self.root.join(&hash[..2]).join(hash))
    }
}

fn validate_sequence(snapshot: &ProjectSnapshot, sequence_id: &str) -> Result<(), EditorError> {
    if snapshot
        .project
        .sequences
        .iter()
        .any(|s| s.id == sequence_id)
    {
        Ok(())
    } else {
        Err(error(
            "SEQUENCE_NOT_FOUND",
            "Sequence is absent from the fixed revision",
        ))
    }
}

fn regular_file(path: &Path) -> Result<File, EditorError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err(error("ASSET_UNAVAILABLE", "Asset must be a regular file"));
    }
    Ok(file)
}

fn copy_hash(
    source: &mut File,
    destination: &mut impl Write,
    cancelled: &dyn Fn() -> bool,
) -> Result<(String, u64), EditorError> {
    copy_hash_admitted(source, destination, cancelled, None)
}

fn copy_hash_admitted(
    source: &mut File,
    destination: &mut impl Write,
    cancelled: &dyn Fn() -> bool,
    reservation: Option<&mut opencut_storage_budget::Reservation>,
) -> Result<(String, u64), EditorError> {
    copy_hash_checked(
        source,
        destination,
        &mut || {
            if cancelled() {
                Err(error("JOB_CANCELLED", "Input materialization cancelled"))
            } else {
                Ok(())
            }
        },
        reservation,
    )
}

fn copy_hash_checked(
    source: &mut impl Read,
    destination: &mut impl Write,
    checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
    mut reservation: Option<&mut opencut_storage_budget::Reservation>,
) -> Result<(String, u64), EditorError> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut size = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        checkpoint()?;
        let count = source.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        let next = size
            .checked_add(count as u64)
            .ok_or_else(|| error("ASSET_TOO_LARGE", "Asset length overflow"))?;
        if let Some(reservation) = reservation.as_mut() {
            reservation.ensure(next)?;
        }
        destination.write_all(&buffer[..count]).map_err(io_error)?;
        digest.update(&buffer[..count]);
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| error("ASSET_TOO_LARGE", "Asset length overflow"))?;
    }
    let hash = digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((hash, size))
}

fn verify_file(path: &Path, locked: &LockedAsset) -> Result<(), EditorError> {
    let mut file = regular_file(path)?;
    let (hash, size) = copy_hash(&mut file, &mut std::io::sink(), &|| false)?;
    if hash != locked.content_hash || size != locked.size {
        return Err(error(
            "ASSET_INTEGRITY_ERROR",
            "Existing content-addressed object is corrupt",
        ));
    }
    Ok(())
}

fn make_read_only(file: &File) -> Result<(), EditorError> {
    let mut permissions = file.metadata().map_err(io_error)?.permissions();
    permissions.set_readonly(true);
    file.set_permissions(permissions).map_err(io_error)
}

fn sync_directory(path: &Path) -> Result<(), EditorError> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(io_error)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

struct Temporary(PathBuf);
impl Temporary {
    fn persist(mut self) -> PathBuf {
        std::mem::take(&mut self.0)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn error(code: &str, message: impl Into<String>) -> EditorError {
    EditorError::new(code, message)
}
fn io_error(e: std::io::Error) -> EditorError {
    error("ASSET_IO_ERROR", e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencut_contracts::{Asset, AssetKind, FrameRate, ProjectDocument, Sequence};

    fn snapshot(path: &Path) -> ProjectSnapshot {
        ProjectSnapshot {
            revision: "revision-a".into(),
            project: ProjectDocument {
                id: "project-a".into(),
                name: "test".into(),
                schema_version: 1,
                default_sequence_id: "main".into(),
                sequences: vec![Sequence {
                    id: "main".into(),
                    name: "main".into(),
                    width: 16,
                    height: 16,
                    fps: FrameRate {
                        numerator: 24,
                        denominator: 1,
                    },
                    tracks: vec![],
                    caption_cues: vec![],
                }],
                assets: vec![Asset {
                    version: None,
                    id: "source".into(),
                    name: "source".into(),
                    kind: AssetKind::Video,
                    uri: path.to_string_lossy().into_owned(),
                    duration: None,
                    metadata: Default::default(),
                }],
            },
        }
    }

    #[test]
    fn streaming_content_hash_matches_independent_digest_across_block_boundaries() {
        struct ShortReads(std::io::Cursor<Vec<u8>>);
        impl Read for ShortReads {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let length = bytes.len().min(997);
                self.0.read(&mut bytes[..length])
            }
        }
        for length in [0, 1, 63, 64, 65, 65_535, 65_536, 65_537, 131_072, 131_073] {
            let bytes: Vec<u8> = (0..length).map(|n| (n * 73 + 19) as u8).collect();
            let expected = format!("{:x}", Sha256::digest(&bytes));
            let mut output = Vec::new();
            let (actual, size) = copy_hash_checked(
                &mut ShortReads(std::io::Cursor::new(bytes.clone())),
                &mut output,
                &mut || Ok(()),
                None,
            )
            .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(size, length as u64);
            assert_eq!(output, bytes);
        }
    }

    #[test]
    fn versioned_binding_is_independent_of_source_and_job_lock_cannot_retarget_it() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.mov");
        fs::write(&source, b"original version").unwrap();
        let mut snapshot = snapshot(&source);
        let store = AssetStore::open(temp.path().join("cas")).unwrap();
        snapshot.project.assets[0] = store.pin_asset(&snapshot.project.assets[0]).unwrap();
        snapshot.project.schema_version = 2;
        let version = snapshot.project.assets[0].version.clone().unwrap();
        assert_eq!(version.id, format!("av_{}", version.content_hash));
        let ids = vec!["source".into()];
        let first = store.capture(&snapshot, "main", &ids).unwrap();
        fs::write(&source, b"replacement version").unwrap();
        let second = store.capture(&snapshot, "main", &ids).unwrap();
        assert_eq!(first, second);
        let mut replacement = snapshot.project.assets[0].clone();
        replacement.version = None;
        let replacement = store.pin_asset(&replacement).unwrap();
        assert_ne!(replacement.version, Some(version.clone()));
        let mut retargeted = first.clone();
        let other_version = replacement.version.unwrap();
        retargeted.assets[0].content_hash = other_version.content_hash;
        retargeted.assets[0].size = other_version.size;
        assert_eq!(
            store
                .materialize(
                    &snapshot,
                    "main",
                    &retargeted,
                    &ids,
                    &temp.path().join("attempt"),
                    &|| false
                )
                .unwrap_err()
                .code,
            "ASSET_LOCK_INVALID"
        );
        fs::remove_file(&source).unwrap();
        let read_only = AssetStore::read_only(temp.path().join("cas")).unwrap();
        let resolved = read_only
            .resolve_asset(&snapshot.project.assets[0])
            .unwrap();
        assert_eq!(resolved.version, Some(version.clone()));
        assert_eq!(fs::read(&resolved.uri).unwrap(), b"original version");
        assert_eq!(
            store.pin_asset(&snapshot.project.assets[0]).unwrap(),
            snapshot.project.assets[0]
        );
        fs::write(&source, b"original version").unwrap();
        fs::remove_file(store.object_path(&version.content_hash).unwrap()).unwrap();
        assert_eq!(
            store.capture(&snapshot, "main", &ids).unwrap_err().code,
            "ASSET_INTEGRITY_ERROR"
        );
        assert_eq!(
            read_only
                .resolve_asset(&snapshot.project.assets[0])
                .unwrap_err()
                .code,
            "ASSET_INTEGRITY_ERROR"
        );
    }

    #[test]
    fn readonly_resolution_creates_nothing_and_rejects_forged_versions() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"media").unwrap();
        let mut asset = snapshot(&source).project.assets.remove(0);
        let missing_root = temp.path().join("missing-store");
        let reader = AssetStore::read_only(&missing_root).unwrap();
        assert_eq!(reader.resolve_asset(&asset).unwrap(), asset);
        assert_eq!(
            reader.pin_asset(&asset).unwrap_err().code,
            "ASSET_STORE_READ_ONLY"
        );
        assert!(!missing_root.exists());
        let store = AssetStore::open(temp.path().join("cas")).unwrap();
        asset = store.pin_asset(&asset).unwrap();
        let mut forged = asset.clone();
        forged.version.as_mut().unwrap().id = "different-version".into();
        assert_eq!(
            store.pin_asset(&forged).unwrap_err().code,
            "ASSET_VERSION_INVALID"
        );
        forged = asset.clone();
        forged.version.as_mut().unwrap().size += 1;
        assert_eq!(
            store.pin_asset(&forged).unwrap_err().code,
            "ASSET_INTEGRITY_ERROR"
        );
        assert!(reader.resolve_asset(&asset).is_err());
        assert!(!missing_root.exists());
    }

    #[test]
    fn captured_content_survives_replacement_deletion_and_private_copy_is_isolated() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.mov");
        fs::write(&source, b"original media bytes").unwrap();
        let snapshot = snapshot(&source);
        let store = AssetStore::open(temp.path().join("cas")).unwrap();
        let ids = vec!["source".into()];
        let lock = store.capture(&snapshot, "main", &ids).unwrap();
        fs::write(&source, b"new content").unwrap();
        let new_lock = store.capture(&snapshot, "main", &ids).unwrap();
        assert_ne!(lock.assets[0].content_hash, new_lock.assets[0].content_hash);
        fs::remove_file(&source).unwrap();
        let resolved = store
            .materialize(
                &snapshot,
                "main",
                &lock,
                &ids,
                &temp.path().join("attempt"),
                &|| false,
            )
            .unwrap();
        let private_path = &resolved.project.assets[0].uri;
        assert_eq!(fs::read(private_path).unwrap(), b"original media bytes");
        assert_eq!(snapshot.project.assets[0].uri, source.to_string_lossy());
        let object = store.object_path(&lock.assets[0].content_hash).unwrap();
        fs::remove_file(&object).unwrap();
        fs::write(&object, b"corrupt").unwrap();
        assert_eq!(fs::read(private_path).unwrap(), b"original media bytes");
        assert_eq!(
            store
                .materialize(
                    &snapshot,
                    "main",
                    &lock,
                    &ids,
                    &temp.path().join("second"),
                    &|| false
                )
                .unwrap_err()
                .code,
            "ASSET_INTEGRITY_ERROR"
        );
        assert_eq!(fs::read_dir(temp.path().join("second")).unwrap().count(), 0);
    }

    #[test]
    fn lock_requires_exact_revision_sequence_coverage_and_valid_hash() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"input").unwrap();
        let snapshot = snapshot(&source);
        let store = AssetStore::open(temp.path().join("cas")).unwrap();
        let ids = vec!["source".into()];
        let lock = store.capture(&snapshot, "main", &ids).unwrap();
        for variant in 0..6 {
            let mut invalid = lock.clone();
            match variant {
                0 => invalid.version = 2,
                1 => invalid.revision = "wrong".into(),
                2 => invalid.sequence_id = "wrong".into(),
                3 => invalid.assets.clear(),
                4 => invalid.assets.push(invalid.assets[0].clone()),
                _ => invalid.assets[0].content_hash = "../escape".into(),
            }
            assert_eq!(
                store
                    .materialize(
                        &snapshot,
                        "main",
                        &invalid,
                        &ids,
                        &temp.path().join("attempt"),
                        &|| false
                    )
                    .unwrap_err()
                    .code,
                "ASSET_LOCK_INVALID"
            );
        }
    }

    #[test]
    fn capture_deduplicates_and_ignores_unrequested_missing_media() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"input").unwrap();
        let mut snapshot = snapshot(&source);
        let mut missing = snapshot.project.assets[0].clone();
        missing.id = "unused".into();
        missing.uri = "/missing/unused.mov".into();
        snapshot.project.assets.push(missing);
        let store = AssetStore::open(temp.path().join("cas")).unwrap();
        let ids = vec!["source".into(), "source".into()];
        let lock = store.capture(&snapshot, "main", &ids).unwrap();
        assert_eq!(lock.assets.len(), 1);
        assert_eq!(lock, store.capture(&snapshot, "main", &ids).unwrap());
        assert!(
            store
                .capture(&snapshot, "main", &["unused".into()])
                .is_err()
        );
        let serialized = serde_json::to_string(&lock).unwrap();
        assert!(!serialized.contains(&source.to_string_lossy().to_string()));
    }

    #[test]
    fn concurrent_capture_publishes_one_valid_content_object() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, vec![42u8; 600_000]).unwrap();
        let snapshot = snapshot(&source);
        let store = AssetStore::open(temp.path().join("cas")).unwrap();
        let locks = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        store
                            .capture(&snapshot, "main", &["source".into()])
                            .unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(locks.iter().all(|lock| lock == &locks[0]));
        assert_eq!(fs::read_dir(&store.root).unwrap().count(), 1);
        verify_file(
            &store.object_path(&locks[0].assets[0].content_hash).unwrap(),
            &locks[0].assets[0],
        )
        .unwrap();
    }

    #[test]
    fn cancellation_removes_incomplete_private_copy_and_special_files_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, vec![42u8; 300_000]).unwrap();
        let snapshot = snapshot(&source);
        let store = AssetStore::open(temp.path().join("cas")).unwrap();
        let ids = vec!["source".into()];
        let lock = store.capture(&snapshot, "main", &ids).unwrap();
        let calls = std::cell::Cell::new(0);
        let directory = temp.path().join("attempt");
        let result = store.materialize(&snapshot, "main", &lock, &ids, &directory, &|| {
            calls.set(calls.get() + 1);
            calls.get() > 1
        });
        assert_eq!(result.unwrap_err().code, "JOB_CANCELLED");
        assert_eq!(fs::read_dir(directory).unwrap().count(), 0);
        assert!(regular_file(temp.path()).is_err());
        #[cfg(unix)]
        assert!(regular_file(Path::new("/dev/null")).is_err());
    }
    #[cfg(unix)]
    fn verification_fixture() -> (tempfile::TempDir, AssetStore, AssetVersion, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        fs::write(&source, vec![17; 131_072]).unwrap();
        let store = AssetStore::open(temp.path().join("cas")).unwrap();
        let version = store
            .pin_asset(&snapshot(&source).project.assets[0])
            .unwrap()
            .version
            .unwrap();
        let path = store.object_path(&version.content_hash).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (temp, store, version, path)
    }

    #[cfg(unix)]
    #[test]
    fn verification_reuses_bytes_across_open_but_never_authority() {
        let (_temp, store, version, _) = verification_fixture();
        let mut first = 0;
        drop(
            store
                .open_version_checked(&version, &mut || {
                    first += 1;
                    Ok(())
                })
                .unwrap(),
        );
        assert!(first > 2);
        let mut second = 0;
        let (mut file, size, _) = store
            .open_version_checked(&version, &mut || {
                second += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(second, 2);
        assert_eq!(size, 131_072);
        let mut bytes = [0; 8];
        file.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [17; 8]);
        for deny_at in [1, 2] {
            let mut checks = 0;
            let error = store
                .open_version_checked(&version, &mut || {
                    checks += 1;
                    if checks == deny_at {
                        Err(error("NOT_AUTHORIZED", "revoked"))
                    } else {
                        Ok(())
                    }
                })
                .err()
                .unwrap();
            assert_eq!(error.code, "NOT_AUTHORIZED");
        }
    }

    #[cfg(unix)]
    #[test]
    fn verification_detects_same_size_rewrite_even_with_restored_modified_time() {
        let (_temp, store, version, path) = verification_fixture();
        drop(store.open_version_verified(&version).unwrap());
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, vec![18; 131_072]).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(
            store.open_version_verified(&version).err().unwrap().code,
            "ASSET_INTEGRITY_ERROR"
        );
        fs::write(&path, vec![17; 131_072]).unwrap();
        let mut checks = 0;
        drop(
            store
                .open_version_checked(&version, &mut || {
                    checks += 1;
                    Ok(())
                })
                .unwrap(),
        );
        assert!(checks > 2);
    }

    #[cfg(unix)]
    #[test]
    fn verification_rejects_replaced_symlink_and_missing_objects() {
        let (_temp, store, version, path) = verification_fixture();
        drop(store.open_version_verified(&version).unwrap());
        let original = path.with_extension("original");
        fs::rename(&path, &original).unwrap();
        fs::write(&path, vec![18; 131_072]).unwrap();
        assert_eq!(
            store.open_version_verified(&version).err().unwrap().code,
            "ASSET_INTEGRITY_ERROR"
        );
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&original, &path).unwrap();
        assert_eq!(
            store.open_version_verified(&version).err().unwrap().code,
            "ASSET_UNAVAILABLE"
        );
        fs::remove_file(&path).unwrap();
        assert!(store.open_version_verified(&version).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn verification_rechecks_identity_after_final_admission_callback() {
        let (_temp, store, version, path) = verification_fixture();
        drop(store.open_version_verified(&version).unwrap());
        let mut checks = 0;
        let result = store.open_version_with_identity(&version, &mut || {
            checks += 1;
            if checks == 2 {
                fs::write(&path, vec![18; 131_072]).unwrap();
            }
            Ok(())
        });
        assert_eq!(result.err().unwrap().code, "ASSET_INTEGRITY_ERROR");
        assert_eq!(
            store.open_version_verified(&version).err().unwrap().code,
            "ASSET_INTEGRITY_ERROR"
        );
    }
}
