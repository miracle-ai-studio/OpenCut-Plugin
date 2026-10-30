//! Source-only preparation and exact frame selection over application ports.
//! No file, decoder, encoder, process or client-side timeline calculation.
use crate::{JobRecord, JobRequest};
pub use opencut_contracts::source_preview::*;
use opencut_contracts::{
    AssetKind, EditorError, Time,
    scene::{EditorSnapshot, ElementContent},
};
pub use opencut_job_contracts::ArtifactDescriptor;
pub use opencut_job_execution::scene_audio::TrustedJobAuthority;
pub use opencut_job_execution::source_preview::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const KIND: &str = opencut_job_contracts::source_preview::KIND;
pub const INDEX_HEADER_BYTES: u64 = 16;
pub const INDEX_RECORD_BYTES: u64 = 64;
pub const MAX_SOURCE_FRAMES: u64 = 180_000;
#[cfg(test)]
mod tests;

pub struct AuthorizedSourcePreview {
    pub snapshot: EditorSnapshot,
    pub authority: TrustedJobAuthority,
    pub processing_policy: SourcePreviewPolicy,
}

pub trait SourcePreviewSubmissionStore {
    fn fixed_source_preview_snapshot(
        &self,
        project: &str,
        revision: &str,
    ) -> Result<AuthorizedSourcePreview, EditorError>;
    fn find_source_preview(&self, request: &JobRequest) -> Result<Option<JobRecord>, EditorError>;
    /// Revalidate current source/snapshot/authority and request in one acceptance
    /// transaction. A matching key never grants access to original or proxy.
    fn accept_source_preview(
        &mut self,
        request: JobRequest,
        prepared: PreparedSourcePreview,
    ) -> Result<JobRecord, EditorError>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSourcePreviewResult {
    pub public: SourcePreviewResult,
    pub artifact: ArtifactDescriptor,
    pub frame_index: ArtifactDescriptor,
}

/// Immutable captured facts, never a successful access decision. Native reads
/// compare the complete input/result TEXT under fresh publication fences.
pub struct CapturedSourcePreviewJob {
    pub job_id: String,
    pub prepared: PreparedSourcePreview,
    pub input_text: String,
    pub result_text: Option<String>,
    pub result: Option<StoredSourcePreviewResult>,
    pub published_attempt: Option<u32>,
}
pub fn capture_job(job: &JobRecord) -> Result<CapturedSourcePreviewJob, EditorError> {
    if job.kind != KIND {
        return Err(stored("Task is not source preparation"));
    }
    let prepared: PreparedSourcePreview = serde_json::from_value(job.input.clone())
        .map_err(|_| stored("Invalid captured source input"))?;
    validate_prepared(&prepared)?;
    Ok(CapturedSourcePreviewJob {
        job_id: job.job_id.clone(),
        prepared,
        input_text: serde_json::to_string(&job.input)
            .map_err(|_| stored("Cannot retain source input"))?,
        result_text: None,
        result: None,
        published_attempt: None,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePreviewIndexRecord {
    pub source_pts: i64,
    pub source_duration: i64,
    pub proxy_pts: i64,
    pub proxy_duration: i64,
    pub source_video: SourcePreviewFrameVideo,
}

pub trait SourcePreviewFrameIndexRead {
    fn frame_count(&self) -> u64;
    fn frame(&mut self, ordinal: u64) -> Result<SourcePreviewIndexRecord, EditorError>;
}

pub trait SourcePreviewCheckpoint {
    fn checkpoint(&mut self, completed_work: bool) -> Result<(), EditorError>;
}
impl<F: FnMut(bool) -> Result<(), EditorError>> SourcePreviewCheckpoint for F {
    fn checkpoint(&mut self, completed_work: bool) -> Result<(), EditorError> {
        self(completed_work)
    }
}
pub trait SourcePreviewExecutor {
    fn execute(
        &self,
        prepared: &PreparedSourcePreview,
        checkpoint: &mut dyn SourcePreviewCheckpoint,
    ) -> Result<StoredSourcePreviewResult, EditorError>;
}
pub fn execute(
    executor: &dyn SourcePreviewExecutor,
    prepared: &PreparedSourcePreview,
    checkpoint: &mut dyn SourcePreviewCheckpoint,
) -> Result<StoredSourcePreviewResult, EditorError> {
    validate_prepared(prepared)?;
    checkpoint.checkpoint(false)?;
    let result = executor.execute(prepared, checkpoint)?;
    validate_result(prepared, &result)?;
    checkpoint.checkpoint(false)?;
    Ok(result)
}

fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", message)
}
fn stored(message: &str) -> EditorError {
    EditorError::new("INVALID_STORED_RESULT", message)
}
fn id(value: &str) -> bool {
    crate::media_frame_jobs::valid_id(value)
}
fn hash(value: &str) -> bool {
    crate::media_frame_jobs::valid_hash(value)
}
fn decimal(value: &str) -> Result<u64, EditorError> {
    crate::classic_audio::decimal(value)
}
fn signed(value: &str) -> Result<i64, EditorError> {
    let number = value
        .parse::<i64>()
        .map_err(|_| stored("Invalid source PTS"))?;
    if number.to_string() != value {
        return Err(stored("Noncanonical source PTS"));
    }
    Ok(number)
}
pub fn policy_fingerprint(policy: &SourcePreviewPolicy) -> Result<String, EditorError> {
    validate_policy(policy)?;
    let bytes = serde_json::to_vec(policy).map_err(|_| invalid("Cannot identify source policy"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub fn runtime_fingerprint(runtime: &SourcePreviewRuntimeIdentity) -> Result<String, EditorError> {
    let bytes = serde_json::to_vec(runtime)
        .map_err(|_| invalid("Cannot identify source preparation runtime"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub fn validate_policy(policy: &SourcePreviewPolicy) -> Result<(), EditorError> {
    if policy.max_source_pixels == 0
        || policy.max_source_pixels > 16_777_216
        || policy.max_decoded_bytes < 65536
        || policy.max_decoded_bytes > 1024 * 1024 * 1024
        || policy.max_source_rss_bytes < policy.max_decoded_bytes
        || policy.max_source_rss_bytes > 2 * 1024 * 1024 * 1024
        || policy.max_encoder_rss_bytes == 0
        || policy.max_encoder_rss_bytes > 1024 * 1024 * 1024
        || policy.max_output_bytes == 0
        || policy.max_output_bytes > 4 * 1024 * 1024 * 1024
        || policy.max_frames == 0
        || policy.max_frames > MAX_SOURCE_FRAMES
        || !(1..=4).contains(&policy.max_decode_threads)
        || !(1..=3_600_000).contains(&policy.deadline_ms)
    {
        return Err(invalid("Invalid captured source preparation admission"));
    }
    Ok(())
}
pub fn validate_raster(raster: SourcePreviewRaster) -> Result<(), EditorError> {
    if raster.width == 0
        || raster.height == 0
        || raster.width.max(raster.height) > 960
        || raster.width.min(raster.height) > 540
    {
        return Err(invalid(
            "Source preview requires the existing 540p raster bound",
        ));
    }
    Ok(())
}
fn validate_identity(identity: &SourcePreviewIdentity) -> Result<(), EditorError> {
    if !id(&identity.project_id)
        || !id(&identity.asset_id)
        || !id(&identity.source_version.id)
        || !hash(&identity.source_version.content_hash)
        || identity.source_version.size == 0
        || !hash(&identity.runtime_fingerprint)
        || !hash(&identity.policy_fingerprint)
    {
        return Err(invalid("Invalid immutable source preparation identity"));
    }
    validate_raster(identity.raster_bound)
}
pub fn request_for(prepared: &PreparedSourcePreview) -> Result<JobRequest, EditorError> {
    validate_prepared(prepared)?;
    request_identity(&prepared.identity, &prepared.authority)
}
pub fn request_identity(
    identity: &SourcePreviewIdentity,
    authority: &TrustedJobAuthority,
) -> Result<JobRequest, EditorError> {
    validate_identity(identity)?;
    crate::media_frame_jobs::validate_authority(authority)?;
    let bytes = serde_json::to_vec(&(identity, authority))
        .map_err(|_| invalid("Cannot identify source preparation"))?;
    Ok(JobRequest {
        kind: KIND.into(),
        idempotency_key: format!("source-preview:{:x}", Sha256::digest(bytes)),
        input: serde_json::to_value(identity)
            .map_err(|_| invalid("Cannot encode source declaration"))?,
    })
}

pub fn validate_resolution(
    request: &crate::classic_visual::VisualPreviewSourceRequest,
    resolution: &crate::classic_visual::VisualPreviewSourceResolution,
) -> Result<(), EditorError> {
    match &resolution.source_preparation {
        SourcePreviewUse::Original {
            element_id,
            asset_id,
            state,
            job_id,
        } => {
            if element_id != &request.element_id
                || asset_id != &request.asset_id
                || job_id
                    .as_ref()
                    .is_some_and(|id| !crate::job_views::valid_job_id(id))
                || (matches!(state, SourcePreviewState::NotRequested) != job_id.is_none())
                || resolution.transfer_id.is_some()
                || resolution.byte_length.is_some()
            {
                return Err(stored(
                    "Original preview source state changed its fixed layer",
                ));
            }
        }
        SourcePreviewUse::Prepared {
            element_id,
            job_id,
            source,
            frame,
        } => {
            validate_public_result(source)?;
            let identity = &source.identity;
            if element_id != &request.element_id
                || !crate::job_views::valid_job_id(job_id)
                || identity.project_id != request.project_id
                || identity.asset_id != request.asset_id
                || identity.source_version != request.source_version
                || identity.selected_video_stream != request.selected_video_stream
                || identity.raster_bound != request.raster_bound
            {
                return Err(stored(
                    "Prepared preview source changed original bytes/track/raster",
                ));
            }
            if let Some(frame) = frame {
                let ordinal = decimal(&frame.ordinal)?;
                let count = decimal(&source.frame_count)?;
                validate_index_record(
                    &SourcePreviewIndexRecord {
                        source_pts: signed(&frame.source_pts)?,
                        source_duration: signed(&frame.source_duration)?,
                        proxy_pts: signed(&frame.proxy_pts)?,
                        proxy_duration: signed(&frame.proxy_duration)?,
                        source_video: frame.source_video.clone(),
                    },
                    request.raster_bound,
                )?;
                let at = opencut_time::RationalTime::try_from(&request.requested_time)?;
                if ordinal >= count
                    || frame.last_frame != (ordinal + 1 == count)
                    || i128::from(signed(&frame.source_pts)?)
                        * i128::from(source.source_time_base.numerator)
                        * i128::from(at.timebase())
                        > i128::from(at.ticks()) * i128::from(source.source_time_base.denominator)
                    || (ordinal == 0 && frame.source_pts != source.first_source_pts)
                    || (frame.last_frame
                        && (frame.source_pts != source.last_source_pts
                            || frame.source_duration != source.last_source_duration))
                    || resolution
                        .transfer_id
                        .as_ref()
                        .is_none_or(|id| !crate::media_frame_jobs::valid_id(id))
                    || resolution.byte_length.as_ref() != Some(&source.video.size_bytes)
                {
                    return Err(stored(
                        "Prepared preview frame changed its exact source selection",
                    ));
                }
            } else {
                let at = opencut_time::RationalTime::try_from(&request.requested_time)?;
                if resolution.transfer_id.is_some()
                    || resolution.byte_length.is_some()
                    || i128::from(signed(&source.first_source_pts)?)
                        * i128::from(source.source_time_base.numerator)
                        * i128::from(at.timebase())
                        <= i128::from(at.ticks()) * i128::from(source.source_time_base.denominator)
                {
                    return Err(stored("Prepared preview gap changed original first PTS"));
                }
            }
        }
    }
    Ok(())
}
pub fn validate_prepared(prepared: &PreparedSourcePreview) -> Result<(), EditorError> {
    validate_policy(&prepared.processing_policy)?;
    validate_identity(&prepared.identity)?;
    crate::media_frame_jobs::validate_authority(&prepared.authority)?;
    let snapshot = &prepared.snapshot;
    if !id(&snapshot.revision)
        || snapshot.project.id != prepared.identity.project_id
        || policy_fingerprint(&prepared.processing_policy)? != prepared.identity.policy_fingerprint
    {
        return Err(invalid(
            "Source preparation differs from captured scope or policy",
        ));
    }
    let sequence = snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == prepared.sequence_id)
        .ok_or_else(|| invalid("Source preparation scene is missing"))?;
    let (format, _) = opencut_composition::scene::resolve_scene_format(&snapshot.project, sequence)
        .map_err(|e| EditorError::new(e.code, e.message))?;
    let raster = opencut_composition::scene::preview_canvas(format.canvas_size);
    if prepared.identity.raster_bound
        != (SourcePreviewRaster {
            width: raster.width,
            height: raster.height,
        })
    {
        return Err(invalid("Source preparation changed the fixed scene raster"));
    }
    let source = snapshot
        .project
        .assets
        .iter()
        .find(|a| a.id == prepared.identity.asset_id)
        .ok_or_else(|| invalid("Source preparation asset is missing"))?;
    if source.kind != AssetKind::Video || source.version != prepared.identity.source_version {
        return Err(invalid("Source preparation changed original video bytes"));
    }
    Ok(())
}
pub fn validate_request(
    request: &JobRequest,
    prepared: &PreparedSourcePreview,
) -> Result<(), EditorError> {
    if request_for(prepared)? != *request {
        return Err(invalid(
            "Source preparation differs from canonical accepted request",
        ));
    }
    Ok(())
}
pub fn prepare(
    store: &mut impl SourcePreviewSubmissionStore,
    assets: &impl crate::media_frame_jobs::MediaFrameAssetCapture,
    input: SourcePreviewPrepare,
    runtime_fingerprint: &str,
) -> Result<Vec<JobRecord>, EditorError> {
    validate_submission(&input)?;
    if !hash(runtime_fingerprint) {
        return Err(invalid("Invalid source preparation runtime fingerprint"));
    }
    prepare_validated(store, assets, input, runtime_fingerprint)
}
pub fn validate_submission(input: &SourcePreviewPrepare) -> Result<(), EditorError> {
    if !id(&input.project_id)
        || !id(&input.revision)
        || !id(&input.sequence_id)
        || input.asset_id.as_ref().is_some_and(|a| !id(a))
        || input
            .selected_video_stream
            .is_some_and(|stream| stream > 63)
        || (input.asset_id.is_none() && input.selected_video_stream.is_some())
    {
        return Err(invalid("Invalid fixed source preparation request"));
    }
    Ok(())
}
fn prepare_validated(
    store: &mut impl SourcePreviewSubmissionStore,
    assets: &impl crate::media_frame_jobs::MediaFrameAssetCapture,
    input: SourcePreviewPrepare,
    runtime_fingerprint: &str,
) -> Result<Vec<JobRecord>, EditorError> {
    let fixed = store.fixed_source_preview_snapshot(&input.project_id, &input.revision)?;
    if fixed.snapshot.project.id != input.project_id || fixed.snapshot.revision != input.revision {
        return Err(invalid("Authorized source snapshot changed"));
    }
    opencut_editor_core::scene::validate_project_scene(&fixed.snapshot.project)?;
    let sequence = fixed
        .snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == input.sequence_id)
        .ok_or_else(|| invalid("Source preparation scene is missing"))?;
    let (format, _) =
        opencut_composition::scene::resolve_scene_format(&fixed.snapshot.project, sequence)
            .map_err(|e| EditorError::new(e.code, e.message))?;
    let raster = opencut_composition::scene::preview_canvas(format.canvas_size);
    let targets = source_targets(&fixed.snapshot, &input)?;
    let mut candidates = Vec::with_capacity(targets.len());
    for (asset_id, selected_video_stream) in targets {
        let asset = fixed
            .snapshot
            .project
            .assets
            .iter()
            .find(|a| a.id == asset_id)
            .ok_or_else(|| invalid("Source preparation asset is missing"))?;
        let prepared = PreparedSourcePreview {
            snapshot: fixed.snapshot.clone(),
            sequence_id: input.sequence_id.clone(),
            identity: SourcePreviewIdentity {
                project_id: input.project_id.clone(),
                asset_id,
                source_version: asset.version.clone(),
                selected_video_stream,
                raster_bound: SourcePreviewRaster {
                    width: raster.width,
                    height: raster.height,
                },
                runtime_fingerprint: runtime_fingerprint.into(),
                policy_fingerprint: policy_fingerprint(&fixed.processing_policy)?,
            },
            authority: fixed.authority.clone(),
            processing_policy: fixed.processing_policy.clone(),
        };
        let request = request_for(&prepared)?;
        let existing = store.find_source_preview(&request)?;
        candidates.push((prepared, request, existing));
    }
    // An authorized idempotent lookup returns a task, not permission to read
    // original/proxy bytes. Capture only sources needed for a new acceptance;
    // execution and published reads retain their independent integrity fences.
    let ids: Vec<_> = candidates
        .iter()
        .filter(|(_, _, existing)| existing.is_none())
        .map(|(prepared, _, _)| prepared.identity.asset_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let lock = if ids.is_empty() {
        None
    } else {
        Some(assets.capture(&fixed.snapshot, &input.sequence_id, &ids)?)
    };
    if let Some(lock) = &lock {
        if lock.revision != fixed.snapshot.revision
            || lock.sequence_id != input.sequence_id
            || lock.assets.len() != ids.len()
        {
            return Err(invalid("Source capture changed fixed participating assets"));
        }
        for (prepared, _, _) in candidates
            .iter()
            .filter(|(_, _, existing)| existing.is_none())
        {
            let captured = lock
                .assets
                .iter()
                .find(|asset| asset.asset_id == prepared.identity.asset_id)
                .ok_or_else(|| invalid("Source capture omitted fixed bytes"))?;
            if captured.content_hash != prepared.identity.source_version.content_hash
                || captured.size != prepared.identity.source_version.size
            {
                return Err(invalid("Source capture changed original bytes"));
            }
        }
    }
    // The native capture owner still holds its collection gate through all
    // new acceptances, which recheck the fixed snapshot and current authority.
    candidates
        .into_iter()
        .map(|(prepared, request, existing)| match existing {
            Some(job) => Ok(job),
            None => store.accept_source_preview(request, prepared),
        })
        .collect()
}
pub fn source_targets(
    snapshot: &EditorSnapshot,
    input: &SourcePreviewPrepare,
) -> Result<BTreeSet<(String, Option<u32>)>, EditorError> {
    validate_submission(input)?;
    if snapshot.project.id != input.project_id || snapshot.revision != input.revision {
        return Err(invalid("Source target snapshot changed"));
    }
    let sequence = snapshot
        .project
        .sequences
        .iter()
        .find(|sequence| sequence.id == input.sequence_id)
        .ok_or_else(|| invalid("Source preparation scene is missing"))?;
    let targets: BTreeSet<(String, Option<u32>)> = if let Some(asset) = &input.asset_id {
        BTreeSet::from([(asset.clone(), input.selected_video_stream)])
    } else {
        sequence
            .tracks
            .iter()
            .filter(|track| track.enabled)
            .flat_map(|track| &track.elements)
            .filter(|element| element.enabled)
            .filter_map(|element| {
                if let ElementContent::Video {
                    asset_id,
                    video_stream,
                    ..
                } = &element.content
                {
                    Some((asset_id.clone(), *video_stream))
                } else {
                    None
                }
            })
            .collect()
    };
    if targets.len() > 256 {
        return Err(invalid("Too many source preparation identities"));
    }
    for (id, _) in &targets {
        if !snapshot
            .project
            .assets
            .iter()
            .any(|asset| &asset.id == id && asset.kind == AssetKind::Video)
        {
            return Err(invalid("Source preparation requires a fixed video asset"));
        }
    }
    Ok(targets)
}

fn time_base(base: SourcePreviewTimeBase) -> Result<(), EditorError> {
    if base.numerator == 0
        || base.denominator == 0
        || base.numerator > i32::MAX as u32
        || base.denominator > i32::MAX as u32
    {
        return Err(stored("Invalid observed source/proxy time base"));
    }
    Ok(())
}
pub fn validate_frame_video(
    video: &SourcePreviewFrameVideo,
    raster: SourcePreviewRaster,
) -> Result<(), EditorError> {
    if video.source_width == 0
        || video.source_height == 0
        || video.source_width > 16384
        || video.source_height > 16384
        || u64::from(video.source_width) * u64::from(video.source_height) > 16_777_216
        || video.raster_width == 0
        || video.raster_height == 0
        || video.raster_width > video.source_width
        || video.raster_height > video.source_height
        || ![0, 90, 180, 270].contains(&video.rotation_degrees)
        || video.pixel_aspect_numerator < 0
        || video.pixel_aspect_denominator <= 0
    {
        return Err(stored("Invalid observed source frame geometry"));
    }
    let (width, height) = if video.rotation_degrees % 180 == 0 {
        (video.raster_width, video.raster_height)
    } else {
        (video.raster_height, video.raster_width)
    };
    if width > raster.width || height > raster.height {
        return Err(stored("Source raster exceeds captured preview canvas"));
    }
    Ok(())
}
pub fn validate_index_record(
    record: &SourcePreviewIndexRecord,
    raster: SourcePreviewRaster,
) -> Result<(), EditorError> {
    if record.source_duration <= 0 || record.proxy_pts < 0 || record.proxy_duration <= 0 {
        return Err(stored("Invalid actual source/proxy frame timing"));
    }
    validate_frame_video(&record.source_video, raster)
}
pub fn validate_result(
    input: &PreparedSourcePreview,
    result: &StoredSourcePreviewResult,
) -> Result<(), EditorError> {
    validate_prepared(input)?;
    validate_public_result(&result.public)?;
    let p = &result.public;
    if p.identity != input.identity
        || p.producer_revision != input.snapshot.revision
        || decimal(&p.frame_count)? > input.processing_policy.max_frames
        || result.artifact.content_hash != p.video.content_hash
        || result.artifact.size.to_string() != p.video.size_bytes
        || result.artifact.mime_type != p.video.mime_type
        || result.frame_index.content_hash != p.frame_index.content_hash
        || result.frame_index.size.to_string() != p.frame_index.size_bytes
        || result.frame_index.mime_type != p.frame_index.mime_type
        || result.artifact.size > input.processing_policy.max_output_bytes
    {
        return Err(stored(
            "Stored source preparation changed captured input or artifacts",
        ));
    }
    Ok(())
}
pub fn validate_public_result(result: &SourcePreviewResult) -> Result<(), EditorError> {
    validate_identity(&result.identity)?;
    time_base(result.source_time_base)?;
    time_base(result.proxy_time_base)?;
    let count = decimal(&result.frame_count)?;
    if count == 0
        || count > MAX_SOURCE_FRAMES
        || !id(&result.producer_revision)
        || result.actual_stream_index > 63
        || result
            .identity
            .selected_video_stream
            .is_some_and(|n| n != result.actual_stream_index)
        || signed(&result.first_source_pts)? > signed(&result.last_source_pts)?
        || signed(&result.last_source_duration)? <= 0
        || !hash(&result.video.content_hash)
        || decimal(&result.video.size_bytes)? == 0
        || result.video.mime_type != "video/x-matroska"
        || !hash(&result.frame_index.content_hash)
        || decimal(&result.frame_index.size_bytes)?
            != INDEX_HEADER_BYTES + count * INDEX_RECORD_BYTES
        || result.frame_index.mime_type != "application/vnd.opencut.source-preview-index"
    {
        return Err(stored("Invalid immutable source preparation result"));
    }
    let runtime = &result.runtime;
    if runtime_fingerprint(runtime)? != result.identity.runtime_fingerprint {
        return Err(stored(
            "Source preparation runtime differs from captured fingerprint",
        ));
    }
    if !hash(&runtime.host_sha256)
        || !hash(&runtime.decoder.worker_sha256)
        || !hash(&runtime.ffmpeg_sha256)
        || !hash(&runtime.ffprobe_sha256)
        || runtime.decoder.decoder_libraries.len() != 5
    {
        return Err(stored("Source preparation omitted actual runtime identity"));
    }
    let mut names = BTreeSet::new();
    for library in &runtime.decoder.decoder_libraries {
        if !["avcodec", "avformat", "avutil", "swscale", "swresample"]
            .contains(&library.name.as_str())
            || !names.insert(&library.name)
            || library.version.is_empty()
            || library.version.len() > 128
            || !hash(&library.configuration_sha256)
            || !hash(&library.binary_sha256)
        {
            return Err(stored("Invalid source decoder library identity"));
        }
    }
    let platform = &runtime.decoder.platform;
    if [
        &platform.os,
        &platform.architecture,
        &platform.kernel_release,
        &platform.system_build,
    ]
    .iter()
    .any(|value| value.is_empty() || value.len() > 256 || value.chars().any(char::is_control))
    {
        return Err(stored("Missing source decoder platform observation"));
    }
    Ok(())
}

/// Select by original presentation time. Encoded frame timestamps are exact
/// observations, never a source FPS estimate or a rounded proxy time.
pub fn select_frame(
    index: &mut impl SourcePreviewFrameIndexRead,
    result: &SourcePreviewResult,
    requested: &Time,
) -> Result<Option<SourcePreviewFrameSelection>, EditorError> {
    validate_public_result(result)?;
    let requested = opencut_time::RationalTime::try_from(requested)?;
    if requested.ticks() < 0 {
        return Err(invalid("Negative original source request"));
    }
    let count = decimal(&result.frame_count)?;
    if index.frame_count() != count {
        return Err(stored("Source frame index count changed"));
    }
    let before_or_at = |pts: i64| {
        i128::from(pts)
            * i128::from(result.source_time_base.numerator)
            * i128::from(requested.timebase())
            <= i128::from(requested.ticks()) * i128::from(result.source_time_base.denominator)
    };
    let mut low = 0;
    let mut high = count;
    while low < high {
        let middle = low + (high - low) / 2;
        let record = index.frame(middle)?;
        validate_index_record(&record, result.identity.raster_bound)?;
        if before_or_at(record.source_pts) {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    if low == 0 {
        return Ok(None);
    }
    let ordinal = low - 1;
    let record = index.frame(ordinal)?;
    validate_index_record(&record, result.identity.raster_bound)?;
    Ok(Some(SourcePreviewFrameSelection {
        ordinal: ordinal.to_string(),
        source_pts: record.source_pts.to_string(),
        source_duration: record.source_duration.to_string(),
        proxy_pts: record.proxy_pts.to_string(),
        proxy_duration: record.proxy_duration.to_string(),
        source_video: record.source_video,
        last_frame: ordinal + 1 == count,
    }))
}
