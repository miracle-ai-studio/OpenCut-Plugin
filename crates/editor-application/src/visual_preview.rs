//! Scene pictures reuse the D0 consumer-clock checks and all resource admission.
use crate::{
    blend_preview_binding::PreparedBlendBindings,
    classic_scene_frame::{bind_scene_frame, plan_frame},
};
use opencut_api_contracts::visual_preview::*;
use opencut_composition::scene::{NativeTextCoverage, PreparedSceneFrame};
use opencut_contracts::{EditorError, Time};
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;

/// Internal observations obtained from the native application session. This
/// is not accepted from a public preview caller.
#[derive(Clone)]
pub struct VisualFrameObservations {
    pub resolution: opencut_contracts::export_video::RasterResolution,
    pub session: opencut_api_contracts::audio_preview::AudioPreviewStream,
    pub prepare_ahead: bool,
    pub retained_picture_bytes: u32,
    pub hit_query: Option<VisualPreviewHitPoint>,
    pub time: Option<Time>,
    pub decoded: Option<Vec<opencut_contracts::visual_frame::DecodedVisualSource>>,
    pub rasters: Option<Vec<opencut_contracts::text_frame::TextRasterReceipt>>,
    pub captions: Option<Vec<opencut_contracts::caption_frame::CaptionLayoutReceipt>>,
    pub graphics: Option<Vec<opencut_contracts::graphic_frame::GraphicRasterReceipt>>,
}

/// Pure binding facts retained independently of the native binary payload.
#[derive(Debug, serde::Serialize)]
pub struct PictureBindings {
    pub prepare_ahead: bool,
    pub next_time: Time,
    pub plan: opencut_api_contracts::scene_frame_plan::SceneFramePlan,
    pub admission: VisualPreviewAdmission,
    pub decode_admissions: std::collections::BTreeMap<String, VisualDecodeAdmission>,
    pub bound_media: Option<opencut_contracts::visual_frame::BoundVisualFrame>,
    pub bound_execution: Option<opencut_contracts::text_frame::BoundTextFrame>,
    pub bound_graphic: Option<opencut_contracts::graphic_frame::BoundGraphicFrame>,
    pub bound_blend: Option<opencut_contracts::blend_frame::BoundBlendFrame>,
    pub bound: Option<opencut_contracts::scene_frame::BoundSceneFrame>,
    pub time_seconds: f64,
    pub consumer_time_seconds: f64,
    pub max_lag_ms: u32,
}

