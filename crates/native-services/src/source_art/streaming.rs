//! Bounded source waveform preparation. Input PTS are deliberately absent:
//! buckets describe concatenated decoded samples, across every source channel.
//! Projection, authorization and placement clocks remain in the application.
use super::{error, protocol};
use crate::native_audio::dsp::{PcmArray, PcmMemory, PcmReservation, resample_preparation_at};
use opencut_contracts::EditorError;
use std::collections::VecDeque;
type Result<T> = std::result::Result<T, EditorError>;
const EXACT_MAX: u64 = (1u64 << 53) - 1;
fn budget(message: &str) -> EditorError {
    error("SOURCE_ART_BUDGET_EXCEEDED", message)
}
fn native_error(value: EditorError) -> EditorError {
    if value.code == "AUDIO_BUDGET_EXCEEDED" {
        budget(&value.message)
    } else {
        value
    }
}
struct Chunk {
    start: u64,
    frames: usize,
    channels: Vec<PcmArray>,
    _metadata: PcmReservation,
}
pub(super) struct WaveformStream {
    source_rate: u32,
    target_rate: u32,
    channels: u32,
    max_chunk_frames: usize,
    max_resident_frames: u64,
    max_output_bytes: u64,
    block_frames: usize,
    native_total: u64,
    target_total: u64,
    phase: f64,
    resident: u64,
    output_bytes: u64,
    chunks: VecDeque<Chunk>,
    peaks: PcmArray,
    memory: PcmMemory,
    closed: bool,
    _metadata: PcmReservation,
}
impl WaveformStream {
    pub fn new(
        source_rate: u32,
        target_rate: u32,
        channels: u32,
        max_chunk_frames: usize,
        max_resident_frames: u64,
        max_output_bytes: u64,
        memory: PcmMemory,
    ) -> Result<Self> {
        if !(1..=384000).contains(&source_rate)
            || ![44100, 48000].contains(&target_rate)
            || !(1..=64).contains(&channels)
            || !(1..=65536).contains(&max_chunk_frames)
            || max_resident_frames < max_chunk_frames as u64
            || max_resident_frames > EXACT_MAX
            || max_output_bytes < 4
        {
            return Err(protocol("Invalid native source waveform admission"));
        }
        let ratio = f64::from(source_rate) / f64::from(target_rate);
        // Preserve the retained stream's block/guard policy and operation
        // order, including the 128-sample consumer alignment.
        let available = max_resident_frames as f64 - max_chunk_frames as f64 * 2.0 - 128.0;
        let blocks = (available / (2.0 * ratio + 2.0) / 128.0).floor();
        if blocks < 1.0 {
            return Err(budget("No guarded native waveform block fits admission"));
        }
        let block_frames = (blocks * 128.0).min(65536.0) as usize;
        let metadata = memory
            .reserve(std::mem::size_of::<Self>() as u64)
            .map_err(native_error)?;
        let peaks = memory.array(block_frames / 128).map_err(native_error)?;
        Ok(Self {
            source_rate,
            target_rate,
            channels,
            max_chunk_frames,
            max_resident_frames,
            max_output_bytes,
            block_frames,
            native_total: 0,
            target_total: 0,
            phase: 0.0,
            resident: 0,
            output_bytes: 0,
            chunks: VecDeque::new(),
            peaks,
            memory,
            closed: false,
            _metadata: metadata,
        })
    }
    pub fn push<T: AsRef<[f32]>>(
        &mut self,
        planes: &[T],
        checkpoint: &mut dyn FnMut() -> Result<()>,
        emit: &mut dyn FnMut(&[f32]) -> Result<()>,
    ) -> Result<()> {
        let result = self.push_inner(planes, checkpoint, emit);
        if result.is_err() {
            self.closed = true;
            self.chunks.clear();
        }
        result
    }
    fn push_inner<T: AsRef<[f32]>>(
        &mut self,
        planes: &[T],
        checkpoint: &mut dyn FnMut() -> Result<()>,
        emit: &mut dyn FnMut(&[f32]) -> Result<()>,
    ) -> Result<()> {
        checkpoint()?;
        let frames = planes.first().map_or(0, |p| p.as_ref().len());
        if self.closed
            || planes.len() != self.channels as usize
            || frames == 0
            || frames > self.max_chunk_frames
            || planes
                .iter()
                .any(|p| p.as_ref().len() != frames || p.as_ref().iter().any(|v| !v.is_finite()))
        {
            return Err(protocol(
                "Native source waveform changed decoded chunk format",
            ));
        }
        let end = self
            .native_total
            .checked_add(frames as u64)
            .filter(|n| *n <= EXACT_MAX)
            .ok_or_else(|| budget("Native waveform extent overflow"))?;
        if self
            .resident
            .checked_add(frames as u64)
            .is_none_or(|n| n > self.max_resident_frames)
        {
            return Err(budget("Native waveform retained chunks exceed admission"));
        }
        // Vec/VecDeque growth may hold old and replacement allocations at
        // once. Charge four descriptors per live entry, including first growth.
        let metadata = self
            .memory
            .reserve(
                (4 * std::mem::size_of::<Chunk>()
                    + 4 * planes.len() * std::mem::size_of::<PcmArray>()) as u64,
            )
            .map_err(native_error)?;
        let mut channels = Vec::new();
        channels
            .try_reserve_exact(planes.len())
            .map_err(|_| budget("Cannot allocate waveform channel owners"))?;
        for plane in planes {
            let mut values = self.memory.array(frames).map_err(native_error)?;
            values.as_mut_slice().copy_from_slice(plane.as_ref());
            channels.push(values);
        }
        self.chunks
            .try_reserve(1)
            .map_err(|_| budget("Cannot retain waveform chunk descriptor"))?;
        self.chunks.push_back(Chunk {
            start: self.native_total,
            frames,
            channels,
            _metadata: metadata,
        });
        self.native_total = end;
        self.resident += frames as u64;
        loop {
            let next = self.next_phase(self.block_frames);
            if next.ceil() + 32.0 > self.native_total as f64 {
                break;
            }
            self.render(self.block_frames, next, false, checkpoint, emit)?;
        }
        checkpoint()
    }
    fn next_phase(&self, frames: usize) -> f64 {
        let mut next = self.phase;
        let ratio = f64::from(self.source_rate) / f64::from(self.target_rate);
        for _ in 0..frames {
            next += ratio;
        }
        next
    }
    fn render(
        &mut self,
        frames: usize,
        next: f64,
        final_block: bool,
        checkpoint: &mut dyn FnMut() -> Result<()>,
        emit: &mut dyn FnMut(&[f32]) -> Result<()>,
    ) -> Result<()> {
        checkpoint()?;
        let peak_count = frames.div_ceil(128);
        if frames == 0
            || frames > self.block_frames
            || peak_count > self.peaks.len()
            || self
                .output_bytes
                .checked_add(peak_count as u64 * 4)
                .is_none_or(|n| n > self.max_output_bytes)
        {
            return Err(budget("Native waveform summary exceeds output admission"));
        }
        self.peaks.as_mut_slice()[..peak_count].fill(0.0);
        let from = (self.phase.floor() as u64).saturating_sub(32);
        let to = self
            .native_total
            .min((next.ceil() as u64).saturating_add(32));
        if self.phase < self.native_total as f64 {
            if to <= from
                || (!final_block && to < (next.ceil() as u64).saturating_add(32))
                || self
                    .resident
                    .checked_add(to - from)
                    .and_then(|n| n.checked_add(frames as u64 * 2))
                    .is_none_or(|n| n > self.max_resident_frames)
            {
                return Err(budget("Native waveform guards exceed resident admission"));
            }
            let count =
                usize::try_from(to - from).map_err(|_| budget("Native waveform guard overflow"))?;
            let mut native = self.memory.array(count).map_err(native_error)?;
            let mut target = self.memory.array(frames).map_err(native_error)?;
            for channel in 0..self.channels as usize {
                checkpoint()?;
                let mut covered = 0;
                for chunk in &self.chunks {
                    let begin = from.max(chunk.start);
                    let end = to.min(chunk.start + chunk.frames as u64);
                    if end <= begin {
                        continue;
                    }
                    if begin != from + covered as u64 {
                        return Err(protocol("Native waveform lost a source guard"));
                    }
                    let length = (end - begin) as usize;
                    native.as_mut_slice()[covered..covered + length].copy_from_slice(
                        &chunk.channels[channel].as_slice()
                            [(begin - chunk.start) as usize..(end - chunk.start) as usize],
                    );
                    covered += length;
                }
                if covered != count {
                    return Err(protocol("Native waveform source guard is incomplete"));
                }
                if self.source_rate == self.target_rate {
                    target.as_mut_slice().fill(0.0);
                    let start = (self.phase - from as f64) as usize;
                    let length = frames.min(count.saturating_sub(start));
                    target.as_mut_slice()[..length]
                        .copy_from_slice(&native.as_slice()[start..start + length]);
                } else {
                    resample_preparation_at(
                        native.as_slice(),
                        target.as_mut_slice(),
                        self.source_rate,
                        self.target_rate,
                        self.phase - from as f64,
                        checkpoint,
                    )
                    .map_err(native_error)?;
                }
                for (index, value) in target.as_slice().iter().enumerate() {
                    let peak = &mut self.peaks.as_mut_slice()[index / 128];
                    *peak = peak.max(value.abs());
                }
            }
        }
        emit(&self.peaks.as_slice()[..peak_count])?;
        checkpoint()?;
        self.output_bytes += peak_count as u64 * 4;
        self.target_total = self
            .target_total
            .checked_add(frames as u64)
            .filter(|n| *n <= EXACT_MAX)
            .ok_or_else(|| budget("Native waveform target overflow"))?;
        self.phase = next;
        let discard_before = (self.phase.floor() as u64).saturating_sub(32);
        while self
            .chunks
            .front()
            .is_some_and(|c| c.start + c.frames as u64 <= discard_before)
        {
            self.resident -= self.chunks.pop_front().unwrap().frames as u64;
        }
        Ok(())
    }
    pub fn finish(
        &mut self,
        checkpoint: &mut dyn FnMut() -> Result<()>,
        emit: &mut dyn FnMut(&[f32]) -> Result<()>,
    ) -> Result<(u64, u64)> {
        if self.closed {
            return Err(protocol("Native waveform stream has already retired"));
        }
        let result = (|| {
            checkpoint()?;
            let extent = (self.native_total as f64
                * (f64::from(self.target_rate) / f64::from(self.source_rate)))
            .ceil();
            if !extent.is_finite() || extent > EXACT_MAX as f64 {
                return Err(budget("Native waveform decoded extent overflow"));
            }
            let extent = extent as u64;
            while self.target_total < extent {
                let frames = (extent - self.target_total).min(self.block_frames as u64) as usize;
                self.render(frames, self.next_phase(frames), true, checkpoint, emit)?;
            }
            if extent == 0 {
                emit(&[0.0])?;
                self.output_bytes = 4;
            }
            if self.output_bytes != extent.div_ceil(128).max(1) * 4 {
                return Err(protocol(
                    "Native waveform summary differs from observed EOF",
                ));
            }
            checkpoint()?;
            Ok((self.native_total, extent))
        })();
        self.closed = true;
        self.chunks.clear();
        result
    }
}

#[cfg(test)]
#[path = "streaming_tests.rs"]
mod tests;
