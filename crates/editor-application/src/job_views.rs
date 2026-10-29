//! Projection of stored execution records into the public task contract.
use super::{JobProgress, JobRecord, JobState};
use opencut_api_contracts::{job_results::AssetAnalysisResult, job_views::*};
use opencut_contracts::EditorError;
use opencut_job_contracts as jobs;
use opencut_job_contracts::asset_analysis::{AssetAnalysisData, AssetAnalysisOptions};
use opencut_media_contracts as media;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Native stored kinds are exact execution identities. Public views classify
/// each known shape explicitly;unknown data never falls through by a suffix.
pub fn public_kind(stored: &str) -> Option<&'static str> {
    match stored {
        "media.preview_source" => Some("media.preview_source"),
        "media.probe" => Some("media.probe"),
        "preview.frame" => Some("preview.frame"),
        "preview.project_frame" => Some("preview.project_frame"),
        "preview.composited_frame" => Some("preview.composited_frame"),
        "preview.project_range" => Some("preview.project_range"),
        "render.export" => Some("render.export"),
        "render.project" => Some("render.project"),
        "artifact.verify" => Some("artifact.verify"),
        "media.import" => Some("media.import"),
        "analysis.asset.v4" => Some("analysis.asset"),
        "analysis.transcribe" => Some("analysis.asset"),
        "analysis.silence" => Some("analysis.asset"),
        "analysis.shots" => Some("analysis.asset"),
        "analysis.waveform" => Some("analysis.asset"),
        "analysis.speaker_turns" => Some("analysis.asset"),
        "analysis.sequence_transcribe" => Some("analysis.sequence_transcribe"),
        "analysis.sequence_transcribe.v4" => Some("analysis.sequence_transcribe"),
        "render.project.v3" => Some("render.project"),
        "preview.project_frame.v3" => Some("preview.composited_frame"),
        "preview.project_range.v3" => Some("preview.project_range"),
        "render.project_audio.v4" => Some("render.audio"),
        "render.project_audio_range.v4" => Some("render.audio"),
        "render.project_frame.v4" => Some("render.media_frame"),
        "render.project_text_frame.v4" => Some("render.text_frame"),
        "render.project_caption_frame.v4" => Some("render.caption_frame"),
        "render.project_graphic_frame.v4" => Some("render.graphic_frame"),
        "render.project_mask_frame.v4" => Some("render.mask_frame"),
        "render.project_blend_frame.v4" => Some("render.blend_frame"),
        "render.project_scene_frame.v4" => Some("render.frame"),
        "render.project_video.v4" => Some("render.media_video"),
        "render.project_caption_video.v4" => Some("render.caption_video"),
        "render.project_graphic_video.v4" => Some("render.graphic_video"),
        "render.project_mask_video.v4" => Some("render.mask_video"),
        "render.project_blend_video.v4" => Some("render.blend_video"),
        "render.project_scene_video.v4" => Some("render.scene_video"),
        "render.export_video.v2" => Some("render.video"),
        "render.export_video_range.v1" => Some("render.video_range"),
        _ => None,
    }
}

