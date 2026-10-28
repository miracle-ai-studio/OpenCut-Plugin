//! Classic realtime facts: ordinary PCM preserves observed PTS; prepared PCM
//! starts at zero. Gain ramps interpolate physical amplitudes at 60 Hz from the
//! generation's anchor, rather than sampling dB independently at every sample.
//! Legacy linear envelopes retain the exact authored per-sample export clock.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CLASSIC_REALTIME_AUDIO_PROFILE: &str = "classic-realtime-v31-web-audio";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewGainOverride {
    pub track_id: String,
    pub element_id: String,
    pub decibels_tenths: i32,
}
struct GainTarget {
    track_id: String,
    locked: bool,
    decibels: bool,
    animated: bool,
}
pub struct PreparedPreviewAudio {
    audio: PreparedAudio,
    targets: BTreeMap<String, GainTarget>,
}
impl PreparedPreviewAudio {
    pub fn new(
        snapshot: &EditorSnapshot,
        sequence_id: &str,
        format: PcmFormat,
    ) -> Result<Self, CompositionError> {
        prepare_preview_audio_scene(snapshot, sequence_id, format)
    }
    pub fn audio(&self) -> &PreparedAudio {
        &self.audio
    }
    pub fn extent(&self) -> u64 {
        self.audio.duration_samples()
    }
    /// Use the same maximum admitted prefix as fixed-revision WAV execution.
    pub fn block_frame_limit(&self, start: u64) -> u32 {
        self.audio.block_frame_limit(start, MAX_AUDIO_BLOCK_FRAMES)
    }
    pub fn validate_temporary_gain(
        &self,
        track_id: &str,
        element_id: &str,
        decibels_tenths: i32,
    ) -> Result<PreviewGainOverride, CompositionError> {
        let target = self
            .targets
            .get(element_id)
            .filter(|t| t.track_id == track_id)
            .ok_or_else(|| {
                invalid("Temporary gain target does not belong to this fixed scene/track")
            })?;
        validate_gain_target(target, track_id, element_id, decibels_tenths)
    }
}
/// Validate one fixed element without preparing unrelated scene audio.
pub fn validate_preview_gain_override_scene(
    track: &SceneTrack,
    element: &TimelineElement,
    decibels_tenths: i32,
) -> Result<PreviewGainOverride, CompositionError> {
    if !track.elements.iter().any(|candidate| candidate == element) {
        return Err(invalid(
            "Temporary gain target does not belong to this fixed scene/track",
        ));
    }
    let style = match &element.content {
        ElementContent::Audio { audio, .. } | ElementContent::Video { audio, .. } => audio,
        _ => return Err(invalid("Temporary gain requires an audio-bearing element")),
    };
    let target = GainTarget {
        track_id: track.id.clone(),
        locked: track.locked,
        decibels: matches!(style.gain, Gain::Decibels { .. }),
        animated: has_keys(&gains::channels(&element.channels)),
    };
    validate_gain_target(&target, &track.id, &element.id, decibels_tenths)
}
fn validate_gain_target(
    target: &GainTarget,
    track_id: &str,
    element_id: &str,
    decibels_tenths: i32,
) -> Result<PreviewGainOverride, CompositionError> {
    if target.locked {
        return Err(CompositionError {
            code: "LOCKED_TRACK",
            message: "Temporary gain target is on a locked track".into(),
        });
    }
    if !target.decibels {
        return Err(unsupported(
            "Temporary gain cannot reinterpret legacy linear gain",
        ));
    }
    if target.animated {
        return Err(unsupported(
            "Temporary static gain cannot replace an animated volume envelope",
        ));
    }
    if !(-600..=200).contains(&decibels_tenths) {
        return Err(invalid("Temporary gain requires -600..200 tenths dB"));
    }
    Ok(PreviewGainOverride {
        track_id: track_id.into(),
        element_id: element_id.into(),
        decibels_tenths,
    })
}
fn has_keys(channels: &[AnimationChannel]) -> bool {
    channels
        .iter()
        .any(|c| matches!(c, AnimationChannel::Scalar { keys, .. } if !keys.is_empty()))
}
pub fn prepare_preview_audio_scene(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    format: PcmFormat,
) -> Result<PreparedPreviewAudio, CompositionError> {
    if !matches!(format.sample_rate, 44_100 | 48_000) || format.channels != 2 {
        return Err(unsupported(
            "Realtime browser audio requires 44100 or 48000 Hz stereo",
        ));
    }
    let mut audio = prepare_audio_scene(
        snapshot,
        sequence_id,
        AudioExecutionProfile::ClassicV31,
        format,
    )?;
    let sequence = snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == sequence_id)
        .ok_or_else(|| invalid("Missing fixed preview scene"))?;
    let mut targets = BTreeMap::new();
    for track in &sequence.tracks {
        for element in &track.elements {
            let style = match &element.content {
                ElementContent::Audio { audio, .. } | ElementContent::Video { audio, .. } => audio,
                _ => continue,
            };
            targets.insert(
                element.id.clone(),
                GainTarget {
                    track_id: track.id.clone(),
                    locked: track.locked,
                    decibels: matches!(style.gain, Gain::Decibels { .. }),
                    animated: has_keys(&gains::channels(&element.channels)),
                },
            );
        }
    }
    for voice in &mut audio.voices {
        // Classic uses a concatenated, retimed Float32 buffer for animated
        // element volume even when maintainPitch is false. Track animation is
        // an explicit V4 extension and keeps its independent sequence clock.
        if has_keys(&voice.element_channels)
            && matches!(voice.element_gain, Gain::Decibels { .. })
            && voice.processing == AudioProcessing::LinearInterpolationZeroTail
        {
            voice.processing = AudioProcessing::ClassicPreparedLinear;
        }
    }
    budget::admit(&audio.voices)?;
    Ok(PreparedPreviewAudio { audio, targets })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewAudioSegment {
    pub segment_id: u32,
    /// Pinned decoder's observed seconds, not authored project ticks.
    #[serde(deserialize_with = "runtime_pcm::deserialize_f64")]
    pub timestamp_seconds: f64,
    #[serde(with = "runtime_pcm::decimal_u64")]
    pub frame_count: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewSourceFormat {
    pub element_id: String,
    pub sample_rate: u32,
    #[serde(with = "runtime_pcm::decimal_u64")]
    pub frame_count: u64,
    pub channels: u32,
    pub prepared: bool,
    /// Ordinary sources retain native-rate chunks and their PTS. Prepared
    /// sources contain one target-rate buffer and must supply an empty list.
    pub segments: Vec<PreviewAudioSegment>,
}
pub struct BoundPreviewAudio<'a> {
    prepared: &'a PreparedPreviewAudio,
    formats: Vec<BoundPreviewSource>,
    original_order: Vec<usize>,
}
impl BoundPreviewAudio<'_> {
    /// Preserve the admitted input order for exact fixed-session receipts.
    /// The compiled voice lookup remains in prepared order.
    pub fn source_formats(&self) -> impl Iterator<Item = &PreviewSourceFormat> {
        self.original_order
            .iter()
            .map(|&index| &self.formats[index].format)
    }
}
struct BoundPreviewSource {
    format: PreviewSourceFormat,
    native_starts: Vec<i64>,
}

