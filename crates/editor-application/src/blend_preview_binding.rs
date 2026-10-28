//! Blend pictures reuse the D0 consumer-clock checks and all resource admission.
use crate::visual_preview::VisualFrameObservations;
use crate::{
    classic_blend_frame::{bind_blend_frame, plan_frame},
    graphic_preview_binding::PreparedGraphicBindings,
};
use opencut_api_contracts::visual_preview::*;
use opencut_composition::scene::{NativeTextCoverage, PreparedBlendFrame};
use opencut_contracts::{EditorError, Time};
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BlendPreviewBindings {
    pub plan: opencut_api_contracts::blend_frame_plan::BlendFramePlan,
    pub admission: VisualPreviewAdmission,
    pub decode_admissions: std::collections::BTreeMap<String, VisualDecodeAdmission>,
    pub bound_media: Option<opencut_contracts::visual_frame::BoundVisualFrame>,
    pub bound_execution: Option<opencut_contracts::text_frame::BoundTextFrame>,
    pub bound_graphic: Option<opencut_contracts::graphic_frame::BoundGraphicFrame>,
    pub bound: Option<opencut_contracts::blend_frame::BoundBlendFrame>,
    pub time_seconds: f64,
    pub consumer_time_seconds: f64,
    pub max_lag_ms: u32,
}
pub struct PreparedBlendBindings {
    prepared: PreparedBlendFrame,
    original: PreparedGraphicBindings,
    policy: LocalVisualExecutionPolicy,
    coverage: Vec<NativeTextCoverage>,
}
impl PreparedBlendBindings {
    pub fn new(
        prepared: PreparedBlendFrame,
        policy: LocalVisualExecutionPolicy,
        coverage: Vec<NativeTextCoverage>,
    ) -> Result<Self, EditorError> {
        let original = PreparedGraphicBindings::new(
            prepared.execution().clone(),
            policy.clone(),
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
    ) -> Result<BlendPreviewBindings, EditorError> {
        let child = self.original.result(input, consumer)?;
        let bound = child
            .bound
            .as_ref()
            .map(|graphic| bind_blend_frame(&self.prepared, graphic))
            .transpose()?;
        Ok(BlendPreviewBindings {
            plan: plan_frame(&self.prepared, &self.policy, &self.coverage)?,
            admission: child.admission,
            decode_admissions: child.decode_admissions,
            bound_media: child.bound_media,
            bound_execution: child.bound_execution,
            bound_graphic: child.bound,
            bound,
            time_seconds: child.time_seconds,
            consumer_time_seconds: child.consumer_time_seconds,
            max_lag_ms: child.max_lag_ms,
        })
    }
}
#[cfg(test)]
mod tests;
