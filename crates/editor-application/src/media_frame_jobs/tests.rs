use super::*;
use crate::{classic_visual::*, scene::new_document};
use opencut_contracts::{
    AssetKind, AssetVersion, CreateProject, FrameRate, LockedAsset, TimeRange, scene::*,
};

fn t(n: i64, d: u32) -> Time {
    Time {
        ticks: n.to_string(),
        timebase: d,
    }
}
fn policy() -> LocalVisualExecutionPolicy {
    LocalVisualExecutionPolicy {
        max_frame_pixels: 16_000_000,
        max_decoded_pixels: 64_000_000,
        max_gpu_bytes: 256 * 1024 * 1024,
        max_rgba_bytes: 256 * 1024 * 1024,
        max_encoded_bytes: 32 * 1024 * 1024,
        max_output_bytes: 129 * 1024 * 1024,
        max_rss_bytes: 2 * 1024 * 1024 * 1024,
        deadline_ms: 120000,
    }
}
fn input() -> PreparedMediaFramePng {
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
    PreparedMediaFramePng {
        snapshot: EditorSnapshot {
            project,
            revision: "fixed-r1".into(),
        },
        sequence_id: "scene".into(),
        time: t(1, 7),
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
        authority: TrustedJobAuthority::LocalOwner,
        processing_policy: policy(),
    }
}
fn observation() -> DecodedVisualSource {
    DecodedVisualSource::ReadyImage {
        element_id: "element".into(),
        width: 320,
        height: 90,
    }
}
fn stored(input: &PreparedMediaFramePng) -> StoredFramePngResult {
    let prepared = validate_prepared(input).unwrap();
    StoredFramePngResult {
        public: MediaFramePngResult {
            schema_version: 1,
            identity: public_identity(&prepared),
            asset_lock: input.asset_lock.clone(),
            render_input_sha256: render_input_sha256(
                &input.snapshot,
                &input.sequence_id,
                &input.time,
            )
            .unwrap(),
            sources: public_sources(&prepared, &[observation()]).unwrap(),
            execution: MediaFrameExecutionSummary {
                host_sha256: "b".repeat(64),
                runtime:
                    opencut_job_contracts::media_frame::VisualRuntimeIdentity::HistoricalBrowser(
                        opencut_job_contracts::media_frame::HistoricalBrowserVisualIdentity {
                            browser_version: "test-browser".into(),
                            browser_sha256: "c".repeat(64),
                            module_sha256: "d".repeat(64),
                            shader_sha256: "e".repeat(64),
                            playwright_version: "1.63.0".into(),
                            mediabunny_version: "1.41.0".into(),
                            gpu_backend: "webgpu".into(),
                            gpu_device: "fixture-device".into(),
                            texture_format: "bgra8unorm".into(),
                            decoder_profile: CLASSIC_VISUAL_DECODER_PROFILE.into(),
                        },
                    ),
            },
            artifact: MediaFrameArtifactMetadata {
                mime_type: "image/png".into(),
                size_bytes: "100".into(),
                content_hash: "f".repeat(64),
            },
        },
        artifact: ArtifactDescriptor {
            path: "/private/result.png".into(),
            mime_type: "image/png".into(),
            size: 100,
            content_hash: "f".repeat(64),
        },
    }
}
#[test]
fn exact_presentation_time_full_snapshot_identity_and_safe_result_projection() {
    let input = input();
    let prepared = validate_prepared(&input).unwrap();
    assert_eq!(prepared.identity().time, t(1, 7));
    let digest = render_input_sha256(&input.snapshot, &input.sequence_id, &input.time).unwrap();
    let mut renamed = input.snapshot.clone();
    renamed.project.name = "Other authored snapshot".into();
    assert_ne!(
        digest,
        render_input_sha256(&renamed, &input.sequence_id, &input.time).unwrap()
    );
    let result = stored(&input);
    let public = validate_stored_result(&input, &result).unwrap();
    assert_eq!(public.sources[0].width, Some(320));
    let json = serde_json::to_string(&public).unwrap();
    assert!(!json.contains("/private"));
    assert!(!json.contains("uri"));
    assert!(!json.contains("path"));
    for mutation in 0..4 {
        let mut changed = result.clone();
        match mutation {
            0 => changed.public.identity.time = t(1, 8),
            1 => changed.public.sources[0].asset_id = "other".into(),
            2 => changed.public.asset_lock.assets[0].size += 1,
            _ => match &mut changed.public.execution.runtime {
                VisualRuntimeIdentity::HistoricalBrowser(identity) => {
                    identity.shader_sha256 = "unknown".into()
                }
                VisualRuntimeIdentity::Native(identity) => {
                    identity.shader_sha256 = "unknown".into()
                }
            },
        }
        assert_eq!(
            validate_stored_result(&input, &changed).unwrap_err().code,
            "INVALID_STORED_RESULT"
        );
    }
}
#[test]
fn original_request_binding_and_lock_are_not_replaced_by_a_prepared_payload() {
    let input = input();
    let request = JobRequest {
        kind: MEDIA_FRAME_PNG_KIND.into(),
        idempotency_key: "key".into(),
        input: serde_json::to_value(FrameRenderInput {
            idempotency_key: "key".into(),
            project_id: "project".into(),
            revision: "fixed-r1".into(),
            sequence_id: "scene".into(),
            time: t(1, 7),
        })
        .unwrap(),
    };
    validate_prepared_request(&request, &input).unwrap();
    let mut other = input.clone();
    other.time = t(1, 8);
    assert!(validate_prepared_request(&request, &other).is_err());
    other = input.clone();
    other.asset_lock.assets[0].content_hash = "b".repeat(64);
    assert_eq!(
        validate_prepared(&other).unwrap_err().code,
        "ASSET_LOCK_INVALID"
    );
}
#[derive(Default)]
struct Resources {
    checkpoints: usize,
}
impl ClassicVisualResources for Resources {
    fn resolve(&mut self, _: &VisualResource, _: &str) -> Result<(String, u64), EditorError> {
        Ok(("transfer".into(), 16))
    }
    fn read(&mut self, _: &str, _: u64, _: u32) -> Result<(Vec<u8>, bool), EditorError> {
        Ok((vec![0; 16], true))
    }
    fn release(&mut self, _: &str) -> Result<(), EditorError> {
        Ok(())
    }
    fn checkpoint(&mut self) -> Result<(), EditorError> {
        self.checkpoints += 1;
        Ok(())
    }
}
#[derive(Default)]
struct Sink {
    bytes: usize,
}
impl ClassicVisualRgbaSink for Sink {
    fn chunk(&mut self, offset: u64, bytes: &[u8]) -> Result<(), EditorError> {
        assert_eq!(offset, self.bytes as u64);
        self.bytes += bytes.len();
        Ok(())
    }
}
#[test]
fn session_phases_bind_once_and_check_complete_rgba_and_frozen_geometry_receipt() {
    let input = input();
    let prepared = validate_prepared(&input).unwrap();
    let mut session = ClassicVisualSession::new(&prepared, &input.processing_policy).unwrap();
    let mut resources = Resources::default();
    let mut sink = Sink::default();
    assert!(
        !serde_json::to_string(&session.plan().unwrap())
            .unwrap()
            .contains("/private")
    );
    assert!(session.rgba_chunk(0, &[0; 4], &mut sink).is_err());
    assert!(
        session
            .callback(
                VisualCallback::Bind(VisualBindFrame {
                    decoded: vec![observation()]
                }),
                &mut resources
            )
            .is_err()
    );
    let admission = session
        .callback(
            VisualCallback::Admit(VisualAdmitDecode {
                element_id: "element".into(),
            }),
            &mut resources,
        )
        .unwrap();
    assert!(admission["maxPixels"].is_string());
    assert!(
        session
            .callback(
                VisualCallback::Admit(VisualAdmitDecode {
                    element_id: "element".into()
                }),
                &mut resources
            )
            .is_err()
    );
    session
        .callback(
            VisualCallback::Bind(VisualBindFrame {
                decoded: vec![observation()],
            }),
            &mut resources,
        )
        .unwrap();
    let bound = session.bound().unwrap().clone();
    assert_eq!(bound.layers[0].quad.as_ref().unwrap().height, 18.0);
    assert!(
        session
            .callback(
                VisualCallback::Bind(VisualBindFrame {
                    decoded: vec![observation()]
                }),
                &mut resources
            )
            .is_err()
    );
    let mut receipt = ClassicVisualRenderReceipt {
        identity: prepared.identity().clone(),
        engine_identity: CLASSIC_VISUAL_PROFILE.into(),
        bound,
        rgba_bytes: "12288".into(),
        peak_accounted_bytes: "12288".into(),
    };
    assert!(session.validate_receipt(&receipt).is_err());
    assert!(session.rgba_chunk(4, &[0; 4], &mut sink).is_err());
    session.rgba_chunk(0, &[0; 8192], &mut sink).unwrap();
    session.rgba_chunk(8192, &[0; 4096], &mut sink).unwrap();
    session.validate_receipt(&receipt).unwrap();
    receipt.bound.layers[0].quad.as_mut().unwrap().width += 1.0;
    assert!(session.validate_receipt(&receipt).is_err());
    assert!(resources.checkpoints >= 5);
}
#[test]
fn resource_and_actual_dimensions_cannot_bypass_admission() {
    let input = input();
    let prepared = validate_prepared(&input).unwrap();
    let mut resources = Resources::default();
    let mut session = ClassicVisualSession::new(&prepared, &input.processing_policy).unwrap();
    let mut resource = session.resource("image").unwrap();
    resource.size = "17".into();
    assert!(
        session
            .callback(
                VisualCallback::Resolve(VisualResolveBytes {
                    resource,
                    handle: "fixed-handle".into()
                }),
                &mut resources
            )
            .is_err()
    );
    let giant = DecodedVisualSource::ReadyImage {
        element_id: "element".into(),
        width: 16385,
        height: 1,
    };
    assert_eq!(
        session.project_decoded_sources(&[giant]).unwrap_err().code,
        "VISUAL_BUDGET_EXCEEDED"
    );
    let mut denied = input.processing_policy.clone();
    denied.max_gpu_bytes = 1;
    assert!(ClassicVisualSession::new(&prepared, &denied).is_err());
    denied = input.processing_policy.clone();
    denied.deadline_ms = 0;
    assert!(ClassicVisualSession::new(&prepared, &denied).is_err());
}

