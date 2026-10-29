use super::*;
use opencut_composition::scene::*;
use opencut_contracts::AssetKind;
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub struct ClassicVisualSession<'a> {
    prepared: &'a PreparedVisualFrame,
    policy: LocalVisualExecutionPolicy,
    admissions: BTreeSet<String>,
    source_selections: BTreeSet<String>,
    bound: Option<BoundVisualFrame>,
    received_bytes: u64,
    rgba_bytes: u64,
    decoded_limit_pixels: u64,
    decoded_limit_bytes: u64,
}
fn budget(message: &str) -> EditorError {
    EditorError::new("VISUAL_BUDGET_EXCEEDED", message)
}
impl<'a> ClassicVisualSession<'a> {
    pub fn new(
        prepared: &'a PreparedVisualFrame,
        policy: &LocalVisualExecutionPolicy,
    ) -> Result<Self, EditorError> {
        crate::media_frame_jobs::validate_policy(policy)?;
        let i = prepared.identity();
        let pixels = u64::from(i.width)
            .checked_mul(u64::from(i.height))
            .ok_or_else(|| budget("Canvas pixel overflow"))?;
        let rgba_bytes = pixels
            .checked_mul(4)
            .ok_or_else(|| budget("RGBA overflow"))?;
        let padded_row = (u64::from(i.width) * 4).div_ceil(256) * 256;
        let readback = padded_row
            .checked_mul(u64::from(i.height))
            .ok_or_else(|| budget("Readback overflow"))?;
        // Eight full canvas textures cover scene/layer/blend/background/readback
        // staging work. Decode reserves are additional, never borrowed from it.
        let gpu_base = rgba_bytes
            .checked_mul(8)
            .and_then(|n| n.checked_add(readback))
            .ok_or_else(|| budget("GPU overflow"))?;
        // Custom AVIO streams every source. The file length is not a resident
        // allocation. Each demuxer reserves bounded probing and one AVIO block;
        // codec reference frames are admitted separately in bytes below.
        if prepared
            .resources()
            .iter()
            .any(|r| r.version.size == 0 || r.version.size > 9_007_199_254_740_991)
        {
            return Err(budget("Encoded resource cannot be addressed safely"));
        }
        let encoded_reserve = prepared.resources().iter().try_fold(0u64, |n, r| {
            n.checked_add(r.version.size.min(4 * 1024 * 1024) + 65536)
                .ok_or_else(|| budget("Encoded reserve overflow"))
        })?;
        if encoded_reserve > policy.max_encoded_bytes {
            return Err(budget("Native demux buffers exceed encoded admission"));
        }
        let gpu_remaining = policy
            .max_gpu_bytes
            .checked_sub(gpu_base)
            .ok_or_else(|| budget("Frame GPU budget exceeded"))?;
        let rgba_remaining = policy
            .max_rgba_bytes
            .checked_sub(rgba_bytes)
            .and_then(|n| n.checked_sub(encoded_reserve))
            .ok_or_else(|| budget("Frame RGBA budget exceeded"))?;
        // GPU admission covers the source texture and upload staging. CPU
        // decode bytes are a separate aggregate cap, not a guessed number of
        // browser canvases. The native decoder checks its actual format/refs.
        let decoded_limit_pixels = policy
            .max_decoded_pixels
            .min(gpu_remaining / 8)
            .min(rgba_remaining / 4);
        if pixels == 0
            || pixels > policy.max_frame_pixels
            || prepared.layers().len() > 60
            || prepared.resources().len() > 32
            || (decoded_limit_pixels == 0 && !prepared.layers().is_empty())
        {
            return Err(budget("Visual frame exceeds pixel/layer admission"));
        }
        Ok(Self {
            prepared,
            policy: policy.clone(),
            admissions: BTreeSet::new(),
            source_selections: BTreeSet::new(),
            bound: None,
            received_bytes: 0,
            rgba_bytes,
            decoded_limit_pixels,
            decoded_limit_bytes: rgba_remaining,
        })
    }
    pub fn prepared(&self) -> &PreparedVisualFrame {
        self.prepared
    }
    pub fn policy(&self) -> &LocalVisualExecutionPolicy {
        &self.policy
    }
    pub fn resource(&self, id: &str) -> Result<VisualResource, EditorError> {
        let r = self
            .prepared
            .resources()
            .iter()
            .find(|r| r.asset_id == id)
            .ok_or_else(|| invalid("Unknown visual resource"))?;
        let kind = match r.kind {
            AssetKind::Video => VisualResourceKind::Video,
            AssetKind::Image | AssetKind::Graphic => VisualResourceKind::Image,
            _ => return Err(invalid("Unsupported visual resource kind")),
        };
        Ok(VisualResource {
            asset_id: id.into(),
            content_hash: r.version.content_hash.clone(),
            size: r.version.size.to_string(),
            kind,
        })
    }
    pub fn typed_plan(&self) -> Result<ClassicVisualPlan, EditorError> {
        let mut layers = Vec::new();
        for layer in self.prepared.layers() {
            let (id, decode) = match &layer.content {
                SampledContent::Image { asset_id } => (asset_id, VisualDecodeRequest::Image),
                SampledContent::Video {
                    asset_id,
                    stream,
                    source,
                    ..
                } => {
                    let eof_policy =
                        video_eof_policy_scene(layer).map_err(|e| invalid(e.message))?;
                    let readable = source.sample == SourceReadState::InBounds
                        || matches!(eof_policy, VisualVideoEofPolicy::HoldLastFrame { .. });
                    (
                        asset_id,
                        VisualDecodeRequest::Video {
                            requested_time: source.source_time.clone(),
                            requested_time_seconds: {
                                let t = opencut_time::RationalTime::try_from(&source.source_time)?;
                                t.ticks() as f64 / f64::from(t.timebase())
                            },
                            point_profile: match source.point_profile {
                                SourcePointProfile::ClassicVideoRoundedTicks => {
                                    "classic_video_rounded_ticks"
                                }
                                SourcePointProfile::ExactRational => "exact_rational",
                            }
                            .into(),
                            selected_video_stream: *stream,
                            eof_policy,
                            no_frame_reason: if readable {
                                None
                            } else {
                                Some(VisualNoFrameReason::OutsideReadWindow)
                            },
                        },
                    )
                }
                _ => return Err(invalid("Preparation contains nonmedia visual node")),
            };
            layers.push(ClassicVisualLayerPlan {
                element_id: layer.element_id.clone(),
                resource: self.resource(id)?,
                decode,
            });
        }
        Ok(ClassicVisualPlan {
            identity: self.prepared.identity().clone(),
            resources: self
                .prepared
                .resources()
                .iter()
                .map(|r| self.resource(&r.asset_id))
                .collect::<Result<_, _>>()?,
            layers,
        })
    }
    pub fn plan(&self) -> Result<Value, EditorError> {
        serde_json::to_value(self.typed_plan()?)
            .map_err(|_| invalid("Cannot serialize visual plan"))
    }
    pub fn decode_admission(&self) -> Result<VisualDecodeAdmission, EditorError> {
        let max_pixels = self.decoded_limit_pixels / (self.prepared.layers().len() as u64).max(1);
        if max_pixels == 0 {
            return Err(budget("No decoded pixels available"));
        }
        Ok(VisualDecodeAdmission {
            max_width: 16384,
            max_height: 16384,
            max_pixels: max_pixels.to_string(),
            max_decoded_bytes: (self.decoded_limit_bytes
                / (self.prepared.layers().len() as u64).max(1))
            .to_string(),
        })
    }
    /// Reserve fixed-source size hints first, then divide the remaining space.
    /// Per-layer caps still sum to at most the original decoded limit. Missing
    /// or inconsistent metadata cannot enlarge the aggregate budget, and actual
    /// dimensions remain independently checked by the decoder and binding.
    pub fn decode_admission_for(
        &self,
        element_id: &str,
    ) -> Result<VisualDecodeAdmission, EditorError> {
        let mut admission = self.decode_admission()?;
        let mut total = 0u64;
        let mut requested = None;
        for layer in self.prepared.layers() {
            let asset_id = match &layer.content {
                SampledContent::Image { asset_id } | SampledContent::Video { asset_id, .. } => {
                    asset_id
                }
                _ => return Err(invalid("Decode admission requires a media layer")),
            };
            let pixels = self
                .prepared
                .resources()
                .iter()
                .find(|r| &r.asset_id == asset_id)
                .and_then(|r| r.metadata.intrinsic_size.as_ref())
                .map_or(0, |size| u64::from(size.width) * u64::from(size.height));
            total = total.saturating_add(pixels);
            if layer.element_id == element_id {
                requested = Some(pixels);
            }
        }
        let requested = requested.ok_or_else(|| invalid("Unknown decode admission layer"))?;
        // Oversized hints retain the prior conservative equal-share rejection;
        // they never claim space that the original frame policy did not grant.
        if total <= self.decoded_limit_pixels {
            let pixels = requested
                + (self.decoded_limit_pixels - total)
                    / (self.prepared.layers().len() as u64).max(1);
            if pixels == 0 {
                return Err(budget("No decoded pixels available for this layer"));
            }
            admission.max_pixels = pixels.to_string();
            admission.max_decoded_bytes = ((u128::from(self.decoded_limit_bytes)
                * u128::from(pixels))
                / u128::from(self.decoded_limit_pixels))
            .to_string();
        }
        Ok(admission)
    }
    pub fn bound(&self) -> Option<&BoundVisualFrame> {
        self.bound.as_ref()
    }
    /// Pure validation also used when projecting a persisted completion result.
    pub fn project_decoded_sources(
        &self,
        decoded: &[DecodedVisualSource],
    ) -> Result<BoundVisualFrame, EditorError> {
        for observation in decoded {
            if let DecodedVisualSource::ReadyImage {
                element_id,
                width,
                height,
            }
            | DecodedVisualSource::ReadyVideo {
                element_id,
                width,
                height,
                ..
            } = observation
                && (*width > 16384
                    || *height > 16384
                    || u64::from(*width) * u64::from(*height)
                        > decimal(&self.decode_admission_for(element_id)?.max_pixels)?)
            {
                return Err(budget("Decoded dimensions exceed prior admission"));
            }
        }
        bind_visual_sources_scene(self.prepared, decoded).map_err(|e| invalid(e.message))
    }
    pub fn callback(
        &mut self,
        request: VisualCallback,
        resources: &mut (impl ClassicVisualResources + ?Sized),
    ) -> Result<Value, EditorError> {
        resources.checkpoint()?;
        match request {
            VisualCallback::Resolve(v) => {
                if self.resource(&v.resource.asset_id)? != v.resource {
                    return Err(invalid("Resource differs from fixed revision"));
                }
                let (id, size) = resources.resolve(&v.resource, &v.handle)?;
                if size != decimal(&v.resource.size)? {
                    return Err(invalid("Verified resource size changed"));
                }
                Ok(
                    json!({"transferId":id,"byteLength":size.to_string(),"contentHash":v.resource.content_hash,"maxChunkBytes":65536}),
                )
            }
            VisualCallback::Read(v) => {
                if v.count == 0 || v.count > 65536 {
                    return Err(invalid("Invalid visual byte chunk"));
                }
                let (bytes, eof) = resources.read(&v.transfer_id, decimal(&v.offset)?, v.count)?;
                if bytes.len() > v.count as usize {
                    return Err(invalid("Resource port exceeded chunk"));
                }
                let hex = crate::browser_callback_encoding::bytes_hex(&bytes);
                Ok(json!({"offset":v.offset,"bytesHex":hex,"eof":eof}))
            }
            VisualCallback::Release(v) => {
                resources.release(&v.transfer_id)?;
                Ok(json!({"released":true}))
            }
            VisualCallback::Admit(v) => {
                if self.bound.is_some()
                    || !self
                        .prepared
                        .layers()
                        .iter()
                        .any(|l| l.element_id == v.element_id)
                    || !self.admissions.insert(v.element_id.clone())
                {
                    return Err(invalid(
                        "Decode admission is unknown, repeated or already bound",
                    ));
                }
                let admission = self.decode_admission_for(&v.element_id)?;
                serde_json::to_value(admission)
                    .map_err(|_| invalid("Cannot serialize visual admission"))
            }
            VisualCallback::Bind(v) => {
                if self.bound.is_some() || self.admissions.len() != self.prepared.layers().len() {
                    return Err(invalid(
                        "Frame must bind once after complete decode admission",
                    ));
                }
                let bound = self.project_decoded_sources(&v.decoded)?;
                let value = serde_json::to_value(&bound)
                    .map_err(|_| invalid("Cannot serialize bound frame"))?;
                self.bound = Some(bound);
                Ok(value)
            }
            VisualCallback::PreviewSource(v) => {
                if self.bound.is_some()
                    || !self.admissions.contains(&v.element_id)
                    || !self.source_selections.insert(v.element_id.clone())
                {
                    return Err(invalid(
                        "Preview source selection must follow one admitted layer",
                    ));
                }
                let layer = self
                    .prepared
                    .layers()
                    .iter()
                    .find(|layer| layer.element_id == v.element_id)
                    .ok_or_else(|| invalid("Unknown preview source layer"))?;
                let SampledContent::Video {
                    asset_id,
                    stream,
                    source,
                    ..
                } = &layer.content
                else {
                    return Err(invalid("Preview source requires a video layer"));
                };
                let resource = self
                    .prepared
                    .resources()
                    .iter()
                    .find(|resource| resource.asset_id == *asset_id)
                    .ok_or_else(|| invalid("Preview original source is missing"))?;
                let identity = self.prepared.identity();
                let request = VisualPreviewSourceRequest {
                    project_id: identity.project_id.clone(),
                    revision: identity.revision.clone(),
                    element_id: v.element_id,
                    asset_id: asset_id.clone(),
                    source_version: resource.version.clone(),
                    selected_video_stream: *stream,
                    requested_time: source.source_time.clone(),
                    raster_bound: crate::source_preview::SourcePreviewRaster {
                        width: identity.width,
                        height: identity.height,
                    },
                };
                crate::source_preview::validate_raster(request.raster_bound)?;
                let resolution = resources.preview_source(&request)?;
                crate::source_preview::validate_resolution(&request, &resolution)?;
                resources.checkpoint()?;
                serde_json::to_value(resolution)
                    .map_err(|_| invalid("Cannot serialize preview source selection"))
            }
        }
    }
    pub fn rgba_chunk(
        &mut self,
        offset: u64,
        bytes: &[u8],
        sink: &mut (impl ClassicVisualRgbaSink + ?Sized),
    ) -> Result<(), EditorError> {
        if self.bound.is_none()
            || offset != self.received_bytes
            || bytes.is_empty()
            || bytes.len() > 65536
            || !bytes.len().is_multiple_of(4)
        {
            return Err(invalid(
                "RGBA chunks must follow bound frame in sequential pixel order",
            ));
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("RGBA offset overflow"))?;
        if end > self.rgba_bytes {
            return Err(invalid("RGBA chunk exceeds frame"));
        }
        sink.chunk(offset, bytes)?;
        self.received_bytes = end;
        Ok(())
    }
    pub fn validate_receipt(
        &self,
        receipt: &ClassicVisualRenderReceipt,
    ) -> Result<(), EditorError> {
        if receipt.identity != *self.prepared.identity()
            || self.bound.as_ref() != Some(&receipt.bound)
            || self.received_bytes != self.rgba_bytes
            || decimal(&receipt.rgba_bytes)? != self.rgba_bytes
            || decimal(&receipt.peak_accounted_bytes)? > self.policy.max_rgba_bytes
            || receipt.engine_identity != CLASSIC_VISUAL_PROFILE
        {
            return Err(invalid(
                "Receipt differs from complete fixed frame execution",
            ));
        }
        Ok(())
    }
}
