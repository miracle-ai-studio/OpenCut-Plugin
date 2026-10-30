//! Session-owned verified artifact descriptors. No authorization is retained;
//! every byte read crosses the current preview and publication checkpoints.
use super::*;
use opencut_editor_application::{
    classic_text_frame::{ClassicTextFrameResources, TextFontResource},
    classic_visual::*,
    source_preview::{self as source, ArtifactDescriptor, CapturedSourcePreviewJob},
};
use opencut_native_services::{artifacts::ArtifactSource, source_preview::SourcePreviewIndex};
use opencut_sqlite_store::{EditorApplication, JobState};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::Metadata,
    io::{self, Read, Seek, SeekFrom},
};

#[derive(Clone, PartialEq, Eq)]
struct FileIdentity(Vec<i128>);
fn file_identity(metadata: Metadata) -> Result<FileIdentity, EditorError> {
    if !metadata.is_file() {
        return Err(integrity());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(integrity());
        }
        Ok(FileIdentity(vec![
            metadata.dev() as i128,
            metadata.ino() as i128,
            metadata.len() as i128,
            metadata.mtime() as i128,
            metadata.mtime_nsec() as i128,
            metadata.ctime() as i128,
            metadata.ctime_nsec() as i128,
        ]))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(integrity())
    }
}
fn integrity() -> EditorError {
    EditorError::new(
        "ARTIFACT_VERIFICATION_FAILED",
        "Prepared source artifact changed or is unavailable",
    )
}
// Only immutable verification facts are shared, never authorization, handles,
// pixels or a renderer. Opening still acquires the current attempt lease and
// validates the published path. Each read checks the opened inode again.
#[derive(Clone, PartialEq, Eq)]
struct VerifiedFile {
    root: PathBuf,
    job: String,
    attempt: u32,
    descriptor: ArtifactDescriptor,
    identity: FileIdentity,
}
const MAX_VERIFIED_FILES: usize = 128;
#[derive(Default)]
struct ArtifactVerificationCache(VecDeque<VerifiedFile>);
impl ArtifactVerificationCache {
    fn contains(&mut self, file: &VerifiedFile) -> bool {
        let Some(index) = self.0.iter().position(|entry| entry == file) else {
            return false;
        };
        let entry = self.0.remove(index).expect("located verification fact");
        self.0.push_back(entry);
        true
    }
    fn remember(&mut self, file: VerifiedFile) {
        if !self.contains(&file) {
            if self.0.len() >= MAX_VERIFIED_FILES {
                self.0.pop_front();
            }
            self.0.push_back(file);
        }
    }
}
struct VerifiedArtifact {
    file: ArtifactSource,
    identity: FileIdentity,
    size: u64,
}
impl VerifiedArtifact {
    fn open(
        root: &Path,
        job: &str,
        attempt: u32,
        descriptor: &ArtifactDescriptor,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        static VERIFIED: OnceLock<Mutex<ArtifactVerificationCache>> = OnceLock::new();
        Self::open_with_cache(
            root,
            job,
            attempt,
            descriptor,
            checkpoint,
            VERIFIED.get_or_init(Mutex::default),
        )
    }
    fn open_with_cache(
        root: &Path,
        job: &str,
        attempt: u32,
        descriptor: &ArtifactDescriptor,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
        cache: &Mutex<ArtifactVerificationCache>,
    ) -> Result<Self, EditorError> {
        checkpoint()?;
        let file = ArtifactSource::open(root, job, attempt, Path::new(&descriptor.path))?;
        let metadata = file.metadata().map_err(|_| integrity())?;
        if metadata.len() != descriptor.size {
            return Err(integrity());
        }
        let mut verified = Self {
            identity: file_identity(metadata)?,
            file,
            size: descriptor.size,
        };
        let observation = VerifiedFile {
            root: root.to_path_buf(),
            job: job.into(),
            attempt,
            descriptor: descriptor.clone(),
            identity: verified.identity.clone(),
        };
        let known = cache
            .lock()
            .map_err(|_| integrity())?
            .contains(&observation);
        if known {
            verified.check()?;
            checkpoint()?;
            return Ok(verified);
        }
        let mut digest = Sha256::new();
        let mut bytes = [0; 65536];
        loop {
            checkpoint()?;
            let count = verified.read(&mut bytes).map_err(|_| integrity())?;
            if count == 0 {
                break;
            }
            digest.update(&bytes[..count]);
            checkpoint()?;
        }
        if format!("{:x}", digest.finalize()) != descriptor.content_hash {
            return Err(integrity());
        }
        verified.seek(SeekFrom::Start(0)).map_err(|_| integrity())?;
        checkpoint()?;
        cache.lock().map_err(|_| integrity())?.remember(observation);
        Ok(verified)
    }
    fn check(&self) -> Result<(), EditorError> {
        if file_identity(self.file.metadata().map_err(|_| integrity())?)? != self.identity {
            return Err(integrity());
        }
        Ok(())
    }
    fn read_range(&mut self, offset: u64, count: u32) -> Result<(Vec<u8>, bool), EditorError> {
        if offset > self.size || count == 0 || count > 65536 {
            return Err(integrity());
        }
        self.seek(SeekFrom::Start(offset))
            .map_err(|_| integrity())?;
        let mut bytes = vec![0; u64::from(count).min(self.size - offset) as usize];
        self.read_exact(&mut bytes).map_err(|_| integrity())?;
        let eof = offset + bytes.len() as u64 == self.size;
        Ok((bytes, eof))
    }
}
impl Read for VerifiedArtifact {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check().map_err(|e| io::Error::other(e.message))?;
        let count = self.file.read(bytes)?;
        self.check().map_err(|e| io::Error::other(e.message))?;
        Ok(count)
    }
}
impl Seek for VerifiedArtifact {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check().map_err(|e| io::Error::other(e.message))?;
        self.file.seek(position)
    }
}
struct Cached {
    captured: CapturedSourcePreviewJob,
    video: Option<VerifiedArtifact>,
    index: Option<SourcePreviewIndex<VerifiedArtifact>>,
    touched: u64,
}
#[derive(Default)]
pub(crate) struct SourceCache {
    entries: BTreeMap<String, Cached>,
    touch: u64,
}

