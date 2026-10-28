//! Native source observation and bounded concat PCM. The application supplies
//! decoder/window admission; timestamps remain observations, never an export
//! placement clock. Ordinary realtime PTS consumers use their separate port.
use super::{AudioSession, callback, dsp::*, input::NativeInput, invalid, pcm::*};
use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::*;
use opencut_media::{MediaError, native_decode::*};
use serde::Deserialize;
use serde_json::json;
use std::time::Instant;
type Result<T> = std::result::Result<T, EditorError>;

pub(crate) struct Source<'a, R: ClassicAudioResources + ?Sized> {
    pub resources: &'a mut R,
    pub transfer: &'a str,
    pub size: u64,
}
fn port_error(e: EditorError) -> MediaError {
    MediaError::Admission {
        code: e.code,
        message: e.message,
    }
}
impl<R: ClassicAudioResources + ?Sized> ByteSource for Source<'_, R> {
    fn len(&self) -> u64 {
        self.size
    }
    fn checkpoint(&mut self) -> std::result::Result<(), MediaError> {
        self.resources.checkpoint().map_err(port_error)
    }
    fn read_at(
        &mut self,
        offset: u64,
        output: &mut [u8],
    ) -> std::result::Result<usize, MediaError> {
        self.checkpoint()?;
        if output.is_empty() {
            return Ok(0);
        }
        let count = output.len().min(65536) as u32;
        let (data, eof) = self
            .resources
            .read(self.transfer, offset, count)
            .map_err(port_error)?;
        if data.len() > count as usize
            || offset
                .checked_add(data.len() as u64)
                .is_none_or(|end| end > self.size)
            || eof != (offset + data.len() as u64 == self.size)
            || (data.is_empty() && !eof)
        {
            return Err(port_error(invalid(
                "Authorized native audio range changed its extent",
            )));
        }
        output[..data.len()].copy_from_slice(&data);
        self.checkpoint()?;
        Ok(data.len())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeProfile {
    Sinc,
    Concat,
    WindowedConcat,
}
impl DecodeProfile {
    pub fn name(self) -> &'static str {
        match self {
            Self::Sinc => "native-libav-sinc-at-target-rate",
            Self::Concat => "native-libav-concatenate-linear-resample",
            Self::WindowedConcat => "native-libav-windowed-concatenate-linear-resample",
        }
    }
}
pub(crate) struct DecodedSource {
    pub pcm: PcmSource,
    pub primary: Option<ClassicAudioPrimarySelection>,
    pub profile: DecodeProfile,
    pub track: NativeAudioTrack,
    pub native_frames: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Admission {
    max_frames: u64,
    max_channels: u32,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    working_frames: Option<u64>,
    #[serde(default)]
    source_sample_rate: Option<u32>,
    #[serde(default)]
    target_sample_rate: Option<u32>,
    #[serde(default)]
    max_native_frames: Option<String>,
    #[serde(default)]
    windows: Vec<AdmittedWindow>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdmittedWindow {
    source_start_frame: String,
    source_end_frame: String,
    target_start_frame: String,
}
struct Retained {
    start: u64,
    end: u64,
    target: u64,
    chunks: Vec<Window>,
}
#[derive(Clone, Copy)]
pub(crate) struct DecodeConfig {
    pub sample_rate: u32,
    pub max_pcm_bytes: u64,
    pub max_encoded_bytes: u64,
    pub deadline: Instant,
}
pub(crate) fn check<R: ClassicAudioResources + ?Sized>(
    resources: &mut R,
    deadline: Instant,
) -> Result<()> {
    resources.checkpoint()?;
    if Instant::now() >= deadline {
        return Err(EditorError::new(
            "AUDIO_TIME_LIMIT",
            "Native audio exceeded its captured deadline",
        ));
    }
    Ok(())
}
pub(crate) fn decode<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
    session: &mut S,
    resources: &mut R,
    resource: &AudioResource,
    handle: &str,
    config: &DecodeConfig,
    memory: &PcmMemory,
) -> Result<DecodedSource> {
    let codec_bytes = (config.max_pcm_bytes / 8).min(16 * 1024 * 1024);
    let mut input = NativeInput::open(
        session,
        resources,
        resource,
        handle,
        config,
        memory,
        codec_bytes,
    )?;
    let result = decode_opened(session, resources, &mut input, config, memory, None, false);
    let released = input.close(resources);
    let result = result?;
    released?;
    check(resources, config.deadline)?;
    Ok(result)
}
/// Preparation consumes the exact same bounded concatenation and resampling
/// kernel as export, with its independently admitted realtime consumer facts.
/// The native extent is reported separately from the target-rate PCM extent.
pub(super) fn decode_preview_preparation<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
    session: &mut S,
    resources: &mut R,
    input: &mut NativeInput,
    admission: &serde_json::Value,
    retain_decoder: bool,
    config: &DecodeConfig,
    memory: &PcmMemory,
) -> Result<DecodedSource> {
    decode_opened(
        session,
        resources,
        input,
        config,
        memory,
        Some(admission),
        retain_decoder,
    )
}
fn decode_opened<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
    session: &mut S,
    resources: &mut R,
    input: &mut NativeInput,
    config: &DecodeConfig,
    memory: &PcmMemory,
    preview_admission: Option<&serde_json::Value>,
    retain_decoder: bool,
) -> Result<DecodedSource> {
    let resource = input.resource.clone();
    let resource = &resource;
    let track = input.track.clone();
    let observed = input.primary.clone();
    let preview = preview_admission.is_some();
    let standalone =
        !preview && resource.kind == "audio" && resource.size <= config.max_encoded_bytes;
    let mut pcm_admission = None;
    if standalone
        && matches!(observed.codec.as_str(), "pcm-s16" | "pcm-f32")
        && observed.sample_rate == config.sample_rate
        && observed.channels <= 2
    {
        let proposal = callback(
            session,
            resources,
            "admitPcmAudioDecode",
            json!({"resource":resource,"primary":observed}),
        )?;
        match proposal["mode"].as_str() {
            Some("whole") if proposal.as_object().is_some_and(|o| o.len() == 1) => {}
            Some("windowed") if proposal.as_object().is_some_and(|o| o.len() == 2) => {
                pcm_admission = Some(proposal["admission"].clone())
            }
            _ => return Err(invalid("Invalid native PCM admission negotiation")),
        }
    }
    let container = !standalone || pcm_admission.is_some();
    let admission: Admission = serde_json::from_value(if let Some(admission) = preview_admission { admission.clone() } else { match pcm_admission {
        Some(a) => a,
        None => callback(
            session,
            resources,
            if container {
                "admitContainerDecode"
            } else {
                "admitDecode"
            },
            json!({"resource":resource,"primary":if container { Some(&observed) } else { None }}),
        )?,
    }})
    .map_err(|_| invalid("Invalid native audio decode admission"))?;
    if container {
        // Retained AudioBufferSink.buffers(0) starts at getKeyPacket(0), not at
        // a decoder pre-roll packet whose output is subsequently discarded.
        input.select_origin(resources, 0.0, config)?;
    }
    let channels = if container {
        track.channels.min(2)
    } else {
        track.channels
    };
    let sparse = admission.mode.as_deref() == Some("windowed");
    if admission.max_frames == 0
        || admission.max_frames > (1u64 << 53) - 1
        || !(1..=64).contains(&admission.max_channels)
        || track.channels > admission.max_channels
        || channels == 0
        || channels > 64
        || admission.windows.len() > 256
        || (container && !matches!(admission.mode.as_deref(), Some("whole" | "windowed")))
        || (!container
            && (admission.mode.is_some()
                || admission.working_frames.is_some()
                || !admission.windows.is_empty()))
    {
        return Err(invalid("Native primary format exceeds decode admission"));
    }
    let _window_metadata = memory
        .reserve((admission.windows.len().max(1) * 4 * std::mem::size_of::<Retained>()) as u64)?;
    let mut retained = Vec::new();
    let max_native = if container {
        if admission.source_sample_rate != Some(track.sample_rate)
            || admission.target_sample_rate != Some(config.sample_rate)
            || admission.max_channels != track.channels
            || admission
                .working_frames
                .is_none_or(|n| n == 0 || n > admission.max_frames)
        {
            return Err(invalid(
                "Native container admission changed its rates or residency",
            ));
        }
        let n = decimal(
            admission
                .max_native_frames
                .as_deref()
                .ok_or_else(|| invalid("Missing native frame limit"))?,
        )?;
        if u128::from(n)
            != u128::from(admission.max_frames) * u128::from(track.sample_rate)
                / u128::from(config.sample_rate)
        {
            return Err(invalid("Native and target frame admissions disagree"));
        }
        n
    } else {
        u64::try_from(
            (u128::from(admission.max_frames) * u128::from(track.sample_rate))
                .div_ceil(u128::from(config.sample_rate)),
        )
        .map_err(|_| invalid("Native frame bound overflow"))?
    };
    if sparse {
        let mut previous = 0;
        for w in &admission.windows {
            let start = decimal(&w.source_start_frame)?;
            let end = decimal(&w.source_end_frame)?;
            let target = decimal(&w.target_start_frame)?;
            if start < previous || end <= start || end > max_native || target > admission.max_frames
            {
                return Err(invalid("Invalid native retained PCM windows"));
            }
            retained.push(Retained {
                start,
                end,
                target,
                chunks: Vec::new(),
            });
            previous = end;
        }
    } else {
        if !admission.windows.is_empty() {
            return Err(invalid("Whole native PCM cannot use sparse windows"));
        }
        retained.push(Retained {
            start: 0,
            end: max_native,
            target: 0,
            chunks: Vec::new(),
        });
    }
    let mut total = 0u64;
    loop {
        check(resources, config.deadline)?;
        let frame = input.read(resources, config)?;
        let Some(frame) = frame else {
            break;
        };
        if frame.sample_rate != track.sample_rate || frame.channels.len() != track.channels as usize
        {
            return Err(invalid("Native decoded primary format changed"));
        }
        let end = total
            .checked_add(frame.frames() as u64)
            .filter(|n| *n <= max_native)
            .ok_or_else(|| invalid("Native decoder exceeded its logical frame admission"))?;
        for w in &mut retained {
            let from = total.max(w.start);
            let to = end.min(w.end);
            if to <= from {
                continue;
            }
            if w.chunks
                .last()
                .map_or(w.start, |c| c.start + c.samples.frames() as u64)
                != from
            {
                return Err(invalid("Native retained concatenation has a gap"));
            }
            let mut planes = Vec::new();
            for c in 0..channels as usize {
                let mut plane = memory.array((to - from) as usize)?;
                plane.as_mut_slice().copy_from_slice(
                    &frame.channels[c][(from - total) as usize..(to - total) as usize],
                );
                planes.push(plane);
            }
            w.chunks
                .push(Window::new(from, Samples::Planar(planes), memory)?);
        }
        total = end;
    }
    if total == 0 || !input.reached_eof()? {
        return Err(invalid("Native source decoded no actual EOF PCM"));
    }
    if !retain_decoder {
        input.retire_decoder();
    }
    // Preserve the retained engine's floating-point length calculation. Source
    // positions supplied by composition remain exact integers/rationals.
    let scaled = if preview {
        total as f64 * f64::from(config.sample_rate) / f64::from(track.sample_rate)
    } else {
        total as f64 * (f64::from(config.sample_rate) / f64::from(track.sample_rate))
    };
    let frames = if container {
        scaled.ceil()
    } else {
        (total as f64 / (f64::from(track.sample_rate) / f64::from(config.sample_rate))).floor()
    } as u64;
    if frames == 0 || frames > admission.max_frames {
        return Err(invalid("Native target PCM exceeded frame admission"));
    }
    let profile = if !container {
        DecodeProfile::Sinc
    } else if sparse {
        DecodeProfile::WindowedConcat
    } else {
        DecodeProfile::Concat
    };
    let mut output = Vec::new();
    let mut phase = 0.0;
    let mut cursor = 0u64;
    let ratio = f64::from(track.sample_rate) / f64::from(config.sample_rate);
    let resampler = if container {
        None
    } else {
        Some(DecodeResampler::new(
            track.sample_rate,
            config.sample_rate,
            memory,
        )?)
    };
    for w in retained {
        if w.chunks.is_empty() || w.target >= frames {
            continue;
        }
        let end_native = w
            .chunks
            .last()
            .map(|c| c.start + c.samples.frames() as u64)
            .unwrap();
        let native = PcmSource::new(track.sample_rate, total, channels, true, w.chunks, memory)?;
        let end = if end_native == total {
            frames
        } else {
            frames.min(
                (end_native as f64 * (f64::from(config.sample_rate) / f64::from(track.sample_rate)))
                    .ceil() as u64,
            )
        };
        if !container {
            let mut planes = Vec::new();
            for c in 0..channels {
                let mut target = memory.array(frames as usize)?;
                resampler.as_ref().unwrap().process_source(
                    total,
                    &mut |i| native.value(c, i),
                    target.as_mut_slice(),
                    &mut || check(resources, config.deadline),
                )?;
                planes.push(target);
            }
            output.push(Window::new(0, Samples::Planar(planes), memory)?);
            break;
        }
        while cursor < w.target {
            if cursor % 16384 == 0 {
                check(resources, config.deadline)?;
            }
            phase += ratio;
            cursor += 1;
        }
        while cursor < end {
            check(resources, config.deadline)?;
            let count = (end - cursor).min(4096) as usize;
            let start = phase;
            for _ in 0..count {
                phase += ratio;
            }
            let from = w.start.max((start.floor() as u64).saturating_sub(32));
            let to = end_native.min((phase.ceil() as u64).saturating_add(32));
            let mut planes = Vec::new();
            for c in 0..channels {
                let mut target = memory.array(count)?;
                // Whole-context decoding retains one continuous floating read
                // index. Sparse contexts retain the original absolute phase at
                // each admitted 4096-frame block and its guarded local origin.
                let mut local = if sparse { start - from as f64 } else { start };
                let origin = if sparse { from } else { 0 };
                let limit = if sparse { to } else { total };
                for value in target.as_mut_slice() {
                    let position = origin as f64 + local;
                    if position < limit as f64 {
                        let lower = local.floor() as u64 + origin;
                        let fraction = local - local.floor();
                        let a = f64::from(native.value(c, lower)?);
                        let sample = if lower + 1 < limit {
                            a * (1.0 - fraction) + f64::from(native.value(c, lower + 1)?) * fraction
                        } else if lower > origin {
                            a + (a - f64::from(native.value(c, lower - 1)?)) * fraction
                        } else {
                            a
                        };
                        *value = sample as f32;
                        if !value.is_finite() {
                            return Err(invalid("Native concat resampling overflow"));
                        }
                    }
                    local += ratio;
                }
                planes.push(target);
            }
            output.push(Window::new(cursor, Samples::Planar(planes), memory)?);
            cursor += count as u64;
        }
    }
    let pcm = PcmSource::new(config.sample_rate, frames, channels, sparse, output, memory)?;
    if !sparse && pcm.resident_frames() != frames {
        return Err(invalid("Native dense decode lost source frames"));
    }
    if sparse && pcm.resident_frames() > admission.working_frames.unwrap() {
        return Err(invalid(
            "Native sparse PCM exceeded resident frame admission",
        ));
    }
    Ok(DecodedSource {
        pcm,
        primary: if container { Some(observed) } else { None },
        profile,
        native_frames: total,
        track,
    })
}
