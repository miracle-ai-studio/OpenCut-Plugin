//! Native source-art execution over a single authorized binary byte port.
//! The application owns projections and target-size policy; this worker never
//! reads a native path or an authored project document and never creates a GPU.
use super::protocol::{Plan, Query, Receipt};
use super::*;
use crate::{native_audio::dsp::PcmMemory, native_process::wire};
use opencut_media::{MediaError, MediaTime, native_decode::*};
use opencut_renderer::raster::{SourceThumbnailImage, SourceThumbnailLimits};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    io::{Read, Write},
    rc::Rc,
    time::Instant,
};
type Result<T> = std::result::Result<T, EditorError>;
struct Channel<'a> {
    input: &'a mut dyn Read,
    output: &'a mut dyn Write,
    deadline: Instant,
    sequence: u64,
    output_bytes: u64,
    output_limit: u64,
}
impl Channel<'_> {
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(error(
                "SOURCE_ART_TIME_LIMIT",
                "Native source operation exceeded its deadline",
            ));
        }
        Ok(())
    }
    fn request(&mut self, mut meta: Value, data: &[u8]) -> Result<wire::Packet> {
        self.check()?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .filter(|n| *n <= (1u64 << 53) - 1)
            .ok_or_else(|| protocol("Source callback counter overflow"))?;
        meta["id"] = self.sequence.into();
        wire::send(self.output, meta, data)?;
        let packet = wire::read(self.input)?;
        if packet.meta["type"] != "source_reply"
            || wire::integer(&packet.meta, "id")? != self.sequence
        {
            return Err(protocol("Native source reply changed request identity"));
        }
        self.check()?;
        Ok(packet)
    }
    fn emit(&mut self, bytes: &[u8]) -> Result<()> {
        for bytes in bytes.chunks(65536) {
            if self
                .output_bytes
                .checked_add(bytes.len() as u64)
                .is_none_or(|n| n > self.output_limit)
            {
                return Err(error(
                    "SOURCE_ART_BUDGET_EXCEEDED",
                    "Native source-art output exceeds admission",
                ));
            }
            let packet = self.request(
                json!({"type":"source_output","offset":self.output_bytes}),
                bytes,
            )?;
            if !packet.data.is_empty() {
                return Err(protocol("Unexpected source output acknowledgement data"));
            }
            self.output_bytes += bytes.len() as u64;
        }
        Ok(())
    }
}
struct Source<'a> {
    channel: Rc<RefCell<Channel<'a>>>,
    size: u64,
}
fn admission(e: EditorError) -> MediaError {
    MediaError::Admission {
        code: e.code,
        message: e.message,
    }
}
impl ByteSource for Source<'_> {
    fn len(&self) -> u64 {
        self.size
    }
    fn checkpoint(&mut self) -> std::result::Result<(), MediaError> {
        self.channel.borrow().check().map_err(admission)
    }
    fn read_at(
        &mut self,
        offset: u64,
        output: &mut [u8],
    ) -> std::result::Result<usize, MediaError> {
        self.checkpoint()?;
        if output.is_empty() {
            return Ok(0);
        }
        if offset > self.size {
            return Err(admission(protocol(
                "Source range exceeds immutable descriptor",
            )));
        }
        let count = output.len().min(65536);
        let packet = self
            .channel
            .borrow_mut()
            .request(
                json!({"type":"source_read","offset":offset,"count":count}),
                &[],
            )
            .map_err(admission)?;
        let eof = packet.meta["eof"]
            .as_bool()
            .ok_or_else(|| admission(protocol("Missing native source EOF fact")))?;
        if wire::integer(&packet.meta, "offset").map_err(admission)? != offset
            || packet.data.len() > count
            || offset
                .checked_add(packet.data.len() as u64)
                .is_none_or(|n| n > self.size)
            || eof != (offset + packet.data.len() as u64 == self.size)
            || (packet.data.is_empty() && !eof)
        {
            return Err(admission(protocol(
                "Native source bytes changed their ordered range",
            )));
        }
        output[..packet.data.len()].copy_from_slice(&packet.data);
        self.checkpoint()?;
        Ok(packet.data.len())
    }
}
fn renderer_error(e: opencut_renderer::Error) -> EditorError {
    if e.code == "VISUAL_BUDGET_EXCEEDED" {
        error("SOURCE_ART_BUDGET_EXCEEDED", &e.message)
    } else if e.code == "INVALID_VISUAL_PLAN" {
        error("SOURCE_ART_DECODE_FAILED", &e.message)
    } else {
        error(&e.code, &e.message)
    }
}
fn raster_check(
    channel: &Rc<RefCell<Channel<'_>>>,
) -> std::result::Result<(), opencut_renderer::Error> {
    channel
        .borrow()
        .check()
        .map_err(|e| opencut_renderer::Error::new(e.code, e.message))
}
pub(crate) fn serve(
    packet: wire::Packet,
    input: &mut dyn Read,
    output: &mut dyn Write,
) -> Result<()> {
    let plan: Plan = serde_json::from_value(packet.meta["plan"].clone())
        .map_err(|_| protocol("Invalid typed native source plan"))?;
    plan.validate()?;
    let deadline = Instant::now() + Duration::from_millis(plan.policy.deadline_ms);
    let mut actual = resolve_identity(
        &std::env::current_exe().map_err(|_| protocol("Cannot observe native source worker"))?,
    )?;
    actual.set_algorithm(plan.identity.algorithm());
    if actual != plan.identity {
        return Err(error(
            "SOURCE_ART_ENGINE_CHANGED",
            "Native source worker differs from configured dependencies",
        ));
    }
    wire::send(
        output,
        json!({"type":"ready","purpose":"source_art","pid":std::process::id(),"identity":actual}),
        &[],
    )?;
    let channel = Rc::new(RefCell::new(Channel {
        input,
        output,
        deadline,
        sequence: 0,
        output_bytes: 0,
        output_limit: plan.output_limit(),
    }));
    let mut source = Source {
        channel: channel.clone(),
        size: plan.version.size,
    };
    let receipt = match plan.query {
        Query::Waveform {
            sample_rate,
            working_bytes,
            output_bytes,
        } => waveform(
            &plan,
            &mut source,
            sample_rate,
            working_bytes,
            output_bytes,
            deadline,
        )?,
        Query::Thumbnail { ref time } => thumbnail(&plan, &mut source, time.as_ref(), deadline)?,
    };
    channel.borrow().check()?;
    let mut current = resolve_identity(
        &std::env::current_exe().map_err(|_| protocol("Cannot observe native source worker"))?,
    )?;
    current.set_algorithm(actual.algorithm());
    if current != actual {
        return Err(error(
            "SOURCE_ART_ENGINE_CHANGED",
            "Native source dependencies changed during execution",
        ));
    }
    wire::send(
        channel.borrow_mut().output,
        json!({"type":"source_complete","receipt":receipt}),
        &[],
    )?;
    let finish = wire::read(channel.borrow_mut().input)?;
    if finish.meta["type"] != "finish" || !finish.data.is_empty() {
        return Err(protocol("Native source completion was not acknowledged"));
    }
    channel.borrow().check()
}
fn waveform(
    plan: &Plan,
    source: &mut Source<'_>,
    target: u32,
    working: u64,
    max_output: u64,
    deadline: Instant,
) -> Result<Receipt> {
    let memory =
        PcmMemory::new(working).map_err(|e| error("SOURCE_ART_BUDGET_EXCEEDED", &e.message))?;
    let codec_bytes = (working / 8).min(16 * 1024 * 1024);
    let _codec = memory
        .reserve(codec_bytes)
        .map_err(|e| error("SOURCE_ART_BUDGET_EXCEEDED", &e.message))?;
    let mut decoder = NativeAudioDecoder::open(
        source,
        None,
        AudioDecodeLimits {
            max_channels: 64,
            max_frame_samples: 65536,
            max_resident_bytes: codec_bytes,
            max_io_bytes_per_operation: plan.policy.max_encoded_bytes.saturating_mul(8).max(65536),
            max_packets_per_operation: 100000,
        },
        deadline,
    )
    .map_err(crate::media::execution_error)?;
    let track = decoder.track().clone();
    if !(1..=64).contains(&track.channels) || !(1..=384000).contains(&track.sample_rate) {
        return Err(protocol("Native waveform primary format is unobserved"));
    }
    let chunk = (working / (64 * u64::from(track.channels) * 4)).min(65536) as usize;
    if chunk == 0 {
        return Err(error(
            "SOURCE_ART_BUDGET_EXCEEDED",
            "No native waveform decoder chunk fits",
        ));
    }
    decoder
        .select_keyframe_before_seconds(source, 0.0, deadline)
        .map_err(crate::media::execution_error)?;
    let mut stream = streaming::WaveformStream::new(
        track.sample_rate,
        target,
        track.channels,
        chunk,
        chunk as u64 * 16,
        max_output,
        memory.clone(),
    )?;
    let checks = source.channel.clone();
    let emits = source.channel.clone();
    let mut check = || checks.borrow().check();
    let mut emit = |peaks: &[f32]| {
        let packed: Vec<u8> = peaks.iter().flat_map(|p| p.to_le_bytes()).collect();
        emits.borrow_mut().emit(&packed)
    };
    while let Some(frame) = decoder
        .read(source, deadline)
        .map_err(crate::media::execution_error)?
    {
        if frame.sample_rate != track.sample_rate || frame.channels.len() != track.channels as usize
        {
            return Err(protocol("Native waveform primary format changed"));
        }
        // A codec frame may exceed the per-channel retained-stream chunk.
        // Borrow slices rather than duplicating that decoded frame or rejecting
        // a valid high-channel source. Decoder memory remains reserved above.
        let mut planes: [&[f32]; 64] = [&[]; 64];
        for start in (0..frame.channels[0].len()).step_by(chunk) {
            let end = (start + chunk).min(frame.channels[0].len());
            for (plane, channel) in planes.iter_mut().zip(&frame.channels) {
                *plane = &channel[start..end];
            }
            stream.push(&planes[..track.channels as usize], &mut check, &mut emit)?;
        }
    }
    drop(decoder);
    let (_, total) = stream.finish(&mut check, &mut emit)?;
    let bytes = source.channel.borrow().output_bytes as usize;
    Ok(Receipt::Waveform {
        identity: plan.identity.clone(),
        sample_rate: target,
        channels: track.channels,
        total_samples: total.to_string(),
        bucket_size: 128,
        output_bytes: bytes,
    })
}
fn thumbnail(
    plan: &Plan,
    source: &mut Source<'_>,
    time: Option<&opencut_contracts::Time>,
    deadline: Instant,
) -> Result<Receipt> {
    use opencut_contracts::AssetKind;
    let limits = SourceThumbnailLimits {
        max_pixels: plan.policy.max_pcm_bytes / 16,
        max_bytes: plan.policy.max_pcm_bytes,
        retained_bytes: plan.encoded_resident_bytes,
        max_jpeg_bytes: SOURCE_ART_MAX_JPEG_BYTES,
    };
    let channel = source.channel.clone();
    let (image, source_width, source_height, observation) = if plan.kind == AssetKind::Image {
        if source.size > plan.policy.max_encoded_bytes
            || source.size.checked_mul(3) != Some(plan.encoded_resident_bytes)
        {
            return Err(error(
                "SOURCE_ART_BUDGET_EXCEEDED",
                "Static source input exceeds captured memory",
            ));
        }
        let length =
            usize::try_from(source.size).map_err(|_| protocol("Static source extent overflow"))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|_| {
            error(
                "SOURCE_ART_BUDGET_EXCEEDED",
                "Cannot allocate admitted image input",
            )
        })?;
        let mut offset = 0u64;
        let mut block = [0u8; 65536];
        while offset < source.size {
            let n = source
                .read_at(offset, &mut block)
                .map_err(crate::media::execution_error)?;
            if n == 0 {
                return Err(protocol("Static source ended before its descriptor"));
            }
            bytes.extend_from_slice(&block[..n]);
            offset += n as u64;
        }
        let image = SourceThumbnailImage::encoded(&bytes, limits, &mut || raster_check(&channel))
            .map_err(renderer_error)?;
        let (w, h) = image.source_size();
        (
            image,
            w,
            h,
            SourceArtThumbnailObservation::StaticImage {
                source_width: w,
                source_height: h,
            },
        )
    } else {
        let at = time.map(opencut_time::RationalTime::try_from).transpose()?;
        let requested = if let Some(at) = at {
            MediaTime {
                ticks: i64::try_from(at.ticks())
                    .map_err(|_| protocol("Native source point exceeds decoder time base"))?,
                timebase: at.timebase(),
            }
        } else {
            MediaTime {
                ticks: 1,
                timebase: 1,
            }
        };
        let mut decoder = NativeVideoDecoder::open(
            source,
            None,
            DecodeLimits {
                max_pixels: limits.max_pixels,
                max_resident_bytes: plan
                    .policy
                    .max_pcm_bytes
                    .saturating_sub(plan.encoded_resident_bytes),
                max_io_bytes_per_operation: plan
                    .policy
                    .max_encoded_bytes
                    .saturating_mul(8)
                    .max(65536),
                max_packets_per_operation: 100000,
            },
            deadline,
        )
        .map_err(crate::media::execution_error)?;
        let track = decoder.track().map_err(crate::media::execution_error)?;
        let (mut w, mut h) = (track.coded_width, track.coded_height);
        if track.pixel_aspect_numerator > 0 && track.pixel_aspect_denominator > 0 {
            let (n, d) = (
                f64::from(track.pixel_aspect_numerator),
                f64::from(track.pixel_aspect_denominator),
            );
            if n > d {
                let value = (f64::from(w) * n / d).round();
                if value > u32::MAX as f64 {
                    return Err(protocol("Native display width overflow"));
                }
                w = value as u32;
            } else {
                let value = (f64::from(h) * d / n).round();
                if value > u32::MAX as f64 {
                    return Err(protocol("Native display height overflow"));
                }
                h = value as u32;
            }
        }
        if track.rotation_degrees % 180 != 0 {
            std::mem::swap(&mut w, &mut h);
        }
        thumbnail_size(w, h)?;
        let requested_seconds = requested.ticks as f64 / f64::from(requested.timebase);
        let mut observation = SourceArtThumbnailObservation::Video {
            track_id: u32::try_from(track.track_id)
                .map_err(|_| protocol("Native source track has no nonnegative container id"))?,
            source_width: w,
            source_height: h,
            requested_seconds,
            actual_timestamp_seconds: None,
            frame_duration_seconds: None,
        };
        let frame = decoder
            .sample(source, requested, deadline)
            .map_err(crate::media::execution_error)?;
        let Some(frame) = frame else {
            return Ok(Receipt::ThumbnailNoFrame {
                identity: plan.identity.clone(),
                observation,
                reason: SourceArtNoFrameReason::NoSampleAtOneSecond,
                output_bytes: 0,
            });
        };
        if frame.width != track.coded_width
            || frame.height != track.coded_height
            || frame.rotation_degrees != track.rotation_degrees
        {
            return Err(protocol(
                "Native source dimensions changed after track observation",
            ));
        }
        if let SourceArtThumbnailObservation::Video {
            actual_timestamp_seconds,
            frame_duration_seconds,
            ..
        } = &mut observation
        {
            *actual_timestamp_seconds = Some(frame.timestamp_seconds());
            *frame_duration_seconds = Some(frame.duration_seconds());
        }
        // Retire codec reference surfaces before copying the retained RGBA
        // into Skia. The two phases reuse the same captured PCM envelope.
        drop(decoder);
        let image = SourceThumbnailImage::rgba(
            frame.width,
            frame.height,
            &frame.rgba,
            frame.rotation_degrees,
            limits,
            &mut || raster_check(&channel),
        )
        .map_err(renderer_error)?;
        drop(frame);
        (image, w, h, observation)
    };
    let (width, height) = thumbnail_size(source_width, source_height)?;
    let bytes = image
        .jpeg(width, height, limits, &mut || raster_check(&channel))
        .map_err(renderer_error)?;
    let output_bytes = bytes.len();
    source.channel.borrow_mut().emit(&bytes)?;
    Ok(Receipt::ThumbnailReady {
        identity: plan.identity.clone(),
        width,
        height,
        observation,
        output_bytes,
    })
}
