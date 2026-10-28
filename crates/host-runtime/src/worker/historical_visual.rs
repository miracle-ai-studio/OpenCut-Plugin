//! Schema 3 execution inside the existing leased worker attempt and output admission.
use super::*;
use opencut_contracts::historical_visual::HistoricalVisualSnapshot;
use opencut_job_execution::historical_visual::{
    HistoricalVisualPreparedRange, HistoricalVisualPreparedRender, PreparedHistoricalVisualFrame,
};
use opencut_project_render::historical_visual::{
    HistoricalVisualProjectRenderAdapter, HistoricalVisualResolvedProjectFrameRequest,
    HistoricalVisualResolvedProjectRangeRequest, HistoricalVisualResolvedProjectRenderRequest,
    HistoricalVisualResolvedProjectSnapshot,
};

struct ResolveContext<'a> {
    root: &'a Path,
    directory: &'a Path,
    cancel: &'a CancellationToken,
    progress: &'a mut dyn FnMut(ProgressUpdate),
}

fn resolved(
    context: ResolveContext<'_>,
    snapshot: &HistoricalVisualSnapshot,
    sequence_id: &str,
    lock: &AssetLockManifest,
    visual_only: bool,
) -> Result<HistoricalVisualResolvedProjectSnapshot, EditorError> {
    let base = ProjectSnapshot {
        project: snapshot.project.base.clone(),
        revision: snapshot.revision.clone(),
    };
    let scope = if visual_only {
        InputScope::VisualFrame(sequence_id)
    } else {
        InputScope::Full(sequence_id)
    };
    let verified = prepare_inputs(
        context.root,
        context.directory,
        &base,
        scope,
        Some(lock),
        context.cancel,
        context.progress,
    )?;
    let mut private = snapshot.clone();
    private.project.base = verified.project;
    let resolver = |asset: &opencut_contracts::Asset| Ok(PathBuf::from(&asset.uri));
    let result = if visual_only {
        HistoricalVisualProjectRenderAdapter::resolve_visual_assets(private, sequence_id, resolver)
    } else {
        HistoricalVisualProjectRenderAdapter::resolve_assets(private, sequence_id, resolver)
    };
    result.map_err(|error| EditorError::new(error.code, error.message))
}

