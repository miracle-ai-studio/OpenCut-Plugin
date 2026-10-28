use super::*;
use opencut_contracts::{CreateProject, FrameRate};
use serde_json::json;
fn fixture() -> (EditorSnapshot, SubtitleTextPlanRequest) {
    let mut project = crate::scene::new_document(&CreateProject {
        id: "p".into(),
        name: "Canvas text".into(),
        sequence_id: "s".into(),
        width: 320,
        height: 180,
        fps: FrameRate {
            numerator: 24,
            denominator: 1,
        },
    })
    .unwrap();
    project.fonts.push(serde_json::from_value(json!({
        "id":"f", "family":"Fixed face", "weight":400,"style":"normal","face_index":0,
        "content":{"id":format!("av_{}", "a".repeat(64)),"content_hash":"a".repeat(64),"size":100},
        "origin":{"kind":"legacy_locator","uri":"/private/font.ttf"},"license":null
    })).unwrap());
    project.sequences[0].tracks.push(serde_json::from_value(json!({
        "id":"titles","name":"Titles","kind":"text","enabled":true,"locked":false,"muted":false,"opacity":1.0,
        "gain":{"unit":"decibels","value":0.0},"channels":[],"elements":[{
            "id":"e","name":"Title","enabled":true,
            "timeline_range":{"start":{"ticks":"0","timebase":1},"duration":{"ticks":"5","timebase":1}},
            "source_span":{"profile":"classic_trimmed","trim_start":{"ticks":"0","timebase":1},"trim_end":{"ticks":"0","timebase":1},"source_duration":null},
            "channels":[], "visual":{"profile":"classic_canvas_v31","opacity":1.0,"blend_mode":"normal","effects":[],"masks":[],
                "transform":{"position_x_px":17.0,"position_y_px":-20.0,"scale_x":2.0,"scale_y":0.5,"rotation_degrees":90.0}},
            "content":{"kind":"text","text":{"content":"AV To\n中文编辑","font":{"kind":"locked","font_id":"f"},
                "font_weight":"normal","font_style":"normal","decoration":"none","align":"center",
                "font_size":{"unit":"classic_height90","value":15.0},
                "color":{"encoding":"srgb","red":1.0,"green":1.0,"blue":1.0,"alpha":1.0},
                "letter_spacing_px":0.0,"line_height_multiplier":1.2,
                "background":{"enabled":false,"color":{"encoding":"srgb","red":0.0,"green":0.0,"blue":0.0,"alpha":1.0},
                    "corner_radius_percent":0.0,"padding_x_at_size15":30.0,"padding_y_at_size15":42.0,"offset_x_px":0.0,"offset_y_px":0.0}}}
        }]
    })).unwrap());
    (
        EditorSnapshot {
            project:project.clone(),
            revision: "r".into(),
        },
        SubtitleTextPlanRequest {
            project_id:"p".into(), expected_revision:"r".into(), sequence_id:"s".into(), idempotency_key:"subtitles".into(),
            track:TextTrackTarget::New{track_id:"subtitles".into()},
            source:SubtitleTextSource::File{format:SubtitleFormat::Srt,content:"1\n00:00:01,000 --> 00:00:02,000\nHello world\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond line\n".into()},
            style: { let ElementContent::Text{text}= &project.sequences[0].tracks.iter().find(|t|t.id=="titles").unwrap().elements[0].content else {unreachable!()}; let mut text=text.clone();text.content.clear();text },
            max_width_px:None,observations:None,
        },
    )
}