pub(crate) struct PreviewResources<'a, R> {
    pub inner: R,
    pub app: &'a EditorApplication,
    pub root: PathBuf,
    pub plan: &'a SourcePlan,
    pub cache: &'a mut SourceCache,
    transfers: BTreeMap<String, String>,
    next: u64,
}
impl<'a, R> PreviewResources<'a, R> {
    pub fn new(
        inner: R,
        app: &'a EditorApplication,
        root: PathBuf,
        plan: &'a SourcePlan,
        cache: &'a mut SourceCache,
    ) -> Self {
        Self {
            inner,
            app,
            root,
            plan,
            cache,
            transfers: BTreeMap::new(),
            next: 0,
        }
    }
}
impl<R: ClassicVisualResources> ClassicVisualResources for PreviewResources<'_, R> {
    fn checkpoint(&mut self) -> Result<(), EditorError> {
        self.inner.checkpoint()
    }
    fn resolve(
        &mut self,
        resource: &VisualResource,
        handle: &str,
    ) -> Result<(String, u64), EditorError> {
        self.inner.resolve(resource, handle)
    }
    fn preview_source(
        &mut self,
        request: &VisualPreviewSourceRequest,
    ) -> Result<VisualPreviewSourceResolution, EditorError> {
        self.inner.checkpoint()?;
        let Some(job) = self.plan.jobs.iter().find(|job| {
            job.identity.project_id == request.project_id
                && job.identity.asset_id == request.asset_id
                && job.identity.source_version == request.source_version
                && job.identity.selected_video_stream == request.selected_video_stream
                && job.identity.raster_bound == request.raster_bound
        }) else {
            return Ok(VisualPreviewSourceResolution::original(
                request,
                SourcePreviewState::NotRequested,
                None,
            ));
        };
        self.cache.touch = self.cache.touch.checked_add(1).ok_or_else(|| {
            EditorError::new(
                "PREVIEW_RESOURCE_LIMIT",
                "Source cache touch counter overflow",
            )
        })?;
        if !self.cache.entries.contains_key(&job.job_id) {
            if self.cache.entries.len() >= 32 {
                let oldest = self
                    .cache
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.touched)
                    .map(|(id, _)| id.clone())
                    .ok_or_else(integrity)?;
                self.cache.entries.remove(&oldest);
            }
            let captured = self.app.capture_source_preview_job(&job.job_id)?;
            if captured.prepared.identity != job.identity {
                return Err(integrity());
            }
            self.cache.entries.insert(
                job.job_id.clone(),
                Cached {
                    captured,
                    video: None,
                    index: None,
                    touched: self.cache.touch,
                },
            );
        }
        let cached = self
            .cache
            .entries
            .get_mut(&job.job_id)
            .expect("captured source preview");
        cached.touched = self.cache.touch;
        let (state, attempt, cancelled) =
            self.app.source_preview_job_status(&mut cached.captured)?;
        if state != JobState::Succeeded {
            let state = if cancelled {
                SourcePreviewState::Cancelled
            } else {
                match state {
                    JobState::Queued => SourcePreviewState::Queued,
                    JobState::Running => SourcePreviewState::Running,
                    JobState::Failed => SourcePreviewState::Failed,
                    JobState::Cancelled => SourcePreviewState::Cancelled,
                    JobState::Succeeded => unreachable!(),
                }
            };
            self.inner.checkpoint()?;
            return Ok(VisualPreviewSourceResolution::original(
                request,
                state,
                Some(job.job_id.clone()),
            ));
        }
        self.app.require_source_preview_read(&cached.captured)?;
        let result = cached.captured.result.as_ref().ok_or_else(integrity)?;
        if cached.video.is_none() || cached.index.is_none() {
            let mut checkpoint = || {
                self.inner.checkpoint()?;
                self.app.require_source_preview_read(&cached.captured)
            };
            let video = VerifiedArtifact::open(
                &self.root,
                &job.job_id,
                attempt,
                &result.artifact,
                &mut checkpoint,
            )?;
            let index = VerifiedArtifact::open(
                &self.root,
                &job.job_id,
                attempt,
                &result.frame_index,
                &mut checkpoint,
            )?;
            let count = result.public.frame_count.parse().map_err(|_| integrity())?;
            let index = SourcePreviewIndex::new(index, count, result.public.identity.raster_bound)?;
            cached.video = Some(video);
            cached.index = Some(index);
        }
        let frame = source::select_frame(
            cached.index.as_mut().ok_or_else(integrity)?,
            &result.public,
            &request.requested_time,
        )?;
        self.app.require_source_preview_read(&cached.captured)?;
        self.inner.checkpoint()?;
        let transfer = if frame.is_some() {
            self.next = self.next.checked_add(1).ok_or_else(integrity)?;
            let id = format!("source-preview:{}:{}", self.next, job.job_id);
            self.transfers.insert(id.clone(), job.job_id.clone());
            Some(id)
        } else {
            None
        };
        let byte_length = transfer
            .as_ref()
            .map(|_| result.public.video.size_bytes.clone());
        Ok(VisualPreviewSourceResolution {
            source_preparation: SourcePreviewUse::Prepared {
                element_id: request.element_id.clone(),
                job_id: job.job_id.clone(),
                source: Box::new(result.public.clone()),
                frame,
            },
            transfer_id: transfer,
            byte_length,
        })
    }
    fn read(
        &mut self,
        transfer: &str,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), EditorError> {
        let Some(job) = self.transfers.get(transfer) else {
            return self.inner.read(transfer, offset, count);
        };
        self.inner.checkpoint()?;
        let cached = self.cache.entries.get_mut(job).ok_or_else(integrity)?;
        self.app.require_source_preview_read(&cached.captured)?;
        let result = cached
            .video
            .as_mut()
            .ok_or_else(integrity)?
            .read_range(offset, count)?;
        self.app.require_source_preview_read(&cached.captured)?;
        self.inner.checkpoint()?;
        Ok(result)
    }
    fn release(&mut self, transfer: &str) -> Result<(), EditorError> {
        if self.transfers.remove(transfer).is_some() {
            return self.inner.checkpoint();
        }
        self.inner.release(transfer)
    }
}
impl<R: ClassicTextFrameResources> ClassicTextFrameResources for PreviewResources<'_, R> {
    fn resolve_font(
        &mut self,
        font: &TextFontResource,
        handle: &str,
    ) -> Result<(String, u64), EditorError> {
        self.inner.resolve_font(font, handle)
    }
}

