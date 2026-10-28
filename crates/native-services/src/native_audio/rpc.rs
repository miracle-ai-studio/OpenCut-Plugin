//! One parent-side native audio callback dispatcher. The application owns
//! resource authorization and compiled facts for both export and realtime.
use super::{
    identity,
    preview::InputBinding,
    protocol::{self, error},
};
use crate::native_process::{self, WorkerProcess, wire};
use opencut_contracts::{EditorError, renderer_identity::NativeAudioIdentity};
use opencut_editor_application::classic_audio::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
type Result<T> = std::result::Result<T, EditorError>;

pub(super) fn receive(
    worker: &WorkerProcess,
    resources: &mut dyn ClassicAudioResources,
) -> Result<wire::Packet> {
    loop {
        resources.checkpoint()?;
        if let Some(packet) = worker.receive()? {
            resources.checkpoint()?;
            protocol::reject_failure(&packet)?;
            return Ok(packet);
        }
    }
}
pub(super) fn initialize(
    worker: &mut WorkerProcess,
    purpose: &str,
    policy: Value,
    resources: &mut dyn ClassicAudioResources,
) -> Result<NativeAudioIdentity> {
    worker.send(
        json!({"type":"initialize","purpose":purpose,"policy":policy}),
        &[],
    )?;
    let ready = receive(worker, resources)?;
    if ready.meta["type"] != "ready"
        || ready.meta["purpose"] != purpose
        || wire::integer(&ready.meta, "pid")? != u64::from(worker.pid())
        || !ready.data.is_empty()
    {
        return Err(error("Invalid native audio readiness"));
    }
    let identity: NativeAudioIdentity = serde_json::from_value(ready.meta["identity"].clone())
        .map_err(|_| error("Invalid native audio identity"))?;
    validate_native_identity(&identity)?;
    if identity.worker_sha256 != worker.expected_hash
        || identity.dsp_sha256 != identity::dsp_hash()
        || identity.platform != native_process::system_identity()?
        || identity.decoder_libraries != native_process::library_identities()?
    {
        return Err(EditorError::new(
            "AUDIO_ENGINE_CHANGED",
            "Native audio worker differs from the configured runtime",
        ));
    }
    worker.unchanged()?;
    resources.checkpoint()?;
    Ok(identity)
}
pub(super) struct PreviewWindow {
    pub cursor: u64,
    pub limit: u64,
    admitted: Option<PreviewBlockRequest>,
    planned: Option<PreviewBlockRequest>,
    pub bindings: Vec<InputBinding>,
}
impl PreviewWindow {
    pub fn new(cursor: u64, limit: u64) -> Result<Self> {
        if cursor > limit {
            return Err(error("Invalid native preview read-ahead envelope"));
        }
        Ok(Self {
            cursor,
            limit,
            admitted: None,
            planned: None,
            bindings: Vec::new(),
        })
    }
    pub fn complete(&self) -> bool {
        self.cursor == self.limit && self.admitted.is_none() && self.planned.is_none()
    }
    fn admit(&self, request: &PreviewBlockRequest) -> Result<()> {
        let start = decimal(&request.start_sample)?;
        if self.admitted.is_some()
            || self.planned.is_some()
            || start != self.cursor
            || request.frame_count == 0
            || request.frame_count > 4096
            || start
                .checked_add(u64::from(request.frame_count))
                .is_none_or(|n| n > self.limit)
        {
            return Err(error(
                "Native preview input exceeds its captured output/read-ahead envelope",
            ));
        }
        Ok(())
    }
}
pub(super) enum Mode<'a> {
    Export,
    Preparation,
    Preview(&'a mut PreviewWindow),
    Stop,
}

