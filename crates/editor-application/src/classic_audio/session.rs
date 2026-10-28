use super::*;
use opencut_composition::scene::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[path = "decoder_windows.rs"]
mod decoder_windows;

/// Primary and prepared observations belong to one fixed preparation. Ordinary
/// PCM windows may change only for the captured Rust-planned consumer command;
/// callbacks cannot replace authored source clocks, gains or revision.
pub struct ClassicAudioSession<'a> {
    prepared: &'a PreparedAudio,
    budget: ClassicAudioPreparationBudget,
    observations: BTreeMap<String, (PitchRequest, ClassicAudioPreparationFacts)>,
    retired_decoded: BTreeSet<String>,
    // A mixed preview resource still owns its ordinary rolling decoder after
    // the independent preparation PCM has served every prepared voice.
    retired_preview_preparation: BTreeSet<String>,
    compiled_output_until: u64,
    max_encoded_bytes: u64,
    max_pcm_bytes: u64,
    admissions: BTreeMap<String, (u64, u32)>,
    primary: BTreeMap<String, Option<ClassicAudioPrimarySelection>>,
    window_admissions: BTreeMap<String, WindowDecodeState>,
    rolling_admissions: BTreeMap<String, RollingPreviewAdmission>,
    range: Option<ClassicAudioRange>,
    preview: Option<&'a PreparedPreviewAudio>,
    preview_bound: Option<BoundPreviewAudio<'a>>,
    preview_window: Option<PreviewBlockRequest>,
    planned_preview_window: Option<PreviewBlockRequest>,
    preview_anchor: u64,
    preview_gain: Option<PreviewGainOverride>,
    preview_decoded: BTreeMap<String, PreviewDecodedObservation>,
    preview_resident_frames: BTreeMap<String, u64>,
}
struct WindowDecodeState {
    reply: WindowDecodeAdmission,
    resident_frame_limit: u64,
    resident_frames: Option<u64>,
}
impl<'a> ClassicAudioSession<'a> {
    pub fn new(
        prepared: &'a PreparedAudio,
        max_pcm_bytes: u64,
        max_encoded_bytes: u64,
    ) -> Result<Self, EditorError> {
        Self::new_with_range(prepared, max_pcm_bytes, max_encoded_bytes, None)
    }
    pub fn new_with_range(
        prepared: &'a PreparedAudio,
        max_pcm_bytes: u64,
        max_encoded_bytes: u64,
        range: Option<ClassicAudioRange>,
    ) -> Result<Self, EditorError> {
        Self::new_inner(prepared, max_pcm_bytes, max_encoded_bytes, range, None)
    }
    pub fn new_preview(
        prepared: &'a PreparedPreviewAudio,
        max_pcm_bytes: u64,
        max_encoded_bytes: u64,
    ) -> Result<Self, EditorError> {
        Self::new_inner(
            prepared.audio(),
            max_pcm_bytes,
            max_encoded_bytes,
            None,
            Some(prepared),
        )
    }
    fn range_cache_bytes(&self) -> u64 {
        let resources = self
            .prepared
            .resources()
            .map(|r| r.asset_id.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            .max(1) as u64;
        ((self.max_encoded_bytes / resources).saturating_sub(6 * 65536) / 3).min(8 * 1024 * 1024)
    }
    fn encoded_reservation_bytes(&self, _encoded_size: u64) -> u64 {
        let cache = self.range_cache_bytes();
        self.prepared
            .resources()
            .map(|resource| (resource.asset_id.as_str(), &resource.version))
            .collect::<BTreeMap<_, _>>()
            .values()
            .map(|version| {
                version
                    .size
                    .min(cache)
                    .saturating_mul(3)
                    .saturating_add(6 * 65536)
            })
            .fold(0, u64::saturating_add)
    }
    fn new_inner(
        prepared: &'a PreparedAudio,
        max_pcm_bytes: u64,
        max_encoded_bytes: u64,
        range: Option<ClassicAudioRange>,
        preview: Option<&'a PreparedPreviewAudio>,
    ) -> Result<Self, EditorError> {
        let range_frames = if let Some(range) = &range {
            let start = decimal(&range.start_sample)?;
            let count = decimal(&range.frame_count)?;
            if count == 0
                || start
                    .checked_add(count)
                    .is_none_or(|end| end > prepared.duration_samples())
            {
                return Err(invalid("Output range is outside the fixed whole scene"));
            }
            count
        } else {
            0
        };
        let identity = prepared.identity();
        if identity.profile != AudioExecutionProfile::ClassicV31
            || identity.format.channels != 2
            || prepared.voices().count() > MAX_PREPARED_AUDIO_VOICES
            || prepared
                .resources()
                .map(|resource| &resource.asset_id)
                .collect::<BTreeSet<_>>()
                .len()
                > 32
            || prepared.duration_samples() == 0
            || max_pcm_bytes == 0
            || max_encoded_bytes == 0
            || max_encoded_bytes > 32 * 1024 * 1024
            || (preview.is_some() && max_pcm_bytes > 256 * 1024 * 1024)
        {
            return Err(invalid(
                "Classic browser execution requires a bounded stereo Classic preparation",
            ));
        }
        let max_frames = max_pcm_bytes / 4;
        let output_bytes = if preview.is_some() {
            crate::audio_preview::AUDIO_PREVIEW_MAX_BUFFERED_FRAMES.checked_mul(8)
        } else {
            prepared.duration_samples().checked_mul(8)
        };
        if output_bytes
            .and_then(|bytes| {
                range_frames
                    .checked_mul(8)
                    .and_then(|crop| bytes.checked_add(crop))
            })
            .is_none_or(|bytes| bytes > max_pcm_bytes)
        {
            return Err(EditorError::new(
                "AUDIO_BUDGET_EXCEEDED",
                "Whole scene PCM exceeds admission",
            ));
        }
        for voice in prepared.voices() {
            if voice.stream.is_some() {
                return Err(EditorError::new(
                    "UNSUPPORTED_AUDIO_CAPABILITY",
                    "Classic browser adapter requires an unspecified stream and selects the primary audio track",
                ));
            }
            if voice.resource.version.size == 0
                || voice.resource.version.size > 9_007_199_254_740_991
            {
                return Err(EditorError::new(
                    "AUDIO_BUDGET_EXCEEDED",
                    "Encoded resource exceeds admission",
                ));
            }
        }
        Ok(Self {
            prepared,
            max_encoded_bytes,
            max_pcm_bytes,
            admissions: BTreeMap::new(),
            primary: BTreeMap::new(),
            window_admissions: BTreeMap::new(),
            rolling_admissions: BTreeMap::new(),
            range,
            preview,
            preview_bound: None,
            preview_window: None,
            planned_preview_window: None,
            preview_anchor: 0,
            preview_gain: None,
            preview_decoded: BTreeMap::new(),
            preview_resident_frames: BTreeMap::new(),
            observations: BTreeMap::new(),
            retired_decoded: BTreeSet::new(),
            retired_preview_preparation: BTreeSet::new(),
            compiled_output_until: 0,
            budget: ClassicAudioPreparationBudget {
                max_source_frames: max_frames,
                max_output_frames: max_frames,
                max_total_channel_frames: max_frames,
            },
        })
    }
    pub fn output_frame_count(&self) -> u64 {
        self.range
            .as_ref()
            .map(|r| decimal(&r.frame_count).expect("validated range"))
            .unwrap_or_else(|| self.prepared.duration_samples())
    }
    pub fn preview_prepared(&self) -> Option<&PreparedPreviewAudio> {
        self.preview
    }
    pub fn validate_temporary_gain(
        &self,
        track_id: &str,
        element_id: &str,
        tenths: i32,
    ) -> Result<PreviewGainOverride, EditorError> {
        self.preview
            .ok_or_else(|| invalid("Temporary gain requires a realtime session"))?
            .validate_temporary_gain(track_id, element_id, tenths)
            .map_err(composition)
    }
    /// Trusted control plane supplies the generation anchor/validated overlay;
    /// browser callbacks cannot choose either clock or gain.
    pub fn set_preview_state(
        &mut self,
        anchor: u64,
        gain: Option<PreviewGainOverride>,
    ) -> Result<(), EditorError> {
        let preview = self
            .preview
            .ok_or_else(|| invalid("Not a realtime session"))?;
        if anchor > preview.extent() {
            return Err(invalid("Preview anchor is outside the fixed scene"));
        }
        if let Some(g) = &gain {
            preview
                .validate_temporary_gain(&g.track_id, &g.element_id, g.decibels_tenths)
                .map_err(composition)?;
        }
        if self.preview_anchor != anchor || self.preview_gain != gain {
            self.preview_window = None;
            self.planned_preview_window = None;
        }
        self.preview_anchor = anchor;
        self.preview_gain = gain;
        Ok(())
    }
    pub fn resource(&self, id: &str) -> Result<AudioResource, EditorError> {
        let r = self
            .prepared
            .resources()
            .find(|r| r.asset_id == id)
            .ok_or_else(|| invalid("Unknown fixed audio resource"))?;
        let kind = match r.kind {
            opencut_contracts::AssetKind::Audio => "audio",
            opencut_contracts::AssetKind::Video => "video",
            _ => return Err(invalid("Resource has no audio decoder")),
        };
        Ok(AudioResource {
            asset_id: r.asset_id.clone(),
            content_hash: r.version.content_hash.clone(),
            size: r.version.size,
            kind: kind.into(),
        })
    }
    /// Called after validate_receipt has checked actual admissions and observed
    /// formats. Legacy executions retain their original stored representation.
    pub fn decoder_evidence(
        &self,
        receipt: &ClassicAudioRenderReceipt,
    ) -> Result<Option<ClassicAudioDecoderEvidence>, EditorError> {
        self.validate_receipt(receipt)?;
        let sources = receipt
            .decoders
            .iter()
            .map(|decoder| {
                Ok(ClassicAudioDecoderSource {
                    resource: self.resource(&decoder.asset_id)?,
                    decoder: decoder.clone(),
                })
            })
            .collect::<Result<Vec<_>, EditorError>>()?;
        Ok(sources
            .iter()
            .any(|s| {
                s.resource.kind == "audio"
                    && s.decoder.profile
                        == "mediabunny-1.41.0-windowed-concatenate-offline-resample-v1"
            })
            .then_some(ClassicAudioDecoderEvidence { sources }))
    }
    pub fn native_decoder_evidence(
        &self,
        receipt: &ClassicAudioRenderReceipt,
    ) -> Result<ClassicAudioDecoderEvidence, EditorError> {
        self.validate_native_receipt(receipt)?;
        Ok(ClassicAudioDecoderEvidence {
            sources: receipt
                .decoders
                .iter()
                .map(|decoder| {
                    Ok(ClassicAudioDecoderSource {
                        resource: self.resource(&decoder.asset_id)?,
                        decoder: decoder.clone(),
                    })
                })
                .collect::<Result<_, EditorError>>()?,
        })
    }
    fn checked_resource(&self, value: &AudioResource) -> Result<(), EditorError> {
        if &self.resource(&value.asset_id)? != value {
            return Err(invalid("Resource differs from fixed revision"));
        }
        if self.retired_decoded.contains(&value.asset_id) {
            return Err(invalid(
                "Decoded source has completed all preparation consumers",
            ));
        }
        Ok(())
    }
    pub fn plan(&self) -> Result<Value, EditorError> {
        let i = self.prepared.identity();
        let mut resources = BTreeMap::new();
        let mut voices = Vec::new();
        for v in self.prepared.voices() {
            resources.insert(
                v.resource.asset_id.clone(),
                self.resource(&v.resource.asset_id)?,
            );
            let mut voice = json!({"elementId":v.element_id,"assetId":v.resource.asset_id});
            if self.preview.is_some() {
                voice["sourceMode"] = json!(if v.processing
                    == AudioProcessing::LinearInterpolationZeroTail
                {
                    "preserve-pts"
                } else {
                    "prepared-concat"
                });
            }
            voices.push(voice);
        }
        let mut plan = json!({"identity":{"projectId":i.project_id,"sequenceId":i.sequence_id,"revision":i.revision,
            "profile":"classic-export-v31-web-audio","sampleRate":i.format.sample_rate,"channels":i.format.channels},
            "frameCount":self.prepared.duration_samples().to_string(),"outputFrameCount":self.output_frame_count().to_string(),
            "range":self.range,"resources":resources.values().collect::<Vec<_>>(),"voices":voices});
        if self.preview.is_some() {
            plan["identity"]["profile"] = json!(CLASSIC_REALTIME_AUDIO_PROFILE);
        }
        let cache = self.range_cache_bytes();
        if cache < 65536 {
            return Err(invalid("No bounded range cache fits the fixed resources"));
        }
        plan["encodedSource"] = json!({"mode":"verified-range","cacheBytes":cache});
        Ok(plan)
    }
    pub fn validate_receipt(&self, receipt: &ClassicAudioRenderReceipt) -> Result<(), EditorError> {
        self.validate_receipt_for(receipt, false)
    }
    /// Native execution keeps the same fixed clocks/preparation/mastering
    /// validation, while requiring truthful native decoder observations. The
    /// publishing host separately binds and verifies the isolated worker hash.
    pub fn validate_native_receipt(
        &self,
        receipt: &ClassicAudioRenderReceipt,
    ) -> Result<(), EditorError> {
        self.validate_receipt_for(receipt, true)
    }
    fn validate_receipt_for(
        &self,
        receipt: &ClassicAudioRenderReceipt,
        native: bool,
    ) -> Result<(), EditorError> {
        if self.preview.is_some() {
            return Err(invalid(
                "Realtime session cannot validate an export receipt",
            ));
        }
        let identity = self.prepared.identity();
        if receipt.identity.project_id != identity.project_id
            || receipt.identity.sequence_id != identity.sequence_id
            || receipt.identity.revision != identity.revision
            || receipt.identity.sample_rate != identity.format.sample_rate
            || receipt.identity.channels != identity.format.channels
            || receipt.identity.profile != "classic-export-v31-web-audio"
            || self.observations.len() != self.prepared.voices().count()
            || receipt.preparation.len() != self.observations.len()
            || receipt.preparation.len() > MAX_PREPARED_AUDIO_VOICES
            || receipt.decoders.len() > 32
            || receipt.peak_accounted_bytes > self.max_pcm_bytes
            || receipt.range != self.range
            || receipt.mastering.profile != "classic-export-whole-scene-web-audio-v31"
            || decimal(&receipt.mastering.whole_scene_frames)? != self.prepared.duration_samples()
            || !receipt.mastering.input_peak.is_finite()
            || receipt.mastering.input_peak < 0.0
            || receipt.mastering.applied != (receipt.mastering.input_peak > 0.98)
        {
            return Err(invalid(
                "Execution receipt differs from admitted whole-scene preparation",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut decoded = BTreeMap::new();
        for preparation in &receipt.preparation {
            let (observed, facts) = self
                .observations
                .get(&preparation.element_id)
                .ok_or_else(|| invalid("Receipt contains unknown voice"))?;
            if !seen.insert(&preparation.element_id) || &preparation.facts != facts {
                return Err(invalid("Receipt changed or duplicated preparation facts"));
            }
            let voice = self
                .prepared
                .voices()
                .find(|v| v.element_id == preparation.element_id)
                .expect("admitted voice");
            decoded.insert(&voice.resource.asset_id, observed);
        }
        if decoded.len() != receipt.decoders.len() {
            return Err(invalid(
                "Decoder receipt does not cover unique fixed sources",
            ));
        }
        let mut sources = std::collections::BTreeSet::new();
        for decoder in &receipt.decoders {
            let observation = decoded
                .get(&decoder.asset_id)
                .ok_or_else(|| invalid("Unknown decoder receipt resource"))?;
            let resource = self.resource(&decoder.asset_id)?;
            let kind = &resource.kind;
            let windowed = self
                .window_admissions
                .get(&decoder.asset_id)
                .is_some_and(|a| a.reply.mode == "windowed");
            let standalone = kind == "audio" && resource.size <= self.max_encoded_bytes;
            let profile = if native && windowed {
                "native-libav-windowed-concatenate-linear-resample"
            } else if native && standalone {
                "native-libav-sinc-at-target-rate"
            } else if native {
                "native-libav-concatenate-linear-resample"
            } else if windowed {
                "mediabunny-1.41.0-windowed-concatenate-offline-resample-v1"
            } else if standalone {
                "web-audio-decode-at-target-rate"
            } else {
                "mediabunny-1.41.0-concatenate-offline-resample"
            };
            if !sources.insert(&decoder.asset_id)
                || decoder.profile != profile
                || decoder.sample_rate != observation.decoded_sample_rate
                || decoder.frame_count != observation.decoded_frame_count
                || decoder.channels != observation.decoded_channels
                || self.primary.get(&decoder.asset_id) != Some(&decoder.primary)
                || (kind == "video"
                    && decoder
                        .primary
                        .as_ref()
                        .is_none_or(|p| decoder.channels != p.channels.min(2)))
            {
                return Err(invalid(
                    "Decoder receipt changed its observed format or implementation",
                ));
            }
        }
        Ok(())
    }
    fn decoder_remaining_bytes(&self, encoded_size: u64) -> u64 {
        let mut decoded = BTreeMap::new();
        let mut prepared_bytes = 0u64;
        for (id, (_, facts)) in &self.observations {
            let voice = self
                .prepared
                .voices()
                .find(|v| v.element_id == id)
                .expect("admitted voice");
            if !self.retired_decoded.contains(&voice.resource.asset_id)
                && !self
                    .retired_preview_preparation
                    .contains(&voice.resource.asset_id)
            {
                decoded.insert(&voice.resource.asset_id, facts.decoded_bytes);
            }
            if facts.mode != ClassicAudioPreparationMode::UnpreparedLinear {
                prepared_bytes = prepared_bytes.saturating_add(
                    facts
                        .output_frames
                        .saturating_mul(u64::from(facts.num_channels))
                        .saturating_mul(4),
                );
            }
        }
        let occupied = if self.preview.is_some() {
            crate::audio_preview::AUDIO_PREVIEW_MAX_BUFFERED_FRAMES.saturating_mul(8)
        } else {
            self.prepared.duration_samples().saturating_mul(8)
        }
        .saturating_add(
            self.range
                .as_ref()
                .map_or(0, |_| self.output_frame_count().saturating_mul(8)),
        )
        .saturating_add(self.encoded_reservation_bytes(encoded_size))
        .saturating_add(decoded.values().copied().sum::<u64>())
        .saturating_add(
            self.preview_decoded
                .values()
                .filter(|d| {
                    !self
                        .retired_preview_preparation
                        .contains(&d.resource.asset_id)
                })
                .map(|d| {
                    self.preview_resident_frames
                        .get(&d.resource.asset_id)
                        .copied()
                        .map(Ok)
                        .unwrap_or_else(|| decimal(&d.frame_count))
                        .unwrap_or(u64::MAX)
                        .saturating_mul(u64::from(d.channels))
                        .saturating_mul(4)
                })
                .fold(0u64, u64::saturating_add),
        )
        .saturating_add(prepared_bytes);
        let rolling = self.rolling_admissions.values().fold(0u64, |n, a| {
            n.saturating_add(
                (a.working_frames + u64::from(a.max_chunk_frames) * 4)
                    .saturating_mul(u64::from(a.max_channels) * 4),
            )
        });
        self.max_pcm_bytes
            .saturating_sub(occupied.saturating_add(rolling))
    }

    // A new private PCM probe can calculate the unchanged working admission
    // without committing primary/decoder state when the legacy path suffices.
    fn admit_decoder(
        &mut self,
        v: DecodeAdmission,
        permit_pcm_audio: bool,
        record: bool,
    ) -> Result<Value, EditorError> {
        if self.rolling_admissions.contains_key(&v.resource.asset_id) {
            return Err(invalid("Rolling primary allocation cannot be replaced"));
        }
        if self
            .window_admissions
            .get(&v.resource.asset_id)
            .is_some_and(|a| a.reply.mode == "windowed")
        {
            return Err(invalid(
                "Windowed admission cannot be replaced by whole-source admission",
            ));
        }
        self.checked_resource(&v.resource)?;
        let resource = self
            .prepared
            .resources()
            .find(|r| r.asset_id == v.resource.asset_id)
            .expect("checked resource");
        let channels = if v.resource.kind == "video" || self.preview.is_some() || permit_pcm_audio {
            let primary = v
                .primary
                .as_ref()
                .ok_or_else(|| invalid("Video admission requires an actual primary audio track"))?;
            if primary.sample_rate == 0
                || primary.sample_rate > 384_000
                || !(1..=64).contains(&primary.channels)
                || primary.codec.trim().is_empty()
                || primary.codec.len() > 128
                || primary.codec.chars().any(char::is_control)
            {
                return Err(invalid("Invalid actual primary audio format"));
            }
            if self.preview.is_some()
                && primary.channels > 2
                && self.prepared.voices().any(|voice| {
                    voice.resource.asset_id == v.resource.asset_id
                        && voice.processing == AudioProcessing::LinearInterpolationZeroTail
                })
            {
                return Err(EditorError::new(
                    "UNSUPPORTED_AUDIO_PREVIEW_CHANNEL_LAYOUT",
                    "Ordinary realtime preview requires an independently verified mono or stereo layout",
                ));
            }
            primary.channels
        } else {
            if v.primary.is_some() {
                return Err(invalid(
                    "Web Audio admission cannot supply container primary facts",
                ));
            }
            match &resource.metadata.audio {
                opencut_contracts::scene::AudioPresence::Present {
                    metadata: Some(metadata),
                } => metadata.channels,
                _ => 64,
            }
        };
        if self
            .primary
            .get(&v.resource.asset_id)
            .is_some_and(|previous| previous != &v.primary)
        {
            return Err(invalid(
                "Actual primary audio selection changed after admission",
            ));
        }
        if !(1..=64).contains(&channels) {
            return Err(invalid("Invalid observed decoder channels"));
        }
        // Preserve the decoder's four-copy reservation and half-budget headroom.
        let frames = self.decoder_remaining_bytes(v.resource.size) / (32 * u64::from(channels));
        if frames == 0 {
            return Err(EditorError::new(
                "AUDIO_BUDGET_EXCEEDED",
                "No decoder workspace remains in admission",
            ));
        }
        let admitted = self
            .admissions
            .get(&v.resource.asset_id)
            .copied()
            .unwrap_or((frames, channels));
        if record {
            self.admissions
                .entry(v.resource.asset_id.clone())
                .or_insert(admitted);
            self.primary.entry(v.resource.asset_id).or_insert(v.primary);
        }
        Ok(json!({"maxFrames":admitted.0,"maxChannels":admitted.1}))
    }
    pub fn callback(
        &mut self,
        request: AudioCallback,
        resources: &mut (impl ClassicAudioResources + ?Sized),
    ) -> Result<Value, EditorError> {
        resources.checkpoint()?;
        match request {
            AudioCallback::OpenRange(v) => {
                self.checked_resource(&v.resource)?;
                let (id, size) = resources.resolve(&v.resource, &v.handle)?;
                if size != v.resource.size {
                    return Err(invalid("Verified descriptor size changed"));
                }
                Ok(json!({"transferId":id,"byteLength":size.to_string(),
                    "contentHash":v.resource.content_hash,"maxChunkBytes":65536}))
            }
            AudioCallback::Resolve(v) => {
                self.checked_resource(&v.resource)?;
                let (id, size) = resources.resolve(&v.resource, &v.handle)?;
                if size != v.resource.size {
                    return Err(invalid("Verified descriptor size changed"));
                }
                Ok(json!({"transferId":id,"byteLength":size.to_string()}))
            }
            AudioCallback::Read(v) => {
                if v.count == 0 || v.count > 65_536 {
                    return Err(invalid("Byte chunks must contain 1..65536 bytes"));
                }
                let offset = decimal(&v.offset)?;
                let (bytes, eof) = resources.read(&v.transfer_id, offset, v.count)?;
                if bytes.len() > v.count as usize || bytes.len() as u64 > self.max_encoded_bytes {
                    return Err(invalid("Resource port exceeded byte chunk admission"));
                }
                let hex = crate::browser_callback_encoding::bytes_hex(&bytes);
                Ok(json!({"offset":v.offset,"bytesHex":hex,"eof":eof}))
            }
            AudioCallback::Release(v) => {
                resources.release(&v.transfer_id)?;
                Ok(json!({"released":true}))
            }
            AudioCallback::ReleaseDecoded(v) => self.release_decoded(v),
            AudioCallback::Admit(v) => self.admit_decoder(v, false, true),
            AudioCallback::AdmitContainer(v) => self.admit_windowed(v, resources),
            AudioCallback::AdmitPcmAudioWindowed(v) => self.admit_pcm_audio_windowed(v, resources),
            AudioCallback::AdmitPreviewDecode(v) => self.admit_preview_decode(v, resources),
            AudioCallback::Pitch(v) => self.pitch(v),
            AudioCallback::Positions(v) => {
                let observation = self
                    .observations
                    .get(&v.element_id)
                    .ok_or_else(|| invalid("Source sampling requires admitted decoded facts"))?;
                if observation.0.decoded_sample_rate != v.source_sample_rate {
                    return Err(invalid("Decoded sampling rate changed"));
                }
                serialize(
                    compile_original_audio_positions_scene(
                        self.prepared,
                        &v.element_id,
                        decimal(&v.local_start_sample)?,
                        v.frame_count,
                        v.source_sample_rate,
                    )
                    .map_err(composition)?,
                )
            }
            AudioCallback::Block(v) => self.block(v),
            AudioCallback::PreviewWindows(v) => self.preview_windows(v),
            AudioCallback::BindPreview(v) => self.bind_preview(v),
            AudioCallback::PreviewBlock(v) => self.preview_block(v),
            AudioCallback::ObservePreviewDecoded(v) => self.observe_preview_decoded(v),
            AudioCallback::ObserveWindowedPreview(v) => self.observe_windowed_preview(v),
        }
    }
    fn pitch(&mut self, request: PitchRequest) -> Result<Value, EditorError> {
        let resident = request
            .resident_frame_count
            .as_deref()
            .map(decimal)
            .transpose()?;
        self.pitch_inner(request, resident)
    }
    fn pitch_inner(
        &mut self,
        request: PitchRequest,
        resident_frames: Option<u64>,
    ) -> Result<Value, EditorError> {
        let voice = self
            .prepared
            .voices()
            .find(|v| v.element_id == request.element_id)
            .ok_or_else(|| invalid("Unknown prepared voice"))?;
        if self.retired_decoded.contains(&voice.resource.asset_id)
            || self
                .retired_preview_preparation
                .contains(&voice.resource.asset_id)
        {
            return Err(invalid("Decoded preparation has already completed"));
        }
        if let Some((previous, facts)) = self.observations.get(&request.element_id) {
            let state = self
                .window_admissions
                .get(&voice.resource.asset_id)
                .filter(|s| s.reply.mode == "windowed");
            if previous != &request || state.and_then(|s| s.resident_frames) != resident_frames {
                return Err(invalid(
                    "Decoded facts or residency changed within fixed execution",
                ));
            }
            return serialize(facts);
        }
        if request.decoded_sample_rate != self.prepared.identity().format.sample_rate {
            return Err(invalid(
                "Browser decoder must use the admitted target sample rate",
            ));
        }
        let admitted = self
            .admissions
            .get(&voice.resource.asset_id)
            .ok_or_else(|| invalid("Decode must be admitted before PCM observations"))?;
        let decoded_frames = decimal(&request.decoded_frame_count)?;
        if decoded_frames == 0
            || decoded_frames > admitted.0
            || request.decoded_channels == 0
            || request.decoded_channels > admitted.1
        {
            return Err(invalid("Decoded PCM exceeds its actual format admission"));
        }
        if self
            .primary
            .get(&voice.resource.asset_id)
            .and_then(Option::as_ref)
            .is_some_and(|primary| request.decoded_channels != primary.channels.min(2))
        {
            return Err(invalid(
                "Video PCM must retain the primary track's first one or two channels",
            ));
        }
        for (id, (observed, _)) in &self.observations {
            let other = self
                .prepared
                .voices()
                .find(|v| v.element_id == id)
                .expect("admitted voice");
            if other.resource.asset_id == voice.resource.asset_id
                && (observed.decoded_sample_rate != request.decoded_sample_rate
                    || observed.decoded_frame_count != request.decoded_frame_count
                    || observed.decoded_channels != request.decoded_channels)
            {
                return Err(invalid("Shared decoded resource changed between voices"));
            }
        }
        let input = ClassicAudioPreparationInput {
            source_start: voice.source.clock.nominal_request.start.clone(),
            timeline_duration: voice.timeline_duration.clone(),
            rate: voice.source.clock.rate,
            maintain_pitch: voice.maintain_pitch,
            decoded_sample_rate: request.decoded_sample_rate,
            decoded_frames: decimal(&request.decoded_frame_count)?,
            decoded_channels: request.decoded_channels,
            target_sample_rate: self.prepared.identity().format.sample_rate,
        };
        let windowed = self
            .window_admissions
            .get(&voice.resource.asset_id)
            .is_some_and(|s| s.reply.mode == "windowed");
        if self
            .window_admissions
            .get(&voice.resource.asset_id)
            .is_some_and(|s| windowed && decoded_frames > s.reply.max_frames)
        {
            return Err(invalid(
                "Logical prepared PCM exceeded target-rate admission",
            ));
        }
        let mode =
            classic_audio_preparation_mode_for_processing(voice.processing).map_err(composition)?;
        let facts = if let Some(resident) = resident_frames {
            let state = self
                .window_admissions
                .get_mut(&voice.resource.asset_id)
                .filter(|s| s.reply.mode == "windowed")
                .ok_or_else(|| {
                    invalid("Resident observation requires windowed source admission")
                })?;
            if resident > state.resident_frame_limit
                || resident > decoded_frames
                || state.resident_frames.is_some_and(|prior| prior != resident)
            {
                return Err(invalid(
                    "Decoded resident PCM changed or exceeded its fixed window admission",
                ));
            }
            let facts =
                classic_audio_preparation_for_mode(&input, &self.budget, Some(resident), mode)
                    .map_err(composition)?;
            state.resident_frames = Some(resident);
            facts
        } else {
            if windowed {
                return Err(invalid(
                    "Windowed source requires its actual residency observation",
                ));
            }
            classic_audio_preparation_for_mode(&input, &self.budget, None, mode)
                .map_err(composition)?
        };
        let value = serialize(&facts)?;
        self.observations
            .insert(request.element_id.clone(), (request, facts));
        Ok(value)
    }
    fn release_decoded(&mut self, request: DecodedRelease) -> Result<Value, EditorError> {
        self.checked_resource(&request.resource)?;
        if self.preview.is_some() {
            let asset = &request.resource.asset_id;
            if !self.window_admissions.contains_key(asset)
                || !self.preview_decoded.contains_key(asset)
                || !self.prepared.voices().any(|voice| {
                    voice.resource.asset_id == *asset
                        && voice.processing != AudioProcessing::LinearInterpolationZeroTail
                })
                || !self
                    .prepared
                    .voices()
                    .filter(|voice| {
                        voice.resource.asset_id == *asset
                            && voice.processing != AudioProcessing::LinearInterpolationZeroTail
                    })
                    .all(|voice| {
                        self.observations
                            .get(voice.element_id)
                            .is_some_and(|(_, facts)| {
                                facts.mode != ClassicAudioPreparationMode::UnpreparedLinear
                            })
                    })
            {
                return Err(invalid(
                    "Preview preparation still has an unprepared consumer",
                ));
            }
            self.retired_preview_preparation.insert(asset.clone());
            return Ok(json!({"released":true}));
        }
        if !self
            .prepared
            .voices()
            .filter(|voice| voice.resource.asset_id == request.resource.asset_id)
            .all(|voice| {
                self.observations
                    .get(voice.element_id)
                    .is_some_and(|(_, facts)| {
                        facts.mode != ClassicAudioPreparationMode::UnpreparedLinear
                            || self.compiled_output_until == self.prepared.duration_samples()
                    })
            })
        {
            return Err(invalid(
                "Decoded PCM still has an ordinary or unprepared consumer",
            ));
        }
        self.retired_decoded.insert(request.resource.asset_id);
        Ok(json!({"released":true}))
    }
    /// Native transports can encode exact compiled facts without allocating a
    /// second per-sample JSON tree. This shares the existing compiler and its
    /// live authorization/cancellation checkpoint with callback dispatch.
    pub fn compile_export_block_typed(
        &mut self,
        request: BlockRequest,
        resources: &mut (impl ClassicAudioResources + ?Sized),
    ) -> Result<RuntimeAudioBlock, EditorError> {
        resources.checkpoint()?;
        self.block_typed(request)
    }
    fn block(&mut self, request: BlockRequest) -> Result<Value, EditorError> {
        serialize(self.block_typed(request)?)
    }
    fn block_typed(&mut self, request: BlockRequest) -> Result<RuntimeAudioBlock, EditorError> {
        if self.preview.is_some() {
            return Err(invalid(
                "Realtime session requires preview source binding and block callbacks",
            ));
        }
        if request.source_formats.len() > MAX_PREPARED_AUDIO_VOICES {
            return Err(invalid("Too many final source buffers"));
        }
        let start = decimal(&request.start_sample)?;
        if !(1..=MAX_AUDIO_BLOCK_FRAMES).contains(&request.frame_count)
            || start >= self.prepared.duration_samples()
            || start
                .checked_add(u64::from(request.frame_count))
                .is_none_or(|end| end > self.prepared.duration_samples())
        {
            return Err(invalid("Requested audio block is outside the fixed scene"));
        }
        // The private WAV driver advances by this actual prefix. Admission is
        // shared with preview; the strict compiler never silently drops voices.
        let frame_count = self.prepared.block_frame_limit(start, request.frame_count);
        if frame_count == 0 {
            return Err(invalid("Requested audio block has no admitted prefix"));
        }
        let formats = request
            .source_formats
            .iter()
            .map(|format| {
                let (decoded, facts) = self
                    .observations
                    .get(&format.element_id)
                    .ok_or_else(|| invalid("Final PCM requires admitted decoded observations"))?;
                let prepared = facts.mode != ClassicAudioPreparationMode::UnpreparedLinear;
                let expected_frames = if prepared {
                    facts.output_frames
                } else {
                    decimal(&decoded.decoded_frame_count)?
                };
                let channels = if prepared {
                    facts.num_channels
                } else {
                    decoded.decoded_channels
                };
                if format.prepared != prepared
                    || format.channels != channels
                    || format.sample_rate != facts.target_sample_rate
                    || decimal(&format.frame_count)? != expected_frames
                {
                    return Err(invalid("Final PCM differs from exact admitted preparation"));
                }
                Ok(RuntimeSourceFormat {
                    element_id: format.element_id.clone(),
                    sample_rate: format.sample_rate,
                    frame_count: expected_frames,
                    channels,
                    mode: facts.mode,
                })
            })
            .collect::<Result<Vec<_>, EditorError>>()?;
        let bound = bind_classic_audio_pcm_scene(self.prepared, &formats).map_err(composition)?;
        let value =
            compile_bound_audio_block_scene(&bound, start, frame_count).map_err(composition)?;
        // Only the actually admitted contiguous prefix proves every ordinary
        // source consumer has finished. Arbitrary pure block requests retain
        // their existing semantics, but cannot manufacture release credit.
        if start == self.compiled_output_until {
            self.compiled_output_until += u64::from(frame_count);
        }
        Ok(value)
    }
    fn bind_preview(&mut self, request: BindPreviewSources) -> Result<Value, EditorError> {
        let preview = self
            .preview
            .ok_or_else(|| invalid("Preview source binding requires a realtime session"))?;
        let rolling = !self.rolling_admissions.is_empty();
        if request.window.is_some() && self.planned_preview_window != request.window {
            return Err(invalid("PCM window differs from the Rust planned command"));
        }
        let bound = if rolling {
            bind_preview_audio_window(preview, &request.source_formats)
        } else {
            bind_preview_audio_scene(preview, &request.source_formats)
        }
        .map_err(composition)?;
        if self
            .preview_bound
            .as_ref()
            .is_some_and(|old| !rolling && !old.source_formats().eq(request.source_formats.iter()))
        {
            return Err(invalid("Bound preview PCM facts changed"));
        }
        let mut shared: BTreeMap<&str, &PreviewSourceFormat> = BTreeMap::new();
        for format in &request.source_formats {
            let voice = self
                .prepared
                .voices()
                .find(|v| v.element_id == format.element_id)
                .ok_or_else(|| invalid("Unknown preview voice"))?;
            let admitted = self
                .admissions
                .get(&voice.resource.asset_id)
                .ok_or_else(|| invalid("Actual source binding requires decoder admission"))?;
            let primary = self
                .primary
                .get(&voice.resource.asset_id)
                .and_then(Option::as_ref)
                .ok_or_else(|| invalid("Preview decode requires observed primary identity"))?;
            let native = self
                .preview_decoded
                .get(&voice.resource.asset_id)
                .ok_or_else(|| {
                    invalid("Preview source must report actual native PCM before binding")
                })?;
            if (format.prepared
                || !self
                    .rolling_admissions
                    .contains_key(&voice.resource.asset_id))
                && self
                    .preview_bound
                    .as_ref()
                    .and_then(|old| {
                        old.source_formats()
                            .find(|old| old.element_id == format.element_id)
                    })
                    .is_some_and(|old| old != format)
            {
                return Err(invalid("Fixed prepared or nonrolling source facts changed"));
            }
            if format.channels != primary.channels.min(2) || format.channels > admitted.1 {
                return Err(invalid("Bound PCM changed its selected primary channels"));
            }
            if format.prepared {
                let (decoded, facts) =
                    self.observations.get(&format.element_id).ok_or_else(|| {
                        invalid("Prepared preview PCM requires exact pitch preparation facts")
                    })?;
                if facts.mode == ClassicAudioPreparationMode::UnpreparedLinear
                    || format.frame_count != facts.output_frames
                    || format.sample_rate != facts.target_sample_rate
                    || format.channels != facts.num_channels
                    || decimal(&decoded.decoded_frame_count)? > admitted.0
                {
                    return Err(invalid(
                        "Prepared preview buffer differs from admitted preparation",
                    ));
                }
            } else {
                let max_frames = self
                    .rolling_admissions
                    .get(&voice.resource.asset_id)
                    .map_or(admitted.0, |a| a.working_frames);
                if format.sample_rate != primary.sample_rate
                    || format.frame_count > max_frames
                    || (!self
                        .rolling_admissions
                        .contains_key(&voice.resource.asset_id)
                        && format.frame_count != decimal(&native.frame_count)?)
                {
                    return Err(invalid(
                        "Ordinary PCM changed its actual primary sample rate or exceeded admission",
                    ));
                }
                if let Some(old) = shared.insert(voice.resource.asset_id.as_str(), format)
                    && (old.sample_rate != format.sample_rate
                        || old.frame_count != format.frame_count
                        || old.channels != format.channels
                        || old.segments != format.segments)
                {
                    return Err(invalid(
                        "Shared ordinary source changed its observed PTS facts",
                    ));
                }
            }
        }
        // This binding borrows only the immutable preparation. Retain the one
        // validated PTS index for all blocks; live resource checkpoints still
        // run before every compilation and host publication.
        // The next compile must consume this exact transient window. Fixed
        // preparation/primary identity remains immutable across commands.
        self.preview_window = request.window;
        self.preview_bound = Some(bound);
        Ok(json!({"bound":true}))
    }
    fn observe_preview_decoded(
        &mut self,
        observed: PreviewDecodedObservation,
    ) -> Result<Value, EditorError> {
        self.observe_preview_inner(observed, None)
    }
    fn observe_windowed_preview(
        &mut self,
        request: WindowedPreviewObservation,
    ) -> Result<Value, EditorError> {
        let resident = decimal(&request.resident_frame_count)?;
        self.observe_preview_inner(request.decoded, Some(resident))
    }
    fn observe_preview_inner(
        &mut self,
        observed: PreviewDecodedObservation,
        resident: Option<u64>,
    ) -> Result<Value, EditorError> {
        if self.preview.is_none() {
            return Err(invalid(
                "Native preview PCM observations require a realtime session",
            ));
        }
        self.checked_resource(&observed.resource)?;
        let primary = self
            .primary
            .get(&observed.resource.asset_id)
            .and_then(Option::as_ref)
            .ok_or_else(|| invalid("Native PCM requires primary admission"))?;
        let admitted = self
            .admissions
            .get(&observed.resource.asset_id)
            .ok_or_else(|| invalid("Native PCM requires decoder admission"))?;
        let frames = decimal(&observed.frame_count)?;
        let window = self
            .window_admissions
            .get(&observed.resource.asset_id)
            .filter(|s| s.reply.mode == "windowed");
        if resident.is_some() != window.is_some()
            || window.is_some_and(|s| {
                frames > decimal(&s.reply.max_native_frames).unwrap_or(0)
                    || resident.is_some_and(|n| {
                        // The immutable decoded extent is in native frames;
                        // resident preparation PCM is in the target device rate.
                        let target_frames = (u128::from(frames)
                            * u128::from(self.prepared.identity().format.sample_rate))
                        .div_ceil(u128::from(primary.sample_rate.max(1)));
                        u128::from(n) > target_frames || n > s.reply.working_frames
                    })
            })
        {
            return Err(invalid(
                "Preview residency requires its fixed window admission",
            ));
        }
        let rolling = self
            .rolling_admissions
            .contains_key(&observed.resource.asset_id);
        let prepared = self
            .window_admissions
            .contains_key(&observed.resource.asset_id);
        if (rolling && !prepared && frames != 0)
            || observed.sample_rate != primary.sample_rate
            || observed.channels != primary.channels.min(2)
            || (frames == 0 && (!rolling || prepared))
            || frames > admitted.0
        {
            return Err(invalid(
                "Observed native PCM changed primary facts or exceeded admission",
            ));
        }
        if let Some(old) = self.preview_decoded.get(&observed.resource.asset_id) {
            if old != &observed
                || self
                    .preview_resident_frames
                    .get(&observed.resource.asset_id)
                    .copied()
                    != resident
            {
                return Err(invalid("Observed native PCM changed within fixed playback"));
            }
            return Ok(json!({"observed":true}));
        }
        let bytes = resident
            .unwrap_or(frames)
            .checked_mul(u64::from(observed.channels))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| invalid("Native PCM byte count overflow"))?;
        let occupied = self
            .preview_decoded
            .values()
            .filter(|d| {
                !self
                    .retired_preview_preparation
                    .contains(&d.resource.asset_id)
            })
            .try_fold(
                crate::audio_preview::AUDIO_PREVIEW_MAX_BUFFERED_FRAMES * 8,
                |sum, d| {
                    self.preview_resident_frames
                        .get(&d.resource.asset_id)
                        .copied()
                        .map(Ok)
                        .unwrap_or_else(|| decimal(&d.frame_count))?
                        .checked_mul(u64::from(d.channels))
                        .and_then(|n| n.checked_mul(4))
                        .and_then(|n| sum.checked_add(n))
                        .ok_or_else(|| invalid("Retained native PCM byte count overflow"))
                },
            )?;
        if occupied
            .checked_add(bytes)
            .is_none_or(|n| n > self.max_pcm_bytes)
        {
            return Err(EditorError::new(
                "AUDIO_BUDGET_EXCEEDED",
                "Retained native preview PCM exceeds admission",
            ));
        }
        if let Some(resident) = resident {
            self.preview_resident_frames
                .insert(observed.resource.asset_id.clone(), resident);
        }
        self.preview_decoded
            .insert(observed.resource.asset_id.clone(), observed);
        Ok(json!({"observed":true}))
    }
    fn preview_windows(&mut self, request: PreviewBlockRequest) -> Result<Value, EditorError> {
        let preview = self
            .preview
            .ok_or_else(|| invalid("Not a realtime session"))?;
        let start = decimal(&request.start_sample)?;
        if start < self.preview_anchor
            || start
                .checked_add(u64::from(request.frame_count))
                .is_none_or(|end| end > preview.extent())
        {
            return Err(invalid("Source window is outside its fixed generation"));
        }
        let rates = self
            .rolling_admissions
            .iter()
            .map(|(id, admission)| (id.clone(), admission.source_sample_rate))
            .collect();
        let windows = preview_source_windows(preview, start, request.frame_count, &rates)
            .map_err(composition)?;
        self.planned_preview_window = Some(request);
        serialize(windows)
    }
    /// Trusted native transports may encode the typed block directly. This
    /// entry point retains the same live authorization/cancellation checkpoint
    /// as callback dispatch; it cannot bypass the fixed preview binding.
    pub fn compile_preview_block_typed(
        &mut self,
        request: PreviewBlockRequest,
        resources: &mut dyn ClassicAudioResources,
    ) -> Result<PreviewAudioBlock, EditorError> {
        resources.checkpoint()?;
        self.preview_block_typed(request)
    }
    fn preview_block(&self, request: PreviewBlockRequest) -> Result<Value, EditorError> {
        // callback() has already performed its one checkpoint.
        serialize(self.preview_block_typed(request)?)
    }
    fn preview_block_typed(
        &self,
        request: PreviewBlockRequest,
    ) -> Result<PreviewAudioBlock, EditorError> {
        if self.preview.is_none() {
            return Err(invalid("Not a realtime session"));
        }
        if !self.rolling_admissions.is_empty() && self.preview_window.as_ref() != Some(&request) {
            return Err(invalid(
                "Rolling PCM must bind this exact command before compilation",
            ));
        }
        let bound = self
            .preview_bound
            .as_ref()
            .ok_or_else(|| invalid("Preview PCM must bind before block compilation"))?;
        compile_preview_audio_block_scene(
            bound,
            decimal(&request.start_sample)?,
            request.frame_count,
            self.preview_anchor,
            self.preview_gain.as_ref(),
        )
        .map_err(composition)
    }
    pub fn validate_preview_receipt(
        &self,
        receipt: &ClassicAudioPreviewReceipt,
    ) -> Result<(), EditorError> {
        if self.preview.is_none() {
            return Err(invalid("Not a realtime session"));
        }
        let identity = self.prepared.identity();
        if receipt.identity.project_id != identity.project_id
            || receipt.identity.revision != identity.revision
            || receipt.identity.sequence_id != identity.sequence_id
            || receipt.identity.profile != CLASSIC_REALTIME_AUDIO_PROFILE
            || receipt.identity.sample_rate != identity.format.sample_rate
            || receipt.identity.channels != 2
            || receipt.engine_identity.is_empty()
            || receipt.peak_accounted_bytes > self.max_pcm_bytes
            || self
                .preview_bound
                .as_ref()
                .is_none_or(|bound| !bound.source_formats().eq(receipt.source_formats.iter()))
            || receipt.preparation.len() != self.observations.len()
            || receipt.decoders.len() > 32
        {
            return Err(invalid(
                "Realtime preparation receipt differs from fixed admitted identity",
            ));
        }
        let mut prepared_ids = std::collections::BTreeSet::new();
        for item in &receipt.preparation {
            if !prepared_ids.insert(&item.element_id)
                || self
                    .observations
                    .get(&item.element_id)
                    .is_none_or(|(_, facts)| facts != &item.facts)
            {
                return Err(invalid(
                    "Realtime preparation receipt changed exact pitch facts",
                ));
            }
        }
        let expected: std::collections::BTreeSet<_> = self
            .prepared
            .resources()
            .map(|r| r.asset_id.as_str())
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for decoder in &receipt.decoders {
            let rolling = self.rolling_admissions.contains_key(&decoder.asset_id);
            let unknown_extent = rolling && !self.window_admissions.contains_key(&decoder.asset_id);
            if unknown_extent != decoder.frame_count.is_none() {
                return Err(invalid(
                    "Preview source extent changed its observed preparation",
                ));
            }
            let frames = decoder
                .frame_count
                .as_deref()
                .map(decimal)
                .transpose()?
                .unwrap_or(0);
            let admitted = self
                .admissions
                .get(&decoder.asset_id)
                .ok_or_else(|| invalid("Unknown realtime decoder receipt"))?;
            let observed = self
                .preview_decoded
                .get(&decoder.asset_id)
                .ok_or_else(|| invalid("Realtime receipt contains unobserved native PCM"))?;
            if !expected.contains(decoder.asset_id.as_str())
                || !seen.insert(decoder.asset_id.as_str())
                || self.primary.get(&decoder.asset_id).and_then(Option::as_ref)
                    != Some(&decoder.primary)
                || decoder.sample_rate != decoder.primary.sample_rate
                || decoder.channels != decoder.primary.channels.min(2)
                || (!unknown_extent && frames == 0)
                || frames > admitted.0
                || observed.sample_rate != decoder.sample_rate
                || observed.channels != decoder.channels
                || decoder
                    .frame_count
                    .as_ref()
                    .is_some_and(|count| &observed.frame_count != count)
            {
                return Err(invalid(
                    "Realtime decoder receipt changed admitted primary PCM",
                ));
            }
            for voice in self
                .prepared
                .voices()
                .filter(|v| v.resource.asset_id == decoder.asset_id)
            {
                let format = receipt
                    .source_formats
                    .iter()
                    .find(|f| f.element_id == voice.element_id)
                    .ok_or_else(|| invalid("Missing realtime source receipt"))?;
                if !format.prepared {
                    let expected = if rolling { 0 } else { frames };
                    if format.frame_count != expected {
                        return Err(invalid(
                            "Initial ordinary source length differs from its actual decoder window",
                        ));
                    }
                } else {
                    let (decoded, _) = self
                        .observations
                        .get(voice.element_id)
                        .ok_or_else(|| invalid("Missing prepared PCM observation"))?;
                    let target_frames = u128::from(frames)
                        .checked_mul(u128::from(identity.format.sample_rate))
                        .ok_or_else(|| invalid("Decoded resample extent overflow"))?
                        .div_ceil(u128::from(decoder.sample_rate));
                    if u128::from(decimal(&decoded.decoded_frame_count)?) != target_frames
                        || decoded.decoded_channels != decoder.channels
                    {
                        return Err(invalid(
                            "Prepared concat observation differs from actual native decoder extent",
                        ));
                    }
                }
            }
        }
        if seen != expected {
            return Err(invalid(
                "Realtime receipt omits a fixed participating source",
            ));
        }
        Ok(())
    }
}
fn composition(e: opencut_composition::CompositionError) -> EditorError {
    EditorError::new(e.code, e.message)
}
fn serialize(value: impl serde::Serialize) -> Result<Value, EditorError> {
    serde_json::to_value(value).map_err(|_| {
        EditorError::new(
            "SERIALIZATION_ERROR",
            "Could not encode bounded audio facts",
        )
    })
}
