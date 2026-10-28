use super::{
    channel::{Channel, Resources, Session},
    identity,
    preview::{PreviewEngine, PreviewMaster},
    preview_protocol::{BlockReceipt, Policy},
    protocol::{self, error},
    source::DecodeConfig,
};
use crate::native_process::wire;
use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::*;
use serde_json::json;
use std::{
    cell::RefCell,
    io::{Read, Write},
    rc::Rc,
    time::{Duration, Instant},
};
pub(super) fn serve(
    packet: wire::Packet,
    input: &mut dyn Read,
    output: &mut dyn Write,
) -> Result<(), EditorError> {
    let policy: Policy = serde_json::from_value(packet.meta["policy"].clone())
        .map_err(|_| error("Invalid native preview policy"))?;
    policy.validate()?;
    let started = Instant::now();
    let lifetime = started + Duration::from_millis(policy.lifetime_deadline_ms);
    let identity = identity::observe()?;
    validate_native_identity(&identity)?;
    wire::send(
        output,
        json!({"type":"ready","purpose":"audio_preview","pid":std::process::id(),"identity":identity}),
        &[],
    )?;
    let packet = wire::read(input)?;
    if packet.meta["type"] != "preview_prepare"
        || wire::integer(&packet.meta, "frame")? != 0
        || !packet.data.is_empty()
    {
        return Err(error("Invalid native preview preparation command"));
    }
    let handles: Vec<ClassicAudioResourceHandle> =
        serde_json::from_value(packet.meta["handles"].clone())
            .map_err(|_| error("Invalid native preview handles"))?;
    let sample_rate = u32::try_from(wire::integer(
        &packet.meta["plan"]["identity"],
        "sampleRate",
    )?)
    .map_err(|_| error("Invalid native preview rate"))?;
    let channel = Rc::new(RefCell::new(Channel {
        input,
        output,
        request: 0,
        frame: 0,
        deadline: started + Duration::from_millis(policy.preparation_deadline_ms),
    }));
    let mut resources = Resources {
        channel: channel.clone(),
    };
    let mut session = Session {
        channel: channel.clone(),
        plan: packet.meta["plan"].clone(),
    };
    let config = DecodeConfig {
        sample_rate,
        max_pcm_bytes: policy.max_pcm_bytes,
        max_encoded_bytes: policy.max_encoded_bytes,
        deadline: channel.borrow().deadline,
    };
    let mut engine = PreviewEngine::prepare(
        &mut session,
        &mut resources,
        &handles,
        &config,
        &identity.worker_sha256,
    )?;
    let data = protocol::encode(&engine.receipt)?;
    {
        let mut io = channel.borrow_mut();
        wire::send(
            io.output,
            json!({"type":"preview_prepared","frame":0,"length":data.len()}),
            &[],
        )?;
        for (n, chunk) in data.chunks(wire::DATA_BYTES).enumerate() {
            wire::send(
                io.output,
                json!({"type":"result_chunk","frame":0,"offset":n*wire::DATA_BYTES}),
                chunk,
            )?;
        }
        io.deadline = lifetime;
    }
    let mut frame = 0u64;
    let mut master: Option<PreviewMaster> = None;
    loop {
        let packet = wire::read(channel.borrow_mut().input)?;
        frame = frame
            .checked_add(1)
            .ok_or_else(|| error("Native preview command counter overflow"))?;
        if wire::integer(&packet.meta, "frame")? != frame || !packet.data.is_empty() {
            return Err(error("Native preview command order changed"));
        }
        let deadline =
            (Instant::now() + Duration::from_millis(policy.operation_deadline_ms)).min(lifetime);
        {
            let mut io = channel.borrow_mut();
            io.frame = frame;
            io.request = 0;
            io.deadline = deadline;
        }
        resources.checkpoint()?;
        match wire::text(&packet.meta, "type")? {
            "preview_next" => {
                let generation = wire::integer(&packet.meta, "generation")?;
                let epoch = wire::integer(&packet.meta, "overlay_epoch")?;
                let start = wire::integer(&packet.meta, "start_sample")?;
                let count = u32::try_from(wire::integer(&packet.meta, "frame_count")?)
                    .map_err(|_| error("Invalid native preview block size"))?;
                if master
                    .as_ref()
                    .is_none_or(|m| !m.matches(generation, epoch))
                {
                    master = Some(PreviewMaster::new(
                        sample_rate,
                        engine.memory(),
                        generation,
                        epoch,
                        start,
                    )?);
                }
                let block = master.as_mut().unwrap().next(
                    &mut engine,
                    &mut session,
                    &mut resources,
                    start,
                    count,
                    &DecodeConfig { deadline, ..config },
                )?;
                for c in 0..2 {
                    resources.checkpoint()?;
                    let data: Vec<u8> = block
                        .pcm
                        .as_slice()
                        .chunks_exact(2)
                        .flat_map(|v| v[c].to_le_bytes())
                        .collect();
                    wire::send(
                        channel.borrow_mut().output,
                        json!({"type":"preview_pcm","frame":frame,"channel":c,"count":count,"generation":generation,"overlay_epoch":epoch,"start_sample":start}),
                        &data,
                    )?;
                }
                let receipt = BlockReceipt {
                    identity: engine.receipt.identity.clone(),
                    engine_identity: identity.worker_sha256.clone(),
                    generation,
                    overlay_epoch: epoch,
                    start_sample: start.to_string(),
                    frame_count: count,
                    lookahead_frames: block.lookahead_frames,
                    inputs: block.inputs,
                    peak_accounted_bytes: engine.memory().peak_bytes(),
                };
                resources.checkpoint()?;
                wire::send(
                    channel.borrow_mut().output,
                    json!({"type":"preview_block","frame":frame,"receipt":receipt}),
                    &[],
                )?;
                channel.borrow_mut().deadline = lifetime;
            }
            "finish" => {
                drop(master.take());
                engine.close(&mut resources)?;
                if identity::observe()? != identity {
                    return Err(EditorError::new(
                        "AUDIO_ENGINE_CHANGED",
                        "Native audio runtime changed during preview",
                    ));
                }
                resources.checkpoint()?;
                return wire::send(
                    channel.borrow_mut().output,
                    json!({"type":"complete","frame":frame}),
                    &[],
                );
            }
            _ => return Err(error("Unknown native preview command")),
        }
    }
}
