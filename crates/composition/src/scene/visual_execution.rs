//! Fixed Classic media-frame facts. Runtime dimensions are observations, not
//! permission to replace the revision, source clock or animation state.
use super::*;
pub use opencut_contracts::visual_frame::*;
use std::collections::BTreeMap;

pub const CLASSIC_VISUAL_PROFILE: &str = "classic-v31-gpu-normal-frame";

#[derive(Clone, Debug)]
pub struct PreparedVisualFrame {
    graph: SceneComposition,
    identity: VisualFrameIdentity,
}
impl PreparedVisualFrame {
    pub fn identity(&self) -> &VisualFrameIdentity {
        &self.identity
    }
    pub fn graph(&self) -> &SceneComposition {
        &self.graph
    }
    pub fn resources(&self) -> &[CasResource] {
        &self.graph.resources
    }
    pub fn layers(&self) -> &[CompiledLayer] {
        &self.graph.layers
    }
}

fn f32_value(value: f64) -> Result<f32, CompositionError> {
    let value = value as f32;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(unsupported("Classic GPU uniform exceeds finite f32"))
    }
}
fn rgba(color: &Color) -> Result<[f32; 4], CompositionError> {
    let values = [color.red, color.green, color.blue, color.alpha];
    if color.encoding != ColorEncoding::Srgb
        || values
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err(unsupported(
            "Classic media frame requires normalized sRGB background",
        ));
    }
    Ok([
        values[0] as f32,
        values[1] as f32,
        values[2] as f32,
        values[3] as f32,
    ])
}
fn covers_request(request: &Time, timestamp: f64, duration: f64) -> Result<bool, CompositionError> {
    let request = time(request)?;
    let seconds = request.ticks() as f64 / f64::from(request.timebase());
    let end = timestamp + duration;
    // Browser demux timestamps are IEEE seconds observations. A preceding frame
    // with a gap is not held past its own half-open presentation interval.
    Ok(timestamp.is_finite()
        && duration.is_finite()
        && duration > 0.0
        && end.is_finite()
        && timestamp <= seconds
        && seconds < end)
}

/// EOF behavior belongs to the fixed authored media layout. The decoder may
/// report its terminal frame, but cannot choose a different source window.
pub fn video_eof_policy_scene(
    layer: &CompiledLayer,
) -> Result<VisualVideoEofPolicy, CompositionError> {
    if layer
        .visual
        .as_ref()
        .is_some_and(|style| style.profile == VisualProfile::LegacyCompositionV3)
        && let SampledContent::Video { source, .. } = &layer.content
        && matches!(
            source.sample,
            SourceReadState::InBounds | SourceReadState::EndOfResource
        )
        && contains(&source.requested_read_span, time(&source.source_time)?)?
        && let BoundedReadWindow::Known {
            range: Some(range), ..
        } = &source.read_window
    {
        let start = time(&range.start)?;
        let end = end(range)?;
        return Ok(VisualVideoEofPolicy::HoldLastFrame {
            read_window_start_seconds: start.ticks() as f64 / f64::from(start.timebase()),
            read_window_end_seconds: end.ticks() as f64 / f64::from(end.timebase()),
        });
    }
    Ok(VisualVideoEofPolicy::NoFrame)
}

