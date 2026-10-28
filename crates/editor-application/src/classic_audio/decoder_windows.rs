//! Fixed-source window planning only. No codec, PCM, locator or platform I/O.
use super::*;
use opencut_time::RationalTime;

const FILTER_GUARD_FRAMES: u64 = 4096;
fn count(value: u128) -> Result<u64, EditorError> {
    u64::try_from(value).map_err(|_| invalid("Decoder window count overflow"))
}
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
impl ClassicAudioSession<'_> {
    pub(super) fn admit_preview_decode(
        &mut self,
        request: DecodeAdmission,
        resources: &mut (impl ClassicAudioResources + ?Sized),
    ) -> Result<Value, EditorError> {
        self.checked_resource(&request.resource)?;
        if self.preview.is_none() {
            return Err(invalid("Preview allocation requires a realtime session"));
        }
        resources.checkpoint()?;
        let asset_id = request.resource.asset_id.clone();
        if self.retired_preview_preparation.contains(&asset_id) {
            return Err(invalid(
                "Completed preview preparation cannot be decoded again",
            ));
        }
        let ordinary = self.prepared.voices().any(|v| {
            v.resource.asset_id == asset_id
                && v.processing == AudioProcessing::LinearInterpolationZeroTail
        });
        let preparation = self.prepared.voices().any(|v| {
            v.resource.asset_id == asset_id
                && v.processing != AudioProcessing::LinearInterpolationZeroTail
        });
        if !ordinary && !preparation {
            return Err(invalid("Preview source has no fixed consumer"));
        }
        if !ordinary {
            self.admit_preview_windowed(request, resources)?;
            return serialize(PreviewDecodeAllocation {
                stream: None,
                preparation: Some(self.window_admissions[&asset_id].reply.clone()),
            });
        }
        let primary = request
            .primary
            .clone()
            .ok_or_else(|| invalid("Rolling preview requires its observed primary"))?;
        if !(1..=2).contains(&primary.channels)
            || primary.sample_rate == 0
            || primary.sample_rate > 384000
        {
            return Err(invalid("Ordinary preview requires mono/stereo primary PCM"));
        }
        if let Some(prior) = self.rolling_admissions.get(&asset_id) {
            if self.primary.get(&asset_id) != Some(&request.primary) {
                return Err(invalid("Rolling primary selection changed"));
            }
            return serialize(PreviewDecodeAllocation {
                stream: Some(prior.clone()),
                preparation: self
                    .window_admissions
                    .get(&asset_id)
                    .map(|s| s.reply.clone()),
            });
        }
        if self.admissions.contains_key(&asset_id) {
            return Err(invalid(
                "Fixed decoder allocation cannot change execution mode",
            ));
        }
        let remaining = self
            .prepared
            .resources()
            .filter(|r| !self.admissions.contains_key(&r.asset_id))
            .map(|r| r.asset_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            .max(1) as u64;
        let available = self.decoder_remaining_bytes(request.resource.size) / remaining;
        if preparation {
            // One source owns both consumers. Only prepared voices contribute
            // static concat windows;ordinary voices keep command-scoped PTS.
            self.admit_windows_inner(request.clone(), resources, true, false, true)?;
        }
        // A mixed source leaves the original preparation headroom and charges
        // its rolling/cache allocation separately,never twice for two aliases.
        let available = if preparation {
            available / 4
        } else {
            available
        };
        let frame_bytes = u64::from(primary.channels) * 4;
        let chunk_frames = (available / (frame_bytes * 8)).min(65536);
        let working_frames = available / frame_bytes - chunk_frames * 4;
        if chunk_frames == 0 || working_frames < chunk_frames * 2 {
            return Err(EditorError::new(
                "AUDIO_BUDGET_EXCEEDED",
                "No bounded rolling PCM window fits admission",
            ));
        }
        let admission = RollingPreviewAdmission {
            working_frames,
            max_channels: primary.channels,
            max_chunk_frames: chunk_frames as u32,
            source_sample_rate: primary.sample_rate,
            target_sample_rate: self.prepared.identity().format.sample_rate,
        };
        resources.checkpoint()?;
        if !preparation {
            self.admissions
                .insert(asset_id.clone(), (working_frames, primary.channels));
        }
        self.primary.insert(asset_id.clone(), request.primary);
        self.rolling_admissions
            .insert(asset_id.clone(), admission.clone());
        serialize(PreviewDecodeAllocation {
            stream: Some(admission),
            preparation: self
                .window_admissions
                .get(&asset_id)
                .map(|s| s.reply.clone()),
        })
    }

    pub(super) fn admit_windowed(
        &mut self,
        request: DecodeAdmission,
        resources: &mut (impl ClassicAudioResources + ?Sized),
    ) -> Result<Value, EditorError> {
        self.admit_windows_inner(request, resources, false, false, false)
    }
    pub(super) fn admit_preview_windowed(
        &mut self,
        request: DecodeAdmission,
        resources: &mut (impl ClassicAudioResources + ?Sized),
    ) -> Result<Value, EditorError> {
        self.admit_windows_inner(request, resources, true, false, false)
    }
    pub(super) fn admit_pcm_audio_windowed(
        &mut self,
        request: DecodeAdmission,
        resources: &mut (impl ClassicAudioResources + ?Sized),
    ) -> Result<Value, EditorError> {
        self.checked_resource(&request.resource)?;
        let primary = request
            .primary
            .as_ref()
            .ok_or_else(|| invalid("PCM admission requires observed primary facts"))?;
        if self.preview.is_some()
            || request.resource.kind != "audio"
            || native_pcm_sample_bytes(&primary.codec).is_none()
            || !(1..=2).contains(&primary.channels)
            || primary.sample_rate != self.prepared.identity().format.sample_rate
        {
            return Err(invalid(
                "Windowed PCM requires native-rate mono/stereo signed16 or float32 PCM audio",
            ));
        }
        if !self
            .window_admissions
            .contains_key(&request.resource.asset_id)
        {
            let legacy = self.admit_decoder(
                DecodeAdmission {
                    resource: request.resource.clone(),
                    primary: None,
                },
                false,
                false,
            )?;
            let frames = legacy["maxFrames"]
                .as_u64()
                .ok_or_else(|| invalid("Invalid legacy decoder admission"))?;
            let small = self
                .prepared
                .resources()
                .find(|r| r.asset_id == request.resource.asset_id)
                .and_then(|r| r.duration.as_ref())
                .and_then(|t| RationalTime::try_from(t).ok())
                .is_some_and(|t| {
                    t.ticks() > 0
                        && u128::from(t.ticks().unsigned_abs()) * u128::from(primary.sample_rate)
                            <= u128::from(frames) * u128::from(t.timebase())
                });
            if small {
                // This probe publishes no admission or primary observation.
                // The unchanged Web Audio path performs its original admission.
                return Ok(serde_json::json!({"mode":"whole"}));
            }
        }
        let admission = self.admit_windows_inner(request, resources, false, true, false)?;
        Ok(serde_json::json!({"mode":"windowed","admission":admission}))
    }
    fn admit_windows_inner(
        &mut self,
        request: DecodeAdmission,
        resources: &mut (impl ClassicAudioResources + ?Sized),
        preview: bool,
        pcm_audio: bool,
        preparation_only: bool,
    ) -> Result<Value, EditorError> {
        self.checked_resource(&request.resource)?;
        if self.preview.is_some() != preview
            || (!preview && !matches!(request.resource.kind.as_str(), "audio" | "video"))
        {
            return Err(invalid(
                "Windowed admission differs from the fixed execution source profile",
            ));
        }
        let primary = request
            .primary
            .clone()
            .ok_or_else(|| invalid("Missing primary audio format"))?;
        if let Some(prior) = self.window_admissions.get(&request.resource.asset_id) {
            if self.primary.get(&request.resource.asset_id) != Some(&request.primary) {
                return Err(invalid("Windowed primary audio selection changed"));
            }
            return serialize(&prior.reply);
        }
        // Preserve original format validation, occupied-byte accounting and
        // its half-budget headroom. A new operation cannot widen the policy.
        resources.checkpoint()?;
        let shared = if preparation_only {
            let remaining = self
                .prepared
                .resources()
                .filter(|r| !self.admissions.contains_key(&r.asset_id))
                .map(|r| r.asset_id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                .max(1) as u64;
            Some(self.decoder_remaining_bytes(request.resource.size) / remaining)
        } else {
            None
        };
        let old = self.admit_decoder(request.clone(), request.resource.kind == "audio", true)?;
        let working = old["maxFrames"]
            .as_u64()
            .ok_or_else(|| invalid("Invalid decoder admission"))?;
        let working = shared.map_or(working, |bytes| {
            working.min(bytes / (u64::from(primary.channels) * 4 * 16))
        });
        if working == 0 {
            return Err(EditorError::new(
                "AUDIO_BUDGET_EXCEEDED",
                "No prepared source window fits shared admission",
            ));
        }
        let target = u64::from(self.prepared.identity().format.sample_rate);
        let native = u64::from(primary.sample_rate);
        let asset = self
            .prepared
            .resources()
            .find(|r| r.asset_id == request.resource.asset_id)
            .ok_or_else(|| invalid("Unknown windowed resource"))?;
        let small = !pcm_audio
            && !preparation_only
            && asset
                .duration
                .as_ref()
                .and_then(|t| RationalTime::try_from(t).ok())
                .is_some_and(|t| {
                    t.ticks() > 0
                        && u128::from(t.ticks().unsigned_abs()) * u128::from(native.max(target))
                            <= u128::from(working) * u128::from(t.timebase())
                });
        let logical = if small {
            working
        } else {
            // Addressability is independent of PCM residency. Both native and
            // target counters must stay exactly representable in the adapter;
            // actual CPU/time and encoded-byte policies remain unchanged.
            const SAFE: u64 = (1u64 << 53) - 1;
            SAFE.min(count(
                u128::from(SAFE) * u128::from(target) / u128::from(native),
            )?)
        };
        let max_native = count(u128::from(logical) * u128::from(native) / u128::from(target))?;
        let divisor = gcd(native, target);
        let period = native / divisor;
        let mut ranges = Vec::new();
        if !small {
            for voice in self.prepared.voices().filter(|v| {
                v.resource.asset_id == request.resource.asset_id
                    && (!preparation_only
                        || v.processing != AudioProcessing::LinearInterpolationZeroTail)
            }) {
                resources.checkpoint()?;
                // A zero-availability mathematical projection gives the same
                // original authored slice/prefix without inventing a decoded
                // buffer or deriving extent from rounded placement duration.
                let facts = classic_audio_preparation_for_mode(
                    &ClassicAudioPreparationInput {
                        source_start: voice.source.clock.nominal_request.start.clone(),
                        timeline_duration: voice.timeline_duration.clone(),
                        rate: voice.source.clock.rate,
                        maintain_pitch: voice.maintain_pitch,
                        decoded_sample_rate: target as u32,
                        decoded_frames: 0,
                        decoded_channels: primary.channels.min(2),
                        target_sample_rate: target as u32,
                    },
                    &self.budget,
                    Some(0),
                    classic_audio_preparation_mode_for_processing(voice.processing)
                        .map_err(composition)?,
                )
                .map_err(composition)?;
                let first = voice
                    .source
                    .position_at(0, target as u32)
                    .map_err(composition)?
                    .lower;
                let last = voice
                    .source
                    .position_at(voice.placement_frame_count.saturating_sub(1), target as u32)
                    .map_err(composition)?
                    .lower;
                let low = first.min(facts.source_slice_start_sample);
                let high = last
                    .max(
                        facts
                            .source_slice_start_sample
                            .checked_add(facts.requested_source_frames)
                            .ok_or_else(|| invalid("Decoder window prefix overflow"))?,
                    )
                    .saturating_add(2);
                let start = count(u128::from(low) * u128::from(native) / u128::from(target))?
                    .saturating_sub(FILTER_GUARD_FRAMES)
                    .saturating_sub(period)
                    / period
                    * period;
                let end =
                    count((u128::from(high) * u128::from(native)).div_ceil(u128::from(target)))?
                        .saturating_add(FILTER_GUARD_FRAMES)
                        .min(max_native);
                if start < end {
                    ranges.push((start, end));
                }
            }
        }
        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (start, end) in ranges {
            if let Some(prior) = merged.last_mut().filter(|r| start <= r.1) {
                prior.1 = prior.1.max(end);
            } else {
                merged.push((start, end));
            }
        }
        let mut resident = 0u128;
        let mut resident_target = 0u128;
        let mut windows = Vec::new();
        for (start, end) in merged {
            let frames = end - start;
            let resampled = (u128::from(frames) * u128::from(target)).div_ceil(u128::from(native));
            // Classic's floating-point ceil may add a final frame at an exact
            // rational boundary. Reserve it, rather than changing that count.
            resident += u128::from(frames).max(resampled + 1);
            resident_target += resampled + 1;
            windows.push(DecodePcmWindow {
                source_start_frame: start.to_string(),
                source_end_frame: end.to_string(),
                // Keep one extra source period before a nonzero output start,
                // allowing the adapter to preserve the engine's fractional
                // absolute read phase rather than rounding an offset to zero.
                target_start_frame: count(
                    u128::from(if start == 0 { 0 } else { start + period }) * u128::from(target)
                        / u128::from(native),
                )?
                .to_string(),
            });
        }
        if resident > u128::from(working) {
            return Err(EditorError::new(
                "AUDIO_BUDGET_EXCEEDED",
                "Required source windows exceed original decoder working admission",
            ));
        }
        let reply = WindowDecodeAdmission {
            mode: if small { "whole" } else { "windowed" }.into(),
            max_frames: logical,
            max_channels: primary.channels,
            working_frames: working,
            source_sample_rate: primary.sample_rate,
            target_sample_rate: target as u32,
            max_native_frames: max_native.to_string(),
            windows,
        };
        resources.checkpoint()?;
        if !small {
            self.admissions.insert(
                request.resource.asset_id.clone(),
                (
                    if preview {
                        logical.max(max_native)
                    } else {
                        logical
                    },
                    primary.channels,
                ),
            );
        }
        self.window_admissions.insert(
            request.resource.asset_id,
            WindowDecodeState {
                reply: reply.clone(),
                resident_frame_limit: count(resident_target)?,
                resident_frames: None,
            },
        );
        serialize(&reply)
    }
}
