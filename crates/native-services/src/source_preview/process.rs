use super::{
    protocol::{Frame, Plan, Track},
    *,
};
use crate::{
    browser_resources::VerifiedBrowserResources,
    classic_video::{ClassicVideoEncoder, ClassicVideoEncoderConfig},
    native_process::{Domain, ProcessConfig, WorkerProcess, wire},
};
use opencut_composition::scene::CasResource;
use opencut_editor_application::{
    classic_visual::ClassicVisualRgbaSink,
    media_video_jobs::VideoExecutionLimit,
    source_preview::{ArtifactDescriptor, validate_prepared},
};
use serde_json::json;
use std::{
    cell::RefCell,
    fs::File,
    io::Read,
    rc::Rc,
    time::{Duration, Instant},
};

fn artifact(
    path: &Path,
    mime: &str,
    check: &mut dyn FnMut(bool) -> Result<(), EditorError>,
) -> Result<ArtifactDescriptor, EditorError> {
    let mut file = File::open(path).map_err(|_| protocol("Cannot open prepared source output"))?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut bytes = [0; 65536];
    loop {
        check(false)?;
        let count = file
            .read(&mut bytes)
            .map_err(|_| protocol("Cannot hash prepared source output"))?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
        size += count as u64;
        check(true)?;
    }
    Ok(ArtifactDescriptor {
        path: path
            .to_str()
            .ok_or_else(|| protocol("Invalid source output path"))?
            .into(),
        mime_type: mime.into(),
        size,
        content_hash: format!("{:x}", hash.finalize()),
    })
}
fn public(value: &ArtifactDescriptor) -> SourcePreviewArtifact {
    SourcePreviewArtifact {
        content_hash: value.content_hash.clone(),
        size_bytes: value.size.to_string(),
        mime_type: value.mime_type.clone(),
    }
}

