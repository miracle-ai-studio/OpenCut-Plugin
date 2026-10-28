//! Whole-scene V4 shared media/text/caption/graphic RGBA + Classic PCM, encoded in one private
//! attempt. Output remains tentative until the normal queue publication fence.
use super::{
    media_output::TaskOutput,
    output_attempt::{
        attempt_directory, cancelled, check_attempt, check_output_name, error, file_hash, io_error,
    },
};
use crate::{classic_audio_runtime, classic_visual_runtime};
use opencut_contracts::EditorError;
use opencut_editor_application::{
    budget::Writer,
    classic_audio::{ClassicAudioResources, ClassicAudioSession},
    classic_blend_frame::ClassicBlendFrameSession,
    classic_caption_frame::ClassicCaptionFrameSession,
    classic_graphic_frame::ClassicGraphicFrameSession,
    classic_mask_frame::ClassicMaskFrameSession,
    classic_scene_frame::ClassicSceneFrameSession,
    classic_text_frame::ClassicTextFrameSession,
    classic_visual::{ClassicVisualResources, ClassicVisualRgbaSink},
    export_video as export,
    font_import::FontImportPolicy,
    media_video_jobs::{self as video, PreparedMediaVideoMp4, StoredVideoMp4Result},
    scene_blend_video_jobs as blend, scene_caption_video_jobs as caption,
    scene_graphic_video_jobs as graphic, scene_mask_video_jobs as mask, scene_video_jobs as scene,
};
use opencut_job_contracts::{
    ArtifactDescriptor,
    media_frame::{MediaFrameArtifactMetadata, MediaFrameExecutionSummary},
    media_video::*,
    scene_audio::*,
};
use opencut_media::{CancellationToken, ProgressStage, ProgressUpdate};
use opencut_native_services::{
    classic_audio::{ClassicAudioWavSink, VerifiedClassicAudioResources},
    classic_video::{ClassicVideoEncoder, ClassicVideoEncoderConfig, ClassicVideoProcess},
    classic_visual::VerifiedClassicTextResources,
    native_audio::NativeAudioProcess,
};
use opencut_sqlite_store::{EditorApplication, JobRecord};
use opencut_storage_budget::{AttemptLease, Budget};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    collections::BTreeSet,
    ffi::OsStr,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[cfg(unix)]
