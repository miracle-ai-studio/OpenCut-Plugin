use super::*;
use crate::classic_text_frame::tests::{graph, policy, proof};
use crate::visual_preview::VisualFrameObservations;
use opencut_composition::scene::{SampledContent, prepare_caption_frame};
use opencut_contracts::scene::*;
fn fixture() -> PreparedCaptionBindings {
    let mut graph = graph();
    let SampledContent::Text { text } = graph.layers[1].content.clone() else {
        panic!()
    };
    graph.layers[1].content = SampledContent::Caption {
        text: text.content,
        style: SceneCaptionStyle {
            font: text.font,
            font_size: text.font_size,
            color: text.color,
            background_color: None,
            max_width_px: None,
            line_height_px: Some(12.0),
            text_align: text.align,
            layout: CaptionLayoutProfile::PinnedCaptionV4,
        },
    };
    let prepared = prepare_caption_frame(&graph).unwrap();
    let coverage = proof(prepared.execution());
    PreparedCaptionBindings::new(prepared, policy(), coverage).unwrap()
}
fn input(prepared: &PreparedCaptionBindings) -> VisualFrameObservations {
    VisualFrameObservations {
        resolution: opencut_contracts::export_video::RasterResolution::Source,
        prepare_ahead: false,
        retained_picture_bytes: 0,
        hit_query: None,
        session: opencut_api_contracts::audio_preview::AudioPreviewStream {
            session_id: "session".into(),
            generation: "1".into(),
            overlay_epoch: "0".into(),
            owner_id: "owner".into(),
            lease_id: "lease".into(),
        },
        time: Some(prepared.time().clone()),
        decoded: None,
        rasters: None,
        captions: None,
        graphics: None,
    }
}
#[test]
fn caption_preview_exposes_original_facts_and_retains_sound_consumer_freshness() {
    let prepared = fixture();
    let request = input(&prepared);
    let output = prepared.result(&request, prepared.time()).unwrap();
    assert_eq!(
        output.plan.identity.profile,
        "classic-v31-gpu-normal-caption-frame-v3"
    );
    assert_eq!(output.plan.execution.texts[0].text.content, "AB\nC");
    assert_eq!(output.plan.captions[0].max_width_px, 3.2);
    assert!(output.bound.is_none());
    // A one-second consumer advance is independent of renderer observations.
    let current = opencut_time::RationalTime::try_from(prepared.time()).unwrap();
    let stale = Time {
        ticks: (current.ticks() + i64::from(current.timebase())).to_string(),
        timebase: current.timebase(),
    };
    assert_eq!(
        prepared.result(&request, &stale).err().unwrap().code,
        "PREVIEW_FRAME_STALE"
    );
}
#[test]
fn caption_preview_cannot_bind_without_both_caption_and_raster_observations() {
    let prepared = fixture();
    let mut request = input(&prepared);
    request.rasters = Some(vec![]);
    assert_eq!(
        prepared
            .result(&request, prepared.time())
            .err()
            .unwrap()
            .code,
        "INVALID_VISUAL_PLAN"
    );
    request.rasters = None;
    request.captions = Some(vec![]);
    assert!(prepared.result(&request, prepared.time()).is_err());
}
