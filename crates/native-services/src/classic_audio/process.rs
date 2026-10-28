use super::transport::*;
use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::*;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

/// Configured by host assembly, never accepted from a public tool request.
pub struct ClassicAudioProcessConfig {
    pub node: PathBuf,
    pub runner: PathBuf,
    pub chromium: PathBuf,
    pub deadline: Duration,
    pub max_pcm_bytes: u64,
    pub max_encoded_bytes: u64,
    pub max_rss_bytes: u64,
    /// Strict hard admission requires an actual enforced Linux cgroup. macOS
    /// callers must explicitly select monitored termination instead.
    pub strict_cgroup: Option<PathBuf>,
}
pub type ClassicAudioProcessResult = ClassicAudioExecutionReceipt;
pub struct ClassicAudioProcess {
    pub config: ClassicAudioProcessConfig,
}

impl ClassicAudioProcess {
    #[cfg(unix)]
    pub fn render(
        &self,
        session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
        handles: &[ClassicAudioResourceHandle],
        sink: &mut dyn ClassicAudioPcmSink,
    ) -> Result<ClassicAudioProcessResult, EditorError> {
        let config = &self.config;
        if config.deadline.is_zero()
            || config.deadline > Duration::from_secs(300)
            || config.max_rss_bytes == 0
            || config.max_pcm_bytes > 512 * 1024 * 1024
        {
            return Err(error(
                "INVALID_AUDIO_PLAN",
                "Invalid isolated audio process admission",
            ));
        }
        resources.checkpoint()?;
        let execution = uuid::Uuid::new_v4().to_string();
        let plan = session.plan()?;
        let frames = session.output_frame_count();
        let mut process = crate::browser_process::BrowserProcess::start(
            &config.node,
            &config.runner,
            config.deadline,
        )?;
        let mut stdin = process
            .input
            .take()
            .ok_or_else(|| error("AUDIO_HOST_FAILED", "Missing runner input"))?;
        let pid = process.guard.child.id();
        let begin = json!({"type":"begin","plan":plan,"resources":handles,
            "runtime":{"chromiumExecutable":config.chromium},
            "admission":{"maxPcmBytes":config.max_pcm_bytes,"maxEncodedBytes":config.max_encoded_bytes,
                "maxRssBytes":config.max_rss_bytes.to_string(),"deadlineMs":config.deadline.as_millis() as u64,
                "quotaProfile":if config.strict_cgroup.is_some() {"strict-hard"} else {"monitored-terminate"},
                "cgroupDirectory":config.strict_cgroup}});
        let started = Instant::now();
        let result = (|| {
            packet(&mut stdin, &execution, begin)?;
            let mut runtime = None;
            let mut offsets = [0u64; 2];
            let mut requests = std::collections::HashSet::new();
            loop {
                resources.checkpoint()?;
                if started.elapsed() >= config.deadline {
                    return Err(error(
                        "AUDIO_TIME_LIMIT",
                        "Audio process exceeded its deadline",
                    ));
                }
                let message = match process.receive()? {
                    Some(message) => message,
                    None => continue,
                };
                if message.get("executionId").and_then(Value::as_str) != Some(&execution) {
                    return Err(error(
                        "AUDIO_PROTOCOL_ERROR",
                        "Audio packet changed its execution identity",
                    ));
                }
                match string(&message, "type")? {
                    "started" => {
                        let browser = number(&message, "browserPid")?;
                        if browser == 0
                            || browser == u64::from(pid)
                            || browser > u64::from(i32::MAX as u32)
                            || process.guard.browser_pid.load(Ordering::Acquire) != 0
                        {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "Invalid or duplicate browser process identity",
                            ));
                        }
                        process
                            .guard
                            .browser_pid
                            .store(browser as u32, Ordering::Release);
                    }
                    "ready" => {
                        if runtime.is_some() {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "Duplicate runner readiness",
                            ));
                        }
                        let browser = number(&message, "browserPid")?;
                        if browser == 0
                            || browser
                                != u64::from(process.guard.browser_pid.load(Ordering::Acquire))
                        {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "Browser process identity changed",
                            ));
                        }
                        let identity = message.get("identity").ok_or_else(|| {
                            error("AUDIO_PROTOCOL_ERROR", "Missing actual runtime identity")
                        })?;
                        if identity.get("playwrightVersion").and_then(Value::as_str)
                            != Some("1.63.0")
                            || identity.get("mediabunnyVersion").and_then(Value::as_str)
                                != Some("1.41.0")
                            || identity
                                .get("engine")
                                .and_then(Value::as_str)
                                .is_none_or(str::is_empty)
                            || ["browserSha256", "moduleSha256"].iter().any(|key| {
                                identity
                                    .get(key)
                                    .and_then(Value::as_str)
                                    .is_none_or(|hash| {
                                        hash.len() != 64
                                            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
                                    })
                            })
                        {
                            return Err(error(
                                "AUDIO_ENGINE_CHANGED",
                                "Browser runtime lacks the pinned actual execution identity",
                            ));
                        }
                        runtime = Some(message);
                    }
                    "request" => {
                        if runtime.is_none() {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "Callback before actual browser readiness",
                            ));
                        }
                        let id = string(&message, "id")?.to_owned();
                        if id.is_empty()
                            || id.len() > 128
                            || requests.len() >= 100_000
                            || !requests.insert(id.clone())
                        {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "Invalid or reused bounded callback ID",
                            ));
                        }
                        let callback: AudioCallback = serde_json::from_value(
                            json!({"op":message.get("op"),"args":message.get("args")}),
                        )
                        .map_err(|_| {
                            error("AUDIO_PROTOCOL_ERROR", "Invalid typed audio callback")
                        })?;
                        if let AudioCallback::Block(request) = callback {
                            let block = session.compile_export_block_typed(request, resources)?;
                            let bytes = super::compiled_packet::pack_export_block(block)?;
                            reply_encoded(&mut stdin, &execution, &id, &bytes)?;
                            continue;
                        }
                        match session.callback(callback, resources) {
                            Ok(value) => reply(&mut stdin, &execution, &id, value)?,
                            Err(cause) => {
                                packet(
                                    &mut stdin,
                                    &execution,
                                    json!({"type":"reply","id":id,"ok":false,"error":{"code":cause.code,"message":cause.message}}),
                                )?;
                                return Err(cause);
                            }
                        }
                    }
                    "pcm" => {
                        if runtime.is_none() {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "PCM before browser readiness",
                            ));
                        }
                        let channel = number(&message, "channel")?;
                        let offset = decimal(string(&message, "offsetFrames")?)?;
                        let count = number(&message, "frames")?;
                        if channel > 1
                            || count == 0
                            || count > AUDIO_PCM_CHUNK_FRAMES as u64
                            || offset != offsets[channel as usize]
                            || count > frames.saturating_sub(offset)
                        {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "Invalid PCM chunk order or range",
                            ));
                        }
                        let hex = string(&message, "bytesHex")?;
                        if hex.len() != count as usize * 8 {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "PCM byte count differs from admitted samples",
                            ));
                        }
                        let bytes = decode_hex(hex)?;
                        let samples = bytes
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes(b.try_into().expect("four bytes")))
                            .collect::<Vec<_>>();
                        if samples.iter().any(|v| !v.is_finite()) {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "Nonfinite PCM cannot be published",
                            ));
                        }
                        sink.chunk(channel as u32, offset, &samples)?;
                        offsets[channel as usize] += count;
                    }
                    "complete" => {
                        if offsets != [frames; 2] || runtime.is_none() {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "Audio completed with missing PCM",
                            ));
                        }
                        let receipt = message.get("receipt").cloned().ok_or_else(|| {
                            error("AUDIO_PROTOCOL_ERROR", "Missing execution receipt")
                        })?;
                        if receipt.get("identity") != plan.get("identity") {
                            return Err(error(
                                "REVISION_CONFLICT",
                                "Audio receipt differs from the fixed preparation",
                            ));
                        }
                        let runtime = runtime.expect("checked runtime");
                        if receipt.get("engineIdentity") != runtime.pointer("/identity/engine") {
                            return Err(error(
                                "AUDIO_ENGINE_CHANGED",
                                "Audio receipt changed its actual engine identity",
                            ));
                        }
                        let quota = message.get("quota").cloned().ok_or_else(|| {
                            error("AUDIO_PROTOCOL_ERROR", "Missing actual quota receipt")
                        })?;
                        let observed: AudioQuotaReceipt = serde_json::from_value(quota.clone())
                            .map_err(|_| {
                                error("AUDIO_PROTOCOL_ERROR", "Invalid typed quota receipt")
                            })?;
                        let strict = config.strict_cgroup.is_some();
                        if observed.profile
                            != if strict {
                                "strict-hard"
                            } else {
                                "monitored-terminate"
                            }
                            || decimal(&observed.max_rss_bytes)? != config.max_rss_bytes
                            || decimal(&observed.observed_peak_rss_bytes)? > config.max_rss_bytes
                            || observed.hard_memory_enforced != strict
                            || !(1..=250).contains(&observed.monitor_interval_ms)
                            || observed.elapsed_ms > config.deadline.as_millis() as u64
                        {
                            return Err(error(
                                "AUDIO_PROTOCOL_ERROR",
                                "Quota receipt differs from host admission",
                            ));
                        }
                        let receipt: ClassicAudioRenderReceipt = serde_json::from_value(receipt)
                            .map_err(|_| {
                                error("AUDIO_PROTOCOL_ERROR", "Invalid typed rendering receipt")
                            })?;
                        session.validate_receipt(&receipt)?;
                        return Ok(ClassicAudioProcessResult {
                            receipt,
                            quota: observed,
                            runtime_identity: ClassicAudioRuntimeReceipt {
                                browser_pid: process.guard.browser_pid.load(Ordering::Acquire),
                                identity: serde_json::from_value(runtime["identity"].clone())
                                    .map_err(|_| {
                                        error(
                                            "AUDIO_PROTOCOL_ERROR",
                                            "Invalid typed engine identity",
                                        )
                                    })?,
                            },
                        });
                    }
                    "failed" => {
                        return Err(error(
                            message
                                .pointer("/error/code")
                                .and_then(Value::as_str)
                                .unwrap_or("AUDIO_HOST_FAILED"),
                            "Isolated Classic browser execution failed",
                        ));
                    }
                    "cancelled" => {
                        return Err(error(
                            "JOB_CANCELLED",
                            "Isolated audio execution acknowledged cancellation",
                        ));
                    }
                    _ => return Err(error("AUDIO_PROTOCOL_ERROR", "Unknown audio packet type")),
                }
            }
        })();
        if result.is_err() {
            let _ = packet(
                &mut stdin,
                &execution,
                json!({"type":"cancel","reason":"Host policy, cancellation or execution failure"}),
            );
        }
        drop(stdin);
        process.finish(result.is_ok(), &execution)?;
        let receipt = result?;
        resources.checkpoint()?;
        Ok(receipt)
    }
}
impl ClassicAudioExecutor for ClassicAudioProcess {
    fn execute(
        &self,
        session: &mut ClassicAudioSession<'_>,
        resources: &mut dyn ClassicAudioResources,
        handles: &[ClassicAudioResourceHandle],
        sink: &mut dyn ClassicAudioPcmSink,
    ) -> Result<ClassicAudioExecutionReceipt, EditorError> {
        #[cfg(unix)]
        {
            self.render(session, resources, handles, sink)
        }
        #[cfg(not(unix))]
        {
            let _ = (session, resources, handles, sink);
            Err(error(
                "CAPABILITY_UNAVAILABLE",
                "Classic isolated process adapter requires supported process-group termination",
            ))
        }
    }
}
