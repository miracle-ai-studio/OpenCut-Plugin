//! Shared native audio callback and bounded fact/PCM transport.
use super::{
    AudioSession,
    protocol::{self, error},
};
use crate::native_process::wire;
use opencut_composition::scene::RuntimeAudioBlock;
use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::*;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    io::{Read, Write},
    rc::Rc,
    time::Instant,
};
pub(super) struct Channel<'a> {
    pub(super) input: &'a mut dyn Read,
    pub(super) output: &'a mut dyn Write,
    pub(super) request: u64,
    pub(super) frame: u64,
    pub(super) deadline: Instant,
}
impl Channel<'_> {
    fn advance(&mut self) -> Result<(), EditorError> {
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
        Ok(())
    }
    pub(super) fn request(
        &mut self,
        operation: &str,
        args: Value,
    ) -> Result<wire::Packet, EditorError> {
        self.advance()?;
        wire::send(
            self.output,
            json!({"type":"request","frame":self.frame,"id":self.request,"operation":operation,"args":args}),
            &[],
        )?;
        let packet = wire::read(self.input)?;
        if wire::integer(&packet.meta, "frame")? != self.frame
            || wire::integer(&packet.meta, "id")? != self.request
        {
            return Err(error("Native audio reply belongs to another callback"));
        }
        Ok(packet)
    }
    pub(super) fn json<T: serde::de::DeserializeOwned>(
        &mut self,
        args: Value,
    ) -> Result<T, EditorError> {
        let packet = self.request("callback", args)?;
        self.json_reply(packet)
    }
    fn json_reply<T: serde::de::DeserializeOwned>(
        &mut self,
        packet: wire::Packet,
    ) -> Result<T, EditorError> {
        let data = self.fact_reply(packet, "json_reply", "json_chunk")?;
        serde_json::from_slice(&data)
            .map_err(|_| error("Native audio facts differ from their shared Rust declaration"))
    }
    fn fact_reply(
        &mut self,
        packet: wire::Packet,
        reply_kind: &str,
        chunk_kind: &str,
    ) -> Result<Vec<u8>, EditorError> {
        let length = usize::try_from(wire::integer(&packet.meta, "length")?)
            .map_err(|_| error("Invalid audio fact length"))?;
        if wire::text(&packet.meta, "type")? != reply_kind
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
            if wire::text(&packet.meta, "type")? != chunk_kind
                || wire::integer(&packet.meta, "frame")? != self.frame
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
        Ok(data)
    }
    /// Binding timestamps keep their exact JSON spelling; dense compiled facts
    /// return as bounded little-endian sample records with exact Float64 bits.
    fn preview_body(
        &mut self,
        args: &BindPreviewSources,
    ) -> Result<opencut_composition::scene::PreviewAudioBlock, EditorError> {
        let operation = "preview_block";
        let data = protocol::encode(args)?;
        self.advance()?;
        wire::send(
            self.output,
            json!({"type":"request_json","frame":self.frame,"id":self.request,"operation":operation,"length":data.len()}),
            &[],
        )?;
        for (n, chunk) in data.chunks(wire::DATA_BYTES).enumerate() {
            if Instant::now() >= self.deadline {
                return Err(EditorError::new(
                    "AUDIO_TIME_LIMIT",
                    "Native audio request exceeded its deadline",
                ));
            }
            wire::send(
                self.output,
                json!({"type":"request_json_chunk","frame":self.frame,"id":self.request,"offset":n*wire::DATA_BYTES}),
                chunk,
            )?;
        }
        let packet = wire::read(self.input)?;
        if wire::integer(&packet.meta, "frame")? != self.frame
            || wire::integer(&packet.meta, "id")? != self.request
        {
            return Err(error("Native audio body reply changed its callback"));
        }
        let bytes = self.fact_reply(packet, "preview_reply", "preview_chunk")?;
        protocol::decode_preview(&bytes)
    }
}
pub(super) struct Resources<'a> {
    pub(super) channel: Rc<RefCell<Channel<'a>>>,
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
pub(super) struct Session<'a> {
    pub(super) channel: Rc<RefCell<Channel<'a>>>,
    pub(super) plan: Value,
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
    fn compile_preview(
        &mut self,
        request: BindPreviewSources,
        _: &mut Resources<'_>,
    ) -> Result<opencut_composition::scene::PreviewAudioBlock, EditorError> {
        self.channel.borrow_mut().preview_body(&request)
    }
}
pub(super) struct Output<'a>(pub(super) Rc<RefCell<Channel<'a>>>);
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
        let mut io = self.0.borrow_mut();
        let frame = io.frame;
        wire::send(
            io.output,
            json!({"type":"pcm","frame":frame,"channel":channel,"offset":offset,"count":samples.len()}),
            &bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::Duration;
    #[test]
    fn preview_fact_chunks_keep_order_identity_deadline_and_type_fences() {
        for (frame, id, offset, kind, expired, valid) in [
            (7, 9, 0, "preview_chunk", false, true),
            (8, 9, 0, "preview_chunk", false, false),
            (7, 10, 0, "preview_chunk", false, false),
            (7, 9, 1, "preview_chunk", false, false),
            (7, 9, 0, "json_chunk", false, false),
            (7, 9, 0, "preview_chunk", true, false),
        ] {
            let mut bytes = Vec::new();
            wire::send(
                &mut bytes,
                json!({"type":"preview_reply","frame":7,"id":9,"length":3}),
                &[],
            )
            .unwrap();
            wire::send(
                &mut bytes,
                json!({"type":kind,"frame":frame,"id":id,"offset":offset}),
                &[1, 2, 3],
            )
            .unwrap();
            let mut input = Cursor::new(bytes);
            let mut output = Vec::new();
            let packet = wire::read(&mut input).unwrap();
            let mut channel = Channel {
                input: &mut input,
                output: &mut output,
                request: 9,
                frame: 7,
                deadline: if expired {
                    Instant::now()
                } else {
                    Instant::now() + Duration::from_secs(5)
                },
            };
            let result = channel.fact_reply(packet, "preview_reply", "preview_chunk");
            assert_eq!(result.is_ok(), valid);
            if valid {
                assert_eq!(result.unwrap(), vec![1, 2, 3]);
            }
        }
    }
}
