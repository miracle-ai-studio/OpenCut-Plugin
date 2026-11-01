use super::*;
use crate::classic_visual::VisualProcessSession;
use opencut_contracts::source_preview::{SourcePreviewMatrix, SourcePreviewUse};
use opencut_contracts::visual_frame::{
    DecodedVisualSource, VisualNoFrameReason, VisualVideoEofPolicy,
};
use opencut_editor_application::classic_visual::*;
use opencut_media::{
    MediaError, MediaTime,
    native_decode::{
        ByteSource, DecodeLimits, DecodedVideoFrame, NativeVideoDecoder, VideoOutputSize,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

struct CachedDecoder {
    decoder: NativeVideoDecoder,
    representation: Option<String>,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DecoderKey {
    element_id: String,
    source: (String, String, Option<u32>, bool),
}
pub struct NativeMediaDecoders {
    entries: BTreeMap<DecoderKey, CachedDecoder>,
}
pub struct MediaFrameSources {
    pub source_preparation: Vec<SourcePreviewUse>,
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
fn key(layer: &ClassicVisualLayerPlan) -> DecoderKey {
    let image = matches!(layer.decode, VisualDecodeRequest::Image);
    let stream = match layer.decode {
        VisualDecodeRequest::Video {
            selected_video_stream,
            ..
        } => selected_video_stream,
        VisualDecodeRequest::Image => None,
    };
    DecoderKey {
        element_id: if image {
            "image".into()
        } else {
            layer.element_id.clone()
        },
        source: (
            layer.resource.content_hash.clone(),
            layer.resource.size.clone(),
            stream,
            image,
        ),
    }
}

fn retain_active<T>(entries: &mut BTreeMap<DecoderKey, T>, active: &BTreeSet<DecoderKey>) {
    // A split changes the element identity, not the immutable source. Transfer
    // only an inactive owner; overlapping uses retain independent seek state.
    // Representation and current admission are checked after fresh resolution.
    for key in active {
        if entries.contains_key(key) {
            continue;
        }
        let previous = entries
            .keys()
            .find(|old| !active.contains(*old) && old.source == key.source)
            .cloned();
        if let Some(previous) = previous {
            let decoder = entries.remove(&previous).expect("inactive source owner");
            entries.insert(key.clone(), decoder);
        }
    }
    entries.retain(|key, _| active.contains(key));
}

impl NativeMediaDecoders {
    pub(crate) fn decode<R: ClassicVisualResources + ?Sized, S: VisualProcessSession<R>>(
        &mut self,
        session: &mut S,
        resources: &mut R,
        plan: &ClassicVisualPlan,
        handles: &[ClassicVisualResourceHandle],
        source_raster: SourceRaster,
        deadline: Instant,
    ) -> Result<MediaFrameSources, EditorError> {
        resources.checkpoint()?;
        let active: BTreeSet<_> = plan.layers.iter().map(key).collect();
        retain_active(&mut self.entries, &active);
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
            let mut source_preparation = Vec::new();
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
                let resolution = if matches!(source_raster, SourceRaster::Preview)
                    && matches!(layer.decode, VisualDecodeRequest::Video { .. })
                    && plan.identity.width.max(plan.identity.height) <= 960
                    && plan.identity.width.min(plan.identity.height) <= 540
                {
                    Some(
                        serde_json::from_value::<VisualPreviewSourceResolution>(callback(
                            session,
                            resources,
                            "resolvePreviewSource",
                            json!({"elementId":layer.element_id}),
                        )?)
                        .map_err(|_| invalid("Invalid typed preview source resolution"))?,
                    )
                } else {
                    None
                };
                if let Some(resolution) = &resolution {
                    source_preparation.push(resolution.source_preparation.clone());
                }
                let proxy = resolution.as_ref().and_then(|resolution| {
                    match &resolution.source_preparation {
                        SourcePreviewUse::Prepared { source, frame, .. } => {
                            Some((source.as_ref(), frame.as_ref()))
                        }
                        SourcePreviewUse::Original { .. } => None,
                    }
                });
                if matches!(proxy, Some((_, None))) {
                    let VisualDecodeRequest::Video { requested_time, .. } = &layer.decode else {
                        return Err(invalid("Prepared source requires video"));
                    };
                    self.entries.remove(&key(layer));
                    decoded.push(DecodedVisualSource::NoFrame {
                        element_id: layer.element_id.clone(),
                        requested_time: requested_time.clone(),
                        reason: VisualNoFrameReason::TimestampGap,
                    });
                    continue;
                }
                let proxy = proxy.map(|(source, frame)| {
                    (source, frame.expect("selected prepared source frame"))
                });
                let representation = proxy.map(|(source, _)| source.video.content_hash.clone());
                let resource_key =
                    if let Some(resolution) = resolution.as_ref().filter(|_| proxy.is_some()) {
                        let transfer = resolution
                            .transfer_id
                            .as_ref()
                            .ok_or_else(|| invalid("Prepared source omitted verified transfer"))?;
                        let size = resolution
                            .byte_length
                            .as_ref()
                            .ok_or_else(|| invalid("Prepared source omitted byte length"))?;
                        let key = format!("prepared:{}", layer.element_id);
                        opened.insert(key.clone(), (transfer.clone(), number(size)?));
                        key
                    } else {
                        layer.resource.asset_id.clone()
                    };
                let (transfer, size) = opened
                    .get(&resource_key)
                    .ok_or_else(|| invalid("Native layer has no verified source"))?;
                let mut source = Source {
                    resources,
                    transfer,
                    size: *size,
                };
                let mut limits = DecodeLimits {
                    max_pixels: number(&admission.max_pixels)?,
                    max_resident_bytes: number(&admission.max_decoded_bytes)?,
                    max_io_bytes_per_operation: 512 * 1024 * 1024,
                    max_packets_per_operation: 100_000,
                };
                if let Some((_, frame)) = proxy {
                    limits.max_resident_bytes = limits
                        .max_resident_bytes
                        .checked_sub(
                            u64::from(frame.source_video.raster_width)
                                * u64::from(frame.source_video.raster_height)
                                * 4,
                        )
                        .filter(|n| *n >= 65536)
                        .ok_or_else(|| {
                            invalid("Prepared source restoration exceeds decoded admission")
                        })?;
                }
                let key = key(layer);
                let reusable = if let Some(entry) = self.entries.get_mut(&key) {
                    entry.representation == representation
                        && entry.decoder.retain_within(limits).map_err(media_error)?
                } else {
                    false
                };
                if !reusable {
                    self.entries.remove(&key);
                }
                let new = !self.entries.contains_key(&key);
                if new {
                    let stream = if proxy.is_some() {
                        Some(0)
                    } else {
                        match &layer.decode {
                            VisualDecodeRequest::Video {
                                selected_video_stream,
                                ..
                            } => *selected_video_stream,
                            _ => None,
                        }
                    };
                    let decoder = NativeVideoDecoder::open(&mut source, stream, limits, deadline)
                        .map_err(media_error)?;
                    self.entries.insert(
                        key.clone(),
                        CachedDecoder {
                            decoder,
                            representation,
                        },
                    );
                }
                let decoder = &mut self
                    .entries
                    .get_mut(&key)
                    .expect("admitted native decoder")
                    .decoder;
                let before = if new { 0 } else { decoder.encoded_bytes_read() };
                let mut frame = match &layer.decode {
                    VisualDecodeRequest::Image => decoder
                        .first_image(&mut source, deadline)
                        .map_err(media_error)?,
                    VisualDecodeRequest::Video { requested_time, .. } => decoder
                        .sample_with_output(
                            &mut source,
                            if let Some((prepared, frame)) = proxy {
                                MediaTime {
                                    ticks: frame
                                        .proxy_pts
                                        .parse::<i64>()
                                        .ok()
                                        .and_then(|pts| {
                                            pts.checked_mul(i64::from(
                                                prepared.proxy_time_base.numerator,
                                            ))
                                        })
                                        .ok_or_else(|| invalid("Prepared source PTS overflow"))?,
                                    timebase: prepared.proxy_time_base.denominator,
                                }
                            } else {
                                MediaTime {
                                    ticks: requested_time
                                        .ticks
                                        .parse()
                                        .map_err(|_| invalid("Invalid fixed source time"))?,
                                    timebase: requested_time.timebase,
                                }
                            },
                            if proxy.is_some() {
                                VideoOutputSize::Source
                            } else {
                                match source_raster {
                                    SourceRaster::Original => VideoOutputSize::Source,
                                    SourceRaster::Preview => VideoOutputSize::Fit {
                                        width: plan.identity.width,
                                        height: plan.identity.height,
                                    },
                                }
                            },
                            deadline,
                        )
                        .map_err(media_error)?,
                };
                bytes_read = bytes_read.saturating_add(decoder.encoded_bytes_read() - before);
                let reached_eof =
                    proxy.map_or_else(|| decoder.reached_eof(), |(_, frame)| frame.last_frame);
                if let Some((prepared, selected)) = proxy {
                    let actual = frame
                        .take()
                        .ok_or_else(|| invalid("Prepared video has no actual indexed frame"))?;
                    if actual.pts.to_string() != selected.proxy_pts
                        || actual.duration.to_string() != selected.proxy_duration
                        || actual.time_base_numerator != prepared.proxy_time_base.numerator
                        || actual.time_base_denominator != prepared.proxy_time_base.denominator
                        || actual.source_width != prepared.identity.raster_bound.width
                        || actual.source_height != prepared.identity.raster_bound.height
                        || actual.width != prepared.identity.raster_bound.width
                        || actual.height != prepared.identity.raster_bound.height
                        || actual.rotation_degrees != 0
                    {
                        return Err(invalid(
                            "Actual prepared frame differs from independently indexed proxy clock/raster",
                        ));
                    }
                    let video = &selected.source_video;
                    let rgba = crate::source_preview::raster::restore(
                        &actual.rgba,
                        video,
                        prepared.identity.raster_bound,
                        &mut || source.resources.checkpoint(),
                    )?;
                    frame = Some(Arc::new(DecodedVideoFrame {
                        source_width: video.source_width,
                        source_height: video.source_height,
                        width: video.raster_width,
                        height: video.raster_height,
                        rgba,
                        pts: selected
                            .source_pts
                            .parse()
                            .map_err(|_| invalid("Invalid original source PTS"))?,
                        duration: selected
                            .source_duration
                            .parse()
                            .map_err(|_| invalid("Invalid original source duration"))?,
                        time_base_numerator: prepared.source_time_base.numerator,
                        time_base_denominator: prepared.source_time_base.denominator,
                        rotation_degrees: video.rotation_degrees,
                        matrix: match video.matrix {
                            SourcePreviewMatrix::Bt601 => {
                                opencut_media::native_decode::VideoColorMatrix::Bt601
                            }
                            SourcePreviewMatrix::Bt709 => {
                                opencut_media::native_decode::VideoColorMatrix::Bt709
                            }
                            SourcePreviewMatrix::Bt2020 => {
                                opencut_media::native_decode::VideoColorMatrix::Bt2020
                            }
                            SourcePreviewMatrix::Rgb => {
                                opencut_media::native_decode::VideoColorMatrix::Rgb
                            }
                            SourcePreviewMatrix::Unspecified => {
                                opencut_media::native_decode::VideoColorMatrix::Unspecified
                            }
                        },
                        full_range: video.full_range,
                        pixel_aspect_numerator: video.pixel_aspect_numerator,
                        pixel_aspect_denominator: video.pixel_aspect_denominator,
                        source_has_alpha: video.has_alpha,
                    }));
                }
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
                            && reached_eof
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
                                reason: if reached_eof {
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
                source_preparation,
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
        (frame.source_width, frame.source_height)
    } else {
        (frame.source_height, frame.source_width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn video(element: &str, source: &str, stream: Option<u32>) -> DecoderKey {
        DecoderKey {
            element_id: element.into(),
            source: (source.into(), "1024".into(), stream, false),
        }
    }

    #[test]
    fn split_handoff_preserves_one_owner_without_sharing_overlapping_uses() {
        let old = video("left", "source", None);
        let next = video("right", "source", None);
        let overlap = video("overlay", "source", None);
        let mut entries = BTreeMap::from([(old.clone(), 17)]);
        retain_active(&mut entries, &BTreeSet::from([old.clone(), overlap]));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.get(&old), Some(&17));
        retain_active(&mut entries, &BTreeSet::from([next.clone()]));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.get(&next), Some(&17));
    }

    #[test]
    fn source_version_stream_and_size_changes_do_not_inherit_decode_state() {
        let original = video("left", "source", None);
        let mut changed_size = video("right", "source", None);
        changed_size.source.1 = "1025".into();
        for next in [
            video("right", "changed", None),
            video("right", "source", Some(0)),
            changed_size,
        ] {
            let mut entries = BTreeMap::from([(original.clone(), 17)]);
            retain_active(&mut entries, &BTreeSet::from([next]));
            assert!(entries.is_empty());
        }
    }
}
