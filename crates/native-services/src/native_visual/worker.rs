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
    fn request(&mut self, operation: &str, args: Value) -> Result<wire::Packet, EditorError> {
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
        )?;
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
        if !packet.data.is_empty() {
            return Err(wire::error("Unexpected application callback bytes"));
        }
        packet
            .meta
            .get("value")
            .cloned()
            .ok_or_else(|| wire::error("Missing native callback value"))
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
    let policy: LocalVisualExecutionPolicy = serde_json::from_value(
        packet
            .meta
            .get("policy")
            .cloned()
            .ok_or_else(|| wire::error("Missing native policy"))?,
    )
    .map_err(|_| wire::error("Invalid native policy"))?;
    let backend = super::platform_backend()?;
    let mut engine = NativeFrameEngine::new(backend, &policy)?;
    let identity = identity::observe(engine.identity())?;
    validate_native_identity(&identity)?;
    wire::send(
        &mut channel.borrow_mut().output,
        json!({"type":"ready","pid":std::process::id(),"identity":identity}),
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
                let result = engine.render(
                    &mut session,
                    &mut resources,
                    begin,
                    &handles,
                    &fonts,
                    &mut Output(channel.clone()),
                    deadline,
                )?;
                wire::send(
                    &mut channel.borrow_mut().output,
                    json!({"type":"frame_complete","frame":next,"receipt":result.receipt,
                    "encoded_bytes_read":result.encoded_bytes_read,"accounted_gpu_bytes":result.accounting.accounted_gpu_bytes,"accounted_cpu_bytes":result.accounting.accounted_cpu_bytes}),
                    &[],
                )?;
                next = next
                    .checked_add(1)
                    .ok_or_else(|| wire::error("Native frame counter overflow"))?;
            }
            "finish" => {
                if wire::integer(&packet.meta, "frames")? != next {
                    return Err(wire::error("Native completion omitted frames"));
                }
                if identity::observe(engine.identity())? != identity {
                    return Err(EditorError::new(
                        "VISUAL_ENGINE_CHANGED",
                        "Native runtime changed during execution",
                    ));
                }
                drop(engine);
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
