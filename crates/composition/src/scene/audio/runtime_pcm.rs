//! Bind actual final PCM buffers to one immutable preparation. This module
//! contains sample facts only; decoding, DSP, PCM reads and policy stay in hosts.
use super::*;
use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};

/// Actual final buffer format, after any Classic pitch preparation. This is an
/// internal execution port, not a frozen project/public protocol declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeSourceFormat {
    pub element_id: String,
    pub sample_rate: u32,
    #[serde(with = "decimal_u64")]
    pub frame_count: u64,
    pub channels: u32,
    pub mode: ClassicAudioPreparationMode,
}

/// A private complete binding in stable prepared voice order. Borrowing keeps
/// all source/gain clocks and the fixed revision owned by the original plan.
pub struct BoundAudioPcm<'a> {
    prepared: &'a PreparedAudio,
    formats: Vec<RuntimeSourceFormat>,
}
impl BoundAudioPcm<'_> {
    pub fn identity(&self) -> &AudioIdentity {
        self.prepared.identity()
    }
    pub fn duration_samples(&self) -> u64 {
        self.prepared.duration_samples()
    }
    pub fn receipts(&self) -> &[AudioReceipt] {
        self.prepared.receipts()
    }
    pub fn source_formats(&self) -> &[RuntimeSourceFormat] {
        &self.formats
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeAudioVoiceBlock {
    pub element_id: String,
    pub block_offset: u32,
    pub frame_count: u32,
    /// Exact indices remain nominal even beyond the actual PCM length. The
    /// renderer reads a zero tail and clamps only the last upper interpolation
    /// tap, as Classic does; these facts never authorize an out-of-buffer read.
    pub positions: Vec<ExactSourceSample>,
    /// Physical amplitudes; gain clocks never switch to a prepared source clock.
    #[serde(deserialize_with = "deserialize_amplitudes")]
    pub amplitudes: Vec<f64>,
}

pub(super) fn deserialize_amplitudes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<f64>, D::Error> {
    struct Amplitudes;
    impl<'de> serde::de::Visitor<'de> for Amplitudes {
        type Value = Vec<f64>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded exact Float64 amplitude facts")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(raw) = seq.next_element::<Box<serde_json::value::RawValue>>()? {
                if out.len() >= MAX_AUDIO_BLOCK_GAIN_SAMPLES {
                    return Err(serde::de::Error::custom(
                        "Audio amplitude work budget exceeded",
                    ));
                }
                let value = raw.get().parse::<f64>().map_err(serde::de::Error::custom)?;
                if !value.is_finite() {
                    return Err(serde::de::Error::custom("Non-finite audio amplitude"));
                }
                out.push(value);
            }
            Ok(out)
        }
    }
    deserializer.deserialize_seq(Amplitudes)
}
pub(super) fn deserialize_f64<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<f64, D::Error> {
    let raw = <Box<serde_json::value::RawValue> as serde::Deserialize>::deserialize(deserializer)?;
    raw.get()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .ok_or_else(|| serde::de::Error::custom("Expected a finite compiled floating-point fact"))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeAudioBlock {
    #[serde(
        serialize_with = "serialize_identity",
        deserialize_with = "deserialize_identity"
    )]
    pub identity: AudioIdentity,
    #[serde(with = "decimal_u64")]
    pub start_sample: u64,
    pub frame_count: u32,
    pub voices: Vec<RuntimeAudioVoiceBlock>,
    /// Original participation and per-block OutsideBlock decisions, unchanged
    /// by runtime format binding; no resource locator is exposed.
    pub receipts: Vec<AudioReceipt>,
}

fn deserialize_identity<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<AudioIdentity, D::Error> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Identity {
        project_id: String,
        revision: String,
        sequence_id: String,
        profile: String,
        sample_rate: u32,
        channels: u32,
    }
    let value = Identity::deserialize(deserializer)?;
    let profile = match value.profile.as_str() {
        "classic-export-v31-web-audio" => AudioExecutionProfile::ClassicV31,
        "legacy-ffmpeg-v3" => AudioExecutionProfile::LegacyFfmpegV3,
        _ => return Err(serde::de::Error::custom("Unknown compiled audio profile")),
    };
    Ok(AudioIdentity {
        project_id: value.project_id,
        revision: value.revision,
        sequence_id: value.sequence_id,
        profile,
        format: PcmFormat {
            sample_rate: value.sample_rate,
            channels: value.channels,
        },
    })
}
pub(super) fn deserialize_u128<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u128, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.is_empty()
        || value.len() > 39
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(serde::de::Error::custom(
            "Expected canonical unsigned decimal u128",
        ));
    }
    value.parse().map_err(serde::de::Error::custom)
}

