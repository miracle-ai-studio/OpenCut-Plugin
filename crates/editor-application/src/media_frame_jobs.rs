//! Fixed-revision PNG submission and completion validation. No native I/O.
use crate::{JobRecord, JobRequest};
use opencut_composition::{CompositionScope, scene::*};
use opencut_contracts::{AssetLockManifest, EditorError, Time, scene::EditorSnapshot};
use opencut_job_contracts::ArtifactDescriptor;
pub use opencut_job_contracts::media_frame::*;
pub use opencut_job_execution::media_frame::*;
pub use opencut_job_execution::scene_audio::TrustedJobAuthority;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const CLASSIC_VISUAL_DECODER_PROFILE: &str =
    "mediabunny-1.41.0-initial-canvases-half-open;static-png-jpeg-browser-image";
pub struct AuthorizedFrameSnapshot {
    pub snapshot: EditorSnapshot,
    pub authority: TrustedJobAuthority,
    pub processing_policy: LocalVisualExecutionPolicy,
}
pub trait MediaFrameJobSubmissionStore {
    fn fixed_scene_frame_snapshot(
        &self,
        project_id: &str,
        revision: &str,
    ) -> Result<AuthorizedFrameSnapshot, EditorError>;
    fn find_accepted_scene_frame_job(
        &self,
        request: &JobRequest,
    ) -> Result<Option<JobRecord>, EditorError>;
    /// Recheck live original authority/request/snapshot/policy in one write lock.
    fn accept_scene_frame_job(
        &mut self,
        request: JobRequest,
        prepared: PreparedMediaFramePng,
    ) -> Result<JobRecord, EditorError>;
}
pub trait MediaFrameAssetCapture {
    /// Verify participating V4 versions and hold the CAS gate through acceptance.
    fn capture(
        &self,
        snapshot: &EditorSnapshot,
        sequence_id: &str,
        asset_ids: &[String],
    ) -> Result<AssetLockManifest, EditorError>;
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredFramePngResult {
    pub public: MediaFramePngResult,
    pub artifact: ArtifactDescriptor,
}

fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", message)
}
fn stored_invalid() -> EditorError {
    EditorError::new(
        "INVALID_STORED_RESULT",
        "Stored V4 frame result differs from its fixed input",
    )
}
pub(crate) fn valid_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}
pub(crate) fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn validate_authority(authority: &TrustedJobAuthority) -> Result<(), EditorError> {
    if let TrustedJobAuthority::Credential {
        principal_id,
        credential_id,
    } = authority
        && (!valid_id(principal_id) || !valid_id(credential_id))
    {
        return Err(invalid("Invalid trusted frame authority"));
    }
    Ok(())
}
pub fn validate_submission(input: &FrameRenderInput) -> Result<(), EditorError> {
    if ![
        &input.idempotency_key,
        &input.project_id,
        &input.revision,
        &input.sequence_id,
    ]
    .into_iter()
    .all(|id| valid_id(id))
        || opencut_time::RationalTime::try_from(&input.time)?.ticks() < 0
    {
        return Err(invalid(
            "Frame submission requires bounded identity and nonnegative exact time",
        ));
    }
    Ok(())
}
pub fn validate_policy(policy: &LocalVisualExecutionPolicy) -> Result<(), EditorError> {
    if [
        policy.max_frame_pixels,
        policy.max_decoded_pixels,
        policy.max_gpu_bytes,
        policy.max_rgba_bytes,
        policy.max_encoded_bytes,
        policy.max_output_bytes,
        policy.max_rss_bytes,
    ]
    .contains(&0)
        || policy.max_gpu_bytes > 2 * 1024 * 1024 * 1024
        || policy.max_rgba_bytes > 1024 * 1024 * 1024
        || policy.max_encoded_bytes > 32 * 1024 * 1024
        || policy.max_output_bytes > 256 * 1024 * 1024
        || !(1..=300000).contains(&policy.deadline_ms)
    {
        return Err(invalid("Invalid host visual admission"));
    }
    Ok(())
}
pub(crate) fn prepare(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    at: &Time,
) -> Result<PreparedVisualFrame, EditorError> {
    crate::font_synthesis::reject_legacy_project(&snapshot.project)?;
    let graph = compile_frame_scene(snapshot, sequence_id, at, CompositionScope::Visual)
        .map_err(|e| EditorError::new(e.code, e.message))?;
    prepare_visual_frame_scene(&graph).map_err(|e| EditorError::new(e.code, e.message))
}
fn validate_lock(
    input: &PreparedMediaFramePng,
    prepared: &PreparedVisualFrame,
) -> Result<(), EditorError> {
    let expected: BTreeMap<_, _> = prepared
        .resources()
        .iter()
        .map(|r| {
            (
                r.asset_id.clone(),
                (r.version.content_hash.clone(), r.version.size),
            )
        })
        .collect();
    let observed: BTreeMap<_, _> = input
        .asset_lock
        .assets
        .iter()
        .map(|r| (r.asset_id.clone(), (r.content_hash.clone(), r.size)))
        .collect();
    if input.asset_lock.version != 1
        || input.asset_lock.revision != input.snapshot.revision
        || input.asset_lock.sequence_id != input.sequence_id
        || observed.len() != input.asset_lock.assets.len()
        || observed != expected
    {
        return Err(EditorError::new(
            "ASSET_LOCK_INVALID",
            "Visual lock differs from fixed participating resources",
        ));
    }
    Ok(())
}
pub fn render_input_sha256(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    at: &Time,
) -> Result<String, EditorError> {
    let bytes = serde_json::to_vec(&(
        1u32,
        snapshot,
        sequence_id,
        at,
        MediaFrameProfile::ClassicV31GpuNormalFrame,
    ))
    .map_err(|_| invalid("Cannot hash fixed frame input"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub fn validate_prepared(
    input: &PreparedMediaFramePng,
) -> Result<PreparedVisualFrame, EditorError> {
    validate_policy(&input.processing_policy)?;
    validate_authority(&input.authority)?;
    if !valid_id(&input.snapshot.project.id)
        || !valid_id(&input.snapshot.revision)
        || !valid_id(&input.sequence_id)
    {
        return Err(invalid("Invalid frame preparation scope"));
    }
    let prepared = prepare(&input.snapshot, &input.sequence_id, &input.time)?;
    crate::classic_visual::ClassicVisualSession::new(&prepared, &input.processing_policy)?;
    validate_lock(input, &prepared)?;
    Ok(prepared)
}
pub fn validate_prepared_request(
    request: &JobRequest,
    prepared: &PreparedMediaFramePng,
) -> Result<(), EditorError> {
    let input: FrameRenderInput = serde_json::from_value(request.input.clone())
        .map_err(|_| invalid("Invalid public frame request"))?;
    validate_submission(&input)?;
    if request.kind != MEDIA_FRAME_PNG_KIND
        || request.idempotency_key != input.idempotency_key
        || input.project_id != prepared.snapshot.project.id
        || input.revision != prepared.snapshot.revision
        || input.sequence_id != prepared.sequence_id
        || input.time != prepared.time
    {
        return Err(invalid(
            "Frame request differs from its private preparation",
        ));
    }
    validate_prepared(prepared)?;
    Ok(())
}
pub fn submit_frame_png(
    store: &mut impl MediaFrameJobSubmissionStore,
    assets: &impl MediaFrameAssetCapture,
    input: FrameRenderInput,
) -> Result<JobRecord, EditorError> {
    validate_submission(&input)?;
    let authorized = store.fixed_scene_frame_snapshot(&input.project_id, &input.revision)?;
    if authorized.snapshot.project.id != input.project_id
        || authorized.snapshot.revision != input.revision
    {
        return Err(invalid(
            "Authorized snapshot differs from fixed frame request",
        ));
    }
    opencut_editor_core::scene::validate_project_scene(&authorized.snapshot.project)?;
    let request = JobRequest {
        idempotency_key: input.idempotency_key.clone(),
        kind: MEDIA_FRAME_PNG_KIND.into(),
        input: serde_json::to_value(&input)
            .map_err(|_| invalid("Cannot serialize frame request"))?,
    };
    if let Some(previous) = store.find_accepted_scene_frame_job(&request)? {
        return Ok(previous);
    }
    validate_policy(&authorized.processing_policy)?;
    validate_authority(&authorized.authority)?;
    let prepared = prepare(&authorized.snapshot, &input.sequence_id, &input.time)?;
    crate::classic_visual::ClassicVisualSession::new(&prepared, &authorized.processing_policy)?;
    let mut ids: Vec<_> = prepared
        .resources()
        .iter()
        .map(|r| r.asset_id.clone())
        .collect();
    ids.sort();
    let asset_lock = assets.capture(&authorized.snapshot, &input.sequence_id, &ids)?;
    let stored = PreparedMediaFramePng {
        snapshot: authorized.snapshot,
        sequence_id: input.sequence_id,
        time: input.time,
        asset_lock,
        authority: authorized.authority,
        processing_policy: authorized.processing_policy,
    };
    validate_lock(&stored, &prepared)?;
    store.accept_scene_frame_job(request, stored)
}

pub fn public_identity(prepared: &PreparedVisualFrame) -> MediaFrameIdentity {
    let i = prepared.identity();
    MediaFrameIdentity {
        project_id: i.project_id.clone(),
        revision: i.revision.clone(),
        sequence_id: i.sequence_id.clone(),
        time: i.time.clone(),
        width: i.width,
        height: i.height,
        profile: MediaFrameProfile::ClassicV31GpuNormalFrame,
    }
}
pub fn public_sources(
    prepared: &PreparedVisualFrame,
    decoded: &[DecodedVisualSource],
) -> Result<Vec<MediaFrameSourceObservation>, EditorError> {
    let bound = bind_visual_sources_scene(prepared, decoded)
        .map_err(|e| EditorError::new(e.code, e.message))?;
    Ok(bound
        .layers
        .iter()
        .zip(&bound.decoded)
        .map(|(layer, observation)| {
            let mut value = MediaFrameSourceObservation {
                element_id: layer.element_id.clone(),
                asset_id: layer.asset_id.clone(),
                requested_time: None,
                selected_video_stream: None,
                width: None,
                height: None,
                actual_timestamp_seconds: None,
                frame_duration_seconds: None,
                eof_held: false,
                no_frame_reason: None,
            };
            match observation {
                DecodedVisualSource::ReadyImage { width, height, .. } => {
                    value.width = Some(*width);
                    value.height = Some(*height);
                }
                DecodedVisualSource::ReadyVideo {
                    width,
                    height,
                    requested_time,
                    selected_video_stream,
                    actual_timestamp_seconds,
                    frame_duration_seconds,
                    eof_held,
                    ..
                } => {
                    value.width = Some(*width);
                    value.height = Some(*height);
                    value.requested_time = Some(requested_time.clone());
                    value.selected_video_stream = *selected_video_stream;
                    value.actual_timestamp_seconds = Some(*actual_timestamp_seconds);
                    value.frame_duration_seconds = Some(*frame_duration_seconds);
                    value.eof_held = *eof_held;
                }
                DecodedVisualSource::NoFrame {
                    requested_time,
                    reason,
                    ..
                } => {
                    value.requested_time = Some(requested_time.clone());
                    value.no_frame_reason = Some(match reason {
                        VisualNoFrameReason::OutsideReadWindow => {
                            MediaFrameNoSourceReason::OutsideReadWindow
                        }
                        VisualNoFrameReason::Eof => MediaFrameNoSourceReason::Eof,
                        VisualNoFrameReason::TimestampGap => MediaFrameNoSourceReason::TimestampGap,
                    });
                }
            }
            value
        })
        .collect())
}
pub(crate) fn private_sources(
    sources: &[MediaFrameSourceObservation],
) -> Result<Vec<DecodedVisualSource>, EditorError> {
    sources
        .iter()
        .map(|s| match (s.requested_time.as_ref(), s.no_frame_reason) {
            (None, None)
                if !s.eof_held
                    && s.selected_video_stream.is_none()
                    && s.actual_timestamp_seconds.is_none()
                    && s.frame_duration_seconds.is_none() =>
            {
                Ok(DecodedVisualSource::ReadyImage {
                    element_id: s.element_id.clone(),
                    width: s.width.ok_or_else(stored_invalid)?,
                    height: s.height.ok_or_else(stored_invalid)?,
                })
            }
            (Some(requested_time), None) => Ok(DecodedVisualSource::ReadyVideo {
                element_id: s.element_id.clone(),
                width: s.width.ok_or_else(stored_invalid)?,
                height: s.height.ok_or_else(stored_invalid)?,
                requested_time: requested_time.clone(),
                selected_video_stream: s.selected_video_stream,
                actual_timestamp_seconds: s.actual_timestamp_seconds.ok_or_else(stored_invalid)?,
                frame_duration_seconds: s.frame_duration_seconds.ok_or_else(stored_invalid)?,
                eof_held: s.eof_held,
            }),
            (Some(requested_time), Some(reason))
                if !s.eof_held
                    && s.selected_video_stream.is_none()
                    && s.width.is_none()
                    && s.height.is_none()
                    && s.actual_timestamp_seconds.is_none()
                    && s.frame_duration_seconds.is_none() =>
            {
                Ok(DecodedVisualSource::NoFrame {
                    element_id: s.element_id.clone(),
                    requested_time: requested_time.clone(),
                    reason: match reason {
                        MediaFrameNoSourceReason::OutsideReadWindow => {
                            VisualNoFrameReason::OutsideReadWindow
                        }
                        MediaFrameNoSourceReason::Eof => VisualNoFrameReason::Eof,
                        MediaFrameNoSourceReason::TimestampGap => VisualNoFrameReason::TimestampGap,
                    },
                })
            }
            _ => Err(stored_invalid()),
        })
        .collect()
}
pub fn validate_stored_result(
    input: &PreparedMediaFramePng,
    stored: &StoredFramePngResult,
) -> Result<MediaFramePngResult, EditorError> {
    let prepared = validate_prepared(input).map_err(|_| stored_invalid())?;
    let public = &stored.public;
    let execution = &public.execution;
    let decoded = private_sources(&public.sources)?;
    crate::classic_visual::ClassicVisualSession::new(&prepared, &input.processing_policy)?
        .project_decoded_sources(&decoded)
        .map_err(|_| stored_invalid())?;
    if public.schema_version != 1
        || public.identity != public_identity(&prepared)
        || public.asset_lock != input.asset_lock
        || public.render_input_sha256
            != render_input_sha256(&input.snapshot, &input.sequence_id, &input.time)?
        || public.sources != public_sources(&prepared, &decoded)?
        || crate::classic_visual::validate_visual_summary(execution).is_err()
        || stored.artifact.mime_type != "image/png"
        || public.artifact.mime_type != "image/png"
        || stored.artifact.size == 0
        || stored.artifact.size > input.processing_policy.max_output_bytes
        || public.artifact.size_bytes != stored.artifact.size.to_string()
        || public.artifact.content_hash != stored.artifact.content_hash
        || !valid_hash(&stored.artifact.content_hash)
    {
        return Err(stored_invalid());
    }
    Ok(public.clone())
}

#[cfg(test)]
mod tests;
