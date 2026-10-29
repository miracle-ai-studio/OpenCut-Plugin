//! Scene frames use the existing fixed-frame queue and resource capture ports.
//! Only the versioned composition/profile and its layout evidence differ.
use crate::{JobRecord, JobRequest, media_frame_jobs as media, scene_text_frame_jobs as text};
use opencut_composition::{CompositionScope, scene::*};
use opencut_contracts::{EditorError, Time, scene::EditorSnapshot};
use opencut_job_contracts::ArtifactDescriptor;
pub use opencut_job_contracts::scene_frame::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use text::PreparedTextFramePng as PreparedSceneFramePng;

pub trait SceneFrameJobSubmissionStore: text::TextFrameJobSubmissionStore {
    fn accept_scene_scene_frame_job(
        &mut self,
        request: JobRequest,
        prepared: PreparedSceneFramePng,
    ) -> Result<JobRecord, EditorError>;
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSceneFramePngResult {
    pub public: SceneFramePngResult,
    pub artifact: ArtifactDescriptor,
}
fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", message)
}
fn bad_result() -> EditorError {
    EditorError::new(
        "INVALID_STORED_RESULT",
        "Scene frame differs from its fixed input or verified layout",
    )
}
pub fn prepare(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    time: &Time,
) -> Result<PreparedSceneFrame, EditorError> {
    for font in &snapshot.project.fonts {
        crate::font_synthesis::binding_for_resource(font)?;
    }
    let graph = compile_frame_scene(snapshot, sequence_id, time, CompositionScope::Visual)
        .map_err(|e| EditorError::new(e.code, e.message))?;
    prepare_scene_frame(&graph).map_err(|e| EditorError::new(e.code, e.message))
}
/// Playback rasterization after authoritative timeline/animation sampling.
pub fn prepare_preview(snapshot: &EditorSnapshot, sequence_id: &str, time: &Time) -> Result<PreparedSceneFrame, EditorError> {
    prepare_raster(snapshot,sequence_id,time,opencut_contracts::export_video::RasterResolution::Preview540)
}
pub fn prepare_raster(snapshot: &EditorSnapshot, sequence_id: &str, time: &Time, resolution: opencut_contracts::export_video::RasterResolution) -> Result<PreparedSceneFrame, EditorError> {
    for font in &snapshot.project.fonts { crate::font_synthesis::binding_for_resource(font)?; }
    let graph = compile_frame_scene(snapshot, sequence_id, time, CompositionScope::Visual)
        .map_err(|e| EditorError::new(e.code, e.message))?;
    prepare_scene_frame(&project_raster(graph,resolution)).map_err(|e| EditorError::new(e.code, e.message))
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use opencut_contracts::scene::{CanvasSize,FontSize};
    #[test]
    fn projection_scales_canvas_pixels_once_and_preserves_sampled_facts() {
        let mut graph=crate::classic_text_frame::tests::graph();
        graph.format.canvas_size=CanvasSize{width:1920,height:1080};
        let layer=&mut graph.layers[1];
        layer.visual.as_mut().unwrap().transform.position_x_px=80.;
        layer.visual.as_mut().unwrap().transform.position_y_px=-40.;
        let SampledContent::Text{text}=&mut layer.content else {panic!()};
        text.font_size=FontSize::ClassicHeight90{value:15.};text.letter_spacing_px=6.;
        text.background.padding_x_at_size15=12.;text.background.offset_y_px=10.;
        let full=prepare_text_raster(&graph,"title").unwrap();
        let projected=project_preview_raster(graph.clone());
        let half=prepare_text_raster(&projected,"title").unwrap();
        assert_eq!((half.identity.width,half.identity.height),(960,540));
        assert_eq!(half.text.font_size_px,90.);
        assert_eq!(half.text.letter_spacing_px,3.);
        assert_eq!(half.text.background.padding_x_px,6.);
        assert_eq!(half.text.background.offset_y_px,5.);
        assert_eq!(half.text.transform.center_x,520.);
        assert_eq!(half.text.transform.center_y,250.);
        assert_eq!(half.text.font_size_px,full.text.font_size_px/2.);
        assert_eq!(projected.time,graph.time);assert_eq!(projected.resources,graph.resources);
        assert_eq!(projected.fonts,graph.fonts);assert_eq!(projected.layers[1].local_time,graph.layers[1].local_time);
        let doubled=project_raster(graph.clone(),opencut_contracts::export_video::RasterResolution::P2160);
        let large=prepare_text_raster(&doubled,"title").unwrap();
        assert_eq!((large.identity.width,large.identity.height),(3840,2160));
        assert_eq!(large.text.font_size_px,full.text.font_size_px*2.);
        assert_eq!(large.text.letter_spacing_px,12.);
        assert_eq!(large.text.background.padding_x_px,24.);
        assert_eq!(large.text.background.offset_y_px,20.);
        assert_eq!(large.text.transform.center_x,2080.);
        assert_eq!(large.text.transform.center_y,1000.);
        assert_eq!(doubled.time,graph.time);assert_eq!(doubled.resources,graph.resources);
        // Absolute fonts already reduce their padding multiplier; never halve it twice.
        let SampledContent::Text{text}=&mut graph.layers[1].content else {panic!()};
        text.font_size=FontSize::CanvasPixels{value:120.};
        let half=prepare_text_raster(&project_preview_raster(graph),"title").unwrap();
        assert_eq!(half.text.font_size_px,60.);assert_eq!(half.text.background.padding_x_px,48.);
    }
}
pub fn validate_prepared(input: &PreparedSceneFramePng) -> Result<PreparedSceneFrame, EditorError> {
    text::validate_policy(&input.processing_policy)?;
    media::validate_authority(&input.authority)?;
    if [
        &input.snapshot.project.id,
        &input.snapshot.revision,
        &input.sequence_id,
    ]
    .into_iter()
    .any(|v| !media::valid_id(v))
    {
        return Err(invalid("Invalid scene frame scope"));
    }
    let frame = prepare(&input.snapshot, &input.sequence_id, &input.time)?;
    crate::classic_scene_frame::admit_scene_frame(&frame, &input.processing_policy)?;
    validate_lock(input, &frame)?;
    Ok(frame)
}
pub fn validate_prepared_request(
    request: &JobRequest,
    prepared: &PreparedSceneFramePng,
) -> Result<(), EditorError> {
    let input: FrameRenderInput = serde_json::from_value(request.input.clone())
        .map_err(|_| invalid("Invalid scene frame request"))?;
    text::validate_submission(&input)?;
    if request.kind != SCENE_FRAME_PNG_KIND
        || request.idempotency_key != input.idempotency_key
        || input.project_id != prepared.snapshot.project.id
        || input.revision != prepared.snapshot.revision
        || input.sequence_id != prepared.sequence_id
        || input.time != prepared.time
    {
        return Err(invalid("Scene frame request differs from preparation"));
    }
    validate_prepared(prepared)?;
    Ok(())
}
pub fn submit_scene_frame_png(
    store: &mut impl SceneFrameJobSubmissionStore,
    assets: &impl text::TextFrameAssetCapture,
    input: FrameRenderInput,
) -> Result<JobRecord, EditorError> {
    text::validate_submission(&input)?;
    let selected = store.fixed_scene_text_frame_snapshot(&input.project_id, &input.revision)?;
    if selected.snapshot.project.id != input.project_id
        || selected.snapshot.revision != input.revision
    {
        return Err(invalid("Authorized scene snapshot differs from request"));
    }
    opencut_editor_core::scene::validate_project_scene(&selected.snapshot.project)?;
    let request = JobRequest {
        idempotency_key: input.idempotency_key.clone(),
        kind: SCENE_FRAME_PNG_KIND.into(),
        input: serde_json::to_value(&input)
            .map_err(|_| invalid("Cannot encode scene frame request"))?,
    };
    if let Some(existing) = store.find_accepted_scene_text_frame_job(&request)? {
        return Ok(existing);
    }
    text::validate_policy(&selected.processing_policy)?;
    media::validate_authority(&selected.authority)?;
    let frame = prepare(&selected.snapshot, &input.sequence_id, &input.time)?;
    crate::classic_scene_frame::admit_scene_frame(&frame, &selected.processing_policy)?;
    let mut assets_ids: Vec<_> = frame
        .execution()
        .execution()
        .execution()
        .media()
        .resources()
        .iter()
        .map(|r| r.asset_id.clone())
        .collect();
    let mut fonts: Vec<_> = frame
        .execution()
        .execution()
        .execution()
        .graph()
        .fonts
        .iter()
        .map(|f| f.id.clone())
        .collect();
    assets_ids.sort();
    fonts.extend(frame.mask_texts().iter().map(|m| m.font.id.clone()));
    fonts.sort();
    fonts.dedup();
    let (asset_lock, font_lock) =
        assets.capture(&selected.snapshot, &input.sequence_id, &assets_ids, &fonts)?;
    let prepared = PreparedSceneFramePng {
        snapshot: selected.snapshot,
        sequence_id: input.sequence_id,
        time: input.time,
        asset_lock,
        font_lock,
        authority: selected.authority,
        processing_policy: selected.processing_policy,
    };
    validate_lock(&prepared, &frame)?;
    store.accept_scene_scene_frame_job(request, prepared)
}
pub fn public_identity(frame: &PreparedSceneFrame) -> SceneFrameIdentity {
    let i = frame.execution().execution().execution().identity();
    SceneFrameIdentity {
        project_id: i.project_id.clone(),
        revision: i.revision.clone(),
        sequence_id: i.sequence_id.clone(),
        time: i.time.clone(),
        width: i.width,
        height: i.height,
        profile: SceneFrameProfile::ClassicV31GpuSceneFrameV1,
    }
}
pub fn render_input_sha256(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    time: &Time,
) -> Result<String, EditorError> {
    let bytes = serde_json::to_vec(&(
        1u32,
        snapshot,
        sequence_id,
        time,
        SceneFrameProfile::ClassicV31GpuSceneFrameV1,
    ))
    .map_err(|_| invalid("Cannot hash scene input"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub fn validate_stored_result(
    input: &PreparedSceneFramePng,
    stored: &StoredSceneFramePngResult,
) -> Result<SceneFramePngResult, EditorError> {
    let frame = validate_prepared(input).map_err(|_| bad_result())?;
    let public = &stored.public;
    crate::classic_caption_frame::validate_caption_receipts(
        frame.execution().execution(),
        &public.texts,
        &public.captions,
    )
    .map_err(|_| bad_result())?;
    validate_graphic_receipts(frame.execution(), &public.graphics).map_err(|_| bad_result())?;
    let decoded = media::private_sources(&public.sources)?;
    crate::classic_visual::ClassicVisualSession::new(
        frame.execution().execution().execution().media(),
        &crate::classic_graphic_frame::graphic_media_policy(
            frame.execution(),
            &input.processing_policy,
        )?,
    )?
    .project_decoded_sources(&decoded)
    .map_err(|_| bad_result())?;
    text::validate_output_evidence(
        &input.processing_policy,
        &public.execution,
        &public.artifact,
        &stored.artifact,
    )?;
    if public.blends != frame.blends()
        || public.stacks != frame.stacks()
        || public.steps != frame.steps()
        || &public.background != frame.background()
        || public.schema_version != 1
        || public.identity != public_identity(&frame)
        || public.asset_lock != input.asset_lock
        || public.font_lock != input.font_lock
        || public.render_input_sha256
            != render_input_sha256(&input.snapshot, &input.sequence_id, &input.time)?
        || public.sources
            != media::public_sources(frame.execution().execution().execution().media(), &decoded)?
    {
        return Err(bad_result());
    }
    Ok(public.clone())
}

pub(crate) fn validate_lock(
    input: &PreparedSceneFramePng,
    frame: &PreparedSceneFrame,
) -> Result<(), EditorError> {
    use std::collections::BTreeMap;
    let text = frame.execution().execution().execution();
    let mut expected: BTreeMap<_, _> = text
        .graph()
        .fonts
        .iter()
        .map(|f| {
            (
                f.id.clone(),
                (f.content.content_hash.clone(), f.content.size),
            )
        })
        .collect();
    for mask in frame.mask_texts() {
        let value = (
            mask.font.content.content_hash.clone(),
            mask.font.content.size,
        );
        if let Some(previous) = expected.insert(mask.font.id.clone(), value.clone()) {
            if previous != value {
                return Err(EditorError::new(
                    "FONT_LOCK_INVALID",
                    "Scene font identity conflicts",
                ));
            }
        }
    }
    let observed: BTreeMap<_, _> = input
        .font_lock
        .iter()
        .map(|f| (f.font_id.clone(), (f.content_hash.clone(), f.size)))
        .collect();
    if observed.len() != input.font_lock.len() || observed != expected {
        return Err(EditorError::new(
            "FONT_LOCK_INVALID",
            "Scene font lock differs from all fixed title and mask fonts",
        ));
    }
    let mut delegated = input.clone();
    delegated
        .font_lock
        .retain(|f| text.graph().fonts.iter().any(|t| t.id == f.font_id));
    text::validate_lock(&delegated, text)
}
