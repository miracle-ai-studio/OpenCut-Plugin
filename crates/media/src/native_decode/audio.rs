//! Authorized libav audio frames, preserving native rate, channels and PTS.
//! Rate conversion, gap/concatenation choice, preparation and mixing belong to
//! their existing application/DSP ports; this decoder never invents that clock.
use super::{input::Bridge, *};
use ffmpeg_next::{self as av, ffi};
#[derive(Clone, Copy, Debug)]
pub struct AudioDecodeLimits {
    pub max_channels: u32,
    pub max_frame_samples: u32,
    pub max_resident_bytes: u64,
    pub max_io_bytes_per_operation: u64,
    pub max_packets_per_operation: usize,
}
impl AudioDecodeLimits {
    fn validate(&self) -> MediaResult<()> {
        if !(1..=64).contains(&self.max_channels)
            || !(1..=1048576).contains(&self.max_frame_samples)
            || self.max_resident_bytes < 65536
            || self.max_io_bytes_per_operation < 65536
            || !(1..=100000).contains(&self.max_packets_per_operation)
        {
            return Err(MediaError::InvalidRequest(
                "Invalid native audio decode admission".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeAudioTrack {
    pub stream_index: u32,
    pub track_id: i32,
    pub codec: String,
    /// A codec may reveal these only on its first real decoded frame. Zero
    /// before that frame is unknown, never permission to allocate arbitrary PCM.
    pub sample_rate: u32,
    pub channels: u32,
    pub time_base_numerator: u32,
    pub time_base_denominator: u32,
}
#[derive(Debug)]
pub struct DecodedAudioFrame {
    pub pts: Option<i64>,
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
    pub time_base_numerator: u32,
    pub time_base_denominator: u32,
}
impl DecodedAudioFrame {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }
}
pub struct NativeAudioDecoder {
    input: av::format::context::Input,
    bridge: Bridge,
    decoder: av::decoder::Audio,
    track: NativeAudioTrack,
    limits: AudioDecodeLimits,
    draining: bool,
    ended: bool,
    retired: bool,
    last_pts: Option<i64>,
    pending: std::collections::VecDeque<av::Packet>,
    started: bool,
    demux_eof: bool,
}
impl NativeAudioDecoder {
    pub fn open(
        source: &mut dyn ByteSource,
        selected_stream: Option<u32>,
        limits: AudioDecodeLimits,
        deadline: Instant,
    ) -> MediaResult<Self> {
        limits.validate()?;
        let mut bridge = Bridge::new(source.len())?;
        let input = bridge.open(source, limits.max_io_bytes_per_operation, deadline)?;
        // The retained pinned demuxer selects the first audio track in stored
        // order. Do not silently replace that with libav's ranked "best" stream.
        let stream = match selected_stream {
            Some(n) => input.stream(n as usize),
            None => input
                .streams()
                .find(|s| s.parameters().medium() == av::media::Type::Audio),
        }
        .ok_or_else(|| MediaError::Unsupported("Selected native audio stream is missing".into()))?;
        if stream.parameters().medium() != av::media::Type::Audio {
            return Err(invalid("Selected native stream is not audio"));
        }
        let time = stream.time_base();
        if time.numerator() <= 0 || time.denominator() <= 0 {
            return Err(invalid("Native audio stream has no exact time base"));
        }
        let mut context =
            av::codec::Context::from_parameters(stream.parameters()).map_err(failed)?;
        context.set_threading(av::codec::threading::Config {
            count: 1,
            kind: av::codec::threading::Type::Slice,
            ..Default::default()
        });
        // SAFETY: this uniquely owned context is not open. The codec's frame
        // allocation limit precedes accepting any decoded sample allocation.
        unsafe {
            (*context.as_mut_ptr()).max_samples = i64::from(limits.max_frame_samples);
        }
        let decoder = context.decoder().audio().map_err(failed)?;
        let track = NativeAudioTrack {
            stream_index: stream.index() as u32,
            track_id: stream.id(),
            codec: decoder.id().name().into(),
            sample_rate: decoder.rate(),
            channels: u32::from(decoder.channels()),
            time_base_numerator: time.numerator() as u32,
            time_base_denominator: time.denominator() as u32,
        };
        if track.channels > limits.max_channels || track.sample_rate > 384000 {
            return Err(invalid("Native audio header exceeds its admitted format"));
        }
        checkpoint(source, deadline)?;
        Ok(Self {
            input,
            bridge,
            decoder,
            track,
            limits,
            draining: false,
            ended: false,
            retired: false,
            last_pts: None,
            pending: std::collections::VecDeque::new(),
            started: false,
            demux_eof: false,
        })
    }
    pub fn track(&self) -> &NativeAudioTrack {
        &self.track
    }
    pub fn encoded_bytes_read(&self) -> u64 {
        self.bridge.bytes_read()
    }
    pub fn reached_eof(&self) -> bool {
        self.ended
    }
    /// Begin at the last independent encoded packet at/before an observed
    /// stream PTS, or its first packet when the target precedes the stream.
    /// No pre-roll packet is decoded and then discarded: for codecs such as
    /// AAC that would change overlap/noise state of every subsequent frame.
    /// The caller chooses this source-consumer behavior, not an authored clock.
    pub fn select_keyframe_at_pts(
        &mut self,
        source: &mut dyn ByteSource,
        pts: i64,
        deadline: Instant,
    ) -> MediaResult<()> {
        if self.retired || self.started || !self.pending.is_empty() {
            return Err(invalid("Native audio packet origin is already bound"));
        }
        let first = self.bridge.bytes_read();
        let mut packets = 0;
        let mut candidate = None;
        let result = (|| {
            loop {
                checkpoint(source, deadline)?;
                let Some(packet) = self.next_packet(source, deadline, first, &mut packets)? else {
                    if let Some(packet) = candidate.take() {
                        self.pending.push_back(packet);
                        return Ok(());
                    }
                    return Err(invalid(
                        "Native audio has no independent packet at the requested origin",
                    ));
                };
                if !packet.is_key() {
                    return Err(MediaError::Unsupported(
                        "Native audio origin requires independently addressable packets".into(),
                    ));
                }
                let observed = packet.pts().ok_or_else(|| {
                    MediaError::Unsupported(
                        "Native audio packet has no observed presentation timestamp".into(),
                    )
                })?;
                if observed <= pts {
                    candidate = Some(packet);
                    continue;
                }
                if let Some(packet) = candidate.take() {
                    self.pending.push_back(packet);
                }
                self.pending.push_back(packet);
                return Ok(());
            }
        })();
        if result.is_err() {
            self.retired = true;
            self.pending.clear();
            self.decoder.flush();
        }
        result
    }
    pub fn read(
        &mut self,
        source: &mut dyn ByteSource,
        deadline: Instant,
    ) -> MediaResult<Option<DecodedAudioFrame>> {
        if self.retired {
            return Err(invalid("Native audio decoder was retired"));
        }
        self.started = true;
        let result = self.read_inner(source, deadline).and_then(|frame| {
            checkpoint(source, deadline)?;
            Ok(frame)
        });
        if result.is_err() {
            self.retired = true;
            self.decoder.flush();
        }
        result
    }
    fn read_inner(
        &mut self,
        source: &mut dyn ByteSource,
        deadline: Instant,
    ) -> MediaResult<Option<DecodedAudioFrame>> {
        if self.ended {
            return Ok(None);
        }
        let mut packets = 0usize;
        let first_byte = self.bridge.bytes_read();
        loop {
            checkpoint(source, deadline)?;
            let mut frame = av::frame::Audio::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => return self.convert(frame).map(Some),
                Err(av::Error::Eof) => {
                    self.ended = true;
                    return Ok(None);
                }
                Err(av::Error::Other { errno }) if errno == av::error::EAGAIN => {}
                Err(error) => return Err(failed(error)),
            }
            if self.draining {
                return Err(invalid(
                    "Native audio decoder requested packets after draining EOF",
                ));
            }
            let packet = if let Some(packet) = self.pending.pop_front() {
                Some(packet)
            } else {
                self.next_packet(source, deadline, first_byte, &mut packets)?
            };
            match packet {
                Some(packet) => self.decoder.send_packet(&packet).map_err(failed)?,
                None => {
                    self.decoder.send_eof().map_err(failed)?;
                    self.draining = true;
                }
            }
        }
    }
    fn next_packet(
        &mut self,
        source: &mut dyn ByteSource,
        deadline: Instant,
        first_byte: u64,
        packets: &mut usize,
    ) -> MediaResult<Option<av::Packet>> {
        if self.demux_eof {
            return Ok(None);
        }
        loop {
            checkpoint(source, deadline)?;
            let remaining = self
                .limits
                .max_io_bytes_per_operation
                .checked_sub(self.bridge.bytes_read().saturating_sub(first_byte))
                .ok_or(MediaError::OutputLimit {
                    limit_bytes: self.limits.max_io_bytes_per_operation,
                })?;
            let input = &mut self.input;
            let mut packet = av::Packet::empty();
            let read = self
                .bridge
                .run(source, deadline, remaining, || Ok(packet.read(input)))?;
            match read {
                Ok(()) => {
                    *packets += 1;
                    if *packets > self.limits.max_packets_per_operation {
                        return Err(MediaError::OutputLimit {
                            limit_bytes: self.limits.max_io_bytes_per_operation,
                        });
                    }
                    if packet.stream() != self.track.stream_index as usize {
                        continue;
                    }
                    // Origin selection owns at most two packets. Their complete
                    // bound remains inside the caller's codec work reservation.
                    if packet.size() as u64 > self.limits.max_resident_bytes / 4 {
                        return Err(MediaError::OutputLimit {
                            limit_bytes: self.limits.max_resident_bytes,
                        });
                    }
                    return Ok(Some(packet));
                }
                Err(av::Error::Eof) => {
                    self.demux_eof = true;
                    return Ok(None);
                }
                Err(e) => return Err(failed(e)),
            }
        }
    }
    fn convert(&mut self, frame: av::frame::Audio) -> MediaResult<DecodedAudioFrame> {
        let channels = u32::from(frame.channels());
        let rate = frame.rate();
        let frames = frame.samples();
        if frame.is_corrupt()
            || frame.has_decode_errors()
            || channels == 0
            || channels > self.limits.max_channels
            || rate == 0
            || rate > 384000
            || frames == 0
            || frames > self.limits.max_frame_samples as usize
            || (self.track.sample_rate != 0 && self.track.sample_rate != rate)
            || (self.track.channels != 0 && self.track.channels != channels)
        {
            return Err(invalid(
                "Native audio frame changed or exceeded its admitted format",
            ));
        }
        self.track.sample_rate = rate;
        self.track.channels = channels;
        let sample_bytes = frame.format().bytes();
        if sample_bytes == 0 || sample_bytes > 8 {
            return Err(MediaError::Unsupported(
                "Native audio sample format is unsupported".into(),
            ));
        }
        let resident = (frames as u64)
            .checked_mul(u64::from(channels))
            .and_then(|n| n.checked_mul(sample_bytes as u64 + 4))
            .and_then(|n| n.checked_add(65536))
            .ok_or_else(|| invalid("Native audio frame residency overflow"))?;
        if resident > self.limits.max_resident_bytes {
            return Err(MediaError::OutputLimit {
                limit_bytes: self.limits.max_resident_bytes,
            });
        }
        // SAFETY: the received frame is uniquely owned. Check the codec's
        // actual plane extent before constructing bounded typed plane slices.
        let plane_bytes = unsafe { (*frame.as_ptr()).linesize[0] };
        let wanted = frames
            .checked_mul(sample_bytes)
            .and_then(|n| {
                n.checked_mul(if frame.is_planar() {
                    1
                } else {
                    channels as usize
                })
            })
            .ok_or_else(|| invalid("Native audio plane size overflow"))?;
        if plane_bytes <= 0
            || (plane_bytes as usize) < wanted
            || plane_bytes as u64 > self.limits.max_resident_bytes
        {
            return Err(invalid("Native audio plane extent is invalid"));
        }
        let pts = frame.timestamp().or(frame.pts());
        if pts.zip(self.last_pts).is_some_and(|(now, old)| now < old) {
            return Err(invalid("Native audio PTS moved backwards"));
        }
        if let Some(pts) = pts {
            self.last_pts = Some(pts);
        }
        let mut output = Vec::with_capacity(channels as usize);
        for channel in 0..channels as usize {
            let data = frame.data(if frame.is_planar() { channel } else { 0 });
            let stride = if frame.is_planar() {
                1
            } else {
                channels as usize
            };
            let offset = if frame.is_planar() { 0 } else { channel };
            let mut values = Vec::new();
            values
                .try_reserve_exact(frames)
                .map_err(|_| invalid("Cannot allocate admitted native PCM frame"))?;
            for i in 0..frames {
                let at = (i * stride + offset) * sample_bytes;
                let sample = match frame.format().packed() {
                    av::format::Sample::U8(_) => (f64::from(data[at]) - 128.0) / 128.0,
                    av::format::Sample::I16(_) => {
                        f64::from(i16::from_ne_bytes(data[at..at + 2].try_into().unwrap()))
                            / 32768.0
                    }
                    av::format::Sample::I32(_) => {
                        f64::from(i32::from_ne_bytes(data[at..at + 4].try_into().unwrap()))
                            / 2147483648.0
                    }
                    av::format::Sample::I64(_) => {
                        i64::from_ne_bytes(data[at..at + 8].try_into().unwrap()) as f64
                            / 9223372036854775808.0
                    }
                    av::format::Sample::F32(_) => {
                        f64::from(f32::from_ne_bytes(data[at..at + 4].try_into().unwrap()))
                    }
                    av::format::Sample::F64(_) => {
                        f64::from_ne_bytes(data[at..at + 8].try_into().unwrap())
                    }
                    _ => {
                        return Err(MediaError::Unsupported(
                            "Native audio sample format is unsupported".into(),
                        ));
                    }
                };
                let sample = sample as f32;
                if !sample.is_finite() {
                    return Err(invalid("Native decoder returned non-finite PCM"));
                }
                values.push(sample);
            }
            output.push(values);
        }
        Ok(DecodedAudioFrame {
            pts,
            sample_rate: rate,
            channels: output,
            time_base_numerator: self.track.time_base_numerator,
            time_base_denominator: self.track.time_base_denominator,
        })
    }
    /// Seek in the selected stream's exact observed time base. The caller
    /// supplies execution-window facts; no authored timeline is inferred here.
    pub fn seek_pts(
        &mut self,
        source: &mut dyn ByteSource,
        pts: i64,
        deadline: Instant,
    ) -> MediaResult<()> {
        if self.retired {
            return Err(invalid("Native audio decoder was retired"));
        }
        let input = &mut self.input;
        let stream = self.track.stream_index as i32;
        let result = self.bridge.run(
            source,
            deadline,
            self.limits.max_io_bytes_per_operation,
            || {
                // SAFETY: only this decoder borrows the input, with a live AVIO scope.
                let code = unsafe {
                    ffi::av_seek_frame(input.as_mut_ptr(), stream, pts, ffi::AVSEEK_FLAG_BACKWARD)
                };
                if code < 0 {
                    return Err(failed(av::Error::from(code)));
                }
                Ok(())
            },
        );
        if result.is_err() {
            self.retired = true;
        }
        self.decoder.flush();
        self.draining = false;
        self.ended = false;
        self.last_pts = None;
        self.pending.clear();
        self.started = false;
        self.demux_eof = false;
        result
    }
}
