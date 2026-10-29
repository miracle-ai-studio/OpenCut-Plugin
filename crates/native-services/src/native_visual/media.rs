use super::*;
use crate::classic_visual::VisualProcessSession;
use opencut_contracts::visual_frame::{
    DecodedVisualSource, VisualNoFrameReason, VisualVideoEofPolicy,
};
use opencut_editor_application::classic_visual::*;
use opencut_media::{
    MediaError, MediaTime,
    native_decode::{ByteSource, DecodeLimits, DecodedVideoFrame, NativeVideoDecoder},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

struct CachedDecoder {
    decoder: NativeVideoDecoder,
    limits: DecodeLimits,
}
pub struct NativeMediaDecoders {
    entries: BTreeMap<String, CachedDecoder>,
}
pub struct MediaFrameSources {
    pub decoded: Vec<DecodedVisualSource>,
    pub sources: BTreeMap<String, Arc<DecodedVideoFrame>>,
    pub encoded_bytes_read: u64,
}
impl Default for NativeMediaDecoders {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

/// Borrows the current execution port on every operation, including warm
/// AVIO reads. No descriptor path or old frame's authorization is retained.
pub(crate) struct Source<'a, R: ClassicVisualResources + ?Sized> {
    pub resources: &'a mut R,
    pub transfer: &'a str,
    pub size: u64,
}
impl<R: ClassicVisualResources + ?Sized> ByteSource for Source<'_, R> {
    fn len(&self) -> u64 {
        self.size
    }
    fn checkpoint(&mut self) -> Result<(), MediaError> {
        self.resources.checkpoint().map_err(port_error)
    }
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, MediaError> {
        self.checkpoint()?;
        if output.is_empty() {
            return Ok(0);
        }
        let count = output.len().min(65536) as u32;
        let (bytes, eof) = self
            .resources
            .read(self.transfer, offset, count)
            .map_err(port_error)?;
        if bytes.len() > count as usize
            || offset
                .checked_add(bytes.len() as u64)
                .is_none_or(|end| end > self.size)
            || eof != (offset + bytes.len() as u64 == self.size)
        {
            return Err(MediaError::InvalidRequest(
                "Verified source returned an invalid range".into(),
            ));
        }
        output[..bytes.len()].copy_from_slice(&bytes);
        self.checkpoint()?;
        Ok(bytes.len())
    }
}
fn port_error(error: EditorError) -> MediaError {
    MediaError::Admission {
        code: error.code,
        message: error.message,
    }
}
pub(crate) fn callback<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
    session: &mut S,
    resources: &mut R,
    name: &str,
    args: Value,
) -> Result<Value, EditorError> {
    let request = serde_json::from_value(json!({"op":name,"args":args}))
        .map_err(|_| invalid("Native execution callback differs from its typed session"))?;
    session.callback(request, resources)
}
pub(crate) fn callbacks<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
    session: &mut S,
    resources: &mut R,
    requests: Vec<(&str, Value)>,
    accept: &mut dyn FnMut(Value) -> Result<(), EditorError>,
) -> Result<(), EditorError> {
    let requests = requests
        .into_iter()
        .map(|(name, args)| {
            serde_json::from_value(json!({"op":name,"args":args}))
                .map_err(|_| invalid("Native execution callback differs from its typed session"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    session.callbacks(requests, resources, accept)
}
fn key(layer: &ClassicVisualLayerPlan) -> String {
    let stream = match layer.decode {
        VisualDecodeRequest::Video {
            selected_video_stream,
            ..
        } => selected_video_stream.map_or_else(|| "primary".into(), |n| n.to_string()),
        VisualDecodeRequest::Image => "image".into(),
    };
    format!(
        "{}:{}:{}:{}",
        if matches!(layer.decode, VisualDecodeRequest::Image) {
            "image"
        } else {
            &layer.element_id
        },
        layer.resource.content_hash,
        layer.resource.size,
        stream
    )
}

impl NativeMediaDecoders {
    pub(crate) fn decode<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
        &mut self,
        session: &mut S,
        resources: &mut R,
        plan: &ClassicVisualPlan,
        handles: &[ClassicVisualResourceHandle],
        deadline: Instant,
    ) -> Result<MediaFrameSources, EditorError> {
        resources.checkpoint()?;
        let active: BTreeSet<_> = plan.layers.iter().map(key).collect();
        self.entries.retain(|key, _| active.contains(key));
        let mut opened = BTreeMap::new();
        let result = (|| {
            let mut requests = Vec::new();
            for resource in &plan.resources {
                let handle = handles
                    .iter()
                    .find(|h| h.asset_id == resource.asset_id)
                    .ok_or_else(|| invalid("Native source handle is missing"))?;
                requests.push((
                    "resolveVerifiedBytes",
                    json!({"resource":resource,"handle":handle.handle}),
                ));
            }
            for layer in &plan.layers {
                requests.push(("admitDecode", json!({"elementId":layer.element_id})));
            }
            let mut responses = 0;
            let mut admissions = Vec::with_capacity(plan.layers.len());
            if !requests.is_empty() {
                callbacks(session, resources, requests, &mut |value| {
                    let index = responses;
                    responses += 1;
                    if index >= plan.resources.len() {
                        admissions.push(
                            serde_json::from_value::<VisualDecodeAdmission>(value)
                                .map_err(|_| invalid("Invalid native decode admission"))?,
                        );
                        return Ok(());
                    }
                    let resource = &plan.resources[index];
                    let transfer = value
                        .get("transferId")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("Missing verified native transfer"))?
                        .to_owned();
                    let length = value
                        .get("byteLength")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("Missing verified native length"))?;
                    if length != resource.size {
                        return Err(invalid("Verified native source changed length"));
                    }
                    opened.insert(resource.asset_id.clone(), (transfer, number(length)?));
                    Ok(())
                })?;
            }
            if responses != plan.resources.len() + plan.layers.len() {
                return Err(invalid("Native media callback results changed length"));
            }
            let mut sources = BTreeMap::new();
            let mut decoded = Vec::new();
            let mut bytes_read = 0u64;
            for (layer, admission) in plan.layers.iter().zip(admissions) {
                resources.checkpoint()?;
                if let VisualDecodeRequest::Video {
                    requested_time,
                    no_frame_reason: Some(reason),
                    ..
                } = &layer.decode
                {
                    decoded.push(DecodedVisualSource::NoFrame {
                        element_id: layer.element_id.clone(),
                        requested_time: requested_time.clone(),
                        reason: *reason,
                    });
                    continue;
                }
                let (transfer, size) = opened
                    .get(&layer.resource.asset_id)
                    .ok_or_else(|| invalid("Native layer has no verified source"))?;
                let mut source = Source {
                    resources,
                    transfer,
                    size: *size,
                };
                let limits = DecodeLimits {
                    max_pixels: number(&admission.max_pixels)?,
                    max_resident_bytes: number(&admission.max_decoded_bytes)?,
                    max_io_bytes_per_operation: 512 * 1024 * 1024,
                    max_packets_per_operation: 100_000,
                };
                let key = key(layer);
                if self
                    .entries
                    .get(&key)
                    .is_some_and(|entry| entry.limits != limits)
                {
                    self.entries.remove(&key);
                }
                let new = !self.entries.contains_key(&key);
                if new {
                    let stream = match &layer.decode {
                        VisualDecodeRequest::Video {
                            selected_video_stream,
                            ..
                        } => *selected_video_stream,
                        _ => None,
                    };
                    let decoder = NativeVideoDecoder::open(&mut source, stream, limits, deadline)
                        .map_err(media_error)?;
                    self.entries
                        .insert(key.clone(), CachedDecoder { decoder, limits });
                }
                let decoder = &mut self
                    .entries
                    .get_mut(&key)
                    .expect("admitted native decoder")
                    .decoder;
                let before = if new { 0 } else { decoder.encoded_bytes_read() };
                let frame = match &layer.decode {
                    VisualDecodeRequest::Image => decoder
                        .first_image(&mut source, deadline)
                        .map_err(media_error)?,
                    VisualDecodeRequest::Video { requested_time, .. } => decoder
                        .sample(
                            &mut source,
                            MediaTime {
                                ticks: requested_time
                                    .ticks
                                    .parse()
                                    .map_err(|_| invalid("Invalid fixed source time"))?,
                                timebase: requested_time.timebase,
                            },
                            deadline,
                        )
                        .map_err(media_error)?,
                };
                bytes_read = bytes_read.saturating_add(decoder.encoded_bytes_read() - before);
                match (&layer.decode, frame) {
                    (VisualDecodeRequest::Image, Some(frame)) => {
                        let (width, height) = display_size(&frame);
                        decoded.push(DecodedVisualSource::ReadyImage {
                            element_id: layer.element_id.clone(),
                            width,
                            height,
                        });
                        sources.insert(layer.element_id.clone(), frame);
                    }
                    (VisualDecodeRequest::Image, None) => {
                        return Err(invalid("Native image did not decode a frame"));
                    }
                    (
                        VisualDecodeRequest::Video {
                            requested_time,
                            requested_time_seconds,
                            selected_video_stream,
                            eof_policy,
                            ..
                        },
                        frame,
                    ) => {
                        let request = MediaTime {
                            ticks: requested_time
                                .ticks
                                .parse()
                                .map_err(|_| invalid("Invalid fixed source time"))?,
                            timebase: requested_time.timebase,
                        };
                        let expired = frame
                            .as_ref()
                            .is_none_or(|frame| frame.ends_before(request));
                        let hold = expired
                            && decoder.reached_eof()
                            && matches!(eof_policy,VisualVideoEofPolicy::HoldLastFrame{read_window_start_seconds,read_window_end_seconds}
                            if requested_time_seconds>=read_window_start_seconds&&requested_time_seconds<read_window_end_seconds);
                        if let Some(frame) = frame.filter(|_| !expired || hold) {
                            let (width, height) = display_size(&frame);
                            decoded.push(DecodedVisualSource::ReadyVideo {
                                element_id: layer.element_id.clone(),
                                width,
                                height,
                                requested_time: requested_time.clone(),
                                selected_video_stream: *selected_video_stream,
                                actual_timestamp_seconds: frame.timestamp_seconds(),
                                frame_duration_seconds: frame.duration_seconds(),
                                eof_held: hold,
                            });
                            sources.insert(layer.element_id.clone(), frame);
                        } else {
                            decoded.push(DecodedVisualSource::NoFrame {
                                element_id: layer.element_id.clone(),
                                requested_time: requested_time.clone(),
                                reason: if decoder.reached_eof() {
                                    VisualNoFrameReason::Eof
                                } else {
                                    VisualNoFrameReason::TimestampGap
                                },
                            });
                        }
                    }
                }
            }
            Ok(MediaFrameSources {
                decoded,
                sources,
                encoded_bytes_read: bytes_read,
            })
        })();
        let mut cleanup = None;
        for (transfer, _) in opened.values() {
            if let Err(error) = resources.release(transfer) {
                if cleanup.is_none() {
                    cleanup = Some(error);
                }
            }
        }
        if result.is_err() || cleanup.is_some() {
            self.entries.clear();
        }
        let result = result?;
        if let Some(error) = cleanup {
            return Err(error);
        }
        resources.checkpoint()?;
        Ok(result)
    }
}
fn display_size(frame: &DecodedVideoFrame) -> (u32, u32) {
    if frame.rotation_degrees % 180 == 0 {
        (frame.width, frame.height)
    } else {
        (frame.height, frame.width)
    }
}