pub struct PreparedPicture {
    prepared: PreparedSceneFrame,
    original: PreparedBlendBindings,
    policy: LocalVisualExecutionPolicy,
    coverage: Vec<NativeTextCoverage>,
}
impl PreparedPicture {
    pub fn frame(&self) -> &PreparedSceneFrame {
        &self.prepared
    }
    pub fn policy(&self) -> &LocalVisualExecutionPolicy {
        &self.policy
    }
    pub fn session(
        &self,
    ) -> Result<crate::classic_scene_frame::ClassicSceneFrameSession<'_>, EditorError> {
        crate::classic_scene_frame::ClassicSceneFrameSession::new(
            &self.prepared,
            &self.policy,
            &self.coverage,
        )
    }
    pub fn rendered_result(
        &self,
        input: &VisualPreviewFrame,
        consumer: &Time,
        receipt: &crate::classic_scene_frame::ClassicSceneFrameReceipt,
        picture: VisualPreviewPicture,
        hits: Vec<VisualPreviewHit>,
    ) -> Result<VisualPreviewFrameResult, EditorError> {
        let text = &receipt.execution.execution.execution.execution.bound;
        let observed = VisualFrameObservations {
            resolution: input.resolution,
            session: input.session.clone(),
            prepare_ahead: input.prepare_ahead,
            retained_picture_bytes: input.retained_picture_bytes,
            hit_query: input.hit_query,
            time: input.time.clone(),
            decoded: Some(text.decoded.clone()),
            rasters: Some(text.rasters.clone()),
            captions: Some(receipt.execution.execution.execution.captions.clone()),
            graphics: Some(receipt.execution.execution.graphics.clone()),
        };
        let bound = self.result(&observed, consumer)?;
        let identity = &bound.plan.identity;
        let VisualPreviewDigest::Blake3 { value: digest } = &picture.content_digest;
        if receipt.identity != *identity
            || picture.width != identity.width
            || picture.height != identity.height
            || u64::from(picture.byte_length)
                != u64::from(identity.width) * u64::from(identity.height) * 4
            || !crate::media_frame_jobs::valid_hash(digest)
            || bound.bound.is_none()
        {
            return Err(EditorError::new(
                "INVALID_VISUAL_PLAN",
                "Native picture differs from its completed scene binding",
            ));
        }
        Ok(VisualPreviewFrameResult {
            prepare_ahead: bound.prepare_ahead,
            next_time: bound.next_time,
            hit_test: input.hit_query.map(|point| VisualPreviewHitTest {
                identity: identity.clone(),
                point,
                hits,
            }),
            plan: bound.plan,
            admission: bound.admission,
            decode_admissions: bound.decode_admissions,
            bound_media: bound.bound_media,
            bound_execution: bound.bound_execution,
            bound_graphic: bound.bound_graphic,
            bound_blend: bound.bound_blend,
            bound: bound.bound,
            time_seconds: bound.time_seconds,
            consumer_time_seconds: bound.consumer_time_seconds,
            max_lag_ms: bound.max_lag_ms,
            picture,
        })
    }

    pub fn font_version(&self, id: &str) -> Option<opencut_contracts::AssetVersion> {
        self.coverage
            .iter()
            .find(|proof| proof.font.id == id)
            .map(|proof| proof.font.content.clone())
    }
    pub fn new(
        prepared: PreparedSceneFrame,
        policy: LocalVisualExecutionPolicy,
        coverage: Vec<NativeTextCoverage>,
    ) -> Result<Self, EditorError> {
        let original = PreparedBlendBindings::new(
            prepared.blend().clone(),
            crate::classic_scene_frame::scene_execution_policy(&prepared, &policy)?,
            crate::classic_scene_frame::title_coverage(&prepared, &coverage)?,
        )?;
        Ok(Self {
            prepared,
            original,
            policy,
            coverage,
        })
    }
    pub fn media_version(
        &self,
        resource: &VisualResource,
    ) -> Result<opencut_contracts::AssetVersion, EditorError> {
        let media = self.prepared.execution().execution().execution().media();
        let session = crate::classic_visual::ClassicVisualSession::new(media, &self.policy)?;
        if session.resource(&resource.asset_id)? != *resource {
            return Err(EditorError::new(
                "NOT_AUTHORIZED",
                "Media differs from the fixed picture",
            ));
        }
        media
            .resources()
            .iter()
            .find(|r| r.asset_id == resource.asset_id)
            .map(|r| r.version.clone())
            .ok_or_else(|| EditorError::new("NOT_AUTHORIZED", "Media is outside the fixed picture"))
    }
    pub fn time(&self) -> &Time {
        self.original.time()
    }
    pub fn result(
        &self,
        input: &VisualFrameObservations,
        consumer: &Time,
    ) -> Result<PictureBindings, EditorError> {
        let child = self.original.result(input, consumer)?;
        let bound = child
            .bound
            .as_ref()
            .map(|graphic| bind_scene_frame(&self.prepared, graphic))
            .transpose()?;
        let fps = &self.prepared.source_graph().format.fps;
        let frame_step = RationalTime::new(i64::from(fps.denominator), fps.numerator)?;
        // Low-FPS media can still contain continuously animated composition.
        // Sample at least every 100 ms so a held source frame cannot deadlock
        // the separate presentation/PCM credit fence.
        let step = if frame_step.cmp_exact(RationalTime::new(1, 10)?).is_gt() {
            RationalTime::new(1, 10)?
        } else {
            frame_step
        };
        Ok(PictureBindings {
            prepare_ahead: input.prepare_ahead,
            next_time: RationalTime::try_from(self.time())?
                .checked_add(step)?
                .into(),
            plan: plan_frame(&self.prepared, &self.policy, &self.coverage)?,
            admission: opencut_api_contracts::visual_preview::VisualPreviewAdmission {
                max_frame_pixels: self.policy.max_frame_pixels.to_string(),
                max_decoded_pixels: self.policy.max_decoded_pixels.to_string(),
                max_gpu_bytes: self.policy.max_gpu_bytes.to_string(),
                max_rgba_bytes: self.policy.max_rgba_bytes.to_string(),
                max_encoded_bytes: self.policy.max_encoded_bytes.to_string(),
            },
            decode_admissions: child.decode_admissions,
            bound_media: child.bound_media,
            bound_execution: child.bound_execution,
            bound_graphic: child.bound_graphic,
            bound_blend: child.bound,
            bound,
            time_seconds: child.time_seconds,
            consumer_time_seconds: child.consumer_time_seconds,
            max_lag_ms: child.max_lag_ms,
        })
    }
}