#[test]
fn subtitle_text_preparation_keeps_complete_graph_and_exact_ranges() {
    let (snapshot, request) = fixture();
    let before = snapshot.clone();
    let prepared = prepare(&snapshot, &request, None).unwrap();
    assert_eq!(snapshot, before);
    assert_eq!(prepared.frames().len(), 2);
    assert_eq!(
        prepared
            .sequence
            .tracks
            .iter()
            .find(|t| t.id == "titles")
            .unwrap(),
        snapshot.project.sequences[0]
            .tracks
            .iter()
            .find(|t| t.id == "titles")
            .unwrap()
    );
    assert_eq!(
        prepared.sequence.captions,
        snapshot.project.sequences[0].captions
    );
    assert_eq!(
        prepared
            .sequence
            .tracks
            .iter()
            .find(|t| t.id == "subtitles")
            .unwrap()
            .elements[0]
            .timeline_range,
        TimeRange {
            start: Time {
                ticks: "1000".into(),
                timebase: 1000
            },
            duration: Time {
                ticks: "1000".into(),
                timebase: 1000
            }
        }
    );
    assert_eq!(prepared.frames()[0].texts()[0].text.content, "Hello world");
    assert_eq!(prepared.frames()[0].texts()[0].text.font_size_px, 30.0);
    assert_eq!(prepared.frames()[0].layers().len(), 1);
    assert_ne!(prepared.ids[0], prepared.ids[1]);
    assert_eq!(
        prepare(&snapshot, &request, None).unwrap().ids,
        prepared.ids
    );
}
#[test]
fn subtitle_text_rejects_style_content_locked_wrong_track_and_overlap() {
    let (snapshot, request) = fixture();
    let mut bad = request.clone();
    bad.style.content = "injected".into();
    assert!(prepare(&snapshot, &bad, None).is_err());
    bad = request.clone();
    bad.track = TextTrackTarget::Existing {
        track_id: "titles".into(),
    };
    assert!(prepare(&snapshot, &bad, None).is_err()); // overlaps original title
    let mut locked = snapshot.clone();
    locked.project.sequences[0]
        .tracks
        .iter_mut()
        .find(|t| t.id == "titles")
        .unwrap()
        .locked = true;
    assert_eq!(
        prepare(&locked, &bad, None).unwrap_err().code,
        "TRACK_LOCKED"
    );
    locked.project.sequences[0]
        .tracks
        .iter_mut()
        .find(|t| t.id == "titles")
        .unwrap()
        .locked = false;
    locked.project.sequences[0]
        .tracks
        .iter_mut()
        .find(|t| t.id == "titles")
        .unwrap()
        .kind = SceneTrackKind::Video;
    assert!(prepare(&locked, &bad, None).is_err());
    bad = request.clone();
    bad.track = TextTrackTarget::New {
        track_id: "e".into(),
    };
    assert!(prepare(&snapshot, &bad, None).is_err());
    bad = request.clone();
    bad.max_width_px = Some(321);
    assert!(prepare(&snapshot, &bad, None).is_err());
    bad = request.clone();
    bad.source = SubtitleTextSource::File {
        format: SubtitleFormat::Srt,
        content: "1\n00:00:01,000 --> 00:00:03,000\nA\n\n2\n00:00:02,000 --> 00:00:04,000\nB\n"
            .into(),
    };
    assert!(prepare(&snapshot, &bad, None).is_err());
}
#[test]
fn subtitle_text_rejects_missing_fixed_transcript_and_layer_collection() {
    let (snapshot, mut request) = fixture();
    let prepared = prepare(&snapshot, &request, None).unwrap();
    assert!(finish(&prepared, &request, &[]).is_err());
    request.expected_revision = "other".into();
    assert!(prepare(&snapshot, &request, None).is_err());
    request.expected_revision = "r".into();
    request.source = SubtitleTextSource::Transcript {
        analysis_job_id: "job".into(),
        words_per_caption: 3,
    };
    assert!(prepare(&snapshot, &request, None).is_err());
}