#[cfg(unix)]
pub(super) fn execute(
    executor: &SourcePreviewProcessExecutor<'_>,
    prepared: &PreparedSourcePreview,
    checkpoint: &mut dyn SourcePreviewCheckpoint,
) -> Result<StoredSourcePreviewResult, EditorError> {
    validate_prepared(prepared)?;
    let config = &executor.config;
    let policy = &prepared.processing_policy;
    let started = Instant::now();
    let deadline = Duration::from_millis(policy.deadline_ms);
    let checkpoint = Rc::new(RefCell::new(checkpoint));
    let mut check = |completed| {
        if started.elapsed() >= deadline {
            return Err(error(
                "SOURCE_PREVIEW_TIME_LIMIT",
                "Source preparation exceeded captured deadline",
            ));
        }
        checkpoint.borrow_mut().checkpoint(completed)
    };
    check(false)?;
    if resolve_runtime(&config.worker, &config.ffmpeg, &config.ffprobe)? != config.runtime
        || runtime_fingerprint(&config.runtime)? != prepared.identity.runtime_fingerprint
    {
        return Err(error(
            "SOURCE_PREVIEW_ENGINE_CHANGED",
            "Captured source preparation runtime changed",
        ));
    }
    check(false)?;
    let plan = Plan {
        identity: prepared.identity.clone(),
        decoder: config.runtime.decoder.clone(),
        policy: policy.clone(),
    };
    plan.validate()?;
    let asset = prepared
        .snapshot
        .project
        .assets
        .iter()
        .find(|asset| asset.id == prepared.identity.asset_id)
        .ok_or_else(|| protocol("Captured source asset is missing"))?;
    let resource = CasResource {
        asset_id: asset.id.clone(),
        version: asset.version.clone(),
        kind: asset.kind.clone(),
        duration: asset.duration.clone(),
        metadata: asset.metadata.clone(),
        license: asset.license.clone(),
    };
    let gate = checkpoint.clone();
    let mut resources = VerifiedBrowserResources::new(
        &config.asset_root,
        [&resource],
        move || gate.borrow_mut().checkpoint(false),
        "SOURCE_PREVIEW_PROTOCOL_ERROR",
    )?;
    let handle = resources
        .handles()
        .find(|(id, _)| *id == asset.id)
        .map(|(_, handle)| handle.to_owned())
        .ok_or_else(|| protocol("Missing original source handle"))?;
    let (transfer, size) = resources.resolve(
        &asset.id,
        &asset.version.content_hash,
        asset.version.size,
        &handle,
    )?;
    if size != asset.version.size {
        return Err(protocol("Source descriptor extent changed"));
    }
    // Children drop before the descriptor/collection gate on every failure.
    let mut worker = WorkerProcess::start(ProcessConfig {
        worker: config.worker.clone(),
        max_rss_bytes: policy.max_source_rss_bytes,
        lifetime_deadline: deadline,
        operation_deadline: deadline,
        progress: None,
        strict_cgroup: config.strict_cgroup.clone(),
        domain: Domain::SourcePreview,
    })?;
    if worker.expected_hash != config.runtime.decoder.worker_sha256 {
        return Err(error(
            "SOURCE_PREVIEW_ENGINE_CHANGED",
            "Source worker changed before execution",
        ));
    }
    worker.send(
        json!({"type":"initialize","purpose":"source_preview","plan":plan}),
        &[],
    )?;
    let receive = |worker: &WorkerProcess| -> Result<wire::Packet, EditorError> {
        loop {
            if started.elapsed() >= deadline {
                return Err(error(
                    "SOURCE_PREVIEW_TIME_LIMIT",
                    "Source preparation exceeded captured deadline",
                ));
            }
            checkpoint.borrow_mut().checkpoint(false)?;
            if let Some(packet) = worker.receive()? {
                if packet.meta["type"] == "failed" {
                    let code = packet.meta["error"]["code"]
                        .as_str()
                        .filter(|code| {
                            !code.is_empty()
                                && code.len() <= 64
                                && code.bytes().all(|b| {
                                    b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
                                })
                        })
                        .unwrap_or("SOURCE_PREVIEW_DECODE_FAILED");
                    return Err(error(code, "Isolated source preparation failed"));
                }
                return Ok(packet);
            }
        }
    };
    let ready = receive(&worker)?;
    let observed: SourcePreviewDecoderIdentity =
        serde_json::from_value(ready.meta["identity"].clone())
            .map_err(|_| protocol("Invalid source decoder identity"))?;
    if ready.meta["type"] != "ready"
        || ready.meta["purpose"] != "source_preview"
        || wire::integer(&ready.meta, "pid")? != u64::from(worker.pid())
        || !ready.data.is_empty()
        || observed != config.runtime.decoder
    {
        return Err(protocol(
            "Source decoder readiness differs from captured runtime",
        ));
    }
    let raster = prepared.identity.raster_bound;
    let encoded_gate = checkpoint.clone();
    let mut encoder = ClassicVideoEncoder::start_source_preview(
        ClassicVideoEncoderConfig {
            ffmpeg: config.ffmpeg.clone(),
            ffprobe: config.ffprobe.clone(),
            output: config.video_output.clone(),
            audio: config.video_output.clone(),
            width: raster.width,
            height: raster.height,
            fps_numerator: 30,
            fps_denominator: 1,
            frame_count: policy.max_frames,
            max_output_bytes: policy.max_output_bytes,
            max_rss_bytes: policy.max_encoder_rss_bytes,
            execution_limit: VideoExecutionLimit::Total {
                milliseconds: policy.deadline_ms,
            },
        },
        executor.admission,
        move |completed| encoded_gate.borrow_mut().checkpoint(completed),
    )?;
    let mut index = SourcePreviewIndexBuilder::new(policy.max_frames, raster)?;
    let mut track = None;
    let mut base = None;
    let mut first = None;
    let mut last = None;
    let mut frame_open = false;
    let mut offset = 0u64;
    let mut completed = 0u64;
    let mut sequence = 0u64;
    loop {
        let packet = receive(&worker)?;
        let kind = wire::text(&packet.meta, "type")?;
        if kind == "source_complete" {
            if !packet.data.is_empty()
                || frame_open
                || completed == 0
                || wire::integer(&packet.meta, "frame_count")? != completed
            {
                return Err(protocol("Source iteration completed with missing frames"));
            }
            worker.send(json!({"type":"finish"}), &[])?;
            break;
        }
        sequence = sequence
            .checked_add(1)
            .filter(|n| *n <= (1u64 << 53) - 1)
            .ok_or_else(|| protocol("Source callback counter overflow"))?;
        if wire::integer(&packet.meta, "id")? != sequence {
            return Err(protocol("Source preparation callback order changed"));
        }
        match kind {
            "source_read" => {
                let at = wire::integer(&packet.meta, "offset")?;
                let count = u32::try_from(wire::integer(&packet.meta, "count")?)
                    .map_err(|_| protocol("Invalid source read length"))?;
                if !packet.data.is_empty() || count == 0 || count > 65536 || at > size {
                    return Err(protocol("Source read exceeds captured descriptor"));
                }
                let (bytes, eof) = resources.read(&transfer, at, count)?;
                worker.send(
                    json!({"type":"source_reply","id":sequence,"offset":at,"eof":eof}),
                    &bytes,
                )?;
                continue;
            }
            "source_track" => {
                let value: Track = serde_json::from_value(packet.meta["track"].clone())
                    .map_err(|_| protocol("Invalid actual source track"))?;
                if !packet.data.is_empty()
                    || track.is_some()
                    || completed != 0
                    || frame_open
                    || value.stream_index > 63
                    || prepared
                        .identity
                        .selected_video_stream
                        .is_some_and(|n| n != value.stream_index)
                {
                    return Err(protocol("Source decoder changed selected stream"));
                }
                track = Some(value);
            }
            "source_frame" => {
                let frame: Frame = serde_json::from_value(packet.meta["frame"].clone())
                    .map_err(|_| protocol("Invalid actual source frame"))?;
                if !packet.data.is_empty()
                    || track.is_none()
                    || frame_open
                    || frame.ordinal != completed
                    || base.is_some_and(|base| base != frame.time_base)
                    || frame.time_base.numerator == 0
                    || frame.time_base.denominator == 0
                {
                    return Err(protocol(
                        "Source frame changed original sequence or time base",
                    ));
                }
                index.source_frame(frame.pts, frame.duration, frame.video)?;
                base = Some(frame.time_base);
                first.get_or_insert(frame.pts);
                last = Some((frame.pts, frame.duration));
                frame_open = true;
                offset = 0;
            }
            "source_rgba" => {
                if !frame_open
                    || packet.data.is_empty()
                    || packet.data.len() > 65536
                    || wire::integer(&packet.meta, "ordinal")? != completed
                    || wire::integer(&packet.meta, "offset")? != offset
                    || offset
                        .checked_add(packet.data.len() as u64)
                        .is_none_or(|n| n > u64::from(raster.width) * u64::from(raster.height) * 4)
                {
                    return Err(protocol(
                        "Source RGBA changed its admitted frame extent/order",
                    ));
                }
                encoder.chunk(offset, &packet.data)?;
                offset += packet.data.len() as u64;
            }
            "source_frame_complete" => {
                if !packet.data.is_empty()
                    || !frame_open
                    || wire::integer(&packet.meta, "ordinal")? != completed
                    || offset != u64::from(raster.width) * u64::from(raster.height) * 4
                {
                    return Err(protocol("Source frame lacks complete RGBA"));
                }
                encoder.finish_frame()?;
                completed += 1;
                frame_open = false;
            }
            _ => return Err(protocol("Unexpected source preparation packet")),
        }
        check(true)?;
        worker.send(json!({"type":"source_reply","id":sequence}), &[])?;
    }
    let quota = worker.complete()?;
    if quota
        .observed_peak_rss_bytes
        .parse::<u64>()
        .ok()
        .is_none_or(|n| n > policy.max_source_rss_bytes)
        || quota.elapsed_ms > policy.deadline_ms
        || quota.hard_memory_enforced != config.strict_cgroup.is_some()
    {
        return Err(protocol(
            "Actual source decoder quota differs from captured admission",
        ));
    }
    resources.release(&transfer)?;
    let observation = encoder.finish_source_preview(
        completed,
        executor.admission,
        &mut |ordinal, pts, duration| index.proxy_frame(ordinal, pts, duration),
    )?;
    if observation.ffmpeg_sha256 != config.runtime.ffmpeg_sha256
        || observation.ffprobe_sha256 != config.runtime.ffprobe_sha256
    {
        return Err(error(
            "SOURCE_PREVIEW_ENGINE_CHANGED",
            "Source encoder/probe dependencies changed",
        ));
    }
    check(false)?;
    let bytes = index.finish()?;
    executor
        .admission
        .write(&config.index_output, &bytes)
        .map_err(crate::media::execution_error)?;
    check(true)?;
    let video = artifact(&config.video_output, "video/x-matroska", &mut check)?;
    let frame_index = artifact(
        &config.index_output,
        "application/vnd.opencut.source-preview-index",
        &mut check,
    )?;
    if resolve_runtime(&config.worker, &config.ffmpeg, &config.ffprobe)? != config.runtime {
        return Err(error(
            "SOURCE_PREVIEW_ENGINE_CHANGED",
            "Source runtime changed before completion",
        ));
    }
    check(false)?;
    let track = track.ok_or_else(|| protocol("Source completion omitted selected stream"))?;
    let (last_pts, last_duration) =
        last.ok_or_else(|| protocol("Source completion omitted original PTS"))?;
    Ok(StoredSourcePreviewResult {
        public: SourcePreviewResult {
            identity: prepared.identity.clone(),
            producer_revision: prepared.snapshot.revision.clone(),
            actual_stream_index: track.stream_index,
            actual_track_id: track.track_id,
            source_time_base: base
                .ok_or_else(|| protocol("Source completion omitted original time base"))?,
            proxy_time_base: observation.time_base,
            encoding: SourcePreviewEncoding::Ffv1Rgba,
            frame_count: completed.to_string(),
            first_source_pts: first
                .ok_or_else(|| protocol("Source completion omitted first PTS"))?
                .to_string(),
            last_source_pts: last_pts.to_string(),
            last_source_duration: last_duration.to_string(),
            video: public(&video),
            frame_index: public(&frame_index),
            runtime: config.runtime.clone(),
        },
        artifact: video,
        frame_index,
    })
}