pub(super) fn execute(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<Value, EditorError> {
    execute_with_profile(
        database,
        root,
        worker,
        job,
        cancel,
        held,
        video::VideoFrameProfile::TextV2,
        progress,
    )
}
#[cfg(unix)]
pub(super) fn execute_with_profile(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
    profile: video::VideoFrameProfile,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<Value, EditorError> {
    execute_kernel(
        database,
        root,
        worker,
        job,
        cancel,
        held,
        progress,
        Some(profile),
    )
}
#[cfg(unix)]
pub(super) fn execute_export(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<Value, EditorError> {
    execute_kernel(database, root, worker, job, cancel, held, progress, None)
}
#[cfg(unix)]
fn execute_kernel(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
    progress: &mut dyn FnMut(ProgressUpdate),
    selected_profile: Option<video::VideoFrameProfile>,
) -> Result<Value, EditorError> {
    let export_input: Option<export::PreparedExportVideo> = if selected_profile.is_none() {
        if !matches!(
            job.kind.as_str(),
            export::EXPORT_VIDEO_KIND | export::EXPORT_VIDEO_RANGE_KIND
        ) {
            return Err(error(
                "INVALID_ARGUMENT",
                "Configurable export kind differs",
            ));
        }
        Some(
            serde_json::from_value(job.input.clone())
                .map_err(|_| error("INVALID_ARGUMENT", "Invalid private configurable export"))?,
        )
    } else {
        None
    };
    let profile = selected_profile
        .unwrap_or_else(|| export::frame_profile(export_input.as_ref().unwrap().visual_profile));
    if export_input.is_none() && job.kind != profile.kind() {
        return Err(error(
            "INVALID_ARGUMENT",
            "Video kind differs from selected execution profile",
        ));
    }
    cancelled(cancel)?;
    let started = Instant::now();
    let input: PreparedMediaVideoMp4 = if let Some(input) = &export_input {
        input.execution.clone()
    } else {
        serde_json::from_value(job.input.clone())
            .map_err(|_| error("INVALID_ARGUMENT", "Invalid private V4 video preparation"))?
    };
    let prepared = if let Some(input) = &export_input {
        export::prepare(input)?
    } else {
        let fixed = video::validate_prepared_with_profile(&input, profile)?;
        video::PreparedVideoSelection {
            identity: fixed.identity,
            audio: Some(fixed.audio),
            asset_ids: fixed.asset_ids,
            font_ids: fixed.font_ids,
        }
    };
    if let Some(export) = &export_input {
        if export.range.is_some() != (job.kind == export::EXPORT_VIDEO_RANGE_KIND) {
            return Err(error(
                "INVALID_ARGUMENT",
                "Captured range differs from the registered job kind",
            ));
        }
    }
    let mapping = export_input
        .as_ref()
        .map(|i| export::range_mapping(i, &prepared))
        .transpose()?
        .flatten();
    let first_frame = mapping
        .as_ref()
        .map(|m| m.frames.start_frame.parse::<u64>())
        .transpose()
        .map_err(|_| error("INVALID_ARGUMENT", "Invalid first output frame"))?
        .unwrap_or(0);
    let app = EditorApplication::open_managed(database)?;
    let revalidate = || {
        if export_input.as_ref().is_some_and(|i| i.range.is_some()) {
            return app.revalidate_export_video_range(
                &job.job_id,
                worker,
                job.attempt,
                &input.authority,
            );
        }
        if export_input.is_some() {
            return app.revalidate_export_video(&job.job_id, worker, job.attempt, &input.authority);
        }
        match profile {
            video::VideoFrameProfile::TextV2 => {
                app.revalidate_scene_video_job(&job.job_id, worker, job.attempt, &input.authority)
            }
            video::VideoFrameProfile::CaptionV3 => app.revalidate_scene_caption_video_job(
                &job.job_id,
                worker,
                job.attempt,
                &input.authority,
            ),
            video::VideoFrameProfile::GraphicV1 => app.revalidate_scene_graphic_video_job(
                &job.job_id,
                worker,
                job.attempt,
                &input.authority,
            ),
            video::VideoFrameProfile::SceneV1 => app.revalidate_scene_scene_video_job(
                &job.job_id,
                worker,
                job.attempt,
                &input.authority,
            ),
            video::VideoFrameProfile::MaskV1 => app.revalidate_scene_mask_video_job(
                &job.job_id,
                worker,
                job.attempt,
                &input.authority,
            ),
            video::VideoFrameProfile::BlendV1 => app.revalidate_scene_blend_video_job(
                &job.job_id,
                worker,
                job.attempt,
                &input.authority,
            ),
        }
    };
    revalidate()?;
    let policy = &input.processing_policy;
    let execution_limit = policy
        .execution_limit()
        .ok_or_else(|| error("VIDEO_BUDGET_EXCEEDED", "Invalid captured video lifetime"))?;
    let deadline = Duration::from_millis(execution_limit.milliseconds());
    let last_progress = Arc::new(Mutex::new(Instant::now()));
    let count = prepared
        .identity
        .frame_count
        .parse::<u64>()
        .map_err(|_| error("INVALID_ARGUMENT", "Invalid fixed video frame count"))?;
    let host_sha256 =
        opencut_native_services::reproducibility::execution_identity(vec![])?.process_binary_sha256;
    let workspace = root
        .parent()
        .ok_or_else(|| error("STORAGE_BUDGET_ERROR", "Missing managed output workspace"))?;
    let directory = attempt_directory(root, job)?;
    let attempt = AttemptLease::acquire(&directory).map_err(io_error)?;
    check_attempt(&attempt, &directory)?;
    {
        let mut guard = held
            .lock()
            .map_err(|_| error("INTERNAL_ERROR", "Attempt lifetime guard unavailable"))?;
        if guard.is_some() {
            return Err(error("INTERNAL_ERROR", "Output attempt already held"));
        }
        *guard = Some(attempt.clone());
    }
    // Opt-in bounded stage diagnostics contain timings only, never plans or media.
    let profile_directory = std::env::var_os("OPENCUT_VIDEO_PROFILE_DIR");
    let checkpoint_calls = Cell::new(0u64);
    let checkpoint_time = Cell::new(Duration::ZERO);
    let checkpoint = || {
        let measured = profile_directory.as_ref().map(|_| Instant::now());
        let result = (|| {
            cancelled(cancel)?;
            check_attempt(&attempt, &directory)?;
            let elapsed = if execution_limit.renews_on_progress() {
                last_progress
                    .lock()
                    .map_err(|_| error("INTERNAL_ERROR", "Video progress clock unavailable"))?
                    .elapsed()
            } else {
                started.elapsed()
            };
            if elapsed >= deadline {
                return Err(error(
                    "VIDEO_TIME_LIMIT",
                    "Video attempt exceeded its captured total or no-progress limit",
                ));
            }
            revalidate()
        })();
        if let Some(start) = measured {
            checkpoint_calls.set(checkpoint_calls.get() + 1);
            checkpoint_time.set(checkpoint_time.get() + start.elapsed());
        }
        result
    };
    let work_checkpoint = |completed: bool| -> Result<(), EditorError> {
        checkpoint()?;
        if completed && execution_limit.renews_on_progress() {
            *last_progress
                .lock()
                .map_err(|_| error("INTERNAL_ERROR", "Video progress clock unavailable"))? =
                Instant::now();
        }
        Ok(())
    };
    checkpoint()?;
    progress(ProgressUpdate {
        stage: ProgressStage::ResolvingAssets,
        fraction: None,
        media_time: None,
    });
    let audio_name = OsStr::new("video-audio.wav");
    let budget = Budget::new(workspace);
    let writer = Writer {
        job_id: job.job_id.clone(),
        attempt: job.attempt,
    };
    let (audio_file, audio_summary, audio_decoder_evidence_v1, _audio_reservation) =
        if let Some(audio) = &prepared.audio {
            progress(ProgressUpdate {
                stage: ProgressStage::Decoding,
                fraction: None,
                media_time: None,
            });
            let audio_range = mapping.as_ref().and_then(|m| m.audio.as_ref());
            let audio_frames = if let Some(range) = audio_range {
                range
                    .frame_count
                    .parse::<u64>()
                    .map_err(|_| error("INVALID_ARGUMENT", "Invalid audio output length"))?
            } else {
                audio.duration_samples()
            };
            let audio_bytes = ClassicAudioWavSink::checked_bytes(48000, audio_frames)?;
            let _audio_reservation = budget.reserve_audio_wav(audio_bytes, writer.clone())?;
            let audio_file = attempt
                .create_readwrite_child(audio_name)
                .map_err(io_error)?;
            let mut audio_sink = ClassicAudioWavSink::new(audio_file, 48000, audio_frames)?;
            let mut audio_resources =
                VerifiedClassicAudioResources::new(app.asset_store_root(), audio, checkpoint)?;
            let audio_handles = audio_resources.handles();
            let mut audio_session = ClassicAudioSession::new_with_range(
                audio,
                policy.audio.max_pcm_bytes,
                policy.audio.max_encoded_bytes,
                audio_range.map(
                    |r| opencut_editor_application::classic_audio::ClassicAudioRange {
                        start_sample: r.start_sample.clone(),
                        frame_count: r.frame_count.clone(),
                    },
                ),
            )?;
            let audio_config = classic_audio_runtime::configured(&policy.audio)?;
            let audio_execution = NativeAudioProcess {
                config: audio_config,
            }
            .execute(
                &mut audio_session,
                &mut audio_resources,
                &audio_handles,
                &mut audio_sink,
            )?;
            audio_resources.checkpoint()?;
            let audio_file = audio_sink.finalize()?;
            check_output_name(&attempt, audio_name, &audio_file)?;
            audio_file.sync_all().map_err(io_error)?;
            work_checkpoint(true)?;
            let audio_decoder_evidence_v1 =
                Some(audio_session.native_decoder_evidence(&audio_execution.receipt)?);
            let audio_summary =
                super::scene_audio::execution_summary(&audio_execution, host_sha256.clone())?;
            drop(audio_resources);
            (
                Some(audio_file),
                Some(audio_summary),
                audio_decoder_evidence_v1,
                Some(_audio_reservation),
            )
        } else {
            (None, None, None, None)
        };
    checkpoint()?;
    let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
    let ffprobe = crate::media_tool_runtime::ffprobe()?;
    let runtime_config = export_input
        .as_ref()
        .and_then(|v| v.options.runtime_fingerprint.as_ref())
        .map(|_| classic_visual_runtime::configured_scene(&policy.visual))
        .transpose()?;
    let verify_cache_runtime = || -> Result<(), EditorError> {
        if let Some(expected) = export_input
            .as_ref()
            .and_then(|v| v.options.runtime_fingerprint.as_ref())
        {
            if *expected
                != opencut_native_services::preview_cache::fingerprint(
                    runtime_config
                        .as_ref()
                        .expect("runtime fence configuration"),
                    &ffmpeg,
                    &ffprobe,
                )?
            {
                return Err(error(
                    "PREVIEW_RUNTIME_CHANGED",
                    "The cache renderer changed after admission",
                ));
            }
        }
        Ok(())
    };
    verify_cache_runtime()?;
    let output_name = OsStr::new(match export_input.as_ref().map(|i| i.options.format) {
        Some(export::ExportFormat::Webm) => "video.webm",
        _ => "video.mp4",
    });
    let output_path = directory.join(output_name);
    let admission = TaskOutput {
        budget: budget.clone(),
        attempt: attempt.clone(),
        directory: directory.clone(),
        writer,
    };
    let encoder_config = ClassicVideoEncoderConfig {
        ffmpeg: ffmpeg.clone(),
        ffprobe: ffprobe.clone(),
        output: output_path.clone(),
        audio: directory.join(audio_name),
        width: prepared.identity.width,
        height: prepared.identity.height,
        fps_numerator: prepared.identity.fps.numerator,
        fps_denominator: prepared.identity.fps.denominator,
        frame_count: count,
        max_output_bytes: if export_input
            .as_ref()
            .is_some_and(|v| v.options.resolution == export::RasterResolution::Preview540)
        {
            policy.max_output_bytes.min(8 * 1024 * 1024)
        } else {
            policy.max_output_bytes
        },
        max_rss_bytes: policy.visual.max_rss_bytes,
        execution_limit: execution_limit.remaining_after(started.elapsed()),
    };
    let mut encoder = if let Some(export) = &export_input {
        ClassicVideoEncoder::start_export(
            encoder_config,
            export.options.clone(),
            &admission,
            work_checkpoint,
        )?
    } else {
        ClassicVideoEncoder::start(encoder_config, &admission, work_checkpoint)?
    };
    let config = match profile {
        video::VideoFrameProfile::TextV2 => {
            classic_visual_runtime::configured_text(&policy.visual)?
        }
        video::VideoFrameProfile::CaptionV3 => {
            classic_visual_runtime::configured_caption(&policy.visual)?
        }
        video::VideoFrameProfile::GraphicV1 => {
            classic_visual_runtime::configured_graphic(&policy.visual)?
        }
        video::VideoFrameProfile::SceneV1 => {
            classic_visual_runtime::configured_scene(&policy.visual)?
        }
        video::VideoFrameProfile::MaskV1 => {
            classic_visual_runtime::configured_mask(&policy.visual)?
        }
        video::VideoFrameProfile::BlendV1 => {
            classic_visual_runtime::configured_blend(&policy.visual)?
        }
    };
    let remaining_limit = execution_limit.remaining_after(started.elapsed());
    let progress_clock = execution_limit
        .renews_on_progress()
        .then(|| last_progress.clone());
    let mut process = match profile {
        video::VideoFrameProfile::TextV2 => ClassicVideoProcess::start(
            config,
            count,
            remaining_limit,
            progress_clock.clone(),
            &mut || checkpoint(),
        )?,
        video::VideoFrameProfile::CaptionV3 => ClassicVideoProcess::start_caption(
            config,
            count,
            remaining_limit,
            progress_clock.clone(),
            &mut || checkpoint(),
        )?,
        video::VideoFrameProfile::GraphicV1 => ClassicVideoProcess::start_graphic(
            config,
            count,
            remaining_limit,
            progress_clock.clone(),
            &mut || checkpoint(),
        )?,
        video::VideoFrameProfile::SceneV1 => ClassicVideoProcess::start_scene(
            config,
            count,
            remaining_limit,
            progress_clock.clone(),
            &mut || checkpoint(),
        )?,
        video::VideoFrameProfile::MaskV1 => ClassicVideoProcess::start_mask(
            config,
            count,
            remaining_limit,
            progress_clock.clone(),
            &mut || checkpoint(),
        )?,
        video::VideoFrameProfile::BlendV1 => ClassicVideoProcess::start_blend(
            config,
            count,
            remaining_limit,
            progress_clock.clone(),
            &mut || checkpoint(),
        )?,
    };
    let mut receipts = Sha256::new();
    let mut rgba_bytes = 0u64;
    let mut font_inspections =
        opencut_native_services::classic_visual::VideoResourceCache::default();
    progress(ProgressUpdate {
        stage: ProgressStage::Encoding,
        fraction: Some(0.0),
        media_time: None,
    });
    let startup_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut frame_profile = Vec::new();
    for index in 0..count {
        let frame_started = Instant::now();
        let checkpoint_before = checkpoint_time.get();
        crate::audio_preview::yield_to_realtime_audio(database, || checkpoint())?;
        checkpoint()?;
        let at = video::grid_time(&prepared.identity.fps, first_frame + index)?;
        let selected = if export_input
            .as_ref()
            .is_some_and(|v| v.options.resolution == export::RasterResolution::Preview540)
        {
            video::PreparedVideoFrame::Scene(
                opencut_editor_application::scene_frame_jobs::prepare_preview(
                    &input.snapshot,
                    &input.sequence_id,
                    &at,
                )?,
            )
        } else {
            video::prepare_profiled_frame(&input.snapshot, &input.sequence_id, &at, profile)?
        };
        selected.admit(&policy.visual)?;
        let prepared_ms = frame_started.elapsed().as_secs_f64() * 1000.0;
        let resource_started = Instant::now();
        let resolving = Cell::new(true);
        let resource_checkpoint = || work_checkpoint(resolving.get());
        let mut resources = VerifiedClassicTextResources::new_video_frame(
            app.asset_store_root(),
            &selected,
            &FontImportPolicy::default(),
            resource_checkpoint,
            &mut font_inspections,
        )?;
        resolving.set(false);
        let resource_ms = resource_started.elapsed().as_secs_f64() * 1000.0;
        let render_started = Instant::now();
        let handles = resources.handles();
        let fonts = resources.font_handles();
        let sink: &mut dyn ClassicVisualRgbaSink = &mut encoder;
        let bytes = match &selected {
            video::PreparedVideoFrame::Text(frame) => {
                let mut session =
                    ClassicTextFrameSession::new(frame, &policy.visual, resources.coverage())?;
                let receipt =
                    process.render_frame(&mut session, &mut resources, &handles, &fonts, sink)?;
                serde_json::to_vec(&receipt)
            }
            video::PreparedVideoFrame::Caption(frame) => {
                let mut session =
                    ClassicCaptionFrameSession::new(frame, &policy.visual, resources.coverage())?;
                let receipt = process.render_caption_frame(
                    &mut session,
                    &mut resources,
                    &handles,
                    &fonts,
                    sink,
                )?;
                serde_json::to_vec(&receipt)
            }
            video::PreparedVideoFrame::Graphic(frame) => {
                let mut session =
                    ClassicGraphicFrameSession::new(frame, &policy.visual, resources.coverage())?;
                let receipt = process.render_graphic_frame(
                    &mut session,
                    &mut resources,
                    &handles,
                    &fonts,
                    sink,
                )?;
                serde_json::to_vec(&receipt)
            }
            video::PreparedVideoFrame::Scene(frame) => {
                let mut session =
                    ClassicSceneFrameSession::new(frame, &policy.visual, resources.coverage())?;
                let receipt = process.render_scene_frame(
                    &mut session,
                    &mut resources,
                    &handles,
                    &fonts,
                    sink,
                )?;
                serde_json::to_vec(&receipt)
            }
            video::PreparedVideoFrame::Mask(frame) => {
                let mut session =
                    ClassicMaskFrameSession::new(frame, &policy.visual, resources.coverage())?;
                let receipt = process.render_mask_frame(
                    &mut session,
                    &mut resources,
                    &handles,
                    &fonts,
                    sink,
                )?;
                serde_json::to_vec(&receipt)
            }
            video::PreparedVideoFrame::Blend(frame) => {
                let mut session =
                    ClassicBlendFrameSession::new(frame, &policy.visual, resources.coverage())?;
                let receipt = process.render_blend_frame(
                    &mut session,
                    &mut resources,
                    &handles,
                    &fonts,
                    sink,
                )?;
                serde_json::to_vec(&receipt)
            }
        }
        .map_err(|_| {
            error(
                "SERIALIZATION_ERROR",
                "Cannot hash complete verified video frame receipt",
            )
        })?;
        let render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
        encoder.finish_frame()?;
        resources.checkpoint()?;
        receipts.update(index.to_le_bytes());
        receipts.update((bytes.len() as u64).to_le_bytes());
        receipts.update(&bytes);
        rgba_bytes = rgba_bytes
            .checked_add(
                u64::from(prepared.identity.width) * u64::from(prepared.identity.height) * 4,
            )
            .ok_or_else(|| error("VIDEO_BUDGET_EXCEEDED", "Video byte counter overflow"))?;
        if profile_directory.is_some() && frame_profile.len() < 50 {
            frame_profile.push(serde_json::json!({"frame":index,"prepare_ms":prepared_ms,
                "resources_ms":resource_ms,"render_ms":render_ms,
                "total_ms":frame_started.elapsed().as_secs_f64()*1000.0,
                "checkpoints_ms":(checkpoint_time.get()-checkpoint_before).as_secs_f64()*1000.0}));
        }
        progress(ProgressUpdate {
            stage: ProgressStage::Encoding,
            fraction: Some(((index + 1) as f64 / count as f64) as f32),
            media_time: None,
        });
    }
    let finish_started = Instant::now();
    let (runtime, _quota) = process.finish(&mut || checkpoint())?;
    verify_cache_runtime()?;
    work_checkpoint(true)?;
    checkpoint()?;
    if let Some(audio_file) = &audio_file {
        check_output_name(&attempt, audio_name, audio_file)?;
    }
    progress(ProgressUpdate {
        stage: ProgressStage::Verifying,
        fraction: None,
        media_time: None,
    });
    let observed = encoder.finish(&admission)?;
    checkpoint()?;
    let mut file = attempt.open_readonly_child(output_name).map_err(io_error)?;
    let size = file.metadata().map_err(io_error)?.len();
    if size == 0 || size > policy.max_output_bytes {
        return Err(error(
            "ARTIFACT_VERIFICATION_FAILED",
            "MP4 differs from captured output admission",
        ));
    }
    let mut hash_checked = Instant::now();
    let content_hash = file_hash(&mut file, size, cancel, || {
        if hash_checked.elapsed() >= Duration::from_millis(50) {
            work_checkpoint(true)?;
            hash_checked = Instant::now();
        }
        Ok(())
    })?;
    work_checkpoint(true)?;
    file.sync_all().map_err(io_error)?;
    attempt.sync_directory().map_err(io_error)?;
    check_output_name(&attempt, output_name, &file)?;
    checkpoint()?;
    progress(ProgressUpdate {
        stage: ProgressStage::Verifying,
        fraction: Some(1.0),
        media_time: None,
    });
    if let Some(directory) = &profile_directory {
        // Diagnostics cannot affect publication; only administrator-selected paths.
        let name = format!("{}-{}.json", job.job_id, job.attempt);
        if let Ok(bytes) = serde_json::to_vec_pretty(&serde_json::json!({
            "startup_ms":startup_ms,"finish_ms":finish_started.elapsed().as_secs_f64()*1000.0,
            "total_ms":started.elapsed().as_secs_f64()*1000.0,"checkpoint_calls":checkpoint_calls.get(),
            "checkpoints_ms":checkpoint_time.get().as_secs_f64()*1000.0,"frames":frame_profile}))
        {
            let _ = std::fs::write(std::path::PathBuf::from(directory).join(name), bytes);
        }
    }
    let identity = runtime.identity;
    let visual_summary = MediaFrameExecutionSummary {
        host_sha256: host_sha256.clone(),
        runtime: opencut_job_contracts::media_frame::VisualRuntimeIdentity::Native(identity),
    };
    let result = if let Some(export_input) = &export_input {
        let format = export_input.options.format;
        let stored = export::StoredExportVideo {
            audio_decoder_evidence_v1,
            public: export::ExportVideoResult {
                schema_version: 2,
                identity: export::public_identity(export_input, &prepared),
                asset_lock: input.asset_lock.clone(),
                font_lock: input.font_lock.clone(),
                render_input_sha256: export::render_input_sha256(export_input)?,
                execution: export::ExportVideoExecution {
                    visual: visual_summary,
                    audio: audio_summary,
                    encoder: export::ExportVideoEncoderSummary {
                        backend: Some(observed.backend),
                        ffmpeg_sha256: observed.ffmpeg_sha256,
                        ffprobe_sha256: observed.ffprobe_sha256,
                        video_codec: format.video_codec().into(),
                        audio_codec: export_input
                            .options
                            .include_audio
                            .then(|| format.audio_codec().into()),
                        pixel_format: "yuv420p".into(),
                        rates: export::classic_export_rates(
                            &export_input.options,
                            prepared.identity.width,
                            prepared.identity.height,
                        )
                        .ok_or_else(|| {
                            error("INVALID_ARGUMENT", "Invalid captured export bitrate")
                        })?,
                        observed_video_frames: observed.video_frames.to_string(),
                        observed_video_duration_seconds: observed.video_duration_seconds,
                        observed_audio_duration_seconds: export_input
                            .options
                            .include_audio
                            .then_some(observed.audio_duration_seconds),
                    },
                    frame_receipts_sha256: format!("{:x}", receipts.finalize()),
                    frames_rendered: count.to_string(),
                    rgba_bytes: rgba_bytes.to_string(),
                },
                artifact: MediaFrameArtifactMetadata {
                    mime_type: format.mime_type().into(),
                    size_bytes: size.to_string(),
                    content_hash: content_hash.clone(),
                },
            },
            artifact: ArtifactDescriptor {
                path: output_path.into_os_string().into_string().map_err(|_| {
                    error(
                        "ARTIFACT_VERIFICATION_FAILED",
                        "Private export output is not UTF-8",
                    )
                })?,
                mime_type: format.mime_type().into(),
                size,
                content_hash,
            },
        };
        export::validate_stored_result(export_input, &stored)?;
        serde_json::to_value(stored).map_err(|_| {
            error(
                "SERIALIZATION_ERROR",
                "Cannot serialize configurable export result",
            )
        })?
    } else {
        let stored = StoredVideoMp4Result {
            audio_decoder_evidence_v1,
            public: MediaVideoMp4Result {
                schema_version: 1,
                identity: prepared.identity,
                asset_lock: input.asset_lock.clone(),
                font_lock: input.font_lock.clone(),
                render_input_sha256: video::render_input_sha256(
                    &input.snapshot,
                    &input.sequence_id,
                )?,
                execution: MediaVideoExecutionSummary {
                    visual: visual_summary,
                    audio: audio_summary.expect("Legacy output always prepares audio"),
                    encoder: MediaVideoEncoderSummary {
                        ffmpeg_sha256: observed.ffmpeg_sha256,
                        ffprobe_sha256: observed.ffprobe_sha256,
                        video_codec: "h264".into(),
                        audio_codec: "aac".into(),
                        pixel_format: "yuv420p".into(),
                        crf: 18,
                        audio_bitrate: 192000,
                        observed_video_frames: observed.video_frames.to_string(),
                        observed_video_duration_seconds: observed.video_duration_seconds,
                        observed_audio_duration_seconds: observed.audio_duration_seconds,
                    },
                    frame_receipts_sha256: format!("{:x}", receipts.finalize()),
                    frames_rendered: count.to_string(),
                    rgba_bytes: rgba_bytes.to_string(),
                },
                artifact: MediaFrameArtifactMetadata {
                    mime_type: "video/mp4".into(),
                    size_bytes: size.to_string(),
                    content_hash: content_hash.clone(),
                },
            },
            artifact: ArtifactDescriptor {
                path: output_path.into_os_string().into_string().map_err(|_| {
                    error(
                        "ARTIFACT_VERIFICATION_FAILED",
                        "Private video output is not UTF-8",
                    )
                })?,
                mime_type: "video/mp4".into(),
                size,
                content_hash,
            },
        };
        match profile {
            video::VideoFrameProfile::TextV2 => {
                video::validate_stored_result(&input, &stored)?;
                serde_json::to_value(stored)
            }
            video::VideoFrameProfile::CaptionV3 => {
                let public = stored.public;
                let stored = caption::StoredCaptionVideoMp4Result {
                    public: caption::CaptionVideoMp4Result {
                        schema_version: 2,
                        identity: caption::public_identity(&public.identity),
                        asset_lock: public.asset_lock,
                        font_lock: public.font_lock,
                        render_input_sha256: caption::render_input_sha256(
                            &input.snapshot,
                            &input.sequence_id,
                        )?,
                        execution: public.execution,
                        artifact: public.artifact,
                    },
                    artifact: stored.artifact,
                    audio_decoder_evidence_v1: stored.audio_decoder_evidence_v1,
                };
                caption::validate_stored_result(&input, &stored)?;
                serde_json::to_value(stored)
            }
            video::VideoFrameProfile::GraphicV1 => {
                let public = stored.public;
                let stored = graphic::StoredGraphicVideoMp4Result {
                    public: graphic::GraphicVideoMp4Result {
                        schema_version: 1,
                        identity: graphic::public_identity(&public.identity),
                        asset_lock: public.asset_lock,
                        font_lock: public.font_lock,
                        render_input_sha256: graphic::render_input_sha256(
                            &input.snapshot,
                            &input.sequence_id,
                        )?,
                        execution: public.execution,
                        artifact: public.artifact,
                    },
                    artifact: stored.artifact,
                    audio_decoder_evidence_v1: stored.audio_decoder_evidence_v1,
                };
                graphic::validate_stored_result(&input, &stored)?;
                serde_json::to_value(stored)
            }
            video::VideoFrameProfile::SceneV1 => {
                let public = stored.public;
                let stored = scene::StoredSceneVideoMp4Result {
                    public: scene::SceneVideoMp4Result {
                        schema_version: 1,
                        identity: scene::public_identity(&public.identity),
                        asset_lock: public.asset_lock,
                        font_lock: public.font_lock,
                        render_input_sha256: scene::render_input_sha256(
                            &input.snapshot,
                            &input.sequence_id,
                        )?,
                        execution: public.execution,
                        artifact: public.artifact,
                    },
                    artifact: stored.artifact,
                    audio_decoder_evidence_v1: stored.audio_decoder_evidence_v1,
                };
                scene::validate_stored_result(&input, &stored)?;
                serde_json::to_value(stored)
            }
            video::VideoFrameProfile::MaskV1 => {
                let public = stored.public;
                let stored = mask::StoredMaskVideoMp4Result {
                    public: mask::MaskVideoMp4Result {
                        schema_version: 1,
                        identity: mask::public_identity(&public.identity),
                        asset_lock: public.asset_lock,
                        font_lock: public.font_lock,
                        render_input_sha256: mask::render_input_sha256(
                            &input.snapshot,
                            &input.sequence_id,
                        )?,
                        execution: public.execution,
                        artifact: public.artifact,
                    },
                    artifact: stored.artifact,
                    audio_decoder_evidence_v1: stored.audio_decoder_evidence_v1,
                };
                mask::validate_stored_result(&input, &stored)?;
                serde_json::to_value(stored)
            }
            video::VideoFrameProfile::BlendV1 => {
                let public = stored.public;
                let stored = blend::StoredBlendVideoMp4Result {
                    public: blend::BlendVideoMp4Result {
                        schema_version: 1,
                        identity: blend::public_identity(&public.identity),
                        asset_lock: public.asset_lock,
                        font_lock: public.font_lock,
                        render_input_sha256: blend::render_input_sha256(
                            &input.snapshot,
                            &input.sequence_id,
                        )?,
                        execution: public.execution,
                        artifact: public.artifact,
                    },
                    artifact: stored.artifact,
                    audio_decoder_evidence_v1: stored.audio_decoder_evidence_v1,
                };
                blend::validate_stored_result(&input, &stored)?;
                serde_json::to_value(stored)
            }
        }
        .map_err(|_| {
            error(
                "SERIALIZATION_ERROR",
                "Cannot serialize private video result",
            )
        })?
    };
    checkpoint()?;
    if opencut_native_services::reproducibility::execution_identity(vec![])?.process_binary_sha256
        != host_sha256
    {
        return Err(error(
            "EXECUTION_IDENTITY_CHANGED",
            "Executing host changed during video processing",
        ));
    }
    check_output_name(&attempt, output_name, &file)?;
    Ok(result)
}

#[cfg(not(unix))]
pub(super) fn execute(
    _: &Path,
    _: &Path,
    _: &str,
    _: &JobRecord,
    _: &CancellationToken,
    _: &Mutex<Option<AttemptLease>>,
    _: &mut dyn FnMut(ProgressUpdate),
) -> Result<Value, EditorError> {
    Err(error(
        "CAPABILITY_UNAVAILABLE",
        "Persistent video output requires native Unix admission and process termination",
    ))
}

#[cfg(not(unix))]
pub(super) fn execute_with_profile(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
    _: video::VideoFrameProfile,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<Value, EditorError> {
    execute(database, root, worker, job, cancel, held, progress)
}

#[cfg(not(unix))]
pub(super) fn execute_export(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
    progress: &mut dyn FnMut(ProgressUpdate),
) -> Result<Value, EditorError> {
    execute(database, root, worker, job, cancel, held, progress)
}
