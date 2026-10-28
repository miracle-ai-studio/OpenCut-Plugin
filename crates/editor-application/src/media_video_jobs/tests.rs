use super::*;
use crate::scene::new_document;
use opencut_contracts::{
    AssetKind, AssetVersion, CreateProject, FrameRate, LockedAsset, TimeRange,
};

fn t(n: i64, d: u32) -> Time {
    Time {
        ticks: n.to_string(),
        timebase: d,
    }
}
fn policy() -> LocalVideoExecutionPolicy {
    LocalVideoExecutionPolicy {
        visual: opencut_job_execution::media_frame::LocalVisualExecutionPolicy {
            max_frame_pixels: 16_000_000,
            max_decoded_pixels: 64_000_000,
            max_gpu_bytes: 256 * 1024 * 1024,
            max_rgba_bytes: 256 * 1024 * 1024,
            max_encoded_bytes: 32 * 1024 * 1024,
            max_output_bytes: 129 * 1024 * 1024,
            max_rss_bytes: 2 * 1024 * 1024 * 1024,
            deadline_ms: 120000,
        },
        audio: opencut_job_execution::scene_audio::LocalAudioExecutionPolicy {
            profile:
                opencut_job_execution::scene_audio::LocalAudioProcessingProfile::LocalClassicAudioV1,
            max_pcm_bytes: 256 * 1024 * 1024,
            max_encoded_bytes: 32 * 1024 * 1024,
            max_rss_bytes: 2 * 1024 * 1024 * 1024,
            deadline_ms: 120000,
        },
        max_frames: 1800,
        max_output_bytes: 256 * 1024 * 1024,
        deadline_ms: None,
        stalled_after_ms: Some(300000),
    }
}
pub(crate) fn input() -> PreparedMediaVideoMp4 {
    let mut project = new_document(&CreateProject {
        id: "project".into(),
        name: "Picture".into(),
        sequence_id: "scene".into(),
        width: 64,
        height: 48,
        fps: FrameRate {
            numerator: 30,
            denominator: 1,
        },
    })
    .unwrap();
    project.sequences[0].main_track_id = "main".into();
    project.sequences[0].tracks[0].id = "main".into();
    project.assets.push(ProjectAsset {
        id: "image".into(),
        name: "Picture".into(),
        kind: AssetKind::Image,
        version: AssetVersion {
            id: format!("av_{}", "a".repeat(64)),
            content_hash: "a".repeat(64),
            size: 16,
        },
        duration: None,
        origin: ResourceOrigin::LegacyLocator {
            uri: "/private/never-publish.png".into(),
        },
        metadata: ProjectMediaMetadata {
            mime_type: Some("image/png".into()),
            intrinsic_size: Some(CanvasSize {
                width: 1,
                height: 1,
            }),
            frame_rate: None,
            rotation_degrees: None,
            audio: AudioPresence::Absent {},
        },
        license: None,
    });
    project.sequences[0].tracks[0]
        .elements
        .push(TimelineElement {
            id: "element".into(),
            name: "Picture".into(),
            timeline_range: TimeRange {
                start: t(0, 1),
                duration: t(2, 1),
            },
            source_span: ElementSourceSpan::ClassicTrimmed {
                trim_start: t(0, 1),
                trim_end: t(0, 1),
                source_duration: None,
            },
            enabled: true,
            content: ElementContent::Image {
                asset_id: "image".into(),
            },
            channels: vec![],
            visual: Some(VisualStyle {
                profile: VisualProfile::ClassicCanvasV31,
                transform: Transform {
                    position_x_px: 0.0,
                    position_y_px: 0.0,
                    scale_x: 1.0,
                    scale_y: 1.0,
                    rotation_degrees: 0.0,
                },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                effects: vec![],
                masks: vec![],
            }),
        });
    PreparedMediaVideoMp4 {
        snapshot: EditorSnapshot {
            project,
            revision: "fixed-r1".into(),
        },
        sequence_id: "scene".into(),
        asset_lock: AssetLockManifest {
            version: 1,
            revision: "fixed-r1".into(),
            sequence_id: "scene".into(),
            assets: vec![LockedAsset {
                asset_id: "image".into(),
                content_hash: "a".repeat(64),
                size: 16,
            }],
        },
        font_lock: vec![],
        authority: TrustedJobAuthority::LocalOwner,
        processing_policy: policy(),
    }
}

