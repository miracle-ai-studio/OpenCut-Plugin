//! Parent owns authorization, typed application callbacks, PCM sink and final
//! publication fence. The shared process monitor can kill stalled codec calls.
use super::{
    identity, invalid,
    protocol::{self, error},
};
use crate::native_process::{self, Domain, ProcessConfig, WorkerProcess, wire};
use opencut_contracts::{EditorError, renderer_identity::NativeAudioIdentity};
use opencut_editor_application::{
    classic_audio::*,
    scene_audio_jobs::{LocalAudioExecutionPolicy, validate_policy},
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
pub struct NativeAudioProcessConfig {
    pub worker: PathBuf,
    pub policy: LocalAudioExecutionPolicy,
    pub strict_cgroup: Option<PathBuf>,
}
#[derive(Debug)]
pub struct NativeAudioRuntimeReceipt {
    pub worker_pid: u32,
    pub identity: NativeAudioIdentity,
}
#[derive(Debug)]
pub struct NativeAudioExecutionReceipt {
    pub receipt: ClassicAudioRenderReceipt,
    pub quota: AudioQuotaReceipt,
    pub runtime_identity: NativeAudioRuntimeReceipt,
}
pub struct NativeAudioProcess {
    pub config: NativeAudioProcessConfig,
}
impl NativeAudioProcess {
    pub fn execute(
        &self,
        session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
        handles: &[ClassicAudioResourceHandle],
        sink: &mut dyn ClassicAudioPcmSink,
    ) -> Result<NativeAudioExecutionReceipt, EditorError> {
        self.execute_inner(session, resources, handles, sink)
            .map_err(|e| Domain::Audio.protocol(e))
    }
    fn execute_inner(
        &self,
        session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
        handles: &[ClassicAudioResourceHandle],
        sink: &mut dyn ClassicAudioPcmSink,
    ) -> Result<NativeAudioExecutionReceipt, EditorError> {
        let config = &self.config;
        validate_policy(&config.policy)?;
        resources.checkpoint()?;
        let deadline = Duration::from_millis(config.policy.deadline_ms);
        let mut worker = WorkerProcess::start(ProcessConfig {
            worker: config.worker.clone(),
            max_rss_bytes: config.policy.max_rss_bytes,
            lifetime_deadline: deadline,
            operation_deadline: deadline,
            progress: None,
            strict_cgroup: config.strict_cgroup.clone(),
            domain: Domain::Audio,
        })?;
        worker.send(
            json!({"type":"initialize","purpose":"audio_export","policy":config.policy}),
            &[],
        )?;
        let ready = receive(&worker, resources)?;
        protocol::reject_failure(&ready)?;
        if ready.meta["type"] != "ready"
            || ready.meta["purpose"] != "audio_export"
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
        worker.send(
            json!({"type":"audio","frame":0,"plan":session.plan()?,"handles":handles}),
            &[],
        )?;
        let mut requests = 0u64;
        let mut offsets = [0u64; 2];
        let receipt = loop {
            let packet = receive(&worker, resources)?;
            protocol::reject_failure(&packet)?;
            if wire::integer(&packet.meta, "frame")? != 0 {
                return Err(error("Native audio output belongs to another command"));
            }
            match wire::text(&packet.meta, "type")? {
                "request" => {
                    requests = requests
                        .checked_add(1)
                        .filter(|n| *n <= 100000)
                        .ok_or_else(|| error("Native audio callback limit exceeded"))?;
                    if wire::integer(&packet.meta, "id")? != requests || !packet.data.is_empty() {
                        return Err(error("Native audio callback order changed"));
                    }
                    let args = &packet.meta["args"];
                    match wire::text(&packet.meta, "operation")? {
                        "callback" => {
                            let request: AudioCallback = serde_json::from_value(args.clone())
                                .map_err(|_| error("Invalid typed native audio callback"))?;
                            let data = match request {
                                AudioCallback::Block(request) => protocol::encode(
                                    &session.compile_export_block_typed(request, resources)?,
                                )?,
                                request => {
                                    protocol::encode(&session.callback(request, resources)?)?
                                }
                            };
                            resources.checkpoint()?;
                            worker.send(json!({"type":"json_reply","frame":0,"id":requests,"length":data.len()}),&[])?;
                            for (n, chunk) in data.chunks(wire::DATA_BYTES).enumerate() {
                                resources.checkpoint()?;
                                worker.send(json!({"type":"json_chunk","frame":0,"id":requests,"offset":n*wire::DATA_BYTES}),chunk)?;
                            }
                        }
                        "read" => {
                            let count = wire::integer(args, "count")?;
                            if count == 0 || count > wire::DATA_BYTES as u64 {
                                return Err(error("Invalid native audio verified range"));
                            }
                            let (bytes, eof) = resources.read(
                                wire::text(args, "transfer")?,
                                wire::integer(args, "offset")?,
                                count as u32,
                            )?;
                            if bytes.len() > count as usize {
                                return Err(error("Oversized native audio byte reply"));
                            }
                            resources.checkpoint()?;
                            worker.send(
                                json!({"type":"reply","frame":0,"id":requests,"eof":eof}),
                                &bytes,
                            )?;
                        }
                        "release" => {
                            resources.release(wire::text(args, "transfer")?)?;
                            resources.checkpoint()?;
                            worker.send(json!({"type":"reply","frame":0,"id":requests}), &[])?;
                        }
                        _ => return Err(error("Unknown native audio resource operation")),
                    }
                }
                "pcm" => {
                    let channel = wire::integer(&packet.meta, "channel")?;
                    let offset = wire::integer(&packet.meta, "offset")?;
                    let count = wire::integer(&packet.meta, "count")?;
                    if channel > 1
                        || offset != offsets[channel as usize]
                        || count == 0
                        || count > AUDIO_PCM_CHUNK_FRAMES as u64
                        || count as usize * 4 != packet.data.len()
                        || offset
                            .checked_add(count)
                            .is_none_or(|n| n > session.output_frame_count())
                    {
                        return Err(error(
                            "Native audio PCM changed its admitted channel or extent",
                        ));
                    }
                    let samples: Vec<f32> = packet
                        .data
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                        .collect();
                    if samples.iter().any(|v| !v.is_finite()) {
                        return Err(error("Native audio returned non-finite PCM"));
                    }
                    resources.checkpoint()?;
                    sink.chunk(channel as u32, offset, &samples)?;
                    resources.checkpoint()?;
                    offsets[channel as usize] += count;
                }
                "audio_complete" => {
                    if !packet.data.is_empty() || offsets != [session.output_frame_count(); 2] {
                        return Err(error("Native audio completion lost PCM samples"));
                    }
                    let receipt: ClassicAudioRenderReceipt =
                        serde_json::from_value(packet.meta["receipt"].clone())
                            .map_err(|_| error("Invalid native audio completion receipt"))?;
                    session.validate_native_receipt(&receipt)?;
                    if receipt.engine_identity != identity.worker_sha256 {
                        return Err(invalid("Native audio receipt changed its worker identity"));
                    }
                    break receipt;
                }
                _ => return Err(error("Unexpected native audio process packet")),
            }
        };
        resources.checkpoint()?;
        worker.send(json!({"type":"finish","frames":1}), &[])?;
        let complete = receive(&worker, resources)?;
        protocol::reject_failure(&complete)?;
        if complete.meta["type"] != "complete"
            || wire::integer(&complete.meta, "frames")? != 1
            || !complete.data.is_empty()
        {
            return Err(error("Invalid native audio teardown receipt"));
        }
        let worker_pid = worker.pid();
        let quota: AudioQuotaReceipt = worker.complete()?.into();
        if quota.max_rss_bytes != config.policy.max_rss_bytes.to_string()
            || quota
                .observed_peak_rss_bytes
                .parse::<u64>()
                .ok()
                .is_none_or(|n| n > config.policy.max_rss_bytes)
            || quota.elapsed_ms > config.policy.deadline_ms
            || quota.monitor_interval_ms != 25
        {
            return Err(error(
                "Native audio quota differs from its captured admission",
            ));
        }
        resources.checkpoint()?;
        Ok(NativeAudioExecutionReceipt {
            receipt,
            quota,
            runtime_identity: NativeAudioRuntimeReceipt {
                worker_pid,
                identity,
            },
        })
    }
}
fn receive(
    worker: &WorkerProcess,
    resources: &mut dyn ClassicAudioResources,
) -> Result<wire::Packet, EditorError> {
    loop {
        resources.checkpoint()?;
        if let Some(packet) = worker.receive()? {
            resources.checkpoint()?;
            return Ok(packet);
        }
    }
}
