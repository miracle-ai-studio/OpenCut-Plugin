//! A live preview owns the same process/session implementation as export.
//! Its host supplies current-generation authority on every frame and drops
//! this owner on generation change, shutdown, or bounded idle retirement.
use super::{process::NativeProcess, *};
use crate::classic_visual::{ClassicVisualProcessConfig, validate_quota};
use opencut_editor_application::{
    classic_scene_frame::{ClassicSceneFrameReceipt, ClassicSceneFrameSession},
    classic_text_frame::{ClassicTextFontHandle, ClassicTextFrameResources},
    classic_visual::{
        ClassicVisualResourceHandle, ClassicVisualRgbaSink, ClassicVisualRuntimeReceipt,
        VisualQuotaReceipt,
    },
};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct NativePreviewFrame {
    pub source_preparation: Vec<opencut_contracts::source_preview::SourcePreviewUse>,
    pub receipt: ClassicSceneFrameReceipt,
    pub hits: Vec<NativeVisualHit>,
    pub runtime: ClassicVisualRuntimeReceipt,
    pub quota: VisualQuotaReceipt,
}
pub struct NativeScenePreview {
    process: NativeProcess,
    config: ClassicVisualProcessConfig,
    activity: Arc<Mutex<Instant>>,
    idle_timeout: Duration,
}
impl NativeScenePreview {
    pub fn new(
        config: ClassicVisualProcessConfig,
        idle_timeout: Duration,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        if idle_timeout.is_zero() || idle_timeout > Duration::from_secs(30) {
            return Err(invalid("Invalid native preview idle admission"));
        }
        let activity = Arc::new(Mutex::new(Instant::now()));
        // The monitor enforces idle expiry independently of blocked native
        // code. A frame also retains its captured per-frame deadline.
        let process = NativeProcess::start(
            config.clone(),
            idle_timeout.max(config.deadline),
            Some(activity.clone()),
            checkpoint,
        )?;
        Ok(Self {
            process,
            config,
            activity,
            idle_timeout,
        })
    }
    pub fn idle_expired(&self) -> bool {
        self.activity
            .lock()
            .map_or(true, |at| at.elapsed() >= self.idle_timeout)
    }
    pub fn check(&self) -> Result<(), EditorError> {
        self.process.check()
    }
    pub fn render(
        &mut self,
        session: &mut ClassicSceneFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
        probe: Option<[u32; 2]>,
    ) -> Result<NativePreviewFrame, EditorError> {
        let result = (|| {
            *self
                .activity
                .lock()
                .map_err(|_| invalid("Native preview clock is unavailable"))? = Instant::now();
            let frame = self.process.render_with_probe(
                session,
                resources,
                "beginSceneFrame",
                handles,
                fonts,
                sink,
                probe,
                SourceRaster::Preview,
            )?;
            session.validate_receipt(&frame.receipt)?;
            let bound = &frame.receipt.execution.execution.execution.execution.bound;
            let mut seen = BTreeSet::new();
            for hit in &frame.hits {
                if hit.alpha == 0
                    || !seen.insert(&hit.element_id)
                    || !bound
                        .layers
                        .iter()
                        .any(|layer| layer.element_id == hit.element_id && layer.quad.is_some())
                {
                    return Err(invalid("Native preview hit changed a bound layer"));
                }
            }
            resources.checkpoint()?;
            let quota = self.process.quota();
            validate_quota(&quota, &self.config, None)?;
            *self
                .activity
                .lock()
                .map_err(|_| invalid("Native preview clock is unavailable"))? = Instant::now();
            Ok(NativePreviewFrame {
                source_preparation: frame.source_preparation,
                receipt: frame.receipt,
                hits: frame.hits,
                runtime: self.process.runtime().clone(),
                quota,
            })
        })();
        if result.is_err() {
            self.process.retire();
        }
        result
    }
}
