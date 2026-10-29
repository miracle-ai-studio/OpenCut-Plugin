use super::{input::Bridge, scaler::FrameScaler, *};
use crate::MediaTime;
use ffmpeg_next::{self as av, ffi};
use std::{cmp::Ordering, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoColorMatrix {
    Bt601,
    Bt709,
    Bt2020,
    Rgb,
    Unspecified,
}
/// Container/codec observations for source artwork, including no-frame results.
/// These are decoded source facts; they never replace an authored scene clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeVideoTrack {
    pub stream_index: u32,
    pub track_id: i32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub rotation_degrees: u16,
    pub pixel_aspect_numerator: i32,
    pub pixel_aspect_denominator: i32,
}
#[derive(Debug)]
pub struct DecodedVideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub pts: i64,
    pub duration: i64,
    pub time_base_numerator: u32,
    pub time_base_denominator: u32,
    pub rotation_degrees: u16,
    pub matrix: VideoColorMatrix,
    pub full_range: bool,
}
impl DecodedVideoFrame {
    pub fn timestamp_seconds(&self) -> f64 {
        self.pts as f64 * f64::from(self.time_base_numerator)
            / f64::from(self.time_base_denominator)
    }
    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 * f64::from(self.time_base_numerator)
            / f64::from(self.time_base_denominator)
    }
    pub fn ends_before(&self, time: MediaTime) -> bool {
        i128::from(self.pts) + i128::from(self.duration)
            <= timestamp(time, self.time_base_numerator, self.time_base_denominator)
    }
}

/// One selected immutable stream, with two neighboring decoded frames and a
/// single converted presentation frame. Continuous sampling never reopens the
/// demuxer; a distant/backwards seek flushes and starts at a preceding keyframe.
pub struct NativeVideoDecoder {
    input: av::format::context::Input,
    bridge: Bridge,
    decoder: av::decoder::Video,
    scaler: Option<FrameScaler>,
    limits: DecodeLimits,
    stream: usize,
    time_base: [u32; 2],
    end_pts: Option<i64>,
    rotation: u16,
    previous: Option<av::frame::Video>,
    next: Option<av::frame::Video>,
    converted: Option<Arc<DecodedVideoFrame>>,
    last_request: Option<MediaTime>,
    last_decoded_pts: Option<i64>,
    positioned: bool,
    draining: bool,
    ended: bool,
    retired: bool,
    image_mode: Option<bool>,
    reference_frames: u64,
}
struct Operation {
    packets: usize,
    first_byte: u64,
}

impl NativeVideoDecoder {
    pub fn open(
        source: &mut dyn ByteSource,
        selected_stream: Option<u32>,
        limits: DecodeLimits,
        deadline: Instant,
    ) -> MediaResult<Self> {
        limits.validate()?;
        // avcodec registration is automatic. Avoid network/device registration
        // and global log mutations; this adapter cannot open URLs or devices.
        let mut bridge = Bridge::new(source.len())?;
        let input = bridge.open(source, limits.max_io_bytes_per_operation, deadline)?;
        let stream = match selected_stream {
            Some(index) => input.stream(index as usize),
            None => input
                .streams()
                .find(|stream| stream.parameters().medium() == av::media::Type::Video),
        }
        .ok_or_else(|| invalid("Selected native video stream is missing"))?;
        if stream.parameters().medium() != av::media::Type::Video {
            return Err(invalid("Selected native stream is not video"));
        }
        let time_base = stream.time_base();
        if time_base.numerator() <= 0 || time_base.denominator() <= 0 {
            return Err(invalid("Native video stream has no exact time base"));
        }
        let rotation = rotation(&stream)?;
        let end_pts = (stream.start_time() != ffi::AV_NOPTS_VALUE && stream.duration() > 0)
            .then(|| stream.start_time().checked_add(stream.duration()))
            .flatten();
        let index = stream.index();
        let mut context =
            av::codec::Context::from_parameters(stream.parameters()).map_err(failed)?;
        context.set_threading(av::codec::threading::Config {
            count: 1,
            kind: av::codec::threading::Type::Slice,
            ..Default::default()
        });
        // SAFETY: the context is uniquely owned and not opened yet. This is
        // checked by codec allocation before accepting coded dimensions.
        unsafe {
            (*context.as_mut_ptr()).max_pixels = limits.max_pixels as i64;
        }
        let reference_frames = match context.id() {
            av::codec::Id::H264 | av::codec::Id::HEVC => 16,
            av::codec::Id::AV1 | av::codec::Id::VP9 => 8,
            av::codec::Id::VP8 => 3,
            av::codec::Id::PNG
            | av::codec::Id::MJPEG
            | av::codec::Id::WEBP
            | av::codec::Id::BMP
            | av::codec::Id::GIF => 0,
            _ => 16,
        };
        let decoder = context.decoder().video().map_err(failed)?;
        checkpoint(source, deadline)?;
        Ok(Self {
            input,
            bridge,
            decoder,
            scaler: None,
            limits,
            stream: index,
            time_base: [time_base.numerator() as u32, time_base.denominator() as u32],
            end_pts,
            rotation,
            previous: None,
            next: None,
            converted: None,
            last_request: None,
            last_decoded_pts: None,
            positioned: false,
            draining: false,
            ended: false,
            retired: false,
            image_mode: None,
            reference_frames,
        })
    }