#[test]
fn cfr_uses_exact_global_fps_and_keeps_original_master_duration() {
    let mut fixed = input();
    let SettingsProfile::ClassicGlobal { settings } = &mut fixed.snapshot.project.settings else {
        panic!()
    };
    settings.fps = FrameRate {
        numerator: 30000,
        denominator: 1001,
    };
    fixed.snapshot.project.sequences[0].tracks[0].elements[0]
        .timeline_range
        .duration = t(1001, 1000);
    let plan = validate_prepared(&fixed).unwrap();
    assert_eq!(plan.identity.frame_count, "30");
    assert_eq!(plan.identity.audio_frame_count, "48048");
    assert_eq!(
        RationalTime::try_from(&plan.identity.video_duration)
            .unwrap()
            .cmp_exact(RationalTime::try_from(&t(1001, 1000)).unwrap()),
        std::cmp::Ordering::Equal
    );
    assert_eq!(frame_time(&plan.identity, 29).unwrap(), t(29029, 30000));
    assert!(frame_time(&plan.identity, 30).is_err());
    fixed.snapshot.project.sequences[0].tracks[0].elements[0]
        .timeline_range
        .duration = t(1001001, 1000000);
    let plan = validate_prepared(&fixed).unwrap();
    assert_eq!(plan.identity.frame_count, "31");
    assert_eq!(plan.identity.audio_frame_count, "48049");
    assert_eq!(plan.identity.timeline_duration, t(1001001, 1000000));
    // Workload length is not frame residency. A captured larger policy may
    // retain exactly the same small rendering case and actual memory limits.
    fixed.processing_policy.max_frames = MAX_VIDEO_FRAMES;
    fixed.processing_policy.max_output_bytes = i64::MAX as u64;
    fixed.processing_policy.stalled_after_ms = Some(MAX_VIDEO_DEADLINE_MS);
    assert_eq!(validate_prepared(&fixed).unwrap().identity, plan.identity);
    fixed.processing_policy.max_frames = MAX_VIDEO_FRAMES + 1;
    assert!(validate_policy(&fixed.processing_policy).is_err());
}
#[test]
fn full_scene_media_and_font_locks_are_exact_and_disjoint() {
    let mut fixed = input();
    validate_prepared(&fixed).unwrap();
    fixed.font_lock.push(FontLock {
        font_id: "image".into(),
        content_hash: "a".repeat(64),
        size: 16,
    });
    assert_eq!(
        validate_prepared(&fixed).err().unwrap().code,
        "FONT_LOCK_INVALID"
    );
    fixed.font_lock.clear();
    fixed.asset_lock.assets.clear();
    assert_eq!(
        validate_prepared(&fixed).err().unwrap().code,
        "ASSET_LOCK_INVALID"
    );
}
#[test]
fn separate_audio_and_visual_encoded_budgets_keep_both_limits_and_complete_lock() {
    let mut fixed = input();
    let bytes = 21 * 1024 * 1024;
    fixed.snapshot.project.assets[0].version.size = bytes;
    fixed.asset_lock.assets[0].size = bytes;
    let mut sound = fixed.snapshot.project.assets[0].clone();
    sound.id = "sound".into();
    sound.kind = AssetKind::Audio;
    sound.duration = Some(t(2, 1));
    sound.version = AssetVersion {
        id: format!("av_{}", "b".repeat(64)),
        content_hash: "b".repeat(64),
        size: bytes,
    };
    sound.metadata = ProjectMediaMetadata {
        mime_type: Some("audio/wav".into()),
        intrinsic_size: None,
        frame_rate: None,
        rotation_degrees: None,
        audio: AudioPresence::Present {
            metadata: Some(AudioMetadata {
                sample_rate: 48_000,
                channels: 1,
                channel_layout: None,
            }),
        },
    };
    fixed.asset_lock.assets.push(LockedAsset {
        asset_id: sound.id.clone(),
        content_hash: sound.version.content_hash.clone(),
        size: bytes,
    });
    fixed.snapshot.project.assets.push(sound);
    let mut track = fixed.snapshot.project.sequences[0].tracks[0].clone();
    track.id = "audio".into();
    track.kind = SceneTrackKind::Audio;
    let element = &mut track.elements[0];
    element.id = "voice".into();
    element.visual = None;
    element.content = ElementContent::Audio {
        asset_id: "sound".into(),
        audio: AudioStyle {
            stream: None,
            gain: Gain::Decibels { value: 0.0 },
            muted: false,
        },
        retime: Retime {
            rate: RationalRate {
                numerator: 1,
                denominator: 1,
            },
            maintain_pitch: false,
            mapping: RetimeMapping::ClassicRoundedTicks { timebase: 120000 },
        },
    };
    fixed.snapshot.project.sequences[0].tracks.push(track);
    // Separate sequential realms each admit21MiB;42MiB is not charged to the
    // visual32MiB policy. Both resources remain captured in the exact lock.
    let prepared = validate_prepared(&fixed).unwrap();
    assert_eq!(prepared.asset_ids, ["image", "sound"]);
    let mut omitted_lock = fixed.clone();
    omitted_lock.asset_lock.assets.pop();
    assert_eq!(
        validate_prepared(&omitted_lock).err().unwrap().code,
        "ASSET_LOCK_INVALID"
    );

    let mut oversized_audio = fixed.clone();
    oversized_audio.snapshot.project.assets[1].version.size = 33 * 1024 * 1024;
    oversized_audio.asset_lock.assets[1].size = 33 * 1024 * 1024;
    assert_eq!(
        validate_prepared(&oversized_audio).unwrap().asset_ids,
        ["image", "sound"]
    );
    fixed.snapshot.project.assets[0].version.size = 33 * 1024 * 1024;
    fixed.asset_lock.assets[0].size = 33 * 1024 * 1024;
    // Native images stream through bounded AVIO/probe buffers as videos do;
    // encoded file length is not an allocation of the entire source.
    assert_eq!(
        validate_prepared(&fixed).unwrap().asset_ids,
        ["image", "sound"]
    );

    let mut video = input();
    let asset = &mut video.snapshot.project.assets[0];
    asset.kind = AssetKind::Video;
    asset.version.size = 3 * 1024 * 1024 * 1024;
    asset.duration = Some(t(2, 1));
    asset.metadata.mime_type = Some("video/mp4".into());
    let element = &mut video.snapshot.project.sequences[0].tracks[0].elements[0];
    element.visual.as_mut().unwrap().profile = VisualProfile::LegacyCompositionV3;
    element.source_span = ElementSourceSpan::LegacySourceRange {
        range: TimeRange {
            start: t(0, 1),
            duration: t(2, 1),
        },
    };
    element.content = ElementContent::Video {
        asset_id: "image".into(),
        video_stream: None,
        source_audio_enabled: false,
        audio: AudioStyle {
            stream: None,
            gain: Gain::LinearLegacy { value: 1.0 },
            muted: false,
        },
        retime: Retime {
            rate: RationalRate {
                numerator: 1,
                denominator: 1,
            },
            maintain_pitch: false,
            mapping: RetimeMapping::ExactRational,
        },
    };
    video.snapshot.project.settings = SettingsProfile::LegacyPerScene {
        formats: vec![LegacySequenceFormat {
            sequence_id: "scene".into(),
            canvas_size: CanvasSize {
                width: 64,
                height: 48,
            },
            fps: opencut_contracts::FrameRate {
                numerator: 30,
                denominator: 1,
            },
            background: Background::Color {
                color: Color {
                    encoding: ColorEncoding::Srgb,
                    red: 0.,
                    green: 0.,
                    blue: 0.,
                    alpha: 1.,
                },
            },
        }],
    };
    let admitted = prepare_with_audio_selection(
        &video.snapshot,
        "scene",
        &video.processing_policy,
        VideoFrameProfile::SceneV1,
        false,
    )
    .unwrap();
    assert_eq!(admitted.asset_ids, ["image"]);
    assert_eq!(admitted.identity.frame_count, "60");
    assert_eq!(
        (admitted.identity.width, admitted.identity.height),
        (64, 48)
    );
    video.processing_policy.visual.max_decoded_pixels = 0;
    assert!(
        prepare_with_audio_selection(
            &video.snapshot,
            "scene",
            &video.processing_policy,
            VideoFrameProfile::SceneV1,
            false
        )
        .is_err()
    );
}
#[test]
fn whole_scene_admission_rejects_late_unsupported_blend_and_budget() {
    let mut fixed = input();
    let mut late = fixed.snapshot.project.sequences[0].tracks[0].elements[0].clone();
    late.id = "late".into();
    late.timeline_range.start = t(2, 1);
    late.visual.as_mut().unwrap().blend_mode = BlendMode::Multiply;
    fixed.snapshot.project.sequences[0].tracks[0]
        .elements
        .push(late);
    assert!(validate_prepared(&fixed).is_err());
    let mut fixed = input();
    fixed.processing_policy.max_frames = 59;
    assert_eq!(
        validate_prepared(&fixed).err().unwrap().code,
        "VIDEO_BUDGET_EXCEEDED"
    );
}
#[test]
fn fixed_request_binding_rejects_changed_revision_and_private_scope() {
    let fixed = input();
    let public = FixedVideoInput {
        idempotency_key: "key".into(),
        project_id: "project".into(),
        revision: "fixed-r1".into(),
        sequence_id: "scene".into(),
    };
    let mut request = JobRequest {
        idempotency_key: "key".into(),
        kind: MEDIA_VIDEO_MP4_KIND.into(),
        input: serde_json::to_value(&public).unwrap(),
    };
    validate_prepared_request(&request, &fixed).unwrap();
    request.input["revision"] = serde_json::json!("changed");
    assert_eq!(
        validate_prepared_request(&request, &fixed)
            .unwrap_err()
            .code,
        "INVALID_ARGUMENT"
    );
}