pub(super) fn execute(
    root: &Path,
    directory: &Path,
    backend: &FfmpegBackend,
    job: &JobRecord,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<Value, EditorError> {
    match job.kind.as_str() {
        "analysis.sequence_transcribe" => {
            let input: opencut_job_execution::historical_visual::PreparedSequenceTranscribe =
                parse(job.input.clone())?;
            let model = std::env::var_os("OPENCUT_WHISPER_MODEL")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    EditorError::new(
                        "CAPABILITY_UNAVAILABLE",
                        "Local transcription model is not configured",
                    )
                })?;
            let executable = std::env::var_os("OPENCUT_WHISPER_CLI")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "whisper-cli".into());
            let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
            let engine = WhisperBackend::new(executable, model, ffmpeg).map_err(media_error)?;
            #[cfg(unix)]
            let engine = if let Some(admission) = backend.output_admission() {
                engine.with_output_admission(admission, directory.to_path_buf())
            } else {
                engine
            };
            let private_inputs = PrivateInputs(directory.join("inputs"));
            let snapshot = resolved(
                ResolveContext {
                    root,
                    directory: &private_inputs.0,
                    cancel,
                    progress,
                },
                &input.snapshot,
                &input.sequence_id,
                &input.asset_lock,
                false,
            )?;
            let output = directory.join("scene-audio.wav");
            let private_audio = PreviewFiles {
                intermediate: None,
                output: output.clone(),
                keep_output: false,
            };
            let plan = HistoricalVisualProjectRenderAdapter::plan_resolved(
                &HistoricalVisualResolvedProjectRenderRequest {
                    resolved: &snapshot,
                    output: output.clone(),
                    container: Container::Mp4,
                    video_codec: VideoCodec::H264,
                    overwrite: false,
                },
            )
            .map_err(|error| EditorError::new(error.code, error.message))?;
            backend
                .render_timeline_audio(&plan.media_request.base, cancel, progress)
                .map_err(media_error)?;
            let transcription = engine
                .transcribe(
                    &TranscribeRequest {
                        input: output,
                        language: input.language,
                        word_timestamps: false,
                    },
                    cancel,
                )
                .map_err(media_error)?;
            if cancel.is_cancelled() {
                return Err(EditorError::new(
                    "JOB_CANCELLED",
                    "Transcription was cancelled",
                ));
            }
            drop(private_audio);
            Ok(json!(opencut_job_contracts::SequenceTranscribeResult {
                project_id: input.snapshot.project.base.id,
                revision: input.snapshot.revision,
                sequence_id: input.sequence_id,
                asset_lock: input.asset_lock,
                duration: opencut_contracts::Time {
                    ticks: plan.media_request.base.duration.ticks.to_string(),
                    timebase: plan.media_request.base.duration.timebase
                },
                transcription,
            }))
        }
        "render.project.v3" => {
            let input: HistoricalVisualPreparedRender = parse(job.input.clone())?;
            let private_inputs = PrivateInputs(directory.join("inputs"));
            let snapshot = resolved(
                ResolveContext {
                    root,
                    directory: &private_inputs.0,
                    cancel,
                    progress,
                },
                &input.snapshot,
                &input.sequence_id,
                &input.asset_lock,
                false,
            )?;
            let output = directory.join(output_name(input.container));
            let plan = HistoricalVisualProjectRenderAdapter::plan_resolved(
                &HistoricalVisualResolvedProjectRenderRequest {
                    resolved: &snapshot,
                    output: output.clone(),
                    container: input.container,
                    video_codec: input.video_codec,
                    overwrite: false,
                },
            )
            .map_err(|error| EditorError::new(error.code, error.message))?;
            let rendered = backend
                .render_timeline_historical_visual(&plan.media_request, cancel, progress)
                .map_err(media_error)?;
            if cancel.is_cancelled() {
                return Err(EditorError::new(
                    "JOB_CANCELLED",
                    "Schema 3 render was cancelled",
                ));
            }
            let result = ProjectRenderJobResult {
                revision: plan.revision,
                asset_lock: Some(input.asset_lock),
                asset_locking: AssetLockingMode::MediaContentLocked,
                media: rendered.try_map_paths(public_path)?,
                artifact: artifact(&output, output_mime(input.container))?,
            };
            result.validate()?;
            Ok(json!(result))
        }
        "preview.project_frame.v3" => {
            let input: PreparedHistoricalVisualFrame = parse(job.input.clone())?;
            let private_inputs = PrivateInputs(directory.join("inputs"));
            let snapshot = resolved(
                ResolveContext {
                    root,
                    directory: &private_inputs.0,
                    cancel,
                    progress,
                },
                &input.snapshot,
                &input.sequence_id,
                &input.asset_lock,
                true,
            )?;
            let output = directory.join("frame.png");
            let mut private_files = PreviewFiles {
                intermediate: None,
                output: output.clone(),
                keep_output: false,
            };
            let plan = HistoricalVisualProjectRenderAdapter::plan_frame_resolved(
                &HistoricalVisualResolvedProjectFrameRequest {
                    resolved: &snapshot,
                    requested_time: input.requested_time.clone(),
                    alignment: input.alignment,
                    output: output.clone(),
                    overwrite: false,
                },
            )
            .map_err(|error| EditorError::new(error.code, error.message))?;
            let rendered = backend
                .render_timeline_frame_historical_visual(&plan.media_request, cancel, progress)
                .map_err(media_error)?;
            let observed = RationalTime::new(
                rendered.actual_sequence_time.ticks,
                rendered.actual_sequence_time.timebase,
            )?;
            if observed.cmp_exact(RationalTime::try_from(&plan.actual_sequence_time)?)
                != std::cmp::Ordering::Equal
            {
                return Err(EditorError::new(
                    "ARTIFACT_VERIFICATION_FAILED",
                    "Schema 3 frame timestamp differs",
                ));
            }
            if cancel.is_cancelled() {
                return Err(EditorError::new(
                    "JOB_CANCELLED",
                    "Schema 3 frame was cancelled",
                ));
            }
            let result = CompositedProjectFrameJobResult {
                revision: plan.revision,
                sequence_id: plan.sequence_id,
                requested_time: input.requested_time,
                actual_sequence_time: plan.actual_sequence_time,
                alignment: input.alignment,
                quality: CompositedProjectFrameQuality::CompositedProjectFrame,
                asset_lock: Some(input.asset_lock),
                asset_locking: AssetLockingMode::MediaContentLocked,
                width: rendered.width,
                height: rendered.height,
                text_layouts: rendered.text_layouts,
                artifact: artifact(&output, "image/png")?,
            };
            result.validate()?;
            private_files.keep_output = true;
            Ok(json!(result))
        }
        "preview.project_range.v3" => {
            let input: HistoricalVisualPreparedRange = parse(job.input.clone())?;
            let private_inputs = PrivateInputs(directory.join("inputs"));
            let snapshot = resolved(
                ResolveContext {
                    root,
                    directory: &private_inputs.0,
                    cancel,
                    progress,
                },
                &input.snapshot,
                &input.sequence_id,
                &input.asset_lock,
                false,
            )?;
            let output = directory.join("preview.mp4");
            let mut private_files = PreviewFiles {
                intermediate: None,
                output: output.clone(),
                keep_output: false,
            };
            let plan = HistoricalVisualProjectRenderAdapter::plan_range_resolved(
                &HistoricalVisualResolvedProjectRangeRequest {
                    resolved: &snapshot,
                    requested_range: input.requested_range.clone(),
                    alignment: input.alignment,
                    output: output.clone(),
                    overwrite: false,
                },
            )
            .map_err(|error| EditorError::new(error.code, error.message))?;
            let rendered = backend
                .render_timeline_range_historical_visual(&plan.media_request, cancel, progress)
                .map_err(media_error)?;
            if cancel.is_cancelled() {
                return Err(EditorError::new(
                    "JOB_CANCELLED",
                    "Schema 3 range was cancelled",
                ));
            }
            let result = ProjectRangeJobResult {
                revision: plan.revision,
                sequence_id: plan.sequence_id,
                requested_range: input.requested_range,
                actual_sequence_range: plan.actual_sequence_range,
                alignment: input.alignment,
                quality: ProjectRangeQuality::EncodedProjectRange,
                asset_lock: Some(input.asset_lock),
                asset_locking: AssetLockingMode::MediaContentLocked,
                media: rendered.try_map_paths(public_path)?,
                artifact: artifact(&output, "video/mp4")?,
            };
            result.validate()?;
            private_files.keep_output = true;
            Ok(json!(result))
        }
        _ => Err(EditorError::new(
            "UNSUPPORTED_JOB_KIND",
            "Not a schema 3 project job",
        )),
    }
}