fn error(code: &str, message: &str) -> EditorError {
    EditorError::new(code, message)
}
fn bad_result() -> EditorError {
    error(
        "INVALID_STORED_RESULT",
        "Stored task result cannot be represented by the public task contract",
    )
}
fn decode<T: DeserializeOwned>(value: Value) -> Result<T, EditorError> {
    serde_json::from_value(value).map_err(|_| bad_result())
}
fn public_failure(job: &str, code: &str) -> PublicJobFailure {
    let (code, message) = match code {
        "MEDIA_OUTPUT_LIMIT_EXCEEDED" => (
            "MEDIA_OUTPUT_LIMIT_EXCEEDED",
            "Media output reached its admitted file limit; ask the owner to inspect the output ceiling and workspace headroom before submitting a new task",
        ),
        "STORAGE_BUDGET_EXCEEDED" => (
            "STORAGE_BUDGET_EXCEEDED",
            "Workspace admission limit prevented task file allocation; ask the owner to inspect policy or release eligible data before submitting a new task",
        ),
        "STORAGE_CAPACITY_LOW" => (
            "STORAGE_CAPACITY_LOW",
            "Observed volume headroom is insufficient for task file allocation",
        ),
        "DATABASE_CAPACITY_EXHAUSTED" => (
            "DATABASE_CAPACITY_EXHAUSTED",
            "Workspace database has no space to commit task state; free capacity before retrying",
        ),
        "STORAGE_BUDGET_BUSY" => (
            "STORAGE_BUDGET_BUSY",
            "Workspace admission remained busy; inspect the terminal task before submitting a new task",
        ),
        "STORAGE_BUDGET_ERROR" => (
            "STORAGE_BUDGET_ERROR",
            "Workspace admission state requires owner inspection",
        ),
        "MISSING_PLUGIN_DEPENDENCY" => (
            "MISSING_PLUGIN_DEPENDENCY",
            "Required effect plugins are unavailable; export is blocked and project data is preserved",
        ),
        "AUDIO_BUDGET_EXCEEDED" => (
            "AUDIO_BUDGET_EXCEEDED",
            "Audio exceeds the admitted resource or execution limits",
        ),
        "VISUAL_BUDGET_EXCEEDED" => (
            "VISUAL_BUDGET_EXCEEDED",
            "Visual rendering exceeds the admitted resource or execution limits",
        ),
        "VIDEO_BUDGET_EXCEEDED" => (
            "VIDEO_BUDGET_EXCEEDED",
            "Video exceeds the admitted resource or execution limits",
        ),
        "SOURCE_PREVIEW_BUDGET_EXCEEDED" | "SOURCE_PREVIEW_TIME_LIMIT" => (
            "SOURCE_PREVIEW_LIMIT_EXCEEDED",
            "Source preview preparation exceeded its captured resource or execution limits",
        ),
        "INVALID_ARGUMENT" | "TIME_OVERFLOW" | "CONSTRAINT_VIOLATION" => (
            "INVALID_ARGUMENT",
            "Task input is invalid for this operation",
        ),
        "MISSING_ASSET" | "ASSET_UNAVAILABLE" | "NOT_FOUND" => {
            ("MISSING_ASSET", "A required task dependency is unavailable")
        }
        "CAPABILITY_UNAVAILABLE" | "ASSET_LOCK_REQUIRED" => (
            "CAPABILITY_UNAVAILABLE",
            "A required task capability is unavailable",
        ),
        "ARTIFACT_VERIFICATION_FAILED" | "ASSET_CORRUPTED" | "UPLOAD_INTEGRITY_ERROR" => (
            "ARTIFACT_VERIFICATION_FAILED",
            "Task content failed integrity validation",
        ),
        "UNSUPPORTED_JOB_KIND" => (
            "UNSUPPORTED_JOB_KIND",
            "The public task contract cannot represent the task kind",
        ),
        "UNSUPPORTED_V4" | "UNSUPPORTED_RENDER_FEATURE" => (
            "UNSUPPORTED_RENDER_FEATURE",
            "This scene contains a feature that the selected renderer cannot execute",
        ),
        "INVALID_STORED_RESULT" => (
            "INVALID_STORED_RESULT",
            "Stored task result cannot be represented by the public task contract",
        ),
        _ => (
            "JOB_FAILED",
            "Task execution failed; consult its authorized diagnostic record",
        ),
    };
    PublicJobFailure {
        code: code.into(),
        message: message.into(),
        diagnostic_job_id: job.into(),
    }
}
pub fn public_error(original: EditorError) -> EditorError {
    // Envelopes for new view tools may not leak worker/backend error text either.
    match original.code.as_str() {
        "NOT_AUTHORIZED" => crate::access::denied(),
        "NOT_FOUND" => error("NOT_FOUND", "Task or required object was not found"),
        "INVALID_ARGUMENT" => error("INVALID_ARGUMENT", "Invalid public task request"),
        "IDEMPOTENCY_KEY_REUSED" => error(
            "IDEMPOTENCY_KEY_REUSED",
            "Idempotency key was already used with another request",
        ),
        "INVALID_CURSOR" => error(
            "INVALID_CURSOR",
            "Cursor does not match this stream or filter",
        ),
        "CURSOR_EXPIRED" => error(
            "CURSOR_EXPIRED",
            "Cursor anchor is unavailable; start a new query",
        ),
        "REVISION_CONFLICT" => error(
            "REVISION_CONFLICT",
            "The expected revision is no longer current",
        ),
        "DATABASE_BUSY" => error(
            "DATABASE_BUSY",
            "Task state is temporarily locked; inspect it and retry the same request",
        ),
        "STORAGE_ERROR" | "INTERNAL_ERROR" => {
            error("INTERNAL_ERROR", "Task state could not be read or changed")
        }
        _ => {
            let failure = public_failure("", &original.code);
            error(&failure.code, &failure.message)
        }
    }
}
/// Submission errors occur before a job exists. Do not send users to a
/// diagnostic job that was never accepted, while preserving safe known codes.
pub fn public_submission_error(original: EditorError) -> EditorError {
    let mut public = public_error(original);
    if public.code == "JOB_FAILED" {
        public.message = "The task could not be accepted; no task was created. Check rendering support and source availability before retrying".into();
    }
    public
}
pub fn progress(value: JobProgress) -> ProgressView {
    let stage = match value.stage.as_str() {
        "resolving_assets" => PublicJobStage::ResolvingAssets,
        "probing" => PublicJobStage::Probing,
        "selecting_frame" => PublicJobStage::SelectingFrame,
        "decoding" => PublicJobStage::Decoding,
        "encoding" => PublicJobStage::Encoding,
        "verifying" => PublicJobStage::Verifying,
        _ => PublicJobStage::Unknown,
    };
    ProgressView {
        stage,
        fraction: value.fraction,
        media_time: value.media_time,
    }
}
fn metadata(value: media::MediaMetadata) -> MediaInfo {
    MediaInfo {
        format_name: value.format_name,
        format_brand: value.format_brand,
        duration: value.duration,
        bit_rate: value.bit_rate,
        streams: value.streams,
    }
}
fn verification(value: media::VerificationReport) -> VerificationView {
    VerificationView {
        passed: value.passed,
        metadata: metadata(value.metadata),
        decoded_samples: value.decoded_samples,
        checks: value
            .checks
            .into_iter()
            .map(|v| VerificationCheckView {
                name: v.name,
                passed: v.passed,
            })
            .collect(),
    }
}
fn layouts(values: Vec<media::TextLayoutReport>) -> Vec<TextLayoutView> {
    values
        .into_iter()
        .map(|v| TextLayoutView {
            requested_family: v.requested_family,
            policy: v.policy,
            used_faces: v
                .used_faces
                .into_iter()
                .map(|f| FontFaceView {
                    family: f.family,
                    postscript_name: f.postscript_name,
                    face_index: f.face_index,
                })
                .collect(),
            line_count: v.line_count,
            glyph_count: v.glyph_count,
            rtl_runs: v.rtl_runs,
            cluster_ranges: v.cluster_ranges,
            pixel_bounds: v.pixel_bounds,
        })
        .collect()
}
fn export(value: media::ExportResult) -> ExportView {
    ExportView {
        metadata: metadata(value.metadata),
        verification: verification(value.verification),
        text_layouts: layouts(value.text_layouts),
    }
}
pub fn valid_job_id(value: &str) -> bool {
    value.strip_prefix("job_").is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn parse_artifact_id(value: &str) -> Result<(&str, &str), EditorError> {
    let mut parts = value.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("artifact"), Some(job), Some(hash), None)
            if valid_job_id(job) && valid_hash(hash) =>
        {
            Ok((job, hash))
        }
        _ => Err(error("INVALID_ARGUMENT", "Invalid artifact identity")),
    }
}
fn artifact(
    job_id: &str,
    value: jobs::ArtifactDescriptor,
) -> Result<ArtifactReference, EditorError> {
    if !valid_hash(&value.content_hash) || !valid_job_id(job_id) {
        return Err(bad_result());
    }
    let mime = match value.mime_type.as_str() {
        "image/png" => "image/png",
        "video/mp4" => "video/mp4",
        "video/webm" => "video/webm",
        "video/x-matroska" => "video/x-matroska",
        "application/vnd.opencut.source-preview-index" => {
            "application/vnd.opencut.source-preview-index"
        }
        "audio/wav" => "audio/wav",
        "audio/mpeg" => "audio/mpeg",
        _ => "application/octet-stream",
    };
    Ok(ArtifactReference {
        artifact_id: format!("artifact:{job_id}:{}", value.content_hash),
        job_id: job_id.into(),
        role: ArtifactRole::Primary,
        content_hash: value.content_hash,
        size_bytes: value.size.to_string(),
        mime_type: mime.into(),
        availability: ArtifactAvailability::Unchecked,
    })
}

