use super::*;
use crate::scene::new_document;
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
fn input() -> PreparedTextFramePng {
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
    PreparedTextFramePng {
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
        font_lock: vec![],
        authority: TrustedJobAuthority::LocalOwner,
        processing_policy: policy(),
    }
}

#[test]
fn fonts_cannot_be_smuggled_into_an_unrelated_media_frame() {
    let mut fixed = input();
    validate_prepared(&fixed).unwrap();
    fixed.font_lock.push(FontLock {
        font_id: "font".into(),
        content_hash: "b".repeat(64),
        size: 16,
    });
    assert_eq!(
        validate_prepared(&fixed).unwrap_err().code,
        "FONT_LOCK_INVALID"
    );
    fixed.font_lock.clear();
    fixed.asset_lock.assets[0].asset_id = "font".into();
    assert_eq!(
        validate_prepared(&fixed).unwrap_err().code,
        "ASSET_LOCK_INVALID"
    );
}
#[test]
fn original_public_request_and_text_profile_are_fixed() {
    let fixed = input();
    let mut public = FrameRenderInput {
        idempotency_key: "key".into(),
        project_id: fixed.snapshot.project.id.clone(),
        revision: fixed.snapshot.revision.clone(),
        sequence_id: fixed.sequence_id.clone(),
        time: fixed.time.clone(),
    };
    let request = |p: &FrameRenderInput| JobRequest {
        kind: TEXT_FRAME_PNG_KIND.into(),
        idempotency_key: p.idempotency_key.clone(),
        input: serde_json::to_value(p).unwrap(),
    };
    validate_prepared_request(&request(&public), &fixed).unwrap();
    public.time = t(1, 8);
    assert_eq!(
        validate_prepared_request(&request(&public), &fixed)
            .unwrap_err()
            .code,
        "INVALID_ARGUMENT"
    );
    assert_ne!(
        render_input_sha256(&fixed.snapshot, &fixed.sequence_id, &fixed.time).unwrap(),
        media::render_input_sha256(&fixed.snapshot, &fixed.sequence_id, &fixed.time).unwrap()
    );
}

