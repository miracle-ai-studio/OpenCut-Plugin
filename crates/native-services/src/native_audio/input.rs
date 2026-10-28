//! One verified range descriptor and native decoder owner for either consumer.
//! Every operation borrows current application authorization; no host path is
//! exposed to the decoder and no grant is cached across operations.
use super::{
    AudioSession, callback,
    dsp::*,
    invalid, media_error,
    source::{self, DecodeConfig, Source},
};
use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::*;
use opencut_media::native_decode::*;
use serde_json::json;
type Result<T> = std::result::Result<T, EditorError>;

pub(super) struct NativeInput {
    decoder: Option<NativeAudioDecoder>,
    reservation: Option<PcmReservation>,
    transfer: String,
    pub resource: AudioResource,
    pub track: NativeAudioTrack,
    pub primary: ClassicAudioPrimarySelection,
    preview_pending: Option<(DecodedAudioFrame, usize)>,
}
impl NativeInput {
    pub fn open<R: ClassicAudioResources + ?Sized, S: AudioSession<R>>(
        session: &mut S,
        resources: &mut R,
        resource: &AudioResource,
        handle: &str,
        config: &DecodeConfig,
        memory: &PcmMemory,
        codec_bytes: u64,
    ) -> Result<Self> {
        source::check(resources, config.deadline)?;
        if codec_bytes < 65536 || codec_bytes > config.max_pcm_bytes {
            return Err(invalid("Native codec has no admitted frame workspace"));
        }
        let opened = callback(
            session,
            resources,
            "openVerifiedRange",
            json!({"resource":resource,"handle":handle}),
        )?;
        let transfer = opened["transferId"]
            .as_str()
            .ok_or_else(|| invalid("Missing native audio transfer"))?;
        let result = (|| {
            if opened["byteLength"].as_str() != Some(&resource.size.to_string())
                || opened["contentHash"].as_str() != Some(&resource.content_hash)
                || opened["maxChunkBytes"].as_u64() != Some(65536)
            {
                return Err(invalid(
                    "Native audio descriptor differs from the fixed source",
                ));
            }
            let reservation = memory.reserve(codec_bytes)?;
            let decoder = NativeAudioDecoder::open(
                &mut Source {
                    resources,
                    transfer,
                    size: resource.size,
                },
                None,
                AudioDecodeLimits {
                    max_channels: 64,
                    max_frame_samples: 65536,
                    max_resident_bytes: codec_bytes,
                    max_io_bytes_per_operation: config.max_encoded_bytes.max(65536),
                    max_packets_per_operation: 100000,
                },
                config.deadline,
            )
            .map_err(media_error)?;
            let track = decoder.track().clone();
            if track.sample_rate == 0 || track.channels == 0 {
                return Err(EditorError::new(
                    "UNSUPPORTED_AUDIO_CAPABILITY",
                    "Native primary format is unknown before PCM admission",
                ));
            }
            let primary = ClassicAudioPrimarySelection {
                track_id: u32::try_from(track.track_id)
                    .map_err(|_| invalid("Native audio track has no nonnegative identity"))?,
                codec: match track.codec.as_str() {
                    "pcm_s16le" | "pcm_s16be" => "pcm-s16",
                    "pcm_f32le" | "pcm_f32be" => "pcm-f32",
                    other => other,
                }
                .into(),
                sample_rate: track.sample_rate,
                channels: track.channels,
            };
            source::check(resources, config.deadline)?;
            Ok(Self {
                decoder: Some(decoder),
                reservation: Some(reservation),
                transfer: transfer.into(),
                resource: resource.clone(),
                track,
                primary,
                preview_pending: None,
            })
        })();
        if result.is_err() {
            let _ = resources.release(transfer);
        }
        result
    }
    fn decoder(&mut self) -> Result<&mut NativeAudioDecoder> {
        self.decoder
            .as_mut()
            .ok_or_else(|| invalid("Native audio decoder was released"))
    }
    pub fn select_origin<R: ClassicAudioResources + ?Sized>(
        &mut self,
        resources: &mut R,
        seconds: f64,
        config: &DecodeConfig,
    ) -> Result<()> {
        let (decoder, transfer, size) = (
            self.decoder
                .as_mut()
                .ok_or_else(|| invalid("Native audio decoder was released"))?,
            self.transfer.as_str(),
            self.resource.size,
        );
        decoder
            .select_keyframe_before_seconds(
                &mut Source {
                    resources,
                    transfer,
                    size,
                },
                seconds,
                config.deadline,
            )
            .map_err(media_error)
    }
    pub fn restart<R: ClassicAudioResources + ?Sized>(
        &mut self,
        resources: &mut R,
        seconds: f64,
        config: &DecodeConfig,
    ) -> Result<()> {
        self.preview_pending = None;
        // The retained Wave consumer emits 2048-frame PCM packets independent
        // of libav's byte-sized demux packets. Preserve those observed segment
        // boundaries: ordinary interpolation clamps at a buffer's final sample.
        // This only selects a physical decoder origin, never an authored clock.
        let wave_pts = if self.wave_pcm() {
            if u64::from(self.track.sample_rate) * u64::from(self.track.time_base_numerator)
                != u64::from(self.track.time_base_denominator)
            {
                return Err(invalid(
                    "Native Wave seek has no exact per-sample time base",
                ));
            }
            let rate = f64::from(self.track.sample_rate);
            let mut packet = (seconds * rate / 2048.0).floor();
            if let Some(duration) = self.track.duration_pts {
                let frames = u128::from(duration as u64)
                    * u128::from(self.track.time_base_numerator)
                    * u128::from(self.track.sample_rate)
                    / u128::from(self.track.time_base_denominator);
                if frames > 0 {
                    packet = packet.min(((frames - 1) / 2048) as f64);
                }
            }
            let frames = packet * 2048.0;
            if !frames.is_finite() || frames < 0.0 || frames >= i64::MAX as f64 {
                return Err(invalid(
                    "Native Wave packet origin exceeds its exact stream range",
                ));
            }
            Some(frames as i64)
        } else {
            None
        };
        let ticks = wave_pts.map_or_else(
            || {
                seconds * f64::from(self.track.time_base_denominator)
                    / f64::from(self.track.time_base_numerator)
            },
            |pts| pts as f64,
        );
        if !ticks.is_finite() || ticks < 0.0 || ticks >= i64::MAX as f64 {
            return Err(invalid(
                "Native observed audio seek exceeds its stream time base",
            ));
        }
        let (decoder, transfer, size) = (
            self.decoder
                .as_mut()
                .ok_or_else(|| invalid("Native audio decoder was released"))?,
            self.transfer.as_str(),
            self.resource.size,
        );
        decoder
            .seek_pts(
                &mut Source {
                    resources,
                    transfer,
                    size,
                },
                ticks.floor() as i64,
                config.deadline,
            )
            .map_err(media_error)?;
        if let Some(pts) = wave_pts {
            // An exact Wave sample index must not round through seconds and
            // floor to the preceding sample at, for example, 30720/44100.
            decoder
                .select_keyframe_at_pts(
                    &mut Source {
                        resources,
                        transfer,
                        size,
                    },
                    pts,
                    config.deadline,
                )
                .map_err(media_error)
        } else {
            self.select_origin(resources, seconds, config)
        }
    }
    fn wave_pcm(&self) -> bool {
        self.track.container == "wav" && self.track.codec.starts_with("pcm_")
    }
    pub fn read_preview<R: ClassicAudioResources + ?Sized>(
        &mut self,
        resources: &mut R,
        config: &DecodeConfig,
    ) -> Result<Option<DecodedAudioFrame>> {
        if !self.wave_pcm() {
            return self.read(resources, config);
        }
        let mut output: Option<DecodedAudioFrame> = None;
        while output.as_ref().map_or(0, DecodedAudioFrame::frames) < 2048 {
            if self.preview_pending.is_none() {
                let Some(frame) = self.read(resources, config)? else {
                    break;
                };
                self.preview_pending = Some((frame, 0));
            }
            let (frame, offset) = self.preview_pending.as_mut().unwrap();
            let pts = frame
                .pts
                .ok_or_else(|| invalid("Native Wave PCM has no observed timestamp"))?;
            if u64::from(frame.time_base_numerator) * u64::from(frame.sample_rate)
                != u64::from(frame.time_base_denominator)
            {
                return Err(invalid("Native Wave PCM has no exact per-sample time base"));
            }
            let first_pts = pts
                .checked_add(*offset as i64)
                .ok_or_else(|| invalid("Native Wave packet timestamp overflow"))?;
            let current = output.get_or_insert_with(|| DecodedAudioFrame {
                pts: Some(first_pts),
                sample_rate: frame.sample_rate,
                channels: (0..frame.channels.len())
                    .map(|_| Vec::with_capacity(2048))
                    .collect(),
                time_base_numerator: frame.time_base_numerator,
                time_base_denominator: frame.time_base_denominator,
            });
            let len = current.frames();
            let count = (2048 - len).min(frame.frames() - *offset);
            if pts.checked_add(*offset as i64)
                != current.pts.and_then(|p| p.checked_add(len as i64))
            {
                return Err(invalid(
                    "Native Wave PCM packet assembly changed its observed continuity",
                ));
            }
            for (channel, values) in current.channels.iter_mut().zip(&frame.channels) {
                channel.extend_from_slice(&values[*offset..*offset + count]);
            }
            *offset += count;
            if *offset == frame.frames() {
                self.preview_pending = None;
            }
        }
        Ok(output)
    }
    pub fn read<R: ClassicAudioResources + ?Sized>(
        &mut self,
        resources: &mut R,
        config: &DecodeConfig,
    ) -> Result<Option<DecodedAudioFrame>> {
        source::check(resources, config.deadline)?;
        let (decoder, transfer, size) = (
            self.decoder
                .as_mut()
                .ok_or_else(|| invalid("Native audio decoder was released"))?,
            self.transfer.as_str(),
            self.resource.size,
        );
        let frame = decoder
            .read(
                &mut Source {
                    resources,
                    transfer,
                    size,
                },
                config.deadline,
            )
            .map_err(media_error)?;
        if frame.as_ref().is_some_and(|f| {
            f.sample_rate != self.track.sample_rate
                || f.channels.len() != self.track.channels as usize
        }) {
            return Err(invalid("Native decoded primary format changed"));
        }
        Ok(frame)
    }
    pub fn reached_eof(&mut self) -> Result<bool> {
        Ok(self.decoder()?.reached_eof())
    }
    pub fn retire_decoder(&mut self) {
        self.preview_pending = None;
        self.decoder.take();
        self.reservation.take();
    }
    pub fn close<R: ClassicAudioResources + ?Sized>(mut self, resources: &mut R) -> Result<()> {
        self.retire_decoder();
        resources.release(&self.transfer)
    }
}
