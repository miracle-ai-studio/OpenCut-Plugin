use super::*;
use crate::classic_text_frame::tests::{graph, policy, proof, raster};
use crate::visual_preview::VisualFrameObservations;
use opencut_composition::scene::{SampledContent, graphic_raster_receipts, prepare_graphic_frame};
use opencut_contracts::visual_frame::DecodedVisualSource;
fn fixture() -> PreparedGraphicBindings {
    let mut graph = graph();
    let mut shape = graph.layers[1].clone();
    shape.element_id = "shape".into();
    shape.content = SampledContent::Graphic {
        graphic: crate::scene_graphic_video_jobs::tests::graphic(),
    };
    graph.layers.insert(1, shape);
    let prepared = prepare_graphic_frame(&graph).unwrap();
    let coverage = proof(prepared.execution().execution());
    PreparedGraphicBindings::new(prepared, policy(), coverage).unwrap()
}
fn input(prepared: &PreparedGraphicBindings) -> VisualFrameObservations {
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
fn graphic_preview_binds_all_three_partitions_in_original_order_and_full_policy() {
    let prepared = fixture();
    let mut request = input(&prepared);
    let plan = prepared.result(&request, prepared.time()).unwrap();
    assert_eq!(
        plan.admission.max_decoded_pixels,
        policy().max_decoded_pixels.to_string()
    );
    assert!(plan.bound.is_none() && plan.bound_execution.is_none());
    request.decoded = Some(
        ["back", "front"]
            .into_iter()
            .map(|id| DecodedVisualSource::ReadyImage {
                element_id: id.into(),
                width: 1,
                height: 1,
            })
            .collect(),
    );
    let media = prepared.result(&request, prepared.time()).unwrap();
    assert!(media.bound_media.is_some() && media.bound_execution.is_none());
    request.rasters = Some(vec![raster(&plan.plan.execution.execution)]);
    request.captions = Some(vec![]);
    let child = prepared.result(&request, prepared.time()).unwrap();
    assert!(child.bound_execution.is_some() && child.bound.is_none());
    request.graphics = Some(graphic_raster_receipts(&prepared.prepared).unwrap());
    let complete = prepared.result(&request, prepared.time()).unwrap();
    let bound = complete.bound.unwrap();
    assert_eq!(
        bound
            .layers
            .iter()
            .map(|l| l.element_id.as_str())
            .collect::<Vec<_>>(),
        ["back", "shape", "title", "front"]
    );
    assert_eq!(bound.rasters, request.rasters.clone().unwrap());
    request.graphics.as_mut().unwrap()[0].origin_x += 1;
    assert!(prepared.result(&request, prepared.time()).is_err());
}
#[test]
fn graphic_preview_retains_original_time_and_requires_complete_observations() {
    let prepared = fixture();
    let mut request = input(&prepared);
    request.graphics = Some(vec![]);
    assert_eq!(
        prepared
            .result(&request, prepared.time())
            .err()
            .unwrap()
            .code,
        "INVALID_VISUAL_PLAN"
    );
    request.graphics = None;
    let time = opencut_time::RationalTime::try_from(prepared.time()).unwrap();
    let stale = Time {
        ticks: (time.ticks() + i64::from(time.timebase())).to_string(),
        timebase: time.timebase(),
    };
    assert_eq!(
        prepared.result(&request, &stale).err().unwrap().code,
        "PREVIEW_FRAME_STALE"
    );
    request.time = Some(stale);
    assert_eq!(
        prepared
            .result(&request, prepared.time())
            .err()
            .unwrap()
            .code,
        "INVALID_VISUAL_PLAN"
    );
}
#[test]
fn graphic_preview_reserves_generated_pixels_before_media_decode() {
    let prepared = fixture();
    let mut reduced = policy();
    reduced.max_decoded_pixels = 600;
    assert_eq!(
        PreparedGraphicBindings::new(prepared.prepared, reduced, prepared.coverage)
            .err()
            .unwrap()
            .code,
        "VISUAL_BUDGET_EXCEEDED"
    );
}

#[test]
fn graphic_preview_matches_native_admission_without_deducting_text_and_fonts_twice() {
    let original = fixture();
    let frame = original.prepared.execution().execution();
    let text_pixels = u64::from(frame.identity().width)
        * u64::from(frame.identity().height)
        * frame.texts().len() as u64;
    let graphic_pixels: u64 = graphic_raster_receipts(&original.prepared)
        .unwrap()
        .iter()
        .map(|r| u64::from(r.width) * u64::from(r.height))
        .sum();
    let mut exact = policy();
    // The two one-pixel media layers fit after exactly one generated allocation.
    exact.max_decoded_pixels = graphic_pixels + text_pixels + 2;
    // Retain only enough RGBA for one text/font reservation, the graphic source,
    // the output canvas, bounded encoded reads and the two decoded pixels.
    let fonts: u64 = frame.texts().iter().map(|t| t.font.content.size).sum();
    let encoded: u64 = frame
        .media()
        .resources()
        .iter()
        .map(|r| r.version.size.min(4 * 1024 * 1024) + 65536)
        .sum();
    exact.max_rgba_bytes = graphic_pixels * 16
        + text_pixels * 16
        + fonts * 3
        + u64::from(frame.identity().width) * u64::from(frame.identity().height) * 4
        + encoded
        + 2 * 4;
    let native =
        crate::classic_graphic_frame::graphic_media_policy(&original.prepared, &exact).unwrap();
    let native_media =
        crate::classic_visual::ClassicVisualSession::new(frame.media(), &native).unwrap();
    let expected = native_media.decode_admission().unwrap();
    assert_eq!(expected.max_pixels, "1");
    let prepared =
        PreparedGraphicBindings::new(original.prepared, exact, original.coverage).unwrap();
    let planned = prepared.result(&input(&prepared), prepared.time()).unwrap();
    assert_eq!(planned.decode_admissions.len(), 2);
    for actual in planned.decode_admissions.values() {
        assert_eq!(actual.max_pixels, expected.max_pixels);
        assert_eq!(actual.max_decoded_bytes, expected.max_decoded_bytes);
    }
}
