use super::*;
use crate::classic_text_frame::tests::{policy, proof, raster};
use crate::visual_preview::VisualFrameObservations;
use opencut_composition::scene::graphic_raster_receipts;
use opencut_contracts::visual_frame::DecodedVisualSource;

fn input(prepared: &PreparedBlendBindings) -> VisualFrameObservations {
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
fn preview_shares_exact_child_admission_and_requires_all_source_bindings() {
    let frame = crate::classic_blend_frame::tests::prepared();
    let text = frame.execution().execution().execution();
    let text_pixels = u64::from(text.identity().width)
        * u64::from(text.identity().height)
        * text.texts().len() as u64;
    let graphic_pixels: u64 = graphic_raster_receipts(frame.execution())
        .unwrap()
        .iter()
        .map(|r| u64::from(r.width) * u64::from(r.height))
        .sum();
    let mut exact = policy();
    exact.max_decoded_pixels = text_pixels + graphic_pixels + 2;
    let coverage = proof(text);
    let original =
        PreparedGraphicBindings::new(frame.execution().clone(), exact.clone(), coverage.clone())
            .unwrap();
    let prepared = PreparedBlendBindings::new(frame, exact, coverage).unwrap();
    let mut request = input(&prepared);
    let plan = prepared.result(&request, prepared.time()).unwrap();
    let child = original.result(&request, original.time()).unwrap();
    assert_eq!(
        serde_json::to_value(plan.admission).unwrap(),
        serde_json::to_value(child.admission).unwrap()
    );
    assert_eq!(
        serde_json::to_value(plan.decode_admissions).unwrap(),
        serde_json::to_value(child.decode_admissions).unwrap()
    );
    assert_eq!(
        serde_json::to_value(plan.plan.execution).unwrap(),
        serde_json::to_value(child.plan).unwrap()
    );
    assert!(plan.bound.is_none() && plan.bound_graphic.is_none());
    request.decoded = Some(
        ["back", "front"]
            .iter()
            .map(|id| DecodedVisualSource::ReadyImage {
                element_id: (*id).into(),
                width: 1,
                height: 1,
            })
            .collect(),
    );
    request.rasters = Some(vec![raster(
        &prepared
            .result(&input(&prepared), prepared.time())
            .unwrap()
            .plan
            .execution
            .execution
            .execution,
    )]);
    request.captions = Some(vec![]);
    let partial = prepared.result(&request, prepared.time()).unwrap();
    assert!(
        partial.bound_execution.is_some()
            && partial.bound_graphic.is_none()
            && partial.bound.is_none()
    );
    request.graphics = Some(graphic_raster_receipts(prepared.prepared.execution()).unwrap());
    let complete = prepared.result(&request, prepared.time()).unwrap();
    let bound = complete.bound.unwrap();
    assert_eq!(bound.execution, complete.bound_graphic.unwrap());
    assert_eq!(bound.blends, prepared.prepared.blends());
    assert_eq!(bound.execution.layers.len(), bound.blends.len());
    request.graphics.as_mut().unwrap()[0].width += 1;
    assert!(prepared.result(&request, prepared.time()).is_err());
}

#[test]
fn preview_preserves_clock_and_incomplete_observation_rejection() {
    let frame = crate::classic_blend_frame::tests::prepared();
    let coverage = proof(frame.execution().execution().execution());
    let prepared = PreparedBlendBindings::new(frame, policy(), coverage).unwrap();
    let mut request = input(&prepared);
    request.graphics = Some(vec![]);
    assert_eq!(
        prepared.result(&request, prepared.time()).unwrap_err().code,
        "INVALID_VISUAL_PLAN"
    );
    request.graphics = None;
    let time = opencut_time::RationalTime::try_from(prepared.time()).unwrap();
    let stale = Time {
        ticks: (time.ticks() + i64::from(time.timebase())).to_string(),
        timebase: time.timebase(),
    };
    assert_eq!(
        prepared.result(&request, &stale).unwrap_err().code,
        "PREVIEW_FRAME_STALE"
    );
    request.time = Some(stale);
    assert_eq!(
        prepared.result(&request, prepared.time()).unwrap_err().code,
        "INVALID_VISUAL_PLAN"
    );
}
