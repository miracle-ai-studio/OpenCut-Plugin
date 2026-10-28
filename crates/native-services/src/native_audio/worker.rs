use super::{
    channel::{Channel, Output, Resources, Session},
    protocol::{self, error},
    source::DecodeConfig,
    *,
};
use crate::native_process::wire;
use opencut_editor_application::{
    classic_audio::*,
    scene_audio_jobs::{LocalAudioExecutionPolicy, validate_policy},
};
use serde_json::json;
use std::{
    cell::RefCell,
    io::{Read, Write},
    rc::Rc,
    time::{Duration, Instant},
};

pub(crate) fn serve_initialized(
    packet: wire::Packet,
    input: &mut dyn Read,
    output: &mut dyn Write,
) -> Result<(), EditorError> {
    if packet.meta["purpose"] == "audio_preview" {
        return super::preview_worker::serve(packet, input, output);
    }
    let policy: LocalAudioExecutionPolicy = serde_json::from_value(packet.meta["policy"].clone())
        .map_err(|_| error("Invalid native audio policy"))?;
    validate_policy(&policy)?;
    let deadline = Instant::now() + Duration::from_millis(policy.deadline_ms);
    let identity = identity::observe()?;
    validate_native_identity(&identity)?;
    wire::send(
        output,
        json!({"type":"ready","purpose":"audio_export","pid":std::process::id(),"identity":identity}),
        &[],
    )?;
    let packet = wire::read(input)?;
    if packet.meta["type"] != "audio"
        || wire::integer(&packet.meta, "frame")? != 0
        || !packet.data.is_empty()
    {
        return Err(error("Invalid native audio execution command"));
    }
    let handles: Vec<ClassicAudioResourceHandle> =
        serde_json::from_value(packet.meta["handles"].clone())
            .map_err(|_| error("Invalid native audio handles"))?;
    let sample_rate = u32::try_from(wire::integer(
        &packet.meta["plan"]["identity"],
        "sampleRate",
    )?)
    .map_err(|_| error("Invalid native audio rate"))?;
    let channel = Rc::new(RefCell::new(Channel {
        input,
        output,
        request: 0,
        frame: 0,
        deadline,
    }));
    let mut resources = Resources {
        channel: channel.clone(),
    };
    let mut session = Session {
        channel: channel.clone(),
        plan: packet.meta["plan"].clone(),
    };
    let receipt = render::render(
        &mut session,
        &mut resources,
        &handles,
        &mut Output(channel.clone()),
        &DecodeConfig {
            sample_rate,
            max_pcm_bytes: policy.max_pcm_bytes,
            max_encoded_bytes: policy.max_encoded_bytes,
            deadline,
        },
        &identity.worker_sha256,
    )?;
    resources.checkpoint()?;
    wire::send(
        channel.borrow_mut().output,
        json!({"type":"audio_complete","frame":0,"receipt":receipt}),
        &[],
    )?;
    let final_packet = wire::read(channel.borrow_mut().input)?;
    if final_packet.meta["type"] != "finish"
        || wire::integer(&final_packet.meta, "frames")? != 1
        || !final_packet.data.is_empty()
    {
        return Err(error("Invalid native audio completion command"));
    }
    if identity::observe()? != identity {
        return Err(EditorError::new(
            "AUDIO_ENGINE_CHANGED",
            "Native audio runtime changed during execution",
        ));
    }
    resources.checkpoint()?;
    wire::send(
        channel.borrow_mut().output,
        json!({"type":"complete","frames":1}),
        &[],
    )
}