fn layers(prepared: &PreparedSubtitleText) -> Vec<TextLayerPlan> {
    prepared
        .frames
        .iter()
        .map(|frame| {
            let plan = &frame.texts()[0];
            TextLayerPlan {
                element_id: plan.identity.element_id.clone(),
                text: crate::classic_text_frame::text_input(&plan.text),
                font: opencut_contracts::text_frame::ValidatedTextFont {
                    font_id: plan.font.id.clone(),
                    content_hash: plan.font.content.content_hash.clone(),
                    size: plan.font.content.size.to_string(),
                    weight: plan.font.weight,
                    style: plan.font.style,
                    face_index: plan.font.face_index,
                    glyph_coverage_validated: true,
                    validated_text_hash: hash(&plan.text.content),
                },
                opacity: plan.opacity,
            }
        })
        .collect()
}
fn observations(
    prepared: &PreparedSubtitleText,
    layers: &[TextLayerPlan],
) -> Vec<SubtitleTextObservation> {
    use opencut_contracts::{caption_frame::*, text_frame::*};
    layers
        .iter()
        .map(|layer| {
            let words = layer.text.content.split_whitespace().collect::<Vec<_>>();
            let wrapped = words.join("\n");
            let line_height = layer.text.line_height_px;
            let block_height = words.len() as f64 * line_height;
            let center = (words.len() - 1) as f64 * line_height / 2.0;
            let mut measurements = vec![CaptionWrapMeasurement {
                content: words.join(" "),
                width: 130.0,
            }];
            measurements.extend(words.iter().map(|s| CaptionWrapMeasurement {
                content: (*s).into(),
                width: 60.0,
            }));
            let mut transform = layer.text.transform.clone();
            transform.center_x = 160.0;
            transform.center_y = 180.0 - 9.0 - block_height / 2.0;
            SubtitleTextObservation {
                caption: CaptionLayoutReceipt {
                    element_id: layer.element_id.clone(),
                    original_text_hash: hash(&layer.text.content),
                    wrapped_text: wrapped.clone(),
                    measurements,
                },
                raster: TextRasterReceipt {
                    element_id: layer.element_id.clone(),
                    width: prepared.frames[0].identity().width,
                    height: 180,
                    layout: TextLayout {
                        font_id: layer.font.font_id.clone(),
                        font_content_hash: layer.font.content_hash.clone(),
                        validated_text_hash: hash(&wrapped),
                        font_weight: layer.font.weight,
                        font_style: layer.font.style,
                        face_index: layer.font.face_index,
                        font_size_px: layer.text.font_size_px,
                        line_height_px: line_height,
                        letter_spacing_px: layer.text.letter_spacing_px,
                        align: "center".into(),
                        baseline: "middle".into(),
                        block: TextBlockMetrics {
                            max_width: 60.0,
                            height: block_height,
                            visual_center_offset: center,
                        },
                        background: None,
                        lines: words
                            .iter()
                            .enumerate()
                            .map(|(i, s)| TextLineMetrics {
                                content: (*s).into(),
                                baseline_y: i as f64 * line_height - center,
                                width: 60.0,
                                ascent: 20.0,
                                descent: 5.0,
                                decoration: None,
                            })
                            .collect(),
                        transform,
                    },
                },
            }
        })
        .collect()
}
#[test]
fn subtitle_text_wraps_and_places_one_atomic_full_scene_edit() {
    let (snapshot, mut request) = fixture();
    request.max_width_px = Some(100);
    let prepared = prepare(&snapshot, &request, None).unwrap();
    let layers = layers(&prepared);
    assert!(
        finish(&prepared, &request, &layers)
            .unwrap()
            .transaction
            .is_none()
    );
    request.observations = Some(observations(&prepared, &layers));
    let result = finish(&prepared, &request, &layers).unwrap();
    let transaction = result.transaction.unwrap();
    assert_eq!(transaction.idempotency_key, "subtitles");
    assert_eq!(transaction.operations.len(), 1);
    let ProjectEditOperation::SequencePut { sequence } = &transaction.operations[0] else {
        panic!("one full scene edit")
    };
    assert_eq!(
        sequence.tracks.iter().find(|t| t.id == "titles").unwrap(),
        snapshot.project.sequences[0]
            .tracks
            .iter()
            .find(|t| t.id == "titles")
            .unwrap()
    );
    assert_eq!(sequence.captions, snapshot.project.sequences[0].captions);
    let element = &sequence
        .tracks
        .iter()
        .find(|t| t.id == "subtitles")
        .unwrap()
        .elements[0];
    let ElementContent::Text { text } = &element.content else {
        panic!()
    };
    let mut expected = request.style.clone();
    expected.content = "Hello\nworld".into();
    assert_eq!(text, &expected);
    assert_eq!(
        element.visual.as_ref().unwrap().transform.position_y_px,
        45.0
    );
    assert_eq!(
        element.timeline_range,
        prepared
            .sequence
            .tracks
            .iter()
            .find(|t| t.id == "subtitles")
            .unwrap()
            .elements[0]
            .timeline_range
    );
}
#[test]
fn subtitle_text_rejects_swapped_extra_missing_and_forged_observations() {
    let (snapshot, mut request) = fixture();
    request.max_width_px = Some(100);
    let prepared = prepare(&snapshot, &request, None).unwrap();
    let layers = layers(&prepared);
    let good = observations(&prepared, &layers);
    for observations in [
        vec![good[0].clone()],
        vec![good[1].clone(), good[0].clone()],
        vec![good[0].clone(), good[1].clone(), good[0].clone()],
    ] {
        request.observations = Some(observations);
        assert!(finish(&prepared, &request, &layers).is_err());
    }
    request.observations = Some(good.clone());
    let mut bad = layers.clone();
    bad[0].font.content_hash = "b".repeat(64);
    assert!(finish(&prepared, &request, &bad).is_err());
    bad = layers.clone();
    bad[0].font.glyph_coverage_validated = false;
    assert!(finish(&prepared, &request, &bad).is_err());
    for field in 0..4 {
        let mut bad = good.clone();
        match field {
            0 => bad[0].raster.width = 319,
            1 => bad[0].raster.layout.transform.center_y += 1.0,
            2 => bad[0].caption.original_text_hash = "b".repeat(64),
            _ => bad[0].raster.layout.font_content_hash = "b".repeat(64),
        };
        request.observations = Some(bad);
        assert!(finish(&prepared, &request, &layers).is_err());
    }
}

