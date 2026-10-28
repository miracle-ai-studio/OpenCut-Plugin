use crate::classic_visual::{ClassicVisualProcessConfig, VisualProcessSession, validate_quota};
use crate::native_visual::NativeProcess;
use opencut_contracts::EditorError;
use opencut_editor_application::media_video_jobs::{
    MAX_VIDEO_DEADLINE_MS, MAX_VIDEO_FRAMES, VideoExecutionLimit,
};
use opencut_editor_application::{
    classic_blend_frame::*, classic_caption_frame::*, classic_graphic_frame::*,
    classic_mask_frame::*, classic_scene_frame::*, classic_text_frame::*, classic_visual::*,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
trait VideoReceiptValidation<R> {
    fn validate_video_receipt(&self, receipt: &R) -> Result<(), EditorError>;
}
impl VideoReceiptValidation<ClassicTextFrameReceipt> for ClassicTextFrameSession<'_> {
    fn validate_video_receipt(&self, receipt: &ClassicTextFrameReceipt) -> Result<(), EditorError> {
        self.validate_receipt(receipt)
    }
}
impl VideoReceiptValidation<ClassicCaptionFrameReceipt> for ClassicCaptionFrameSession<'_> {
    fn validate_video_receipt(
        &self,
        receipt: &ClassicCaptionFrameReceipt,
    ) -> Result<(), EditorError> {
        self.validate_receipt(receipt)
    }
}
impl VideoReceiptValidation<ClassicGraphicFrameReceipt> for ClassicGraphicFrameSession<'_> {
    fn validate_video_receipt(
        &self,
        receipt: &ClassicGraphicFrameReceipt,
    ) -> Result<(), EditorError> {
        self.validate_receipt(receipt)
    }
}
impl VideoReceiptValidation<ClassicMaskFrameReceipt> for ClassicMaskFrameSession<'_> {
    fn validate_video_receipt(&self, receipt: &ClassicMaskFrameReceipt) -> Result<(), EditorError> {
        self.validate_receipt(receipt)
    }
}
impl VideoReceiptValidation<ClassicSceneFrameReceipt> for ClassicSceneFrameSession<'_> {
    fn validate_video_receipt(
        &self,
        receipt: &ClassicSceneFrameReceipt,
    ) -> Result<(), EditorError> {
        self.validate_receipt(receipt)
    }
}
impl VideoReceiptValidation<ClassicBlendFrameReceipt> for ClassicBlendFrameSession<'_> {
    fn validate_video_receipt(
        &self,
        receipt: &ClassicBlendFrameReceipt,
    ) -> Result<(), EditorError> {
        self.validate_receipt(receipt)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Text,
    Caption,
    Graphic,
    Blend,
    Mask,
    Scene,
}
impl FrameKind {
    fn begin(self) -> &'static str {
        match self {
            Self::Text => "beginTextFrame",
            Self::Caption => "beginCaptionFrame",
            Self::Graphic => "beginGraphicFrame",
            Self::Blend => "beginBlendFrame",
            Self::Mask => "beginMaskFrame",
            Self::Scene => "beginSceneFrame",
        }
    }
}
fn err(message: &str) -> EditorError {
    EditorError::new("VIDEO_PROTOCOL_ERROR", message)
}
/// One native decoder/GPU owner for a complete admitted frame sequence.
pub struct ClassicVideoProcess {
    process: NativeProcess,
    config: ClassicVisualProcessConfig,
    kind: FrameKind,
    count: u64,
    next: u64,
    limit: VideoExecutionLimit,
}
impl ClassicVideoProcess {
    pub fn start(
        config: ClassicVisualProcessConfig,
        count: u64,
        limit: VideoExecutionLimit,
        clock: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Self::start_kind(config, count, limit, clock, checkpoint, FrameKind::Text)
    }
    pub fn start_caption(
        config: ClassicVisualProcessConfig,
        count: u64,
        limit: VideoExecutionLimit,
        clock: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Self::start_kind(config, count, limit, clock, checkpoint, FrameKind::Caption)
    }
    pub fn start_graphic(
        config: ClassicVisualProcessConfig,
        count: u64,
        limit: VideoExecutionLimit,
        clock: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Self::start_kind(config, count, limit, clock, checkpoint, FrameKind::Graphic)
    }
    pub fn start_blend(
        config: ClassicVisualProcessConfig,
        count: u64,
        limit: VideoExecutionLimit,
        clock: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Self::start_kind(config, count, limit, clock, checkpoint, FrameKind::Blend)
    }
    pub fn start_mask(
        config: ClassicVisualProcessConfig,
        count: u64,
        limit: VideoExecutionLimit,
        clock: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Self::start_kind(config, count, limit, clock, checkpoint, FrameKind::Mask)
    }
    pub fn start_scene(
        config: ClassicVisualProcessConfig,
        count: u64,
        limit: VideoExecutionLimit,
        clock: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Self::start_kind(config, count, limit, clock, checkpoint, FrameKind::Scene)
    }
    fn start_kind(
        config: ClassicVisualProcessConfig,
        count: u64,
        limit: VideoExecutionLimit,
        clock: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
        kind: FrameKind,
    ) -> Result<Self, EditorError> {
        if count == 0
            || count > MAX_VIDEO_FRAMES
            || limit.milliseconds() == 0
            || limit.milliseconds() > MAX_VIDEO_DEADLINE_MS
            || limit.renews_on_progress() != clock.is_some()
        {
            return Err(err("Invalid persistent native admission"));
        }
        #[cfg(unix)]
        {
            let process = NativeProcess::start(
                config.clone(),
                Duration::from_millis(limit.milliseconds()),
                clock,
                checkpoint,
            )?;
            Ok(Self {
                process,
                config,
                kind,
                count,
                next: 0,
                limit,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (config, count, limit, clock, checkpoint, kind);
            Err(err("Native process lifecycle unavailable"))
        }
    }
    pub fn runtime(&self) -> Result<&ClassicVisualRuntimeReceipt, EditorError> {
        Ok(self.process.runtime())
    }
    pub fn render_frame(
        &mut self,
        session: &mut ClassicTextFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<ClassicTextFrameReceipt, EditorError> {
        if self.kind != FrameKind::Text {
            return Err(err("Frame differs from the selected composition format"));
        }
        self.render_session(session, resources, handles, fonts, sink)
    }
    pub fn render_caption_frame(
        &mut self,
        session: &mut ClassicCaptionFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<ClassicCaptionFrameReceipt, EditorError> {
        if self.kind != FrameKind::Caption {
            return Err(err("Frame differs from the selected composition format"));
        }
        self.render_session(session, resources, handles, fonts, sink)
    }
    pub fn render_graphic_frame(
        &mut self,
        session: &mut ClassicGraphicFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<ClassicGraphicFrameReceipt, EditorError> {
        if self.kind != FrameKind::Graphic {
            return Err(err("Frame differs from the selected composition format"));
        }
        self.render_session(session, resources, handles, fonts, sink)
    }
    pub fn render_blend_frame(
        &mut self,
        session: &mut ClassicBlendFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<ClassicBlendFrameReceipt, EditorError> {
        if self.kind != FrameKind::Blend {
            return Err(err("Frame differs from the selected composition format"));
        }
        self.render_session(session, resources, handles, fonts, sink)
    }
    pub fn render_mask_frame(
        &mut self,
        session: &mut ClassicMaskFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<ClassicMaskFrameReceipt, EditorError> {
        if self.kind != FrameKind::Mask {
            return Err(err("Frame differs from the selected composition format"));
        }
        self.render_session(session, resources, handles, fonts, sink)
    }
    pub fn render_scene_frame(
        &mut self,
        session: &mut ClassicSceneFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<ClassicSceneFrameReceipt, EditorError> {
        if self.kind != FrameKind::Scene {
            return Err(err("Frame differs from the selected composition format"));
        }
        self.render_session(session, resources, handles, fonts, sink)
    }
    fn render_session<S, R>(
        &mut self,
        session: &mut S,
        resources: &mut R,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<S::Receipt, EditorError>
    where
        R: ClassicTextFrameResources + ?Sized,
        S: VisualProcessSession<R>
            + VideoReceiptValidation<<S as VisualProcessSession<R>>::Receipt>,
    {
        if self.next >= self.count {
            return Err(err("Native video exceeded its frame admission"));
        }
        let receipt =
            self.process
                .render(session, resources, self.kind.begin(), handles, fonts, sink)?;
        session.validate_video_receipt(&receipt)?;
        resources.checkpoint()?;
        self.next += 1;
        Ok(receipt)
    }
    pub fn finish(
        self,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<(ClassicVisualRuntimeReceipt, VisualQuotaReceipt), EditorError> {
        if self.next != self.count {
            return Err(err("Native video omitted admitted frames"));
        }
        let result = self.process.finish(checkpoint)?;
        validate_quota(
            &result.1,
            &self.config,
            if self.limit.renews_on_progress() {
                None
            } else {
                Some(Duration::from_millis(self.limit.milliseconds()))
            },
        )?;
        Ok(result)
    }
}
