//! Configurable output shares fixed scene/frame/audio admission, authority and
//! resource capture with the frozen MP4 tools. Options are part of durable input.
use crate::media_frame_jobs::{valid_hash, valid_id, validate_authority};
use crate::{JobRecord, JobRequest, media_video_jobs as video};
use opencut_contracts::{EditorError, scene::*};
use opencut_job_contracts::ArtifactDescriptor;
pub use opencut_job_contracts::export_video::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedExportVideo {
    pub options: ExportVideoOptions,
    pub visual_profile: ExportVisualProfile,
    pub execution: video::PreparedMediaVideoMp4,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<VideoFrameRange>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredExportVideo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_decoder_evidence_v1: Option<crate::classic_audio::ClassicAudioDecoderEvidence>,
    pub public: ExportVideoResult,
    pub artifact: ArtifactDescriptor,
}
pub trait ExportVideoStore: video::MediaVideoJobSubmissionStore {
    fn accept_export_video(
        &mut self,
        request: JobRequest,
        prepared: PreparedExportVideo,
    ) -> Result<JobRecord, EditorError>;
}
fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", message)
}
pub fn frame_profile(profile: ExportVisualProfile) -> video::VideoFrameProfile {
    match profile {
        ExportVisualProfile::SceneV1 => video::VideoFrameProfile::SceneV1,
        ExportVisualProfile::TextV2 => video::VideoFrameProfile::TextV2,
        ExportVisualProfile::CaptionV3 => video::VideoFrameProfile::CaptionV3,
        ExportVisualProfile::GraphicV1 => video::VideoFrameProfile::GraphicV1,
        ExportVisualProfile::BlendV1 => video::VideoFrameProfile::BlendV1,
        ExportVisualProfile::MaskV1 => video::VideoFrameProfile::MaskV1,
    }
}
pub fn select_profile(
    snapshot: &EditorSnapshot,
    sequence: &str,
) -> Result<ExportVisualProfile, EditorError> {
    if !snapshot
        .project
        .sequences
        .iter()
        .any(|scene| scene.id == sequence)
    {
        return Err(invalid("Export scene does not exist"));
    }
    // One complete compositor accepts every new export. Content does not
    // select a development-era subset or a different public execution path.
    Ok(ExportVisualProfile::SceneV1)
}

