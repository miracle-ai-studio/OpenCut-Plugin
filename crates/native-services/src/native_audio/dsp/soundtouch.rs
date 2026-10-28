// SoundTouchJS 0.3.0 native adaptation, LGPL-2.1-or-later.
// Copyright (c) Olli Parviainen, Ryan Berdeen, Jakub Fiala,
// Steve 'Cutter' Blades. Original notice/source identities and complete license
// are retained in vendor/audio and packages/classic-audio-renderer/vendor.
// Preserve Float32 writes, f64 arithmetic, FIFO/filter history and unflushed EOF.
use super::*;

struct Fifo {
    data: PcmArray,
    position: usize,
    frames: usize,
    memory: PcmMemory,
}
impl Fifo {
    fn new(memory: &PcmMemory) -> Result<Self> {
        Ok(Self {
            data: memory.array(0)?,
            position: 0,
            frames: 0,
            memory: memory.clone(),
        })
    }
    fn get(&self, frame: usize, channel: usize) -> Option<f32> {
        self.data
            .as_slice()
            .get(
                (self.position + frame)
                    .checked_mul(2)?
                    .checked_add(channel)?,
            )
            .copied()
    }
    fn get_signed(&self, frame: isize, channel: usize) -> Option<f32> {
        let frame = self.position.checked_add_signed(frame)?;
        self.data
            .as_slice()
            .get(frame.checked_mul(2)?.checked_add(channel)?)
            .copied()
    }
    fn rewind(&mut self) {
        if self.position != 0 {
            self.data
                .as_mut_slice()
                .copy_within(self.position * 2..(self.position + self.frames) * 2, 0);
            self.position = 0;
        }
    }
    fn ensure(&mut self, frames: usize) -> Result<()> {
        let samples = frames
            .checked_mul(2)
            .ok_or_else(|| budget("SoundTouch FIFO overflow"))?;
        if samples > self.data.len() {
            let mut next = self.memory.array(samples)?;
            next.as_mut_slice()[..self.frames * 2].copy_from_slice(
                &self.data.as_slice()[self.position * 2..(self.position + self.frames) * 2],
            );
            self.data = next;
            self.position = 0;
        } else {
            self.rewind();
        }
        Ok(())
    }
    fn put(&mut self, samples: &[f32]) -> Result<()> {
        if samples.len() % 2 != 0 {
            return Err(invalid("SoundTouch requires interleaved stereo"));
        }
        let count = samples.len() / 2;
        self.ensure(
            self.frames
                .checked_add(count)
                .ok_or_else(|| budget("SoundTouch FIFO overflow"))?,
        )?;
        let start = self.frames * 2;
        self.data.as_mut_slice()[start..start + samples.len()].copy_from_slice(samples);
        self.frames += count;
        Ok(())
    }
    fn receive(&mut self, frames: usize) {
        let frames = frames.min(self.frames);
        self.frames -= frames;
        self.position += frames;
    }
}