fn holds_terminal_frame(
    layer: &CompiledLayer,
    request: &Time,
    timestamp: f64,
    duration: f64,
) -> Result<bool, CompositionError> {
    let VisualVideoEofPolicy::HoldLastFrame {
        read_window_start_seconds,
        read_window_end_seconds,
    } = video_eof_policy_scene(layer)?
    else {
        return Ok(false);
    };
    let request = time(request)?;
    let seconds = request.ticks() as f64 / f64::from(request.timebase());
    let end = timestamp + duration;
    Ok(timestamp.is_finite()
        && timestamp >= 0.0
        && duration.is_finite()
        && duration > 0.0
        && end.is_finite()
        && end <= seconds
        && timestamp < read_window_end_seconds
        && read_window_start_seconds < end)
}
pub fn prepare_visual_frame_scene(
    graph: &SceneComposition,
) -> Result<PreparedVisualFrame, CompositionError> {
    reject_legacy_synthesis_fonts(&graph.fonts)?;
    if graph.scope != CompositionScope::Visual
        || !graph.audio.is_empty()
        || !graph.fonts.is_empty()
        || !graph.transitions.is_empty()
    {
        return Err(unsupported("Media frame excludes audio/fonts/transitions"));
    }
    if graph.project_id.is_empty()
        || graph.sequence_id.is_empty()
        || graph.revision.is_empty()
        || time(&graph.time)?.ticks() < 0
        || graph.format.canvas_size.width == 0
        || graph.format.canvas_size.height == 0
    {
        return Err(invalid("Invalid fixed visual frame identity"));
    }
    match &graph.background {
        CompiledBackground::Color { color } => {
            rgba(color)?;
        }
        _ => return Err(unsupported("Classic media frame excludes blur background")),
    }
    let mut ids = HashSet::new();
    let mut required = HashSet::new();
    for layer in &graph.layers {
        if layer.element_id.is_empty() || !ids.insert(&layer.element_id) {
            return Err(invalid("Duplicate visual layer identity"));
        }
        let style = layer
            .visual
            .as_ref()
            .ok_or_else(|| unsupported("Visual layer style missing"))?;
        if style.blend_mode != BlendMode::Normal
            || style.effects.iter().any(|e| e.enabled)
            || style.masks.iter().any(|m| m.enabled)
        {
            return Err(unsupported(format!(
                "Layer {} requires unsupported visual capability",
                layer.element_id
            )));
        }
        for value in [
            style.transform.position_x_px,
            style.transform.position_y_px,
            style.transform.scale_x,
            style.transform.scale_y,
            style.transform.rotation_degrees,
            style.effective_opacity,
        ] {
            f32_value(value)?;
        }
        let asset_id = match &layer.content {
            SampledContent::Image { asset_id } => asset_id,
            SampledContent::Video {
                asset_id,
                stream,
                source,
                ..
            } => {
                let point_profile = match style.profile {
                    VisualProfile::ClassicCanvasV31 => SourcePointProfile::ClassicVideoRoundedTicks,
                    VisualProfile::LegacyCompositionV3 => SourcePointProfile::ExactRational,
                };
                if stream.is_some() || source.point_profile != point_profile {
                    return Err(unsupported(
                        "Media decode requires the primary track and its fixed source clock",
                    ));
                }
                time(&source.source_time)?;
                if source.sample == SourceReadState::NeedsResourceProbe {
                    return Err(unsupported(
                        "Video requires observed source extent before execution",
                    ));
                }
                asset_id
            }
            _ => {
                return Err(unsupported(format!(
                    "Layer {} is not video or image",
                    layer.element_id
                )));
            }
        };
        required.insert(asset_id.as_str());
    }
    let mut resources = HashSet::new();
    for resource in &graph.resources {
        if !resources.insert(resource.asset_id.as_str())
            || !required.contains(resource.asset_id.as_str())
        {
            return Err(invalid(
                "Visual resources must exactly cover participating layers",
            ));
        }
    }
    if resources != required {
        return Err(invalid("Visual resource binding missing"));
    }
    Ok(PreparedVisualFrame {
        identity: VisualFrameIdentity {
            project_id: graph.project_id.clone(),
            revision: graph.revision.clone(),
            sequence_id: graph.sequence_id.clone(),
            time: graph.time.clone(),
            width: graph.format.canvas_size.width,
            height: graph.format.canvas_size.height,
            profile: CLASSIC_VISUAL_PROFILE.into(),
        },
        graph: graph.clone(),
    })
}

