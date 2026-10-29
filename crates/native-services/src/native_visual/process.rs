use super::{identity, wire, *};
use crate::classic_visual::{ClassicVisualProcessConfig, VisualProcessSession};
use crate::native_process::{Domain, ProcessConfig, WorkerProcess};
use opencut_editor_application::{
    classic_text_frame::{
        ClassicTextFontHandle, ClassicTextFrameResources, ClassicTextFrameSession,
    },
    classic_visual::*,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) struct NativeProcessFrame<R> {
    pub receipt: R,
    pub hits: Vec<NativeVisualHit>,
}
pub(crate) struct NativeProcess {
    config: ClassicVisualProcessConfig,
    worker: WorkerProcess,
    runtime: Option<ClassicVisualRuntimeReceipt>,
    text_runtime: Option<opencut_contracts::renderer_identity::NativeTextRuntimeIdentity>,
    next: u64,
    failed: bool,
}
impl NativeProcess {
    #[cfg(not(unix))]
    pub fn start(
        _config: ClassicVisualProcessConfig,
        _deadline: Duration,
        _progress: Option<Arc<Mutex<Instant>>>,
        _checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Err(EditorError::new(
            "CAPABILITY_UNAVAILABLE",
            "Native process lifecycle is unavailable on this platform",
        ))
    }
    #[cfg(not(unix))]
    pub(super) fn start_text(
        _config: ClassicVisualProcessConfig,
        _checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Err(EditorError::new(
            "CAPABILITY_UNAVAILABLE",
            "Native text process lifecycle is unavailable on this platform",
        ))
    }
    #[cfg(unix)]
    pub fn start(
        config: ClassicVisualProcessConfig,
        deadline: Duration,
        progress: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Self::start_kind(config, deadline, progress, checkpoint, false)
    }
    #[cfg(unix)]
    pub(super) fn start_text(
        config: ClassicVisualProcessConfig,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        Self::start_kind(config.clone(), config.deadline, None, checkpoint, true)
    }
    #[cfg(unix)]
    fn start_kind(
        config: ClassicVisualProcessConfig,
        deadline: Duration,
        progress: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
        text_only: bool,
    ) -> Result<Self, EditorError> {
        opencut_editor_application::media_frame_jobs::validate_policy(&config.policy)?;
        if config.deadline != Duration::from_millis(config.policy.deadline_ms) || deadline.is_zero()
        {
            return Err(invalid("Native process admission changed"));
        }
        checkpoint()?;
        super::platform_backend()?;
        let worker = WorkerProcess::start(ProcessConfig {
            worker: config.worker.clone(),
            max_rss_bytes: config.policy.max_rss_bytes,
            lifetime_deadline: deadline,
            operation_deadline: config.deadline,
            progress,
            strict_cgroup: config.strict_cgroup.clone(),
            domain: Domain::Visual,
        })?;
        let expected = worker.expected_hash.clone();
        let mut this = Self {
            config,
            worker,
            runtime: None,
            text_runtime: None,
            next: 0,
            failed: false,
        };
        let purpose = if text_only {
            "text_measurement"
        } else {
            "visual"
        };
        this.send(
            json!({"type":"initialize","purpose":purpose,"policy":this.config.policy}),
            &[],
        )?;
        let packet = loop {
            checkpoint()?;
            if let Some(packet) = this.receive()? {
                break packet;
            }
        };
        reject_failure(&packet)?;
        if wire::text(&packet.meta, "type")? != "ready"
            || wire::text(&packet.meta, "purpose")? != purpose
            || !packet.data.is_empty()
            || wire::integer(&packet.meta, "pid")? != u64::from(this.worker.monitor.pid())
        {
            return Err(wire::error("Invalid native readiness"));
        }
        if text_only {
            let text_identity: opencut_contracts::renderer_identity::NativeTextRuntimeIdentity =
                serde_json::from_value(packet.meta["identity"].clone())
                    .map_err(|_| wire::error("Invalid native text identity"))?;
            opencut_editor_application::classic_visual::validate_text_identity(&text_identity)?;
            if text_identity.worker_sha256 != expected
                || text_identity.rasterizer
                    != opencut_editor_application::classic_visual::NATIVE_RASTERIZER
                || text_identity.platform != identity::platform()?
            {
                return Err(invalid("Native text worker identity changed"));
            }
            this.text_runtime = Some(text_identity);
        } else {
            let identity: ClassicVisualEngineIdentity =
                serde_json::from_value(packet.meta["identity"].clone())
                    .map_err(|_| wire::error("Invalid native renderer identity"))?;
            validate_native_identity(&identity)?;
            if identity.renderer_sha256 != expected {
                return Err(invalid("Native renderer identity changed"));
            }
            this.runtime = Some(ClassicVisualRuntimeReceipt {
                worker_pid: this.worker.monitor.pid(),
                identity,
            });
        }
        this.worker.unchanged()?;
        this.worker.monitor.frame(None);
        checkpoint()?;
        Ok(this)
    }
    pub fn runtime(&self) -> &ClassicVisualRuntimeReceipt {
        self.runtime.as_ref().expect("ready native runtime")
    }
    pub(super) fn check(&self) -> Result<(), EditorError> {
        self.worker.monitor.check()?;
        if self.failed || !self.worker.monitor.running() {
            return Err(invalid("Native preview worker is no longer running"));
        }
        Ok(())
    }
    pub(super) fn quota(&self) -> VisualQuotaReceipt {
        self.worker.quota().into()
    }
    pub(super) fn retire(&mut self) {
        self.failed = true;
        self.worker.monitor.stop();
    }
    pub fn render<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
        &mut self,
        session: &mut S,
        resources: &mut R,
        begin: &str,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<S::Receipt, EditorError> {
        self.render_with_probe(
            session,
            resources,
            begin,
            handles,
            fonts,
            sink,
            None,
            SourceRaster::Original,
        )
        .map(|frame| frame.receipt)
    }
    pub(super) fn render_with_probe<
        R: ClassicVisualResources + ?Sized,
        S: VisualProcessSession<R>,
    >(
        &mut self,
        session: &mut S,
        resources: &mut R,
        begin: &str,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
        probe: Option<[u32; 2]>,
        source_raster: SourceRaster,
    ) -> Result<NativeProcessFrame<S::Receipt>, EditorError> {
        if self.failed || self.runtime.is_none() || session.policy() != &self.config.policy {
            return Err(invalid("Native frame owner or admission changed"));
        }
        self.worker
            .monitor
            .frame(Some(Instant::now() + self.config.deadline));
        let result = self.render_inner(
            session,
            resources,
            begin,
            handles,
            fonts,
            sink,
            probe,
            source_raster,
        );
        self.failed = result.is_err();
        if self.failed {
            self.worker.monitor.stop();
        } else {
            self.worker.monitor.frame(None);
            self.next += 1;
        }
        result
    }
    fn render_inner<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
        &mut self,
        session: &mut S,
        resources: &mut R,
        begin: &str,
        handles: &[ClassicVisualResourceHandle],
        fonts: &[ClassicTextFontHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
        probe: Option<[u32; 2]>,
        source_raster: SourceRaster,
    ) -> Result<NativeProcessFrame<S::Receipt>, EditorError> {
        self.exchange(session, resources,
            json!({"type":"frame","frame":self.next,"begin":begin,"plan":session.plan()?,"handles":handles,"fonts":fonts,"hit_probe":probe,"source_raster":source_raster}),
            Some(sink), "frame_complete", probe)
    }
    pub(super) fn measure(
        &mut self,
        session: &mut ClassicTextFrameSession<'_>,
        resources: &mut dyn ClassicTextFrameResources,
        fonts: &[ClassicTextFontHandle],
        options: &NativeTextMeasureOptions,
    ) -> Result<NativeTextMeasurement, EditorError> {
        if self.failed || self.text_runtime.is_none() || session.policy() != &self.config.policy {
            return Err(invalid(
                "Native text measurement owner or admission changed",
            ));
        }
        self.worker
            .monitor
            .frame(Some(Instant::now() + self.config.deadline));
        let message = json!({"type":"measure","frame":self.next,"plan":session.plan()?,"fonts":fonts,"options":options});
        let result = self.exchange(session, resources, message, None, "measure_complete", None);
        self.failed = result.is_err();
        if self.failed {
            self.worker.monitor.stop();
        } else {
            self.worker.monitor.frame(None);
            self.next += 1;
        }
        result.map(|frame| frame.receipt)
    }
    fn exchange<R, S, T>(
        &mut self,
        session: &mut S,
        resources: &mut R,
        message: Value,
        mut sink: Option<&mut dyn ClassicVisualRgbaSink>,
        completion: &str,
        probe: Option<[u32; 2]>,
    ) -> Result<NativeProcessFrame<T>, EditorError>
    where
        R: ClassicVisualResources + ?Sized,
        S: VisualProcessSession<R>,
        T: serde::de::DeserializeOwned,
    {
        resources.checkpoint()?;
        self.send(message, &[])?;
        let mut request = 0u64;
        let mut logical_requests = 0u64;
        loop {
            resources.checkpoint()?;
            let Some(packet) = self.receive()? else {
                continue;
            };
            reject_failure(&packet)?;
            if wire::integer(&packet.meta, "frame")? != self.next {
                return Err(wire::error("Native output belongs to another frame"));
            }
            match wire::text(&packet.meta, "type")? {
                "request" => {
                    request += 1;
                    if request > 200_000
                        || wire::integer(&packet.meta, "id")? != request
                        || !packet.data.is_empty()
                    {
                        return Err(wire::error("Native callback order changed"));
                    }
                    let args = &packet.meta["args"];
                    let operation = wire::text(&packet.meta, "operation")?;
                    let work = if operation == "callbacks" {
                        let count = args.as_array().map_or(0, Vec::len);
                        if count == 0 || count > crate::classic_visual::MAX_CALLBACK_BATCH {
                            return Err(wire::error("Invalid native callback batch"));
                        }
                        count as u64
                    } else {
                        1
                    };
                    logical_requests = logical_requests
                        .checked_add(work)
                        .filter(|count| *count <= 200_000)
                        .ok_or_else(|| wire::error("Excessive native frame requests"))?;
                    let mut response = json!({"type":"reply","frame":self.next,"id":request});
                    let mut bytes = Vec::new();
                    match operation {
                        "callbacks" => {
                            // Deserialize the whole envelope before any application
                            // action. Each original callback keeps fresh fences and
                            // one individually bounded reply; no aggregate replies.
                            let callbacks: Vec<S::Callback> =
                                crate::classic_visual::decode_callback_batch(args)?;
                            let mut index = 0u64;
                            session.callbacks(callbacks, resources, &mut |value| {
                                self.send(json!({"type":"reply","frame":self.next,"id":request,"index":index,"value":value}), &[])?;
                                index += 1;
                                Ok(())
                            })?;
                            if index != work {
                                return Err(wire::error(
                                    "Native callback batch results changed length",
                                ));
                            }
                            continue;
                        }
                        "callback" => {
                            let callback: S::Callback = serde_json::from_value(args.clone())
                                .map_err(|_| {
                                    wire::error("Native callback differs from typed session")
                                })?;
                            response["value"] = session.callback(callback, resources)?;
                        }
                        "read" => {
                            let count = wire::integer(args, "count")?;
                            if count == 0 || count > wire::DATA_BYTES as u64 {
                                return Err(wire::error("Invalid native byte request"));
                            }
                            let (data, eof) = resources.read(
                                wire::text(args, "transfer")?,
                                wire::integer(args, "offset")?,
                                count as u32,
                            )?;
                            if data.len() > count as usize {
                                return Err(wire::error("Oversized verified source response"));
                            }
                            bytes = data;
                            response["eof"] = json!(eof);
                        }
                        "release" => resources.release(wire::text(args, "transfer")?)?,
                        _ => return Err(wire::error("Unknown native resource operation")),
                    }
                    resources.checkpoint()?;
                    self.send(response, &bytes)?;
                }
                "rgba" => {
                    if packet.data.is_empty() || packet.data.len() % 4 != 0 {
                        return Err(wire::error("Invalid native RGBA chunk"));
                    }
                    session.rgba_chunk(
                        wire::integer(&packet.meta, "offset")?,
                        &packet.data,
                        &mut **sink
                            .as_mut()
                            .ok_or_else(|| wire::error("Text measurement cannot emit pixels"))?,
                    )?;
                }
                kind if kind == completion => {
                    if !packet.data.is_empty() {
                        return Err(wire::error("Unexpected completion bytes"));
                    }
                    resources.checkpoint()?;
                    let hits: Vec<NativeVisualHit> = if completion == "frame_complete" {
                        serde_json::from_value(packet.meta["hit_test"].clone())
                            .map_err(|_| wire::error("Invalid native hit observation"))?
                    } else {
                        Vec::new()
                    };
                    if (probe.is_none() && !hits.is_empty()) || hits.len() > 3900 {
                        return Err(wire::error("Unexpected native hit observations"));
                    }
                    return Ok(NativeProcessFrame {
                        receipt: serde_json::from_value(packet.meta["receipt"].clone()).map_err(
                            |_| wire::error("Native receipt differs from typed session"),
                        )?,
                        hits,
                    });
                }
                _ => return Err(wire::error("Unexpected native frame packet")),
            }
        }
    }
    fn send(&mut self, meta: Value, data: &[u8]) -> Result<(), EditorError> {
        self.worker.send(meta, data)
    }
    fn receive(&self) -> Result<Option<wire::Packet>, EditorError> {
        self.worker.receive()
    }
    pub fn finish(
        mut self,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<(ClassicVisualRuntimeReceipt, VisualQuotaReceipt), EditorError> {
        let quota = self.finish_inner(checkpoint)?;
        Ok((self.runtime().clone(), quota))
    }
    pub(super) fn finish_text(
        mut self,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<
        (
            opencut_contracts::renderer_identity::NativeTextRuntimeIdentity,
            VisualQuotaReceipt,
        ),
        EditorError,
    > {
        let quota = self.finish_inner(checkpoint)?;
        Ok((
            self.text_runtime
                .as_ref()
                .ok_or_else(|| invalid("Native worker is not a text measurer"))?
                .clone(),
            quota,
        ))
    }
    fn finish_inner(
        &mut self,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<VisualQuotaReceipt, EditorError> {
        if self.failed {
            return Err(wire::error("Cannot complete a retired native worker"));
        }
        self.worker
            .monitor
            .frame(Some(Instant::now() + self.config.deadline));
        checkpoint()?;
        self.send(json!({"type":"finish","frames":self.next}), &[])?;
        let packet = loop {
            checkpoint()?;
            if let Some(packet) = self.receive()? {
                break packet;
            }
        };
        reject_failure(&packet)?;
        if wire::text(&packet.meta, "type")? != "complete"
            || wire::integer(&packet.meta, "frames")? != self.next
            || !packet.data.is_empty()
        {
            return Err(wire::error("Invalid native completion"));
        }
        let quota = self.worker.complete()?.into();
        checkpoint()?;
        Ok(quota)
    }
}
fn reject_failure(packet: &wire::Packet) -> Result<(), EditorError> {
    if wire::text(&packet.meta, "type")? == "failed" {
        return Err(EditorError::new(
            wire::text(&packet.meta, "code")?,
            wire::text(&packet.meta, "message")?,
        ));
    }
    Ok(())
}