// Exact interpretation of already accepted private inputs. This is not used
// to submit new work, retry under another renderer, or negotiate a public API.
fn original_profile(
    snapshot: &EditorSnapshot,
    sequence: &str,
) -> Result<ExportVisualProfile, EditorError> {
    let scene = snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == sequence)
        .ok_or_else(|| invalid("Export scene does not exist"))?;
    // Admission checks authored nodes too; disabled content cannot select a
    // narrower profile that would silently omit an unsupported kind later.
    let elements = || scene.tracks.iter().flat_map(|t| &t.elements);
    let blurred = matches!(&snapshot.project.settings, SettingsProfile::ClassicGlobal { settings } if matches!(settings.background, Background::MainTrackBlur { .. }));
    Ok(
        if blurred
            || snapshot
                .project
                .fonts
                .iter()
                .any(opencut_editor_core::scene::is_font_synthesis_resource)
            || elements().any(|e| {
                matches!(
                    e.content,
                    ElementContent::EffectLayer { .. } | ElementContent::Sticker { .. }
                ) || e.visual.as_ref().is_some_and(|v| {
                    !v.effects.is_empty()
                        || v.masks.len() > 1
                        || v.masks.iter().any(|m| {
                            m.inverted
                                || m.stroke.width_px > 0.
                                || !matches!(
                                    m.shape,
                                    LayerMaskShape::Rectangle { .. }
                                        | LayerMaskShape::Ellipse { .. }
                                )
                        })
                })
            })
        {
            ExportVisualProfile::SceneV1
        } else if elements().any(|e| e.visual.as_ref().is_some_and(|v| !v.masks.is_empty())) {
            ExportVisualProfile::MaskV1
        } else if elements().any(|e| {
            e.visual
                .as_ref()
                .is_some_and(|v| v.blend_mode != BlendMode::Normal)
        }) {
            ExportVisualProfile::BlendV1
        } else if elements().any(|e| matches!(e.content, ElementContent::Graphic { .. })) {
            ExportVisualProfile::GraphicV1
        } else if !scene.captions.is_empty() {
            ExportVisualProfile::CaptionV3
        } else {
            ExportVisualProfile::TextV2
        },
    )
}
pub fn validate_submission(input: &SubmitExportVideo) -> Result<(), EditorError> {
    if input
        .options
        .runtime_fingerprint
        .as_ref()
        .is_some_and(|v| !valid_hash(v))
    {
        return Err(invalid("Runtime fingerprint must be a SHA-256 digest"));
    }
    video::validate_submission(&video::FixedVideoInput {
        idempotency_key: input.idempotency_key.clone(),
        project_id: input.project_id.clone(),
        revision: input.revision.clone(),
        sequence_id: input.sequence_id.clone(),
    })
}
pub fn prepare(input: &PreparedExportVideo) -> Result<video::PreparedVideoSelection, EditorError> {
    let fixed = &input.execution;
    validate_authority(&fixed.authority)?;
    if [
        &fixed.snapshot.project.id,
        &fixed.snapshot.revision,
        &fixed.sequence_id,
    ]
    .iter()
    .any(|id| !valid_id(id))
        || (input.visual_profile != ExportVisualProfile::SceneV1
            && original_profile(&fixed.snapshot, &fixed.sequence_id)? != input.visual_profile)
    {
        return Err(invalid("Export fixed identity/profile changed"));
    }
    let mut plan = video::prepare_with_frame_window(
        &fixed.snapshot,
        &fixed.sequence_id,
        &fixed.processing_policy,
        frame_profile(input.visual_profile),
        input.options.include_audio,
        input.range.as_ref(),
    )?;
    project_raster_identity(&mut plan, &input.options);
    range_mapping(input, &plan)?;
    if classic_export_rates(&input.options, plan.identity.width, plan.identity.height).is_none() {
        return Err(invalid("Export quality has invalid dimensions"));
    }
    let assets = plan
        .asset_ids
        .iter()
        .map(|id| {
            let a = fixed
                .snapshot
                .project
                .assets
                .iter()
                .find(|a| &a.id == id)
                .ok_or_else(|| invalid("Export asset missing"))?;
            Ok(opencut_contracts::LockedAsset {
                asset_id: id.clone(),
                content_hash: a.version.content_hash.clone(),
                size: a.version.size,
            })
        })
        .collect::<Result<Vec<_>, EditorError>>()?;
    let fonts = plan
        .font_ids
        .iter()
        .map(|id| {
            let f = fixed
                .snapshot
                .project
                .fonts
                .iter()
                .find(|f| &f.id == id)
                .ok_or_else(|| invalid("Export font missing"))?;
            Ok(opencut_contracts::text_frame::FontLock {
                font_id: id.clone(),
                content_hash: f.content.content_hash.clone(),
                size: f.content.size,
            })
        })
        .collect::<Result<Vec<_>, EditorError>>()?;
    if fixed.asset_lock
        != (opencut_contracts::AssetLockManifest {
            version: 1,
            revision: fixed.snapshot.revision.clone(),
            sequence_id: fixed.sequence_id.clone(),
            assets,
        })
        || fixed.font_lock != fonts
    {
        return Err(EditorError::new(
            "ASSET_LOCK_INVALID",
            "Export inventory differs from its fixed participating resources",
        ));
    }
    Ok(plan)
}
pub fn validate_prepared_request(
    request: &JobRequest,
    prepared: &PreparedExportVideo,
) -> Result<(), EditorError> {
    let (public, range) = if request.kind == EXPORT_VIDEO_RANGE_KIND {
        let public: SubmitExportVideoRange = serde_json::from_value(request.input.clone())
            .map_err(|_| invalid("Invalid public range export input"))?;
        (
            SubmitExportVideo {
                idempotency_key: public.idempotency_key,
                project_id: public.project_id,
                revision: public.revision,
                sequence_id: public.sequence_id,
                options: public.options,
            },
            Some(public.range),
        )
    } else {
        (
            serde_json::from_value::<SubmitExportVideo>(request.input.clone())
                .map_err(|_| invalid("Invalid public export input"))?,
            None,
        )
    };
    validate_submission(&public)?;
    if !matches!(
        request.kind.as_str(),
        EXPORT_VIDEO_KIND | EXPORT_VIDEO_RANGE_KIND
    ) || range != prepared.range
        || request.idempotency_key != public.idempotency_key
        || public.options != prepared.options
        || public.project_id != prepared.execution.snapshot.project.id
        || public.revision != prepared.execution.snapshot.revision
        || public.sequence_id != prepared.execution.sequence_id
    {
        return Err(invalid(
            "Export request differs from its durable options and fixed scene",
        ));
    }
    prepare(prepared)?;
    Ok(())
}
pub fn submit(
    store: &mut impl ExportVideoStore,
    assets: &impl video::MediaVideoAssetCapture,
    input: SubmitExportVideo,
) -> Result<JobRecord, EditorError> {
    submit_inner(store, assets, input, None)
}
pub fn submit_range(
    store: &mut impl ExportVideoStore,
    assets: &impl video::MediaVideoAssetCapture,
    input: SubmitExportVideoRange,
) -> Result<JobRecord, EditorError> {
    let base = SubmitExportVideo {
        idempotency_key: input.idempotency_key,
        project_id: input.project_id,
        revision: input.revision,
        sequence_id: input.sequence_id,
        options: input.options,
    };
    submit_inner(store, assets, base, Some(input.range))
}
fn export_request(
    input: &SubmitExportVideo,
    range: Option<&VideoFrameRange>,
) -> Result<JobRequest, EditorError> {
    Ok(JobRequest {
        idempotency_key: input.idempotency_key.clone(),
        kind: if range.is_some() {
            EXPORT_VIDEO_RANGE_KIND
        } else {
            EXPORT_VIDEO_KIND
        }
        .into(),
        input: if let Some(range) = range {
            serde_json::to_value(SubmitExportVideoRange {
                idempotency_key: input.idempotency_key.clone(),
                project_id: input.project_id.clone(),
                revision: input.revision.clone(),
                sequence_id: input.sequence_id.clone(),
                options: input.options.clone(),
                range: range.clone(),
            })
        } else {
            serde_json::to_value(&input)
        }
        .map_err(|_| invalid("Cannot serialize export input"))?,
    })
}
/// Read the original admitted request under the same fixed-snapshot access
/// checks. This never captures media, reserves output storage or accepts a Job.
pub fn cached_range(
    store: &impl ExportVideoStore,
    input: SubmitExportVideoRange,
) -> Result<Option<JobRecord>, EditorError> {
    let base = SubmitExportVideo {
        idempotency_key: input.idempotency_key,
        project_id: input.project_id,
        revision: input.revision,
        sequence_id: input.sequence_id,
        options: input.options,
    };
    validate_submission(&base)?;
    let fixed = store.fixed_scene_video_snapshot(&base.project_id, &base.revision)?;
    if fixed.snapshot.project.id != base.project_id || fixed.snapshot.revision != base.revision {
        return Err(invalid("Authorized export revision changed"));
    }
    store.find_accepted_scene_video_job(&export_request(&base, Some(&input.range))?)
}
fn submit_inner(
    store: &mut impl ExportVideoStore,
    assets: &impl video::MediaVideoAssetCapture,
    input: SubmitExportVideo,
    range: Option<VideoFrameRange>,
) -> Result<JobRecord, EditorError> {
    validate_submission(&input)?;
    let authorized = store.fixed_scene_video_snapshot(&input.project_id, &input.revision)?;
    if authorized.snapshot.project.id != input.project_id
        || authorized.snapshot.revision != input.revision
    {
        return Err(invalid("Authorized export revision changed"));
    }
    let request = export_request(&input, range.as_ref())?;
    if let Some(existing) = store.find_accepted_scene_video_job(&request)? {
        return Ok(existing);
    }
    validate_authority(&authorized.authority)?;
    let profile = select_profile(&authorized.snapshot, &input.sequence_id)?;
    let mut plan = video::prepare_with_frame_window(
        &authorized.snapshot,
        &input.sequence_id,
        &authorized.processing_policy,
        frame_profile(profile),
        input.options.include_audio,
        range.as_ref(),
    )?;
    project_raster_identity(&mut plan, &input.options);
    let (asset_lock, font_lock) = assets.capture(
        &authorized.snapshot,
        &input.sequence_id,
        &plan.asset_ids,
        &plan.font_ids,
    )?;
    let prepared = PreparedExportVideo {
        range,
        options: input.options,
        visual_profile: profile,
        execution: video::PreparedMediaVideoMp4 {
            snapshot: authorized.snapshot,
            sequence_id: input.sequence_id,
            asset_lock,
            font_lock,
            authority: authorized.authority,
            processing_policy: authorized.processing_policy,
        },
    };
    validate_prepared_request(&request, &prepared)?;
    store.accept_export_video(request, prepared)
}
fn project_raster_identity(plan: &mut video::PreparedVideoSelection, options: &ExportVideoOptions) {
    if options.resolution == RasterResolution::Preview540 {
        let size =
            opencut_composition::scene::preview_canvas(opencut_contracts::scene::CanvasSize {
                width: plan.identity.width,
                height: plan.identity.height,
            });
        plan.identity.width = size.width;
        plan.identity.height = size.height;
    }
}
pub fn public_identity(
    input: &PreparedExportVideo,
    plan: &video::PreparedVideoSelection,
) -> ExportVideoIdentity {
    let i = &plan.identity;
    ExportVideoIdentity {
        project_id: i.project_id.clone(),
        revision: i.revision.clone(),
        sequence_id: i.sequence_id.clone(),
        visual_profile: input.visual_profile,
        options: input.options.clone(),
        width: i.width,
        height: i.height,
        fps: i.fps.clone(),
        frame_count: i.frame_count.clone(),
        timeline_duration: i.timeline_duration.clone(),
        video_duration: i.video_duration.clone(),
        audio_frame_count: range_mapping(input, plan)
            .expect("admitted export range")
            .and_then(|mapping| mapping.audio)
            .map(|a| a.frame_count)
            .or_else(|| {
                plan.audio
                    .as_ref()
                    .filter(|_| input.range.is_none())
                    .map(|a| a.duration_samples().to_string())
            }),
    }
}
pub fn render_input_sha256(input: &PreparedExportVideo) -> Result<String, EditorError> {
    let bytes = serde_json::to_vec(&(
        2u32,
        &input.execution.snapshot,
        &input.execution.sequence_id,
        &input.options,
        input.visual_profile,
        48000u32,
        2u32,
    ))
    .map_err(|_| invalid("Cannot hash fixed export options"))?;
    if let Some(range) = &input.range {
        let bound = serde_json::to_vec(&(1u32, &bytes, range))
            .map_err(|_| invalid("Cannot hash export interval"))?;
        return Ok(format!("{:x}", Sha256::digest(bound)));
    }
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub fn validate_stored_result(
    input: &PreparedExportVideo,
    stored: &StoredExportVideo,
) -> Result<ExportVideoResult, EditorError> {
    let bad = || {
        EditorError::new(
            "INVALID_STORED_RESULT",
            "Export result differs from fixed revision/options/encoder evidence",
        )
    };
    let plan = prepare(input).map_err(|_| bad())?;
    let p = &stored.public;
    let e = &p.execution;
    let enc = &e.encoder;
    let v = &e.visual;
    let format = input.options.format;
    let rates = classic_export_rates(&input.options, plan.identity.width, plan.identity.height)
        .ok_or_else(bad)?;
    let frames = crate::classic_audio::decimal(&plan.identity.frame_count)?;
    let rgba = frames
        .checked_mul(u64::from(plan.identity.width))
        .and_then(|n| n.checked_mul(u64::from(plan.identity.height)))
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(bad)?;
    let expected_rgba = match enc.backend {
        Some(VideoEncoderBackend::WebcodecsAvc)
            if format == ExportFormat::Mp4
                && input.options.resolution == RasterResolution::Preview540
                && !input.options.include_audio
                && input.range.is_some()
                && input.visual_profile == ExportVisualProfile::SceneV1 =>
        {
            0
        }
        Some(VideoEncoderBackend::WebcodecsAvc) => return Err(bad()),
        Some(VideoEncoderBackend::FfmpegLibx264) if format != ExportFormat::Mp4 => {
            return Err(bad());
        }
        Some(VideoEncoderBackend::FfmpegLibvpxVp9) if format != ExportFormat::Webm => {
            return Err(bad());
        }
        _ => rgba,
    };
    let duration = opencut_time::RationalTime::try_from(&plan.identity.video_duration)?;
    let seconds = duration.ticks() as f64 / f64::from(duration.timebase());
    // WebM timestamps have millisecond precision and include Opus codec delay.
    let video_tolerance = if format == ExportFormat::Webm {
        0.05
    } else {
        0.002
    };
    if p.schema_version != 2
        || p.identity != public_identity(input, &plan)
        || p.asset_lock != input.execution.asset_lock
        || p.font_lock != input.execution.font_lock
        || p.render_input_sha256 != render_input_sha256(input)?
        || enc.rates != rates
        || enc.video_codec != format.video_codec()
        || enc.audio_codec.as_deref() != input.options.include_audio.then(|| format.audio_codec())
        || enc.pixel_format != "yuv420p"
        || enc.observed_video_frames != plan.identity.frame_count
        || e.frames_rendered != plan.identity.frame_count
        || e.rgba_bytes != expected_rgba.to_string()
        || !enc.observed_video_duration_seconds.is_finite()
        || (enc.observed_video_duration_seconds - seconds).abs() > video_tolerance
        || crate::classic_visual::validate_visual_summary(v).is_err()
        || [
            &enc.ffmpeg_sha256,
            &enc.ffprobe_sha256,
            &e.frame_receipts_sha256,
        ]
        .into_iter()
        .any(|s| !valid_hash(s))
        || p.artifact.mime_type != format.mime_type()
        || stored.artifact.mime_type != format.mime_type()
        || stored.artifact.size == 0
        || stored.artifact.size > input.execution.processing_policy.max_output_bytes
        || p.artifact.size_bytes != stored.artifact.size.to_string()
        || p.artifact.content_hash != stored.artifact.content_hash
        || !valid_hash(&stored.artifact.content_hash)
    {
        return Err(bad());
    }
    match (&plan.audio, &e.audio, enc.observed_audio_duration_seconds) {
        (None, None, None) if stored.audio_decoder_evidence_v1.is_none() => {}
        (Some(audio), Some(summary), Some(observed)) => {
            crate::scene_audio_jobs::validate_execution_summary(
                audio,
                summary,
                stored.audio_decoder_evidence_v1.as_ref(),
            )
            .map_err(|_| bad())?;
            if !crate::classic_visual::validate_visual_audio_identity(v, summary)
                || !observed.is_finite() || (observed - p.identity.audio_frame_count.as_ref().and_then(|n| n.parse::<u64>().ok()).ok_or_else(bad)? as f64 / 48000.0).abs() > 0.05
                || summary.mastering_profile != crate::scene_audio_jobs::AudioMasteringProfile::ClassicExportWholeSceneWebAudioV31 { return Err(bad()); }
        }
        _ => return Err(bad()),
    }
    Ok(p.clone())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    fn input() -> PreparedExportVideo {
        PreparedExportVideo {
            range: None,
            options: ExportVideoOptions::default(),
            visual_profile: ExportVisualProfile::SceneV1,
            execution: crate::media_video_jobs::tests::input(),
        }
    }
    #[test]
    fn audio_off_skips_even_audio_admission_and_keeps_exact_visual_inventory() {
        let mut input = input();
        input.execution.processing_policy.audio.max_pcm_bytes = 1;
        assert!(prepare(&input).is_err());
        input.options.include_audio = false;
        let plan = prepare(&input).unwrap();
        assert!(plan.audio.is_none());
        assert_eq!(plan.asset_ids, vec!["image"]);
        assert_eq!(plan.identity.frame_count, "60");
        assert_eq!(public_identity(&input, &plan).audio_frame_count, None);
        assert_eq!(
            select_profile(&input.execution.snapshot, &input.execution.sequence_id).unwrap(),
            ExportVisualProfile::SceneV1
        );
        let mut original = input.clone();
        original.visual_profile = ExportVisualProfile::TextV2;
        let historical = prepare(&original).unwrap();
        assert_eq!(historical.asset_ids, plan.asset_ids);
        assert_eq!(historical.identity.frame_count, plan.identity.frame_count);
    }
    #[test]
    fn options_are_part_of_durable_request_identity_and_render_digest() {
        let input = input();
        let mut changed = input.clone();
        changed.options.format = ExportFormat::Webm;
        assert_ne!(
            render_input_sha256(&input).unwrap(),
            render_input_sha256(&changed).unwrap()
        );
        let public = SubmitExportVideo {
            idempotency_key: "same-key".into(),
            project_id: input.execution.snapshot.project.id.clone(),
            revision: input.execution.snapshot.revision.clone(),
            sequence_id: input.execution.sequence_id.clone(),
            options: input.options.clone(),
        };
        let request = JobRequest {
            idempotency_key: public.idempotency_key.clone(),
            kind: EXPORT_VIDEO_KIND.into(),
            input: serde_json::to_value(public).unwrap(),
        };
        validate_prepared_request(&request, &input).unwrap();
        assert!(validate_prepared_request(&request, &changed).is_err());
        changed = input.clone();
        changed.options.include_audio = false;
        assert!(validate_prepared_request(&request, &changed).is_err());
        changed = input.clone();
        changed.visual_profile = ExportVisualProfile::GraphicV1;
        assert!(prepare(&changed).is_err());
    }
    pub(crate) fn stored_export_fixture() -> (PreparedExportVideo, StoredExportVideo) {
        use opencut_job_contracts::media_frame::{
            MediaFrameArtifactMetadata, MediaFrameExecutionSummary,
        };
        let mut input = input();
        input.options = ExportVideoOptions {
            runtime_fingerprint: None,
            resolution: opencut_contracts::export_video::RasterResolution::Source,
            format: ExportFormat::Webm,
            quality: ExportQuality::Low,
            include_audio: false,
        };
        let plan = prepare(&input).unwrap();
        let hash = "d".repeat(64);
        let stored = StoredExportVideo {
            audio_decoder_evidence_v1: None,
            public: ExportVideoResult {
                schema_version: 2,
                identity: public_identity(&input, &plan),
                asset_lock: input.execution.asset_lock.clone(),
                font_lock: input.execution.font_lock.clone(),
                render_input_sha256: render_input_sha256(&input).unwrap(),
                execution: ExportVideoExecution {
                    visual: MediaFrameExecutionSummary {
                        host_sha256: hash.clone(),
                        runtime: opencut_job_contracts::media_frame::VisualRuntimeIdentity::HistoricalBrowser(opencut_job_contracts::media_frame::HistoricalBrowserVisualIdentity {
browser_version: "test-browser".into(),
                        browser_sha256: hash.clone(),
                        module_sha256: hash.clone(),
                        shader_sha256: hash.clone(),
                        playwright_version: "1.63.0".into(),
                        mediabunny_version: "1.41.0".into(),
                        gpu_backend: "webgpu".into(),
                        gpu_device: "test-gpu".into(),
                        texture_format: "bgra8unorm".into(),
                        decoder_profile: crate::media_frame_jobs::CLASSIC_VISUAL_DECODER_PROFILE
                            .into(),
                    }),
},
                    audio: None,
                    encoder: ExportVideoEncoderSummary {
                        backend:None,
                        ffmpeg_sha256: hash.clone(),
                        ffprobe_sha256: hash.clone(),
                        video_codec: "vp9".into(),
                        audio_codec: None,
                        pixel_format: "yuv420p".into(),
                        rates: classic_export_rates(&input.options, 64, 48).unwrap(),
                        observed_video_frames: "60".into(),
                        observed_video_duration_seconds: 2.0,
                        observed_audio_duration_seconds: None,
                    },
                    frame_receipts_sha256: hash.clone(),
                    frames_rendered: "60".into(),
                    rgba_bytes: (60u64 * 64 * 48 * 4).to_string(),
                },
                artifact: MediaFrameArtifactMetadata {
                    mime_type: "video/webm".into(),
                    size_bytes: "100".into(),
                    content_hash: hash.clone(),
                },
            },
            artifact: ArtifactDescriptor {
                path: "/private/attempt/video.webm".into(),
                mime_type: "video/webm".into(),
                size: 100,
                content_hash: hash,
            },
        };
        (input, stored)
    }
    #[test]
    fn stored_output_binds_format_quality_audio_absence_and_private_artifact() {
        let (input, stored) = stored_export_fixture();
        let public = validate_stored_result(&input, &stored).unwrap();
        assert!(
            !serde_json::to_string(&public)
                .unwrap()
                .contains("/private/")
        );
        let mut changed = stored.clone();
        changed.public.identity.options.quality = ExportQuality::High;
        assert!(validate_stored_result(&input, &changed).is_err());
        changed = stored.clone();
        changed.public.execution.encoder.rates.video_bitrate += 1000;
        assert!(validate_stored_result(&input, &changed).is_err());
        changed = stored.clone();
        changed
            .public
            .execution
            .encoder
            .observed_audio_duration_seconds = Some(2.0);
        assert!(validate_stored_result(&input, &changed).is_err());
        changed = stored.clone();
        changed.public.execution.encoder.audio_codec = Some("opus".into());
        assert!(validate_stored_result(&input, &changed).is_err());
        changed = stored.clone();
        changed.artifact.mime_type = "video/mp4".into();
        assert!(validate_stored_result(&input, &changed).is_err());
        changed = stored.clone();
        let mut canvas_input = input.clone();
        canvas_input.options.format = ExportFormat::Mp4;
        canvas_input.options.resolution = RasterResolution::Preview540;
        canvas_input.range = Some(VideoFrameRange {
            start_frame: "0".into(),
            frame_count: "60".into(),
        });
        let canvas_plan = prepare(&canvas_input).unwrap();
        let mut canvas = stored.clone();
        canvas.public.identity = public_identity(&canvas_input, &canvas_plan);
        canvas.public.render_input_sha256 = render_input_sha256(&canvas_input).unwrap();
        canvas.public.execution.encoder.backend = Some(VideoEncoderBackend::WebcodecsAvc);
        canvas.public.execution.encoder.video_codec = "h264".into();
        canvas.public.execution.encoder.rates = classic_export_rates(
            &canvas_input.options,
            canvas_plan.identity.width,
            canvas_plan.identity.height,
        )
        .unwrap();
        canvas.public.execution.rgba_bytes = "0".into();
        canvas.public.artifact.mime_type = "video/mp4".into();
        canvas.artifact.mime_type = "video/mp4".into();
        assert!(validate_stored_result(&canvas_input, &canvas).is_ok());
        let mut wrong = canvas.clone();
        wrong.public.execution.encoder.backend = None;
        assert!(validate_stored_result(&canvas_input, &wrong).is_err());
        wrong = canvas.clone();
        wrong.public.execution.rgba_bytes = "737280".into();
        assert!(validate_stored_result(&canvas_input, &wrong).is_err());
        let mut wrong_input = canvas_input.clone();
        wrong_input.range = None;
        assert!(validate_stored_result(&wrong_input, &canvas).is_err());
        wrong_input = canvas_input.clone();
        wrong_input.options.resolution = RasterResolution::Source;
        assert!(validate_stored_result(&wrong_input, &canvas).is_err());
        changed.public.execution.frames_rendered = "59".into();
        assert!(validate_stored_result(&input, &changed).is_err());
        changed = stored.clone();
        changed
            .public
            .execution
            .encoder
            .observed_video_duration_seconds = f64::NAN;
        assert!(validate_stored_result(&input, &changed).is_err());
    }
}

/// Public mapping is derived from the fixed frame grid, never from encoder floats.
pub fn range_mapping(
    input: &PreparedExportVideo,
    plan: &video::PreparedVideoSelection,
) -> Result<Option<ExportRangeMapping>, EditorError> {
    let Some(frames) = &input.range else {
        return Ok(None);
    };
    let first = crate::classic_audio::decimal(&frames.start_frame)?;
    let count = crate::classic_audio::decimal(&frames.frame_count)?;
    let end = first
        .checked_add(count)
        .ok_or_else(|| invalid("Export interval overflow"))?;
    let audio = plan
        .audio
        .as_ref()
        .map(|audio| {
            let boundary = |frame: u64| -> Result<u64, EditorError> {
                let n = u128::from(frame) * u128::from(plan.identity.fps.denominator) * 48000;
                u64::try_from(n / u128::from(plan.identity.fps.numerator))
                    .map_err(|_| invalid("Export sample interval overflow"))
            };
            let start = boundary(first)?;
            let end = boundary(end)?.min(audio.duration_samples());
            if end <= start {
                return Err(invalid(
                    "Export interval has no corresponding scene audio samples",
                ));
            }
            Ok(opencut_job_contracts::scene_audio::AudioSampleRange {
                start_sample: start.to_string(),
                frame_count: (end - start).to_string(),
            })
        })
        .transpose()?;
    Ok(Some(ExportRangeMapping {
        frames: frames.clone(),
        source_start: video::grid_time(&plan.identity.fps, first)?,
        source_end: video::grid_time(&plan.identity.fps, end)?,
        audio,
    }))
}
pub fn range_result(
    input: &PreparedExportVideo,
    output: ExportVideoResult,
) -> Result<ExportVideoRangeResult, EditorError> {
    let mapping =
        range_mapping(input, &prepare(input)?)?.ok_or_else(|| invalid("Missing range identity"))?;
    Ok(ExportVideoRangeResult {
        schema_version: 1,
        mapping,
        output,
    })
}