#[test]
fn runtime_unique_resource_limit_is_checked_before_queue_execution() {
    let input = input();
    let prepared = validate_prepared(&input).unwrap();
    let mut graph = prepared.graph().clone();
    let original_layer = graph.layers[0].clone();
    let original_resource = graph.resources[0].clone();
    for index in 1..33 {
        let id = format!("image-{index}");
        let mut layer = original_layer.clone();
        layer.element_id = format!("element-{index}");
        layer.content = SampledContent::Image {
            asset_id: id.clone(),
        };
        graph.layers.push(layer);
        let mut resource = original_resource.clone();
        resource.asset_id = id;
        graph.resources.push(resource);
    }
    let too_many = prepare_visual_frame_scene(&graph).unwrap();
    assert_eq!(
        ClassicVisualSession::new(&too_many, &input.processing_policy)
            .err()
            .unwrap()
            .code,
        "VISUAL_BUDGET_EXCEEDED"
    );
    graph.layers.pop();
    graph.resources.pop();
    let admitted = prepare_visual_frame_scene(&graph).unwrap();
    assert!(ClassicVisualSession::new(&admitted, &input.processing_policy).is_ok());
    // The limit counts unique resources, so reusing a source does not consume a
    // second transfer slot; the separate layer bound still applies.
    let mut repeated = original_layer;
    repeated.element_id = "reused-element".into();
    graph.layers.push(repeated);
    let reused = prepare_visual_frame_scene(&graph).unwrap();
    assert!(ClassicVisualSession::new(&reused, &input.processing_policy).is_ok());
}

