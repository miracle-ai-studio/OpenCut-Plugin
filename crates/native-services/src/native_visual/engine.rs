use super::{assemble, media::callback, plan::Plan, text, *};
use crate::classic_visual::VisualProcessSession;
use opencut_contracts::{
    blend_frame::BoundBlendFrame, graphic_frame::BoundGraphicFrame, mask_frame::BoundMaskFrame,
    scene_frame::BoundSceneFrame, text_frame::BoundTextFrame, visual_frame::BoundVisualLayer,
};
use opencut_editor_application::{
    classic_text_frame::ClassicTextFontHandle, classic_visual::*,
    media_frame_jobs::LocalVisualExecutionPolicy,
};
use opencut_renderer::{
    Backend, Execution, Identity, Limits, Receipt, Renderer,
    raster::{RasterLimits, TextRasterizer},
};
use serde_json::json;
use std::{sync::Arc, time::Instant};

pub struct NativeFrameEngine {
    renderer: Renderer,
    media: NativeMediaDecoders,
    text: TextRasterizer,
    policy: LocalVisualExecutionPolicy,
    failed: bool,
}
pub(crate) struct NativeRenderedFrame<R> {
    pub receipt: R,
    pub accounting: Receipt,
    pub encoded_bytes_read: u64,
}
pub(super) struct Bindings {
    pub layers: Vec<BoundVisualLayer>,
    pub clear: [f32; 4],
    pub background: [f32; 4],
    pub blend: Option<BoundBlendFrame>,
    pub masks: Option<BoundMaskFrame>,
    pub scene: Option<BoundSceneFrame>,
}
impl NativeFrameEngine {
    pub fn new(backend: Backend, policy: &LocalVisualExecutionPolicy) -> Result<Self, EditorError> {
        opencut_editor_application::media_frame_jobs::validate_policy(policy)?;
        let renderer = Renderer::new(
            backend,
            Limits {
                max_frame_pixels: policy.max_frame_pixels,
                max_gpu_bytes: policy.max_gpu_bytes,
                max_cpu_bytes: policy.max_rgba_bytes,
                max_layers: 3900,
            },
        )
        .map_err(text::raster_error)?;
        Ok(Self {
            renderer,
            media: NativeMediaDecoders::default(),
            text: TextRasterizer::new(policy.max_encoded_bytes),
            policy: policy.clone(),
            failed: false,
        })
    }
    pub fn identity(&self) -> &Identity {
        self.renderer.identity()
    }