pub fn bind_visual_sources_scene(
    prepared: &PreparedVisualFrame,
    decoded: &[DecodedVisualSource],
) -> Result<BoundVisualFrame, CompositionError> {
    let mut observations = BTreeMap::new();
    for value in decoded {
        if observations.insert(value.element_id(), value).is_some() {
            return Err(invalid("Duplicate decoded element"));
        }
    }
    if observations.len() != prepared.layers().len() {
        return Err(invalid(
            "Decoded observations must cover the complete frame",
        ));
    }
    let mut layers = Vec::new();
    let mut ordered = Vec::new();
    for layer in prepared.layers() {
        let observation = observations
            .get(layer.element_id.as_str())
            .ok_or_else(|| invalid("Decoded observation missing"))?;
        let (asset_id, size) = match (&layer.content, *observation) {
            (
                SampledContent::Image { asset_id },
                DecodedVisualSource::ReadyImage { width, height, .. },
            ) => (asset_id, Some((*width, *height))),
            (
                SampledContent::Video {
                    asset_id,
                    stream,
                    source,
                    ..
                },
                DecodedVisualSource::ReadyVideo {
                    requested_time,
                    selected_video_stream,
                    actual_timestamp_seconds,
                    frame_duration_seconds,
                    eof_held,
                    width,
                    height,
                    ..
                },
            ) if requested_time == &source.source_time
                && selected_video_stream == stream
                && (if *eof_held {
                    holds_terminal_frame(
                        layer,
                        requested_time,
                        *actual_timestamp_seconds,
                        *frame_duration_seconds,
                    )?
                } else {
                    covers_request(
                        requested_time,
                        *actual_timestamp_seconds,
                        *frame_duration_seconds,
                    )? && source.sample == SourceReadState::InBounds
                }) =>
            {
                (asset_id, Some((*width, *height)))
            }
            (
                SampledContent::Video {
                    asset_id, source, ..
                },
                DecodedVisualSource::NoFrame {
                    requested_time,
                    reason,
                    ..
                },
            ) if requested_time == &source.source_time
                && match reason {
                    VisualNoFrameReason::OutsideReadWindow => {
                        source.sample != SourceReadState::InBounds
                            && matches!(
                                video_eof_policy_scene(layer)?,
                                VisualVideoEofPolicy::NoFrame
                            )
                    }
                    VisualNoFrameReason::Eof => {
                        source.sample == SourceReadState::InBounds
                            || matches!(
                                video_eof_policy_scene(layer)?,
                                VisualVideoEofPolicy::HoldLastFrame { .. }
                            )
                    }
                    VisualNoFrameReason::TimestampGap => source.sample == SourceReadState::InBounds,
                } =>
            {
                (asset_id, None)
            }
            _ => {
                return Err(invalid(
                    "Decoded kind, request point, selection or timestamp differs from fixed layer",
                ));
            }
        };
        let quad = if let Some((width, height)) = size {
            if width == 0 || height == 0 {
                return Err(invalid("Actual decoded dimensions must be positive"));
            }
            let style = layer.visual.as_ref().expect("prepared style");
            let transform = style.transform;
            // Both authored media layouts use contain fit and canvas pixels
            // from center. Keep their sampled values and clock untouched;
            // only the final GPU uniforms narrow to f32. This is the current
            // GPU renderer, not a claim of original FFmpeg raster identity.
            let contain = (f64::from(prepared.identity.width) / f64::from(width))
                .min(f64::from(prepared.identity.height) / f64::from(height));
            let scaled_width = f64::from(width) * contain * transform.scale_x;
            let scaled_height = f64::from(height) * contain * transform.scale_y;
            let quad = VisualQuad {
                center_x: f32_value(
                    f64::from(prepared.identity.width) / 2.0 + transform.position_x_px,
                )?,
                center_y: f32_value(
                    f64::from(prepared.identity.height) / 2.0 + transform.position_y_px,
                )?,
                width: f32_value(scaled_width.abs())?,
                height: f32_value(scaled_height.abs())?,
                rotation_radians: match style.profile {
                    VisualProfile::ClassicCanvasV31 => {
                        f32_value(transform.rotation_degrees)?.to_radians()
                    }
                    VisualProfile::LegacyCompositionV3 => {
                        f32_value(transform.rotation_degrees * std::f64::consts::PI / 180.0)?
                    }
                },
                flip_x: scaled_width < 0.0,
                flip_y: scaled_height < 0.0,
                opacity: f32_value(style.effective_opacity)?,
            };
            if quad.width == 0.0 || quad.height == 0.0 {
                None
            } else {
                Some(quad)
            }
        } else {
            None
        };
        layers.push(BoundVisualLayer {
            element_id: layer.element_id.clone(),
            asset_id: asset_id.clone(),
            quad,
        });
        ordered.push((**observation).clone());
    }
    let CompiledBackground::Color { color } = &prepared.graph.background else {
        unreachable!("prepared color")
    };
    Ok(BoundVisualFrame {
        identity: prepared.identity.clone(),
        clear: [0.0, 0.0, 0.0, 1.0],
        background: rgba(color)?,
        layers,
        decoded: ordered,
    })
}

#[cfg(test)]
mod tests;
