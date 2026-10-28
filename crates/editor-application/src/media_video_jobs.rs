//! Fixed-revision CFR video admission and result validation. Native execution
//! uses the same complete text/media frames and whole-scene Classic master.
use crate::media_frame_jobs::{valid_hash, valid_id, validate_authority};
use crate::{JobRecord, JobRequest};
use opencut_composition::{CompositionScope, scene::*};
pub use opencut_contracts::text_frame::FontLock;
use opencut_contracts::{AssetLockManifest, EditorError, Time, scene::*};
use opencut_job_contracts::ArtifactDescriptor;
pub use opencut_job_contracts::media_video::*;
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;
pub use opencut_job_execution::media_video::*;
pub use opencut_job_execution::scene_audio::TrustedJobAuthority;
use opencut_time::RationalTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

pub struct AuthorizedVideoSnapshot {
    pub snapshot: EditorSnapshot,
    pub authority: TrustedJobAuthority,
    pub processing_policy: LocalVideoExecutionPolicy,
}
pub trait MediaVideoJobSubmissionStore {
    fn fixed_scene_video_snapshot(
        &self,
        project_id: &str,
        revision: &str,
    ) -> Result<AuthorizedVideoSnapshot, EditorError>;
    fn find_accepted_scene_video_job(
        &self,
        request: &JobRequest,
    ) -> Result<Option<JobRecord>, EditorError>;
    fn accept_scene_video_job(
        &mut self,
        request: JobRequest,
        prepared: PreparedMediaVideoMp4,
    ) -> Result<JobRecord, EditorError>;
}
pub trait MediaVideoAssetCapture {
    /// Verify participating CAS versions under the collection gate held through acceptance.
    fn capture(
        &self,
        snapshot: &EditorSnapshot,
        sequence_id: &str,
        asset_ids: &[String],
        font_ids: &[String],
    ) -> Result<(AssetLockManifest, Vec<FontLock>), EditorError>;
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredVideoMp4Result {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_decoder_evidence_v1: Option<crate::classic_audio::ClassicAudioDecoderEvidence>,
    pub public: MediaVideoMp4Result,
    pub artifact: ArtifactDescriptor,
}
/// One fixed audio preparation plus bounded frame-index planning, never N copies
/// of a complete snapshot or another video-specific visual layout.
pub struct PreparedVideo {
    pub identity: MediaVideoIdentity,
    pub audio: PreparedAudio,
    pub asset_ids: Vec<String>,
    pub font_ids: Vec<String>,
}
/// Internal configurable export selection. Legacy MP4 callers always request
/// audio and retain their original mandatory-audio preparation and wire schema.
pub struct PreparedVideoSelection {
    pub identity: MediaVideoIdentity,
    pub audio: Option<PreparedAudio>,
    pub asset_ids: Vec<String>,
    pub font_ids: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoFrameProfile {
    TextV2,
    CaptionV3,
    GraphicV1,
    BlendV1,
    MaskV1,
    SceneV1,
}
impl VideoFrameProfile {
    pub fn kind(self) -> &'static str {
        match self {
            Self::TextV2 => MEDIA_VIDEO_MP4_KIND,
            Self::CaptionV3 => opencut_job_contracts::scene_caption_video::CAPTION_VIDEO_MP4_KIND,
            Self::SceneV1 => opencut_job_contracts::scene_video::SCENE_VIDEO_MP4_KIND,
            Self::MaskV1 => opencut_job_contracts::scene_mask_video::MASK_VIDEO_MP4_KIND,
            Self::BlendV1 => opencut_job_contracts::scene_blend_video::BLEND_VIDEO_MP4_KIND,
            Self::GraphicV1 => opencut_job_contracts::scene_graphic_video::GRAPHIC_VIDEO_MP4_KIND,
        }
    }
}
pub enum PreparedVideoFrame {
    Text(PreparedTextFrame),
    Caption(PreparedCaptionFrame),
    Graphic(PreparedGraphicFrame),
    Blend(PreparedBlendFrame),
    Mask(PreparedMaskFrame),
    Scene(PreparedSceneFrame),
}
impl PreparedVideoFrame {
    pub fn execution(&self) -> &PreparedTextFrame {
        match self {
            Self::Text(frame) => frame,
            Self::Caption(frame) => frame.execution(),
            Self::Graphic(frame) => frame.execution().execution(),
            Self::Mask(frame) => frame.execution().execution().execution(),
            Self::Scene(frame) => frame.execution().execution().execution(),
            Self::Blend(frame) => frame.execution().execution().execution(),
        }
    }
    /// Admission belongs to the complete selected frame, including generated sources.
    pub fn admit(&self, policy: &LocalVisualExecutionPolicy) -> Result<(), EditorError> {
        match self {
            Self::Mask(frame) => crate::classic_mask_frame::admit_mask_frame(frame, policy),
            Self::Scene(frame) => crate::classic_scene_frame::admit_scene_frame(frame, policy),
            Self::Blend(frame) => crate::classic_blend_frame::admit_blend_frame(frame, policy),
            Self::Graphic(frame) => {
                crate::classic_graphic_frame::admit_graphic_frame(frame, policy)
            }
            _ => crate::classic_text_frame::admit_text_frame(self.execution(), policy).map(|_| ()),
        }
    }
}
pub fn prepare_profiled_frame(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    at: &Time,
    profile: VideoFrameProfile,
) -> Result<PreparedVideoFrame, EditorError> {
    match profile {
        VideoFrameProfile::TextV2 => {
            prepare_frame(snapshot, sequence_id, at).map(PreparedVideoFrame::Text)
        }
        VideoFrameProfile::CaptionV3 => {
            crate::scene_caption_frame_jobs::prepare(snapshot, sequence_id, at)
                .map(PreparedVideoFrame::Caption)
        }
        VideoFrameProfile::SceneV1 => crate::scene_frame_jobs::prepare(snapshot, sequence_id, at)
            .map(PreparedVideoFrame::Scene),
        VideoFrameProfile::MaskV1 => {
            crate::scene_mask_frame_jobs::prepare(snapshot, sequence_id, at)
                .map(PreparedVideoFrame::Mask)
        }
        VideoFrameProfile::BlendV1 => {
            crate::scene_blend_frame_jobs::prepare(snapshot, sequence_id, at)
                .map(PreparedVideoFrame::Blend)
        }
        VideoFrameProfile::GraphicV1 => {
            crate::scene_graphic_frame_jobs::prepare(snapshot, sequence_id, at)
                .map(PreparedVideoFrame::Graphic)
        }
    }
}
fn valid_authored_box_masks(element: &TimelineElement, visual: &VisualStyle) -> bool {
    let active = visual
        .masks
        .iter()
        .filter(|m| m.enabled)
        .collect::<Vec<_>>();
    active.len() <= 1
        && (active.is_empty()
            || matches!(
                element.content,
                ElementContent::Video { .. }
                    | ElementContent::Image { .. }
                    | ElementContent::Graphic { .. }
            ))
        && active.iter().all(|m| {
            !m.inverted
                && m.stroke.width_px == 0.0
                && matches!(m.feather, MaskFeather::ClassicPixels { .. })
                && matches!(
                    m.shape,
                    LayerMaskShape::Rectangle { .. } | LayerMaskShape::Ellipse { .. }
                )
        })
}
fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", message)
}
fn stored_invalid() -> EditorError {
    EditorError::new(
        "INVALID_STORED_RESULT",
        "Stored V4 video result differs from its fixed input",
    )
}
fn budget() -> EditorError {
    EditorError::new(
        "VIDEO_BUDGET_EXCEEDED",
        "Whole-scene video exceeds host admission",
    )
}
fn unsupported() -> EditorError {
    EditorError::new(
        "UNSUPPORTED_VIDEO_PROFILE",
        "Video contains unsupported scene content or geometry",
    )
}
pub fn validate_submission(input: &FixedVideoInput) -> Result<(), EditorError> {
    if [
        &input.idempotency_key,
        &input.project_id,
        &input.revision,
        &input.sequence_id,
    ]
    .into_iter()
    .all(|v| valid_id(v))
    {
        Ok(())
    } else {
        Err(invalid(
            "Video submission requires bounded fixed identities",
        ))
    }
}
pub fn validate_policy(policy: &LocalVideoExecutionPolicy) -> Result<(), EditorError> {
    crate::media_frame_jobs::validate_policy(&policy.visual)?;
    crate::scene_audio_jobs::validate_policy(&policy.audio)?;
    if policy.max_frames == 0
        || policy.max_frames > MAX_VIDEO_FRAMES
        || policy.max_output_bytes == 0
        || policy.max_output_bytes > i64::MAX as u64
        || policy.execution_limit().is_none()
    {
        return Err(budget());
    }
    Ok(())
}
pub fn frame_time(identity: &MediaVideoIdentity, index: u64) -> Result<Time, EditorError> {
    let count = crate::classic_audio::decimal(&identity.frame_count)?;
    if index >= count {
        return Err(invalid("CFR frame index is outside the fixed scene"));
    }
    grid_time(&identity.fps, index)
}
pub fn grid_time(fps: &opencut_contracts::FrameRate, index: u64) -> Result<Time, EditorError> {
    let ticks = u128::from(index)
        .checked_mul(u128::from(fps.denominator))
        .ok_or_else(budget)?;
    Ok(RationalTime::new(i64::try_from(ticks).map_err(|_| budget())?, fps.numerator)?.into())
}
/// Resource membership changes only at authored element/caption boundaries.
/// Reuse the original frame compiler at those exact output-grid positions;
/// animation samples scalar geometry/style, never a media or font identity.
/// This inventory is not a proof that every animated frame fits its budget:
/// the worker admits each actual frame before allocation and publication.
fn inventory_frames(
    sequence: &Scene,
    fps: &opencut_contracts::FrameRate,
    first: u64,
    count: u64,
) -> Result<BTreeSet<u64>, EditorError> {
    let end = first.checked_add(count).ok_or_else(budget)?;
    if count == 0 {
        return Err(budget());
    }
    let mut frames = BTreeSet::from([first, end - 1]);
    for range in sequence
        .tracks
        .iter()
        .flat_map(|track| track.elements.iter().map(|element| &element.timeline_range))
        .chain(
            sequence
                .captions
                .iter()
                .map(|caption| &caption.timeline_range),
        )
    {
        let range_end = opencut_time::range_end(range)?;
        for at in [&range.start, &range_end] {
            let at = RationalTime::try_from(at)?;
            if at.ticks() < 0 {
                return Err(invalid("Video inventory boundary must be nonnegative"));
            }
            let numerator = i128::from(at.ticks()) * i128::from(fps.numerator);
            let denominator = i128::from(at.timebase()) * i128::from(fps.denominator);
            let index =
                u64::try_from((numerator + denominator - 1) / denominator).map_err(|_| budget())?;
            if first <= index && index < end {
                frames.insert(index);
            }
        }
    }
    Ok(frames)
}
pub fn prepare_frame(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    at: &Time,
) -> Result<PreparedTextFrame, EditorError> {
    crate::font_synthesis::reject_legacy_project(&snapshot.project)?;
    let graph = compile_frame_scene(snapshot, sequence_id, at, CompositionScope::Visual)
        .map_err(|e| EditorError::new(e.code, e.message))?;
    prepare_text_frame(&graph).map_err(|e| EditorError::new(e.code, e.message))
}
pub fn prepare(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    policy: &LocalVideoExecutionPolicy,
) -> Result<PreparedVideo, EditorError> {
    prepare_with_profile(snapshot, sequence_id, policy, VideoFrameProfile::TextV2)
}
pub fn prepare_with_profile(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    policy: &LocalVideoExecutionPolicy,
    profile: VideoFrameProfile,
) -> Result<PreparedVideo, EditorError> {
    let selected = prepare_with_audio_selection(snapshot, sequence_id, policy, profile, true)?;
    Ok(PreparedVideo {
        identity: selected.identity,
        audio: selected
            .audio
            .expect("Legacy MP4 always includes audio preparation"),
        asset_ids: selected.asset_ids,
        font_ids: selected.font_ids,
    })
}
pub fn prepare_with_audio_selection(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    policy: &LocalVideoExecutionPolicy,
    profile: VideoFrameProfile,
    include_audio: bool,
) -> Result<PreparedVideoSelection, EditorError> {
    prepare_with_frame_window(snapshot, sequence_id, policy, profile, include_audio, None)
}
/// Range rendering keeps the original graph and clock. Only selected visual frames
/// are admitted; audio retains whole-scene admission because mastering is global.
pub fn prepare_with_frame_window(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    policy: &LocalVideoExecutionPolicy,
    profile: VideoFrameProfile,
    include_audio: bool,
    window: Option<&opencut_job_contracts::export_video::VideoFrameRange>,
) -> Result<PreparedVideoSelection, EditorError> {
    validate_policy(policy)?;
    let admission_started = Instant::now();
    if profile != VideoFrameProfile::SceneV1 {
        crate::font_synthesis::reject_legacy_project(&snapshot.project)?;
    }
    opencut_editor_core::scene::validate_project_scene(&snapshot.project)?;
    let sequence = snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == sequence_id)
        .ok_or_else(|| invalid("Video sequence does not exist"))?;
    let (settings, _) = resolve_scene_format(&snapshot.project, sequence)
        .map_err(|e| EditorError::new(e.code, e.message))?;
    if profile != VideoFrameProfile::SceneV1 && settings.profile != FormatProfile::ClassicGlobal {
        return Err(unsupported());
    }
    if (profile == VideoFrameProfile::TextV2 && !sequence.captions.is_empty())
        || (profile != VideoFrameProfile::SceneV1 && !sequence.transitions.is_empty())
    {
        return Err(unsupported());
    }
    let mut duration = RationalTime::new(0, 1)?;
    for track in &sequence.tracks {
        for element in &track.elements {
            let end = RationalTime::try_from(&opencut_time::range_end(&element.timeline_range)?)?;
            if end.cmp_exact(duration).is_gt() {
                duration = end;
            }
            // Reject unsupported authored kinds even if shorter than one output
            // frame; sampling must never silently make an unsupported node vanish.
            if !matches!(
                element.content,
                ElementContent::Video { .. }
                    | ElementContent::Image { .. }
                    | ElementContent::Audio { .. }
                    | ElementContent::Text { .. }
            ) && !(matches!(
                profile,
                VideoFrameProfile::GraphicV1
                    | VideoFrameProfile::BlendV1
                    | VideoFrameProfile::MaskV1
                    | VideoFrameProfile::SceneV1
            ) && matches!(element.content, ElementContent::Graphic { .. }))
                && !(profile == VideoFrameProfile::SceneV1
                    && matches!(
                        element.content,
                        ElementContent::EffectLayer { .. } | ElementContent::Sticker { .. }
                    ))
            {
                return Err(unsupported());
            }
            if element.visual.as_ref().is_some_and(|visual| {
                (visual.profile != VisualProfile::ClassicCanvasV31
                    && !(profile == VideoFrameProfile::SceneV1
                        && visual.profile == VisualProfile::LegacyCompositionV3
                        && matches!(
                            element.content,
                            ElementContent::Image { .. } | ElementContent::Video { .. }
                        )))
                    || (!matches!(
                        profile,
                        VideoFrameProfile::BlendV1
                            | VideoFrameProfile::MaskV1
                            | VideoFrameProfile::SceneV1
                    ) && visual.blend_mode != BlendMode::Normal)
                    || (profile != VideoFrameProfile::SceneV1 && !visual.effects.is_empty())
                    || (profile != VideoFrameProfile::SceneV1
                        && !visual.masks.is_empty()
                        && (profile != VideoFrameProfile::MaskV1
                            || !valid_authored_box_masks(element, visual)))
            }) || matches!(&element.content, ElementContent::Text { text } if !matches!(text.font, FontBinding::Locked { .. }))
            {
                return Err(unsupported());
            }
        }
    }
    if profile != VideoFrameProfile::TextV2 {
        for caption in &sequence.captions {
            if caption.style.layout != CaptionLayoutProfile::PinnedCaptionV4
                || !matches!(caption.style.font, FontBinding::Locked { .. })
            {
                return Err(unsupported());
            }
            let end = RationalTime::try_from(&opencut_time::range_end(&caption.timeline_range)?)?;
            if end.cmp_exact(duration).is_gt() {
                duration = end;
            }
        }
    }
    if duration.ticks() <= 0 {
        return Err(invalid("Video requires a nonempty complete scene"));
    }
    let numerator = i128::from(duration.ticks()) * i128::from(settings.fps.numerator);
    let denominator = i128::from(duration.timebase()) * i128::from(settings.fps.denominator);
    let frame_count =
        u64::try_from((numerator + denominator - 1) / denominator).map_err(|_| budget())?;
    let (first_frame, output_frames) = if let Some(window) = window {
        let first = crate::classic_audio::decimal(&window.start_frame)?;
        let count = crate::classic_audio::decimal(&window.frame_count)?;
        if count == 0 || first.checked_add(count).is_none_or(|end| end > frame_count) {
            return Err(invalid("Video frame interval is outside the fixed scene"));
        }
        (first, count)
    } else {
        (0, frame_count)
    };
    if output_frames == 0 || output_frames > policy.max_frames {
        return Err(budget());
    }
    // These are counter/time addressability checks,not a reservation for a
    // whole video in RAM. The worker retains and admits one frame at a time.
    u64::from(settings.canvas_size.width)
        .checked_mul(u64::from(settings.canvas_size.height))
        .and_then(|bytes| bytes.checked_mul(4))
        .and_then(|bytes| bytes.checked_mul(output_frames))
        .ok_or_else(budget)?;
    grid_time(&settings.fps, first_frame + output_frames - 1)?;
    let audio = if include_audio {
        Some(
            prepare_audio_scene(
                snapshot,
                sequence_id,
                AudioExecutionProfile::ClassicV31,
                PcmFormat {
                    sample_rate: 48_000,
                    channels: 2,
                },
            )
            .map_err(|e| EditorError::new(e.code, e.message))?,
        )
    } else {
        None
    };
    if let Some(audio) = &audio {
        crate::classic_audio::ClassicAudioSession::new(
            audio,
            policy.audio.max_pcm_bytes,
            policy.audio.max_encoded_bytes,
        )?;
    }
    let identity = MediaVideoIdentity {
        project_id: snapshot.project.id.clone(),
        revision: snapshot.revision.clone(),
        sequence_id: sequence_id.into(),
        profile: MediaVideoProfile::ClassicV31CfrH264AacV1,
        width: settings.canvas_size.width,
        height: settings.canvas_size.height,
        fps: settings.fps.clone(),
        frame_count: output_frames.to_string(),
        timeline_duration: duration.into(),
        video_duration: grid_time(&settings.fps, output_frames)?,
        sample_rate: 48_000,
        channels: 2,
        audio_frame_count: audio
            .as_ref()
            .map_or(0, |a| a.duration_samples())
            .to_string(),
    };
    if identity.width % 2 != 0 || identity.height % 2 != 0 {
        return Err(unsupported());
    }
    let mut assets: BTreeSet<String> = audio
        .as_ref()
        .into_iter()
        .flat_map(|audio| audio.resources())
        .map(|r| r.asset_id.clone())
        .collect();
    let mut visual_assets = BTreeSet::new();
    let mut fonts = BTreeSet::new();
    for index in inventory_frames(sequence, &identity.fps, first_frame, output_frames)? {
        // Bound preparation by the captured admission deadline. Duration does
        // not create more inventory frames or a second complete render pass.
        if admission_started.elapsed()
            >= Duration::from_millis(policy.execution_limit().ok_or_else(budget)?.milliseconds())
        {
            return Err(EditorError::new(
                "VIDEO_TIME_LIMIT",
                "Video admission exceeded its captured deadline",
            ));
        }
        let selected = prepare_profiled_frame(
            snapshot,
            sequence_id,
            &grid_time(&identity.fps, index)?,
            profile,
        )?;
        selected.admit(&policy.visual)?;
        let frame = selected.execution();
        visual_assets.extend(frame.media().resources().iter().map(|r| r.asset_id.clone()));
        fonts.extend(frame.graph().fonts.iter().map(|f| f.id.clone()));
        if let PreparedVideoFrame::Scene(scene) = &selected {
            fonts.extend(scene.mask_texts().iter().map(|m| m.font.id.clone()));
        }
    }
    // This is the complete immutable authority/resource lock,not simultaneous
    // residency. Audio and every actual visual frame have separate admission.
    assets.extend(visual_assets.iter().cloned());
    if assets.iter().any(|id| fonts.contains(id)) {
        return Err(invalid("Font and media identities must be distinct"));
    }
    // Do not charge sequential images/fonts or the full authority inventory
    // against one frame's32-resource/encoded-byte cache. The same original
    // per-frame policy remains enforced above and by every worker frame.
    Ok(PreparedVideoSelection {
        identity,
        audio,
        asset_ids: assets.into_iter().collect(),
        font_ids: fonts.into_iter().collect(),
    })
}
pub(crate) fn validate_lock(
    input: &PreparedMediaVideoMp4,
    plan: &PreparedVideo,
) -> Result<(), EditorError> {
    let expected: BTreeMap<_, _> = plan
        .asset_ids
        .iter()
        .map(|id| {
            let a = input
                .snapshot
                .project
                .assets
                .iter()
                .find(|a| &a.id == id)
                .expect("admitted fixed resource");
            (id.clone(), (a.version.content_hash.clone(), a.version.size))
        })
        .collect();
    let observed: BTreeMap<_, _> = input
        .asset_lock
        .assets
        .iter()
        .map(|a| (a.asset_id.clone(), (a.content_hash.clone(), a.size)))
        .collect();
    if input.asset_lock.version != 1
        || input.asset_lock.revision != input.snapshot.revision
        || input.asset_lock.sequence_id != input.sequence_id
        || observed.len() != input.asset_lock.assets.len()
        || observed != expected
    {
        return Err(EditorError::new(
            "ASSET_LOCK_INVALID",
            "Video lock differs from all fixed participating media",
        ));
    }
    let expected: BTreeMap<_, _> = plan
        .font_ids
        .iter()
        .map(|id| {
            let f = input
                .snapshot
                .project
                .fonts
                .iter()
                .find(|f| &f.id == id)
                .expect("admitted fixed font");
            (id.clone(), (f.content.content_hash.clone(), f.content.size))
        })
        .collect();
    let observed: BTreeMap<_, _> = input
        .font_lock
        .iter()
        .map(|f| (f.font_id.clone(), (f.content_hash.clone(), f.size)))
        .collect();
    if observed.len() != input.font_lock.len() || observed != expected {
        return Err(EditorError::new(
            "FONT_LOCK_INVALID",
            "Video font lock differs from all fixed participating fonts",
        ));
    }
    Ok(())
}
pub fn validate_prepared(input: &PreparedMediaVideoMp4) -> Result<PreparedVideo, EditorError> {
    validate_prepared_with_profile(input, VideoFrameProfile::TextV2)
}
pub fn validate_prepared_with_profile(
    input: &PreparedMediaVideoMp4,
    profile: VideoFrameProfile,
) -> Result<PreparedVideo, EditorError> {
    validate_authority(&input.authority)?;
    if ![
        &input.snapshot.project.id,
        &input.snapshot.revision,
        &input.sequence_id,
    ]
    .into_iter()
    .all(|v| valid_id(v))
    {
        return Err(invalid("Invalid fixed video scope"));
    }
    let plan = prepare_with_profile(
        &input.snapshot,
        &input.sequence_id,
        &input.processing_policy,
        profile,
    )?;
    validate_lock(input, &plan)?;
    Ok(plan)
}
pub fn validate_prepared_request(
    request: &JobRequest,
    input: &PreparedMediaVideoMp4,
) -> Result<(), EditorError> {
    validate_prepared_request_with_profile(request, input, VideoFrameProfile::TextV2)
}
pub fn validate_prepared_request_with_profile(
    request: &JobRequest,
    input: &PreparedMediaVideoMp4,
    profile: VideoFrameProfile,
) -> Result<(), EditorError> {
    let public: FixedVideoInput = serde_json::from_value(request.input.clone())
        .map_err(|_| invalid("Invalid public video request"))?;
    validate_submission(&public)?;
    if request.kind != profile.kind()
        || request.idempotency_key != public.idempotency_key
        || public.project_id != input.snapshot.project.id
        || public.revision != input.snapshot.revision
        || public.sequence_id != input.sequence_id
    {
        return Err(invalid("Video request differs from private preparation"));
    }
    validate_prepared_with_profile(input, profile)?;
    Ok(())
}
pub fn submit_video_mp4(
    store: &mut impl MediaVideoJobSubmissionStore,
    assets: &impl MediaVideoAssetCapture,
    input: FixedVideoInput,
) -> Result<JobRecord, EditorError> {
    submit_video_mp4_with_profile(store, assets, input, VideoFrameProfile::TextV2)
}
pub fn submit_video_mp4_with_profile(
    store: &mut impl MediaVideoJobSubmissionStore,
    assets: &impl MediaVideoAssetCapture,
    input: FixedVideoInput,
    profile: VideoFrameProfile,
) -> Result<JobRecord, EditorError> {
    validate_submission(&input)?;
    let authorized = store.fixed_scene_video_snapshot(&input.project_id, &input.revision)?;
    if authorized.snapshot.project.id != input.project_id
        || authorized.snapshot.revision != input.revision
    {
        return Err(invalid("Authorized revision differs from video request"));
    }
    let request = JobRequest {
        idempotency_key: input.idempotency_key.clone(),
        kind: profile.kind().into(),
        input: serde_json::to_value(&input)
            .map_err(|_| invalid("Cannot serialize video request"))?,
    };
    if let Some(existing) = store.find_accepted_scene_video_job(&request)? {
        return Ok(existing);
    }
    validate_authority(&authorized.authority)?;
    let plan = prepare_with_profile(
        &authorized.snapshot,
        &input.sequence_id,
        &authorized.processing_policy,
        profile,
    )?;
    let (asset_lock, font_lock) = assets.capture(
        &authorized.snapshot,
        &input.sequence_id,
        &plan.asset_ids,
        &plan.font_ids,
    )?;
    let prepared = PreparedMediaVideoMp4 {
        snapshot: authorized.snapshot,
        sequence_id: input.sequence_id,
        asset_lock,
        font_lock,
        authority: authorized.authority,
        processing_policy: authorized.processing_policy,
    };
    validate_lock(&prepared, &plan)?;
    store.accept_scene_video_job(request, prepared)
}
pub fn render_input_sha256(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
) -> Result<String, EditorError> {
    let bytes = serde_json::to_vec(&(
        1u32,
        snapshot,
        sequence_id,
        MediaVideoProfile::ClassicV31CfrH264AacV1,
        48_000u32,
        2u32,
    ))
    .map_err(|_| invalid("Cannot hash fixed video input"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub fn validate_stored_result(
    input: &PreparedMediaVideoMp4,
    stored: &StoredVideoMp4Result,
) -> Result<MediaVideoMp4Result, EditorError> {
    let plan = validate_prepared(input).map_err(|_| stored_invalid())?;
    let public = &stored.public;
    if public.schema_version != 1
        || public.identity != plan.identity
        || public.asset_lock != input.asset_lock
        || public.font_lock != input.font_lock
        || public.render_input_sha256 != render_input_sha256(&input.snapshot, &input.sequence_id)?
    {
        return Err(stored_invalid());
    }
    validate_result_evidence(
        input,
        &plan,
        &public.execution,
        &public.artifact,
        &stored.artifact,
        stored.audio_decoder_evidence_v1.as_ref(),
    )?;
    Ok(public.clone())
}
pub(crate) fn validate_result_evidence(
    input: &PreparedMediaVideoMp4,
    plan: &PreparedVideo,
    execution: &MediaVideoExecutionSummary,
    artifact: &opencut_job_contracts::media_frame::MediaFrameArtifactMetadata,
    descriptor: &ArtifactDescriptor,
    decoder_evidence: Option<&crate::classic_audio::ClassicAudioDecoderEvidence>,
) -> Result<(), EditorError> {
    let visual = &execution.visual;
    let encoder = &execution.encoder;
    crate::scene_audio_jobs::validate_execution_summary(
        &plan.audio,
        &execution.audio,
        decoder_evidence,
    )
    .map_err(|_| stored_invalid())?;
    let expected_rgba = crate::classic_audio::decimal(&plan.identity.frame_count)?
        .checked_mul(u64::from(plan.identity.width))
        .and_then(|n| n.checked_mul(u64::from(plan.identity.height)))
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(stored_invalid)?;
    let seconds = |t: &Time| -> Result<f64, EditorError> {
        let r = RationalTime::try_from(t)?;
        Ok(r.ticks() as f64 / f64::from(r.timebase()))
    };
    let video_seconds = seconds(&plan.identity.video_duration)?;
    let audio_seconds = plan.audio.duration_samples() as f64 / 48_000.0;
    if !crate::classic_visual::validate_visual_audio_identity(visual, &execution.audio)
        || crate::classic_visual::validate_visual_summary(visual).is_err()
        || [
            &encoder.ffmpeg_sha256,
            &encoder.ffprobe_sha256,
            &execution.frame_receipts_sha256,
        ]
        .into_iter()
        .any(|h| !valid_hash(h))
        || execution.frames_rendered != plan.identity.frame_count
        || execution.rgba_bytes != expected_rgba.to_string()
        || encoder.observed_video_frames != plan.identity.frame_count
        || encoder.video_codec != "h264"
        || encoder.audio_codec != "aac"
        || encoder.pixel_format != "yuv420p"
        || encoder.crf != 18
        || encoder.audio_bitrate != 192000
        || !encoder.observed_video_duration_seconds.is_finite()
        || (encoder.observed_video_duration_seconds - video_seconds).abs() > 0.002
        || !encoder.observed_audio_duration_seconds.is_finite()
        || (encoder.observed_audio_duration_seconds - audio_seconds).abs() > 0.05
        || execution.audio.mastering_profile
            != crate::scene_audio_jobs::AudioMasteringProfile::ClassicExportWholeSceneWebAudioV31
        || descriptor.mime_type != "video/mp4"
        || artifact.mime_type != "video/mp4"
        || descriptor.size == 0
        || descriptor.size > input.processing_policy.max_output_bytes
        || artifact.size_bytes != descriptor.size.to_string()
        || artifact.content_hash != descriptor.content_hash
        || !valid_hash(&descriptor.content_hash)
    {
        return Err(stored_invalid());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
