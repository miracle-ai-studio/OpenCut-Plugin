use super::*;
use opencut_contracts::{font_derived::ImportFont, scene::*};

#[test]
fn two_hd_sources_and_locked_font_fit_without_unused_scene_canvases() {
    let mut source = crate::classic_text_frame::tests::graph();
    source.format.canvas_size = CanvasSize {
        width: 1920,
        height: 1080,
    };
    source.fonts[0].content.size = 5_857_936;
    source.resources[0].version.size = 3_000_000;
    source.resources[0].metadata.intrinsic_size = Some(CanvasSize {
        width: 1920,
        height: 1080,
    });
    let mut front = source.resources[0].clone();
    front.asset_id = "second-image".into();
    front.version.content_hash = "c".repeat(64);
    front.version.id = format!("av_{}", front.version.content_hash);
    source.resources.push(front);
    source.layers[2].content = SampledContent::Image {
        asset_id: "second-image".into(),
    };
    let prepared = prepare_scene_frame(&source).unwrap();
    let mut policy = crate::classic_text_frame::tests::policy();
    policy.max_frame_pixels = 16_777_216;
    policy.max_decoded_pixels = 16_777_216;
    policy.max_encoded_bytes = 32 * 1024 * 1024;
    policy.max_rgba_bytes = 160 * 1024 * 1024;
    policy.max_gpu_bytes = 512 * 1024 * 1024;
    policy.max_output_bytes = 129 * 1024 * 1024;
    for limit in [policy.max_rgba_bytes, 80 * 1024 * 1024] {
        let mut bounded = policy.clone();
        bounded.max_rgba_bytes = limit;
        let scene = scene_execution_policy(&prepared, &bounded).unwrap();
        let media =
            crate::classic_graphic_frame::graphic_media_policy(prepared.execution(), &scene)
                .unwrap();
        let session = crate::classic_visual::ClassicVisualSession::new(
            prepared.execution().execution().execution().media(),
            &media,
        )
        .unwrap();
        for element in ["back", "front"] {
            let permit = session.decode_admission_for(element).unwrap();
            let pixels = permit.max_pixels.parse::<u64>().unwrap();
            assert_eq!(pixels >= 1920 * 1080, limit == policy.max_rgba_bytes);
        }
    }
    source.layers[0]
        .visual
        .as_mut()
        .unwrap()
        .effects
        .push(SceneEffect {
            id: "blur".into(),
            enabled: true,
            effect: EffectKind::ClassicBlur { intensity: 1.0 },
        });
    let active = prepare_scene_frame(&source).unwrap();
    let active_policy = scene_execution_policy(&active, &policy).unwrap();
    assert_eq!(
        active_policy.max_rgba_bytes,
        policy.max_rgba_bytes - 1920 * 1080 * 16
    );
}