#[cfg(all(test, unix))]
mod verification_tests {
    use super::*;
    use std::fs;

    struct Fixture {
        _directory: tempfile::TempDir,
        root: PathBuf,
        descriptor: ArtifactDescriptor,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(directory.path()).unwrap();
            fs::create_dir_all(root.join("job_test/attempt-1")).unwrap();
            let path = root.join("job_test/attempt-1/source.mkv");
            let bytes = vec![17; 131_072];
            fs::write(&path, &bytes).unwrap();
            Self {
                _directory: directory,
                root,
                descriptor: ArtifactDescriptor {
                    path: path.to_str().unwrap().into(),
                    mime_type: "video/x-matroska".into(),
                    size: bytes.len() as u64,
                    content_hash: format!("{:x}", Sha256::digest(&bytes)),
                },
            }
        }
        fn open(&self, cache: &Mutex<ArtifactVerificationCache>) -> (VerifiedArtifact, usize) {
            let mut checks = 0;
            let file = VerifiedArtifact::open_with_cache(
                &self.root,
                "job_test",
                1,
                &self.descriptor,
                &mut || {
                    checks += 1;
                    Ok(())
                },
                cache,
            )
            .unwrap();
            (file, checks)
        }
    }

    #[test]
    fn verified_bytes_survive_handle_retirement_but_authority_is_never_cached() {
        let fixture = Fixture::new();
        let cache = Mutex::new(ArtifactVerificationCache::default());
        let (file, full_checks) = fixture.open(&cache);
        assert!(full_checks > 2);
        drop(file);
        let (mut reopened, reused_checks) = fixture.open(&cache);
        assert_eq!(reused_checks, 2);
        assert_eq!(reopened.read_range(65_536, 8).unwrap().0, vec![17; 8]);
        drop(reopened);
        for deny_at in [1, 2] {
            let mut checks = 0;
            let error = VerifiedArtifact::open_with_cache(
                &fixture.root,
                "job_test",
                1,
                &fixture.descriptor,
                &mut || {
                    checks += 1;
                    if checks == deny_at {
                        Err(EditorError::new("FORBIDDEN", "revoked"))
                    } else {
                        Ok(())
                    }
                },
                &cache,
            )
            .err()
            .unwrap();
            assert_eq!(error.code, "FORBIDDEN");
        }
    }

    #[test]
    fn same_length_mutation_with_restored_mtime_invalidates_verification() {
        let fixture = Fixture::new();
        let cache = Mutex::new(ArtifactVerificationCache::default());
        drop(fixture.open(&cache).0);
        let before = fs::metadata(&fixture.descriptor.path)
            .unwrap()
            .modified()
            .unwrap();
        fs::write(&fixture.descriptor.path, vec![18; 131_072]).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&fixture.descriptor.path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(before))
            .unwrap();
        let error = VerifiedArtifact::open_with_cache(
            &fixture.root,
            "job_test",
            1,
            &fixture.descriptor,
            &mut || Ok(()),
            &cache,
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "ARTIFACT_VERIFICATION_FAILED");
        fs::write(&fixture.descriptor.path, vec![17; 131_072]).unwrap();
        let (file, checks) = fixture.open(&cache);
        assert!(checks > 2);
        drop(file);
        assert_eq!(fixture.open(&cache).1, 2);
    }

    #[test]
    fn replacement_symlink_and_missing_artifacts_cannot_use_cached_facts() {
        let fixture = Fixture::new();
        let cache = Mutex::new(ArtifactVerificationCache::default());
        drop(fixture.open(&cache).0);
        let path = Path::new(&fixture.descriptor.path);
        let original = path.with_extension("old");
        fs::rename(path, &original).unwrap();
        fs::write(path, vec![18; 131_072]).unwrap();
        for mode in ["replacement", "symlink", "missing"] {
            if mode != "replacement" {
                fs::remove_file(path).unwrap();
                if mode == "symlink" {
                    std::os::unix::fs::symlink(&original, path).unwrap();
                }
            }
            assert_eq!(
                VerifiedArtifact::open_with_cache(
                    &fixture.root,
                    "job_test",
                    1,
                    &fixture.descriptor,
                    &mut || Ok(()),
                    &cache,
                )
                .err()
                .unwrap()
                .code,
                "ARTIFACT_VERIFICATION_FAILED"
            );
        }
    }

    #[test]
    fn changed_publication_digest_and_mutation_after_open_are_rejected() {
        let fixture = Fixture::new();
        let cache = Mutex::new(ArtifactVerificationCache::default());
        drop(fixture.open(&cache).0);
        let mut other = fixture.descriptor.clone();
        other.content_hash = "0".repeat(64);
        assert!(
            VerifiedArtifact::open_with_cache(
                &fixture.root,
                "job_test",
                1,
                &other,
                &mut || Ok(()),
                &cache,
            )
            .is_err()
        );
        let (mut file, checks) = fixture.open(&cache);
        assert_eq!(checks, 2);
        fs::write(&fixture.descriptor.path, vec![18; 131_072]).unwrap();
        assert!(file.read_range(0, 8).is_err());
    }

    #[test]
    fn verification_facts_are_bounded_and_evicted_files_are_reverified() {
        let fixture = Fixture::new();
        let cache = Mutex::new(ArtifactVerificationCache::default());
        drop(fixture.open(&cache).0);
        for index in 0..MAX_VERIFIED_FILES {
            let mut descriptor = fixture.descriptor.clone();
            descriptor.path = fixture
                .root
                .join(format!("job_test/attempt-1/{index}.mkv"))
                .to_str()
                .unwrap()
                .into();
            fs::write(&descriptor.path, vec![17; 131_072]).unwrap();
            drop(
                VerifiedArtifact::open_with_cache(
                    &fixture.root,
                    "job_test",
                    1,
                    &descriptor,
                    &mut || Ok(()),
                    &cache,
                )
                .unwrap(),
            );
        }
        assert_eq!(cache.lock().unwrap().0.len(), MAX_VERIFIED_FILES);
        assert!(fixture.open(&cache).1 > 2);
    }
}
