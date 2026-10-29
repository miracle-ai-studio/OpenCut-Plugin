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
    collections::BTreeMap,
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
