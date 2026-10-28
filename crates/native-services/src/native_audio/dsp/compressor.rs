// Native adaptation of Chromium 153.0.8010.12 DynamicsCompressor.
// Copyright (C) 2011 Google Inc. All rights reserved.
// BSD notice and pinned source identities: vendor/audio/LICENSE.chromium and source.json.
use super::*;
const HEADROOM: f32 = 0.98;
const HALF_PI: f32 = std::f32::consts::FRAC_PI_2;
fn db(linear: f32) -> f32 {
    20.0 * linear.log10()
}
fn linear(decibels: f32) -> f32 {
    10.0f32.powf(0.05 * decibels)
}
fn finite(value: f32, default: f32) -> f32 {
    if value.is_finite() { value } else { default }
}
fn normal(value: f32) -> f32 {
    if value.is_subnormal() { 0.0 } else { value }
}
/// Fixed Classic compressor parameters, with one continuous linked-stereo
/// detector/envelope and the exact integer 6ms predelay ring. Arbitrary caller
/// blocks retain the original 32-frame envelope update phase.
pub struct ClassicCompressor {
    delay: PcmArray,
    read: usize,
    write: usize,
    delay_frames: usize,
    detector: f32,
    gain: f32,
    max_attack_diff: f32,
    division_remaining: usize,
    desired: f32,
    envelope: f32,
    threshold: f32,
    knee_db: f32,
    post_gain: f32,
    attack_frames: f32,
    saturation_frames: f32,
    release: [f32; 5],
}
impl ClassicCompressor {
    pub fn new(sample_rate: u32, memory: &PcmMemory) -> Result<Self> {
        if !(8000..=192000).contains(&sample_rate) {
            return Err(invalid(
                "Native compressor sample rate is outside admission",
            ));
        }
        let rate = sample_rate as f32;
        let zones = [0.09f32, 0.16f32, 0.42f32, 0.98f32];
        let [z1, z2, z3, z4] = zones;
        let bases = [
            0.9999999999999998f32 * z1 + 1.8432219684323923e-16f32 * z2
                - 1.9373394351676423e-16f32 * z3
                + 8.824516011816245e-18f32 * z4,
            -1.5788320352845888f32 * z1 + 2.3305837032074286f32 * z2 - 0.9141194204840429f32 * z3
                + 0.1623677525612032f32 * z4,
            0.5334142869106424f32 * z1 - 1.272736789213631f32 * z2 + 0.9258856042207512f32 * z3
                - 0.18656310191776226f32 * z4,
            0.08783463138207234f32 * z1 - 0.1694162967925622f32 * z2 + 0.08588057951595272f32 * z3
                - 0.00429891410546283f32 * z4,
            -0.042416883008123074f32 * z1 + 0.1115693827987602f32 * z2
                - 0.09764676325265872f32 * z3
                + 0.028494263462021576f32 * z4,
        ];
        let threshold = linear(-1.0);
        let knee_db = db(threshold);
        let saturate_one = linear(knee_db + 0.05 * (0.0 - (-1.0)));
        let delay_frames = ((0.006f32 * rate) as usize).min(1023);
        Ok(Self {
            delay: memory.array(2048)?,
            read: 0,
            write: delay_frames,
            delay_frames,
            detector: 0.0,
            gain: 1.0,
            max_attack_diff: -1.0,
            division_remaining: 0,
            desired: 0.0,
            envelope: 0.0,
            threshold,
            knee_db,
            post_gain: (1.0 / saturate_one).powf(0.6),
            attack_frames: 0.001f32 * rate,
            saturation_frames: 0.0025f32 * rate,
            release: bases.map(|b| (rate * 0.12f32) * b),
        })
    }
    pub fn lookahead_frames(&self) -> usize {
        self.delay_frames
    }
    fn begin_division(&mut self) {
        self.detector = finite(self.detector, 1.0);
        self.desired = self.detector.asin() / HALF_PI;
        let releasing = self.desired > self.gain;
        let difference = if self.desired == 0.0 {
            if releasing { -1.0 } else { 1.0 }
        } else {
            db(self.gain / self.desired)
        };
        self.envelope = if releasing {
            self.max_attack_diff = -1.0;
            let x = 0.25 * (finite(difference, -1.0).clamp(-12.0, 0.0) + 12.0);
            let x2 = x * x;
            let x3 = x2 * x;
            let x4 = x2 * x2;
            let [a, b, c, d, e] = self.release;
            let frames = a + b * x + c * x2 + d * x3 + e * x4;
            linear(5.0 / frames)
        } else {
            let difference = finite(difference, 1.0);
            if self.max_attack_diff == -1.0 || self.max_attack_diff < difference {
                self.max_attack_diff = difference;
            }
            let x = 0.25 / self.max_attack_diff.max(0.5);
            1.0 - x.powf(1.0 / self.attack_frames)
        };
        self.division_remaining = 32;
    }
    /// Apply the native compressor and fixed output gain in place. Export's
    /// final hard clip is a distinct retained stage; real-time preview does not
    /// silently add export-only clipping.
    pub fn process(
        &mut self,
        pcm: &mut [f32],
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        if pcm.len() % 2 != 0 {
            return Err(invalid("Native compressor requires interleaved stereo"));
        }
        for pair in pcm.chunks_exact_mut(2) {
            if self.division_remaining == 0 {
                checkpoint()?;
                self.begin_division();
            }
            if pair.iter().any(|v| !v.is_finite()) {
                return Err(invalid("Native compressor received non-finite PCM"));
            }
            let input = pair[0].abs().max(pair[1].abs());
            for (c, value) in pair.iter().enumerate() {
                self.delay.as_mut_slice()[self.write * 2 + c] = *value;
            }
            let shaped = if input < self.threshold {
                input
            } else {
                linear(self.knee_db + 0.05 * (db(input) - (-1.0)))
            };
            let attenuation = if input <= 0.0001 { 1.0 } else { shaped / input };
            let release_rate = linear((-db(attenuation)).max(2.0) / self.saturation_frames) - 1.0;
            let rate = if attenuation > self.detector {
                release_rate
            } else {
                1.0
            };
            self.detector = finite(
                (self.detector + (attenuation - self.detector) * rate).min(1.0),
                1.0,
            );
            if self.envelope < 1.0 {
                self.gain += (self.desired - self.gain) * self.envelope;
            } else {
                self.gain = (self.gain * self.envelope).min(1.0);
            }
            let warped = (f64::from(HALF_PI * self.gain).sin()) as f32;
            let gain = self.post_gain * warped;
            for (c, value) in pair.iter_mut().enumerate() {
                *value = (self.delay.as_slice()[self.read * 2 + c] * gain) * HEADROOM;
            }
            self.read = (self.read + 1) & 1023;
            self.write = (self.write + 1) & 1023;
            self.division_remaining -= 1;
            if self.division_remaining == 0 {
                self.detector = normal(self.detector);
                self.gain = normal(self.gain);
            }
        }
        checkpoint()
    }
}
/// Export peak gating uses the JS Number threshold (0.98 as f64), not its
/// rounded Float32 representation. A stored Float32 0.98 is slightly larger.
pub fn mastering_required(pcm: &[f32]) -> Result<(f64, bool)> {
    let mut peak = 0.0f64;
    for value in pcm {
        if !value.is_finite() {
            return Err(invalid("Native mastering received non-finite PCM"));
        }
        peak = peak.max(f64::from(value.abs()));
    }
    Ok((peak, peak > 0.98))
}
pub fn clip_export(pcm: &mut [f32]) {
    for value in pcm {
        *value = value.clamp(-HEADROOM, HEADROOM);
    }
}