pub(super) fn service(
    worker: &mut WorkerProcess,
    packet: wire::Packet,
    frame: u64,
    requests: &mut u64,
    session: &mut ClassicAudioSession<'_>,
    resources: &mut dyn ClassicAudioResources,
    mut mode: Mode<'_>,
) -> Result<()> {
    *requests = requests
        .checked_add(1)
        .filter(|n| *n <= 100000)
        .ok_or_else(|| error("Native audio callback limit exceeded"))?;
    if wire::integer(&packet.meta, "frame")? != frame
        || wire::integer(&packet.meta, "id")? != *requests
        || !packet.data.is_empty()
    {
        return Err(error("Native audio callback order or command changed"));
    }
    let kind = wire::text(&packet.meta, "type")?;
    if kind == "request_json" {
        let Mode::Preview(window) = &mut mode else {
            return Err(error(
                "Native audio body request is not a captured preview command",
            ));
        };
        if wire::text(&packet.meta, "operation")? != "preview_block" {
            return Err(error("Unknown native audio body operation"));
        }
        let length = usize::try_from(wire::integer(&packet.meta, "length")?)
            .map_err(|_| error("Invalid native preview binding length"))?;
        if length == 0 || length > protocol::JSON_BYTES {
            return Err(error("Native preview binding exceeds its fact envelope"));
        }
        let mut data = Vec::new();
        data.try_reserve_exact(length)
            .map_err(|_| error("Cannot allocate admitted native preview binding"))?;
        while data.len() < length {
            let chunk = receive(worker, resources)?;
            if chunk.meta["type"] != "request_json_chunk"
                || wire::integer(&chunk.meta, "frame")? != frame
                || wire::integer(&chunk.meta, "id")? != *requests
                || wire::integer(&chunk.meta, "offset")? != data.len() as u64
                || chunk.data.is_empty()
                || chunk.data.len() > length - data.len()
            {
                return Err(error("Native preview binding changed order or extent"));
            }
            data.extend_from_slice(&chunk.data);
        }
        let binding: BindPreviewSources = serde_json::from_slice(&data)
            .map_err(|_| error("Invalid typed native preview binding"))?;
        let request = binding
            .window
            .clone()
            .ok_or_else(|| error("Native preview binding omitted its planned window"))?;
        if window.planned.as_ref() != Some(&request) {
            return Err(error(
                "Native preview binding differs from its Rust-planned input",
            ));
        }
        let digest = format!("{:x}", Sha256::digest(&data));
        session.callback(AudioCallback::BindPreview(binding), resources)?;
        let block = session.compile_preview_block_typed(request.clone(), resources)?;
        if block.start_sample != window.cursor || block.frame_count != request.frame_count {
            return Err(error("Native preview compiler changed its admitted prefix"));
        }
        let bytes = protocol::encode(&block)?;
        window.bindings.push(InputBinding {
            start_sample: request.start_sample,
            frame_count: request.frame_count,
            binding_sha256: digest,
        });
        window.cursor += u64::from(request.frame_count);
        window.admitted = None;
        window.planned = None;
        return reply_json(worker, frame, *requests, &bytes, resources);
    }
    if kind != "request" {
        return Err(error("Unexpected native audio callback packet"));
    }
    let args = &packet.meta["args"];
    match wire::text(&packet.meta, "operation")? {
        "callback" => {
            let request: AudioCallback = serde_json::from_value(args.clone())
                .map_err(|_| error("Invalid typed native audio callback"))?;
            let data = match (&mut mode, request) {
                (Mode::Export, AudioCallback::Block(v)) => {
                    protocol::encode(&session.compile_export_block_typed(v, resources)?)?
                }
                (Mode::Export, request) => {
                    protocol::encode(&session.callback(request, resources)?)?
                }
                (Mode::Preparation, request) => {
                    if matches!(
                        &request,
                        AudioCallback::PreviewWindows(_)
                            | AudioCallback::PreviewBlock(_)
                            | AudioCallback::AdmitPreviewBlock(_)
                            | AudioCallback::Block(_)
                    ) || matches!(&request,AudioCallback::BindPreview(v) if v.window.is_some())
                    {
                        return Err(error(
                            "Native preview preparation requested an uncaptured block",
                        ));
                    }
                    protocol::encode(&session.callback(request, resources)?)?
                }
                (Mode::Preview(window), AudioCallback::AdmitPreviewBlock(mut v)) => {
                    window.admit(&v)?;
                    let value =
                        session.callback(AudioCallback::AdmitPreviewBlock(v.clone()), resources)?;
                    let count = value
                        .as_u64()
                        .filter(|n| *n > 0 && *n <= u64::from(v.frame_count))
                        .ok_or_else(|| error("Invalid admitted native preview prefix"))?
                        as u32;
                    v.frame_count = count;
                    window.admitted = Some(v);
                    protocol::encode(&value)?
                }
                (Mode::Preview(window), AudioCallback::PreviewWindows(v)) => {
                    if window.admitted.as_ref() != Some(&v) || window.planned.is_some() {
                        return Err(error(
                            "Native preview window differs from its admitted prefix",
                        ));
                    }
                    let value =
                        session.callback(AudioCallback::PreviewWindows(v.clone()), resources)?;
                    window.planned = Some(v);
                    protocol::encode(&value)?
                }
                _ => {
                    return Err(error(
                        "Native audio callback differs from its captured execution phase",
                    ));
                }
            };
            reply_json(worker, frame, *requests, &data, resources)
        }
        "read" => {
            if matches!(mode, Mode::Stop) {
                return Err(error("Native audio teardown attempted a source read"));
            }
            let count = wire::integer(args, "count")?;
            if count == 0 || count > wire::DATA_BYTES as u64 {
                return Err(error("Invalid native audio verified range"));
            }
            let (data, eof) = resources.read(
                wire::text(args, "transfer")?,
                wire::integer(args, "offset")?,
                count as u32,
            )?;
            if data.len() > count as usize {
                return Err(error("Oversized native audio byte reply"));
            }
            resources.checkpoint()?;
            worker.send(
                json!({"type":"reply","frame":frame,"id":*requests,"eof":eof}),
                &data,
            )
        }
        "release" => {
            resources.release(wire::text(args, "transfer")?)?;
            resources.checkpoint()?;
            worker.send(json!({"type":"reply","frame":frame,"id":*requests}), &[])
        }
        _ => Err(error("Unknown native audio resource operation")),
    }
}
fn reply_json(
    worker: &mut WorkerProcess,
    frame: u64,
    id: u64,
    data: &[u8],
    resources: &mut dyn ClassicAudioResources,
) -> Result<()> {
    resources.checkpoint()?;
    worker.send(
        json!({"type":"json_reply","frame":frame,"id":id,"length":data.len()}),
        &[],
    )?;
    for (n, chunk) in data.chunks(wire::DATA_BYTES).enumerate() {
        resources.checkpoint()?;
        worker.send(
            json!({"type":"json_chunk","frame":frame,"id":id,"offset":n*wire::DATA_BYTES}),
            chunk,
        )?;
    }
    Ok(())
}
