//! Explicit pinned-caption execution partition. The source graph remains fixed;
//! only its already-sampled caption nodes become internal raster text jobs.
use super::*;
use opencut_contracts::caption_frame::CaptionRasterPlan;
pub const CLASSIC_CAPTION_FRAME_PROFILE: &str = "classic-v31-gpu-normal-caption-frame-v3";
pub const CLASSIC_CAPTION_WRAP_PROFILE: &str = "classic-v31-greedy-whitespace-v1";
#[derive(Clone, Debug)]
pub struct PreparedCaptionFrame {
    source_graph: SceneComposition,
    pub(super) execution: PreparedTextFrame,
    captions: Vec<CaptionRasterPlan>,
}
impl PreparedCaptionFrame {
    pub fn source_graph(&self) -> &SceneComposition {
        &self.source_graph
    }
    pub fn execution(&self) -> &PreparedTextFrame {
        &self.execution
    }
    pub fn captions(&self) -> &[CaptionRasterPlan] {
        &self.captions
    }
}
pub fn prepare_caption_frame(
    graph: &SceneComposition,
) -> Result<PreparedCaptionFrame, CompositionError> {
    reject_legacy_synthesis_fonts(&graph.fonts)?;
    let mut execution = graph.clone();
    let mut captions = Vec::new();
    let width = f64::from(graph.format.canvas_size.width);
    let height = f64::from(graph.format.canvas_size.height);
    for layer in &mut execution.layers {
        let SampledContent::Caption { text, style } = &layer.content else {
            continue;
        };
        if style.layout != CaptionLayoutProfile::PinnedCaptionV4 {
            return Err(unsupported(
                "V3 caption raster requires pinned_caption_v4; legacy captions stay on the legacy renderer",
            ));
        }
        let FontBinding::Locked { font_id } = &style.font else {
            return Err(unsupported("V3 captions require an authorized locked font"));
        };
        let font = graph
            .fonts
            .iter()
            .find(|font| &font.id == font_id)
            .ok_or_else(|| invalid("Caption font resource is missing"))?;
        let font_size_px = match style.font_size {
            FontSize::ClassicHeight90 { value } => value * height / 90.0,
            FontSize::CanvasPixels { value } => value,
        };
        if !font_size_px.is_finite() || font_size_px <= 0.0 {
            return Err(invalid("Caption font size must be positive"));
        }
        let line_height = style
            .line_height_px
            .map(f64::from)
            .unwrap_or(font_size_px * 1.2);
        if !line_height.is_finite() || line_height <= 0.0 {
            return Err(invalid("Caption line height must be positive"));
        }
        let max_width = style.max_width_px.map(f64::from).unwrap_or(width * 0.8);
        if max_width <= 0.0 || max_width > width {
            return Err(invalid("Caption width must fit its canvas"));
        }
        captions.push(CaptionRasterPlan {
            element_id: layer.element_id.clone(),
            max_width_px: max_width,
            bottom_margin_px: height * 0.05,
            wrap_profile: CLASSIC_CAPTION_WRAP_PROFILE.into(),
        });
        let clear = Color {
            encoding: ColorEncoding::Srgb,
            red: 0.0,
            green: 0.0,
            blue: 0.0,
            alpha: 0.0,
        };
        layer.content = SampledContent::Text {
            text: Text {
                content: text.clone(),
                font: style.font.clone(),
                font_weight: if font.weight == 700 {
                    FontWeight::Bold
                } else {
                    FontWeight::Normal
                },
                font_style: font.style,
                decoration: TextDecoration::None,
                align: style.text_align,
                font_size: style.font_size,
                color: style.color.clone(),
                letter_spacing_px: 0.0,
                line_height_multiplier: line_height / font_size_px,
                background: TextBackground {
                    enabled: style.background_color.is_some(),
                    color: style.background_color.clone().unwrap_or(clear),
                    corner_radius_percent: 0.0,
                    padding_x_at_size15: 0.0,
                    padding_y_at_size15: 0.0,
                    offset_x_px: 0.0,
                    offset_y_px: 0.0,
                },
            },
        };
    }
    Ok(PreparedCaptionFrame {
        source_graph: graph.clone(),
        execution: prepare_text_frame(&execution)?,
        captions,
    })
}
