//! Execute fixed application audio facts through native decode, preparation,
//! Float32 mixing and continuous whole-scene mastering. Publication and process
//! ownership stay in the host; this engine accepts neither paths nor projects.
use super::{
    AudioSession, callback,
    dsp::*,
    invalid,
    pcm::*,
    source::{self, DecodeConfig, DecodedSource},
};
use opencut_composition::scene::{
    AudioExecutionProfile, ClassicAudioPreparationFacts, ClassicAudioPreparationMode,
    ExactSourceSample,
};
use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::*;
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, rc::Rc};
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
}
fn sample_facts(p: &ExactSourceSample) -> Result<(u64, u128, u128)> {
    if p.fraction_denominator == 0 || p.fraction_numerator >= p.fraction_denominator {
        return Err(invalid("Invalid native compiled source fraction"));
    }
    Ok((p.lower, p.fraction_numerator, p.fraction_denominator))
}
struct Owner {
    source: Rc<PcmSource>,
    remaining: usize,
    ordinary: bool,
}

pub(crate) fn render<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
    session: &mut S,
    resources: &mut R,
    handles: &[ClassicAudioResourceHandle],
    sink: &mut dyn ClassicAudioPcmSink,
    config: &DecodeConfig,
    engine_identity: &str,
) -> Result<ClassicAudioRenderReceipt> {
    source::check(resources, config.deadline)?;
    if config.max_pcm_bytes == 0
        || config.max_pcm_bytes > 512 * 1024 * 1024
        || config.max_encoded_bytes == 0
        || config.max_encoded_bytes > 32 * 1024 * 1024
    {
        return Err(invalid("Invalid native audio PCM or encoded admission"));
    }
    let raw_plan = session.plan()?;
    let plan: Plan = serde_json::from_value(raw_plan.clone())
        .map_err(|_| invalid("Invalid native fixed audio plan"))?;
    let frames = decimal(&plan.frame_count)?;
    let output_frames = decimal(&plan.output_frame_count)?;
    let range_start = plan
        .range
        .as_ref()
        .map(|r| decimal(&r.start_sample))
        .transpose()?
        .unwrap_or(0);
    let range_count = plan
        .range
        .as_ref()
        .map(|r| decimal(&r.frame_count))
        .transpose()?
        .unwrap_or(frames);
    if plan.identity.profile != "classic-export-v31-web-audio"
        || plan.identity.sample_rate != config.sample_rate
        || plan.identity.channels != 2
        || !(8000..=192000).contains(&config.sample_rate)
        || [
            &plan.identity.project_id,
            &plan.identity.sequence_id,
            &plan.identity.revision,
        ]
        .iter()
        .any(|s| s.is_empty())
        || frames == 0
        || frames > usize::MAX as u64 / 2
        || output_frames != range_count
        || range_count == 0
        || range_start
            .checked_add(range_count)
            .is_none_or(|n| n > frames)
        || plan.resources.len() > 32
        || plan.voices.len() > 256
        || handles.len() != plan.resources.len()
        || plan.encoded_source.mode != "verified-range"
        || !(65536..=8 * 1024 * 1024).contains(&plan.encoded_source.cache_bytes)
        || engine_identity.is_empty()
        || engine_identity.len() > 256
    {
        return Err(invalid(
            "Native audio plan exceeds its fixed execution envelope",
        ));
    }
    let by_asset: BTreeMap<_, _> = plan
        .resources
        .iter()
        .map(|r| (r.asset_id.as_str(), r))
        .collect();
    let order: BTreeMap<_, _> = plan
        .voices
        .iter()
        .enumerate()
        .map(|(i, v)| (v.element_id.as_str(), i))
        .collect();
    let handle_map: BTreeMap<_, _> = handles
        .iter()
        .map(|h| (h.asset_id.as_str(), h.handle.as_str()))
        .collect();
    if by_asset.len() != plan.resources.len()
        || order.len() != plan.voices.len()
        || handle_map.len() != handles.len()
        || plan
            .voices
            .iter()
            .any(|v| v.element_id.is_empty() || !by_asset.contains_key(v.asset_id.as_str()))
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
    {
        return Err(invalid(
            "Native audio resources and voices are not uniquely bound",
        ));
    }
    let memory = PcmMemory::new(config.max_pcm_bytes)?;
    let mut output = memory.array(frames as usize * 2)?;
    let mut owners = BTreeMap::<String, Owner>::new();
    let mut sources = BTreeMap::<String, Rc<PcmSource>>::new();
    let mut preparation = Vec::new();
    let mut decoders = Vec::new();
    for voice in &plan.voices {
        source::check(resources, config.deadline)?;
        if !owners.contains_key(&voice.asset_id) {
            let resource = by_asset[voice.asset_id.as_str()];
            let DecodedSource {
                pcm,
                primary,
                profile,
                track,
            } = source::decode(
                session,
                resources,
                resource,
                handle_map[voice.asset_id.as_str()],
                config,
                &memory,
            )?;
            if track.sample_rate == 0 || track.channels == 0 {
                return Err(invalid("Native decoder omitted its actual format"));
            }
            decoders.push(ClassicAudioDecoderReceipt {
                asset_id: voice.asset_id.clone(),
                profile: profile.name().into(),
                sample_rate: pcm.sample_rate,
                frame_count: pcm.frames.to_string(),
                channels: pcm.channels,
                primary,
            });
            owners.insert(
                voice.asset_id.clone(),
                Owner {
                    source: Rc::new(pcm),
                    ordinary: false,
                    remaining: plan
                        .voices
                        .iter()
                        .filter(|v| v.asset_id == voice.asset_id)
                        .count(),
                },
            );
        }
        let owner = owners
            .get_mut(&voice.asset_id)
            .expect("opened native source");
        let (prepared, facts) = prepare(
            session,
            resources,
            &voice.element_id,
            &owner.source,
            config,
            &memory,
        )?;
        owner.ordinary |= facts.mode == ClassicAudioPreparationMode::UnpreparedLinear;
        owner.remaining -= 1;
        let retire = owner.remaining == 0 && !owner.ordinary;
        sources.insert(voice.element_id.clone(), prepared);
        preparation.push(ClassicAudioPreparationReceipt {
            element_id: voice.element_id.clone(),
            facts,
        });
        if retire {
            release(session, resources, by_asset[voice.asset_id.as_str()])?;
            owners.remove(&voice.asset_id);
        }
    }
    let formats: Vec<_> = plan
        .voices
        .iter()
        .zip(&preparation)
        .map(|(v, p)| {
            let source = &sources[&v.element_id];
            FinalSourceFormat {
                element_id: v.element_id.clone(),
                sample_rate: source.sample_rate,
                frame_count: source.frames.to_string(),
                channels: source.channels,
                prepared: p.facts.mode != ClassicAudioPreparationMode::UnpreparedLinear,
            }
        })
        .collect();
    let mut start = 0;
    while start < frames {
        source::check(resources, config.deadline)?;
        let requested = (frames - start).min(4096) as u32;
        let block = session.compile_block(
            BlockRequest {
                start_sample: start.to_string(),
                frame_count: requested,
                source_formats: formats.clone(),
            },
            resources,
        )?;
        if block.identity.project_id != plan.identity.project_id
            || block.identity.revision != plan.identity.revision
            || block.identity.sequence_id != plan.identity.sequence_id
            || block.identity.profile != AudioExecutionProfile::ClassicV31
            || block.identity.format.sample_rate != plan.identity.sample_rate
            || block.identity.format.channels != plan.identity.channels
            || block.start_sample != start
            || block.frame_count == 0
            || block.frame_count > requested
            || block.voices.len() > 32
            || block.receipts.len() > 256
        {
            return Err(invalid(
                "Compiled native block changed its fixed identity or interval",
            ));
        }
        let mut previous = None;
        let mut work = 0usize;
        for v in &block.voices {
            let index = *order
                .get(v.element_id.as_str())
                .ok_or_else(|| invalid("Unknown native compiled voice"))?;
            if previous.is_some_and(|p| p >= index)
                || !sources.contains_key(&v.element_id)
                || v.frame_count == 0
                || v.block_offset
                    .checked_add(v.frame_count)
                    .is_none_or(|n| n > block.frame_count)
                || v.positions.len() != v.frame_count as usize
                || v.amplitudes.len() != v.frame_count as usize
                || v.amplitudes.iter().any(|a| !a.is_finite() || *a < 0.0)
            {
                return Err(invalid(
                    "Native compiled voice changed stored order or sample facts",
                ));
            }
            previous = Some(index);
            work += v.frame_count as usize;
        }
        if work > 65536 {
            return Err(invalid(
                "Native compiled block exceeds voice sample admission",
            ));
        }
        for v in &block.voices {
            let source = &sources[&v.element_id];
            for (i, position) in v.positions.iter().enumerate() {
                let (lower, numerator, denominator) = sample_facts(position)?;
                for c in 0..2 {
                    let at = (start as usize + v.block_offset as usize + i) * 2 + c;
                    let value = f64::from(output.as_slice()[at])
                        + source.linear(
                            (c as u32).min(source.channels - 1),
                            lower,
                            numerator,
                            denominator,
                        )? * v.amplitudes[i];
                    if !value.is_finite() || !(value as f32).is_finite() {
                        return Err(invalid("Native PCM accumulation overflowed"));
                    }
                    output.as_mut_slice()[at] = value as f32;
                }
            }
        }
        start += u64::from(block.frame_count);
    }
    // Master the full mixed scene before selecting the published range. Codec,
    // prepared inputs and their aliases have no further consumers here.
    sources.clear();
    for asset_id in owners.keys() {
        release(session, resources, by_asset[asset_id.as_str()])?;
    }
    owners.clear();
    let mut peak = 0.0f64;
    for block in output.as_slice().chunks(8192) {
        source::check(resources, config.deadline)?;
        peak = peak.max(mastering_required(block)?.0);
    }
    let applied = peak > 0.98;
    if applied {
        let mut compressor = ClassicCompressor::new(config.sample_rate, &memory)?;
        compressor.process(output.as_mut_slice(), &mut || {
            source::check(resources, config.deadline)
        })?;
        for block in output.as_mut_slice().chunks_mut(8192) {
            source::check(resources, config.deadline)?;
            clip_export(block);
        }
    }
    let mut chunk = memory.array(range_count.min(AUDIO_PCM_CHUNK_FRAMES as u64) as usize)?;
    for c in 0..2 {
        let mut offset = 0;
        while offset < range_count {
            source::check(resources, config.deadline)?;
            let n = (range_count - offset).min(chunk.len() as u64) as usize;
            for i in 0..n {
                chunk.as_mut_slice()[i] =
                    output.as_slice()[((range_start + offset) as usize + i) * 2 + c as usize];
            }
            sink.chunk(c, offset, &chunk.as_slice()[..n])?;
            source::check(resources, config.deadline)?;
            offset += n as u64;
        }
    }
    let receipt = ClassicAudioRenderReceipt {
        identity: plan.identity,
        engine_identity: engine_identity.into(),
        decoders,
        preparation,
        mastering: ClassicAudioMasteringReceipt {
            profile: "classic-export-whole-scene-web-audio-v31".into(),
            whole_scene_frames: plan.frame_count,
            input_peak: peak,
            applied,
        },
        range: plan.range,
        peak_accounted_bytes: memory.peak_bytes(),
    };
    source::check(resources, config.deadline)?;
    Ok(receipt)
}
fn release<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
    session: &mut S,
    resources: &mut R,
    resource: &AudioResource,
) -> Result<()> {
    if callback(
        session,
        resources,
        "releaseDecodedSource",
        json!({"resource":resource}),
    )?["released"]
        != true
    {
        return Err(invalid(
            "Native decoded source retirement was not acknowledged",
        ));
    }
    Ok(())
}
fn prepare<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
    session: &mut S,
    resources: &mut R,
    element: &str,
    decoded: &Rc<PcmSource>,
    config: &DecodeConfig,
    memory: &PcmMemory,
) -> Result<(Rc<PcmSource>, ClassicAudioPreparationFacts)> {
    let facts: ClassicAudioPreparationFacts = serde_json::from_value(callback(session, resources,
        "compilePitchPreparation", json!({"elementId":element,"decodedSampleRate":decoded.sample_rate,
            "decodedFrameCount":decoded.frames.to_string(),"decodedChannels":decoded.channels,
            "residentFrameCount":if decoded.sparse { Some(decoded.resident_frames().to_string()) } else { None }}))?)
        .map_err(|_| invalid("Invalid native pitch preparation facts"))?;
    if facts.decoded_sample_rate != decoded.sample_rate
        || facts.target_sample_rate != config.sample_rate
        || facts.num_channels != decoded.channels.min(2)
        || facts.output_frames == 0
        || facts.output_frames > usize::MAX as u64 / 2
    {
        return Err(invalid(
            "Native pitch facts changed the observed source format",
        ));
    }
    if facts.mode == ClassicAudioPreparationMode::UnpreparedLinear {
        return Ok((decoded.clone(), facts));
    }
    let source = if facts.mode == ClassicAudioPreparationMode::PreparedLinear {
        let mut planes: Vec<_> = (0..facts.num_channels)
            .map(|_| memory.array(facts.output_frames as usize))
            .collect::<Result<_>>()?;
        let mut start = 0;
        while start < facts.output_frames {
            source::check(resources, config.deadline)?;
            let count = (facts.output_frames - start).min(4096) as usize;
            let positions: Vec<ExactSourceSample> = serde_json::from_value(callback(session, resources, "compileSourcePositions",
                json!({"elementId":element,"localStartSample":start.to_string(),"frameCount":count,"sourceSampleRate":decoded.sample_rate}))?)
                .map_err(|_| invalid("Invalid native prepared-linear positions"))?;
            if positions.len() != count {
                return Err(invalid(
                    "Native preparation changed the requested frame count",
                ));
            }
            for (i, p) in positions.iter().enumerate() {
                let (lower, n, d) = sample_facts(p)?;
                for c in 0..facts.num_channels {
                    planes[c as usize].as_mut_slice()[start as usize + i] =
                        decoded.linear(c, lower, n, d)? as f32;
                }
            }
            start += count as u64;
        }
        PcmSource::dense(config.sample_rate, Samples::Planar(planes))?
    } else {
        let actual = facts.actual_source_frames;
        let slice_start = facts.source_slice_start_sample.min(decoded.frames);
        if actual == 0
            || actual
                != facts
                    .requested_source_frames
                    .min(decoded.frames - slice_start)
                    .max(1)
            || actual > usize::MAX as u64
            || facts.resampled_frames == 0
            || facts.resampled_frames > usize::MAX as u64 / 2
        {
            return Err(invalid(
                "Native pitch slice disagrees with actual decoded availability",
            ));
        }
        let tempo = f64::from(facts.tempo.numerator) / f64::from(facts.tempo.denominator);
        if !(0.01..=5.0).contains(&tempo) || (tempo - 1.0).abs() <= 1e-6 {
            return Err(invalid(
                "Native SoundTouch facts changed the selected DSP branch",
            ));
        }
        let mut interleaved = memory.array(facts.resampled_frames as usize * 2)?;
        for c in 0..facts.num_channels {
            source::check(resources, config.deadline)?;
            let mut input = memory.array(actual as usize)?;
            for (n, block) in input.as_mut_slice().chunks_mut(4096).enumerate() {
                source::check(resources, config.deadline)?;
                decoded.slice_channel(c, slice_start + n as u64 * 4096, block)?;
            }
            let mut resampled = memory.array(facts.resampled_frames as usize)?;
            resample_preparation(
                input.as_slice(),
                resampled.as_mut_slice(),
                decoded.sample_rate,
                config.sample_rate,
                &mut || source::check(resources, config.deadline),
            )?;
            for (i, v) in resampled.as_slice().iter().enumerate() {
                if i % 4096 == 0 {
                    source::check(resources, config.deadline)?;
                }
                interleaved.as_mut_slice()[i * 2 + c as usize] = *v;
                if facts.num_channels == 1 {
                    interleaved.as_mut_slice()[i * 2 + 1] = *v;
                }
            }
        }
        let stretched = render_preserved_pitch(
            interleaved.as_slice(),
            tempo,
            facts.output_frames as usize,
            memory,
            &mut || source::check(resources, config.deadline),
        )?;
        drop(interleaved);
        if facts.num_channels == 2 {
            PcmSource::dense(config.sample_rate, Samples::Stereo(stretched))?
        } else {
            let mut mono = memory.array(facts.output_frames as usize)?;
            for (i, v) in mono.as_mut_slice().iter_mut().enumerate() {
                *v = stretched.as_slice()[i * 2];
            }
            PcmSource::dense(config.sample_rate, Samples::Planar(vec![mono]))?
        }
    };
    Ok((Rc::new(source), facts))
}
