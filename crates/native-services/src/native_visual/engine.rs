use super::{assemble, media::callbacks, plan::Plan, text, *};
use crate::classic_visual::VisualProcessSession;
use opencut_contracts::{
    blend_frame::BoundBlendFrame,
    graphic_frame::BoundGraphicFrame,
    mask_frame::BoundMaskFrame,
    scene_frame::BoundSceneFrame,
    text_frame::BoundTextFrame,
    visual_frame::{BoundVisualFrame, BoundVisualLayer},
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
    pub source_preparation: Vec<opencut_contracts::source_preview::SourcePreviewUse>,
    pub receipt: R,
    pub accounting: Receipt,
    pub encoded_bytes_read: u64,
    pub hit_test: Vec<NativeVisualHit>,
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
        probe: Option<[u32; 2]>,
        source_raster: SourceRaster,
    ) -> Result<NativeRenderedFrame<S::Receipt>, EditorError> {
        resources.checkpoint()?;
        if self.failed || session.policy() != &self.policy {
            return Err(invalid("Native frame owner or admission changed"));
        }
        let result = self.render_inner(
            session,
            resources,
            begin,
            handles,
            fonts,
            sink,
            deadline,
            probe,
            source_raster,
        );
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
        probe: Option<[u32; 2]>,
        source_raster: SourceRaster,
    ) -> Result<NativeRenderedFrame<S::Receipt>, EditorError> {
        let plan = Plan::parse(begin, session.plan()?)?;
        text::authorize_fonts(session, resources, &plan, fonts, &mut self.text)?;
        let media = self.media.decode(
            session,
            resources,
            plan.media(),
            handles,
            source_raster,
            deadline,
        )?;
        let limits = RasterLimits {
            max_pixels: self.policy.max_decoded_pixels,
            max_bytes: self.policy.max_rgba_bytes,
        };
        let text = text::prepare_text(&plan, &mut self.text, limits, &mut || {
            resources.checkpoint()
        })?;
        let mut rasters = text.images;
        let mut requests = vec![("bindFrame", json!({"decoded":media.decoded}))];
        if plan.text().is_some() {
            requests.push(if plan.caption().is_some() {
                (
                    "bindCaptionFrameV3",
                    json!({"rasters":text.rasters,"captions":text.captions}),
                )
            } else {
                ("bindTextFrame", json!({"rasters":text.rasters}))
            });
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
            requests.push(("bindGraphicFrameV1", json!({"graphics":graphics})));
        }
        if plan.blend().is_some() {
            requests.push(("bindBlendFrameV1", json!({})));
        }
        if plan.mask().is_some() {
            requests.push(("bindMaskFrameV1", json!({})));
        }
        if plan.scene().is_some() {
            requests.push(("bindSceneFrameV1", json!({})));
        }
        // Consume each bounded reply immediately. Keep typed bindings, never a
        // vector of all intermediate JSON replies or their encoded packets.
        let names: Vec<_> = requests.iter().map(|(name, _)| *name).collect();
        let mut index = 0;
        let mut media_bound = None;
        let mut text_bound: Option<BoundTextFrame> = None;
        let mut bindings = Bindings {
            layers: Vec::new(),
            clear: [0.0; 4],
            background: [0.0; 4],
            blend: None,
            masks: None,
            scene: None,
        };
        callbacks(session, resources, requests, &mut |value| {
            let name = names
                .get(index)
                .ok_or_else(|| invalid("Unexpected native binding reply"))?;
            index += 1;
            match *name {
                "bindFrame" => {
                    let bound: BoundVisualFrame = serde_json::from_value(value)
                        .map_err(|_| invalid("Invalid application-bound native frame"))?;
                    bindings.layers = bound.layers.clone();
                    bindings.clear = bound.clear;
                    bindings.background = bound.background;
                    media_bound = Some(bound);
                }
                "bindTextFrame" | "bindCaptionFrameV3" => {
                    let bound: BoundTextFrame = serde_json::from_value(value)
                        .map_err(|_| invalid("Invalid native text binding"))?;
                    bindings.layers = bound.layers.clone();
                    bindings.clear = bound.clear;
                    bindings.background = bound.background;
                    text_bound = Some(bound);
                }
                "bindGraphicFrameV1" => {
                    let bound: BoundGraphicFrame = serde_json::from_value(value)
                        .map_err(|_| invalid("Invalid native graphic binding"))?;
                    bindings.layers = bound.layers;
                    bindings.clear = bound.clear;
                    bindings.background = bound.background;
                }
                "bindBlendFrameV1" => {
                    bindings.blend = Some(
                        serde_json::from_value::<BoundBlendFrame>(value)
                            .map_err(|_| invalid("Invalid native blend binding"))?,
                    )
                }
                "bindMaskFrameV1" => {
                    bindings.masks = Some(
                        serde_json::from_value::<BoundMaskFrame>(value)
                            .map_err(|_| invalid("Invalid native mask binding"))?,
                    )
                }
                "bindSceneFrameV1" => {
                    bindings.scene = Some(
                        serde_json::from_value::<BoundSceneFrame>(value)
                            .map_err(|_| invalid("Invalid native scene binding"))?,
                    )
                }
                _ => return Err(invalid("Unknown native binding")),
            }
            Ok(())
        })?;
        if index != names.len() {
            return Err(invalid("Missing native binding reply"));
        }
        let media_bound = media_bound.ok_or_else(|| invalid("Missing native media binding"))?;
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
            probe,
            hit_alphas: Vec::new(),
        };
        let accounting =
            assemble::render(&mut self.renderer, &plan, &bindings, &prepared, &mut output)?;
        let actual = json!({"rgbaBytes":accounting.rgba_bytes.to_string(),"peakAccountedBytes":accounting.accounted_cpu_bytes.to_string()});
        let mut receipt = if let Some(bound) = text_bound {
            let identity = &plan.text().expect("text receipt identity").identity;
            json!({"identity":identity,"engineIdentity":identity.profile,"bound":bound,"rgbaBytes":actual["rgbaBytes"],"peakAccountedBytes":actual["peakAccountedBytes"]})
        } else {
            let identity = &plan.media().identity;
            json!({"identity":identity,"engineIdentity":identity.profile,"bound":media_bound,"rgbaBytes":actual["rgbaBytes"],"peakAccountedBytes":actual["peakAccountedBytes"]})
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
            source_preparation: media.source_preparation,
            receipt,
            accounting,
            encoded_bytes_read: media.encoded_bytes_read,
            hit_test: if probe.is_some() {
                assemble::hit_test(&prepared, &output.hit_alphas)?
            } else {
                Vec::new()
            },
        })
    }
}
struct Output<'a, R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>> {
    session: &'a mut S,
    resources: &'a mut R,
    sink: &'a mut dyn ClassicVisualRgbaSink,
    deadline: Instant,
    probe: Option<[u32; 2]>,
    hit_alphas: Vec<u8>,
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
    fn hit_probe(&self) -> Option<[u32; 2]> {
        self.probe
    }
    fn hit_alpha(&mut self, layer: usize, alpha: u8) -> opencut_renderer::Result<()> {
        if self.probe.is_none() || layer != self.hit_alphas.len() {
            return Err(output_error(invalid(
                "Native hit observation order changed",
            )));
        }
        self.hit_alphas.push(alpha);
        Ok(())
    }
    fn rgba(&mut self, offset: u64, bytes: &[u8]) -> opencut_renderer::Result<()> {
        self.session
            .rgba_chunk(offset, bytes, self.sink)
            .map_err(output_error)
    }
}
