//! Persistent native realtime execution. Ordinary sources retain PTS windows;
//! prepared voices reuse export's DSP. Project clocks, gain ramps and source
//! taps are compiled only by the shared application/composition ports.
use super::{
    AudioSession, callback,
    dsp::*,
    input::NativeInput,
    invalid,
    pcm::PcmSource,
    protocol, render,
    rolling::RollingSource,
    source::{self, DecodeConfig},
};
use opencut_composition::scene::*;
use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
type Result<T> = std::result::Result<T, EditorError>;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    identity: ClassicAudioIdentity,
    frame_count: String,
    output_frame_count: String,
    range: Option<ClassicAudioRange>,
    resources: Vec<AudioResource>,
    voices: Vec<Voice>,
    encoded_source: EncodedSource,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncodedSource {
    mode: String,
    cache_bytes: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Voice {
    element_id: String,
    asset_id: String,
    source_mode: SourceMode,
}
#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SourceMode {
    PreservePts,
    PreparedConcat,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct InputBinding {
    pub start_sample: String,
    pub frame_count: u32,
    pub binding_sha256: String,
}
pub(super) struct PreviewEngine {
    plan: Plan,
    pub receipt: ClassicAudioPreviewReceipt,
    rolling: BTreeMap<String, RollingSource>,
    prepared: BTreeMap<String, Rc<PcmSource>>,
    memory: PcmMemory,
    extent: u64,
    _metadata: PcmReservation,
}
pub(super) struct MixedBlock {
    pub pcm: PcmArray,
    pub binding: InputBinding,
}
impl PreviewEngine {
    pub fn prepare<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
        session: &mut S,
        resources: &mut R,
        handles: &[ClassicAudioResourceHandle],
        config: &DecodeConfig,
        engine: &str,
    ) -> Result<Self> {
        source::check(resources, config.deadline)?;
        let plan: Plan = serde_json::from_value(session.plan()?)
            .map_err(|_| invalid("Invalid native realtime audio plan"))?;
        let extent = decimal(&plan.frame_count)?;
        let resource_ids: BTreeSet<_> =
            plan.resources.iter().map(|r| r.asset_id.as_str()).collect();
        let voice_ids: BTreeSet<_> = plan.voices.iter().map(|v| v.element_id.as_str()).collect();
        let handle_map: BTreeMap<_, _> = handles
            .iter()
            .map(|h| (h.asset_id.as_str(), h.handle.as_str()))
            .collect();
        if plan.identity.profile != CLASSIC_REALTIME_AUDIO_PROFILE
            || plan.identity.sample_rate != config.sample_rate
            || plan.identity.channels != 2
            || !matches!(config.sample_rate, 44100 | 48000)
            || extent == 0
            || decimal(&plan.output_frame_count)? != extent
            || plan.range.is_some()
            || plan.resources.len() > 32
            || plan.voices.len() > 256
            || handles.len() != plan.resources.len()
            || resource_ids.len() != plan.resources.len()
            || voice_ids.len() != plan.voices.len()
            || handle_map.len() != handles.len()
            || plan
                .voices
                .iter()
                .any(|v| v.element_id.is_empty() || !resource_ids.contains(v.asset_id.as_str()))
            || plan.resources.iter().any(|r| {
                !handle_map.contains_key(r.asset_id.as_str())
                    || r.size == 0
                    || r.content_hash.len() != 64
                    || !r
                        .content_hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    || !matches!(r.kind.as_str(), "audio" | "video")
            })
            || plan.encoded_source.mode != "verified-range"
            || !(65536..=8 * 1024 * 1024).contains(&plan.encoded_source.cache_bytes)
            || config.max_pcm_bytes == 0
            || config.max_pcm_bytes > 256 * 1024 * 1024
            || config.max_encoded_bytes == 0
            || config.max_encoded_bytes > 32 * 1024 * 1024
            || engine.len() != 64
        {
            return Err(invalid(
                "Native realtime plan exceeds its fixed execution envelope",
            ));
        }
        let memory = PcmMemory::new(config.max_pcm_bytes)?;
        let metadata = memory.reserve(
            (4 * std::mem::size_of::<Self>()
                + plan.voices.len() * 4 * std::mem::size_of::<PreviewSourceFormat>()
                + plan.resources.len()
                    * 4
                    * std::mem::size_of::<ClassicAudioPreviewDecoderReceipt>()) as u64,
        )?;
        let mut result = Self {
            receipt: ClassicAudioPreviewReceipt {
                identity: plan.identity.clone(),
                engine_identity: engine.into(),
                decoders: Vec::new(),
                source_formats: Vec::new(),
                preparation: Vec::new(),
                peak_accounted_bytes: 0,
            },
            plan,
            rolling: BTreeMap::new(),
            prepared: BTreeMap::new(),
            memory,
            extent,
            _metadata: metadata,
        };
        let prepared: Result<()> = (|| {
            for index in 0..result.plan.resources.len() {
                source::check(resources, config.deadline)?;
                let resource = &result.plan.resources[index];
                // Ordinary sources retain one codec each. Admission is shared
                // across those owners rather than charging a whole-scene codec
                // workspace once for every otherwise tiny rolling window.
                let codec_bytes = (config.max_pcm_bytes
                    / (result.plan.resources.len().max(1) as u64 * 4))
                    .min(16 * 1024 * 1024);
                let mut input = Some(NativeInput::open(
                    session,
                    resources,
                    resource,
                    handle_map[resource.asset_id.as_str()],
                    config,
                    &result.memory,
                    codec_bytes,
                )?);
                let value: Result<()> = (|| {
                    let primary = input.as_ref().unwrap().primary.clone();
                    let allocation: PreviewDecodeAllocation = serde_json::from_value(callback(
                        session,
                        resources,
                        "admitPreviewDecode",
                        json!({"resource":resource,"primary":primary}),
                    )?)
                    .map_err(|_| invalid("Invalid native preview consumer allocation"))?;
                    let ordinary = result.plan.voices.iter().any(|v| {
                        v.asset_id == resource.asset_id && v.source_mode == SourceMode::PreservePts
                    });
                    let prepared = result.plan.voices.iter().any(|v| {
                        v.asset_id == resource.asset_id
                            && v.source_mode == SourceMode::PreparedConcat
                    });
                    if allocation.stream.is_some() != ordinary
                        || allocation.preparation.is_some() != prepared
                    {
                        return Err(invalid(
                            "Native preview allocation changed its fixed consumers",
                        ));
                    }
                    let decoded = if let Some(admission) = &allocation.preparation {
                        let encoded = serde_json::to_value(admission)
                            .map_err(|_| invalid("Invalid native preview preparation admission"))?;
                        Some(source::decode_preview_preparation(
                            session,
                            resources,
                            input.as_mut().unwrap(),
                            &encoded,
                            ordinary,
                            config,
                            &result.memory,
                        )?)
                    } else {
                        None
                    };
                    let native_frames = decoded.as_ref().map(|d| d.native_frames);
                    let observation = json!({"resource":resource,"sampleRate":primary.sample_rate,"frameCount":native_frames.unwrap_or(0).to_string(),"channels":primary.channels.min(2)});
                    if allocation
                        .preparation
                        .as_ref()
                        .is_some_and(|a| a.mode == "windowed")
                    {
                        callback(
                            session,
                            resources,
                            "observeWindowedPreviewDecoded",
                            json!({"decoded":observation,"residentFrameCount":decoded.as_ref().unwrap().pcm.resident_frames().to_string()}),
                        )?;
                    } else {
                        callback(session, resources, "observePreviewDecoded", observation)?;
                    }
                    result
                        .receipt
                        .decoders
                        .push(ClassicAudioPreviewDecoderReceipt {
                            asset_id: resource.asset_id.clone(),
                            primary: primary.clone(),
                            sample_rate: primary.sample_rate,
                            frame_count: native_frames.map(|n| n.to_string()),
                            channels: primary.channels.min(2),
                        });
                    if let Some(decoded) = decoded {
                        let source = Rc::new(decoded.pcm);
                        for voice in result.plan.voices.iter().filter(|v| {
                            v.asset_id == resource.asset_id
                                && v.source_mode == SourceMode::PreparedConcat
                        }) {
                            let (pcm, facts) = render::prepare(
                                session,
                                resources,
                                &voice.element_id,
                                &source,
                                config,
                                &result.memory,
                            )?;
                            if facts.mode == ClassicAudioPreparationMode::UnpreparedLinear {
                                return Err(invalid(
                                    "Native preview prepared voice did not prepare its source",
                                ));
                            }
                            result.prepared.insert(voice.element_id.clone(), pcm);
                            result
                                .receipt
                                .preparation
                                .push(ClassicAudioPreparationReceipt {
                                    element_id: voice.element_id.clone(),
                                    facts,
                                });
                        }
                        if callback(
                            session,
                            resources,
                            "releaseDecodedSource",
                            json!({"resource":resource}),
                        )?["released"]
                            != true
                        {
                            return Err(invalid(
                                "Native preview preparation retirement was not acknowledged",
                            ));
                        }
                    }
                    if let Some(admission) = allocation.stream {
                        result.rolling.insert(
                            resource.asset_id.clone(),
                            RollingSource::new(
                                &mut input,
                                admission,
                                config.sample_rate,
                                &result.memory,
                            )?,
                        );
                    }
                    Ok(())
                })();
                let released = input.map(|input| input.close(resources)).transpose();
                value?;
                released?;
            }
            let order: BTreeMap<_, _> = result
                .plan
                .voices
                .iter()
                .enumerate()
                .map(|(i, v)| (v.element_id.as_str(), i))
                .collect();
            result
                .receipt
                .preparation
                .sort_by_key(|p| order[p.element_id.as_str()]);
            for voice in &result.plan.voices {
                let format = if voice.source_mode == SourceMode::PreparedConcat {
                    let source = &result.prepared[&voice.element_id];
                    PreviewSourceFormat {
                        element_id: voice.element_id.clone(),
                        sample_rate: source.sample_rate,
                        frame_count: source.frames,
                        channels: source.channels,
                        prepared: true,
                        segments: Vec::new(),
                    }
                } else {
                    let source = &result.rolling[&voice.asset_id];
                    PreviewSourceFormat {
                        element_id: voice.element_id.clone(),
                        sample_rate: source.sample_rate(),
                        frame_count: 0,
                        channels: source.channels(),
                        prepared: false,
                        segments: Vec::new(),
                    }
                };
                result.receipt.source_formats.push(format);
            }
            callback(
                session,
                resources,
                "bindPreviewSources",
                json!({"sourceFormats":result.receipt.source_formats,"window":null}),
            )?;
            source::check(resources, config.deadline)?;
            Ok(())
        })();
        if let Err(error) = prepared {
            let _ = result.close(resources);
            return Err(error);
        }
        result.receipt.peak_accounted_bytes = result.memory.peak_bytes();
        Ok(result)
    }
    pub fn extent(&self) -> u64 {
        self.extent
    }
    pub fn memory(&self) -> &PcmMemory {
        &self.memory
    }
    pub fn raw_block<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
        &mut self,
        session: &mut S,
        resources: &mut R,
        start: u64,
        count: u32,
        config: &DecodeConfig,
    ) -> Result<MixedBlock> {
        source::check(resources, config.deadline)?;
        if !(1..=AUDIO_PCM_CHUNK_FRAMES as u32).contains(&count)
            || start
                .checked_add(u64::from(count))
                .is_none_or(|end| end > self.extent)
        {
            return Err(invalid("Native preview block exceeds its fixed scene"));
        }
        let mut request = PreviewBlockRequest {
            start_sample: start.to_string(),
            frame_count: count,
        };
        let admitted = callback(
            session,
            resources,
            "admitPreviewBlock",
            serde_json::to_value(&request).unwrap(),
        )?
        .as_u64()
        .filter(|n| *n > 0 && *n <= u64::from(count))
        .ok_or_else(|| invalid("Invalid admitted native preview prefix"))?
            as u32;
        let count = admitted;
        request.frame_count = count;
        let windows: Vec<PreviewSourceWindow> = serde_json::from_value(callback(
            session,
            resources,
            "previewSourceWindows",
            serde_json::to_value(&request).unwrap(),
        )?)
        .map_err(|_| invalid("Invalid native preview source windows"))?;
        if windows.len() > 32
            || windows
                .iter()
                .any(|w| !self.rolling.contains_key(&w.asset_id))
        {
            return Err(invalid(
                "Rust source windows changed the fixed resource set",
            ));
        }
        let mut current = BTreeMap::new();
        for (id, source) in &mut self.rolling {
            current.insert(id.clone(), source.ensure(resources, &windows, config)?);
        }
        // Admit all per-voice metadata copies before cloning a shared asset's
        // segment table. The transport has its separate 16 MiB fact envelope;
        // it is not permission for unaccounted expanded source descriptors.
        let binding_bytes = self
            .plan
            .voices
            .iter()
            .zip(&self.receipt.source_formats)
            .try_fold(0u64, |sum, (voice, format)| {
                let segments = if format.prepared {
                    0
                } else {
                    current[&voice.asset_id].len()
                };
                let bytes = (segments as u64)
                    .checked_mul(std::mem::size_of::<PreviewAudioSegment>() as u64)
                    .and_then(|n| {
                        n.checked_add(
                            (std::mem::size_of::<PreviewSourceFormat>() + format.element_id.len())
                                as u64,
                        )
                    })
                    .ok_or_else(|| invalid("Native preview binding residency overflow"))?;
                sum.checked_add(bytes)
                    .ok_or_else(|| invalid("Native preview binding residency overflow"))
            })?;
        let _binding_memory = self.memory.reserve(binding_bytes)?;
        let mut formats = self.receipt.source_formats.clone();
        for (voice, format) in self.plan.voices.iter().zip(&mut formats) {
            if !format.prepared {
                format.segments = current[&voice.asset_id].clone();
                format.frame_count = format.segments.iter().map(|p| p.frame_count).sum();
            }
        }
        let binding = BindPreviewSources {
            source_formats: formats,
            window: Some(request),
        };
        let binding_sha256 = format!("{:x}", Sha256::digest(protocol::encode(&binding)?));
        let block = session.compile_preview(binding, resources)?;
        let identity = &self.plan.identity;
        if block.identity.project_id != identity.project_id
            || block.identity.revision != identity.revision
            || block.identity.sequence_id != identity.sequence_id
            || block.identity.profile != CLASSIC_REALTIME_AUDIO_PROFILE
            || block.identity.sample_rate != identity.sample_rate
            || block.identity.channels != 2
            || block.start_sample != start
            || block.frame_count != count
            || block.voices.len() > 32
            || block.receipts.len() > 256
        {
            return Err(invalid(
                "Compiled native preview changed its command or fixed identity",
            ));
        }
        let order: BTreeMap<_, _> = self
            .plan
            .voices
            .iter()
            .enumerate()
            .map(|(i, v)| (v.element_id.as_str(), (i, v)))
            .collect();
        let mut previous = None;
        let mut work = 0usize;
        let mut output = self.memory.array(count as usize * 2)?;
        for voice in &block.voices {
            let &(ordinal, fixed) = order
                .get(voice.element_id.as_str())
                .ok_or_else(|| invalid("Unknown native preview voice"))?;
            if previous.is_some_and(|p| p >= ordinal)
                || voice.frame_count == 0
                || voice
                    .block_offset
                    .checked_add(voice.frame_count)
                    .is_none_or(|n| n > count)
                || voice.positions.len() != voice.frame_count as usize
                || voice.amplitudes.len() != voice.frame_count as usize
                || voice.amplitudes.iter().any(|g| !g.is_finite())
            {
                return Err(invalid(
                    "Compiled native preview changed stored order or sample facts",
                ));
            }
            previous = Some(ordinal);
            work += voice.frame_count as usize;
            if work > 65536 {
                return Err(invalid("Native preview exceeds its admitted sample work"));
            }
            for (i, point) in voice.positions.iter().enumerate() {
                if i % 512 == 0 {
                    source::check(resources, config.deadline)?;
                }
                let Some(point) = point else {
                    continue;
                };
                if point.upper < point.lower
                    || point.upper - point.lower > 1
                    || !point.fraction.is_finite()
                    || !(0.0..=1.0).contains(&point.fraction)
                {
                    return Err(invalid("Invalid native compiled preview interpolation"));
                }
                for c in 0..2 {
                    let sample = |frame| -> Result<f32> {
                        if fixed.source_mode == SourceMode::PreparedConcat {
                            let pcm = &self.prepared[&voice.element_id];
                            if point.segment_id != 0 || frame >= pcm.frames {
                                return Err(invalid(
                                    "Compiled preview exceeds actual prepared PCM",
                                ));
                            }
                            pcm.value(c.min(pcm.channels - 1), frame)
                        } else {
                            let pcm = &self.rolling[&fixed.asset_id];
                            pcm.sample(point.segment_id, c.min(pcm.channels() - 1), frame)
                        }
                    };
                    let value = f64::from(sample(point.lower)?) * (1.0 - point.fraction)
                        + f64::from(sample(point.upper)?) * point.fraction;
                    let at = (voice.block_offset as usize + i) * 2 + c as usize;
                    let mixed = f64::from(output.as_slice()[at]) + value * voice.amplitudes[i];
                    if !mixed.is_finite() || !(mixed as f32).is_finite() {
                        return Err(invalid("Native preview mixing overflowed Float32"));
                    }
                    output.as_mut_slice()[at] = mixed as f32;
                }
            }
        }
        source::check(resources, config.deadline)?;
        Ok(MixedBlock {
            pcm: output,
            binding: InputBinding {
                start_sample: start.to_string(),
                frame_count: count,
                binding_sha256,
            },
        })
    }
    pub fn close<R: ClassicAudioResources + ?Sized>(mut self, resources: &mut R) -> Result<()> {
        self.prepared.clear();
        let mut first = None;
        for (_, source) in std::mem::take(&mut self.rolling) {
            if let Err(e) = source.close(resources) {
                if first.is_none() {
                    first = Some(e);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
}

/// The real-time compressor is always active, as in the former device graph.
/// Its declared integer predelay is consumed by bounded read-ahead: delivered
/// PCM already represents the requested content samples, so the device graph
/// must add no second compressor or synthetic latency. This is fixed DSP delay
/// accounting, not a fitted time offset against a reference waveform.
pub(super) struct PreviewMaster {
    compressor: ClassicCompressor,
    generation: u64,
    overlay_epoch: u64,
    next_output: u64,
    next_input: u64,
    warmup: usize,
}
pub(super) struct MasteredBlock {
    pub pcm: PcmArray,
    pub inputs: Vec<InputBinding>,
    pub lookahead_frames: u32,
}
impl PreviewMaster {
    pub fn new(
        rate: u32,
        memory: &PcmMemory,
        generation: u64,
        overlay_epoch: u64,
        start: u64,
    ) -> Result<Self> {
        let compressor = ClassicCompressor::new(rate, memory)?;
        let warmup = compressor.lookahead_frames();
        Ok(Self {
            compressor,
            generation,
            overlay_epoch,
            next_output: start,
            next_input: start,
            warmup,
        })
    }
    pub fn matches(&self, generation: u64, overlay_epoch: u64) -> bool {
        self.generation == generation && self.overlay_epoch == overlay_epoch
    }
    pub fn next<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
        &mut self,
        engine: &mut PreviewEngine,
        session: &mut S,
        resources: &mut R,
        start: u64,
        count: u32,
        config: &DecodeConfig,
    ) -> Result<MasteredBlock> {
        if start != self.next_output
            || count == 0
            || count > AUDIO_PCM_CHUNK_FRAMES as u32
            || start
                .checked_add(u64::from(count))
                .is_none_or(|n| n > engine.extent())
        {
            return Err(invalid(
                "Native realtime compressor changed its contiguous output interval",
            ));
        }
        let mut output = engine.memory.array(count as usize * 2)?;
        let mut written = 0usize;
        let mut inputs = Vec::new();
        while written < count as usize {
            source::check(resources, config.deadline)?;
            let wanted = (count as usize - written + self.warmup).min(AUDIO_PCM_CHUNK_FRAMES);
            let available = engine
                .extent()
                .saturating_sub(self.next_input)
                .min(wanted as u64) as usize;
            let mut pcm = if available > 0 {
                let block = engine.raw_block(
                    session,
                    resources,
                    self.next_input,
                    available as u32,
                    config,
                )?;
                inputs.push(block.binding);
                block.pcm
            } else {
                engine.memory.array(wanted * 2)?
            };
            // Split a short real prefix and the confirmed EOF zeros; never
            // interpret a missing in-bounds decoder window as silence.
            let frames = pcm.len() / 2;
            self.compressor.process(pcm.as_mut_slice(), &mut || {
                source::check(resources, config.deadline)
            })?;
            let discard = self.warmup.min(frames);
            self.warmup -= discard;
            let copied = (frames - discard).min(count as usize - written);
            output.as_mut_slice()[written * 2..(written + copied) * 2]
                .copy_from_slice(&pcm.as_slice()[discard * 2..(discard + copied) * 2]);
            written += copied;
            self.next_input = self
                .next_input
                .checked_add(frames as u64)
                .ok_or_else(|| invalid("Native preview read-ahead counter overflow"))?;
        }
        self.next_output += u64::from(count);
        Ok(MasteredBlock {
            pcm: output,
            inputs,
            lookahead_frames: self.compressor.lookahead_frames() as u32,
        })
    }
}