// The pinned WebCodecs decoder rounds PTS to native samples, but its PCM
// decoder accumulates up to 65,536 packet durations in f64 without that round.
// Allow only bounded arithmetic/JSON rounding around an unambiguous sample
// index. Keep the original PTS in the binding and sampling facts; this check
// must neither shift a real gap nor turn arbitrary off-grid PTS into support.
const MAX_OBSERVED_NATIVE_SAMPLE: i64 = 9_007_199_254_740_991;
fn observed_native_sample(
    timestamp: f64,
    sample_rate: u32,
    scale: f64,
) -> Result<i64, CompositionError> {
    let rate = f64::from(sample_rate);
    let rounded = (timestamp * rate).round();
    if !timestamp.is_finite()
        || !rounded.is_finite()
        || rounded.abs() > MAX_OBSERVED_NATIVE_SAMPLE as f64
    {
        return Err(invalid(
            "Observed PTS exceeds the exact signed native sample range",
        ));
    }
    let index = rounded as i64;
    let observed = timestamp * rate;
    let rounding_bound = (f64::EPSILON * 131_080.0 * scale.max(1.0)).min(1.0 / 1024.0);
    if (observed - rounded).abs() > rounding_bound
        || (index - 1) as f64 / rate == timestamp
        || (index + 1) as f64 / rate == timestamp
    {
        return Err(invalid(
            "Observed PTS is not an unambiguous pinned native sample grid point",
        ));
    }
    Ok(index)
}