/// Native callers have already selected this reference from the validated
/// public projection. Multiple source artifacts keep distinct paths/hashes.
pub fn private_artifact_path<'a>(
    record: &'a JobRecord,
    reference: &ArtifactReference,
) -> Result<&'a str, EditorError> {
    if reference.job_id != record.job_id
        || reference.artifact_id != format!("artifact:{}:{}", record.job_id, reference.content_hash)
    {
        return Err(bad_result());
    }
    let field = match reference.role {
        ArtifactRole::Primary => "artifact",
        ArtifactRole::FrameIndex if record.kind == crate::source_preview::KIND => "frame_index",
        _ => return Err(bad_result()),
    };
    let value = record
        .result
        .as_ref()
        .and_then(|result| result.get(field))
        .ok_or_else(bad_result)?;
    if value.get("content_hash").and_then(Value::as_str) != Some(reference.content_hash.as_str())
        || value
            .get("size")
            .and_then(Value::as_u64)
            .map(|n| n.to_string())
            .as_ref()
            != Some(&reference.size_bytes)
    {
        return Err(bad_result());
    }
    value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(bad_result)
}
fn result(
    job_id: &str,
    kind: &str,
    value: Value,
) -> Result<(JobResultView, Vec<ArtifactReference>), EditorError> {
    let (result, file) = match kind {
        "media.probe" => return Ok((JobResultView::Probe(metadata(decode(value)?)), vec![])),
        "artifact.verify" => {
            return Ok((
                JobResultView::Verification(verification(decode(value)?)),
                vec![],
            ));
        }
        "preview.frame" => {
            let v: jobs::MediaFrameJobResult = decode(value)?;
            (
                JobResultView::SourceFrame(SourceFrameView {
                    requested_time: v.media.requested_time,
                    actual_time: v.media.actual_time,
                    source_pts: v.media.source_pts.to_string(),
                    source_time_base: v.media.source_time_base,
                    alignment: v.media.alignment,
                    exact: v.media.exact,
                    quality: v.media.quality,
                }),
                v.artifact,
            )
        }
        "render.export" => {
            let v: jobs::MediaExportJobResult = decode(value)?;
            (JobResultView::Export(export(v.media)), v.artifact)
        }
        "render.project" => {
            let v: jobs::ProjectRenderJobResult = decode(value)?;
            (
                JobResultView::ProjectRender(ProjectRenderView {
                    revision: v.revision,
                    asset_lock: v.asset_lock,
                    asset_locking: v.asset_locking,
                    media: export(v.media),
                }),
                v.artifact,
            )
        }
        "preview.project_frame" => {
            let v: jobs::ProjectFrameJobResult = decode(value)?;
            (
                JobResultView::ProjectFrame(ProjectFrameView {
                    revision: v.revision,
                    sequence_id: v.sequence_id,
                    requested_time: v.requested_time,
                    actual_sequence_time: v.actual_sequence_time,
                    alignment: v.alignment,
                    quality: v.quality,
                    asset_lock: v.asset_lock,
                    asset_locking: v.asset_locking,
                    width: v.width,
                    height: v.height,
                    text_layouts: layouts(v.text_layouts),
                }),
                v.artifact,
            )
        }
        "preview.composited_frame" => {
            let v: jobs::CompositedProjectFrameJobResult = decode(value)?;
            (
                JobResultView::CompositedFrame(CompositedFrameView {
                    revision: v.revision,
                    sequence_id: v.sequence_id,
                    requested_time: v.requested_time,
                    actual_sequence_time: v.actual_sequence_time,
                    alignment: v.alignment,
                    quality: v.quality,
                    asset_lock: v.asset_lock,
                    asset_locking: v.asset_locking,
                    width: v.width,
                    height: v.height,
                    text_layouts: layouts(v.text_layouts),
                }),
                v.artifact,
            )
        }
        "preview.project_range" => {
            let v: jobs::ProjectRangeJobResult = decode(value)?;
            (
                JobResultView::ProjectRange(ProjectRangeView {
                    revision: v.revision,
                    sequence_id: v.sequence_id,
                    requested_range: v.requested_range,
                    actual_sequence_range: v.actual_sequence_range,
                    alignment: v.alignment,
                    quality: v.quality,
                    asset_lock: v.asset_lock,
                    asset_locking: v.asset_locking,
                    media: export(v.media),
                }),
                v.artifact,
            )
        }
        _ => return Err(error("UNSUPPORTED_JOB_KIND", "Unsupported result kind")),
    };
    Ok((result, vec![artifact(job_id, file)?]))
}
fn analysis_result<R: DeserializeOwned + serde::Serialize>(
    input: &opencut_job_execution::PreparedAnalysisInput,
    kind: &str,
    value: Value,
    validate: impl FnOnce(&R) -> bool,
    wrap: impl FnOnce(R) -> AssetAnalysisData,
) -> Result<JobResultView, EditorError> {
    let result: jobs::AnalysisJobResult<R> = decode(value)?;
    let locked = input.asset_lock.assets.first().ok_or_else(bad_result)?;
    let options_match = matches!(
        (kind, &input.options),
        (
            "analysis.silence",
            opencut_job_execution::AnalysisOptions::Silence { .. }
        ) | (
            "analysis.shots",
            opencut_job_execution::AnalysisOptions::Shots { .. }
        ) | (
            "analysis.waveform",
            opencut_job_execution::AnalysisOptions::Waveform { .. }
        )
    );
    if !options_match
        || input.asset_lock.assets.len() != 1
        || input.asset_lock.revision != input.snapshot.revision
        || input.asset_lock.sequence_id != input.sequence_id
        || locked.asset_id != input.asset_id
        || result.project_id != input.snapshot.project.id
        || result.revision != input.snapshot.revision
        || result.asset_id != input.asset_id
        || &result.asset_version != locked
        || !validate(&result.analysis)
    {
        return Err(bad_result());
    }
    let options = match &input.options {
        opencut_job_execution::AnalysisOptions::Silence {
            noise_db,
            min_duration_ms,
        } => AssetAnalysisOptions::Silence {
            noise_db: *noise_db,
            min_duration_ms: *min_duration_ms,
        },
        opencut_job_execution::AnalysisOptions::Shots { threshold } => {
            AssetAnalysisOptions::Shots {
                threshold: *threshold,
            }
        }
        opencut_job_execution::AnalysisOptions::Waveform { buckets } => {
            AssetAnalysisOptions::Waveform { buckets: *buckets }
        }
    };
    Ok(JobResultView::AssetAnalysis(AssetAnalysisResult {
        project_id: result.project_id,
        revision: result.revision,
        asset_id: result.asset_id,
        asset_version: result.asset_version,
        options,
        data: wrap(result.analysis),
    }))
}

