//! Generated graphics partition over the original sampled graph; no resource projection.
use super::*;
use opencut_contracts::{graphic_frame::*, visual_frame::VisualQuad};
pub const CLASSIC_GRAPHIC_FRAME_PROFILE: &str = "classic-v31-gpu-normal-graphic-frame-v1";
#[derive(Clone, Debug)]
pub struct PreparedGraphicFrame {
    source_graph: SceneComposition,
    pub(super) execution: PreparedCaptionFrame,
    graphics: Vec<GraphicLayerPlan>,
    order: Vec<String>,
}
impl PreparedGraphicFrame {
    pub fn source_graph(&self) -> &SceneComposition {
        &self.source_graph
    }
    pub fn execution(&self) -> &PreparedCaptionFrame {
        &self.execution
    }
    pub fn graphics(&self) -> &[GraphicLayerPlan] {
        &self.graphics
    }
    pub fn order(&self) -> &[String] {
        &self.order
    }
}
fn finite(value: f64) -> Result<f32, CompositionError> {
    let cast = value as f32;
    if !value.is_finite() || !cast.is_finite() {
        return Err(invalid("Graphic geometry/quad is not finite f32"));
    }
    Ok(cast)
}
fn color(value: &Color) -> Result<[f64; 4], CompositionError> {
    let values = [value.red, value.green, value.blue, value.alpha];
    if value.encoding != ColorEncoding::Srgb
        || values
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err(unsupported("Graphic raster requires bounded sRGB color"));
    }
    Ok(values)
}
pub fn graphic_raster_receipt(
    layer: &GraphicLayerPlan,
) -> Result<GraphicRasterReceipt, CompositionError> {
    let g = &layer.graphic;
    let width = g.stroke.width_px;
    if g.source_width == 0
        || g.source_height == 0
        || !width.is_finite()
        || !(width * 2.0).is_finite()
        || width < 0.0
    {
        return Err(invalid("Graphic raster source/stroke is invalid"));
    }
    let padding = if width == 0.0 || g.stroke.align == StrokeAlign::Inside {
        0.0
    } else {
        (width
            * if g.stroke.align == StrokeAlign::Center {
                5.0
            } else {
                10.0
            })
        .ceil()
    };
    if !padding.is_finite() || padding > f64::from(u32::MAX) {
        return Err(invalid("Graphic raster padding overflows"));
    }
    let padding = padding as u32;
    let twice = padding
        .checked_mul(2)
        .ok_or_else(|| invalid("Graphic raster padding overflows"))?;
    let receipt = GraphicRasterReceipt {
        element_id: layer.element_id.clone(),
        width: g
            .source_width
            .checked_add(twice)
            .ok_or_else(|| invalid("Graphic raster width overflows"))?,
        height: g
            .source_height
            .checked_add(twice)
            .ok_or_else(|| invalid("Graphic raster height overflows"))?,
        origin_x: padding,
        origin_y: padding,
    };
    if receipt.width > 16384 || receipt.height > 16384 {
        return Err(invalid("Graphic raster dimensions exceed 16384"));
    }
    Ok(receipt)
}
pub fn graphic_raster_receipts(
    prepared: &PreparedGraphicFrame,
) -> Result<Vec<GraphicRasterReceipt>, CompositionError> {
    prepared
        .graphics
        .iter()
        .map(graphic_raster_receipt)
        .collect()
}
pub fn validate_graphic_receipts(
    prepared: &PreparedGraphicFrame,
    receipts: &[GraphicRasterReceipt],
) -> Result<(), CompositionError> {
    if graphic_raster_receipts(prepared)? != receipts {
        return Err(invalid(
            "Graphic receipts differ from fixed order/source/padding",
        ));
    }
    Ok(())
}
pub fn prepare_graphic_frame(
    graph: &SceneComposition,
) -> Result<PreparedGraphicFrame, CompositionError> {
    reject_legacy_synthesis_fonts(&graph.fonts)?;
    let mut execution = graph.clone();
    let mut graphics = Vec::new();
    let mut order = Vec::new();
    let mut ids = HashSet::new();
    for layer in &graph.layers {
        if layer.element_id.is_empty() || !ids.insert(&layer.element_id) {
            return Err(invalid(
                "Graphic frame layer identity is empty or duplicate",
            ));
        }
        order.push(layer.element_id.clone());
        let SampledContent::Graphic { graphic } = &layer.content else {
            continue;
        };
        let style = layer
            .visual
            .as_ref()
            .ok_or_else(|| unsupported("Graphic visual style missing"))?;
        if style.profile != VisualProfile::ClassicCanvasV31
            || style.blend_mode != BlendMode::Normal
            || style.effects.iter().any(|e| e.enabled)
            || style.masks.iter().any(|m| m.enabled)
        {
            return Err(unsupported(
                "Graphic frame supports only normal Classic graphics without effective effects/masks",
            ));
        }
        let geometry = match graphic.geometry {
            GraphicGeometry::Rectangle {
                corner_radius_percent,
            } if corner_radius_percent.is_finite()
                && (0.0..=100.0).contains(&corner_radius_percent) =>
            {
                GraphicGeometryPlan::Rectangle {
                    corner_radius_percent,
                }
            }
            GraphicGeometry::Ellipse => GraphicGeometryPlan::Ellipse,
            GraphicGeometry::Polygon {
                sides,
                corner_radius_percent,
            } if sides.is_finite()
                && sides >= 3.0
                && corner_radius_percent.is_finite()
                && (0.0..=100.0).contains(&corner_radius_percent) =>
            {
                GraphicGeometryPlan::Polygon {
                    sides,
                    corner_radius_percent,
                }
            }
            GraphicGeometry::Star {
                points,
                depth_percent,
            } if points.is_finite()
                && points >= 2.0
                && depth_percent.is_finite()
                && (0.0..=100.0).contains(&depth_percent) =>
            {
                GraphicGeometryPlan::Star {
                    points,
                    depth_percent,
                }
            }
            GraphicGeometry::LegacyRectangle { .. } | GraphicGeometry::LegacyEllipse { .. } => {
                return Err(unsupported(
                    "Legacy graphic geometry requires its own execution profile",
                ));
            }
            _ => return Err(invalid("Graphic geometry has invalid parametric values")),
        };
        let mut plan = GraphicLayerPlan {
            element_id: layer.element_id.clone(),
            graphic: GraphicRasterPlan {
                geometry,
                source_width: graphic.source_size.width,
                source_height: graphic.source_size.height,
                fill: color(&graphic.fill)?,
                stroke: GraphicStrokePlan {
                    color: color(&graphic.stroke.color)?,
                    width_px: graphic.stroke.width_px,
                    align: graphic.stroke.align,
                },
            },
            quad: None,
        };
        let receipt = graphic_raster_receipt(&plan)?;
        let t = style.transform;
        let contain = (f64::from(graph.format.canvas_size.width)
            / f64::from(graphic.source_size.width))
        .min(f64::from(graph.format.canvas_size.height) / f64::from(graphic.source_size.height));
        let w = f64::from(receipt.width) * contain * t.scale_x;
        let h = f64::from(receipt.height) * contain * t.scale_y;
        let quad = VisualQuad {
            center_x: finite(f64::from(graph.format.canvas_size.width) / 2.0 + t.position_x_px)?,
            center_y: finite(f64::from(graph.format.canvas_size.height) / 2.0 + t.position_y_px)?,
            width: finite(w.abs())?,
            height: finite(h.abs())?,
            rotation_radians: finite(t.rotation_degrees)?.to_radians(),
            flip_x: w < 0.0,
            flip_y: h < 0.0,
            opacity: finite(style.effective_opacity)?,
        };
        if !quad.rotation_radians.is_finite() {
            return Err(invalid("Graphic rotation overflows"));
        }
        if quad.width != 0.0 && quad.height != 0.0 {
            plan.quad = Some(quad)
        }
        graphics.push(plan);
    }
    if order.len() > 60 {
        return Err(unsupported("Graphic frame exceeds 60 layers"));
    }
    execution
        .layers
        .retain(|layer| !matches!(layer.content, SampledContent::Graphic { .. }));
    Ok(PreparedGraphicFrame {
        source_graph: graph.clone(),
        execution: prepare_caption_frame(&execution)?,
        graphics,
        order,
    })
}
