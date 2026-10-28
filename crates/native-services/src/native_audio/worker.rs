use super::{
    protocol::{self, error},
    source::DecodeConfig,
    *,
};
use crate::native_process::wire;
use opencut_composition::scene::RuntimeAudioBlock;
use opencut_editor_application::{
    classic_audio::*,
    scene_audio_jobs::{LocalAudioExecutionPolicy, validate_policy},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    io::{Read, Write},
    rc::Rc,
    time::{Duration, Instant},
};

struct Channel<'a> {
    input: &'a mut dyn Read,
    output: &'a mut dyn Write,
    request: u64,
    deadline: Instant,
}
impl Channel<'_> {
    fn request(&mut self, operation: &str, args: Value) -> Result<wire::Packet, EditorError> {
        if Instant::now() >= self.deadline {
            return Err(EditorError::new(
                "AUDIO_TIME_LIMIT",
                "Native audio exceeded its deadline",
            ));
        }
        self.request = self
            .request
            .checked_add(1)
            .filter(|n| *n <= 100000)
            .ok_or_else(|| error("Native audio callback limit exceeded"))?;
        wire::send(
            self.output,
            json!({"type":"request","frame":0,"id":self.request,"operation":operation,"args":args}),
            &[],
        )?;
        let packet = wire::read(self.input)?;
        if wire::integer(&packet.meta, "frame")? != 0
            || wire::integer(&packet.meta, "id")? != self.request
        {
            return Err(error("Native audio reply belongs to another callback"));
        }
        Ok(packet)
    }
    fn json<T: serde::de::DeserializeOwned>(&mut self, args: Value) -> Result<T, EditorError> {
        let packet = self.request("callback", args)?;
        let length = usize::try_from(wire::integer(&packet.meta, "length")?)
            .map_err(|_| error("Invalid audio fact length"))?;
        if wire::text(&packet.meta, "type")? != "json_reply"
            || !packet.data.is_empty()
            || length == 0
            || length > protocol::JSON_BYTES
        {
            return Err(error("Invalid native audio fact reply"));
        }
        let mut data = Vec::new();
        data.try_reserve_exact(length)
            .map_err(|_| error("Cannot allocate admitted audio facts"))?;
        while data.len() < length {
            if Instant::now() >= self.deadline {
                return Err(EditorError::new(
                    "AUDIO_TIME_LIMIT",
                    "Native audio fact stream exceeded its deadline",
                ));
            }
            let packet = wire::read(self.input)?;
            if wire::text(&packet.meta, "type")? != "json_chunk"
                || wire::integer(&packet.meta, "frame")? != 0
                || wire::integer(&packet.meta, "id")? != self.request
                || wire::integer(&packet.meta, "offset")? != data.len() as u64
                || packet.data.is_empty()
                || packet.data.len() > length - data.len()
            {
                return Err(error(
                    "Native audio fact stream changed its order or extent",
                ));
            }
            data.extend_from_slice(&packet.data);
        }
        serde_json::from_slice(&data)
            .map_err(|_| error("Native audio facts differ from their shared Rust declaration"))
    }
}
struct Resources<'a> {
    channel: Rc<RefCell<Channel<'a>>>,
}
impl ClassicAudioResources for Resources<'_> {
    fn resolve(&mut self, _: &AudioResource, _: &str) -> Result<(String, u64), EditorError> {
        Err(error(
            "Only the parent application resolves audio resources",
        ))
    }
    fn read(
        &mut self,
        transfer: &str,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), EditorError> {
        self.checkpoint()?;
        let packet = self.channel.borrow_mut().request(
            "read",
            json!({"transfer":transfer,"offset":offset,"count":count}),
        )?;
        if packet.meta["type"] != "reply" || packet.data.len() > count as usize {
            return Err(error("Invalid verified audio byte reply"));
        }
        let eof = packet.meta["eof"]
            .as_bool()
            .ok_or_else(|| error("Missing verified audio EOF"))?;
        Ok((packet.data, eof))
    }
    fn release(&mut self, transfer: &str) -> Result<(), EditorError> {
        let p = self
            .channel
            .borrow_mut()
            .request("release", json!({"transfer":transfer}))?;
        if p.meta["type"] != "reply" || !p.data.is_empty() {
            return Err(error("Invalid native audio release reply"));
        }
        Ok(())
    }
    fn checkpoint(&mut self) -> Result<(), EditorError> {
        if Instant::now() >= self.channel.borrow().deadline {
            Err(EditorError::new(
                "AUDIO_TIME_LIMIT",
                "Native audio exceeded its captured deadline",
            ))
        } else {
            Ok(())
        }
    }
}
struct Session<'a> {
    channel: Rc<RefCell<Channel<'a>>>,
    plan: Value,
}
impl AudioSession<Resources<'_>> for Session<'_> {
    fn plan(&self) -> Result<Value, EditorError> {
        Ok(self.plan.clone())
    }
    fn callback(&mut self, request: Value, _: &mut Resources<'_>) -> Result<Value, EditorError> {
        self.channel.borrow_mut().json(request)
    }
    fn compile_block(
        &mut self,
        request: BlockRequest,
        _: &mut Resources<'_>,
    ) -> Result<RuntimeAudioBlock, EditorError> {
        self.channel
            .borrow_mut()
            .json(json!({"op":"compileBlock","args":request}))
    }
}
struct Output<'a>(Rc<RefCell<Channel<'a>>>);
impl ClassicAudioPcmSink for Output<'_> {
    fn chunk(&mut self, channel: u32, offset: u64, samples: &[f32]) -> Result<(), EditorError> {
        if samples.is_empty()
            || samples.len() > AUDIO_PCM_CHUNK_FRAMES
            || samples.iter().any(|v| !v.is_finite())
        {
            return Err(error(
                "Native audio PCM chunk exceeds its transport admission",
            ));
        }
        let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        wire::send(
            self.0.borrow_mut().output,
            json!({"type":"pcm","frame":0,"channel":channel,"offset":offset,"count":samples.len()}),
            &bytes,
        )
    }
}
pub(crate) fn serve_initialized(
    packet: wire::Packet,
    input: &mut dyn Read,
    output: &mut dyn Write,
) -> Result<(), EditorError> {
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