#[test]
fn stored_video_projection_validates_identity_codec_counts_hashes_and_private_artifact() {
    use opencut_job_contracts::media_frame::{
        MediaFrameArtifactMetadata, MediaFrameExecutionSummary,
    };
    use opencut_job_contracts::scene_audio::*;
    let fixed = input();
    let plan = validate_prepared(&fixed).unwrap();
    let digest = "c".repeat(64);
    let mut stored = StoredVideoMp4Result {
        audio_decoder_evidence_v1: None,
        public: MediaVideoMp4Result {
            schema_version: 1,
            identity: plan.identity.clone(),
            asset_lock: fixed.asset_lock.clone(),
            font_lock: fixed.font_lock.clone(),
            render_input_sha256: render_input_sha256(&fixed.snapshot, &fixed.sequence_id).unwrap(),
            execution: MediaVideoExecutionSummary {
                visual: MediaFrameExecutionSummary {
                    host_sha256: digest.clone(),
                    runtime:
                        opencut_job_contracts::media_frame::VisualRuntimeIdentity::HistoricalBrowser(
                            opencut_job_contracts::media_frame::HistoricalBrowserVisualIdentity {
                                browser_version: "test-browser".into(),
                                browser_sha256: digest.clone(),
                                module_sha256: digest.clone(),
                                shader_sha256: digest.clone(),
                                playwright_version: "1.63.0".into(),
                                mediabunny_version: "1.41.0".into(),
                                gpu_backend: "webgpu".into(),
                                gpu_device: "test-gpu".into(),
                                texture_format: "bgra8unorm".into(),
                                decoder_profile:
                                    crate::media_frame_jobs::CLASSIC_VISUAL_DECODER_PROFILE.into(),
                            },
                        ),
                },
                audio: AudioExecutionSummary {
                    host_sha256: digest.clone(),
                    runtime:
                        opencut_job_contracts::scene_audio::AudioRuntimeIdentity::HistoricalBrowser(
                            opencut_job_contracts::scene_audio::HistoricalBrowserAudioIdentity {
                                browser_version: "test-browser".into(),
                                browser_sha256: digest.clone(),
                                module_sha256: digest.clone(),
                                playwright_version: "1.63.0".into(),
                                mediabunny_version: "1.41.0".into(),
                                soundtouch_version: "0.3.0".into(),
                            },
                        ),
                    decoder_profiles: vec![],
                    pitch_profiles: vec![],
                    mastering_profile: AudioMasteringProfile::ClassicExportWholeSceneWebAudioV31,
                    mastering_applied: false,
                },
                encoder: MediaVideoEncoderSummary {
                    ffmpeg_sha256: digest.clone(),
                    ffprobe_sha256: digest.clone(),
                    video_codec: "h264".into(),
                    audio_codec: "aac".into(),
                    pixel_format: "yuv420p".into(),
                    crf: 18,
                    audio_bitrate: 192000,
                    observed_video_frames: "60".into(),
                    observed_video_duration_seconds: 2.0,
                    observed_audio_duration_seconds: 2.0,
                },
                frame_receipts_sha256: digest.clone(),
                frames_rendered: "60".into(),
                rgba_bytes: (60u64 * 64 * 48 * 4).to_string(),
            },
            artifact: MediaFrameArtifactMetadata {
                mime_type: "video/mp4".into(),
                size_bytes: "100".into(),
                content_hash: digest.clone(),
            },
        },
        artifact: ArtifactDescriptor {
            path: "/private/attempt/video.mp4".into(),
            mime_type: "video/mp4".into(),
            size: 100,
            content_hash: digest,
        },
    };
    validate_stored_result(&fixed, &stored).unwrap();
    let original = stored.clone();
    stored.public.identity.frame_count = "59".into();
    assert_eq!(
        validate_stored_result(&fixed, &stored).unwrap_err().code,
        "INVALID_STORED_RESULT"
    );
    stored = original.clone();
    stored.public.execution.encoder.observed_video_frames = "59".into();
    assert!(validate_stored_result(&fixed, &stored).is_err());
    stored = original.clone();
    stored.public.execution.encoder.ffprobe_sha256.clear();
    assert!(validate_stored_result(&fixed, &stored).is_err());
    stored = original.clone();
    stored.artifact.size += 1;
    assert!(validate_stored_result(&fixed, &stored).is_err());
    stored = original.clone();
    stored
        .public
        .execution
        .encoder
        .observed_video_duration_seconds = f64::NAN;
    assert!(validate_stored_result(&fixed, &stored).is_err());
    let public =
        serde_json::to_string(&validate_stored_result(&fixed, &original).unwrap()).unwrap();
    assert!(!public.contains("/private/"));
}