    pub(crate) fn render<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
        &mut self,
        session: &mut S,
        resources: &mut R,
        begin: &str,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
        deadline: Instant,
    ) -> Result<NativeRenderedFrame<S::Receipt>, EditorError> {
        resources.checkpoint()?;
        if self.failed || session.policy() != &self.policy {
            return Err(invalid("Native frame owner or admission changed"));
        }
        let result = self.render_inner(session, resources, begin, handles, fonts, sink, deadline);
        if result.is_err() {
            self.failed = true;
            self.media = NativeMediaDecoders::default();
            self.text.retain(&Default::default());
        }
        result
    }
    fn render_inner<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
        &mut self,
        session: &mut S,
        resources: &mut R,
        begin: &str,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
        deadline: Instant,
    ) -> Result<NativeRenderedFrame<S::Receipt>, EditorError> {
        let plan = Plan::parse(begin, session.plan()?)?;
        text::authorize_fonts(session, resources, &plan, fonts, &mut self.text)?;
        let media = self
            .media
            .decode(session, resources, plan.media(), handles, deadline)?;
        let mut bindings = Bindings {
            layers: media.bound.layers.clone(),
            clear: media.bound.clear,
            background: media.bound.background,
            blend: None,
            masks: None,
            scene: None,
        };
        let limits = RasterLimits {
            max_pixels: self.policy.max_decoded_pixels,
            max_bytes: self.policy.max_rgba_bytes,
        };
        let text = text::prepare_text(&plan, &mut self.text, limits, &mut || {
            resources.checkpoint()
        })?;
        let mut rasters = text.images;
        let mut text_bound = None;
        if plan.text().is_some() {
            let value = if plan.caption().is_some() {
                callback(
                    session,
                    resources,
                    "bindCaptionFrameV3",
                    json!({"rasters":text.rasters,"captions":text.captions}),
                )?
            } else {
                callback(
                    session,
                    resources,
                    "bindTextFrame",
                    json!({"rasters":text.rasters}),
                )?
            };
            let bound: BoundTextFrame = serde_json::from_value(value)
                .map_err(|_| invalid("Invalid native text binding"))?;
            bindings.layers = bound.layers.clone();
            bindings.clear = bound.clear;
            bindings.background = bound.background;
            text_bound = Some(bound);
        }
        let mut graphics = Vec::new();
        if let Some(graphic) = plan.graphic() {
            for layer in &graphic.graphics {
                resources.checkpoint()?;
                let used = rasters
                    .values()
                    .map(|image| image.rgba.len() as u64)
                    .sum::<u64>();
                let available = RasterLimits {
                    max_pixels: limits.max_pixels,
                    max_bytes: limits
                        .max_bytes
                        .checked_sub(used)
                        .ok_or_else(|| invalid("Native generated surfaces exceed admission"))?,
                };
                let (image, receipt) =
                    opencut_renderer::raster::graphic(&layer.element_id, &layer.graphic, available)
                        .map_err(text::raster_error)?;
                rasters.insert(layer.element_id.clone(), Arc::new(image));
                graphics.push(receipt);
            }
            let bound: BoundGraphicFrame = serde_json::from_value(callback(
                session,
                resources,
                "bindGraphicFrameV1",
                json!({"graphics":graphics}),
            )?)
            .map_err(|_| invalid("Invalid native graphic binding"))?;
            bindings.layers = bound.layers;
            bindings.clear = bound.clear;
            bindings.background = bound.background;
        }
        if plan.blend().is_some() {
            bindings.blend = Some(
                serde_json::from_value(callback(
                    session,
                    resources,
                    "bindBlendFrameV1",
                    json!({}),
                )?)
                .map_err(|_| invalid("Invalid native blend binding"))?,
            );
        }
        if plan.mask().is_some() {
            bindings.masks = Some(
                serde_json::from_value(callback(session, resources, "bindMaskFrameV1", json!({}))?)
                    .map_err(|_| invalid("Invalid native mask binding"))?,
            );
        }
        if plan.scene().is_some() {
            bindings.scene = Some(
                serde_json::from_value(callback(
                    session,
                    resources,
                    "bindSceneFrameV1",
                    json!({}),
                )?)
                .map_err(|_| invalid("Invalid native scene binding"))?,
            );
        }
        let prepared = assemble::prepare(
            &plan,
            &bindings,
            &media.sources,
            &mut rasters,
            &mut self.text,
            limits,
            &mut || resources.checkpoint(),
        )?;
        let mut output = Output {
            session,
            resources,
            sink,
            deadline,
        };
        let accounting =
            assemble::render(&mut self.renderer, &plan, &bindings, &prepared, &mut output)?;
        let actual = json!({"rgbaBytes":accounting.rgba_bytes.to_string(),"peakAccountedBytes":accounting.accounted_cpu_bytes.to_string()});
        let mut receipt = if let Some(bound) = text_bound {
            let identity = &plan.text().expect("text receipt identity").identity;
            json!({"identity":identity,"engineIdentity":identity.profile,"bound":bound,"rgbaBytes":actual["rgbaBytes"],"peakAccountedBytes":actual["peakAccountedBytes"]})
        } else {
            let identity = &plan.media().identity;
            json!({"identity":identity,"engineIdentity":identity.profile,"bound":media.bound,"rgbaBytes":actual["rgbaBytes"],"peakAccountedBytes":actual["peakAccountedBytes"]})
        };
        if let Some(plan) = plan.caption() {
            receipt = json!({"identity":plan.identity,"engineIdentity":plan.identity.profile,"execution":receipt,"captions":text.captions});
        }
        if let Some(plan) = plan.graphic() {
            receipt = json!({"identity":plan.identity,"engineIdentity":plan.identity.profile,"execution":receipt,"graphics":graphics});
        }
        if let Some(plan) = plan.blend() {
            receipt = json!({"identity":plan.identity,"engineIdentity":plan.identity.profile,"execution":receipt,"blends":plan.blends});
        }
        if let Some(plan) = plan.mask() {
            receipt = json!({"identity":plan.identity,"engineIdentity":plan.identity.profile,"execution":receipt,"masks":plan.masks});
        }
        if let Some(plan) = plan.scene() {
            receipt = json!({"identity":plan.identity,"engineIdentity":plan.identity.profile,"execution":receipt,"stacks":plan.stacks,
            "maskFonts":plan.mask_fonts,"fontSynthesis":plan.font_synthesis,"steps":plan.steps,"background":plan.background});
        }
        output.resources.checkpoint()?;
        let receipt = serde_json::from_value(receipt)
            .map_err(|_| invalid("Native receipt differs from its typed execution session"))?;
        Ok(NativeRenderedFrame {
            receipt,
            accounting,
            encoded_bytes_read: media.encoded_bytes_read,
        })
    }
}
struct Output<'a, R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>> {
    session: &'a mut S,
    resources: &'a mut R,
    sink: &'a mut dyn ClassicVisualRgbaSink,
    deadline: Instant,
}
fn output_error(error: EditorError) -> opencut_renderer::Error {
    opencut_renderer::Error::new(error.code, error.message)
}
impl<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>> Execution
    for Output<'_, R, S>
{
    fn checkpoint(&mut self) -> opencut_renderer::Result<()> {
        self.resources.checkpoint().map_err(output_error)?;
        if Instant::now() >= self.deadline {
            return Err(opencut_renderer::Error::new(
                "VISUAL_TIME_LIMIT",
                "Native frame exceeded its deadline",
            ));
        }
        Ok(())
    }
    fn deadline(&self) -> Instant {
        self.deadline
    }
    fn rgba(&mut self, offset: u64, bytes: &[u8]) -> opencut_renderer::Result<()> {
        self.session
            .rgba_chunk(offset, bytes, self.sink)
            .map_err(output_error)
    }
}
