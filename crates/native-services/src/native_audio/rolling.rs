//! Ordinary realtime PCM retains observed timestamps. Rust's application port
//! supplies every source interval; this owner decodes, evicts and reports only.
use super::{
    dsp::*,
    input::NativeInput,
    invalid,
    pcm::{Samples, Window},
    source::{self, DecodeConfig},
};
use opencut_composition::scene::{PreviewAudioSegment, PreviewSourceWindow};
use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::{ClassicAudioResources, RollingPreviewAdmission};
type Result<T> = std::result::Result<T, EditorError>;
type Key = (u64, usize);
struct Piece {
    timestamp: f64,
    decoder_epoch: u64,
    data: Window,
    _metadata: PcmReservation,
}
impl Piece {
    fn key(&self) -> Key {
        (self.timestamp.to_bits(), self.data.samples.frames())
    }
    fn end(&self, rate: u32) -> f64 {
        self.timestamp + self.data.samples.frames() as f64 / f64::from(rate)
    }
    fn intersects(&self, windows: &[&PreviewSourceWindow], rate: u32) -> bool {
        windows
            .iter()
            .any(|w| self.timestamp < w.end_seconds && self.end(rate) > w.start_seconds)
    }
}
pub(super) struct RollingSource {
    input: NativeInput,
    admission: RollingPreviewAdmission,
    pieces: Vec<Piece>,
    tail: Option<Key>,
    origin: Option<f64>,
    last_start: f64,
    through: f64,
    evicted_through: f64,
    last_decoded_end: f64,
    ended: bool,
    previous: Vec<(f64, f64)>,
    resident: u64,
    memory: PcmMemory,
    decoder_epoch: u64,
    _metadata: PcmReservation,
}
impl RollingSource {
    pub fn new(
        input: &mut Option<NativeInput>,
        admission: RollingPreviewAdmission,
        target: u32,
        memory: &PcmMemory,
    ) -> Result<Self> {
        let observed = input
            .as_ref()
            .ok_or_else(|| invalid("Native preview input was already transferred"))?;
        if admission.source_sample_rate != observed.track.sample_rate
            || admission.target_sample_rate != target
            || admission.max_channels != observed.track.channels
            || !(1..=2).contains(&admission.max_channels)
            || !(1..=65536).contains(&admission.max_chunk_frames)
            || admission.working_frames < u64::from(admission.max_chunk_frames) * 2
        {
            return Err(invalid(
                "Rolling admission differs from its fixed primary source",
            ));
        }
        let metadata = memory.reserve(
            (4 * std::mem::size_of::<Self>() + 64 * std::mem::size_of::<(f64, f64)>()) as u64,
        )?;
        Ok(Self {
            input: input.take().expect("Validated input owner"),
            admission,
            pieces: Vec::new(),
            tail: None,
            origin: None,
            last_start: 0.0,
            through: f64::NEG_INFINITY,
            evicted_through: 0.0,
            last_decoded_end: f64::NEG_INFINITY,
            ended: false,
            previous: Vec::new(),
            resident: 0,
            memory: memory.clone(),
            decoder_epoch: 0,
            _metadata: metadata,
        })
    }
    pub fn asset_id(&self) -> &str {
        &self.input.resource.asset_id
    }
    pub fn sample_rate(&self) -> u32 {
        self.input.track.sample_rate
    }
    pub fn channels(&self) -> u32 {
        self.input.track.channels
    }
    fn drop_at(&mut self, index: usize) {
        let piece = self.pieces.remove(index);
        self.resident -= piece.data.samples.frames() as u64;
        self.evicted_through = self.evicted_through.max(piece.end(self.sample_rate()));
    }
    fn retain(&mut self, windows: &[&PreviewSourceWindow], keep_tail: bool) {
        let rate = self.sample_rate();
        for index in (0..self.pieces.len()).rev() {
            let piece = &self.pieces[index];
            if (!keep_tail || Some(piece.key()) != self.tail) && !piece.intersects(windows, rate) {
                self.drop_at(index);
            }
        }
    }
    fn restart<R: ClassicAudioResources + ?Sized>(
        &mut self,
        resources: &mut R,
        start: f64,
        windows: &[&PreviewSourceWindow],
        config: &DecodeConfig,
    ) -> Result<()> {
        self.retain(windows, false);
        self.input.restart(resources, start, config)?;
        self.decoder_epoch = self
            .decoder_epoch
            .checked_add(1)
            .ok_or_else(|| invalid("Native rolling decoder epoch overflow"))?;
        self.origin = Some(start);
        self.through = start;
        self.ended = false;
        self.tail = None;
        self.evicted_through = start;
        self.last_decoded_end = f64::NEG_INFINITY;
        Ok(())
    }
    pub fn ensure<R: ClassicAudioResources + ?Sized>(
        &mut self,
        resources: &mut R,
        all_windows: &[PreviewSourceWindow],
        config: &DecodeConfig,
    ) -> Result<Vec<PreviewAudioSegment>> {
        source::check(resources, config.deadline)?;
        let windows: Vec<_> = all_windows
            .iter()
            .filter(|w| w.asset_id == self.asset_id())
            .collect();
        if windows.len() > 32
            || windows.iter().any(|w| {
                !w.start_seconds.is_finite()
                    || !w.end_seconds.is_finite()
                    || w.start_seconds < 0.0
                    || w.end_seconds <= w.start_seconds
            })
        {
            return Err(invalid("Invalid Rust-planned native audio intervals"));
        }
        // Clear only samples no longer borrowed by this command. One bounded
        // read-ahead tail survives until a later iterator result replaces it.
        self.retain(&windows, true);
        for window in &windows {
            source::check(resources, config.deadline)?;
            if self
                .previous
                .iter()
                .any(|(start, end)| *start <= window.start_seconds && *end >= window.end_seconds)
            {
                continue;
            }
            let chunk_seconds =
                f64::from(self.admission.max_chunk_frames) / f64::from(self.sample_rate());
            if self
                .origin
                .is_none_or(|origin| window.start_seconds < origin)
                || window.start_seconds < self.last_start
                || window.start_seconds < self.evicted_through
                || window.start_seconds > self.through + chunk_seconds
            {
                self.restart(resources, window.start_seconds, &windows, config)?;
            }
            self.last_start = window.start_seconds;
            while !self.ended && self.through < window.end_seconds {
                let Some(frame) = self.input.read_preview(resources, config)? else {
                    self.ended = true;
                    self.through = f64::INFINITY;
                    break;
                };
                let frames = frame.frames();
                if frames == 0
                    || frames > self.admission.max_chunk_frames as usize
                    || frame.channels.len() != self.channels() as usize
                {
                    return Err(invalid(
                        "Native rolling frame exceeds its admitted primary chunk",
                    ));
                }
                let pts = frame
                    .pts
                    .ok_or_else(|| invalid("Native rolling audio has no observed PTS"))?;
                let timestamp = pts as f64 * f64::from(frame.time_base_numerator)
                    / f64::from(frame.time_base_denominator);
                let next = timestamp + frames as f64 / f64::from(self.sample_rate());
                if !timestamp.is_finite() || !next.is_finite() || next <= self.last_decoded_end {
                    return Err(invalid(
                        "Native rolling audio did not advance its observed interval",
                    ));
                }
                let key = (timestamp.to_bits(), frames);
                // A seek reconstructs codec state. Compressed formats such as
                // AAC can emit different priming samples at the same PTS when
                // reached from another decode origin. Replace that observation
                // rather than mixing the old and new decoder contexts. Other
                // currently borrowed windows remain retained and accounted.
                if let Some(index) = self
                    .pieces
                    .iter()
                    .position(|p| p.key() == key && p.decoder_epoch != self.decoder_epoch)
                {
                    // Replacement keeps this interval resident. It must not
                    // advance the missing-data frontier, which would force a
                    // second seek (and a fresh codec priming state) next block.
                    let replaced = self.pieces.remove(index);
                    self.resident -= replaced.data.samples.frames() as u64;
                }
                if let Some(old) = self.pieces.iter().find(|p| p.key() == key) {
                    for (c, channel) in frame.channels.iter().enumerate() {
                        if channel.iter().enumerate().any(|(i, value)| {
                            value.to_bits() != old.data.samples.value(c, i).to_bits()
                        }) {
                            return Err(invalid("Repeated native audio interval changed its PCM"));
                        }
                    }
                } else {
                    if let Some(tail) = self.tail {
                        if let Some(index) = self.pieces.iter().position(|p| {
                            p.key() == tail && !p.intersects(&windows, self.sample_rate())
                        }) {
                            self.drop_at(index);
                        }
                    }
                    if self.pieces.len() >= 65536
                        || self
                            .resident
                            .checked_add(frames as u64)
                            .is_none_or(|n| n > self.admission.working_frames)
                    {
                        return Err(EditorError::new(
                            "AUDIO_BUDGET_EXCEEDED",
                            "Current native decoder intervals exceed resident admission",
                        ));
                    }
                    let metadata = self
                        .memory
                        .reserve((4 * std::mem::size_of::<Piece>()) as u64)?;
                    let mut planes = Vec::with_capacity(frame.channels.len());
                    for values in &frame.channels {
                        let mut plane = self.memory.array(frames)?;
                        plane.as_mut_slice().copy_from_slice(values);
                        planes.push(plane);
                    }
                    self.pieces.push(Piece {
                        timestamp,
                        decoder_epoch: self.decoder_epoch,
                        data: Window::new(0, Samples::Planar(planes), &self.memory)?,
                        _metadata: metadata,
                    });
                    self.resident += frames as u64;
                }
                self.tail = Some(key);
                self.last_decoded_end = next;
                self.through = self.through.max(next);
            }
        }
        self.previous = windows
            .iter()
            .map(|w| (w.start_seconds, w.end_seconds))
            .collect();
        self.pieces
            .sort_by(|a, b| a.timestamp.total_cmp(&b.timestamp));
        Ok(self
            .pieces
            .iter()
            .enumerate()
            .map(|(id, p)| PreviewAudioSegment {
                segment_id: id as u32,
                timestamp_seconds: p.timestamp,
                frame_count: p.data.samples.frames() as u64,
            })
            .collect())
    }
    pub fn sample(&self, segment: u32, channel: u32, frame: u64) -> Result<f32> {
        let piece = self
            .pieces
            .get(segment as usize)
            .ok_or_else(|| invalid("Compiled native preview segment is absent"))?;
        if channel >= self.channels() || frame >= piece.data.samples.frames() as u64 {
            return Err(invalid(
                "Compiled native preview tap exceeds its actual segment",
            ));
        }
        Ok(piece.data.samples.value(channel as usize, frame as usize))
    }
    pub fn close<R: ClassicAudioResources + ?Sized>(self, resources: &mut R) -> Result<()> {
        self.input.close(resources)
    }
}