fn graph() -> SceneComposition {
    let mut graph = crate::classic_text_frame::tests::graph();
    graph.fonts = vec![
        crate::font_synthesis::expected_import_resource(&ImportFont {
            project_id: graph.project_id.clone(),
            expected_revision: graph.revision.clone(),
            idempotency_key: "scene-synthesis".into(),
            catalog_id: "google-gudea-bold-synthetic-italic-v1".into(),
            font_id: "font".into(),
            required_text: "AV".into(),
        })
        .unwrap(),
    ];
    for layer in &mut graph.layers {
        if let SampledContent::Text { text } = &mut layer.content {
            text.content = "AV".into();
            text.font_weight = FontWeight::Bold;
            text.font_style = FontStyle::Italic;
        }
    }
    graph
}
fn plan(graph: &SceneComposition) -> Result<SceneFramePlan, EditorError> {
    let prepared = prepare_scene_frame(graph).map_err(|e| EditorError::new(e.code, e.message))?;
    let text = prepared.execution().execution().execution();
    let mut coverage = crate::classic_text_frame::tests::proof(text);
    coverage.extend(prepared.mask_texts().iter().map(|mask| NativeTextCoverage {
        element_id: mask.mask_id.clone(),
        font: mask.font.clone(),
        content: mask.mask.content.clone(),
    }));
    let mut policy = crate::classic_text_frame::tests::policy();
    policy.max_encoded_bytes = 32 * 1024 * 1024;
    policy.max_rgba_bytes = 128 * 1024 * 1024;
    plan_frame(&prepared, &policy, &coverage)
}
#[test]
fn scene_keeps_authored_italic_source_actual_font_and_explicit_owner_binding() {
    let source = graph();
    let before = source.clone();
    let prepared = prepare_scene_frame(&source).unwrap();
    assert_eq!(prepared.source_graph(), &before);
    let native = &prepared.execution().execution().execution().texts()[0];
    assert_eq!(native.font.style, FontStyle::Normal);
    assert_eq!(native.font.content, source.fonts[0].content);
    let result = plan(&source).unwrap();
    assert_eq!(result.font_synthesis.len(), 1);
    let binding = &result.font_synthesis[0];
    assert_eq!(binding.element_id, "title");
    assert_eq!(binding.owner_id, "title");
    assert_eq!(binding.owner_kind, SceneFontSynthesisOwner::Text);
    assert_eq!(binding.binding.actual.style, FontStyle::Normal);
    assert_eq!(binding.binding.requested.style, FontStyle::Italic);
    assert_eq!(
        binding.binding.actual.content_hash,
        source.fonts[0].content.content_hash
    );
    assert_eq!(source, before);
}
#[test]
fn scene_rejects_forged_licensed_resource_and_nonitalic_request_before_execution() {
    let original = graph();
    let mut forged = original.clone();
    forged.fonts[0].license.as_mut().unwrap().attribution = None;
    assert!(plan(&forged).is_err());
    for style in [FontStyle::Normal] {
        let mut changed = original.clone();
        let title = changed
            .layers
            .iter_mut()
            .find(|layer| layer.element_id == "title")
            .unwrap();
        if let SampledContent::Text { text } = &mut title.content {
            text.font_style = style;
        } else {
            panic!("Expected title text");
        }
        assert!(prepare_scene_frame(&changed).is_err());
    }
}
#[test]
fn scene_text_mask_keeps_requested_style_and_distinct_mask_owner() {
    let mut source = graph();
    let mask = Mask {
        id: "italic-mask".into(),
        enabled: true,
        inverted: false,
        shape: LayerMaskShape::Text {
            mask: TextMask {
                content: "AV".into(),
                font: FontBinding::Locked {
                    font_id: "font".into(),
                },
                font_size: FontSize::CanvasPixels { value: 12. },
                font_weight: FontWeight::Bold,
                font_style: FontStyle::Italic,
                decoration: TextDecoration::None,
                letter_spacing_px: 0.,
                line_height_multiplier: 1.,
                center_x: 0.,
                center_y: 0.,
                rotation_degrees: 0.,
                scale: 1.,
            },
        },
        feather: MaskFeather::ClassicPixels { value: 0. },
        stroke: Stroke {
            color: Color {
                encoding: ColorEncoding::Srgb,
                red: 0.,
                green: 0.,
                blue: 0.,
                alpha: 1.,
            },
            width_px: 0.,
            align: StrokeAlign::Outside,
        },
    };
    source
        .layers
        .iter_mut()
        .find(|layer| layer.element_id == "front")
        .unwrap()
        .visual
        .as_mut()
        .unwrap()
        .masks
        .push(mask);
    let result = plan(&source).unwrap();
    assert_eq!(result.font_synthesis.len(), 2);
    let entry = result
        .font_synthesis
        .iter()
        .find(|e| e.owner_kind == SceneFontSynthesisOwner::TextMask)
        .unwrap();
    assert_eq!(entry.element_id, "front");
    assert_eq!(entry.owner_id, "italic-mask");
    assert_eq!(result.mask_fonts[0].font.style, FontStyle::Normal);
    let stack = result
        .stacks
        .iter()
        .find(|stack| stack.element_id == "front")
        .unwrap();
    let LayerMaskShape::Text { mask } = &stack.masks[0].shape else {
        panic!()
    };
    assert_eq!(mask.font_style, FontStyle::Italic);
}

#[test]
fn scene_delegates_sticker_bytes_to_image_without_rewriting_its_source_node() {
    let mut source = crate::classic_text_frame::tests::graph();
    source.layers.retain(|layer| layer.element_id == "front");
    source.fonts.clear();
    source.layers[0].content = SampledContent::Sticker {
        asset_id: "image".into(),
        intrinsic_size: CanvasSize {
            width: 37,
            height: 29,
        },
    };
    let before = source.clone();
    // Frozen text profiles still reject the original sticker node.
    assert!(prepare_text_frame(&source).is_err());
    let prepared = prepare_scene_frame(&source).unwrap();
    assert_eq!(prepared.source_graph(), &before);
    let media = prepared.execution().execution().execution().media();
    assert!(
        matches!(&media.layers()[0].content, SampledContent::Image { asset_id } if asset_id == "image")
    );
    assert_eq!(media.resources(), source.resources.as_slice());
    assert_eq!(prepared.stacks()[0].element_id, "front");
    assert_eq!(source, before);
}

