use super::{identity, monitor::Monitor, wire, *};
use crate::classic_visual::{ClassicVisualProcessConfig, VisualProcessSession};
use opencut_editor_application::{classic_text_frame::ClassicTextFontHandle, classic_visual::*};
use serde_json::{Value, json};
use std::{
    io::{BufReader, Read},
    process::{ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub(crate) struct NativeProcess {
    config: ClassicVisualProcessConfig,
    input: Option<ChildStdin>,
    monitor: Monitor,
    receiver: Option<mpsc::Receiver<Result<wire::Packet, EditorError>>>,
    reader: Option<JoinHandle<()>>,
    diagnostics: Option<JoinHandle<()>>,
    runtime: Option<ClassicVisualRuntimeReceipt>,
    next: u64,
    failed: bool,
}
impl NativeProcess {
    #[cfg(unix)]
    pub fn start(
        config: ClassicVisualProcessConfig,
        deadline: Duration,
        progress: Option<Arc<Mutex<Instant>>>,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<Self, EditorError> {
        use std::os::unix::process::CommandExt;
        opencut_editor_application::media_frame_jobs::validate_policy(&config.policy)?;
        if config.deadline != Duration::from_millis(config.policy.deadline_ms) || deadline.is_zero()
        {
            return Err(invalid("Native process admission changed"));
        }
        checkpoint()?;
        super::platform_backend()?;
        let expected = identity::file_hash(&config.worker)?;
        let mut child = Command::new(&config.worker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|_| {
                EditorError::new(
                    "CAPABILITY_UNAVAILABLE",
                    "Cannot start configured native renderer",
                )
            })?;
        let input = child.stdin.take().expect("piped native input");
        let stdout = child.stdout.take().expect("piped native output");
        let stderr = child.stderr.take().expect("piped native diagnostics");
        let monitor = Monitor::new(child, config.policy.max_rss_bytes, deadline, progress);
        monitor.frame(Some(Instant::now() + config.deadline));
        if let Some(directory) = &config.strict_cgroup {
            attach_cgroup(directory, monitor.pid(), config.policy.max_rss_bytes)?;
        }
        let (sender, receiver) = mpsc::sync_channel(2);
        let reader = std::thread::spawn(move || {
            let mut input = BufReader::new(stdout);
            loop {
                let packet = wire::read(&mut input);
                let failed = packet.is_err();
                if sender.send(packet).is_err() || failed {
                    break;
                }
            }
        });
        let diagnostics = std::thread::spawn(move || {
            let mut input = stderr;
            let mut data = [0; 4096];
            while input.read(&mut data).is_ok_and(|n| n != 0) {}
        });
        // Runtime is replaced only by the verified ready packet; this temporary
        // value never escapes construction or becomes completion evidence.
        let mut this = Self {
            config,
            input: Some(input),
            monitor,
            receiver: Some(receiver),
            reader: Some(reader),
            diagnostics: Some(diagnostics),
            runtime: None,
            next: 0,
            failed: false,
        };
        wire::send(
            this.input.as_mut().unwrap(),
            json!({"type":"initialize","policy":this.config.policy}),
            &[],
        )?;
        let packet = loop {
            checkpoint()?;
            this.monitor.check()?;
            match this
                .receiver
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_millis(25))
            {
                Ok(packet) => break packet?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err(wire::error("Native startup closed")),
            }
        };
        reject_failure(&packet)?;
        if wire::text(&packet.meta, "type")? != "ready"
            || !packet.data.is_empty()
            || wire::integer(&packet.meta, "pid")? != u64::from(this.monitor.pid())
        {
            return Err(wire::error("Invalid native readiness"));
        }
        let identity: ClassicVisualEngineIdentity =
            serde_json::from_value(packet.meta["identity"].clone())
                .map_err(|_| wire::error("Invalid native renderer identity"))?;
        validate_native_identity(&identity)?;
        if identity.renderer_sha256 != expected
            || identity::file_hash(&this.config.worker)? != expected
        {
            return Err(EditorError::new(
                "VISUAL_ENGINE_CHANGED",
                "Configured native renderer changed during startup",
            ));
        }
        this.monitor.frame(None);
        checkpoint()?;
        this.runtime = Some(ClassicVisualRuntimeReceipt {
            worker_pid: this.monitor.pid(),
            identity,
        });
        Ok(this)
    }
    pub fn runtime(&self) -> &ClassicVisualRuntimeReceipt {
        self.runtime.as_ref().expect("ready native runtime")
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
        if self.failed || session.policy() != &self.config.policy {
            return Err(invalid("Native frame owner or admission changed"));
        }
        self.monitor
            .frame(Some(Instant::now() + self.config.deadline));
        let result = self.render_inner(session, resources, begin, handles, fonts, sink);
        self.failed = result.is_err();
        if self.failed {
            self.monitor.stop();
        } else {
            self.monitor.frame(None);
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
    ) -> Result<S::Receipt, EditorError> {
        resources.checkpoint()?;
        self.send(json!({"type":"frame","frame":self.next,"begin":begin,"plan":session.plan()?,"handles":handles,"fonts":fonts}),&[])?;
        let mut request = 0u64;
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
                    let mut response = json!({"type":"reply","frame":self.next,"id":request});
                    let mut bytes = Vec::new();
                    match wire::text(&packet.meta, "operation")? {
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
                        sink,
                    )?;
                }
                "frame_complete" => {
                    if !packet.data.is_empty() {
                        return Err(wire::error("Unexpected completion bytes"));
                    }
                    resources.checkpoint()?;
                    return serde_json::from_value(packet.meta["receipt"].clone())
                        .map_err(|_| wire::error("Native receipt differs from typed session"));
                }
                _ => return Err(wire::error("Unexpected native frame packet")),
            }
        }
    }
    fn send(&mut self, meta: Value, data: &[u8]) -> Result<(), EditorError> {
        self.monitor.check()?;
        let result = wire::send(
            self.input
                .as_mut()
                .ok_or_else(|| wire::error("Closed native input"))?,
            meta,
            data,
        );
        self.monitor.check()?;
        result
    }
    fn receive(&self) -> Result<Option<wire::Packet>, EditorError> {
        self.monitor.check()?;
        match self
            .receiver
            .as_ref()
            .ok_or_else(|| wire::error("Closed native output"))?
            .recv_timeout(Duration::from_millis(25))
        {
            Ok(packet) => {
                self.monitor.check()?;
                packet.map(Some)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(_) => Err(wire::error("Native output closed")),
        }
    }
    pub fn finish(
        mut self,
        checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
    ) -> Result<(ClassicVisualRuntimeReceipt, VisualQuotaReceipt), EditorError> {
        if self.failed {
            return Err(wire::error("Cannot complete a retired native worker"));
        }
        self.monitor
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
        self.input.take();
        if !self.monitor.reap()?.success() {
            return Err(wire::error("Native worker exited unsuccessfully"));
        }
        if identity::file_hash(&self.config.worker)? != self.runtime().identity.renderer_sha256 {
            return Err(EditorError::new(
                "VISUAL_ENGINE_CHANGED",
                "Native worker changed during execution",
            ));
        }
        let quota = self.monitor.quota(
            self.config.policy.max_rss_bytes,
            self.config.strict_cgroup.is_some(),
        );
        checkpoint()?;
        Ok((self.runtime().clone(), quota))
    }
}
impl Drop for NativeProcess {
    fn drop(&mut self) {
        self.input.take();
        self.monitor.stop();
        self.receiver.take();
        if let Some(t) = self.reader.take() {
            let _ = t.join();
        }
        if let Some(t) = self.diagnostics.take() {
            let _ = t.join();
        }
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
fn attach_cgroup(path: &std::path::Path, pid: u32, max_rss: u64) -> Result<(), EditorError> {
    #[cfg(target_os = "linux")]
    {
        if !path.is_absolute() {
            return Err(invalid("Native cgroup must be absolute"));
        }
        let maximum = std::fs::read_to_string(path.join("memory.max"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .ok_or_else(|| invalid("Native cgroup lacks a hard memory limit"))?;
        if maximum == 0 || maximum > max_rss {
            return Err(invalid("Native cgroup limit exceeds admission"));
        }
        std::fs::write(path.join("cgroup.procs"), pid.to_string())
            .map_err(|_| invalid("Cannot attach native worker to cgroup"))?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, pid, max_rss);
        Err(EditorError::new(
            "CAPABILITY_UNAVAILABLE",
            "Hard cgroup quota requires Linux",
        ))
    }
}