#[test]
fn asymmetric_decode_admission_uses_source_hints_without_enlarging_frame_budget() {
    let input = input();
    let prepared = validate_prepared(&input).unwrap();
    let mut graph = prepared.graph().clone();
    let mut large = graph.layers[0].clone();
    large.element_id = "large".into();
    large.content = SampledContent::Image {
        asset_id: "large-source".into(),
    };
    graph.layers.push(large);
    let mut resource = graph.resources[0].clone();
    resource.asset_id = "large-source".into();
    resource.metadata.intrinsic_size = Some(CanvasSize {
        width: 1280,
        height: 720,
    });
    graph.resources.push(resource);
    let mut policy = input.processing_policy;
    policy.max_decoded_pixels = 1280 * 720 + 1;
    let observations = vec![
        DecodedVisualSource::ReadyImage {
            element_id: "element".into(),
            width: 1,
            height: 1,
        },
        DecodedVisualSource::ReadyImage {
            element_id: "large".into(),
            width: 1280,
            height: 720,
        },
    ];
    let prepared = prepare_visual_frame_scene(&graph).unwrap();
    let mut session = ClassicVisualSession::new(&prepared, &policy).unwrap();
    assert_eq!(session.decode_admission().unwrap().max_pixels, "460800");
    let small = session.decode_admission_for("element").unwrap();
    let large = session.decode_admission_for("large").unwrap();
    assert_eq!(small.max_pixels, "1");
    assert_eq!(large.max_pixels, "921600");
    assert_eq!(
        small.max_pixels.parse::<u64>().unwrap() + large.max_pixels.parse::<u64>().unwrap(),
        policy.max_decoded_pixels
    );
    let callback: VisualDecodeAdmission = serde_json::from_value(
        session
            .callback(
                VisualCallback::Admit(VisualAdmitDecode {
                    element_id: "large".into(),
                }),
                &mut Resources::default(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(callback.max_pixels, large.max_pixels);
    assert!(session.project_decoded_sources(&observations).is_ok());
    let mut excessive = observations.clone();
    excessive[1] = DecodedVisualSource::ReadyImage {
        element_id: "large".into(),
        width: 1281,
        height: 720,
    };
    assert_eq!(
        session
            .project_decoded_sources(&excessive)
            .unwrap_err()
            .code,
        "VISUAL_BUDGET_EXCEEDED"
    );
    // An unknown source shares only space left after the known source hint.
    graph.resources[0].metadata.intrinsic_size = None;
    policy.max_decoded_pixels += 3;
    let prepared = prepare_visual_frame_scene(&graph).unwrap();
    let session = ClassicVisualSession::new(&prepared, &policy).unwrap();
    assert_eq!(
        session.decode_admission_for("element").unwrap().max_pixels,
        "2"
    );
    assert_eq!(
        session.decode_admission_for("large").unwrap().max_pixels,
        "921602"
    );
    assert!(session.project_decoded_sources(&observations).is_ok());
    // Oversized metadata cannot reserve additional policy capacity.
    graph.resources[1].metadata.intrinsic_size = Some(CanvasSize {
        width: 16384,
        height: 16384,
    });
    let prepared = prepare_visual_frame_scene(&graph).unwrap();
    let session = ClassicVisualSession::new(&prepared, &policy).unwrap();
    assert_eq!(
        session.decode_admission_for("large").unwrap().max_pixels,
        session.decode_admission().unwrap().max_pixels
    );
    assert!(session.decode_admission_for("unknown-element").is_err());
}

#[test]
fn per_layer_zero_decode_budget_is_rejected_by_shared_admission_and_callback() {
    let input = input();
    let prepared = validate_prepared(&input).unwrap();
    let mut graph = prepared.graph().clone();
    let mut second = graph.layers[0].clone();
    second.element_id = "second-element".into();
    graph.layers.push(second);
    let prepared = prepare_visual_frame_scene(&graph).unwrap();
    let mut policy = input.processing_policy;
    policy.max_decoded_pixels = 1;
    let mut session = ClassicVisualSession::new(&prepared, &policy).unwrap();
    assert_eq!(
        session.decode_admission().unwrap_err().code,
        "VISUAL_BUDGET_EXCEEDED"
    );
    assert_eq!(
        session
            .callback(
                VisualCallback::Admit(VisualAdmitDecode {
                    element_id: "element".into(),
                }),
                &mut Resources::default()
            )
            .unwrap_err()
            .code,
        "VISUAL_BUDGET_EXCEEDED"
    );
}

#[test]
fn retained_picture_keeps_fixed_time_policy_and_revalidates_decoder_facts() {
    use crate::visual_preview::PreparedPicture;
    use opencut_api_contracts::{
        audio_preview::AudioPreviewStream, visual_preview::VisualPreviewFrame,
    };
    let original = input();
    let mut request = VisualPreviewFrame {
        resolution: opencut_contracts::export_video::RasterResolution::Source,
        prepare_ahead: false,
        retained_picture_bytes: 0,
        session: AudioPreviewStream {
            session_id: "s".into(),
            generation: "1".into(),
            overlay_epoch: "0".into(),
            owner_id: "owner".into(),
            lease_id: "lease".into(),
        },
        time: Some(original.time.clone()),
        decoded: None,
        rasters: None,
        captions: None,
        graphics: None,
    };
    let prepare = || {
        PreparedPicture::new(
            crate::scene_frame_jobs::prepare(
                &original.snapshot,
                &original.sequence_id,
                &original.time,
            )
            .unwrap(),
            original.processing_policy.clone(),
            vec![],
        )
        .unwrap()
    };
    let prepared = prepare();
    let plan = prepared.result(&request, &original.time).unwrap();
    let reservation = 16 * 1024 * 1024;
    let reduced = PreparedPicture::new(
        crate::scene_frame_jobs::prepare(&original.snapshot, &original.sequence_id, &original.time)
            .unwrap(),
        crate::visual_preview::reserve_picture_memory(
            original.processing_policy.clone(),
            reservation,
        )
        .unwrap(),
        vec![],
    )
    .unwrap()
    .result(&request, &original.time)
    .unwrap();
    assert_eq!(
        reduced.admission.max_rgba_bytes.parse::<u64>().unwrap() + u64::from(reservation),
        original.processing_policy.max_rgba_bytes
    );
    assert!(
        reduced.decode_admissions["element"]
            .max_decoded_bytes
            .parse::<u64>()
            .unwrap()
            < plan.decode_admissions["element"]
                .max_decoded_bytes
                .parse::<u64>()
                .unwrap()
    );
    assert!(
        crate::visual_preview::reserve_picture_memory(
            original.processing_policy.clone(),
            64 * 1024 * 1024 + 1
        )
        .is_err()
    );
    assert_eq!(plan.plan.identity.revision, "fixed-r1");
    assert_eq!(plan.plan.identity.time, t(1, 7));
    assert_eq!(
        plan.plan
            .execution
            .execution
            .execution
            .execution
            .media
            .layers[0]
            .element_id,
        "element"
    );
    request.decoded = Some(vec![observation()]);
    let bound = prepared.result(&request, &t(2, 7)).unwrap();
    assert!(bound.bound_media.is_some());
    assert_eq!(
        bound.admission.max_rgba_bytes,
        original.processing_policy.max_rgba_bytes.to_string()
    );
    let fresh = prepare().result(&request, &t(2, 7)).unwrap();
    assert_eq!(
        serde_json::to_value(bound).unwrap(),
        serde_json::to_value(fresh).unwrap()
    );
    // A newer consumer is rechecked, never inherited from preparation.
    assert_eq!(
        prepared.result(&request, &t(3, 7)).unwrap_err().code,
        "PREVIEW_FRAME_STALE"
    );
    request.time = Some(t(1, 8));
    assert_eq!(
        prepared.result(&request, &t(2, 7)).unwrap_err().code,
        "INVALID_VISUAL_PLAN"
    );
    request.time = Some(original.time.clone());
    request.decoded = Some(vec![]);
    assert!(prepared.result(&request, &original.time).is_err());
    let mut policy = original.processing_policy.clone();
    policy.max_rgba_bytes = 1;
    assert!(
        PreparedPicture::new(
            crate::scene_frame_jobs::prepare(
                &original.snapshot,
                &original.sequence_id,
                &original.time
            )
            .unwrap(),
            policy,
            vec![],
        )
        .is_err()
    );

    // The same complete picture path consumes retained template media. Source
    // clock and display dimensions remain fixed facts, not UI approximations.
    let mut retained = original.clone();
    let asset = &mut retained.snapshot.project.assets[0];
    asset.kind = AssetKind::Video;
    asset.duration = Some(t(4, 1));
    asset.metadata.mime_type = Some("video/mp4".into());
    asset.metadata.intrinsic_size = Some(CanvasSize {
        width: 320,
        height: 180,
    });
    let element = &mut retained.snapshot.project.sequences[0].tracks[0].elements[0];
    element.visual.as_mut().unwrap().profile = VisualProfile::LegacyCompositionV3;
    element.source_span = ElementSourceSpan::LegacySourceRange {
        range: TimeRange {
            start: t(1, 1),
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
    let picture = PreparedPicture::new(
        crate::scene_frame_jobs::prepare(&retained.snapshot, &retained.sequence_id, &retained.time)
            .unwrap(),
        retained.processing_policy.clone(),
        vec![],
    )
    .unwrap();
    request.decoded = None;
    let result = picture.result(&request, &retained.time).unwrap();
    let media = &result.plan.execution.execution.execution.execution.media;
    let VisualDecodeRequest::Video {
        requested_time,
        point_profile,
        requested_time_seconds,
        ..
    } = &media.layers[0].decode
    else {
        unreachable!()
    };
    assert_eq!(requested_time, &t(8, 7));
    assert_eq!(point_profile, "exact_rational");
    assert_eq!(*requested_time_seconds, 8.0 / 7.0);
    request.decoded = Some(vec![DecodedVisualSource::ReadyVideo {
        element_id: "element".into(),
        width: 320,
        height: 180,
        requested_time: t(8, 7),
        selected_video_stream: None,
        actual_timestamp_seconds: 1.1,
        frame_duration_seconds: 0.1,
        eof_held: false,
    }]);
    let result = picture.result(&request, &retained.time).unwrap();
    let quad = result.bound_media.as_ref().unwrap().layers[0]
        .quad
        .as_ref()
        .unwrap();
    assert_eq!(
        (quad.center_x, quad.center_y, quad.width, quad.height),
        (32.0, 24.0, 64.0, 36.0)
    );
}

#[test]
fn native_decoded_pixel_limits_admit_hd_and_reject_oversized_or_small_cpu_budgets() {
    let input = input();
    let prepared = validate_prepared(&input).unwrap();
    let mut graph = prepared.graph().clone();
    graph.format.canvas_size = CanvasSize {
        width: 1920,
        height: 1080,
    };
    graph.resources[0].metadata.intrinsic_size = Some(graph.format.canvas_size.clone());
    let mut layer = graph.layers[0].clone();
    layer.element_id = "second".into();
    layer.content = SampledContent::Image {
        asset_id: "second-source".into(),
    };
    graph.layers.push(layer);
    let mut source = graph.resources[0].clone();
    source.asset_id = "second-source".into();
    graph.resources.push(source);
    let prepared = prepare_visual_frame_scene(&graph).unwrap();
    let mut policy = input.processing_policy;
    policy.max_rgba_bytes = 128 * 1024 * 1024;
    policy.max_decoded_pixels = 16_777_216;
    let observations = vec![
        DecodedVisualSource::ReadyImage {
            element_id: "element".into(),
            width: 1920,
            height: 1080,
        },
        DecodedVisualSource::ReadyImage {
            element_id: "second".into(),
            width: 1920,
            height: 1080,
        },
    ];
    let current = ClassicVisualSession::new(&prepared, &policy).unwrap();
    assert!(current.project_decoded_sources(&observations).is_ok());
    let mut oversized = observations.clone();
    oversized[1] = DecodedVisualSource::ReadyImage {
        element_id: "second".into(),
        width: 4096,
        height: 2160,
    };
    assert_eq!(
        current
            .project_decoded_sources(&oversized)
            .unwrap_err()
            .code,
        "VISUAL_BUDGET_EXCEEDED"
    );
    policy.max_rgba_bytes = 16 * 1024 * 1024;
    let previous = ClassicVisualSession::new(&prepared, &policy).unwrap();
    assert_eq!(
        previous
            .project_decoded_sources(&observations)
            .unwrap_err()
            .code,
        "VISUAL_BUDGET_EXCEEDED"
    );
}
