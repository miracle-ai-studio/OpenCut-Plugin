//! Owned dense/sparse PCM, separate from authored source clocks. Missing
//! in-bounds windows are errors; only an actual decoded EOF supplies silence.
use super::{dsp::*, invalid};
use opencut_contracts::EditorError;
type Result<T> = std::result::Result<T, EditorError>;

pub(crate) enum Samples {
    Planar(Vec<PcmArray>),
    Stereo(PcmArray),
}
impl Samples {
    pub fn frames(&self) -> usize {
        match self {
            Self::Planar(p) => p.first().map_or(0, PcmArray::len),
            Self::Stereo(p) => p.len() / 2,
        }
    }
    fn channels(&self) -> u32 {
        match self {
            Self::Planar(p) => p.len() as u32,
            Self::Stereo(_) => 2,
        }
    }
    fn value(&self, channel: usize, frame: usize) -> f32 {
        match self {
            Self::Planar(p) => p[channel].as_slice()[frame],
            Self::Stereo(p) => p.as_slice()[frame * 2 + channel],
        }
    }
}
pub(crate) struct Window {
    pub start: u64,
    pub samples: Samples,
    _metadata: PcmReservation,
}
impl Window {
    pub fn new(start: u64, samples: Samples, memory: &PcmMemory) -> Result<Self> {
        // Vec's first allocation can hold four windows, then grows by doubling.
        // Charge its complete upper bound before pushing this owner. Channel
        // descriptors are charged in addition to every owned Float32 array.
        let planes = match &samples {
            Samples::Planar(p) => p.capacity() * std::mem::size_of::<PcmArray>(),
            Samples::Stereo(_) => 0,
        };
        let metadata = memory.reserve((4 * std::mem::size_of::<Self>() + planes) as u64)?;
        Ok(Self {
            start,
            samples,
            _metadata: metadata,
        })
    }
}
pub(crate) struct PcmSource {
    pub sample_rate: u32,
    pub frames: u64,
    pub channels: u32,
    pub sparse: bool,
    windows: Vec<Window>,
    _metadata: PcmReservation,
}
impl PcmSource {
    pub fn new(
        rate: u32,
        frames: u64,
        channels: u32,
        sparse: bool,
        windows: Vec<Window>,
        memory: &PcmMemory,
    ) -> Result<Self> {
        if rate == 0
            || rate > 384000
            || frames == 0
            || frames > (1u64 << 53) - 1
            || channels == 0
            || channels > 64
        {
            return Err(invalid("Invalid native PCM format"));
        }
        let mut end = 0;
        for w in &windows {
            if w.samples.channels() != channels
                || w.samples.frames() == 0
                || w.start < end
                || (!sparse && w.start != end)
                || w.start
                    .checked_add(w.samples.frames() as u64)
                    .is_none_or(|n| n > frames)
                || matches!(&w.samples, Samples::Planar(p) if p.iter().any(|c| c.len() != w.samples.frames()))
            {
                return Err(invalid("Native PCM windows changed their admitted format"));
            }
            end = w.start + w.samples.frames() as u64;
        }
        if !sparse && (windows.is_empty() || windows[0].start != 0 || end != frames) {
            return Err(invalid(
                "Dense native PCM does not cover its decoded extent",
            ));
        }
        Ok(Self {
            sample_rate: rate,
            frames,
            channels,
            sparse,
            windows,
            _metadata: memory.reserve(
                std::mem::size_of::<Self>() as u64 + 2 * std::mem::size_of::<usize>() as u64,
            )?,
        })
    }
    pub fn dense(rate: u32, samples: Samples) -> Result<Self> {
        let memory = match &samples {
            Samples::Planar(p) => p
                .first()
                .map(PcmArray::memory)
                .ok_or_else(|| invalid("Dense PCM has no channel owner"))?,
            Samples::Stereo(p) => p.memory(),
        };
        Self::new(
            rate,
            samples.frames() as u64,
            samples.channels(),
            false,
            vec![Window::new(0, samples, &memory)?],
            &memory,
        )
    }
    pub fn resident_frames(&self) -> u64 {
        self.windows.iter().map(|w| w.samples.frames() as u64).sum()
    }
    pub fn value(&self, channel: u32, frame: u64) -> Result<f32> {
        if channel >= self.channels {
            return Err(invalid("Native PCM channel is outside admission"));
        }
        if frame >= self.frames {
            return Ok(0.0);
        }
        let at = self.windows.partition_point(|w| w.start <= frame);
        let w = at
            .checked_sub(1)
            .and_then(|n| self.windows.get(n))
            .filter(|w| frame - w.start < w.samples.frames() as u64)
            .ok_or_else(|| invalid("Compiled sample is outside admitted native PCM windows"))?;
        let value = w
            .samples
            .value(channel as usize, (frame - w.start) as usize);
        if !value.is_finite() {
            return Err(invalid("Native PCM contains a non-finite sample"));
        }
        Ok(value)
    }
    pub fn linear(
        &self,
        channel: u32,
        lower: u64,
        numerator: u128,
        denominator: u128,
    ) -> Result<f64> {
        if denominator == 0 || numerator >= denominator {
            return Err(invalid(
                "Native interpolation requires a canonical fraction",
            ));
        }
        // Compare as an integer before any f64 conversion, including huge trims.
        if lower >= self.frames {
            return Ok(0.0);
        }
        let fraction = numerator as f64 / denominator as f64;
        let a = f64::from(self.value(channel, lower)?);
        let b = f64::from(self.value(channel, (lower + 1).min(self.frames - 1))?);
        Ok(a * (1.0 - fraction) + b * fraction)
    }
    pub fn slice_channel(&self, channel: u32, start: u64, output: &mut [f32]) -> Result<()> {
        for (i, v) in output.iter_mut().enumerate() {
            *v = self.value(
                channel,
                start
                    .checked_add(i as u64)
                    .ok_or_else(|| invalid("Native PCM slice overflow"))?,
            )?;
        }
        Ok(())
    }
}
