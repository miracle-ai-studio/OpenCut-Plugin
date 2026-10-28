use super::protocol::{Plan, Receipt};
use super::*;
use crate::{
    browser_resources::VerifiedBrowserResources,
    native_process::{Domain, ProcessConfig, WorkerProcess, wire},
};
use opencut_composition::scene::CasResource;
use opencut_contracts::AssetVersion;
use serde_json::{Value, json};
fn decimal(value: &str) -> Result<u64, EditorError> {
    let number = value
        .parse::<u64>()
        .map_err(|_| protocol("Invalid source-art sample counter"))?;
    if number.to_string() != value {
        return Err(protocol("Noncanonical source-art sample counter"));
    }
    Ok(number)
}
fn failure(packet: &wire::Packet) -> Option<EditorError> {
    if packet.meta["type"] != "failed" {
        return None;
    }
    let code = packet.meta["error"]["code"]
        .as_str()
        .filter(|c| {
            !c.is_empty()
                && c.len() <= 64
                && c.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or("SOURCE_ART_DECODE_FAILED");
    Some(Domain::SourceArt.protocol(error(code, "Isolated native source decoding failed")))
}
fn decoded(
    receipt: Receipt,
    bytes: Vec<u8>,
    version: AssetVersion,
) -> Result<DecodedSourceArt, EditorError> {
    Ok(match receipt {
        Receipt::Waveform {
            identity,
            sample_rate,
            channels,
            total_samples,
            bucket_size,
            output_bytes,
        } => {
            if output_bytes != bytes.len() || !bytes.len().is_multiple_of(4) {
                return Err(protocol("Source peaks completion size differs"));
            }
            DecodedSourceArt::Waveform {
                version,
                identity,
                sample_rate,
                channels,
                total_samples: decimal(&total_samples)?,
                bucket_size,
                amplitudes: bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect(),
            }
        }
        Receipt::ThumbnailReady {
            identity,
            width,
            height,
            observation,
            output_bytes,
        } => {
            if output_bytes != bytes.len() {
                return Err(protocol("JPEG completion size differs"));
            }
            DecodedSourceArt::ThumbnailReady {
                version,
                identity,
                width,
                height,
                jpeg_bytes: bytes,
                observation,
            }
        }
        Receipt::ThumbnailNoFrame {
            identity,
            observation,
            reason,
            output_bytes,
        } => {
            if output_bytes != 0 || !bytes.is_empty() {
                return Err(protocol("No-frame result contains bytes"));
            }
            DecodedSourceArt::ThumbnailNoFrame {
                version,
                identity,
                observation,
                reason,
            }
        }
    })
}

#[cfg(unix)]
pub(super) fn execute(
    executor: &SourceArtProcessExecutor,
    prepared: &PreparedSourceArt,
    checkpoint: &mut dyn SourceArtCheckpoint,
) -> Result<DecodedSourceArt, EditorError> {
    let config = &executor.config;
    let policy = prepared.policy();
    if config.deadline != Duration::from_millis(policy.deadline_ms)
        || config.max_encoded_bytes != policy.max_encoded_bytes
        || config.max_pcm_bytes != policy.max_pcm_bytes
        || config.max_rss_bytes != policy.max_rss_bytes
    {
        return Err(protocol("Source-art captured admission differs"));
    }
    source_art_decode_key(prepared, &config.identity)?;
    let plan = Plan::from_prepared(prepared, config.identity.clone())?;
    plan.validate()?;
    let asset = prepared.asset();
    let resource = CasResource {
        asset_id: asset.id.clone(),
        version: asset.version.clone(),
        kind: asset.kind.clone(),
        duration: asset.duration.clone(),
        metadata: asset.metadata.clone(),
        license: asset.license.clone(),
    };
    let mut resources = VerifiedBrowserResources::new(
        &executor.asset_root,
        [&resource],
        || checkpoint.checkpoint(),
        "SOURCE_ART_PROTOCOL_ERROR",
    )?;
    let handle = resources
        .handles()
        .find(|(id, _)| *id == asset.id)
        .map(|(_, handle)| handle.to_owned())
        .ok_or_else(|| protocol("Missing source-art authority handle"))?;
    let (transfer, size) = resources.resolve(
        &asset.id,
        &asset.version.content_hash,
        asset.version.size,
        &handle,
    )?;
    if size != asset.version.size {
        return Err(protocol("Source-art descriptor extent changed"));
    }
    // Declared after the CAS owner: all error paths kill and reap the worker
    // before the descriptor/collection gate can be released.
    let mut worker = WorkerProcess::start(ProcessConfig {
        worker: config.worker.clone(),
        max_rss_bytes: policy.max_rss_bytes,
        lifetime_deadline: config.deadline,
        operation_deadline: config.deadline,
        progress: None,
        strict_cgroup: config.strict_cgroup.clone(),
        domain: Domain::SourceArt,
    })?;
    let SourceArtIdentity::Native(expected) = &config.identity else {
        return Err(protocol("Native source identity required"));
    };
    if worker.expected_hash != expected.worker_sha256 {
        return Err(error(
            "SOURCE_ART_ENGINE_CHANGED",
            "Configured source worker changed",
        ));
    }
    worker.send(
        json!({"type":"initialize","purpose":"source_art","plan":plan}),
        &[],
    )?;
    let receive = |worker: &WorkerProcess,
                   resources: &mut VerifiedBrowserResources<_>|
     -> Result<wire::Packet, EditorError> {
        loop {
            resources.checkpoint()?;
            if let Some(packet) = worker.receive()? {
                resources.checkpoint()?;
                return Ok(packet);
            }
        }
    };
    let ready = receive(&worker, &mut resources)?;
    if let Some(error) = failure(&ready) {
        return Err(error);
    }
    if ready.meta["type"] != "ready"
        || ready.meta["purpose"] != "source_art"
        || wire::integer(&ready.meta, "pid")? != u64::from(worker.pid())
        || !ready.data.is_empty()
    {
        return Err(protocol("Invalid native source-art readiness"));
    }
    let observed: SourceArtIdentity = serde_json::from_value(ready.meta["identity"].clone())
        .map_err(|_| protocol("Invalid native source identity"))?;
    if observed != config.identity {
        return Err(error(
            "SOURCE_ART_ENGINE_CHANGED",
            "Actual source worker differs from captured dependencies",
        ));
    }
    let mut sequence = 0u64;
    let mut bytes = Vec::new();
    let output = loop {
        let packet = receive(&worker, &mut resources)?;
        match wire::text(&packet.meta, "type")? {
            "source_read" | "source_output" => {
                sequence = sequence
                    .checked_add(1)
                    .ok_or_else(|| protocol("Source callback counter overflow"))?;
                if wire::integer(&packet.meta, "id")? != sequence || sequence > (1u64 << 53) - 1 {
                    return Err(protocol("Native source callback order changed"));
                }
                if packet.meta["type"] == "source_read" {
                    if !packet.data.is_empty() {
                        return Err(protocol("Unexpected source-read request payload"));
                    }
                    let offset = wire::integer(&packet.meta, "offset")?;
                    let count = u32::try_from(wire::integer(&packet.meta, "count")?)
                        .map_err(|_| protocol("Invalid source read length"))?;
                    if count == 0 || count > 65536 || offset > size {
                        return Err(protocol("Native source read exceeds its fixed descriptor"));
                    }
                    let (data, eof) = resources.read(&transfer, offset, count)?;
                    worker.send(
                        json!({"type":"source_reply","id":sequence,"offset":offset,"eof":eof}),
                        &data,
                    )?;
                } else {
                    let offset = wire::integer(&packet.meta, "offset")?;
                    if offset != bytes.len() as u64
                        || packet.data.is_empty()
                        || packet.data.len() > 65536
                        || offset
                            .checked_add(packet.data.len() as u64)
                            .is_none_or(|n| n > plan.output_limit())
                    {
                        return Err(error(
                            "SOURCE_ART_BUDGET_EXCEEDED",
                            "Native source-art output exceeds ordered admission",
                        ));
                    }
                    bytes.try_reserve_exact(packet.data.len()).map_err(|_| {
                        error(
                            "SOURCE_ART_BUDGET_EXCEEDED",
                            "Cannot retain admitted source-art output",
                        )
                    })?;
                    bytes.extend_from_slice(&packet.data);
                    resources.checkpoint()?;
                    worker.send(json!({"type":"source_reply","id":sequence}), &[])?;
                }
            }
            "source_complete" => {
                if !packet.data.is_empty() {
                    return Err(protocol("Unexpected completion payload"));
                }
                let receipt: Receipt = serde_json::from_value(packet.meta["receipt"].clone())
                    .map_err(|_| protocol("Invalid native source receipt"))?;
                let output = decoded(receipt, bytes, asset.version.clone())?;
                if output.identity() != &config.identity {
                    return Err(protocol("Native source receipt changed identity"));
                }
                validate_decoded_source_art(prepared, &output)?;
                resources.checkpoint()?;
                worker.send(json!({"type":"finish"}), &[])?;
                break output;
            }
            "failed" => return Err(failure(&packet).expect("matched native failure")),
            _ => return Err(protocol("Unexpected native source-art packet")),
        }
    };
    let quota = worker.complete()?;
    if quota
        .observed_peak_rss_bytes
        .parse::<u64>()
        .ok()
        .is_none_or(|n| n > policy.max_rss_bytes)
        || quota.elapsed_ms > policy.deadline_ms
        || quota.hard_memory_enforced != config.strict_cgroup.is_some()
    {
        return Err(protocol(
            "Native source-art quota differs from captured policy",
        ));
    }
    resources.checkpoint()?;
    resources.release(&transfer)?;
    Ok(output)
}
#[cfg(test)]
pub(super) fn decode_for_test(
    value: Value,
    bytes: Vec<u8>,
) -> Result<DecodedSourceArt, EditorError> {
    decoded(
        serde_json::from_value(value).map_err(|_| protocol("Invalid receipt"))?,
        bytes,
        AssetVersion {
            id: format!("av_{}", "d".repeat(64)),
            content_hash: "d".repeat(64),
            size: 1,
        },
    )
}
