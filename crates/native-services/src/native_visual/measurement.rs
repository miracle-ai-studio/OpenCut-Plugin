//! CPU-only font metrics in the same bounded, authorized worker protocol.
use super::{plan::Plan, process::NativeProcess, text, *};
use crate::classic_visual::{ClassicVisualProcessConfig, VisualProcessSession, validate_quota};
use opencut_contracts::{
    caption_frame::{CaptionLayoutReceipt, CaptionRasterPlan},
    font_synthesis::FontSynthesisBinding,
    renderer_identity::NativeTextRuntimeIdentity,
    subtitle_classic_ass::SubtitleClassicPlacement,
    text_frame::TextRasterReceipt,
};
use opencut_editor_application::{classic_text_frame::*, classic_visual::ClassicVisualResources};
use opencut_renderer::raster::TextRasterizer;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeTextMeasureOptions {
    Plain {
        synthesis: Option<FontSynthesisBinding>,
    },
    Caption {
        directive: CaptionRasterPlan,
        placement: Option<SubtitleClassicPlacement>,
        synthesis: Option<FontSynthesisBinding>,
    },
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeTextMeasurement {
    pub raster: TextRasterReceipt,
    pub caption: Option<CaptionLayoutReceipt>,
}
pub struct NativeTextMeasurements {
    process: NativeProcess,
    config: ClassicVisualProcessConfig,
}
impl NativeTextMeasurements {
    pub fn new(
        config: ClassicVisualProcessConfig,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Ok(Self {
            process: NativeProcess::start_text(config.clone(), checkpoint)?,
            config,
        })
    }
    pub fn measure(
        &mut self,
        session: &mut ClassicTextFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        fonts: &[ClassicTextFontHandle],
        options: &NativeTextMeasureOptions,
    ) -> Result<NativeTextMeasurement, EditorError> {
        let result = self.process.measure(session, resources, fonts, options)?;
        let plan = session.typed_plan()?;
        if plan.texts.len() != 1
            || result.raster.element_id != plan.texts[0].element_id
            || result.raster.width != plan.identity.width
            || result.raster.height != plan.identity.height
            || result.raster.layout.font_content_hash != plan.texts[0].font.content_hash
        {
            self.process.retire();
            return Err(invalid("Native text metrics changed their fixed target"));
        }
        resources.checkpoint()?;
        Ok(result)
    }
    pub fn finish(
        self,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<NativeTextRuntimeIdentity, EditorError> {
        let (identity, quota) = self.process.finish_text(checkpoint)?;
        opencut_editor_application::classic_visual::validate_text_identity(&identity)?;
        validate_quota(&quota, &self.config, Some(self.config.deadline))?;
        Ok(identity)
    }
}

pub(super) fn measure<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
    session: &mut S,
    resources: &mut R,
    fonts: &[ClassicTextFontHandle],
    options: NativeTextMeasureOptions,
    rasterizer: &mut TextRasterizer,
) -> Result<NativeTextMeasurement, EditorError> {
    let plan = Plan::parse("beginTextFrame", session.plan()?)?;
    let text_plan = plan.text().expect("typed text measurement plan");
    if text_plan.texts.len() != 1
        || !plan.media().layers.is_empty()
        || !plan.media().resources.is_empty()
    {
        return Err(invalid(
            "Native measurement requires one fixed text target and no media",
        ));
    }
    text::authorize_fonts(session, resources, &plan, fonts, rasterizer)?;
    let mut layer = text_plan.texts[0].clone();
    let (caption, synthesis) = match options {
        NativeTextMeasureOptions::Plain { synthesis } => (None, synthesis),
        NativeTextMeasureOptions::Caption {
            directive,
            placement,
            synthesis,
        } => {
            if directive.element_id != layer.element_id {
                return Err(invalid("Native caption directive changed its target"));
            }
            let receipt = text::prepare_caption(
                &mut layer,
                &directive,
                text_plan.identity.width,
                text_plan.identity.height,
                rasterizer,
                placement.as_ref(),
            )?;
            (Some(receipt), synthesis)
        }
    };
    resources.checkpoint()?;
    let layout = rasterizer
        .measure_layout(&layer, synthesis.as_ref(), session.policy().max_rgba_bytes)
        .map_err(text::raster_error)?;
    resources.checkpoint()?;
    Ok(NativeTextMeasurement {
        raster: TextRasterReceipt {
            element_id: layer.element_id,
            width: text_plan.identity.width,
            height: text_plan.identity.height,
            layout,
        },
        caption,
    })
}