/// Classic always requests pitch=1 and rate=1. Keep the original unit-rate
/// transposer, including its one-frame history; removing it changes samples.
struct UnitTransposer {
    slope: f64,
    previous: [f32; 2],
}
impl UnitTransposer {
    fn process(&mut self, input: &mut Fifo, output: &mut Fifo) -> Result<()> {
        let frames = input.frames;
        if frames == 0 {
            return Ok(());
        }
        output.ensure(
            output
                .frames
                .checked_add(frames + 1)
                .ok_or_else(|| budget("SoundTouch FIFO overflow"))?,
        )?;
        let start = output.frames * 2;
        let mut produced = 0;
        while self.slope < 1.0 {
            for c in 0..2 {
                let first = f64::from(input.get(0, c).unwrap());
                output.data.as_mut_slice()[start + produced * 2 + c] =
                    ((1.0 - self.slope) * f64::from(self.previous[c]) + self.slope * first) as f32;
            }
            produced += 1;
            self.slope += 1.0;
        }
        self.slope -= 1.0;
        if frames != 1 {
            let mut used = 0;
            'outer: loop {
                while self.slope > 1.0 {
                    self.slope -= 1.0;
                    used += 1;
                    if used >= frames - 1 {
                        break 'outer;
                    }
                }
                for c in 0..2 {
                    output.data.as_mut_slice()[start + produced * 2 + c] = ((1.0 - self.slope)
                        * f64::from(input.get(used, c).unwrap())
                        + self.slope * f64::from(input.get(used + 1, c).unwrap()))
                        as f32;
                }
                produced += 1;
                self.slope += 1.0;
            }
        }
        for c in 0..2 {
            self.previous[c] = input.get(frames - 1, c).unwrap();
        }
        input.receive(frames);
        output.frames += produced;
        Ok(())
    }
}
const SCAN: [&[isize]; 4] = [
    &[
        124, 186, 248, 310, 372, 434, 496, 558, 620, 682, 744, 806, 868, 930, 992, 1054, 1116,
        1178, 1240, 1302, 1364, 1426, 1488,
    ],
    &[-100, -75, -50, -25, 25, 50, 75, 100],
    &[-20, -15, -10, -5, 5, 10, 15, 20],
    &[-4, -3, -2, -1, 1, 2, 3, 4],
];
struct Stretch {
    mid: PcmArray,
    reference: PcmArray,
    window: usize,
    seek: usize,
    nominal_skip: f64,
    skip_fraction: f64,
    required: usize,
}
impl Stretch {
    const OVERLAP: usize = 352; // floor(44100*8ms) rounded down to a multiple of 8.
    fn new(memory: &PcmMemory, tempo: f64) -> Result<Self> {
        let sequence_ms = ((125.0 + 5.0) - 20.0 * tempo).clamp(50.0, 125.0).round();
        let seek_k = (15.0 - 25.0) / (4.0 - 0.25);
        let seek_ms = (25.0 - seek_k * 0.25 + seek_k * tempo)
            .clamp(15.0, 25.0)
            .round();
        let window = (44100.0 * sequence_ms / 1000.0).floor() as usize;
        let seek = (44100.0 * seek_ms / 1000.0).floor() as usize;
        let nominal_skip = tempo * (window - Self::OVERLAP) as f64;
        let required = ((nominal_skip + 0.5).floor() as usize + Self::OVERLAP).max(window) + seek;
        Ok(Self {
            mid: memory.array(Self::OVERLAP * 2)?,
            reference: memory.array(Self::OVERLAP * 2)?,
            window,
            seek,
            nominal_skip,
            skip_fraction: 0.0,
            required,
        })
    }
    fn best_overlap(&mut self, input: &Fifo) -> isize {
        for i in 0..Self::OVERLAP {
            let weight = (i * (Self::OVERLAP - i)) as f64;
            for c in 0..2 {
                self.reference.as_mut_slice()[i * 2 + c] =
                    (f64::from(self.mid.as_slice()[i * 2 + c]) * weight) as f32;
            }
        }
        // JS Number.MIN_VALUE is the smallest positive subnormal, not MIN.
        let mut best_correlation = f64::from_bits(1);
        let mut best = 0;
        let mut center = 0isize;
        for offsets in SCAN {
            for offset in offsets {
                let candidate = center + offset;
                if candidate >= self.seek as isize {
                    break;
                }
                let mut correlation = 0.0;
                for i in 1..Self::OVERLAP {
                    let at = candidate + i as isize;
                    // JS typed-array out-of-bounds values become NaN, and can
                    // never win the comparison. Do not clamp negative offsets.
                    let left = input.get_signed(at, 0);
                    let right = input.get_signed(at, 1);
                    correlation += f64::from(left.unwrap_or(f32::NAN))
                        * f64::from(self.reference.as_slice()[i * 2])
                        + f64::from(right.unwrap_or(f32::NAN))
                            * f64::from(self.reference.as_slice()[i * 2 + 1]);
                }
                if correlation > best_correlation {
                    best_correlation = correlation;
                    best = candidate;
                }
            }
            center = best;
        }
        best
    }
    fn process(
        &mut self,
        input: &mut Fifo,
        output: &mut Fifo,
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        while input.frames >= self.required {
            checkpoint()?;
            let offset = self.best_overlap(input);
            output.ensure(output.frames + Self::OVERLAP)?;
            let start = output.frames * 2;
            let scale = 1.0 / Self::OVERLAP as f64;
            for i in 0..Self::OVERLAP {
                let old = (Self::OVERLAP - i) as f64 * scale;
                let new = i as f64 * scale;
                for c in 0..2 {
                    output.data.as_mut_slice()[start + i * 2 + c] = (f64::from(
                        input
                            .get_signed(offset + i as isize, c)
                            .ok_or_else(|| invalid("SoundTouch overlap exceeds its FIFO"))?,
                    ) * new
                        + f64::from(self.mid.as_slice()[i * 2 + c]) * old)
                        as f32;
                }
            }
            output.frames += Self::OVERLAP;
            let count = self.window - 2 * Self::OVERLAP;
            let base = input
                .position
                .checked_add_signed(offset)
                .ok_or_else(|| invalid("SoundTouch overlap precedes its FIFO"))?;
            let from = (base + Self::OVERLAP) * 2;
            output.put(&input.data.as_slice()[from..from + count * 2])?;
            let from = (base + self.window - Self::OVERLAP) * 2;
            self.mid
                .as_mut_slice()
                .copy_from_slice(&input.data.as_slice()[from..from + Self::OVERLAP * 2]);
            self.skip_fraction += self.nominal_skip;
            let skip = self.skip_fraction.floor() as usize;
            self.skip_fraction -= skip as f64;
            input.receive(skip);
        }
        Ok(())
    }
}

