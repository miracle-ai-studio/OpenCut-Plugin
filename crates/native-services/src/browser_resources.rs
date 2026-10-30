//! Fixed verified CAS descriptors shared by isolated audio and visual adapters.
use opencut_asset_store::{AssetStore, verification::FileIdentity};
use opencut_composition::scene::CasResource;
use opencut_contracts::{AssetVersion, EditorError};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
/// An already verified CAS descriptor. It owns no grant and never reopens a
/// mutable path for range reads; the host checks its live scope around each read.
pub struct VerifiedResourceFile {
    file: File,
    size: u64,
    identity: FileIdentity,
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencut_contracts::{Asset, AssetKind};
    use std::{cell::Cell, rc::Rc};

    fn pin(root: &Path, name: &str, bytes: &[u8]) -> AssetVersion {
        let path = root.parent().unwrap().join(name);
        std::fs::write(&path, bytes).unwrap();
        AssetStore::open(root)
            .unwrap()
            .pin_asset(&Asset {
                id: name.into(),
                name: name.into(),
                kind: AssetKind::Image,
                uri: path.to_string_lossy().into_owned(),
                duration: None,
                version: None,
                metadata: Default::default(),
            })
            .unwrap()
            .version
            .unwrap()
    }

    fn frame<'a>(
        root: &Path,
        versions: impl IntoIterator<Item = (&'a str, &'a AssetVersion)>,
        allowed: Rc<Cell<bool>>,
        cache: &mut VerifiedMediaFiles,
    ) -> VerifiedBrowserResources<Box<dyn FnMut() -> Result<(), EditorError>>> {
        let checkpoint: Box<dyn FnMut() -> Result<(), EditorError>> = Box::new(move || {
            if allowed.get() {
                Ok(())
            } else {
                Err(EditorError::new("NOT_AUTHORIZED", "Revoked test authority"))
            }
        });
        VerifiedBrowserResources::new_versions_cached(
            root,
            versions,
            checkpoint,
            "INVALID_VISUAL_PLAN",
            cache,
        )
        .unwrap()
    }

    fn open<F: FnMut() -> Result<(), EditorError>>(
        resources: &mut VerifiedBrowserResources<F>,
        id: &str,
        version: &AssetVersion,
    ) -> (String, String) {
        let handle = resources
            .handles()
            .find(|(candidate, _)| *candidate == id)
            .unwrap()
            .1
            .to_owned();
        let (transfer, size) = resources
            .resolve(id, &version.content_hash, version.size, &handle)
            .unwrap();
        assert_eq!(size, version.size);
        assert_ne!(transfer, handle);
        (handle, transfer)
    }

    #[test]
    fn immutable_font_proof_owns_bytes_across_disk_replacement_but_never_grants_access() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("cas");
        let bytes = b"immutable font fixture";
        let version = pin(&root, "font", bytes);
        let allowed = Rc::new(Cell::new(true));
        let mut cache = VerifiedMediaFiles::default();
        let mut first = frame(&root, [("font:a", &version)], allowed.clone(), &mut cache);
        assert!(
            first
                .retain_font(
                    "font:a",
                    &version,
                    Arc::from(&b"corrupt font fixture"[..]),
                    &mut cache
                )
                .is_err()
        );
        first
            .retain_font("font:a", &version, Arc::from(&bytes[..]), &mut cache)
            .unwrap();
        let (_, transfer) = open(&mut first, "font:a", &version);
        let path = root
            .join(&version.content_hash[..2])
            .join(&version.content_hash);
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, vec![b'X'; bytes.len()]).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let mut next = frame(&root, [("font:a", &version)], allowed.clone(), &mut cache);
        assert_eq!(&*next.immutable_font("font:a").unwrap().unwrap(), bytes);
        assert_eq!(
            next.read(&transfer, 0, 65536).unwrap(),
            (bytes.to_vec(), true)
        );
        allowed.set(false);
        assert_eq!(
            next.immutable_font("font:a").unwrap_err().code,
            "NOT_AUTHORIZED"
        );
        assert_eq!(
            next.read(&transfer, 0, 1).unwrap_err().code,
            "NOT_AUTHORIZED"
        );
        allowed.set(true);
        let other = pin(&root, "other", b"different font");
        let mut replaced = frame(&root, [("font:a", &other)], allowed.clone(), &mut cache);
        assert!(replaced.immutable_font("font:a").unwrap().is_none());
        assert!(replaced.read(&transfer, 0, 1).is_err());
        assert!(cache.0.is_empty());
    }
    #[test]
    fn preview_binary_ranges_are_bounded_without_widening_regular_callbacks() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cas");
        let bytes: Vec<u8> = (0..512 * 1024 + 7).map(|i| (i % 251) as u8).collect();
        let version = pin(&root, "range.bin", &bytes);
        let mut file = VerifiedResourceFile::open(&root, &version, &mut || Ok(())).unwrap();
        assert_eq!(
            file.read(0, 65537).unwrap_err().code,
            "INVALID_RESOURCE_RANGE"
        );
        assert_eq!(
            file.read_preview_range(0, 512 * 1024).unwrap(),
            (bytes[..512 * 1024].to_vec(), false)
        );
        assert_eq!(
            file.read_preview_range(512 * 1024, 7).unwrap(),
            (bytes[512 * 1024..].to_vec(), true)
        );
        assert_eq!(
            file.read_preview_range(0, 512 * 1024 + 1).unwrap_err().code,
            "INVALID_RESOURCE_RANGE"
        );
    }
    #[test]
    fn continuous_frames_keep_reads_but_retirement_only_acknowledges_close() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("cas");
        let a = pin(&root, "a", b"first synthetic clip");
        let b = pin(&root, "b", b"second synthetic clip");
        let allowed = Rc::new(Cell::new(true));
        let mut cache = VerifiedMediaFiles::default();
        let mut first = frame(&root, [("media:a", &a)], allowed.clone(), &mut cache);
        let (handle, transfer) = open(&mut first, "media:a", &a);
        assert_eq!(
            first.read(&transfer, 0, 65536).unwrap().0,
            b"first synthetic clip"
        );
        let mut continued = frame(&root, [("media:a", &a)], allowed.clone(), &mut cache);
        assert_eq!(continued.handles().next().unwrap().1, handle);
        assert_eq!(continued.read(&transfer, 6, 9).unwrap().0, b"synthetic");

        let mut next = frame(&root, [("media:b", &b)], allowed.clone(), &mut cache);
        assert_eq!(next.retired_releases.len(), 1);
        assert!(next.read(&transfer, 0, 1).is_err());
        assert!(continued.read(&transfer, 0, 1).is_err());
        assert!(
            next.resolve("media:a", &a.content_hash, a.size, &handle)
                .is_err()
        );
        allowed.set(false);
        assert_eq!(next.release(&transfer).unwrap_err().code, "NOT_AUTHORIZED");
        allowed.set(true);
        next.release(&transfer).unwrap();
        assert!(next.release(&transfer).is_err());
        assert!(next.release("unknown").is_err());
        let (_, second) = open(&mut next, "media:b", &b);
        assert_eq!(
            next.read(&second, 0, 65536).unwrap().0,
            b"second synthetic clip"
        );
        assert_eq!(cache.0.len(), 1);
    }

    #[test]
    fn stale_close_cannot_release_a_reopened_transfer() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("cas");
        let a = pin(&root, "a", b"fixed synthetic bytes");
        let allowed = Rc::new(Cell::new(true));
        let mut cache = VerifiedMediaFiles::default();
        let mut resources = frame(&root, [("media:a", &a)], allowed, &mut cache);
        let (handle, first) = open(&mut resources, "media:a", &a);
        resources.release(&first).unwrap();
        let (same_handle, second) = open(&mut resources, "media:a", &a);
        assert_eq!(same_handle, handle);
        assert_ne!(first, second);
        assert!(resources.release(&first).is_err());
        assert!(resources.read(&first, 0, 1).is_err());
        assert_eq!(
            resources.read(&second, 0, 65536).unwrap().0,
            b"fixed synthetic bytes"
        );
    }

    #[test]
    fn replacing_version_retires_old_reads_and_cleanup_does_not_leak_to_later_frames() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("cas");
        let a = pin(&root, "a", b"old fixed version");
        let b = pin(&root, "b", b"new fixed version");
        let allowed = Rc::new(Cell::new(true));
        let mut cache = VerifiedMediaFiles::default();
        let mut first = frame(&root, [("media:a", &a)], allowed.clone(), &mut cache);
        let (_, old) = open(&mut first, "media:a", &a);
        let mut replacement = frame(&root, [("media:a", &b)], allowed.clone(), &mut cache);
        assert!(first.read(&old, 0, 1).is_err());
        let (_, current) = open(&mut replacement, "media:a", &b);
        replacement.release(&old).unwrap();
        assert_eq!(
            replacement.read(&current, 0, 65536).unwrap().0,
            b"new fixed version"
        );
        let mut later = frame(&root, [("media:a", &b)], allowed, &mut cache);
        assert!(later.retired_releases.is_empty());
        assert!(later.release(&old).is_err());
        assert_eq!(later.read(&current, 0, 3).unwrap().0, b"new");
    }
    #[cfg(unix)]
    #[test]
    fn restored_mtime_cannot_hide_mutation_from_an_open_media_descriptor() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("cas");
        let version = pin(&root, "media", b"original source");
        let path = root
            .join(&version.content_hash[..2])
            .join(&version.content_hash);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        drop(VerifiedResourceFile::open(&root, &version, &mut || Ok(())).unwrap());
        let mut file = VerifiedResourceFile::open(&root, &version, &mut || Ok(())).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, b"modified source").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(file.read(0, 8).unwrap_err().code, "ASSET_INTEGRITY_ERROR");
        assert!(VerifiedResourceFile::open(&root, &version, &mut || Ok(())).is_err());
    }
}
impl VerifiedResourceFile {
    pub fn open(
        root: &Path,
        version: &AssetVersion,
        checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        checkpoint()?;
        let mut last_check = Instant::now();
        // Hashing is bounded I/O, not thousands of workspace reopen/SQL checks.
        // Cancellation/authority is checked during a long hash and at both ends.
        let (file, size, identity) =
            AssetStore::read_only(root)?.open_version_with_identity(version, &mut || {
                if last_check.elapsed() >= Duration::from_millis(50) {
                    checkpoint()?;
                    last_check = Instant::now();
                }
                Ok(())
            })?;
        checkpoint()?;
        let verified = Self {
            file,
            size,
            identity,
        };
        verified.check()?;
        Ok(verified)
    }