fn classic(prepared: &PreparedAudio) -> Result<(), CompositionError> {
    if prepared.identity.profile != AudioExecutionProfile::ClassicV31 {
        return Err(unsupported(
            "Runtime PCM binding requires the Classic browser profile",
        ));
    }
    budget::admit(&prepared.voices)
}
pub fn classic_audio_preparation_mode_for_processing(
    processing: AudioProcessing,
) -> Result<ClassicAudioPreparationMode, CompositionError> {
    match processing {
        AudioProcessing::LinearInterpolationZeroTail => {
            Ok(ClassicAudioPreparationMode::UnpreparedLinear)
        }
        AudioProcessing::ClassicPreparedLinear => Ok(ClassicAudioPreparationMode::PreparedLinear),
        AudioProcessing::NativeMaintainPitch => Ok(ClassicAudioPreparationMode::PreparedSoundtouch),
        AudioProcessing::LegacyFfmpegResample => Err(unsupported(
            "Legacy FFmpeg PCM is not a Classic prepared buffer",
        )),
    }
}
fn format_valid(format: &RuntimeSourceFormat) -> Result<(), CompositionError> {
    if format.element_id.is_empty()
        || format.sample_rate == 0
        || format.sample_rate > 384_000
        || format.frame_count == 0
        || !(1..=64).contains(&format.channels)
    {
        return Err(invalid(
            "Actual runtime PCM requires an element ID, positive length, 1..384000 Hz and 1..64 channels",
        ));
    }
    Ok(())
}

pub fn bind_classic_audio_pcm_scene<'a>(
    prepared: &'a PreparedAudio,
    formats: &[RuntimeSourceFormat],
) -> Result<BoundAudioPcm<'a>, CompositionError> {
    classic(prepared)?;
    if formats.len() != prepared.voices.len() {
        return Err(invalid(
            "Runtime PCM must bind every participating voice exactly once",
        ));
    }
    let mut by_id = HashMap::with_capacity(formats.len());
    for format in formats {
        format_valid(format)?;
        if by_id.insert(format.element_id.as_str(), format).is_some() {
            return Err(invalid("Runtime PCM contains a duplicate element ID"));
        }
    }
    let mut bound = Vec::with_capacity(prepared.voices.len());
    for voice in &prepared.voices {
        let format = by_id.remove(voice.element_id.as_str()).ok_or_else(|| {
            invalid("Runtime PCM contains an unknown element or omits a participating voice")
        })?;
        let expected = classic_audio_preparation_mode_for_processing(voice.processing)?;
        if format.mode != expected {
            return Err(invalid(
                "Runtime PCM preparation mode does not match its fixed voice",
            ));
        }
        if expected != ClassicAudioPreparationMode::UnpreparedLinear {
            let authored_frames = clocks::sample_index(
                time(&voice.timeline_duration)?,
                prepared.identity.format.sample_rate,
                clocks::Rounding::Ceil,
            )?
            .max(1);
            if format.sample_rate != prepared.identity.format.sample_rate
                || format.frame_count != authored_frames
                || format.channels > 2
            {
                return Err(invalid(
                    "Prepared PCM requires target sample rate, exact authored output length and mono/stereo",
                ));
            }
        }
        bound.push(format.clone());
    }
    Ok(BoundAudioPcm {
        prepared,
        formats: bound,
    })
}

fn interval(start: u64, count: u32) -> Result<u64, CompositionError> {
    if count == 0 || count > MAX_AUDIO_BLOCK_FRAMES {
        return Err(invalid("Runtime audio block must contain 1..4096 frames"));
    }
    start
        .checked_add(u64::from(count))
        .ok_or_else(|| invalid("Runtime audio sample boundary overflows u64"))
}

