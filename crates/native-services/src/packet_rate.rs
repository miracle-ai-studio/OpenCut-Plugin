//! Isolated packet observations for an already assembled, fixed upload.
//! The host retains the source handle and the live sealed-import authority.
use crate::native_process::{self, Domain, ProcessConfig, WorkerProcess, wire};
use opencut_contracts::EditorError;
use opencut_media::ClassicPacketRate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};
pub(crate) const DEADLINE: Duration = Duration::from_secs(30);
const MAX_RSS: u64 = 256 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    pub size: u64,
}
impl Plan {
    pub fn validate(&self) -> Result<(), EditorError> {
        if self.size == 0 || self.size > (1u64 << 53) - 1 {
            return Err(protocol("Invalid packet-probe immutable length"));
        }
        Ok(())
    }
}
pub(crate) fn protocol(message: impl Into<String>) -> EditorError {
    EditorError::new("MEDIA_INVALID_TOOL_OUTPUT", message)
}
pub(crate) fn identity() -> Result<Value, EditorError> {
    let exe = std::env::current_exe().map_err(|_| protocol("Cannot identify native probe"))?;
    Ok(
        json!({"worker_sha256":native_process::file_hash(&exe)?,"libraries":native_process::library_identities()?}),
    )
}
pub fn probe(
    worker_path: &Path,
    input_path: &Path,
    size: u64,
    checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
) -> Result<ClassicPacketRate, EditorError> {
    checkpoint()?;
    let plan = Plan { size };
    plan.validate()?;
    if !input_path.is_absolute() {
        return Err(protocol("Packet probe requires a fixed private input"));
    }
    let mut input =
        File::open(input_path).map_err(|_| protocol("Packet probe input unavailable"))?;
    if !input
        .metadata()
        .is_ok_and(|m| m.is_file() && m.len() == size)
    {
        return Err(protocol("Packet probe source extent changed"));
    }
    let libraries = native_process::library_identities()?;
    checkpoint()?;
    // The child is declared after the handle and is always killed/reaped before
    // the sealed input can be dropped, including cancellation and bad replies.
    let mut worker = WorkerProcess::start(ProcessConfig {
        worker: worker_path.to_owned(),
        max_rss_bytes: MAX_RSS,
        lifetime_deadline: DEADLINE,
        operation_deadline: DEADLINE,
        progress: None,
        strict_cgroup: None,
        domain: Domain::Probe,
    })?;
    let expected = json!({"worker_sha256":worker.expected_hash,"libraries":libraries});
    worker.send(
        json!({"type":"initialize","purpose":"packet_rate","plan":plan}),
        &[],
    )?;
    let mut ready = false;
    let mut last = 0;
    let mut requests = 0usize;
    loop {
        checkpoint()?;
        let Some(packet) = worker.receive()? else {
            continue;
        };
        checkpoint()?;
        if !packet.data.is_empty() {
            return Err(protocol("Unexpected packet-probe payload"));
        }
        match wire::text(&packet.meta, "type")? {
            "ready" => {
                if ready
                    || packet.meta["purpose"] != "packet_rate"
                    || wire::integer(&packet.meta, "pid")? != u64::from(worker.pid())
                    || packet.meta["identity"] != expected
                {
                    return Err(protocol("Native packet-probe readiness changed"));
                }
                ready = true;
            }
            "source_read" => {
                if !ready {
                    return Err(protocol("Packet probe read before readiness"));
                }
                let id = wire::integer(&packet.meta, "id")?;
                let offset = wire::integer(&packet.meta, "offset")?;
                let count = wire::integer(&packet.meta, "count")?;
                requests += 1;
                if id != last + 1
                    || requests > 1_000_000
                    || !(1..=65536).contains(&count)
                    || offset > size
                {
                    return Err(protocol("Invalid packet-probe byte request"));
                }
                last = id;
                let n = count.min(size - offset) as usize;
                let mut bytes = vec![0; n];
                input
                    .seek(SeekFrom::Start(offset))
                    .and_then(|_| input.read_exact(&mut bytes))
                    .map_err(|_| protocol("Fixed packet-probe source ended early"))?;
                checkpoint()?;
                worker.send(json!({"type":"source_reply","id":id,"offset":offset,"eof":offset+n as u64==size}),&bytes)?;
            }
            "packet_complete" => {
                if !ready || packet.meta["identity"] != expected {
                    return Err(protocol("Native packet-probe completion identity changed"));
                }
                let observed: ClassicPacketRate =
                    serde_json::from_value(packet.meta["observation"].clone())
                        .map_err(|_| protocol("Invalid native packet observation"))?;
                observed.validate().map_err(protocol)?;
                checkpoint()?;
                worker.unchanged()?;
                worker.complete()?;
                checkpoint()?;
                return Ok(observed);
            }
            "failed" => {
                return Err(protocol("Isolated packet-rate probing failed"));
            }
            _ => return Err(protocol("Unexpected native packet-probe message")),
        }
    }
}