    pub fn stream_index(&self) -> usize {
        self.stream
    }
    pub fn track(&self) -> MediaResult<NativeVideoTrack> {
        if self.retired {
            return Err(invalid("Native video decoder was retired"));
        }
        let stream = self
            .input
            .stream(self.stream)
            .ok_or_else(|| invalid("Selected native video stream disappeared"))?;
        let aspect = self.decoder.aspect_ratio();
        Ok(NativeVideoTrack {
            stream_index: self.stream as u32,
            track_id: stream.id(),
            coded_width: self.decoder.width(),
            coded_height: self.decoder.height(),
            rotation_degrees: self.rotation,
            pixel_aspect_numerator: aspect.numerator(),
            pixel_aspect_denominator: aspect.denominator(),
        })
    }
    pub fn encoded_bytes_read(&self) -> u64 {
        self.bridge.bytes_read()
    }
    pub fn reached_eof(&self) -> bool {
        self.ended
    }

    /// For an image, decode one real frame without inventing a frame rate or
    /// temporal duration. Animated formats are deliberately sampled at start.
    pub fn first_image(
        &mut self,
        source: &mut dyn ByteSource,
        deadline: Instant,
    ) -> MediaResult<Option<Arc<DecodedVideoFrame>>> {
        checkpoint(source, deadline)?;
        if self.retired || self.image_mode == Some(false) {
            return Err(invalid("Native decoder was retired"));
        }
        self.image_mode = Some(true);
        if let Some(frame) = &self.converted {
            return Ok(Some(frame.clone()));
        }
        let result = (|| {
            let mut operation = Operation {
                packets: 0,
                first_byte: self.bridge.bytes_read(),
            };
            let Some(frame) = self.read_frame(source, deadline, &mut operation)? else {
                return Ok(None);
            };
            self.previous = Some(frame);
            // Image receipts do not carry media timing. Zero is descriptive,
            // not an invented video duration.
            self.convert(0, 0).map(Some)
        })();
        let result = result.and_then(|frame| {
            checkpoint(source, deadline)?;
            Ok(frame)
        });
        if result.is_err() {
            self.retire();
        }
        result
    }

    pub fn sample(
        &mut self,
        source: &mut dyn ByteSource,
        requested: MediaTime,
        deadline: Instant,
    ) -> MediaResult<Option<Arc<DecodedVideoFrame>>> {
        checkpoint(source, deadline)?;
        if requested.timebase == 0
            || requested.ticks < 0
            || self.retired
            || self.image_mode == Some(true)
        {
            return Err(invalid("Invalid native sample or retired decoder"));
        }
        self.image_mode = Some(false);
        let result = self
            .sample_inner(source, requested, deadline)
            .and_then(|frame| {
                checkpoint(source, deadline)?;
                Ok(frame)
            });
        if result.is_err() {
            self.retire();
        }
        result
    }