    fn changed() -> EditorError {
        EditorError::new(
            "ASSET_INTEGRITY_ERROR",
            "Verified descriptor changed or became unavailable",
        )
    }
    fn check(&self) -> Result<(), EditorError> {
        let metadata = self.file.metadata().map_err(|_| Self::changed())?;
        if FileIdentity::of(metadata)? != self.identity {
            return Err(Self::changed());
        }
        Ok(())
    }
    pub fn read(&mut self, offset: u64, count: u32) -> Result<(Vec<u8>, bool), EditorError> {
        self.read_limit(offset, count, 65536)
    }
    /// Foreground binary preview transport admits its own bounded buffers.
    /// Ordinary renderer/audio callbacks retain the original 64 KiB limit.
    pub fn read_preview_range(
        &mut self,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), EditorError> {
        self.read_limit(offset, count, 512 * 1024)
    }
    fn read_limit(
        &mut self,
        offset: u64,
        count: u32,
        maximum: u32,
    ) -> Result<(Vec<u8>, bool), EditorError> {
        if count == 0 || count > maximum || offset > self.size {
            return Err(EditorError::new(
                "INVALID_RESOURCE_RANGE",
                "Invalid bounded resource range",
            ));
        }
        self.check()?;
        let length = (self.size - offset).min(u64::from(count)) as usize;
        let mut bytes = vec![0; length];
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(&mut bytes))
            .map_err(|_| Self::changed())?;
        self.check()?;
        Ok((bytes, offset + length as u64 == self.size))
    }
}
#[derive(Clone)]
struct Descriptor {
    handle: String,
    data: ResourceData,
    size: u64,
    hash: String,
    active: Arc<Mutex<Option<String>>>,
}
#[derive(Clone)]
enum ResourceData {
    File(Arc<Mutex<VerifiedResourceFile>>),
    // Only complete, hash-verified owned bytes enter this variant. No mutable
    // file, mapped memory, or filesystem metadata can alter their identity.
    Immutable(Arc<[u8]>),
}
impl ResourceData {
    fn check(&self) -> Result<(), EditorError> {
        match self {
            Self::File(file) => file
                .lock()
                .map_err(|_| EditorError::new("INTERNAL_ERROR", "Verified resource lock failed"))?
                .check(),
            Self::Immutable(_) => Ok(()),
        }
    }
    fn read(&self, offset: u64, count: u32) -> Result<(Vec<u8>, bool), EditorError> {
        match self {
            Self::File(file) => file
                .lock()
                .map_err(|_| EditorError::new("INTERNAL_ERROR", "Verified resource lock failed"))?
                .read(offset, count),
            Self::Immutable(bytes) => {
                let at = usize::try_from(offset).map_err(|_| {
                    EditorError::new("INVALID_RESOURCE_RANGE", "Invalid byte offset")
                })?;
                if count == 0 || count > 65536 || at > bytes.len() {
                    return Err(EditorError::new(
                        "INVALID_RESOURCE_RANGE",
                        "Invalid immutable byte range",
                    ));
                }
                let end = at.saturating_add(count as usize).min(bytes.len());
                Ok((bytes[at..end].to_vec(), end == bytes.len()))
            }
        }
    }
}
/// Owned by one video attempt, never shared across revisions, tasks or callers.
/// Keeps media descriptors and at most 32MiB of immutable font bytes; no grants.
#[derive(Default)]
pub(crate) struct VerifiedMediaFiles(BTreeMap<String, (AssetVersion, Descriptor)>);