#[test]
fn four_k_legacy_two_sources_keep_grades_and_admit_bounded_serial_decoders() {
    let mut source = graph();
    source.format.canvas_size = CanvasSize {
        width: 3840,
        height: 2160,
    };
    source.fonts.clear();
    source.resources.truncate(1);
    source.resources[0].metadata.intrinsic_size = Some(source.format.canvas_size);
    let mut front_resource = source.resources[0].clone();
    front_resource.asset_id = "front-image".into();
    front_resource.version.content_hash = "c".repeat(64);
    front_resource.version.id = format!("av_{}", front_resource.version.content_hash);
    source.resources.push(front_resource);
    let mut back = source.layers[0].clone();
    back.visual.as_mut().unwrap().masks.clear();
    back.visual.as_mut().unwrap().effects = vec![SceneEffect {
        id: "grade-back".into(),
        enabled: true,
        effect: EffectKind::LegacyColorAdjust {
            exposure_ev: 0.0,
            contrast: 1.1,
            saturation: 1.06,
        },
    }];
    let mut front = back.clone();
    front.element_id = "front".into();
    front.content = SampledContent::Image {
        asset_id: "front-image".into(),
    };
    source.layers = vec![back, front];
    let prepared = prepare_scene_frame(&source).unwrap();
    assert_eq!(prepared.source_graph(), &source);
    let mut policy = crate::classic_text_frame::tests::policy();
    policy.max_frame_pixels = 16_777_216;
    policy.max_decoded_pixels = 16_777_216;
    policy.max_encoded_bytes = 32 * 1024 * 1024;
    policy.max_rgba_bytes = 224 * 1024 * 1024;
    policy.max_gpu_bytes = 512 * 1024 * 1024;
    policy.max_output_bytes = 129 * 1024 * 1024;
    let scene = scene_execution_policy(&prepared, &policy).unwrap();
    let media =
        crate::classic_graphic_frame::graphic_media_policy(prepared.execution(), &scene).unwrap();
    let session = crate::classic_visual::ClassicVisualSession::new(
        prepared.execution().execution().execution().media(),
        &media,
    )
    .unwrap();
    for layer in &source.layers {
        let permit = session.decode_admission_for(&layer.element_id).unwrap();
        assert_eq!(permit.decoder_surfaces, 2);
        assert!(permit.max_pixels.parse::<u64>().unwrap() >= 3840 * 2160);
        assert_eq!(
            permit.max_decoded_bytes.parse::<u64>().unwrap(),
            permit.max_pixels.parse::<u64>().unwrap() * 8
        );
    }
    // Reproduce the real foreground budget and missing imported size hints.
    let mut videos = source.clone();
    for resource in &mut videos.resources {
        resource.kind = opencut_contracts::AssetKind::Video;
        resource.metadata.intrinsic_size = None;
        resource.metadata.mime_type = Some("video/mp4".into());
    }
    let time = |n: i32| opencut_contracts::Time {
        ticks: n.to_string(),
        timebase: 25,
    };
    let range = opencut_contracts::TimeRange {
        start: time(0),
        duration: time(50),
    };
    for layer in &mut videos.layers {
        let SampledContent::Image { asset_id } = &layer.content else {
            unreachable!()
        };
        layer.content = SampledContent::Video {
            asset_id: asset_id.clone(),
            stream: None,
            source_audio: SourceAudioDecision::ScopeExcluded,
            source: SourceClock {
                timeline_local: time(0),
                source_time: time(0),
                point_profile: SourcePointProfile::ClassicVideoRoundedTicks,
                rate: RationalRate {
                    numerator: 1,
                    denominator: 1,
                },
                mapping: RetimeMapping::ClassicRoundedTicks { timebase: 120000 },
                nominal_request: range.clone(),
                requested_read_span: range.clone(),
                declared_resize_extent: Some(time(50)),
                read_window: BoundedReadWindow::Known {
                    resource_extent: time(50),
                    range: Some(range.clone()),
                },
                sample: SourceReadState::InBounds,
                eof_policy: EofPolicy::RequiresBackendProfile,
            },
        };
    }
    let prepared = prepare_scene_frame(&videos).unwrap();
    policy.max_rgba_bytes = 128 * 1024 * 1024;
    let scene = scene_execution_policy(&prepared, &policy).unwrap();
    let media =
        crate::classic_graphic_frame::graphic_media_policy(prepared.execution(), &scene).unwrap();
    let session = crate::classic_visual::ClassicVisualSession::new(
        prepared.execution().execution().execution().media(),
        &media,
    )
    .unwrap();
    for layer in &videos.layers {
        let permit = session.decode_admission_for(&layer.element_id).unwrap();
        assert_eq!(permit.decoder_surfaces, 1);
        assert!(permit.max_pixels.parse::<u64>().unwrap() >= 3840 * 2160);
        assert_eq!(
            permit.max_decoded_bytes.parse::<u64>().unwrap(),
            permit.max_pixels.parse::<u64>().unwrap() * 4
        );
    }
    let decoded: Vec<_> = videos
        .layers
        .iter()
        .map(
            |layer| opencut_contracts::visual_frame::DecodedVisualSource::ReadyVideo {
                element_id: layer.element_id.clone(),
                width: 3840,
                height: 2160,
                requested_time: time(0),
                selected_video_stream: None,
                actual_timestamp_seconds: 0.0,
                frame_duration_seconds: 0.04,
                eof_held: false,
            },
        )
        .collect();
    let bound = crate::classic_caption_frame::bind_caption_frame(
        prepared.execution().execution(),
        &policy,
        &[],
        &decoded,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(bound.decoded.len(), 2);
    source.layers[0]
        .visual
        .as_mut()
        .unwrap()
        .effects
        .push(SceneEffect {
            id: "mixed-blur".into(),
            enabled: true,
            effect: EffectKind::ClassicBlur { intensity: 1.0 },
        });
    assert!(
        prepare_scene_frame(&source)
            .unwrap_err()
            .message
            .contains("Mixed source-space")
    );
}