#[test]
fn lock_covers_media_entering_after_the_first_output_frame() {
    let mut fixed = input();
    let mut asset = fixed.snapshot.project.assets[0].clone();
    asset.id = "later-image".into();
    asset.version.id = format!("av_{}", "b".repeat(64));
    asset.version.content_hash = "b".repeat(64);
    fixed.snapshot.project.assets.push(asset);
    let mut element = fixed.snapshot.project.sequences[0].tracks[0].elements[0].clone();
    element.id = "later-element".into();
    element.timeline_range.start = t(2, 1);
    element.content = ElementContent::Image {
        asset_id: "later-image".into(),
    };
    fixed.snapshot.project.sequences[0].tracks[0]
        .elements
        .push(element);
    assert_eq!(
        validate_prepared(&fixed).err().unwrap().code,
        "ASSET_LOCK_INVALID"
    );
    fixed.asset_lock.assets.push(LockedAsset {
        asset_id: "later-image".into(),
        content_hash: "b".repeat(64),
        size: 16,
    });
    let plan = validate_prepared(&fixed).unwrap();
    assert_eq!(plan.asset_ids, vec!["image", "later-image"]);
    assert_eq!(plan.identity.frame_count, "120");
    let first = prepare_frame(
        &fixed.snapshot,
        "scene",
        &frame_time(&plan.identity, 0).unwrap(),
    )
    .unwrap();
    assert_eq!(first.media().resources().len(), 1);
    let later = prepare_frame(
        &fixed.snapshot,
        "scene",
        &frame_time(&plan.identity, 90).unwrap(),
    )
    .unwrap();
    assert_eq!(later.media().resources()[0].asset_id, "later-image");

    // A source starting just after a grid position must not enter an earlier
    // one-frame range,even though both touch the same nominal second.
    let mut fractional = fixed.clone();
    fractional.snapshot.project.sequences[0].tracks[0].elements[1]
        .timeline_range
        .start = t(240001, 120000);
    let selected = |start: &str| {
        prepare_with_frame_window(
            &fractional.snapshot,
            "scene",
            &fractional.processing_policy,
            VideoFrameProfile::TextV2,
            false,
            Some(&opencut_job_contracts::export_video::VideoFrameRange {
                start_frame: start.into(),
                frame_count: "1".into(),
            }),
        )
        .unwrap()
    };
    assert!(selected("60").asset_ids.is_empty());
    assert_eq!(selected("61").asset_ids, ["later-image"]);

    // Many long sequential sources share a small working set. Their complete
    // lock must not be mistaken for32 simultaneously decoded resources or a
    // combined encoded allocation. Audio-off isolates the visual requirement.
    let mut long = input();
    let asset = long.snapshot.project.assets[0].clone();
    let element = long.snapshot.project.sequences[0].tracks[0].elements[0].clone();
    long.snapshot.project.assets.clear();
    long.snapshot.project.sequences[0].tracks[0]
        .elements
        .clear();
    long.processing_policy.max_frames = MAX_VIDEO_FRAMES;
    long.processing_policy.max_output_bytes = i64::MAX as u64;
    for index in 0..40 {
        let hash = format!("{:064x}", index + 1);
        let mut asset = asset.clone();
        asset.id = format!("sequential-image-{index}");
        asset.version = AssetVersion {
            id: format!("av_{hash}"),
            content_hash: hash,
            size: 20 * 1024 * 1024,
        };
        let mut element = element.clone();
        element.id = format!("sequential-element-{index}");
        element.content = ElementContent::Image {
            asset_id: asset.id.clone(),
        };
        element.timeline_range = TimeRange {
            start: t(index * 3600, 1),
            duration: t(3600, 1),
        };
        long.snapshot.project.assets.push(asset);
        long.snapshot.project.sequences[0].tracks[0]
            .elements
            .push(element);
    }
    let prepare = |fixed: &PreparedMediaVideoMp4| {
        prepare_with_audio_selection(
            &fixed.snapshot,
            "scene",
            &fixed.processing_policy,
            VideoFrameProfile::TextV2,
            false,
        )
    };
    let plan = prepare(&long).unwrap();
    assert_eq!(plan.asset_ids.len(), 40);
    assert_eq!(plan.identity.frame_count, "4320000");
    // The encoded-memory limit is still aggregate per simultaneous frame:
    // eight distinct sources exceed 32 MiB of bounded probe/AVIO reserves.
    for index in 1..8 {
        let mut overlap = long.snapshot.project.sequences[0].tracks[0].clone();
        overlap.id = format!("overlap-{index}");
        overlap.elements = vec![overlap.elements[index].clone()];
        overlap.elements[0].id = format!("overlap-element-{index}");
        overlap.elements[0].timeline_range.start = t(0, 1);
        long.snapshot.project.sequences[0].tracks.push(overlap);
    }
    assert_eq!(prepare(&long).err().unwrap().code, "VISUAL_BUDGET_EXCEEDED");
}

