//! Fixed sampled text/media facts on the existing sound-consumption clock.
use crate::visual_preview::VisualFrameObservations;
use crate::{
    classic_text_frame::{ClassicTextFrameSession, admit_text_frame},
    visual_preview::{VISUAL_PREVIEW_MAX_LAG_MS, validate_preparation_time},
};
use opencut_api_contracts::visual_preview::*;
use opencut_composition::scene::{NativeTextCoverage, PreparedTextFrame};
use opencut_contracts::{EditorError, Time};
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;
use opencut_time::RationalTime;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct TextPreviewBindings {
    pub plan: opencut_api_contracts::text_frame_plan::TextFramePlan,
    pub admission: VisualPreviewAdmission,
    pub decode_admissions: std::collections::BTreeMap<String, VisualDecodeAdmission>,
    pub bound_media: Option<opencut_contracts::visual_frame::BoundVisualFrame>,
    pub bound: Option<opencut_contracts::text_frame::BoundTextFrame>,
    pub time_seconds: f64,
    pub consumer_time_seconds: f64,
    pub max_lag_ms: u32,
}
pub struct PreparedTextBindings {
    prepared: PreparedTextFrame,
    policy: LocalVisualExecutionPolicy,
    coverage: Vec<NativeTextCoverage>,
}
impl PreparedTextBindings {
    pub fn new(
        prepared: PreparedTextFrame,
        policy: LocalVisualExecutionPolicy,
        coverage: Vec<NativeTextCoverage>,
    ) -> Result<Self, EditorError> {
        admit_text_frame(&prepared, &policy)?;
        ClassicTextFrameSession::new(&prepared, &policy, &coverage)?;
        Ok(Self {
            prepared,
            policy,
            coverage,
        })
    }
    pub fn time(&self) -> &Time {
        &self.prepared.identity().time
    }
    pub fn result(
        &self,
        input: &VisualFrameObservations,
        consumer: &Time,
    ) -> Result<TextPreviewBindings, EditorError> {
        let time = input.time.as_ref().unwrap_or(consumer);
        if time != self.time() || (input.rasters.is_some() && input.decoded.is_none()) {
            return Err(EditorError::new(
                "INVALID_VISUAL_PLAN",
                "Text preview binding requires the original frame and decoded sources",
            ));
        }
        validate_preparation_time(time, consumer, input.prepare_ahead)?;
        let session = ClassicTextFrameSession::new(&self.prepared, &self.policy, &self.coverage)?;
        let plan = session.typed_plan()?;
        let decode_admissions = plan
            .media
            .layers
            .iter()
            .map(|layer| {
                Ok((
                    layer.element_id.clone(),
                    session.decode_admission_for(&layer.element_id)?,
                ))
            })
            .collect::<Result<_, EditorError>>()?;
        let (bound_media, bound) = match input.decoded.as_ref() {
            Some(decoded) => {
                let (media, bound) =
                    session.project_preview_sources(decoded, input.rasters.as_deref())?;
                (Some(media), bound)
            }
            None => (None, None),
        };
        let seconds = |time: &Time| -> Result<f64, EditorError> {
            let time = RationalTime::try_from(time)?;
            Ok(time.ticks() as f64 / f64::from(time.timebase()))
        };
        let policy = &self.policy;
        Ok(TextPreviewBindings {
            plan,
            decode_admissions,
            bound_media,
            bound,
            admission: VisualPreviewAdmission {
                max_frame_pixels: policy.max_frame_pixels.to_string(),
                max_decoded_pixels: policy.max_decoded_pixels.to_string(),
                max_gpu_bytes: policy.max_gpu_bytes.to_string(),
                max_rgba_bytes: policy.max_rgba_bytes.to_string(),
                max_encoded_bytes: policy.max_encoded_bytes.to_string(),
            },
            time_seconds: seconds(time)?,
            consumer_time_seconds: seconds(consumer)?,
            max_lag_ms: VISUAL_PREVIEW_MAX_LAG_MS,
        })
    }
}

/// Validate the complete requested set against live host-owned coverage.
/// Session/owner/credential/lease checks remain in the existing host adapter.
pub fn require_font_batch<'a>(
    requested: &[String],
    participating: impl Iterator<Item = &'a str>,
) -> Result<(), EditorError> {
    if requested.is_empty() || requested.len() > 32 {
        return Err(EditorError::new(
            "INVALID_ARGUMENT",
            "Font authorization requires 1..32 distinct participating fonts",
        ));
    }
    let ids: std::collections::BTreeSet<_> = requested.iter().map(String::as_str).collect();
    if ids.len() != requested.len() {
        return Err(EditorError::new(
            "INVALID_ARGUMENT",
            "Font authorization requires distinct fonts",
        ));
    }
    let available: std::collections::BTreeSet<_> = participating.collect();
    if ids.iter().any(|id| !available.contains(id)) {
        return Err(EditorError::new(
            "NOT_AUTHORIZED",
            "A requested font is not in the current preview frame",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    fn batch_authorization_is_bounded_distinct_and_all_or_nothing() {
        let ids = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(require_font_batch(&ids(&["two", "one"]), ["one", "two"].into_iter()).is_ok());
        assert_eq!(
            require_font_batch(&ids(&["one", "missing"]), ["one"].into_iter())
                .unwrap_err()
                .code,
            "NOT_AUTHORIZED"
        );
        for invalid in [
            vec![],
            ids(&["one", "one"]),
            (0..33).map(|n| n.to_string()).collect(),
        ] {
            assert_eq!(
                require_font_batch(&invalid, ["one", "two"].into_iter())
                    .unwrap_err()
                    .code,
                "INVALID_ARGUMENT"
            );
        }
    }
}