pub fn bind_preview_audio_scene<'a>(
    prepared: &'a PreparedPreviewAudio,
    formats: &[PreviewSourceFormat],
) -> Result<BoundPreviewAudio<'a>, CompositionError> {
    bind_preview_formats(prepared, formats, false)
}

/// A transient decoder window uses the same PTS validation and sample compiler.
/// Empty ordinary windows are actual gap/EOF observations, not a silent source.
pub fn bind_preview_audio_window<'a>(
    prepared: &'a PreparedPreviewAudio,
    formats: &[PreviewSourceFormat],
) -> Result<BoundPreviewAudio<'a>, CompositionError> {
    bind_preview_formats(prepared, formats, true)
}
fn bind_preview_formats<'a>(
    prepared: &'a PreparedPreviewAudio,
    formats: &[PreviewSourceFormat],
    window: bool,
) -> Result<BoundPreviewAudio<'a>, CompositionError> {
    if formats.len() != prepared.audio.voices.len() {
        return Err(invalid(
            "Preview sources must bind the complete fixed voice set",
        ));
    }
    let mut by_id = BTreeMap::new();
    for (ordinal, f) in formats.iter().enumerate() {
        if by_id.insert(f.element_id.as_str(), (ordinal, f)).is_some() {
            return Err(invalid("Duplicate preview source"));
        }
    }
    let mut output = Vec::with_capacity(formats.len());
    let mut original_order = vec![0; formats.len()];
    for voice in &prepared.audio.voices {
        let (ordinal, f) = by_id
            .remove(voice.element_id.as_str())
            .ok_or_else(|| invalid("Unknown or missing preview source"))?;
        if f.sample_rate == 0
            || f.sample_rate > 384_000
            || (f.frame_count == 0 && (!window || f.prepared || !f.segments.is_empty()))
            || !(1..=2).contains(&f.channels)
        {
            return Err(invalid("Invalid actual preview PCM format"));
        }
        let is_prepared = voice.processing != AudioProcessing::LinearInterpolationZeroTail;
        if f.prepared != is_prepared {
            return Err(invalid("Preview source mode differs from its fixed voice"));
        }
        let mut native_starts = Vec::new();
        if is_prepared {
            let frames = clocks::sample_index(
                time(&voice.timeline_duration)?,
                prepared.audio.identity.format.sample_rate,
                clocks::Rounding::Ceil,
            )?
            .max(1);
            if !f.segments.is_empty()
                || f.sample_rate != prepared.audio.identity.format.sample_rate
                || f.frame_count != frames
            {
                return Err(invalid(
                    "Prepared preview PCM must retain exact output length, target rate and zero origin",
                ));
            }
        } else {
            if (!window && f.segments.is_empty()) || f.segments.len() > 65_536 {
                return Err(invalid(
                    "Ordinary preview PCM requires a bounded PTS segment list",
                ));
            }
            let mut ids = std::collections::BTreeSet::new();
            let mut count = 0u64;
            let mut last_end = None;
            // Use the complete observed range for cancellation around zero.
            // Two rounding operations per admitted packet plus transport,
            // capped at 1/1024 of a native sample, cannot hide a sample gap.
            let scale = f
                .segments
                .iter()
                .map(|s| s.timestamp_seconds.abs() * f64::from(f.sample_rate))
                .fold(1.0, f64::max);
            for s in &f.segments {
                if !ids.insert(s.segment_id) || s.frame_count == 0 {
                    return Err(invalid("Invalid observed PTS segment"));
                }
                let start = observed_native_sample(s.timestamp_seconds, f.sample_rate, scale)?;
                let length = i64::try_from(s.frame_count)
                    .map_err(|_| invalid("Decoded PTS segment frame count overflow"))?;
                let end = start
                    .checked_add(length)
                    .filter(|end| {
                        (-MAX_OBSERVED_NATIVE_SAMPLE..=MAX_OBSERVED_NATIVE_SAMPLE).contains(end)
                    })
                    .ok_or_else(|| invalid("Decoded PTS segment endpoint overflow"))?;
                if last_end.is_some_and(|previous| start < previous) {
                    return Err(unsupported(
                        "Overlapping or out-of-order decoded PTS segments require an explicit decoder profile",
                    ));
                }
                last_end = Some(end);
                native_starts.push(start);
                count = count
                    .checked_add(s.frame_count)
                    .ok_or_else(|| invalid("Decoded segment frame count overflow"))?;
            }
            if count != f.frame_count {
                return Err(invalid(
                    "Decoded segments do not cover the actual PCM frame count",
                ));
            }
        }
        original_order[ordinal] = output.len();
        output.push(BoundPreviewSource {
            format: f.clone(),
            native_starts,
        });
    }
    Ok(BoundPreviewAudio {
        prepared,
        formats: output,
        original_order,
    })
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSourceSample {
    pub segment_id: u32,
    #[serde(with = "runtime_pcm::decimal_u64")]
    pub lower: u64,
    #[serde(with = "runtime_pcm::decimal_u64")]
    pub upper: u64,
    #[serde(deserialize_with = "runtime_pcm::deserialize_f64")]
    pub fraction: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewAudioVoiceBlock {
    pub element_id: String,
    pub block_offset: u32,
    pub frame_count: u32,
    pub positions: Vec<Option<PreviewSourceSample>>,
    #[serde(deserialize_with = "runtime_pcm::deserialize_amplitudes")]
    pub amplitudes: Vec<f64>,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewAudioIdentity {
    pub project_id: String,
    pub revision: String,
    pub sequence_id: String,
    pub profile: &'static str,
    pub sample_rate: u32,
    pub channels: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewAudioBlock {
    pub identity: PreviewAudioIdentity,
    #[serde(with = "runtime_pcm::decimal_u64")]
    pub start_sample: u64,
    pub frame_count: u32,
    pub voices: Vec<PreviewAudioVoiceBlock>,
    pub receipts: Vec<AudioReceipt>,
}
impl<'de> Deserialize<'de> for PreviewAudioIdentity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Fields {
            project_id: String,
            revision: String,
            sequence_id: String,
            profile: String,
            sample_rate: u32,
            channels: u32,
        }
        let f = Fields::deserialize(deserializer)?;
        if f.profile != CLASSIC_REALTIME_AUDIO_PROFILE {
            return Err(serde::de::Error::custom(
                "Compiled preview profile differs from fixed realtime semantics",
            ));
        }
        Ok(Self {
            project_id: f.project_id,
            revision: f.revision,
            sequence_id: f.sequence_id,
            profile: CLASSIC_REALTIME_AUDIO_PROFILE,
            sample_rate: f.sample_rate,
            channels: f.channels,
        })
    }
}
fn preview_amplitude(
    base: Gain,
    channels: &[AnimationChannel],
    seconds: f64,
) -> Result<f64, CompositionError> {
    let ticks = (seconds * 120_000.0).round();
    if !ticks.is_finite() || ticks < 0.0 || ticks >= i64::MAX as f64 {
        return Err(invalid("Realtime gain clock overflows"));
    }
    let at: Time = RationalTime::new(ticks as i64, 120_000)
        .map_err(error)?
        .into();
    match gains::physical(base, channels, Some(&at))? {
        Gain::Decibels { value } => finite(10f64.powf(value.clamp(-60.0, 20.0) / 20.0)),
        linear => Ok(animation::gain(linear)?.amplitude),
    }
}
#[derive(Default)]
struct RampCache {
    endpoints: Option<(u64, u64, f64, f64)>,
}
impl RampCache {
    fn sample(
        &mut self,
        base: Gain,
        channels: &[AnimationChannel],
        sample: u64,
        anchor: u64,
        end_seconds: f64,
        rate: u32,
    ) -> Result<f64, CompositionError> {
        if !has_keys(channels) {
            return preview_amplitude(base, &[], 0.0);
        }
        let seconds = sample as f64 / f64::from(rate);
        let origin = anchor as f64 / f64::from(rate);
        let offset = (seconds - origin).max(0.0);
        let knot = (offset * 60.0).floor();
        let left = (origin + knot / 60.0).min(end_seconds);
        let right = (origin + (knot + 1.0) / 60.0).min(end_seconds);
        let (a, b) = match self.endpoints {
            Some((l, r, a, b)) if l == left.to_bits() && r == right.to_bits() => (a, b),
            _ => {
                let a = preview_amplitude(base, channels, left)?;
                let b = if right <= left {
                    a
                } else {
                    preview_amplitude(base, channels, right)?
                };
                self.endpoints = Some((left.to_bits(), right.to_bits(), a, b));
                (a, b)
            }
        };
        if right <= left {
            return Ok(a);
        }
        finite(a + (b - a) * ((seconds - left) / (right - left)).clamp(0.0, 1.0))
    }
}

// Identity and key IDs do not affect an envelope's physical samples. Retain
// every executable field (including signed-zero floats) in this private key.
// Sharing is limited to the current bounded block; no authored state is cached.
fn envelope_facts(
    base: Gain,
    channels: &[AnimationChannel],
) -> Result<serde_json::Value, CompositionError> {
    let mut channels = serde_json::to_value(channels).map_err(|_| invalid("Invalid gain facts"))?;
    for channel in channels
        .as_array_mut()
        .ok_or_else(|| invalid("Invalid gain facts"))?
    {
        let object = channel
            .as_object_mut()
            .ok_or_else(|| invalid("Invalid gain facts"))?;
        object.remove("id");
        object.remove("target");
        if let Some(keys) = object
            .get_mut("keys")
            .and_then(serde_json::Value::as_array_mut)
        {
            for key in keys {
                if let Some(key) = key.as_object_mut() {
                    key.remove("id");
                }
            }
        }
    }
    Ok(serde_json::json!([base, channels]))
}

pub fn compile_preview_audio_block_scene(
    bound: &BoundPreviewAudio<'_>,
    start_sample: u64,
    frame_count: u32,
    anchor_sample: u64,
    gain: Option<&PreviewGainOverride>,
) -> Result<PreviewAudioBlock, CompositionError> {
    if anchor_sample >= bound.prepared.extent()
        || start_sample < anchor_sample
        || start_sample
            .checked_add(u64::from(frame_count))
            .is_none_or(|end| end > bound.prepared.extent())
    {
        return Err(invalid("Preview block precedes its generation anchor"));
    }
    if let Some(g) = gain {
        bound
            .prepared
            .validate_temporary_gain(&g.track_id, &g.element_id, g.decibels_tenths)?;
    }
    let audio = &bound.prepared.audio;
    let block = compile_audio_block_inner(audio, start_sample, frame_count, false)?;
    let rate = audio.identity.format.sample_rate;
    let mut voices: Vec<PreviewAudioVoiceBlock> = Vec::with_capacity(block.voices.len());
    let mut envelope_keys = Vec::<Vec<u8>>::with_capacity(block.voices.len());
    // Equal fixed source clocks and observed PCM layouts have equal taps.
    // Share only within this captured block; resource samples and gain remain
    // per voice and retain their original authored mixing order.
    let mut position_keys: Vec<(&AudioSourceMapping, &PreviewSourceFormat, u64, u32)> =
        Vec::with_capacity(block.voices.len());
    for facts in block.voices {
        let voice = audio
            .voices
            .iter()
            .find(|v| v.element_id == facts.element_id)
            .expect("compiled fixed voice");
        let format = bound
            .formats
            .iter()
            .find(|f| f.format.element_id == facts.element_id)
            .expect("complete binding");
        let source = format;
        let format = &source.format;
        let shared_positions = position_keys
            .iter()
            .position(|(mapping, old, local, count)| {
                *mapping == &voice.source
                    && *local == facts.local_start_sample
                    && *count == facts.frame_count
                    && old.sample_rate == format.sample_rate
                    && old.frame_count == format.frame_count
                    && old.channels == format.channels
                    && old.prepared == format.prepared
                    && old.segments == format.segments
            });
        let mut positions = shared_positions.map_or_else(
            || Vec::with_capacity(facts.frame_count as usize),
            |index| voices[index].positions.clone(),
        );
        let mut amplitudes = Vec::with_capacity(facts.frame_count as usize);
        let local_anchor = anchor_sample.saturating_sub(voice.start);
        let duration = time(&voice.timeline_duration)?;
        let end_seconds = duration.ticks() as f64 / f64::from(duration.timebase());
        let element_gain =
            gain.filter(|g| g.element_id == voice.element_id)
                .map_or(voice.element_gain, |g| Gain::Decibels {
                    value: f64::from(g.decibels_tenths) / 10.0,
                });
        // Constant physical gain is independent of both generation anchors and
        // sample clocks. Animated dB channels retain the original 60Hz ramps.
        let element_constant = if has_keys(&voice.element_channels) {
            None
        } else {
            Some(preview_amplitude(element_gain, &[], 0.0)?)
        };
        let track_constant = if has_keys(&voice.track_channels) {
            None
        } else {
            Some(preview_amplitude(voice.track_gain, &[], 0.0)?)
        };
        let element_legacy = (element_constant.is_none()
            && matches!(element_gain, Gain::LinearLegacy { .. }))
        .then(|| gains::PreparedGainCurve::new(element_gain, &voice.element_channels))
        .transpose()?;
        let track_legacy = (track_constant.is_none()
            && matches!(voice.track_gain, Gain::LinearLegacy { .. }))
        .then(|| gains::PreparedGainCurve::new(voice.track_gain, &voice.track_channels))
        .transpose()?;
        let envelope_key = serde_json::to_vec(&serde_json::json!([
            envelope_facts(element_gain, &voice.element_channels)?,
            envelope_facts(voice.track_gain, &voice.track_channels)?,
            facts.local_start_sample,
            local_anchor,
            end_seconds,
            start_sample + u64::from(facts.block_offset),
            anchor_sample,
            facts.frame_count,
        ]))
        .map_err(|_| invalid("Invalid gain facts"))?;
        let shared = envelope_keys.iter().position(|key| key == &envelope_key);
        if let Some(index) = shared {
            amplitudes.extend_from_slice(&voices[index].amplitudes);
        }
        // Each ramp endpoint is a fixed Rust curve fact. Cache its exact
        // f64 result within this block instead of solving the same Bezier for
        // every 48 kHz sample; anchors, 60 Hz knots and interpolation stay exact.
        let mut element_ramp = RampCache::default();
        let mut track_ramp = RampCache::default();
        let compile_count = if shared_positions.is_some() && shared.is_some() {
            0
        } else {
            facts.frame_count
        };
        for offset in 0..u64::from(compile_count) {
            let local = facts.local_start_sample + offset;
            if shared_positions.is_none() {
                let position = if format.prepared {
                    (local < format.frame_count).then_some(PreviewSourceSample {
                        segment_id: 0,
                        lower: local,
                        upper: (local + 1).min(format.frame_count - 1),
                        fraction: 0.0,
                    })
                } else {
                    let exact = voice.source.position_at(local, format.sample_rate)?;
                    let native = i128::from(exact.lower);
                    let index = source
                        .native_starts
                        .partition_point(|s| i128::from(*s) <= native);
                    index
                        .checked_sub(1)
                        .and_then(|i| format.segments.get(i).map(|segment| (i, segment)))
                        .and_then(|(i, segment)| {
                            let local = native - i128::from(source.native_starts[i]);
                            if local < 0 || local >= i128::from(segment.frame_count) {
                                return None;
                            }
                            let lower = local as u64;
                            Some(PreviewSourceSample {
                                segment_id: segment.segment_id,
                                lower,
                                upper: (lower + 1).min(segment.frame_count - 1),
                                fraction: exact.fraction_numerator as f64
                                    / exact.fraction_denominator as f64,
                            })
                        })
                };
                positions.push(position);
            }
            if shared.is_none() {
                let element = if let Some(curve) = &element_legacy {
                    curve.sample(local, rate)?
                } else {
                    element_constant.map_or_else(
                        || {
                            element_ramp.sample(
                                element_gain,
                                &voice.element_channels,
                                local,
                                local_anchor,
                                end_seconds,
                                rate,
                            )
                        },
                        Ok,
                    )?
                };
                let sequence_sample = start_sample + u64::from(facts.block_offset) + offset;
                let track = if let Some(curve) = &track_legacy {
                    curve.sample(sequence_sample, rate)?
                } else {
                    track_constant.map_or_else(
                        || {
                            track_ramp.sample(
                                voice.track_gain,
                                &voice.track_channels,
                                sequence_sample,
                                anchor_sample,
                                audio.duration_samples as f64 / f64::from(rate),
                                rate,
                            )
                        },
                        Ok,
                    )?
                };
                amplitudes.push(finite(element * track)?);
            }
        }
        position_keys.push((
            &voice.source,
            format,
            facts.local_start_sample,
            facts.frame_count,
        ));
        envelope_keys.push(envelope_key);
        voices.push(PreviewAudioVoiceBlock {
            element_id: facts.element_id,
            block_offset: facts.block_offset,
            frame_count: facts.frame_count,
            positions,
            amplitudes,
        });
    }
    Ok(PreviewAudioBlock {
        identity: PreviewAudioIdentity {
            project_id: audio.identity.project_id.clone(),
            revision: audio.identity.revision.clone(),
            sequence_id: audio.identity.sequence_id.clone(),
            profile: CLASSIC_REALTIME_AUDIO_PROFILE,
            sample_rate: rate,
            channels: 2,
        },
        start_sample,
        frame_count,
        voices,
        receipts: block.receipts,
    })
}

/// Decoder API projections from the same exact source mapping used by the
/// sampler. Browser code neither maps authored clocks nor infers retime.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSourceWindow {
    pub asset_id: String,
    #[serde(deserialize_with = "runtime_pcm::deserialize_f64")]
    pub start_seconds: f64,
    #[serde(deserialize_with = "runtime_pcm::deserialize_f64")]
    pub end_seconds: f64,
}
pub fn preview_source_windows(
    prepared: &PreparedPreviewAudio,
    start: u64,
    frames: u32,
    rates: &BTreeMap<String, u32>,
) -> Result<Vec<PreviewSourceWindow>, CompositionError> {
    let block = compile_audio_block_inner(&prepared.audio, start, frames, false)?;
    let mut by_asset: BTreeMap<&str, Vec<(u64, u64, u32)>> = BTreeMap::new();
    for facts in block.voices {
        let voice = prepared
            .audio
            .voices
            .iter()
            .find(|v| v.element_id == facts.element_id)
            .expect("compiled fixed voice");
        let Some(&rate) = rates.get(&voice.resource.asset_id) else {
            continue;
        };
        if voice.processing != AudioProcessing::LinearInterpolationZeroTail {
            continue;
        }
        if rate == 0 {
            return Err(invalid(
                "Rolling windows require an observed primary sample rate",
            ));
        }
        let low = voice
            .source
            .position_at(facts.local_start_sample, rate)?
            .lower;
        let last = facts
            .local_start_sample
            .checked_add(u64::from(facts.frame_count) - 1)
            .ok_or_else(|| invalid("Preview source range overflow"))?;
        let high = voice
            .source
            .position_at(last, rate)?
            .lower
            .checked_add(2)
            .filter(|end| *end <= MAX_OBSERVED_NATIVE_SAMPLE as u64)
            .ok_or_else(|| invalid("Preview decoder range exceeds safe addressing"))?;
        by_asset
            .entry(&voice.resource.asset_id)
            .or_default()
            .push((low, high, rate));
    }
    let mut output = Vec::new();
    for (asset, mut ranges) in by_asset {
        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64, u32)> = Vec::new();
        for (start, end, rate) in ranges {
            if let Some(last) = merged.last_mut().filter(|last| start <= last.1) {
                last.1 = last.1.max(end);
            } else {
                merged.push((start, end, rate));
            }
        }
        for (start, end, rate) in merged {
            output.push(PreviewSourceWindow {
                asset_id: asset.into(),
                start_seconds: start as f64 / f64::from(rate),
                end_seconds: end as f64 / f64::from(rate),
            });
        }
    }
    Ok(output)
}