#[test]
fn persisted_text_result_rejects_font_raster_and_identity_tampering() {
    use opencut_contracts::text_frame::*;
    use opencut_job_contracts::media_frame::{
        MediaFrameArtifactMetadata, MediaFrameExecutionSummary,
    };
    let mut fixed = input();
    let color = Color {
        encoding: ColorEncoding::Srgb,
        red: 1.0,
        green: 1.0,
        blue: 1.0,
        alpha: 1.0,
    };
    let font = FontResource {
        id: "font".into(),
        family: "Fixed".into(),
        weight: 400,
        style: FontStyle::Normal,
        face_index: 0,
        content: AssetVersion {
            id: format!("av_{}", "b".repeat(64)),
            content_hash: "b".repeat(64),
            size: 1024,
        },
        origin: ResourceOrigin::LocalImport {
            original_name: "fixed.ttf".into(),
        },
        license: None,
    };
    fixed.snapshot.project.fonts.push(font);
    fixed.font_lock.push(FontLock {
        font_id: "font".into(),
        content_hash: "b".repeat(64),
        size: 1024,
    });
    // Preserve the real media node and create two independently ordered title nodes.
    let original = fixed.snapshot.project.sequences[0].tracks[0].clone();
    for index in 0..2 {
        let mut track = original.clone();
        track.id = format!("title-track-{index}");
        track.kind = SceneTrackKind::Text;
        track.elements[0].id = format!("title-{index}");
        track.elements[0].content = ElementContent::Text {
            text: Text {
                content: format!("Title {index}"),
                font: FontBinding::Locked {
                    font_id: "font".into(),
                },
                font_weight: FontWeight::Normal,
                font_style: FontStyle::Normal,
                decoration: TextDecoration::None,
                align: SceneTextAlign::Center,
                font_size: FontSize::CanvasPixels { value: 12.0 },
                color: color.clone(),
                letter_spacing_px: 0.0,
                line_height_multiplier: 1.0,
                background: TextBackground {
                    enabled: false,
                    color: color.clone(),
                    corner_radius_percent: 0.0,
                    padding_x_at_size15: 0.0,
                    padding_y_at_size15: 0.0,
                    offset_x_px: 0.0,
                    offset_y_px: 0.0,
                },
            },
        };
        fixed.snapshot.project.sequences[0].tracks.push(track);
    }
    let prepared = validate_prepared(&fixed).unwrap();
    let proofs: Vec<_> = prepared
        .texts()
        .iter()
        .map(|plan| NativeTextCoverage {
            element_id: plan.identity.element_id.clone(),
            font: plan.font.clone(),
            content: plan.text.content.clone(),
        })
        .collect();
    let session = crate::classic_text_frame::ClassicTextFrameSession::new(
        &prepared,
        &fixed.processing_policy,
        &proofs,
    )
    .unwrap();
    let plan = session.typed_plan().unwrap();
    let texts = plan
        .texts
        .iter()
        .map(|text| TextRasterReceipt {
            element_id: text.element_id.clone(),
            width: 64,
            height: 48,
            layout: TextLayout {
                font_id: text.font.font_id.clone(),
                font_content_hash: text.font.content_hash.clone(),
                validated_text_hash: text.font.validated_text_hash.clone(),
                font_weight: text.font.weight,
                font_style: text.font.style,
                face_index: text.font.face_index,
                font_size_px: 12.0,
                line_height_px: 12.0,
                letter_spacing_px: 0.0,
                align: "center".into(),
                baseline: "middle".into(),
                block: TextBlockMetrics {
                    max_width: 20.0,
                    height: 12.0,
                    visual_center_offset: 0.0,
                },
                background: None,
                lines: vec![TextLineMetrics {
                    content: text.text.content.clone(),
                    baseline_y: 0.0,
                    width: 20.0,
                    ascent: 7.0,
                    descent: 2.0,
                    decoration: None,
                }],
                transform: text.text.transform.clone(),
            },
        })
        .collect();
    let stored = StoredTextFramePngResult {
        public: TextFramePngResult {
            schema_version: 2,
            identity: public_identity(&prepared),
            asset_lock: fixed.asset_lock.clone(),
            font_lock: fixed.font_lock.clone(),
            render_input_sha256: render_input_sha256(
                &fixed.snapshot,
                &fixed.sequence_id,
                &fixed.time,
            )
            .unwrap(),
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
            sources: public_sources(
                prepared.media(),
                &[DecodedVisualSource::ReadyImage {
                    element_id: "element".into(),
                    width: 1,
                    height: 1,
                }],
            )
            .unwrap(),
            texts,
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
    };
    validate_stored_result(&fixed, &stored).unwrap();
    for mutation in 0..9 {
        let mut changed = stored.clone();
        match mutation {
            0 => changed.public.font_lock.clear(),
            1 => changed.public.font_lock[0].content_hash = "c".repeat(64),
            2 => {
                changed.public.texts.pop();
            }
            3 => changed.public.texts.swap(0, 1),
            4 => changed.public.texts[0].layout.font_content_hash = "c".repeat(64),
            5 => changed.public.texts[0].layout.block.height += 1.0,
            6 => changed.public.texts[0].layout.lines[0].content = "Other content".into(),
            7 => changed.public.schema_version = 1,
            _ => changed.public.texts[0].width += 1,
        }
        assert_eq!(
            validate_stored_result(&fixed, &changed).unwrap_err().code,
            "INVALID_STORED_RESULT",
            "mutation {mutation}"
        );
    }
    let mut wrong_profile = serde_json::to_value(&stored).unwrap();
    wrong_profile["public"]["identity"]["profile"] =
        serde_json::json!("classic-v31-gpu-normal-frame");
    assert!(serde_json::from_value::<StoredTextFramePngResult>(wrong_profile).is_err());
    for mutation in 0..2 {
        let mut changed = fixed.clone();
        if mutation == 0 {
            changed.font_lock.clear();
        } else {
            changed.font_lock[0].content_hash = "c".repeat(64);
        }
        assert_eq!(
            validate_stored_result(&changed, &stored).unwrap_err().code,
            "INVALID_STORED_RESULT"
        );
    }
}