use opencut_time::RationalTime;
/// Reserve resident output bitmaps before deriving decoder caps. Subtracting
/// only in the browser would leave native decode permits larger than the budget.
pub fn reserve_picture_memory(
    mut policy: LocalVisualExecutionPolicy,
    bytes: u32,
) -> Result<LocalVisualExecutionPolicy, EditorError> {
    let denied = || {
        EditorError::new(
            "VISUAL_BUDGET_EXCEEDED",
            "Buffered pictures exceed preview admission",
        )
    };
    if bytes > 64 * 1024 * 1024 {
        return Err(denied());
    }
    policy.max_rgba_bytes = policy
        .max_rgba_bytes
        .checked_sub(u64::from(bytes))
        .ok_or_else(denied)?;
    policy.max_gpu_bytes = policy
        .max_gpu_bytes
        .checked_sub(u64::from(bytes))
        .ok_or_else(denied)?;
    Ok(policy)
}
/// Reserve fixed parent/client presentation envelopes for one native owner.
/// Reported queued bytes are validated against this envelope; they never alter
/// decoder admission from frame to frame or force decoder reconstruction.
pub fn reserve_native_picture_memory(
    mut policy: LocalVisualExecutionPolicy,
    retained: u32,
) -> Result<(LocalVisualExecutionPolicy, u32), EditorError> {
    // A long-GOP HD cut can require over one second of preroll. Reserve the
    // parent and client queues up front, within the unchanged owner policy.
    let capacity = (128 * 1024 * 1024u64)
        .min(policy.max_rgba_bytes / 4)
        .min(policy.max_gpu_bytes / 4);
    if capacity < 65536 || u64::from(retained) > capacity {
        return Err(EditorError::new(
            "VISUAL_BUDGET_EXCEEDED",
            "Native preview pictures exceed their reserved envelope",
        ));
    }
    policy.max_rgba_bytes -= capacity * 2;
    policy.max_gpu_bytes -= capacity;
    Ok((policy, capacity as u32))
}
pub const VISUAL_PREVIEW_MAX_LAG_MS: u32 = 150;
// One bounded speculative cut decoder may prepare before a cold source opens.
// This does not extend the presentation window or the retained-picture budget.
pub const VISUAL_PREVIEW_MAX_PREPARE_MS: u32 = 3000;
pub fn validate_preparation_time(
    time: &Time,
    consumer: &Time,
    prepare: bool,
) -> Result<(), EditorError> {
    if !prepare {
        return validate_picture_time(time, consumer);
    }
    let t = RationalTime::try_from(time)?;
    let c = RationalTime::try_from(consumer)?;
    let delta = i128::from(t.ticks()) * i128::from(c.timebase())
        - i128::from(c.ticks()) * i128::from(t.timebase());
    let scale = i128::from(c.timebase()) * i128::from(t.timebase());
    if t.ticks() < 0
        || delta * 1000 > i128::from(VISUAL_PREVIEW_MAX_PREPARE_MS) * scale
        || delta * 1000 < -i128::from(VISUAL_PREVIEW_MAX_LAG_MS) * scale
    {
        return Err(EditorError::new(
            "PREVIEW_FRAME_STALE",
            "Preparation is outside the bounded consumer window",
        ));
    }
    Ok(())
}
pub fn validate_picture_time(time: &Time, consumer: &Time) -> Result<(), EditorError> {
    let t = RationalTime::try_from(time)?;
    let c = RationalTime::try_from(consumer)?;
    // Exact cross multiplication, bounded i64 ticks × u32 bases fits i128.
    let delta = i128::from(c.ticks()) * i128::from(t.timebase())
        - i128::from(t.ticks()) * i128::from(c.timebase());
    if t.ticks() < 0
        || delta < 0
        || delta * 1000
            > i128::from(VISUAL_PREVIEW_MAX_LAG_MS)
                * i128::from(c.timebase())
                * i128::from(t.timebase())
    {
        return Err(EditorError::new(
            "PREVIEW_FRAME_STALE",
            "Picture must be at or within 150 ms behind confirmed audio consumption",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t(ticks: &str, timebase: u32) -> Time {
        Time {
            ticks: ticks.into(),
            timebase,
        }
    }
    #[test]
    fn exact_consumer_window_rejects_future_and_overdue_including_fractional_time() {
        let c = t("1", 3);
        assert!(validate_picture_time(&t("11", 60), &c).is_ok()); // exactly 150ms
        assert!(validate_picture_time(&t("10999", 60000), &c).is_err());
        assert!(validate_picture_time(&t("20001", 60000), &c).is_err());
        assert!(validate_picture_time(&t("1", 3), &c).is_ok());
        assert!(validate_picture_time(&t("-1", 3), &c).is_err());
    }
    #[test]
    fn preparation_has_a_bounded_future_window_without_relaxing_presentation() {
        let c = t("1", 3);
        assert!(validate_preparation_time(&t("10", 3), &c, true).is_ok());
        assert!(validate_preparation_time(&t("200001", 60000), &c, true).is_err());
        assert!(validate_preparation_time(&t("4", 3), &c, false).is_err());
        assert!(validate_preparation_time(&t("10999", 60000), &c, true).is_err());
        assert!(validate_preparation_time(&t("-1", 3), &c, true).is_err());
    }
    #[test]
    fn native_presentation_envelope_is_stable_as_the_consumer_queue_changes() {
        let mut original = crate::classic_text_frame::tests::policy();
        original.max_gpu_bytes = 64 * 1024 * 1024;
        original.max_rgba_bytes = 128 * 1024 * 1024;
        let (cold, capacity) = reserve_native_picture_memory(original.clone(), 0).unwrap();
        let (warm, same_capacity) =
            reserve_native_picture_memory(original.clone(), capacity).unwrap();
        assert_eq!(
            cold, warm,
            "a growing bitmap queue must not rebuild native decoders"
        );
        assert_eq!(capacity, same_capacity);
        assert_eq!(
            cold.max_rgba_bytes + 2 * u64::from(capacity),
            original.max_rgba_bytes
        );
        assert_eq!(
            cold.max_gpu_bytes + u64::from(capacity),
            original.max_gpu_bytes
        );
        assert!(reserve_native_picture_memory(original, capacity + 1).is_err());
    }
    #[test]
    fn native_cut_reserve_keeps_both_queues_inside_the_owner_policy() {
        for (gpu_mib, rgba_mib, expected_mib) in [(64, 128, 16), (256, 256, 64), (512, 512, 128)] {
            let mut original = crate::classic_text_frame::tests::policy();
            original.max_gpu_bytes = gpu_mib * 1024 * 1024;
            original.max_rgba_bytes = rgba_mib * 1024 * 1024;
            let (cold, capacity) = reserve_native_picture_memory(original.clone(), 0).unwrap();
            assert_eq!(u64::from(capacity), expected_mib * 1024 * 1024);
            let (full, _) = reserve_native_picture_memory(original.clone(), capacity).unwrap();
            assert_eq!(cold, full);
            assert_eq!(
                cold.max_rgba_bytes + 2 * u64::from(capacity),
                original.max_rgba_bytes
            );
            assert_eq!(
                cold.max_gpu_bytes + u64::from(capacity),
                original.max_gpu_bytes
            );
            assert!(reserve_native_picture_memory(original, capacity + 1).is_err());
        }
    }
}

pub use crate::text_preview_binding::require_font_batch;
