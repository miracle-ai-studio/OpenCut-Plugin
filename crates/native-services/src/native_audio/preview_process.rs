//! Parent-owned persistent native audio preview. All application callbacks and
//! data publications retain live authorization, captured command and quota fences.
use super::{
    dsp::ClassicCompressor,
    preview_protocol::{BlockReceipt, Policy},
    protocol::{self, error},
    rpc,
};
use crate::native_process::{Domain, ProcessConfig, WorkerProcess, wire};
use opencut_contracts::{EditorError, renderer_identity::NativeAudioIdentity};
use opencut_editor_application::classic_audio::*;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
pub struct NativeAudioPreviewProcessConfig {
    pub worker: PathBuf,
    pub preparation_deadline: Duration,
    pub operation_deadline: Duration,
    pub max_pcm_bytes: u64,
    pub max_encoded_bytes: u64,
    pub max_rss_bytes: u64,
    pub strict_cgroup: Option<PathBuf>,
}
pub struct NativeAudioPreviewPcmBlock {
    pub generation: u64,
    pub overlay_epoch: u64,
    pub start_sample: u64,
    pub frame_count: u32,
    pub channels: [Vec<f32>; 2],
    pub receipt: Value,
}
pub struct NativeAudioPreviewProducer {
    worker: Option<WorkerProcess>,
    policy: Policy,
    identity: NativeAudioIdentity,
    prepared: ClassicAudioPreviewReceipt,
    frame: u64,
    current: Option<(u64, u64)>,
    next_output: u64,
    next_input: u64,
    total: u64,
}
impl NativeAudioPreviewProducer {
    pub fn start(
        config: &NativeAudioPreviewProcessConfig,
        session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
        handles: &[ClassicAudioResourceHandle],
    ) -> Result<Self, EditorError> {
        let policy = Policy {
            preparation_deadline_ms: u64::try_from(config.preparation_deadline.as_millis())
                .map_err(|_| error("Invalid native preview preparation deadline"))?,
            operation_deadline_ms: u64::try_from(config.operation_deadline.as_millis())
                .map_err(|_| error("Invalid native preview operation deadline"))?,
            lifetime_deadline_ms: 3600000,
            max_pcm_bytes: config.max_pcm_bytes,
            max_encoded_bytes: config.max_encoded_bytes,
            max_rss_bytes: config.max_rss_bytes,
        };
        policy.validate()?;
        resources.checkpoint()?;
        if session.preview_prepared().is_none() {
            return Err(error("A native preview session is required"));
        }
        let mut worker = WorkerProcess::start(ProcessConfig {
            worker: config.worker.clone(),
            max_rss_bytes: policy.max_rss_bytes,
            lifetime_deadline: Duration::from_millis(policy.lifetime_deadline_ms),
            operation_deadline: config.preparation_deadline,
            progress: None,
            strict_cgroup: config.strict_cgroup.clone(),
            domain: Domain::Audio,
        })?;
        let identity = rpc::initialize(
            &mut worker,
            "audio_preview",
            serde_json::to_value(&policy).map_err(|_| error("Invalid native preview policy"))?,
            resources,
        )?;
        let plan = session.plan()?;
        worker.send(
            json!({"type":"preview_prepare","frame":0,"plan":plan,"handles":handles}),
            &[],
        )?;
        let mut requests = 0;
        let prepared = loop {
            let packet = rpc::receive(&worker, resources)?;
            if wire::integer(&packet.meta, "frame")? != 0 {
                return Err(error("Native preview preparation changed its command"));
            }
            match wire::text(&packet.meta, "type")? {
                "request" => rpc::service(
                    &mut worker,
                    packet,
                    0,
                    &mut requests,
                    session,
                    resources,
                    rpc::Mode::Preparation,
                )?,
                "preview_prepared" => {
                    if !packet.data.is_empty() {
                        return Err(error("Unexpected native preview receipt bytes"));
                    }
                    let length = usize::try_from(wire::integer(&packet.meta, "length")?)
                        .map_err(|_| error("Invalid native preview receipt length"))?;
                    if length == 0 || length > protocol::JSON_BYTES {
                        return Err(error("Native preview receipt exceeds its fact envelope"));
                    }
                    let mut data = Vec::new();
                    data.try_reserve_exact(length)
                        .map_err(|_| error("Cannot allocate admitted native preview receipt"))?;
                    while data.len() < length {
                        let chunk = rpc::receive(&worker, resources)?;
                        if chunk.meta["type"] != "result_chunk"
                            || wire::integer(&chunk.meta, "frame")? != 0
                            || wire::integer(&chunk.meta, "offset")? != data.len() as u64
                            || chunk.data.is_empty()
                            || chunk.data.len() > length - data.len()
                        {
                            return Err(error(
                                "Native preview receipt changed its order or extent",
                            ));
                        }
                        data.extend_from_slice(&chunk.data);
                    }
                    let receipt: ClassicAudioPreviewReceipt = serde_json::from_slice(&data)
                        .map_err(|_| error("Invalid typed native preview preparation receipt"))?;
                    session.validate_preview_receipt(&receipt)?;
                    if receipt.engine_identity != identity.worker_sha256 {
                        return Err(error("Native preview changed its worker identity"));
                    }
                    break receipt;
                }
                _ => return Err(error("Unexpected native preview preparation packet")),
            }
        };
        worker.monitor.frame(None);
        resources.checkpoint()?;
        Ok(Self {
            worker: Some(worker),
            policy,
            identity,
            prepared,
            frame: 0,
            current: None,
            next_output: 0,
            next_input: 0,
            total: session.output_frame_count(),
        })
    }
    pub fn worker_pid(&self) -> Option<u32> {
        self.worker.as_ref().map(WorkerProcess::pid)
    }
    pub fn runtime_identity(&self) -> &NativeAudioIdentity {
        &self.identity
    }
    pub fn ready_receipt(&self) -> &ClassicAudioPreviewReceipt {
        &self.prepared
    }
    pub fn checkpoint(
        &mut self,
        _session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
    ) -> Result<(), EditorError> {
        let result = (|| {
            resources.checkpoint()?;
            let worker = self
                .worker
                .as_ref()
                .ok_or_else(|| error("Native preview producer has stopped"))?;
            worker.check()?;
            if !worker.monitor.running() {
                return Err(error(
                    "Native preview worker exited while owning its session",
                ));
            }
            Ok(())
        })();
        if result.is_err() {
            self.worker.take();
        }
        result
    }
    pub fn next(
        &mut self,
        session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
        generation: u64,
        overlay_epoch: u64,
        start: u64,
        count: u32,
    ) -> Result<NativeAudioPreviewPcmBlock, EditorError> {
        let result = self.next_inner(session, resources, generation, overlay_epoch, start, count);
        if result.is_err() {
            self.worker.take();
        }
        result.map_err(|e| Domain::Audio.protocol(e))
    }
    fn next_inner(
        &mut self,
        session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
        generation: u64,
        epoch: u64,
        start: u64,
        count: u32,
    ) -> Result<NativeAudioPreviewPcmBlock, EditorError> {
        resources.checkpoint()?;
        if count == 0
            || count > AUDIO_PCM_CHUNK_FRAMES as u32
            || start
                .checked_add(u64::from(count))
                .is_none_or(|n| n > self.total)
        {
            return Err(error("Native preview output exceeds the fixed scene"));
        }
        let reset = self.current != Some((generation, epoch));
        if !reset && start != self.next_output {
            return Err(error(
                "Native preview output is not the captured contiguous prefix",
            ));
        }
        let lookahead =
            ClassicCompressor::lookahead_frames_for_rate(self.prepared.identity.sample_rate) as u64;
        let cursor = if reset {
            start
        } else {
            self.next_input.min(self.total)
        };
        let input_end = start
            .checked_add(u64::from(count))
            .and_then(|n| n.checked_add(lookahead))
            .ok_or_else(|| error("Native preview read-ahead overflow"))?;
        let mut window = rpc::PreviewWindow::new(cursor, input_end.min(self.total))?;
        self.frame = self
            .frame
            .checked_add(1)
            .ok_or_else(|| error("Native preview command counter overflow"))?;
        let worker = self
            .worker
            .as_mut()
            .ok_or_else(|| error("Native preview producer has stopped"))?;
        worker.monitor.frame(Some(
            Instant::now() + Duration::from_millis(self.policy.operation_deadline_ms),
        ));
        worker.send(json!({"type":"preview_next","frame":self.frame,"generation":generation,"overlay_epoch":epoch,"start_sample":start,"frame_count":count}),&[])?;
        let mut requests = 0;
        let mut channels: [Option<Vec<f32>>; 2] = [None, None];
        let receipt = loop {
            let packet = rpc::receive(worker, resources)?;
            if wire::integer(&packet.meta, "frame")? != self.frame {
                return Err(error("Native preview output belongs to another command"));
            }
            match wire::text(&packet.meta, "type")? {
                "request" | "request_json" => rpc::service(
                    worker,
                    packet,
                    self.frame,
                    &mut requests,
                    session,
                    resources,
                    rpc::Mode::Preview(&mut window),
                )?,
                "preview_pcm" => {
                    let channel = usize::try_from(wire::integer(&packet.meta, "channel")?)
                        .map_err(|_| error("Invalid native preview channel"))?;
                    if channel > 1
                        || channels[channel].is_some()
                        || wire::integer(&packet.meta, "count")? != u64::from(count)
                        || wire::integer(&packet.meta, "generation")? != generation
                        || wire::integer(&packet.meta, "overlay_epoch")? != epoch
                        || wire::integer(&packet.meta, "start_sample")? != start
                        || packet.data.len() != count as usize * 4
                    {
                        return Err(error(
                            "Native preview PCM changed its command, channel or extent",
                        ));
                    }
                    let samples: Vec<f32> = packet
                        .data
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                        .collect();
                    if samples.iter().any(|v| !v.is_finite()) {
                        return Err(error("Native preview returned non-finite PCM"));
                    }
                    channels[channel] = Some(samples);
                }
                "preview_block" => {
                    if !packet.data.is_empty()
                        || channels.iter().any(Option::is_none)
                        || !window.complete()
                    {
                        return Err(error(
                            "Native preview completion omitted PCM or input bindings",
                        ));
                    }
                    let receipt: BlockReceipt =
                        serde_json::from_value(packet.meta["receipt"].clone())
                            .map_err(|_| error("Invalid native preview block receipt"))?;
                    if receipt.identity != self.prepared.identity
                        || receipt.engine_identity != self.identity.worker_sha256
                        || receipt.generation != generation
                        || receipt.overlay_epoch != epoch
                        || receipt.start_sample != start.to_string()
                        || receipt.frame_count != count
                        || u64::from(receipt.lookahead_frames) != lookahead
                        || receipt.inputs != window.bindings
                        || receipt.peak_accounted_bytes > self.policy.max_pcm_bytes
                    {
                        return Err(error(
                            "Native preview receipt changed its fixed execution or accepted bindings",
                        ));
                    }
                    break serde_json::to_value(receipt)
                        .map_err(|_| error("Invalid native preview receipt"))?;
                }
                _ => return Err(error("Unexpected native preview block packet")),
            }
        };
        resources.checkpoint()?;
        worker.monitor.frame(None);
        self.current = Some((generation, epoch));
        self.next_output = start + u64::from(count);
        self.next_input = input_end;
        Ok(NativeAudioPreviewPcmBlock {
            generation,
            overlay_epoch: epoch,
            start_sample: start,
            frame_count: count,
            channels: channels.map(|c| c.expect("Completeness checked")),
            receipt,
        })
    }
    pub fn stop(
        &mut self,
        session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
    ) -> Result<(), EditorError> {
        let Some(mut worker) = self.worker.take() else {
            return Ok(());
        };
        resources.checkpoint()?;
        self.frame = self
            .frame
            .checked_add(1)
            .ok_or_else(|| error("Native preview command counter overflow"))?;
        worker.monitor.frame(Some(
            Instant::now() + Duration::from_millis(self.policy.operation_deadline_ms),
        ));
        worker.send(json!({"type":"finish","frame":self.frame}), &[])?;
        let mut requests = 0;
        loop {
            let packet = rpc::receive(&worker, resources)?;
            if wire::integer(&packet.meta, "frame")? != self.frame {
                return Err(error("Native preview teardown changed its command"));
            }
            match wire::text(&packet.meta, "type")? {
                "request" => rpc::service(
                    &mut worker,
                    packet,
                    self.frame,
                    &mut requests,
                    session,
                    resources,
                    rpc::Mode::Stop,
                )?,
                "complete" if packet.data.is_empty() => break,
                _ => return Err(error("Unexpected native preview teardown packet")),
            }
        }
        let quota: AudioQuotaReceipt = worker.complete()?.into();
        if quota.max_rss_bytes != self.policy.max_rss_bytes.to_string()
            || quota
                .observed_peak_rss_bytes
                .parse::<u64>()
                .ok()
                .is_none_or(|n| n > self.policy.max_rss_bytes)
            || quota.monitor_interval_ms != 25
            || quota.elapsed_ms > self.policy.lifetime_deadline_ms
        {
            return Err(error(
                "Native preview quota differs from its captured admission",
            ));
        }
        resources.checkpoint()
    }
}
