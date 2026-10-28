//! One aggregate graphic picture on the existing authorized sound-consumer clock.
use crate::visual_preview::VisualFrameObservations;
use crate::{
    caption_preview_binding::PreparedCaptionBindings,
    classic_graphic_frame::{bind_graphic_frame, graphic_execution_policy, plan_frame},
};
use opencut_api_contracts::visual_preview::*;
use opencut_composition::scene::{NativeTextCoverage, PreparedGraphicFrame};
use opencut_contracts::{EditorError, Time};
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct GraphicPreviewBindings {
    pub plan: opencut_api_contracts::graphic_frame_plan::GraphicFramePlan,
    pub admission: VisualPreviewAdmission,
    pub decode_admissions: std::collections::BTreeMap<String, VisualDecodeAdmission>,
    pub bound_media: Option<opencut_contracts::visual_frame::BoundVisualFrame>,
    pub bound_execution: Option<opencut_contracts::text_frame::BoundTextFrame>,
    pub bound: Option<opencut_contracts::graphic_frame::BoundGraphicFrame>,
    pub time_seconds: f64,
    pub consumer_time_seconds: f64,
    pub max_lag_ms: u32,
}
pub struct PreparedGraphicBindings {
    prepared: PreparedGraphicFrame,
    original: PreparedCaptionBindings,
    policy: LocalVisualExecutionPolicy,
    coverage: Vec<NativeTextCoverage>,
}
impl PreparedGraphicBindings {
    pub fn new(
        prepared: PreparedGraphicFrame,
        policy: LocalVisualExecutionPolicy,
        coverage: Vec<NativeTextCoverage>,
    ) -> Result<Self, EditorError> {
        // Reserve generated sources before granting delegated decode admission.
        let execution_policy = graphic_execution_policy(&prepared, &policy)?;
        let original = PreparedCaptionBindings::new(
            prepared.execution().clone(),
            execution_policy,
            coverage.clone(),
        )?;
        Ok(Self {
            prepared,
            original,
            policy,
            coverage,
        })
    }
    pub fn time(&self) -> &Time {
        self.original.time()
    }
    pub fn result(
        &self,
        input: &VisualFrameObservations,
        consumer: &Time,
    ) -> Result<GraphicPreviewBindings, EditorError> {
        if input.graphics.is_some()
            && (input.decoded.is_none() || input.rasters.is_none() || input.captions.is_none())
        {
            return Err(EditorError::new(
                "INVALID_VISUAL_PLAN",
                "Graphic binding requires the original decoded, text and caption observations",
            ));
        }
        let child = self.original.result(
            &VisualFrameObservations {
                resolution: input.resolution,
                session: input.session.clone(),
                prepare_ahead: input.prepare_ahead,
                retained_picture_bytes: input.retained_picture_bytes,
                hit_query: None,
                time: input.time.clone(),
                decoded: input.decoded.clone(),
                rasters: input.rasters.clone(),
                captions: input.captions.clone(),
                graphics: None,
            },
            consumer,
        )?;
        let bound = match (&child.bound, &input.captions, &input.graphics) {
            (Some(execution), Some(captions), Some(graphics)) => Some(bind_graphic_frame(
                &self.prepared,
                execution,
                captions,
                graphics,
            )?),
            _ => None,
        };
        let policy = &self.policy;
        Ok(GraphicPreviewBindings {
            plan: plan_frame(&self.prepared, policy, &self.coverage)?,
            // The renderer owns one aggregate frame; only delegated decode gets
            // the remaining budget. Never claim a fabricated reduced GPU peak.
            admission: VisualPreviewAdmission {
                max_frame_pixels: policy.max_frame_pixels.to_string(),
                max_decoded_pixels: policy.max_decoded_pixels.to_string(),
                max_gpu_bytes: policy.max_gpu_bytes.to_string(),
                max_rgba_bytes: policy.max_rgba_bytes.to_string(),
                max_encoded_bytes: policy.max_encoded_bytes.to_string(),
            },
            decode_admissions: child.decode_admissions,
            bound_media: child.bound_media,
            bound_execution: child.bound,
            bound,
            time_seconds: child.time_seconds,
            consumer_time_seconds: child.consumer_time_seconds,
            max_lag_ms: child.max_lag_ms,
        })
    }
}
#[cfg(test)]
mod tests;