    fn sample_inner(
        &mut self,
        source: &mut dyn ByteSource,
        requested: MediaTime,
        deadline: Instant,
    ) -> MediaResult<Option<Arc<DecodedVideoFrame>>> {
        let mut operation = Operation {
            packets: 0,
            first_byte: self.bridge.bytes_read(),
        };
        let distant = self.last_request.is_some_and(|previous| {
            let delta = i128::from(requested.ticks) * i128::from(previous.timebase)
                - i128::from(previous.ticks) * i128::from(requested.timebase);
            delta < 0 || delta > 2 * i128::from(previous.timebase) * i128::from(requested.timebase)
        });
        if !self.positioned || distant {
            self.seek(source, requested, deadline)?;
        }
        let target = timestamp(requested, self.time_base[0], self.time_base[1]);
        loop {
            checkpoint(source, deadline)?;
            if self.next.is_none() && !self.ended {
                self.next = self.read_frame(source, deadline, &mut operation)?;
            }
            let Some(next) = &self.next else { break };
            let pts = next
                .timestamp()
                .or(next.pts())
                .ok_or_else(|| invalid("Decoded video has no PTS"))?;
            if i128::from(pts) > target {
                break;
            }
            self.previous = self.next.take();
            // Keep at most the previous conversion until its replacement is
            // needed. A uniquely owned surface can then be overwritten without
            // allocating and clearing another full-resolution RGBA frame.
        }
        self.last_request = Some(requested);
        let Some(frame) = &self.previous else {
            return Ok(None);
        };
        let pts = frame
            .timestamp()
            .or(frame.pts())
            .ok_or_else(|| invalid("Decoded video has no PTS"))?;
        let packet_duration = frame.packet().duration;
        let duration = if packet_duration > 0 {
            packet_duration
        } else {
            self.next
                .as_ref()
                .and_then(|next| next.timestamp().or(next.pts()))
                .or(self.end_pts)
                .and_then(|end| end.checked_sub(pts))
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("Decoded video has no observed frame duration"))?
        };
        if let Some(frame) = &self.converted
            && frame.pts == pts
            && frame.duration == duration
        {
            return Ok(Some(frame.clone()));
        }
        self.convert(pts, duration).map(Some)
    }

    fn seek(
        &mut self,
        source: &mut dyn ByteSource,
        requested: MediaTime,
        deadline: Instant,
    ) -> MediaResult<()> {
        let pts = i64::try_from(timestamp(requested, self.time_base[0], self.time_base[1]))
            .map_err(|_| invalid("Native seek timestamp overflow"))?;
        let input = &mut self.input;
        let stream = self.stream as i32;
        self.bridge.run(
            source,
            deadline,
            self.limits.max_io_bytes_per_operation,
            || {
                // SAFETY: the input remains exclusively borrowed; its current
                // authorized AVIO scope is installed for this synchronous seek.
                let result = unsafe {
                    ffi::av_seek_frame(input.as_mut_ptr(), stream, pts, ffi::AVSEEK_FLAG_BACKWARD)
                };
                if result < 0 {
                    return Err(failed(av::Error::from(result)));
                }
                Ok(())
            },
        )?;
        self.decoder.flush();
        self.previous = None;
        self.next = None;
        self.converted = None;
        self.last_decoded_pts = None;
        self.draining = false;
        self.ended = false;
        self.positioned = true;
        Ok(())
    }

    fn read_frame(
        &mut self,
        source: &mut dyn ByteSource,
        deadline: Instant,
        operation: &mut Operation,
    ) -> MediaResult<Option<av::frame::Video>> {
        if self.ended {
            return Ok(None);
        }
        loop {
            checkpoint(source, deadline)?;
            let mut frame = av::frame::Video::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    self.admit_frame(&frame)?;
                    if frame.is_corrupt() || frame.has_decode_errors() {
                        return Err(invalid("Native decoder reported a corrupt frame"));
                    }
                    if let Some(pts) = frame.timestamp().or(frame.pts()) {
                        if self
                            .last_decoded_pts
                            .is_some_and(|previous| pts <= previous)
                        {
                            return Err(invalid("Native video PTS is not strictly increasing"));
                        }
                        self.last_decoded_pts = Some(pts);
                    }
                    return Ok(Some(frame));
                }
                Err(av::Error::Eof) => {
                    self.ended = true;
                    return Ok(None);
                }
                Err(av::Error::Other { errno }) if errno == av::error::EAGAIN => {}
                Err(error) => return Err(failed(error)),
            }
            if self.draining {
                return Err(invalid("Native decoder requested packets after EOF drain"));
            }
            let input = &mut self.input;
            let mut packet = av::Packet::empty();
            let remaining = self
                .limits
                .max_io_bytes_per_operation
                .checked_sub(
                    self.bridge
                        .bytes_read()
                        .saturating_sub(operation.first_byte),
                )
                .ok_or(MediaError::OutputLimit {
                    limit_bytes: self.limits.max_io_bytes_per_operation,
                })?;
            let read = self
                .bridge
                .run(source, deadline, remaining, || Ok(packet.read(input)))?;
            match read {
                Ok(()) => {
                    operation.packets += 1;
                    if operation.packets > self.limits.max_packets_per_operation {
                        return Err(MediaError::OutputLimit {
                            limit_bytes: self.limits.max_io_bytes_per_operation,
                        });
                    }
                    if packet.stream() != self.stream {
                        continue;
                    }
                    self.decoder.send_packet(&packet).map_err(failed)?;
                }
                Err(av::Error::Eof) => {
                    self.decoder.send_eof().map_err(failed)?;
                    self.draining = true;
                }
                Err(error) => return Err(failed(error)),
            }
        }
    }

    fn frame_residency(&self, frame: &av::frame::Video) -> MediaResult<u64> {
        let pixels = u64::from(frame.width()) * u64::from(frame.height());
        if pixels == 0
            || pixels > self.limits.max_pixels
            || frame.width() > 16384
            || frame.height() > 16384
        {
            return Err(MediaError::OutputLimit {
                limit_bytes: self.limits.max_resident_bytes,
            });
        }
        // Reserve up to sixteen codec reference surfaces, two neighboring
        // frames and one immutable conversion output. The host must
        // additionally enforce RSS for codec-private allocations.
        let surface = unsafe {
            ffi::av_image_get_buffer_size(
                frame.format().into(),
                frame.width() as i32,
                frame.height() as i32,
                64,
            )
        };
        let surfaces = if self.image_mode == Some(true) {
            1
        } else {
            // Codec headers expose the actual reference count after decoding.
            // Unknown counts retain the format's conservative maximum.
            let observed = unsafe { (*self.decoder.as_ptr()).refs };
            if observed > 0 && observed as u64 <= self.reference_frames {
                observed as u64 + 2
            } else {
                self.reference_frames + 2
            }
        };
        let output = (pixels * 4).max(self.converted.as_ref().map_or(0, |f| f.rgba.len() as u64));
        if surface <= 0 || (surface as u64) * surfaces + output > self.limits.max_resident_bytes {
            return Err(MediaError::OutputLimit {
                limit_bytes: self.limits.max_resident_bytes,
            });
        }
        Ok((surface as u64) * surfaces + output)
    }

    fn admit_frame(&self, frame: &av::frame::Video) -> MediaResult<()> {
        self.frame_residency(frame).map(|_| ())
    }

    fn convert(&mut self, pts: i64, duration: i64) -> MediaResult<Arc<DecodedVideoFrame>> {
        let frame = self
            .previous
            .as_ref()
            .ok_or_else(|| invalid("No decoded frame to convert"))?;
        let width = frame.width();
        let height = frame.height();
        let threads = super::scaler::conversion_threads(
            u64::from(width) * u64::from(height),
            self.frame_residency(frame)?,
            self.limits.max_resident_bytes,
        );
        let definition = av::software::scaling::context::Definition {
            format: frame.format(),
            width,
            height,
        };
        if self
            .scaler
            .as_ref()
            .is_none_or(|scaler| scaler.input != definition || scaler.threads != threads)
        {
            self.scaler = None;
            self.scaler = Some(FrameScaler::new(definition, threads)?);
        }
        let scaler = self.scaler.as_mut().expect("prepared scaler");
        let matrix = match frame.color_space() {
            av::color::Space::BT709 => VideoColorMatrix::Bt709,
            av::color::Space::BT470BG | av::color::Space::SMPTE170M => VideoColorMatrix::Bt601,
            av::color::Space::BT2020NCL => VideoColorMatrix::Bt2020,
            av::color::Space::RGB => VideoColorMatrix::Rgb,
            av::color::Space::Unspecified => VideoColorMatrix::Unspecified,
            _ => {
                return Err(MediaError::Unsupported(
                    "Unsupported native source color matrix".into(),
                ));
            }
        };
        let full_range = frame.color_range() == av::color::Range::JPEG
            || matches!(matrix, VideoColorMatrix::Rgb);
        // Swscale does not automatically apply AVFrame's matrix/range metadata.
        // Supply it explicitly; RGB output retains encoded source transfer.
        let colorspace = match matrix {
            VideoColorMatrix::Bt709 => ffi::SWS_CS_ITU709,
            VideoColorMatrix::Bt2020 => ffi::SWS_CS_BT2020,
            _ => ffi::SWS_CS_ITU601,
        };
        let length = width as usize * height as usize * 4;
        let mut bytes = self
            .converted
            .take()
            .and_then(|old| Arc::try_unwrap(old).ok())
            .filter(|old| old.rgba.len() == length)
            .map_or_else(|| vec![0u8; length], |old| old.rgba);
        scaler.convert(frame, &mut bytes, colorspace as i32, full_range)?;
        let frame = Arc::new(DecodedVideoFrame {
            width,
            height,
            rgba: bytes,
            pts,
            duration,
            time_base_numerator: self.time_base[0],
            time_base_denominator: self.time_base[1],
            rotation_degrees: self.rotation,
            matrix,
            full_range,
        });
        self.converted = Some(frame.clone());
        Ok(frame)
    }

    fn retire(&mut self) {
        self.retired = true;
        self.previous = None;
        self.next = None;
        self.converted = None;
        self.scaler = None;
        self.decoder.flush();
    }
}