pub fn project(record: JobRecord) -> JobView {
    // Public intent identity is useful even before a result exists; only these three
    // fields are selected from the legacy prepared input, never its snapshot or URIs.
    let target = if record.kind == "analysis.asset.v4" {
        (|| {
            Some(ProjectTaskTarget {
                project_id: record
                    .input
                    .pointer("/snapshot/project/id")?
                    .as_str()?
                    .into(),
                revision: record.input.pointer("/snapshot/revision")?.as_str()?.into(),
                sequence_id: record
                    .input
                    .pointer("/snapshot/project/default_sequence_id")?
                    .as_str()?
                    .into(),
            })
        })()
    } else if matches!(
        record.kind.as_str(),
        "render.export_video.v2" | "render.export_video_range.v1"
    ) {
        (|| {
            Some(ProjectTaskTarget {
                project_id: record
                    .input
                    .pointer("/execution/snapshot/project/id")?
                    .as_str()?
                    .into(),
                revision: record
                    .input
                    .pointer("/execution/snapshot/revision")?
                    .as_str()?
                    .into(),
                sequence_id: record
                    .input
                    .pointer("/execution/sequence_id")?
                    .as_str()?
                    .into(),
            })
        })()
    } else if record.kind == "analysis.sequence_transcribe.v4" {
        (|| {
            Some(ProjectTaskTarget {
                project_id: record
                    .input
                    .pointer("/audio/snapshot/project/id")?
                    .as_str()?
                    .into(),
                revision: record
                    .input
                    .pointer("/audio/snapshot/revision")?
                    .as_str()?
                    .into(),
                sequence_id: record.input.pointer("/audio/sequence_id")?.as_str()?.into(),
            })
        })()
    } else if matches!(
        record.kind.as_str(),
        "render.project"
            | "media.preview_source"
            | "preview.project_frame"
            | "preview.composited_frame"
            | "preview.project_range"
            | "render.project.v3"
            | "render.project_audio.v4"
            | "render.project_audio_range.v4"
            | "render.project_frame.v4"
            | "render.project_text_frame.v4"
            | "render.project_caption_frame.v4"
            | "render.project_graphic_frame.v4"
            | "render.project_mask_frame.v4"
            | "render.project_scene_frame.v4"
            | "render.project_blend_frame.v4"
            | "render.project_video.v4"
            | "render.project_caption_video.v4"
            | "render.project_graphic_video.v4"
            | "render.project_mask_video.v4"
            | "render.project_scene_video.v4"
            | "render.export_video.v2"
            | "render.export_video_range.v1"
            | "render.project_blend_video.v4"
            | "preview.project_frame.v3"
            | "preview.project_range.v3"
            | "analysis.speaker_turns"
            | "analysis.transcribe"
            | "analysis.sequence_transcribe"
            | "analysis.silence"
            | "analysis.shots"
            | "analysis.waveform"
    ) {
        (|| {
            Some(ProjectTaskTarget {
                project_id: record
                    .input
                    .pointer(
                        if record.kind.ends_with(".v3")
                            || record.kind == "analysis.sequence_transcribe"
                        {
                            "/snapshot/project/base/id"
                        } else {
                            "/snapshot/project/id"
                        },
                    )?
                    .as_str()?
                    .into(),
                revision: record.input.pointer("/snapshot/revision")?.as_str()?.into(),
                sequence_id: record.input.get("sequence_id")?.as_str()?.into(),
            })
        })()
    } else {
        None
    };
    let import = if record.kind == "media.import" {
        serde_json::from_value::<crate::uploads::PreparedMediaImportInput>(record.input.clone())
            .ok()
            .filter(|v| crate::uploads::validate_manifest(v).is_ok())
    } else {
        None
    };
    let analysis = if record.kind == "analysis.transcribe" {
        serde_json::from_value::<opencut_job_execution::PreparedTranscribeInput>(
            record.input.clone(),
        )
        .ok()
    } else {
        None
    };
    let speaker = if record.kind == "analysis.speaker_turns" {
        serde_json::from_value::<opencut_job_execution::PreparedSpeakerTurnsInput>(
            record.input.clone(),
        )
        .ok()
    } else {
        None
    };
    let measured = if matches!(
        record.kind.as_str(),
        "analysis.silence" | "analysis.shots" | "analysis.waveform"
    ) {
        serde_json::from_value::<opencut_job_execution::PreparedAnalysisInput>(record.input.clone())
            .ok()
    } else {
        None
    };
    let resource_target = import
        .as_ref()
        .map(|v| JobResourceTarget {
            project_id: v.project_id.clone(),
            resource_kind: "upload".into(),
            resource_id: v.upload_id.clone(),
        })
        .or_else(|| {
            analysis.as_ref().map(|v| JobResourceTarget {
                project_id: v.snapshot.project.id.clone(),
                resource_kind: "asset".into(),
                resource_id: v.asset_id.clone(),
            })
        })
        .or_else(|| {
            speaker.as_ref().map(|v| JobResourceTarget {
                project_id: v.snapshot.project.id.clone(),
                resource_kind: "asset".into(),
                resource_id: v.asset_id.clone(),
            })
        })
        .or_else(|| {
            measured.as_ref().map(|v| JobResourceTarget {
                project_id: v.snapshot.project.id.clone(),
                resource_kind: "asset".into(),
                resource_id: v.asset_id.clone(),
            })
        });
    let mut view = JobView {
        purpose: (record.kind == "media.preview_source"
            || (record
                .input
                .pointer("/options/resolution")
                .and_then(Value::as_str)
                == Some("preview540")
                && record
                    .input
                    .pointer("/options/runtime_fingerprint")
                    .and_then(Value::as_str)
                    .is_some()))
        .then_some(opencut_api_contracts::job_views::JobPurpose::PlaybackOptimization),
        job_id: record.job_id.clone(),
        kind: record.kind,
        target,
        state: record.state,
        cancel_requested: record.cancel_requested,
        progress: record.progress.map(progress),
        result: None,
        resource_target,
        result_unavailable: None,
        failure: None,
        artifacts: vec![],
    };
    if view.kind == "media.preview_source" {
        if let Ok(prepared) = serde_json::from_value::<crate::source_preview::PreparedSourcePreview>(
            record.input.clone(),
        ) {
            view.resource_target = Some(JobResourceTarget {
                project_id: prepared.identity.project_id,
                resource_kind: "asset".into(),
                resource_id: prepared.identity.asset_id,
            });
        }
    }
    if view.kind == "analysis.asset.v4" {
        if let (Some(project), Some(asset)) = (
            record
                .input
                .pointer("/snapshot/project/id")
                .and_then(Value::as_str),
            record.input.get("asset_id").and_then(Value::as_str),
        ) {
            view.resource_target = Some(JobResourceTarget {
                project_id: project.into(),
                resource_kind: "asset".into(),
                resource_id: asset.into(),
            });
        }
    }
    if record.state == JobState::Succeeded && view.kind == "media.preview_source" {
        let projected = (|| {
            let prepared: crate::source_preview::PreparedSourcePreview =
                decode(record.input.clone())?;
            let stored: crate::source_preview::StoredSourcePreviewResult =
                decode(record.result.unwrap_or(Value::Null))?;
            crate::source_preview::validate_result(&prepared, &stored)?;
            let primary = artifact(&view.job_id, stored.artifact)?;
            let mut index = artifact(&view.job_id, stored.frame_index)?;
            index.role = ArtifactRole::FrameIndex;
            Ok::<_, EditorError>((
                JobResultView::SourcePreview(stored.public),
                vec![primary, index],
            ))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "analysis.asset.v4" {
        let projected = (|| {
            let input: crate::asset_analysis::PreparedAssetAnalysis = decode(record.input.clone())?;
            let result: crate::asset_analysis::AssetAnalysisResult =
                decode(record.result.unwrap_or(Value::Null))?;
            crate::asset_analysis::validate_result(&input, &result)?;
            Ok::<_, EditorError>(JobResultView::AssetAnalysis(result.into()))
        })();
        match projected {
            Ok(result) => view.result = Some(result),
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "analysis.sequence_transcribe.v4"
    {
        let projected = (|| {
            use crate::scene_transcription::{PreparedSceneTranscription, validate_stored_result};
            let input: PreparedSceneTranscription = decode(record.input.clone())?;
            let public = validate_stored_result(&input, record.result.unwrap_or(Value::Null))?;
            Ok::<_, EditorError>(JobResultView::SceneTranscription(public.into()))
        })();
        match projected {
            Ok(result) => view.result = Some(result),
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_graphic_frame.v4"
    {
        let projected = (|| {
            use crate::scene_graphic_frame_jobs::{
                PreparedGraphicFramePng, StoredGraphicFramePngResult, validate_stored_result,
            };
            let input: PreparedGraphicFramePng = decode(record.input.clone())?;
            let stored: StoredGraphicFramePngResult = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::GraphicFrame(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_scene_frame.v4" {
        let projected = (|| {
            use crate::scene_frame_jobs::{
                PreparedSceneFramePng, StoredSceneFramePngResult, validate_stored_result,
            };
            let input: PreparedSceneFramePng = decode(record.input.clone())?;
            let stored: StoredSceneFramePngResult = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::SceneFrame(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_mask_frame.v4" {
        let projected = (|| {
            use crate::scene_mask_frame_jobs::{
                PreparedMaskFramePng, StoredMaskFramePngResult, validate_stored_result,
            };
            let input: PreparedMaskFramePng = decode(record.input.clone())?;
            let stored: StoredMaskFramePngResult = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::MaskFrame(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_blend_frame.v4" {
        let projected = (|| {
            use crate::scene_blend_frame_jobs::{
                PreparedBlendFramePng, StoredBlendFramePngResult, validate_stored_result,
            };
            let input: PreparedBlendFramePng = decode(record.input.clone())?;
            let stored: StoredBlendFramePngResult = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::BlendFrame(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_caption_frame.v4"
    {
        let projected = (|| {
            use crate::scene_caption_frame_jobs::{
                PreparedCaptionFramePng, StoredCaptionFramePngResult, validate_stored_result,
            };
            let input: PreparedCaptionFramePng = decode(record.input.clone())?;
            let stored: StoredCaptionFramePngResult = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::CaptionFrame(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_video.v4" {
        let projected = (|| {
            use crate::media_video_jobs::{
                PreparedMediaVideoMp4, StoredVideoMp4Result, validate_stored_result,
            };
            let input: PreparedMediaVideoMp4 = decode(record.input.clone())?;
            let stored: StoredVideoMp4Result = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::MediaVideo(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_caption_video.v4"
    {
        let projected = (|| {
            use crate::scene_caption_video_jobs::{
                PreparedCaptionVideoMp4, StoredCaptionVideoMp4Result, validate_stored_result,
            };
            let input: PreparedCaptionVideoMp4 = decode(record.input.clone())?;
            let stored: StoredCaptionVideoMp4Result = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::CaptionVideo(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_graphic_video.v4"
    {
        let projected = (|| {
            use crate::scene_graphic_video_jobs::{
                PreparedGraphicVideoMp4, StoredGraphicVideoMp4Result, validate_stored_result,
            };
            let input: PreparedGraphicVideoMp4 = decode(record.input.clone())?;
            let stored: StoredGraphicVideoMp4Result = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::GraphicVideo(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_scene_video.v4" {
        let projected = (|| {
            use crate::scene_video_jobs::{
                PreparedSceneVideoMp4, StoredSceneVideoMp4Result, validate_stored_result,
            };
            let input: PreparedSceneVideoMp4 = decode(record.input.clone())?;
            let stored: StoredSceneVideoMp4Result = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::SceneVideo(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded
        && matches!(
            view.kind.as_str(),
            "render.export_video.v2" | "render.export_video_range.v1"
        )
    {
        let projected = (|| {
            use crate::export_video::{
                PreparedExportVideo, StoredExportVideo, validate_stored_result,
            };
            let input: PreparedExportVideo = decode(record.input.clone())?;
            let stored: StoredExportVideo = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let output = if view.kind == "render.export_video_range.v1" {
                JobResultView::VideoRange(crate::export_video::range_result(&input, public)?.into())
            } else {
                JobResultView::VideoExport(public.into())
            };
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((output, vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_mask_video.v4" {
        let projected = (|| {
            use crate::scene_mask_video_jobs::{
                PreparedMaskVideoMp4, StoredMaskVideoMp4Result, validate_stored_result,
            };
            let input: PreparedMaskVideoMp4 = decode(record.input.clone())?;
            let stored: StoredMaskVideoMp4Result = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::MaskVideo(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_blend_video.v4" {
        let projected = (|| {
            use crate::scene_blend_video_jobs::{
                PreparedBlendVideoMp4, StoredBlendVideoMp4Result, validate_stored_result,
            };
            let input: PreparedBlendVideoMp4 = decode(record.input.clone())?;
            let stored: StoredBlendVideoMp4Result = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::BlendVideo(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_text_frame.v4" {
        let projected = (|| {
            use crate::scene_text_frame_jobs::{
                PreparedTextFramePng, StoredTextFramePngResult, validate_stored_result,
            };
            let input: PreparedTextFramePng = decode(record.input.clone())?;
            let stored: StoredTextFramePngResult = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::TextFrame(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_frame.v4" {
        let projected = (|| {
            use crate::media_frame_jobs::{
                PreparedMediaFramePng, StoredFramePngResult, validate_stored_result,
            };
            let input: PreparedMediaFramePng = decode(record.input.clone())?;
            let stored: StoredFramePngResult = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::MediaFrame(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_audio.v4" {
        let projected = (|| {
            use crate::scene_audio_jobs::{
                PreparedAudioWav, StoredAudioWavResult, validate_stored_result,
            };
            let input: PreparedAudioWav = decode(record.input.clone())?;
            let stored: StoredAudioWavResult = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::Audio(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "render.project_audio_range.v4" {
        let projected = (|| {
            use crate::scene_audio_jobs::{
                PreparedAudioWav, StoredAudioRangeWavResult, validate_stored_range_result,
            };
            let input: PreparedAudioWav = decode(record.input.clone())?;
            let stored: StoredAudioRangeWavResult = decode(record.result.unwrap_or(Value::Null))?;
            let public = validate_stored_range_result(&input, &stored)?;
            let reference = artifact(&view.job_id, stored.artifact)?;
            Ok::<_, EditorError>((JobResultView::Audio(public.into()), vec![reference]))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind.ends_with(".v3") {
        let projected = (|| {
            use opencut_job_execution::historical_visual::{
                HistoricalVisualPreparedRange, HistoricalVisualPreparedRender,
                PreparedHistoricalVisualFrame,
            };
            let (legacy_kind, snapshot, sequence_id, lock) = match view.kind.as_str() {
                "render.project.v3" => {
                    let input: HistoricalVisualPreparedRender = decode(record.input.clone())?;
                    (
                        "render.project",
                        input.snapshot,
                        input.sequence_id,
                        input.asset_lock,
                    )
                }
                "preview.project_frame.v3" => {
                    let input: PreparedHistoricalVisualFrame = decode(record.input.clone())?;
                    (
                        "preview.composited_frame",
                        input.snapshot,
                        input.sequence_id,
                        input.asset_lock,
                    )
                }
                "preview.project_range.v3" => {
                    let input: HistoricalVisualPreparedRange = decode(record.input.clone())?;
                    (
                        "preview.project_range",
                        input.snapshot,
                        input.sequence_id,
                        input.asset_lock,
                    )
                }
                _ => return Err(bad_result()),
            };
            opencut_editor_core::historical_visual::validate_project_historical_visual(
                &snapshot.project,
            )
            .map_err(|_| bad_result())?;
            let base = opencut_contracts::ProjectSnapshot {
                project: snapshot.project.base.clone(),
                revision: snapshot.revision.clone(),
            };
            let scope = if view.kind == "preview.project_frame.v3" {
                opencut_editor_core::dependencies::AssetScope::Visual
            } else {
                opencut_editor_core::dependencies::AssetScope::All
            };
            let required =
                opencut_editor_core::dependencies::required_asset_ids(&base, &sequence_id, scope)
                    .map_err(|_| bad_result())?;
            let required: std::collections::BTreeSet<_> =
                required.iter().map(String::as_str).collect();
            let observed: std::collections::BTreeSet<_> = lock
                .assets
                .iter()
                .map(|item| item.asset_id.as_str())
                .collect();
            if lock.version != 1
                || lock.revision != snapshot.revision
                || lock.sequence_id != sequence_id
                || lock.assets.len() != observed.len()
                || required != observed
                || lock.assets.iter().any(|item| {
                    match snapshot
                        .project
                        .base
                        .assets
                        .iter()
                        .find(|asset| asset.id == item.asset_id)
                    {
                        Some(asset) => asset.version.as_ref().is_some_and(|version| {
                            version.content_hash != item.content_hash || version.size != item.size
                        }),
                        None => true,
                    }
                })
            {
                return Err(bad_result());
            }
            let (legacy, artifacts) = result(
                &view.job_id,
                legacy_kind,
                record.result.unwrap_or(Value::Null),
            )?;
            let output = match legacy {
                JobResultView::ProjectRender(data)
                    if data.revision == snapshot.revision
                        && data.asset_lock.as_ref() == Some(&lock) =>
                {
                    JobResultView::ProjectRender(data)
                }
                JobResultView::CompositedFrame(data)
                    if data.revision == snapshot.revision
                        && data.sequence_id == sequence_id
                        && data.asset_lock.as_ref() == Some(&lock)
                        && record.input.get("requested_time")
                            == serde_json::to_value(&data.requested_time).ok().as_ref()
                        && record.input.get("alignment")
                            == serde_json::to_value(data.alignment).ok().as_ref() =>
                {
                    JobResultView::CompositedFrame(data)
                }
                JobResultView::ProjectRange(data)
                    if data.revision == snapshot.revision
                        && data.sequence_id == sequence_id
                        && data.asset_lock.as_ref() == Some(&lock)
                        && record.input.get("requested_range")
                            == serde_json::to_value(&data.requested_range).ok().as_ref()
                        && record.input.get("alignment")
                            == serde_json::to_value(data.alignment).ok().as_ref() =>
                {
                    JobResultView::ProjectRange(data)
                }
                _ => return Err(bad_result()),
            };
            Ok::<_, EditorError>((output, artifacts))
        })();
        match projected {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded
        && matches!(
            view.kind.as_str(),
            "analysis.silence" | "analysis.shots" | "analysis.waveform"
        )
    {
        let projected = (|| {
            let input = measured.ok_or_else(bad_result)?;
            let value = record.result.unwrap_or(Value::Null);
            match view.kind.as_str() {
                "analysis.silence" => analysis_result(
                    &input,
                    &view.kind,
                    value,
                    media::validate_public_silence_result,
                    AssetAnalysisData::Silence,
                ),
                "analysis.shots" => analysis_result(
                    &input,
                    &view.kind,
                    value,
                    media::validate_public_shots_result,
                    AssetAnalysisData::Shots,
                ),
                _ => analysis_result(
                    &input,
                    &view.kind,
                    value,
                    media::validate_public_waveform_result,
                    AssetAnalysisData::Waveform,
                ),
            }
        })();
        match projected {
            Ok(result) => view.result = Some(result),
            Err(e) => view.result_unavailable = Some(public_failure(&view.job_id, &e.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "analysis.speaker_turns" {
        let projected = (|| {
            let input = speaker.ok_or_else(bad_result)?;
            let result: jobs::SpeakerTurnsJobResult = decode(record.result.unwrap_or(Value::Null))?;
            let locked = input.asset_lock.assets.first().ok_or_else(bad_result)?;
            if input.language != "en"
                || input.asset_lock.assets.len() != 1
                || input.asset_lock.revision != input.snapshot.revision
                || input.asset_lock.sequence_id != input.sequence_id
                || locked.asset_id != input.asset_id
                || result.project_id != input.snapshot.project.id
                || result.revision != input.snapshot.revision
                || result.asset_id != input.asset_id
                || &result.asset_version != locked
                || !media::validate_public_speaker_turns_result(&result.analysis)
            {
                return Err(bad_result());
            }
            Ok::<_, EditorError>(JobResultView::AssetAnalysis(AssetAnalysisResult {
                project_id: result.project_id,
                revision: result.revision,
                asset_id: result.asset_id,
                asset_version: result.asset_version,
                options: AssetAnalysisOptions::SpeakerTurns {},
                data: AssetAnalysisData::SpeakerTurns(result.analysis),
            }))
        })();
        match projected {
            Ok(result) => view.result = Some(result),
            Err(e) => view.result_unavailable = Some(public_failure(&view.job_id, &e.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "analysis.sequence_transcribe" {
        let projected = (|| {
            let input: opencut_job_execution::historical_visual::PreparedSequenceTranscribe =
                decode(record.input.clone())?;
            let result: jobs::SequenceTranscribeResult =
                decode(record.result.unwrap_or(Value::Null))?;
            let composition = opencut_composition::historical_visual::compile_historical_visual(
                &input.snapshot,
                &input.sequence_id,
                opencut_composition::CompositionScope::AudioVisual,
            )
            .map_err(|_| bad_result())?;
            let duration = composition.base.timeline.duration;
            let expected = opencut_editor_core::dependencies::required_asset_ids(
                &opencut_contracts::ProjectSnapshot {
                    project: input.snapshot.project.base.clone(),
                    revision: input.snapshot.revision.clone(),
                },
                &input.sequence_id,
                opencut_editor_core::dependencies::AssetScope::All,
            )
            .map_err(|_| bad_result())?;
            let observed: std::collections::BTreeSet<_> = input
                .asset_lock
                .assets
                .iter()
                .map(|asset| asset.asset_id.as_str())
                .collect();
            let expected: std::collections::BTreeSet<_> =
                expected.iter().map(String::as_str).collect();
            if observed != expected
                || input.asset_lock.assets.len() != expected.len()
                || input.asset_lock.assets.iter().any(|locked| {
                    input
                        .snapshot
                        .project
                        .base
                        .assets
                        .iter()
                        .find(|asset| asset.id == locked.asset_id)
                        .and_then(|asset| asset.version.as_ref())
                        .is_some_and(|version| {
                            version.content_hash != locked.content_hash
                                || version.size != locked.size
                        })
                })
            {
                return Err(bad_result());
            }
            if input.asset_lock.version != 1
                || input.asset_lock.revision != input.snapshot.revision
                || input.asset_lock.sequence_id != input.sequence_id
                || result.project_id != input.snapshot.project.base.id
                || result.revision != input.snapshot.revision
                || result.sequence_id != input.sequence_id
                || result.asset_lock != input.asset_lock
                || result.duration
                    != (opencut_contracts::Time {
                        ticks: duration.ticks.to_string(),
                        timebase: duration.timebase,
                    })
                || !media::validate_public_transcription_result(&result.transcription)
            {
                return Err(bad_result());
            }
            Ok::<_, EditorError>(JobResultView::SceneTranscription(result.into()))
        })();
        match projected {
            Ok(result) => view.result = Some(result),
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "analysis.transcribe" {
        let projected = (|| {
            let input = analysis.ok_or_else(bad_result)?;
            let result: jobs::TranscribeJobResult = decode(record.result.unwrap_or(Value::Null))?;
            let locked = input.asset_lock.assets.first().ok_or_else(bad_result)?;
            if input.asset_lock.assets.len() != 1
                || input.asset_lock.revision != input.snapshot.revision
                || input.asset_lock.sequence_id != input.sequence_id
                || locked.asset_id != input.asset_id
                || result.project_id != input.snapshot.project.id
                || result.revision != input.snapshot.revision
                || result.asset_id != input.asset_id
                || &result.asset_version != locked
                || !media::validate_public_transcription_result(&result.transcription)
            {
                return Err(bad_result());
            }
            Ok::<_, EditorError>(JobResultView::AssetAnalysis(AssetAnalysisResult {
                project_id: result.project_id,
                revision: result.revision,
                asset_id: result.asset_id,
                asset_version: result.asset_version,
                options: AssetAnalysisOptions::Transcribe {
                    language: input.language.clone(),
                    word_timestamps: input.word_timestamps,
                },
                data: AssetAnalysisData::Transcribe(result.transcription),
            }))
        })();
        match projected {
            Ok(result) => view.result = Some(result),
            Err(e) => view.result_unavailable = Some(public_failure(&view.job_id, &e.code)),
        }
    } else if record.state == JobState::Succeeded && view.kind == "media.import" {
        let projected = (|| {
            let input = import.ok_or_else(bad_result)?;
            let result: jobs::MediaImportJobResult = decode(record.result.unwrap_or(Value::Null))?;
            crate::uploads::validate_import_result(&input, &view.job_id, &result)
                .map_err(|_| bad_result())?;
            Ok::<_, EditorError>(JobResultView::MediaImport(result))
        })();
        match projected {
            Ok(result) => view.result = Some(result),
            Err(e) => view.result_unavailable = Some(public_failure(&view.job_id, &e.code)),
        }
    } else if record.state == JobState::Succeeded {
        match result(
            &view.job_id,
            &view.kind,
            record.result.unwrap_or(Value::Null),
        ) {
            Ok((result, artifacts)) => {
                view.result = Some(result);
                view.artifacts = artifacts;
            }
            Err(error) => view.result_unavailable = Some(public_failure(&view.job_id, &error.code)),
        }
    } else if record.state == JobState::Failed {
        view.failure = Some(public_failure(
            &view.job_id,
            record
                .error
                .as_ref()
                .and_then(|e| e.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("JOB_FAILED"),
        ));
    }
    let canonical = public_kind(&view.kind).unwrap_or("unknown");
    if view
        .result
        .as_ref()
        .is_some_and(|result| result.kind() != canonical)
    {
        view.result = None;
        view.artifacts.clear();
        view.result_unavailable = Some(public_failure(&view.job_id, "INVALID_STORED_RESULT"));
    }
    view.kind = canonical.into();
    view
}

pub fn change(e: crate::JobEvent) -> JobChange {
    let kind = match e.event_type.as_str() {
        "queued" => JobChangeKind::Queued,
        "running" | "started" => JobChangeKind::Started,
        "progress" => JobChangeKind::Progress,
        "succeeded" => JobChangeKind::Succeeded,
        "failed" => JobChangeKind::Failed,
        "cancel_requested" => JobChangeKind::CancelRequested,
        "cancelled" => JobChangeKind::Cancelled,
        "recovered" => JobChangeKind::Recovered,
        _ => JobChangeKind::Unknown,
    };
    let progress = if e.event_type == "progress" {
        serde_json::from_value::<JobProgress>(e.payload)
            .ok()
            .map(progress)
    } else {
        None
    };
    JobChange {
        event_id: e.event_id,
        job_id: e.job_id,
        event_type: kind,
        state: e.state,
        progress,
        created_at: e.created_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn record() -> JobRecord {
        JobRecord {
            job_id: format!("job_{}", "a".repeat(32)),
            kind: "media.probe".into(),
            state: JobState::Succeeded,
            input: json!({"snapshot":{"private":"/private/secret"}}),
            result: Some(
                json!({"input":"/private/secret", "format_name":"wav", "duration":null,"bit_rate":null,"streams":[],"future":{"private":"/private/secret"}}),
            ),
            error: None,
            attempt: 42,
            lease_owner: Some("worker-secret".into()),
            lease_until: Some(123),
            cancel_requested: false,
            progress: None,
        }
    }
    #[test]
    fn projection_is_an_allowlist_and_unknown_result_fields_do_not_leak() {
        let view = project(record());
        assert!(matches!(view.result, Some(JobResultView::Probe(_))));
        let encoded = serde_json::to_string(&view).unwrap();
        for private in [
            "/private/secret",
            "worker-secret",
            "lease_until",
            "attempt",
            "snapshot",
            "future",
        ] {
            assert!(!encoded.contains(private), "{encoded}");
        }
    }
    #[test]
    fn invalid_or_future_stored_results_remain_historical_success_with_explicit_unavailability() {
        let mut invalid = record();
        invalid.result = Some(json!({"input":"/private/secret"}));
        let view = project(invalid);
        assert_eq!(view.state, JobState::Succeeded);
        assert!(view.result.is_none());
        assert_eq!(
            view.result_unavailable.unwrap().code,
            "INVALID_STORED_RESULT"
        );
        let mut future = record();
        future.kind = "future.job".into();
        let view = project(future);
        assert_eq!(view.state, JobState::Succeeded);
        assert_eq!(
            view.result_unavailable.unwrap().code,
            "UNSUPPORTED_JOB_KIND"
        );
    }
    #[test]
    fn raw_failure_codes_text_and_progress_stage_are_not_public_diagnostics() {
        let mut failed = record();
        failed.state = JobState::Failed;
        failed.error = Some(
            json!({"code":"/private/secret", "message":"/private/secret", "details":{"private":"/private/secret"}}),
        );
        failed.progress = Some(JobProgress {
            stage: "/private/secret".into(),
            fraction: None,
            media_time: None,
        });
        let view = project(failed);
        assert_eq!(view.failure.as_ref().unwrap().code, "JOB_FAILED");
        assert!(view.result.is_none());
        assert!(
            !serde_json::to_string(&view)
                .unwrap()
                .contains("/private/secret")
        );
    }
    #[test]
    fn known_capacity_and_media_budget_errors_are_public_without_native_details() {
        for code in [
            "DATABASE_CAPACITY_EXHAUSTED",
            "AUDIO_BUDGET_EXCEEDED",
            "VISUAL_BUDGET_EXCEEDED",
            "VIDEO_BUDGET_EXCEEDED",
            "MISSING_PLUGIN_DEPENDENCY",
        ] {
            let public = public_error(EditorError::new(code, "/private/secret"));
            assert_eq!(public.code, code);
            assert!(
                !serde_json::to_string(&public)
                    .unwrap()
                    .contains("/private/secret")
            );

            let mut failed = record();
            failed.state = JobState::Failed;
            failed.error = Some(json!({"code":code,"message":"/private/secret"}));
            let view = project(failed);
            assert_eq!(view.failure.as_ref().unwrap().code, code);
            assert!(
                !serde_json::to_string(&view)
                    .unwrap()
                    .contains("/private/secret")
            );
        }
    }
    #[test]
    fn transient_database_lock_remains_retryable_without_leaking_storage_details() {
        let public = public_error(EditorError::new("DATABASE_BUSY", "/private/secret"));
        assert_eq!(public.code, "DATABASE_BUSY");
        assert!(!public.message.contains("/private/secret"));
    }
    #[test]
    fn rejected_submission_has_no_fictitious_diagnostic_job_and_known_features_keep_a_safe_code() {
        let unknown = public_submission_error(EditorError::new(
            "UNKNOWN_NATIVE_FAILURE",
            "/private/secret",
        ));
        assert!(unknown.message.contains("no task was created"));
        assert!(!unknown.message.contains("diagnostic"));
        for code in ["UNSUPPORTED_V4", "UNSUPPORTED_RENDER_FEATURE"] {
            let public = public_submission_error(EditorError::new(code, "/private/secret"));
            assert_eq!(public.code, "UNSUPPORTED_RENDER_FEATURE");
            assert!(!public.message.contains("/private/secret"));
        }
    }

    #[test]
    fn transcription_result_is_bound_to_the_prepared_asset_and_filters_private_fields() {
        let mut stored = record();
        stored.kind = "analysis.transcribe".into();
        let hash = "b".repeat(64);
        stored.input = json!({
            "snapshot": {"revision":"rev-1","project":{
                "id":"project-1","name":"Fixture","schema_version":2,
                "default_sequence_id":"sequence-1","assets":[{
                    "id":"asset-1","name":"Voice","kind":"audio","uri":"/private/secret.wav",
                    "duration":null,"metadata":{}
                }],"sequences":[{"id":"sequence-1","name":"Main","width":1920,
                    "height":1080,"fps":{"numerator":24,"denominator":1},
                    "tracks":[],"caption_cues":[]}]}
            },
            "sequence_id":"sequence-1","asset_id":"asset-1","language":"en",
            "asset_lock":{"version":1,"revision":"rev-1","sequence_id":"sequence-1",
                "assets":[{"asset_id":"asset-1","content_hash":hash,"size":16}]}
        });
        stored.result = Some(json!({
            "project_id":"project-1","revision":"rev-1","asset_id":"asset-1",
            "asset_version":{"asset_id":"asset-1","content_hash":hash,"size":16},
            "transcription":{"language":"en","provider":"whisper.cpp","model":"ggml-tiny.bin",
                "segments":[{"start":{"ticks":"0","timebase":1000},
                    "end":{"ticks":"250","timebase":1000},"text":"Hello","words":[]}]},
            "private_path":"/private/secret.wav"
        }));
        serde_json::from_value::<opencut_job_execution::PreparedTranscribeInput>(
            stored.input.clone(),
        )
        .unwrap();
        let view = project(stored.clone());
        assert_eq!(view.result.as_ref().unwrap().kind(), "analysis.asset");
        assert_eq!(view.target.as_ref().unwrap().project_id, "project-1");
        assert!(
            !serde_json::to_string(&view)
                .unwrap()
                .contains("/private/secret")
        );
        stored.result.as_mut().unwrap()["asset_version"]["content_hash"] = json!("c".repeat(64));
        let invalid = project(stored);
        assert!(invalid.result.is_none());
        assert_eq!(
            invalid.result_unavailable.unwrap().code,
            "INVALID_STORED_RESULT"
        );
    }
}