#[test]
fn subtitle_text_preparation_checkpoint_stops_before_each_compile() {
    let (snapshot, request) = fixture();
    let mut checks = 0;
    let error = prepare_with_checkpoint(&snapshot, &request, None, &mut || {
        checks += 1;
        if checks == 3 {
            Err(EditorError::new("CANCELLED", "stop"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.code, "CANCELLED");
    assert_eq!(checks, 3);
}

use opencut_job_contracts::scene_audio::*;
fn transcript() -> SceneTranscriptionResult {
    SceneTranscriptionResult{schema_version:1,project_id:"p".into(),revision:"r".into(),sequence_id:"s".into(),duration:Time{ticks:"5005".into(),timebase:1000},audio:AudioWavResult{schema_version:1,identity:AudioWavIdentity{project_id:"p".into(),revision:"r".into(),sequence_id:"s".into(),profile:AudioProfile::ClassicExportV31WebAudio,sample_rate:48000,channels:2,frame_count:"240240".into(),sample_format:AudioSampleFormat::IeeeFloat32Le},asset_lock:opencut_contracts::AssetLockManifest{version:1,revision:"r".into(),sequence_id:"s".into(),assets:vec![]},render_input_sha256:"a".repeat(64),execution:AudioExecutionSummary{host_sha256:"a".repeat(64),runtime:opencut_job_contracts::scene_audio::AudioRuntimeIdentity::HistoricalBrowser(opencut_job_contracts::scene_audio::HistoricalBrowserAudioIdentity{browser_version:"test".into(),browser_sha256:"a".repeat(64),module_sha256:"a".repeat(64),playwright_version:"test".into(),mediabunny_version:"1.41.0".into(),soundtouch_version:"0.3.0".into(),}),decoder_profiles:vec![],pitch_profiles:vec![],mastering_profile:AudioMasteringProfile::ClassicExportWholeSceneWebAudioV31,mastering_applied:false},artifact:AudioArtifactMetadata{mime_type:"audio/wav".into(),size_bytes:"1921978".into(),content_hash:"a".repeat(64)}},transcription:serde_json::from_value(json!({"language":"en","provider":"whisper.cpp","model":"test","segments":[{"start":{"ticks":"1000","timebase":1000},"end":{"ticks":"7000","timebase":1000},"text":"one two three four five six","words":[]}]})).unwrap()}
}
#[test]
fn subtitle_text_transcript_matches_original_caption_timing_and_fixed_scope() {
    let (mut snapshot, mut request) = fixture();
    let result = transcript();
    let mut caption_track = snapshot.project.sequences[0]
        .tracks
        .iter()
        .find(|t| t.id == "titles")
        .unwrap()
        .clone();
    caption_track.id = "captions".into();
    caption_track.kind = SceneTrackKind::Caption;
    caption_track.elements.clear();
    snapshot.project.sequences[0].tracks.push(caption_track);
    request.source = SubtitleTextSource::Transcript {
        analysis_job_id: "analysis".into(),
        words_per_caption: 3,
    };
    let prepared = prepare(&snapshot, &request, Some(&result)).unwrap();
    assert_eq!(
        prepared.ids,
        prepare(&snapshot, &request, Some(&result)).unwrap().ids
    );
    let style = SceneCaptionStyle {
        font: request.style.font.clone(),
        font_size: request.style.font_size.clone(),
        color: request.style.color.clone(),
        background_color: None,
        max_width_px: None,
        line_height_px: None,
        text_align: SceneTextAlign::Center,
        layout: CaptionLayoutProfile::PinnedCaptionV4,
    };
    let old = crate::captions::generate(
        &snapshot,
        &opencut_contracts::subtitles::GenerateCaptions {
            project_id: request.project_id.clone(),
            expected_revision: request.expected_revision.clone(),
            sequence_id: request.sequence_id.clone(),
            track_id: "captions".into(),
            analysis_job_id: "analysis".into(),
            idempotency_key: "legacy-captions".into(),
            words_per_caption: 3,
            style,
        },
        &result,
    )
    .unwrap();
    let legacy = crate::scene::plan(&snapshot, &old).unwrap();
    let captions = &legacy.sequences[0].captions;
    let elements = &prepared
        .sequence
        .tracks
        .iter()
        .find(|t| t.id == "subtitles")
        .unwrap()
        .elements;
    assert_eq!(elements.len(), 2);
    assert_eq!(captions.len(), 2);
    for (element, caption) in elements.iter().zip(captions) {
        let ElementContent::Text { text } = &element.content else {
            panic!()
        };
        assert_eq!(text.content, caption.text);
        assert_eq!(element.timeline_range, caption.timeline_range);
    }
    assert_eq!(
        elements[0].timeline_range.start,
        Time {
            ticks: "1000".into(),
            timebase: 1000
        }
    );
    assert_eq!(
        elements[0].timeline_range.duration,
        Time {
            ticks: "3000".into(),
            timebase: 1000
        }
    );
    assert_eq!(
        elements[1].timeline_range.duration,
        Time {
            ticks: "1005".into(),
            timebase: 1000
        }
    );
    assert!(prepared.warnings[0].contains("estimated"));
    let mut stale = result;
    stale.revision = "other".into();
    assert!(prepare(&snapshot, &request, Some(&stale)).is_err());
}
