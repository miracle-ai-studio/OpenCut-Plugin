use super::*;
use crate::effects::{BlurPass, blur, color};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Layout {
    pub width: u32,
    pub height: u32,
    pub masks: usize,
    pub effects: bool,
    pub colors: BTreeSet<[u32; 2]>,
    pub uploads: BTreeMap<[u32; 2], usize>,
}
pub(super) struct Passes {
    pub color: Vec<[i32; 8]>,
    pub blur: Vec<BlurPass>,
}
impl Passes {
    pub fn empty() -> Self {
        Self {
            color: Vec::new(),
            blur: Vec::new(),
        }
    }
    fn count(&self) -> u64 {
        (self.color.len() + self.blur.len()) as u64
    }
}
pub(super) struct Admission {
    pub receipt: Receipt,
    pub layout: Layout,
    pub background: Passes,
    pub layers: Vec<Passes>,
    pub adjustments: BTreeMap<usize, Passes>,
}

impl Renderer {
    pub(super) fn admit(&self, frame: &Frame<'_>) -> Result<Admission> {
        let pixels = u64::from(frame.width) * u64::from(frame.height);
        let dimension = self.device.limits().max_texture_dimension_2d;
        if pixels == 0
            || pixels > self.limits.max_frame_pixels
            || frame.width > dimension
            || frame.height > dimension
            || frame.layers.len() > self.limits.max_layers
            || frame.steps.len() > self.limits.max_layers + 64
        {
            return Err(budget("Frame exceeds native pixel/layer admission"));
        }
        let valid_color = |color: &[f32; 4]| {
            color
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        };
        if !valid_color(&frame.clear) {
            return Err(invalid("Invalid prepared clear color"));
        }
        let mut layout = Layout {
            width: frame.width,
            height: frame.height,
            masks: 0,
            effects: false,
            colors: BTreeSet::new(),
            uploads: BTreeMap::new(),
        };
        let mut uploaded = 0u64;
        let mut sources = BTreeSet::new();
        let mut mask_outputs = 0u64;
        let mut uniforms = 0u64;
        let mut admit_layer = |layer: &Layer<'_>| -> Result<Passes> {
            let source = &layer.source;
            if source.width == 0
                || source.height == 0
                || source.width > dimension
                || source.height > dimension
            {
                return Err(invalid("Prepared source extent exceeds device limits"));
            }
            let bytes = u64::from(source.width) * u64::from(source.height) * 4;
            if bytes != source.rgba.len() as u64 {
                return Err(invalid("Prepared source extent differs from its pixels"));
            }
            if sources.insert((source.rgba.as_ptr() as usize, source.width, source.height)) {
                *layout
                    .uploads
                    .entry([source.width, source.height])
                    .or_default() += 1;
                uploaded = uploaded
                    .checked_add(bytes)
                    .ok_or_else(|| budget("Source byte overflow"))?;
            }
            let q = &layer.quad;
            if [
                q.center_x,
                q.center_y,
                q.width,
                q.height,
                q.rotation_radians,
                q.opacity,
            ]
            .iter()
            .any(|v| !v.is_finite())
                || q.width <= 0.0
                || q.height <= 0.0
                || layer.masks.len() > 64
            {
                return Err(invalid("Invalid prepared layer geometry"));
            }
            for mask in layer.masks {
                if mask.source.width != frame.width
                    || mask.source.height != frame.height
                    || mask.source.rgba.len() as u64 != pixels * 4
                    || !mask.feather.is_finite()
                    || mask.feather < 0.0
                {
                    return Err(invalid("Invalid full-frame mask raster"));
                }
                if sources.insert((
                    mask.source.rgba.as_ptr() as usize,
                    mask.source.width,
                    mask.source.height,
                )) {
                    *layout
                        .uploads
                        .entry([mask.source.width, mask.source.height])
                        .or_default() += 1;
                    uploaded = uploaded
                        .checked_add(pixels * 4)
                        .ok_or_else(|| budget("Mask byte overflow"))?;
                }
                layout.masks = layout.masks.max(if mask.feather > 0.0 { 3 } else { 1 });
                uniforms += 1;
                if mask.feather > 0.0 {
                    // Each JFA seeds inside/outside and visits ceil(log2(N))
                    // distances, followed by one distance-composition pass.
                    let steps = u32::BITS - (frame.width.max(frame.height) - 1).leading_zeros();
                    uniforms += 3 + 2 * u64::from(steps);
                    mask_outputs += pixels * 4;
                }
            }
            let passes = Passes {
                color: color(layer.effects, layer.color_matrix, layer.rotation_degrees)?,
                blur: blur(layer.effects, layer.effect_size[0], layer.effect_size[1])?,
            };
            layout.effects |= !passes.blur.is_empty();
            if !passes.color.is_empty() {
                layout.colors.insert(if layer.rotation_degrees % 180 == 0 {
                    [source.width, source.height]
                } else {
                    [source.height, source.width]
                });
            }
            uniforms += 2 + passes.count();
            Ok(passes)
        };
        let background = match &frame.background {
            Background::Color(color) => {
                if !valid_color(color) {
                    return Err(invalid("Invalid prepared background"));
                }
                Passes::empty()
            }
            Background::Layer(layer) => admit_layer(layer)?,
        };
        let layers = frame
            .layers
            .iter()
            .map(&mut admit_layer)
            .collect::<Result<Vec<_>>>()?;
        drop(admit_layer);
        if matches!(&frame.background, Background::Color(_)) {
            uploaded += 4;
            *layout.uploads.entry([1, 1]).or_default() += 1;
            uniforms += 2;
        }
        let mut seen = vec![false; frame.layers.len()];
        let mut adjustments = BTreeMap::new();
        for (index, step) in frame.steps.iter().enumerate() {
            match step {
                Step::Layer(layer) => {
                    let was_seen = seen
                        .get_mut(*layer)
                        .ok_or_else(|| invalid("Unknown prepared layer index"))?;
                    if *was_seen {
                        return Err(invalid("Duplicate prepared layer index"));
                    }
                    *was_seen = true;
                }
                Step::Effects(effects) => {
                    let passes = Passes {
                        color: color(effects, ColorMatrix::Unspecified, 0)?,
                        blur: blur(effects, frame.width, frame.height)?,
                    };
                    layout.effects |= !passes.blur.is_empty();
                    if !passes.color.is_empty() {
                        layout.colors.insert([frame.width, frame.height]);
                    }
                    uniforms += passes.count();
                    adjustments.insert(index, passes);
                }
            }
        }
        if seen.contains(&false) {
            return Err(invalid("Prepared order omitted a layer"));
        }
        let readback = (u64::from(frame.width) * 4).div_ceil(256) * 256 * u64::from(frame.height);
        let targets = pixels * 4 * (3 + layout.masks as u64 + if layout.effects { 2 } else { 0 })
            + layout
                .colors
                .iter()
                .map(|[w, h]| u64::from(*w) * u64::from(*h) * 8)
                .sum::<u64>();
        // Retained upload buffers remain live through submission completion.
        // Count their actual padded rows, also on warm executions. The layout
        // contains only this frame's unique sources, including its background.
        let staging = layout
            .uploads
            .iter()
            .map(|([width, height], count)| {
                (u64::from(*width) * 4).div_ceil(256) * 256 * u64::from(*height) * *count as u64
            })
            .sum::<u64>();
        // Uniforms use conservative 256-byte placement alignment.
        let uniform_bytes = uniforms * 256;
        let gpu = targets
            .checked_add(readback)
            .and_then(|n| n.checked_add(uploaded + staging))
            .and_then(|n| n.checked_add(mask_outputs + uniform_bytes + 48))
            .ok_or_else(|| budget("Native GPU accounting overflow"))?;
        let cpu = uploaded
            .checked_add(staging)
            .and_then(|n| n.checked_add(readback + uniform_bytes + CHUNK as u64))
            .ok_or_else(|| budget("Native CPU accounting overflow"))?;
        if gpu > self.limits.max_gpu_bytes || cpu > self.limits.max_cpu_bytes {
            return Err(budget("Prepared frame exceeds native allocation admission"));
        }
        Ok(Admission {
            receipt: Receipt {
                rgba_bytes: pixels * 4,
                accounted_gpu_bytes: gpu,
                accounted_cpu_bytes: cpu,
            },
            layout,
            background,
            layers,
            adjustments,
        })
    }
}