/// Native counterpart of the pinned SoundTouchJS SimpleFilter used by Classic.
/// Input has already been sliced/resampled under application preparation facts.
/// Call extract in 4096-frame blocks, retaining the original unflushed EOF.
pub struct SoundTouch {
    input: Fifo,
    intermediate: Fifo,
    output: Fifo,
    transposer: UnitTransposer,
    stretch: Stretch,
    source_position: usize,
    output_position: usize,
}
impl SoundTouch {
    pub fn new(tempo: f64, memory: &PcmMemory) -> Result<Self> {
        if !tempo.is_finite() || !(0.01..=5.0).contains(&tempo) {
            return Err(invalid(
                "Native SoundTouch tempo is outside Classic admission",
            ));
        }
        Ok(Self {
            input: Fifo::new(memory)?,
            intermediate: Fifo::new(memory)?,
            output: Fifo::new(memory)?,
            transposer: UnitTransposer {
                slope: 0.0,
                previous: [0.0; 2],
            },
            stretch: Stretch::new(memory, tempo)?,
            source_position: 0,
            output_position: 0,
        })
    }
    pub fn extract(
        &mut self,
        source: &[f32],
        target: &mut [f32],
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<usize> {
        if source.len() % 2 != 0 || target.len() % 2 != 0 || target.len() > 4096 * 2 {
            return Err(invalid("Native SoundTouch requires bounded stereo blocks"));
        }
        let needed = self.output_position + target.len() / 2;
        while self.output.frames < needed {
            checkpoint()?;
            let wanted = 16384usize.saturating_sub(self.input.frames);
            let available = (source.len() / 2)
                .saturating_sub(self.source_position)
                .min(wanted);
            let from = self.source_position * 2;
            self.input.put(&source[from..from + available * 2])?;
            self.source_position += available;
            if self.input.frames < 16384 {
                break;
            }
            self.transposer
                .process(&mut self.input, &mut self.intermediate)?;
            self.stretch
                .process(&mut self.intermediate, &mut self.output, checkpoint)?;
        }
        let frames = (target.len() / 2).min(self.output.frames - self.output_position);
        let from = (self.output.position + self.output_position) * 2;
        target[..frames * 2].copy_from_slice(&self.output.data.as_slice()[from..from + frames * 2]);
        let current = self.output_position + frames;
        self.output_position = current.min(22050);
        self.output.receive(current.saturating_sub(22050));
        Ok(frames)
    }
}

/// Match the pinned OfflineAudioContext ScriptProcessor preparation, including
/// its two 4096-frame output buffers. This latency is measured by the independent
/// browser oracle; it is not inferred from a nominal processor buffer size.
pub fn render_preserved_pitch(
    source: &[f32],
    tempo: f64,
    output_frames: usize,
    memory: &PcmMemory,
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> Result<PcmArray> {
    if output_frames == 0 {
        return Err(invalid("Prepared pitch output is empty"));
    }
    let samples = output_frames
        .checked_mul(2)
        .ok_or_else(|| budget("Prepared pitch output overflow"))?;
    let mut output = memory.array(samples)?;
    let mut processor = SoundTouch::new(tempo, memory)?;
    let mut scratch = memory.array(8192)?;
    for start in (8192..output_frames).step_by(4096) {
        checkpoint()?;
        let frames = processor.extract(source, scratch.as_mut_slice(), checkpoint)?;
        let count = frames.min(output_frames - start);
        output.as_mut_slice()[start * 2..(start + count) * 2]
            .copy_from_slice(&scratch.as_slice()[..count * 2]);
    }
    checkpoint()?;
    Ok(output)
}
