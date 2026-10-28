use super::*;
use opencut_contracts::{CreateProject, FrameRate, font_import::ImportFont};
use opencut_job_contracts::scene_audio::*;
use serde_json::json;
fn fixture() -> (EditorSnapshot, SceneCaptionStyle) {
    let mut project = crate::scene::new_document(&CreateProject {
        id: "p".into(),
        name: "Captions".into(),
        sequence_id: "s".into(),
        width: 320,
        height: 180,
        fps: FrameRate {
            numerator: 30,
            denominator: 1,
        },
    })
    .unwrap();
    project.fonts.push(
        crate::font_import::expected_import_resource(&ImportFont {
            project_id: "p".into(),
            expected_revision: "r".into(),
            idempotency_key: "font".into(),
            catalog_id: "google-lato-regular-v1".into(),
            font_id: "font".into(),
            required_text: "one two three".into(),
        })
        .unwrap(),
    );
    project.sequences[0].tracks.push(SceneTrack {
        id: "captions".into(),
        name: "Captions".into(),
        kind: SceneTrackKind::Caption,
        enabled: true,
        muted: false,
        locked: false,
        opacity: 1.0,
        gain: Gain::Decibels { value: 0.0 },
        elements: vec![],
        channels: vec![],
    });
    let style = SceneCaptionStyle {
        font: FontBinding::Locked {
            font_id: "font".into(),
        },
        font_size: FontSize::CanvasPixels { value: 24.0 },
        color: Color {
            encoding: ColorEncoding::Srgb,
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha: 1.0,
        },
        background_color: None,
        max_width_px: None,
        line_height_px: None,
        text_align: SceneTextAlign::Center,
        layout: CaptionLayoutProfile::PinnedCaptionV4,
    };
    (
        EditorSnapshot {
            project,
            revision: "r".into(),
        },
        style,
    )
}
fn transcript() -> SceneTranscriptionResult {
    SceneTranscriptionResult{schema_version:1,project_id:"p".into(),revision:"r".into(),sequence_id:"s".into(),duration:Time{ticks:"5005".into(),timebase:1000},audio:AudioWavResult{schema_version:1,identity:AudioWavIdentity{project_id:"p".into(),revision:"r".into(),sequence_id:"s".into(),profile:AudioProfile::ClassicExportV31WebAudio,sample_rate:48000,channels:2,frame_count:"240240".into(),sample_format:AudioSampleFormat::IeeeFloat32Le},asset_lock:opencut_contracts::AssetLockManifest{version:1,revision:"r".into(),sequence_id:"s".into(),assets:vec![]},render_input_sha256:"a".repeat(64),execution:AudioExecutionSummary{host_sha256:"a".repeat(64),runtime:opencut_job_contracts::scene_audio::AudioRuntimeIdentity::HistoricalBrowser(opencut_job_contracts::scene_audio::HistoricalBrowserAudioIdentity{browser_version:"test".into(),browser_sha256:"a".repeat(64),module_sha256:"a".repeat(64),playwright_version:"test".into(),mediabunny_version:"1.41.0".into(),soundtouch_version:"0.3.0".into(),}),decoder_profiles:vec![],pitch_profiles:vec![],mastering_profile:AudioMasteringProfile::ClassicExportWholeSceneWebAudioV31,mastering_applied:false},artifact:AudioArtifactMetadata{mime_type:"audio/wav".into(),size_bytes:"1921978".into(),content_hash:"a".repeat(64)}},transcription:serde_json::from_value(json!({"language":"en","provider":"whisper.cpp","model":"test","segments":[{"start":{"ticks":"1000","timebase":1000},"end":{"ticks":"7000","timebase":1000},"text":"one two three four five six","words":[]}]})).unwrap()}
}
#[test]
fn generation_is_deterministic_owned_and_clipped_without_word_timing_claim() {
    let (snapshot, style) = fixture();
    let result = transcript();
    let input = GenerateCaptions {
        project_id: "p".into(),
        expected_revision: "r".into(),
        sequence_id: "s".into(),
        track_id: "captions".into(),
        analysis_job_id: "j".into(),
        idempotency_key: "generate".into(),
        words_per_caption: 3,
        style,
    };
    let request = generate(&snapshot, &input, &result).unwrap();
    assert_eq!(request, generate(&snapshot, &input, &result).unwrap());
    let proposed = crate::scene::plan(&snapshot, &request).unwrap();
    let cues = &proposed.sequences[0].captions;
    assert_eq!(cues.len(), 2);
    assert_eq!(cues[0].track_id, "captions");
    assert_eq!(cues[0].text, "one two three");
    assert_eq!(cues[0].timeline_range.start.ticks, "1000");
    assert_eq!(cues[1].timeline_range.duration.ticks, "1005");
    assert!(cues.iter().all(|c| c.source_anchor.is_none()));
    let mut stale = result;
    stale.revision = "other".into();
    assert_eq!(
        generate(&snapshot, &input, &stale).unwrap_err().code,
        "REVISION_CONFLICT"
    );
    let mut locked = snapshot;
    locked.project.sequences[0].tracks[1].locked = true;
    assert_eq!(
        generate(&locked, &input, &transcript()).unwrap_err().code,
        "TRACK_LOCKED"
    );
}
#[test]
fn subtitle_interchange_preserves_unicode_times_and_reports_style_losses() {
    let (snapshot, style) = fixture();
    let input = SubtitleImport {
        project_id: "p".into(),
        expected_revision: "r".into(),
        sequence_id: "s".into(),
        track_id: "captions".into(),
        idempotency_key: "import".into(),
        format: SubtitleFormat::Srt,
        content: "1\n00:00:01,125 --> 00:00:02,625\nHello 字幕\n\n".into(),
        style,
    };
    let request = import(&snapshot, &input).unwrap();
    let proposed = crate::scene::plan(&snapshot, &request).unwrap();
    let updated = EditorSnapshot {
        project: proposed,
        revision: "new".into(),
    };
    let out = export(
        &updated,
        &SubtitleExport {
            project_id: "p".into(),
            revision: "new".into(),
            sequence_id: "s".into(),
            track_id: Some("captions".into()),
            format: SubtitleFormat::Srt,
        },
    )
    .unwrap();
    assert_eq!(out.content, input.content);
    assert_eq!(out.cue_count, 1);
    assert!(!out.losses.is_empty());
    let mut invalid_input = input;
    invalid_input.content = "1\n00:00:03,000 --> 00:00:02,000\nBad\n".into();
    assert!(import(&snapshot, &invalid_input).is_err());
    assert!(snapshot.project.sequences[0].captions.is_empty());
}
