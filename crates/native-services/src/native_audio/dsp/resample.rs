// Native adaptations of Chromium 153.0.8010.12 SincResampler and the non-looping
// AudioBufferSource interpolation path. Copyright (C) 2011 Google Inc.;
// Copyright 2022 The Chromium Authors. BSD notices: vendor/audio.
use super::*;
/// decodeAudioData's fixed Blackman-windowed 32-tap/32-offset conversion.
/// AudioBufferSource preparation is a distinct operation below: its original
/// interpolation and EOF rules must not be replaced by this decode filter.
pub struct DecodeResampler {
    kernels: PcmArray,
    scale: f64,
}
impl DecodeResampler {
    const SIZE: usize = 32;
    const OFFSETS: usize = 32;
    pub fn new(source_rate: u32, target_rate: u32, memory: &PcmMemory) -> Result<Self> {
        if source_rate == 0 || target_rate == 0 || source_rate > 384000 || target_rate > 192000 {
            return Err(invalid(
                "Native sample rate conversion is outside admission",
            ));
        }
        let scale = f64::from(source_rate) / f64::from(target_rate);
        let cutoff = if scale > 1.0 { 1.0 / scale } else { 1.0 } * 0.9;
        let mut kernels = memory.array(Self::SIZE * (Self::OFFSETS + 1))?;
        for offset in 0..=Self::OFFSETS {
            let fractional = offset as f64 / Self::OFFSETS as f64;
            for i in 0..Self::SIZE {
                let s = cutoff * std::f64::consts::PI * (i as f64 - 16.0 - fractional);
                let sinc = if s == 0.0 { 1.0 } else { s.sin() / s } * cutoff;
                let x = (i as f64 - fractional) / Self::SIZE as f64;
                let window = 0.42 - 0.5 * (std::f64::consts::TAU * x).cos()
                    + 0.08 * (std::f64::consts::TAU * 2.0 * x).cos();
                kernels.as_mut_slice()[offset * Self::SIZE + i] = (sinc * window) as f32;
            }
        }
        Ok(Self { kernels, scale })
    }
    pub fn process(
        &self,
        source: &[f32],
        target: &mut [f32],
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        if source.iter().any(|v| !v.is_finite()) {
            return Err(invalid("Native resampling received non-finite PCM"));
        }
        self.process_source(
            source.len() as u64,
            &mut |i| Ok(source[i as usize]),
            target,
            checkpoint,
        )
    }
    /// Read admitted retained chunks without flattening a second full native
    /// source. Arithmetic and filter phase are shared with the slice entrypoint.
    pub fn process_source(
        &self,
        source_frames: u64,
        source: &mut dyn FnMut(u64) -> Result<f32>,
        target: &mut [f32],
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        if self.scale == 1.0 {
            for (i, value) in target.iter_mut().enumerate() {
                if i % 4096 == 0 {
                    checkpoint()?;
                }
                *value = if (i as u64) < source_frames {
                    source(i as u64)?
                } else {
                    0.0
                };
                if !value.is_finite() {
                    return Err(invalid("Native resampling received non-finite PCM"));
                }
            }
            return checkpoint();
        }
        let mut position = 0.0;
        let mut origin = 0i64;
        for (at, out) in target.iter_mut().enumerate() {
            if at % 512 == 0 {
                checkpoint()?;
            }
            while position >= 512.0 {
                position -= 512.0;
                origin += 512;
            }
            let index = position as usize;
            let virtual_offset = (position - index as f64) * Self::OFFSETS as f64;
            let offset = virtual_offset as usize;
            let fraction = virtual_offset - offset as f64;
            let mut tap = |i: usize| -> Result<f32> {
                let sample = origin + index as i64 - 16 + i as i64;
                if sample < 0 || sample as u64 >= source_frames {
                    Ok(0.0)
                } else {
                    let value = source(sample as u64)?;
                    if !value.is_finite() {
                        return Err(invalid("Native resampling received non-finite PCM"));
                    }
                    Ok(value)
                }
            };
            let k1 = &self.kernels.as_slice()[offset * 32..offset * 32 + 32];
            let k2 = &self.kernels.as_slice()[(offset + 1) * 32..(offset + 2) * 32];
            let (mut sum1, mut sum2) = (0.0f32, 0.0f32);
            let mut i = 0;
            // Preserve Blink's per-lane Float32 accumulation on x86, including
            // scalar alignment prefix/tail. ARM uses its scalar reference order.
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            {
                let prefix = ((4 - index % 4) % 4).min(32);
                while i < prefix {
                    let value = tap(i)?;
                    sum1 += value * k1[i];
                    sum2 += value * k2[i];
                    i += 1;
                }
                let end = i + (32 - i) / 4 * 4;
                let (mut a, mut b) = ([0.0f32; 4], [0.0f32; 4]);
                while i < end {
                    for lane in 0..4 {
                        let value = tap(i + lane)?;
                        a[lane] += value * k1[i + lane];
                        b[lane] += value * k2[i + lane];
                    }
                    i += 4;
                }
                sum1 += a[0] + a[1] + a[2] + a[3];
                sum2 += b[0] + b[1] + b[2] + b[3];
            }
            while i < 32 {
                let value = tap(i)?;
                sum1 += value * k1[i];
                sum2 += value * k2[i];
                i += 1;
            }
            *out = ((1.0 - fraction) * f64::from(sum1) + fraction * f64::from(sum2)) as f32;
            if !out.is_finite() {
                return Err(invalid("Native resampling overflowed Float32 PCM"));
            }
            position += self.scale;
        }
        checkpoint()
    }
}
/// The retained OfflineAudioContext buffer-source preparation reads a native
/// rate buffer using a floating source cursor. The final fractional sample
/// extrapolates from its preceding sample, then the remaining target is zero.
pub fn resample_preparation(
    source: &[f32],
    target: &mut [f32],
    source_rate: u32,
    target_rate: u32,
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    if source_rate == 0
        || target_rate == 0
        || source_rate > 384000
        || target_rate > 192000
        || source.iter().any(|v| !v.is_finite())
    {
        return Err(invalid(
            "Native preparation resampling requires finite admitted PCM",
        ));
    }
    target.fill(0.0);
    let mut position = 0.0;
    let step = f64::from(source_rate) / f64::from(target_rate);
    for (at, value) in target.iter_mut().enumerate() {
        if at % 4096 == 0 {
            checkpoint()?;
        }
        if position >= source.len() as f64 {
            break;
        }
        let index = position as usize;
        let next = (index + 1).min(source.len() - 1);
        let fraction = position - index as f64;
        let sample = if index == next && index >= 1 {
            let previous = f64::from(source[index - 1]);
            let last = f64::from(source[index]);
            last + (last - previous) * fraction
        } else {
            (1.0 - fraction) * f64::from(source[index]) + fraction * f64::from(source[next])
        };
        *value = sample.clamp(f64::from(f32::MIN), f64::from(f32::MAX)) as f32;
        position += step;
    }
    checkpoint()
}
