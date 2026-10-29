use super::{identity, wire, *};
use crate::classic_visual::VisualProcessSession;
use opencut_editor_application::{
    classic_text_frame::ClassicTextFontHandle, classic_visual::*,
    media_frame_jobs::LocalVisualExecutionPolicy,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    io::{BufReader, Stdin, Stdout},
    rc::Rc,
    time::{Duration, Instant},
};

struct Channel {
    input: BufReader<Stdin>,
    output: Stdout,
    frame: u64,
    request: u64,
}
impl Channel {
    fn begin_request(&mut self, operation: &str, args: Value) -> Result<(), EditorError> {
        self.request = self
            .request
            .checked_add(1)
            .ok_or_else(|| wire::error("Native request counter overflow"))?;
        if self.request > 200_000 {
            return Err(wire::error("Excessive native frame requests"));
        }
        wire::send(
            &mut self.output,
            json!({"type":"request","frame":self.frame,"id":self.request,"operation":operation,"args":args}),
            &[],
        )
    }
    fn request(&mut self, operation: &str, args: Value) -> Result<wire::Packet, EditorError> {
        self.begin_request(operation, args)?;
        let packet = wire::read(&mut self.input)?;
        if wire::text(&packet.meta, "type")? != "reply"
            || wire::integer(&packet.meta, "frame")? != self.frame
            || wire::integer(&packet.meta, "id")? != self.request
        {
            return Err(wire::error("Native response belongs to another request"));
        }
        Ok(packet)
    }
}
struct Resources {
    channel: Rc<RefCell<Channel>>,
    deadline: Instant,
}
impl ClassicVisualResources for Resources {
    fn resolve(&mut self, _: &VisualResource, _: &str) -> Result<(String, u64), EditorError> {
        Err(wire::error(
            "Native resolution requires the application session",
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
        let eof = packet
            .meta
            .get("eof")
            .and_then(Value::as_bool)
            .ok_or_else(|| wire::error("Missing source EOF observation"))?;
        if packet.data.len() > count as usize {
            return Err(wire::error("Oversized native source read"));
        }
        Ok((packet.data, eof))
    }
    fn release(&mut self, transfer: &str) -> Result<(), EditorError> {
        let packet = self
            .channel
            .borrow_mut()
            .request("release", json!({"transfer":transfer}))?;
        if !packet.data.is_empty() {
            return Err(wire::error("Unexpected release bytes"));
        }
        Ok(())
    }
    fn checkpoint(&mut self) -> Result<(), EditorError> {
        if Instant::now() >= self.deadline {
            return Err(EditorError::new(
                "VISUAL_TIME_LIMIT",
                "Native frame exceeded its deadline",
            ));
        }
        Ok(())
    }
}
struct Session {
    channel: Rc<RefCell<Channel>>,
    plan: Value,
    policy: LocalVisualExecutionPolicy,
}
fn callback_value(mut packet: wire::Packet) -> Result<Value, EditorError> {
    if !packet.data.is_empty() {
        return Err(wire::error("Unexpected application callback bytes"));
    }
    packet
        .meta
        .as_object_mut()
        .and_then(|meta| meta.remove("value"))
        .ok_or_else(|| wire::error("Missing native callback value"))
}
fn batch_reply(
    packet: wire::Packet,
    frame: u64,
    request: u64,
    index: u64,
) -> Result<Value, EditorError> {
    if wire::text(&packet.meta, "type")? != "reply"
        || wire::integer(&packet.meta, "frame")? != frame
        || wire::integer(&packet.meta, "id")? != request
        || wire::integer(&packet.meta, "index")? != index
    {
        return Err(wire::error("Native callback batch reply order changed"));
    }
    callback_value(packet)
}
impl VisualProcessSession<Resources> for Session {
    type Callback = Value;
    type Receipt = Value;
    type Output = ();
    fn policy(&self) -> &LocalVisualExecutionPolicy {
        &self.policy
    }
    fn plan(&self) -> Result<Value, EditorError> {
        Ok(self.plan.clone())
    }
    fn callback(&mut self, request: Value, _: &mut Resources) -> Result<Value, EditorError> {
        let packet = self.channel.borrow_mut().request("callback", request)?;
        callback_value(packet)
    }
    fn callbacks(
        &mut self,
        requests: Vec<Value>,
        resources: &mut Resources,
        accept: &mut dyn FnMut(Value) -> Result<(), EditorError>,
    ) -> Result<(), EditorError> {
        if requests.is_empty() || requests.len() > crate::classic_visual::MAX_CALLBACK_BATCH {
            return Err(wire::error("Invalid native callback batch"));
        }
        let sizes = requests
            .iter()
            .map(|r| {
                serde_json::to_vec(r)
                    .map(|v| v.len())
                    .map_err(|_| wire::error("Invalid native callback"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut channel = self.channel.borrow_mut();
        let mut start = 0;
        while start < requests.len() {
            resources.checkpoint()?;
            let overhead = serde_json::to_vec(&json!({"type":"request","frame":channel.frame,
                "id":channel.request+1,"operation":"callbacks","args":[]}))
            .map_err(|_| wire::error("Invalid native callback batch"))?
            .len();
            let mut end = start;
            let mut length = overhead;
            while end < requests.len()
                && length + sizes[end] + usize::from(end > start) <= wire::META_BYTES
            {
                length += sizes[end] + usize::from(end > start);
                end += 1;
            }
            if end <= start + 1 {
                // A single callback retains its original packet envelope and
                // byte limit, including requests close to the metadata bound.
                let packet = channel.request("callback", requests[start].clone())?;
                accept(callback_value(packet)?)?;
                start += 1;
                continue;
            }
            channel.begin_request("callbacks", json!(&requests[start..end]))?;
            for index in 0..end - start {
                resources.checkpoint()?;
                let packet = wire::read(&mut channel.input)?;
                accept(batch_reply(
                    packet,
                    channel.frame,
                    channel.request,
                    index as u64,
                )?)?;
            }
            start = end;
        }
        Ok(())
    }
    fn rgba_chunk(
        &mut self,
        offset: u64,
        bytes: &[u8],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<(), EditorError> {
        sink.chunk(offset, bytes)
    }
    fn finish(
        &self,
        _: Value,
        _: VisualQuotaReceipt,
        _: ClassicVisualRuntimeReceipt,
    ) -> Result<(), EditorError> {
        Err(wire::error(
            "Only the publishing host finishes a native session",
        ))
    }
}
struct Output(Rc<RefCell<Channel>>);
impl ClassicVisualRgbaSink for Output {
    fn chunk(&mut self, offset: u64, bytes: &[u8]) -> Result<(), EditorError> {
        let mut channel = self.0.borrow_mut();
        let frame = channel.frame;
        wire::send(
            &mut channel.output,
            json!({"type":"rgba","frame":frame,"offset":offset}),
            bytes,
        )
    }
}
fn run(channel: &Rc<RefCell<Channel>>) -> Result<(), EditorError> {
    let packet = wire::read(&mut channel.borrow_mut().input)?;
    if wire::text(&packet.meta, "type")? != "initialize" || !packet.data.is_empty() {
        return Err(wire::error("Missing native initialization"));
    }
    if packet.meta["purpose"] == "audio_export" || packet.meta["purpose"] == "audio_preview" {
        let mut channel = channel.borrow_mut();
        let Channel { input, output, .. } = &mut *channel;
        return crate::native_audio::worker::serve_initialized(packet, input, output);
    }
    if packet.meta["purpose"] == "packet_rate" {
        let mut channel = channel.borrow_mut();
        let Channel { input, output, .. } = &mut *channel;
        return crate::source_art::worker::serve_packet_rate(packet, input, output);
    }
    if packet.meta["purpose"] == "source_art" {
        let mut channel = channel.borrow_mut();
        let Channel { input, output, .. } = &mut *channel;
        return crate::source_art::worker::serve(packet, input, output);
    }
    let policy: LocalVisualExecutionPolicy = serde_json::from_value(
        packet
            .meta
            .get("policy")
            .cloned()
            .ok_or_else(|| wire::error("Missing native policy"))?,
    )
    .map_err(|_| wire::error("Invalid native policy"))?;
    opencut_editor_application::media_frame_jobs::validate_policy(&policy)?;
    let purpose = wire::text(&packet.meta, "purpose")?;
    let mut engine = match purpose {
        "visual" => Some(NativeFrameEngine::new(super::platform_backend()?, &policy)?),
        "text_measurement" => None,
        _ => return Err(wire::error("Unknown native worker purpose")),
    };
    let mut text_metrics = opencut_renderer::raster::TextRasterizer::new(policy.max_encoded_bytes);
    let identity = match &engine {
        Some(engine) => {
            let identity = identity::observe(engine.identity())?;
            validate_native_identity(&identity)?;
            serde_json::to_value(identity)
                .map_err(|_| wire::error("Invalid visual identity serialization"))?
        }
        None => serde_json::to_value(identity::observe_text()?)
            .map_err(|_| wire::error("Invalid text identity serialization"))?,
    };
    wire::send(
        &mut channel.borrow_mut().output,
        json!({"type":"ready","purpose":purpose,"pid":std::process::id(),"identity":identity}),
        &[],
    )?;
    let mut next = 0u64;
    loop {
        let packet = wire::read(&mut channel.borrow_mut().input)?;
        if !packet.data.is_empty() {
            return Err(wire::error("Unexpected native command bytes"));
        }
        match wire::text(&packet.meta, "type")? {
            "frame" => {
                if wire::integer(&packet.meta, "frame")? != next {
                    return Err(wire::error("Native frame order changed"));
                }
                let begin = wire::text(&packet.meta, "begin")?;
                let handles: Vec<ClassicVisualResourceHandle> =
                    serde_json::from_value(packet.meta["handles"].clone())
                        .map_err(|_| wire::error("Invalid native source handles"))?;
                let fonts: Vec<ClassicTextFontHandle> =
                    serde_json::from_value(packet.meta["fonts"].clone())
                        .map_err(|_| wire::error("Invalid native font handles"))?;
                let deadline = Instant::now() + Duration::from_millis(policy.deadline_ms);
                {
                    let mut channel = channel.borrow_mut();
                    channel.frame = next;
                    channel.request = 0;
                }
                let mut resources = Resources {
                    channel: channel.clone(),
                    deadline,
                };
                let mut session = Session {
                    channel: channel.clone(),
                    plan: packet.meta["plan"].clone(),
                    policy: policy.clone(),
                };
                let result = engine
                    .as_mut()
                    .ok_or_else(|| wire::error("A text measurer cannot render pixels"))?
                    .render(
                        &mut session,
                        &mut resources,
                        begin,
                        &handles,
                        &fonts,
                        &mut Output(channel.clone()),
                        deadline,
                        serde_json::from_value(
                            packet.meta.get("hit_probe").cloned().unwrap_or(Value::Null),
                        )
                        .map_err(|_| wire::error("Invalid native hit probe"))?,
                    )?;
                wire::send(
                    &mut channel.borrow_mut().output,
                    json!({"type":"frame_complete","frame":next,"receipt":result.receipt,
                    "encoded_bytes_read":result.encoded_bytes_read,"accounted_gpu_bytes":result.accounting.accounted_gpu_bytes,"accounted_cpu_bytes":result.accounting.accounted_cpu_bytes,"hit_test":result.hit_test}),
                    &[],
                )?;
                next = next
                    .checked_add(1)
                    .ok_or_else(|| wire::error("Native frame counter overflow"))?;
            }
            "measure" => {
                if purpose != "text_measurement" || wire::integer(&packet.meta, "frame")? != next {
                    return Err(wire::error("Native text request order or purpose changed"));
                }
                let fonts: Vec<ClassicTextFontHandle> =
                    serde_json::from_value(packet.meta["fonts"].clone())
                        .map_err(|_| wire::error("Invalid native text handles"))?;
                let options = serde_json::from_value(packet.meta["options"].clone())
                    .map_err(|_| wire::error("Invalid native text options"))?;
                let deadline = Instant::now() + Duration::from_millis(policy.deadline_ms);
                {
                    let mut channel = channel.borrow_mut();
                    channel.frame = next;
                    channel.request = 0;
                }
                let mut resources = Resources {
                    channel: channel.clone(),
                    deadline,
                };
                let mut session = Session {
                    channel: channel.clone(),
                    plan: packet.meta["plan"].clone(),
                    policy: policy.clone(),
                };
                let receipt = super::measurement::measure(
                    &mut session,
                    &mut resources,
                    &fonts,
                    options,
                    &mut text_metrics,
                )?;
                wire::send(
                    &mut channel.borrow_mut().output,
                    json!({"type":"measure_complete","frame":next,"receipt":receipt}),
                    &[],
                )?;
                next = next
                    .checked_add(1)
                    .ok_or_else(|| wire::error("Native text counter overflow"))?;
            }
            "finish" => {
                if wire::integer(&packet.meta, "frames")? != next {
                    return Err(wire::error("Native completion omitted frames"));
                }
                let observed = match &engine {
                    Some(engine) => serde_json::to_value(identity::observe(engine.identity())?),
                    None => serde_json::to_value(identity::observe_text()?),
                }
                .map_err(|_| wire::error("Invalid final native identity"))?;
                if observed != identity {
                    return Err(EditorError::new(
                        "VISUAL_ENGINE_CHANGED",
                        "Native runtime changed during execution",
                    ));
                }
                drop(engine);
                drop(text_metrics);
                wire::send(
                    &mut channel.borrow_mut().output,
                    json!({"type":"complete","frames":next}),
                    &[],
                )?;
                return Ok(());
            }
            _ => return Err(wire::error("Unknown native worker command")),
        }
    }
}
/// Entry point for a dedicated executable. It accepts no project paths or CLI
/// commands, and cannot publish artifacts or open media outside the byte port.
pub fn serve() -> Result<(), EditorError> {
    let channel = Rc::new(RefCell::new(Channel {
        input: BufReader::new(std::io::stdin()),
        output: std::io::stdout(),
        frame: 0,
        request: 0,
    }));
    let result = run(&channel);
    if let Err(error) = &result {
        let _ = wire::send(
            &mut channel.borrow_mut().output,
            json!({"type":"failed","code":error.code,"message":error.message}),
            &[],
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_replies_cannot_cross_frame_request_or_item_boundaries() {
        let original = json!({"type":"reply","frame":7,"id":11,"index":2,"value":{"bound":true}});
        let packet = |meta| wire::Packet {
            meta,
            data: Vec::new(),
        };
        assert_eq!(
            batch_reply(packet(original.clone()), 7, 11, 2).unwrap(),
            json!({"bound":true})
        );
        for (field, value) in [
            ("type", json!("request")),
            ("frame", json!(8)),
            ("id", json!(12)),
            ("index", json!(1)),
        ] {
            let mut meta = original.clone();
            meta[field] = value;
            assert!(batch_reply(packet(meta), 7, 11, 2).is_err());
        }
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove("value");
        assert!(batch_reply(packet(missing), 7, 11, 2).is_err());
        assert!(
            batch_reply(
                wire::Packet {
                    meta: original,
                    data: vec![0]
                },
                7,
                11,
                2
            )
            .is_err()
        );
    }
}