/// Original mapping for prepared-linear's intermediate Float32 write. This
/// intentionally runs before final buffer binding, so there is no preparation
/// dependency cycle. Position generation does not claim decoded samples exist.
pub fn compile_original_audio_positions_scene(
    prepared: &PreparedAudio,
    element_id: &str,
    local_start_sample: u64,
    frame_count: u32,
    decoded_sample_rate: u32,
) -> Result<Vec<ExactSourceSample>, CompositionError> {
    classic(prepared)?;
    let end = interval(local_start_sample, frame_count)?;
    if decoded_sample_rate == 0 || decoded_sample_rate > 384_000 {
        return Err(invalid(
            "Actual decoded PCM sample rate is outside 1..384000 Hz",
        ));
    }
    let voice = prepared
        .voices
        .iter()
        .find(|v| v.element_id == element_id)
        .ok_or_else(|| invalid("Original source positions require a participating element"))?;
    if end > voice.end - voice.start {
        return Err(invalid(
            "Original source positions exceed the authored output span",
        ));
    }
    (local_start_sample..end)
        .map(|sample| voice.source.position_at(sample, decoded_sample_rate))
        .collect()
}

/// Dense executable facts, retaining the authored gain clocks and original
/// revision. Prepared PCM advances from zero at unit rate; original PCM uses
/// the exact effective source rate and trim at its real decoded sample rate.
pub fn compile_bound_audio_block_scene(
    bound: &BoundAudioPcm<'_>,
    start_sample: u64,
    frame_count: u32,
) -> Result<RuntimeAudioBlock, CompositionError> {
    interval(start_sample, frame_count)?;
    let block = compile_audio_block_scene(bound.prepared, start_sample, frame_count)?;
    let mut voices = Vec::with_capacity(block.voices.len());
    for voice in block.voices {
        let format = bound
            .formats
            .iter()
            .find(|f| f.element_id == voice.element_id)
            .ok_or_else(|| invalid("Validated runtime PCM binding is incomplete"))?;
        let prepared_source = format.mode != ClassicAudioPreparationMode::UnpreparedLinear;
        let end = voice
            .local_start_sample
            .checked_add(u64::from(voice.frame_count))
            .ok_or_else(|| invalid("Runtime voice source boundary overflow"))?;
        let positions = (voice.local_start_sample..end)
            .map(|sample| {
                if prepared_source {
                    Ok(ExactSourceSample {
                        lower: sample,
                        fraction_numerator: 0,
                        fraction_denominator: 1,
                    })
                } else {
                    voice.source.position_at(sample, format.sample_rate)
                }
            })
            .collect::<Result<Vec<_>, CompositionError>>()?;
        let amplitudes = match voice.gain {
            SampleGain::Constant { amplitude } => vec![amplitude; voice.frame_count as usize],
            SampleGain::Dense { amplitudes } => amplitudes,
        };
        voices.push(RuntimeAudioVoiceBlock {
            element_id: voice.element_id,
            block_offset: voice.block_offset,
            frame_count: voice.frame_count,
            positions,
            amplitudes,
        });
    }
    Ok(RuntimeAudioBlock {
        identity: block.identity,
        start_sample: block.start_sample,
        frame_count: block.frame_count,
        voices,
        receipts: block.receipts,
    })
}

pub(super) fn serialize_u64<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}
pub(super) fn serialize_u128<S: Serializer>(
    value: &u128,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}
fn serialize_identity<S: Serializer>(
    value: &AudioIdentity,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut identity = serializer.serialize_struct("AudioIdentityV4", 6)?;
    identity.serialize_field("projectId", &value.project_id)?;
    identity.serialize_field("revision", &value.revision)?;
    identity.serialize_field("sequenceId", &value.sequence_id)?;
    identity.serialize_field(
        "profile",
        &match value.profile {
            AudioExecutionProfile::ClassicV31 => "classic-export-v31-web-audio",
            AudioExecutionProfile::LegacyFfmpegV3 => "legacy-ffmpeg-v3",
        },
    )?;
    identity.serialize_field("sampleRate", &value.format.sample_rate)?;
    identity.serialize_field("channels", &value.format.channels)?;
    identity.end()
}
pub(super) mod decimal_u64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub(in crate::scene::audio) fn serialize<S: Serializer>(
        value: &u64,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        super::serialize_u64(value, serializer)
    }
    pub(in crate::scene::audio) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|c| c.is_ascii_digit())
        {
            return Err(D::Error::custom(
                "Expected an unsigned canonical decimal string",
            ));
        }
        value.parse().map_err(D::Error::custom)
    }
}