fn timestamp(time: MediaTime, top: u32, bottom: u32) -> i128 {
    (i128::from(time.ticks) * i128::from(bottom))
        .div_euclid(i128::from(time.timebase) * i128::from(top))
}
fn rotation(stream: &av::Stream<'_>) -> MediaResult<u16> {
    let Some(data) = stream
        .side_data()
        .find(|data| data.kind() == av::codec::packet::side_data::Type::DisplayMatrix)
    else {
        return Ok(0);
    };
    if data.data().len() != 36 {
        return Err(invalid("Invalid native display matrix"));
    }
    let matrix: Vec<i32> = data
        .data()
        .chunks_exact(4)
        .map(|bytes| i32::from_ne_bytes(bytes.try_into().expect("matrix component")))
        .collect();
    if (i128::from(matrix[0]) * i128::from(matrix[4])
        - i128::from(matrix[1]) * i128::from(matrix[3]))
    .cmp(&0)
        != Ordering::Greater
    {
        return Err(MediaError::Unsupported(
            "Mirrored native display matrices require explicit source normalization".into(),
        ));
    }
    let degrees = -unsafe { ffi::av_display_rotation_get(matrix.as_ptr()) };
    if !degrees.is_finite() || (degrees / 90.0 - (degrees / 90.0).round()).abs() > 0.00001 {
        return Err(MediaError::Unsupported(
            "Non-orthogonal native video orientation".into(),
        ));
    }
    Ok(((degrees.round() as i32).rem_euclid(360)) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Source {
        bytes: Vec<u8>,
        revoked: bool,
    }
    impl ByteSource for Source {
        fn len(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read_at(&mut self, offset: u64, output: &mut [u8]) -> MediaResult<usize> {
            self.checkpoint()?;
            let start = (offset as usize).min(self.bytes.len());
            let count = output.len().min(self.bytes.len() - start);
            output[..count].copy_from_slice(&self.bytes[start..start + count]);
            Ok(count)
        }
        fn checkpoint(&mut self) -> MediaResult<()> {
            if self.revoked {
                Err(MediaError::Admission {
                    code: "NOT_AUTHORIZED".into(),
                    message: "Revoked synthetic video".into(),
                })
            } else {
                Ok(())
            }
        }
    }
    #[test]
    #[ignore = "Requires retained OPENCUT_NATIVE_VIDEO_SOURCE; no media generation"]
    fn continuous_rgba_reuse_keeps_live_frames_immutable_and_rechecks_authority() {
        let mut source = Source {
            bytes: std::fs::read(
                std::env::var_os("OPENCUT_NATIVE_VIDEO_SOURCE").expect("Retained moving video"),
            )
            .unwrap(),
            revoked: false,
        };
        let limits = DecodeLimits {
            max_pixels: 16_777_216,
            max_resident_bytes: 512 * 1024 * 1024,
            max_io_bytes_per_operation: 512 * 1024 * 1024,
            max_packets_per_operation: 100_000,
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(45);
        let mut decoder = NativeVideoDecoder::open(&mut source, None, limits, deadline).unwrap();
        let first = decoder
            .sample(
                &mut source,
                MediaTime {
                    ticks: 0,
                    timebase: 30,
                },
                deadline,
            )
            .unwrap()
            .unwrap();
        let original = first.rgba.clone();
        let cached = decoder
            .sample(
                &mut source,
                MediaTime {
                    ticks: 0,
                    timebase: 30,
                },
                deadline,
            )
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &cached));
        drop(cached);
        let second = decoder
            .sample(
                &mut source,
                MediaTime {
                    ticks: 1,
                    timebase: 30,
                },
                deadline,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            first.rgba, original,
            "A caller still owns the previous pixels"
        );
        assert_ne!(first.rgba.as_ptr(), second.rgba.as_ptr());
        drop(first);
        let reusable = second.rgba.as_ptr();
        drop(second);
        let third = decoder
            .sample(
                &mut source,
                MediaTime {
                    ticks: 2,
                    timebase: 30,
                },
                deadline,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            third.rgba.as_ptr(),
            reusable,
            "Only the uniquely owned conversion is recycled"
        );
        assert_ne!(
            third.rgba, original,
            "The full next frame must overwrite the recycled surface"
        );
        let mut fresh = NativeVideoDecoder::open(&mut source, None, limits, deadline).unwrap();
        let reference = fresh
            .sample(
                &mut source,
                MediaTime {
                    ticks: 2,
                    timebase: 30,
                },
                deadline,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            third.rgba, reference.rgba,
            "Recycling must retain the ordinary complete conversion pixels"
        );
        source.revoked = true;
        let before_revocation = decoder.encoded_bytes_read();
        assert!(
            matches!(decoder.sample(&mut source,MediaTime {ticks:3,timebase:30},deadline),Err(MediaError::Admission {code,..}) if code=="NOT_AUTHORIZED")
        );
        // Entry rejection precedes an operation. It must not read or expose
        // cached bytes; retirement belongs to failures during admitted work.
        assert_eq!(decoder.encoded_bytes_read(), before_revocation);
        source.revoked = false;
        let resumed = decoder
            .sample(
                &mut source,
                MediaTime {
                    ticks: 2,
                    timebase: 30,
                },
                deadline,
            )
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&third, &resumed));
        drop(decoder);
        assert_eq!(third.rgba, reference.rgba);
    }
}
