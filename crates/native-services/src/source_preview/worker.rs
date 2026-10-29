//! CPU-only source iteration over live authorized bytes. Each frame is emitted
//! completely before advancing; no full source or project is resident here.
use super::{
    protocol::{Frame, Plan, Track},
    *,
};
use crate::native_process::wire;
use opencut_media::{MediaError, native_decode::*};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    io::{Read, Write},
    rc::Rc,
    time::{Duration, Instant},
};

struct Channel<'a> {
    input: &'a mut dyn Read,
    output: &'a mut dyn Write,
    sequence: u64,
    deadline: Instant,
}
impl Channel<'_> {
    fn check(&self) -> Result<(), EditorError> {
        if Instant::now() >= self.deadline {
            return Err(error(
                "SOURCE_PREVIEW_TIME_LIMIT",
                "Source preparation deadline exceeded",
            ));
        }
        Ok(())
    }
    fn request(&mut self, mut meta: Value, data: &[u8]) -> Result<wire::Packet, EditorError> {
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
            return Err(protocol("Source preparation reply changed its request"));
        }
        self.check()?;
        Ok(packet)
    }
    fn event(&mut self, meta: Value, data: &[u8]) -> Result<(), EditorError> {
        if !self.request(meta, data)?.data.is_empty() {
            return Err(protocol(
                "Unexpected source preparation acknowledgement bytes",
            ));
        }
        Ok(())
    }
}
struct Source<'a> {
    channel: Rc<RefCell<Channel<'a>>>,
    size: u64,
}
fn admission(error: EditorError) -> MediaError {
    MediaError::Admission {
        code: error.code,
        message: error.message,
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
            .ok_or_else(|| admission(protocol("Missing source EOF fact")))?;
        if wire::integer(&packet.meta, "offset").map_err(admission)? != offset
            || packet.data.len() > count
            || offset
                .checked_add(packet.data.len() as u64)
                .is_none_or(|n| n > self.size)
            || eof != (offset + packet.data.len() as u64 == self.size)
            || (packet.data.is_empty() && !eof)
        {
            return Err(admission(protocol("Source range differs from fixed bytes")));
        }
        output[..packet.data.len()].copy_from_slice(&packet.data);
        Ok(packet.data.len())
    }
}
pub(crate) fn serve(
    packet: wire::Packet,
    input: &mut dyn Read,
    output: &mut dyn Write,
) -> Result<(), EditorError> {
    if !packet.data.is_empty() {
        return Err(protocol("Unexpected source initialization bytes"));
    }
    let plan: Plan = serde_json::from_value(packet.meta["plan"].clone())
        .map_err(|_| protocol("Invalid typed source preparation plan"))?;
    plan.validate()?;
    let executable =
        std::env::current_exe().map_err(|_| protocol("Cannot observe source worker"))?;
    if decoder_identity(&executable)? != plan.decoder {
        return Err(error(
            "SOURCE_PREVIEW_ENGINE_CHANGED",
            "Source decoder dependencies changed",
        ));
    }
    wire::send(
        output,
        json!({"type":"ready","purpose":"source_preview","pid":std::process::id(),"identity":plan.decoder}),
        &[],
    )?;
    let deadline = Instant::now() + Duration::from_millis(plan.policy.deadline_ms);
    let channel = Rc::new(RefCell::new(Channel {
        input,
        output,
        sequence: 0,
        deadline,
    }));
    let mut source = Source {
        channel: channel.clone(),
        size: plan.identity.source_version.size,
    };
    let raster = plan.identity.raster_bound;
    let reserve = u64::from(raster.width) * u64::from(raster.height) * 4;
    let limits = DecodeLimits {
        max_pixels: plan.policy.max_source_pixels,
        max_resident_bytes: plan
            .policy
            .max_decoded_bytes
            .checked_sub(reserve)
            .ok_or_else(|| protocol("Source normalized raster exceeds memory admission"))?,
        max_io_bytes_per_operation: source.size.max(65536),
        max_packets_per_operation: 100_000,
    };
    let mut decoder = NativeVideoDecoder::open_with_threads(
        &mut source,
        plan.identity.selected_video_stream,
        limits,
        deadline,
        plan.policy.max_decode_threads as usize,
    )
    .map_err(crate::media::execution_error)?;
    let track = decoder.track().map_err(crate::media::execution_error)?;
    channel.borrow_mut().event(json!({"type":"source_track","track":Track {stream_index:track.stream_index,track_id:track.track_id}}), &[])?;
    let mut normalized = Vec::new();
    let mut ordinal = 0u64;
    while let Some(frame) = decoder
        .next_frame_with_output(
            &mut source,
            VideoOutputSize::Fit {
                width: raster.width,
                height: raster.height,
            },
            deadline,
        )
        .map_err(crate::media::execution_error)?
    {
        if ordinal >= plan.policy.max_frames {
            return Err(error(
                "SOURCE_PREVIEW_BUDGET_EXCEEDED",
                "Source frame count exceeds admission",
            ));
        }
        let video = SourcePreviewFrameVideo {
            source_width: frame.source_width,
            source_height: frame.source_height,
            raster_width: frame.width,
            raster_height: frame.height,
            rotation_degrees: frame.rotation_degrees,
            pixel_aspect_numerator: frame.pixel_aspect_numerator,
            pixel_aspect_denominator: frame.pixel_aspect_denominator,
            matrix: match frame.matrix {
                VideoColorMatrix::Bt601 => SourcePreviewMatrix::Bt601,
                VideoColorMatrix::Bt709 => SourcePreviewMatrix::Bt709,
                VideoColorMatrix::Bt2020 => SourcePreviewMatrix::Bt2020,
                VideoColorMatrix::Rgb => SourcePreviewMatrix::Rgb,
                VideoColorMatrix::Unspecified => SourcePreviewMatrix::Unspecified,
            },
            full_range: frame.full_range,
            has_alpha: frame.source_has_alpha,
        };
        super::raster::normalize(&frame.rgba, &video, raster, &mut normalized, &mut || {
            channel.borrow().check()
        })?;
        channel.borrow_mut().event(json!({"type":"source_frame","frame":Frame { ordinal,pts:frame.pts,duration:frame.duration,time_base:SourcePreviewTimeBase {numerator:frame.time_base_numerator,denominator:frame.time_base_denominator},video }}), &[])?;
        for (part, bytes) in normalized.chunks(65536).enumerate() {
            channel.borrow_mut().event(
                json!({"type":"source_rgba","ordinal":ordinal,"offset":part as u64*65536}),
                bytes,
            )?;
        }
        channel.borrow_mut().event(
            json!({"type":"source_frame_complete","ordinal":ordinal}),
            &[],
        )?;
        ordinal += 1;
    }
    drop(decoder);
    channel.borrow().check()?;
    if decoder_identity(&executable)? != plan.decoder {
        return Err(error(
            "SOURCE_PREVIEW_ENGINE_CHANGED",
            "Source decoder dependencies changed during execution",
        ));
    }
    wire::send(
        channel.borrow_mut().output,
        json!({"type":"source_complete","frame_count":ordinal}),
        &[],
    )?;
    let finish = wire::read(channel.borrow_mut().input)?;
    if finish.meta["type"] != "finish" || !finish.data.is_empty() {
        return Err(protocol("Source completion was not acknowledged"));
    }
    channel.borrow().check()
}