pub(crate) struct VerifiedBrowserResources<F> {
    _store: AssetStore,
    files: BTreeMap<String, Descriptor>,
    // Close acknowledgements for transfers retired by this frame transition.
    // These tokens never authorize reads or opening a resource in the new frame.
    retired_releases: BTreeSet<String>,
    checkpoint: F,
    invalid_code: &'static str,
}
impl<F: FnMut() -> Result<(), EditorError>> VerifiedBrowserResources<F> {
    pub(crate) fn new<'a>(
        root: &Path,
        resources: impl IntoIterator<Item = &'a CasResource>,
        checkpoint: F,
        invalid_code: &'static str,
    ) -> Result<Self, EditorError> {
        Self::new_versions(
            root,
            resources
                .into_iter()
                .map(|resource| (resource.asset_id.as_str(), &resource.version)),
            checkpoint,
            invalid_code,
        )
    }
    /// The host supplies already authorized fixed identities. A font uses its
    /// AssetVersion directly; media kind or source URI is not an access grant.
    pub(crate) fn new_versions<'a, I: Into<String>>(
        root: &Path,
        resources: impl IntoIterator<Item = (I, &'a AssetVersion)>,
        checkpoint: F,
        invalid_code: &'static str,
    ) -> Result<Self, EditorError> {
        Self::new_versions_inner(root, resources, checkpoint, invalid_code, None)
    }
    pub(crate) fn new_versions_cached<'a, I: Into<String>>(
        root: &Path,
        resources: impl IntoIterator<Item = (I, &'a AssetVersion)>,
        checkpoint: F,
        invalid_code: &'static str,
        retained: &mut VerifiedMediaFiles,
    ) -> Result<Self, EditorError> {
        Self::new_versions_inner(root, resources, checkpoint, invalid_code, Some(retained))
    }
    fn new_versions_inner<'a, I: Into<String>>(
        root: &Path,
        resources: impl IntoIterator<Item = (I, &'a AssetVersion)>,
        mut checkpoint: F,
        invalid_code: &'static str,
        mut retained: Option<&mut VerifiedMediaFiles>,
    ) -> Result<Self, EditorError> {
        checkpoint()?;
        let store = AssetStore::read_only(root)?;
        let versions = resources
            .into_iter()
            .map(|(id, version)| (id.into(), version))
            .collect::<BTreeMap<String, _>>();
        if versions.len() > 32 {
            return Err(EditorError::new(
                invalid_code,
                "Fixed resource set exceeds admission",
            ));
        }
        let mut retired_releases = BTreeSet::new();
        if let Some(retained) = retained.as_mut() {
            let retired: Vec<_> = retained
                .0
                .iter()
                .filter(|(id, (version, _))| {
                    versions.get(*id).is_none_or(|current| *current != version)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in retired {
                let (_, descriptor) = retained.0.remove(&id).expect("selected cached descriptor");
                if let Some(transfer) = descriptor
                    .active
                    .lock()
                    .map_err(|_| {
                        EditorError::new("INTERNAL_ERROR", "Verified transfer lock failed")
                    })?
                    .take()
                {
                    retired_releases.insert(transfer);
                }
            }
        }
        let mut files = BTreeMap::new();
        for (id, version) in versions {
            checkpoint()?;
            let prior = retained
                .as_ref()
                .and_then(|cache| cache.0.get(&id))
                .map(|(_, descriptor)| descriptor.clone());
            let descriptor = if let Some(descriptor) = prior {
                descriptor.data.check()?;
                descriptor
            } else {
                let file = VerifiedResourceFile::open(root, version, &mut checkpoint)?;
                Descriptor {
                    // Stable while retained, and separate from each open transfer.
                    handle: format!("resource-{}", uuid::Uuid::new_v4()),
                    data: ResourceData::File(Arc::new(Mutex::new(file))),
                    size: version.size,
                    hash: version.content_hash.clone(),
                    active: Arc::new(Mutex::new(None)),
                }
            };
            checkpoint()?;
            // Fonts retain their original fresh byte/native proof on each frame.
            if id.starts_with("media:") {
                if let Some(cache) = retained.as_mut() {
                    cache
                        .0
                        .insert(id.clone(), (version.clone(), descriptor.clone()));
                }
            }
            files.insert(id, descriptor);
        }
        Ok(Self {
            _store: store,
            files,
            retired_releases,
            checkpoint,
            invalid_code,
        })
    }
    pub(crate) fn immutable_font(&mut self, id: &str) -> Result<Option<Arc<[u8]>>, EditorError> {
        self.checkpoint()?;
        Ok(self.files.get(id).and_then(|d| match &d.data {
            ResourceData::Immutable(bytes) => Some(bytes.clone()),
            _ => None,
        }))
    }
    pub(crate) fn retain_font(
        &mut self,
        id: &str,
        version: &AssetVersion,
        bytes: Arc<[u8]>,
        retained: &mut VerifiedMediaFiles,
    ) -> Result<(), EditorError> {
        use sha2::{Digest, Sha256};
        self.checkpoint()?;
        let d = self
            .files
            .get_mut(id)
            .ok_or_else(|| EditorError::new("NOT_AUTHORIZED", "Unknown font descriptor"))?;
        if !id.starts_with("font:")
            || d.hash != version.content_hash
            || d.size != version.size
            || bytes.len() as u64 != d.size
            || format!("{:x}", Sha256::digest(&bytes)) != d.hash
        {
            return Err(EditorError::new(
                "ASSET_INTEGRITY_ERROR",
                "Immutable font proof differs from captured bytes",
            ));
        }
        let total: usize = retained
            .0
            .iter()
            .filter(|(key, _)| key.as_str() != id)
            .map(|(_, (_, d))| match &d.data {
                ResourceData::Immutable(b) => b.len(),
                _ => 0,
            })
            .sum();
        // Oversized/aggregate-overflow fonts keep the original verified file
        // path. Caching never changes which resources can execute.
        if bytes.len() <= 32 * 1024 * 1024 - total.min(32 * 1024 * 1024) {
            d.data = ResourceData::Immutable(bytes);
            retained.0.insert(id.into(), (version.clone(), d.clone()));
        }
        self.checkpoint()?;
        Ok(())
    }
    pub(crate) fn handles(&self) -> impl Iterator<Item = (&str, &str)> {
        self.files
            .iter()
            .map(|(id, d)| (id.as_str(), d.handle.as_str()))
    }
    pub(crate) fn checkpoint(&mut self) -> Result<(), EditorError> {
        (self.checkpoint)()
    }
    pub(crate) fn resolve(
        &mut self,
        id: &str,
        hash: &str,
        size: u64,
        handle: &str,
    ) -> Result<(String, u64), EditorError> {
        self.checkpoint()?;
        let descriptor = self.files.get_mut(id).ok_or_else(|| {
            EditorError::new("NOT_AUTHORIZED", "Resource is outside fixed execution")
        })?;
        let mut active = descriptor
            .active
            .lock()
            .map_err(|_| EditorError::new("INTERNAL_ERROR", "Verified transfer lock failed"))?;
        if descriptor.handle != handle
            || descriptor.size != size
            || descriptor.hash != hash
            || active.is_some()
        {
            return Err(EditorError::new(
                self.invalid_code,
                "Resource handle is invalid or already active",
            ));
        }
        let transfer = format!("transfer-{}", uuid::Uuid::new_v4());
        *active = Some(transfer.clone());
        Ok((transfer, size))
    }
    pub(crate) fn read(
        &mut self,
        transfer: &str,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), EditorError> {
        self.checkpoint()?;
        let mut selected = None;
        for descriptor in self.files.values_mut() {
            let matches = descriptor
                .active
                .lock()
                .map_err(|_| EditorError::new("INTERNAL_ERROR", "Verified transfer lock failed"))?
                .as_deref()
                == Some(transfer);
            if matches {
                selected = Some(descriptor);
                break;
            }
        }
        let d = selected.ok_or_else(|| {
            EditorError::new("NOT_AUTHORIZED", "Unknown or released resource transfer")
        })?;
        let (bytes, eof) = d.data.read(offset, count)?;
        self.checkpoint()?;
        Ok((bytes, eof))
    }
    pub(crate) fn release(&mut self, transfer: &str) -> Result<(), EditorError> {
        self.checkpoint()?;
        for descriptor in self.files.values_mut() {
            let mut active = descriptor
                .active
                .lock()
                .map_err(|_| EditorError::new("INTERNAL_ERROR", "Verified transfer lock failed"))?;
            if active.as_deref() == Some(transfer) {
                *active = None;
                return Ok(());
            }
        }
        if self.retired_releases.remove(transfer) {
            return Ok(());
        }
        Err(EditorError::new(
            self.invalid_code,
            "Unknown or released resource transfer",
        ))
    }
}