#[test]
fn scene_video_admits_supported_crossfade_through_the_same_frame_execution_plan() {
    let mut fixed = input();
    let asset = &mut fixed.snapshot.project.assets[0];
    asset.kind = AssetKind::Video;
    asset.duration = Some(t(6, 1));
    asset.metadata.mime_type = Some("video/mp4".into());
    let track = &mut fixed.snapshot.project.sequences[0].tracks[0];
    let first = &mut track.elements[0];
    first.source_span = ElementSourceSpan::ClassicTrimmed {
        trim_start: t(1, 1),
        trim_end: t(3, 1),
        source_duration: Some(t(6, 1)),
    };
    first.content = ElementContent::Video {
        asset_id: "image".into(),
        video_stream: None,
        source_audio_enabled: false,
        audio: AudioStyle {
            stream: None,
            gain: Gain::Decibels { value: 0.0 },
            muted: false,
        },
        retime: Retime {
            rate: RationalRate {
                numerator: 1,
                denominator: 1,
            },
            maintain_pitch: false,
            mapping: RetimeMapping::ClassicRoundedTicks { timebase: 120000 },
        },
    };
    let mut incoming = first.clone();
    incoming.id = "incoming".into();
    incoming.timeline_range.start = t(2, 1);
    track.elements.push(incoming);
    fixed.snapshot.project.sequences[0]
        .transitions
        .push(SceneTransition {
            id: "cut".into(),
            outgoing_element_id: "element".into(),
            incoming_element_id: "incoming".into(),
            duration: t(1, 1),
            transition: TransitionKind::LegacyCrossfadeV3,
        });
    let admitted = prepare_with_audio_selection(
        &fixed.snapshot,
        "scene",
        &fixed.processing_policy,
        VideoFrameProfile::SceneV1,
        false,
    )
    .unwrap();
    assert_eq!(admitted.identity.frame_count, "120");
    assert_eq!(admitted.asset_ids, ["image"]);
    // Other backends retain their own admission rules; accepting a Scene
    // crossfade must not silently erase it in a narrower rendering profile.
    assert!(
        prepare_with_audio_selection(
            &fixed.snapshot,
            "scene",
            &fixed.processing_policy,
            VideoFrameProfile::TextV2,
            false
        )
        .is_err()
    );
    fixed.snapshot.project.sequences[0].transitions[0].duration = t(5, 1);
    assert!(
        prepare_with_audio_selection(
            &fixed.snapshot,
            "scene",
            &fixed.processing_policy,
            VideoFrameProfile::SceneV1,
            false
        )
        .is_err()
    );
}
