//! One bounded immutable caption picture on the existing sound consumer clock.
use crate::visual_preview::VisualFrameObservations;
use crate::{
    classic_caption_frame::{bind_caption_frame, plan_caption_frame},
    classic_text_frame::ClassicTextFrameSession,
    text_preview_binding::PreparedTextBindings,
};
use opencut_api_contracts::visual_preview::*;
use opencut_composition::scene::{NativeTextCoverage, PreparedCaptionFrame};
use opencut_contracts::{EditorError, Time};
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CaptionPreviewBindings {
    pub plan: opencut_api_contracts::caption_frame_plan::CaptionFramePlan,
    pub admission: VisualPreviewAdmission,
    pub decode_admissions: std::collections::BTreeMap<String, VisualDecodeAdmission>,
    pub bound_media: Option<opencut_contracts::visual_frame::BoundVisualFrame>,
    pub bound: Option<opencut_contracts::text_frame::BoundTextFrame>,
    pub time_seconds: f64,
    pub consumer_time_seconds: f64,
    pub max_lag_ms: u32,
}
pub struct PreparedCaptionBindings {
    prepared: PreparedCaptionFrame,
    policy: LocalVisualExecutionPolicy,
    coverage: Vec<NativeTextCoverage>,
}
impl PreparedCaptionBindings {
    pub fn new(
        prepared: PreparedCaptionFrame,
        policy: LocalVisualExecutionPolicy,
        coverage: Vec<NativeTextCoverage>,
    ) -> Result<Self, EditorError> {
        crate::classic_text_frame::admit_text_frame(prepared.execution(), &policy)?;
        ClassicTextFrameSession::new(prepared.execution(), &policy, &coverage)?;
        Ok(Self {
            prepared,
            policy,
            coverage,
        })
    }
    pub fn time(&self) -> &Time {
        &self.prepared.execution().identity().time
    }
    pub fn result(
        &self,
        input: &VisualFrameObservations,
        consumer: &Time,
    ) -> Result<CaptionPreviewBindings, EditorError> {
        if input.rasters.is_some() != input.captions.is_some()
            || (input.rasters.is_some() && input.decoded.is_none())
        {
            return Err(EditorError::new(
                "INVALID_VISUAL_PLAN",
                "Caption binding requires decoded media and both original caption and raster observations",
            ));
        }
        let original = PreparedTextBindings::new(
            self.prepared.execution().clone(),
            self.policy.clone(),
            self.coverage.clone(),
        )?;
        let media = original.result(
            &VisualFrameObservations {
                resolution: input.resolution,
                session: input.session.clone(),
                prepare_ahead: input.prepare_ahead,
                retained_picture_bytes: input.retained_picture_bytes,
                hit_query: None,
                time: input.time.clone(),
                decoded: input.decoded.clone(),
                rasters: None,
                captions: None,
                graphics: None,
            },
            consumer,
        )?;
        let bound = match (&input.decoded, &input.rasters, &input.captions) {
            (Some(decoded), Some(rasters), Some(captions)) => Some(bind_caption_frame(
                &self.prepared,
                &self.policy,
                &self.coverage,
                decoded,
                rasters,
                captions,
            )?),
            _ => None,
        };
        Ok(CaptionPreviewBindings {
            plan: plan_caption_frame(&self.prepared, &self.policy, &self.coverage)?,
            admission: media.admission,
            decode_admissions: media.decode_admissions,
            bound_media: media.bound_media,
            bound,
            time_seconds: media.time_seconds,
            consumer_time_seconds: media.consumer_time_seconds,
            max_lag_ms: media.max_lag_ms,
        })
    }
}
#[cfg(test)]
mod tests;
