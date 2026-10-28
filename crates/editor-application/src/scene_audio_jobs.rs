//! Fixed V4 audio submission and safe result validation; no files or processes.
use crate::{JobRecord, JobRequest};
use opencut_composition::scene::{
    AudioExecutionProfile, AudioProcessing, PcmFormat, PreparedAudio, prepare_audio_scene,
};
use opencut_contracts::{AssetKind, AssetLockManifest, EditorError, scene::EditorSnapshot};
use opencut_job_contracts::ArtifactDescriptor;
pub use opencut_job_contracts::scene_audio::*;
pub use opencut_job_execution::scene_audio::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub struct AuthorizedAudioSnapshot {
    pub snapshot: EditorSnapshot,
    pub authority: TrustedJobAuthority,
    pub processing_policy: LocalAudioExecutionPolicy,
}
pub trait AudioJobSubmissionStore {
    /// Read + RunJobs, current live project and this exact V4 revision share a
    /// consistent read boundary. Authority and admission come from the host.
    fn fixed_scene_audio_snapshot(
        &self,
        project_id: &str,
        revision: &str,
    ) -> Result<AuthorizedAudioSnapshot, EditorError>;
    /// Must reauthorize before inspecting a workspace-scoped idempotent receipt.
    fn find_accepted_scene_audio_job(
        &self,
        request: &JobRequest,
    ) -> Result<Option<JobRecord>, EditorError>;
    /// Recheck request/prepared binding and live authority under the write lock;
    /// atomically persist job, project binding, receipt, event and audit.
    fn accept_scene_audio_job(
        &mut self,
        request: JobRequest,
        prepared: PreparedAudioWav,
    ) -> Result<JobRecord, EditorError>;
}
pub trait AudioAssetCapture {
    /// Verify only the requested participating V4 CAS resources. Keep the shared
    /// collection gate through queue acceptance; do not use legacy locators.
    fn capture(
        &self,
        snapshot: &EditorSnapshot,
        sequence_id: &str,
        asset_ids: &[String],
    ) -> Result<AssetLockManifest, EditorError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredAudioWavResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_decoder_evidence_v1: Option<crate::classic_audio::ClassicAudioDecoderEvidence>,
    pub public: AudioWavResult,
    /// Attempt-local storage location. Never serialize this record to a client.
    pub artifact: ArtifactDescriptor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredAudioRangeWavResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_decoder_evidence_v1: Option<crate::classic_audio::ClassicAudioDecoderEvidence>,
    pub public: AudioRangeWavResult,
    /// Attempt-local descriptor; only the public metadata is projected.
    pub artifact: ArtifactDescriptor,
}

fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", message)
}
fn stored_invalid() -> EditorError {
    EditorError::new(
        "INVALID_STORED_RESULT",
        "Stored V4 audio result does not match its fixed input",
    )
}
fn valid_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}
fn hash(value: &impl Serialize) -> Result<String, EditorError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| EditorError::new("SERIALIZATION_ERROR", "Could not hash V4 audio input"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn validate_submission(input: &SubmitAudioWav) -> Result<(), EditorError> {
    if ![
        &input.idempotency_key,
        &input.project_id,
        &input.revision,
        &input.sequence_id,
    ]
    .into_iter()
    .all(|id| valid_id(id))
        || !matches!(input.sample_rate, 44_100 | 48_000)
    {
        return Err(invalid(
            "V4 audio requires bounded identifiers and 44100 or 48000 Hz",
        ));
    }
    Ok(())
}
pub fn validate_range_submission(input: &SubmitAudioRangeWav) -> Result<(), EditorError> {
    validate_submission(&range_base(input))?;
    let start = crate::classic_audio::decimal(&input.range.start_sample)?;
    let count = crate::classic_audio::decimal(&input.range.frame_count)?;
    if count == 0 || start.checked_add(count).is_none() {
        return Err(invalid(
            "Audio range requires a nonempty bounded sample interval",
        ));
    }
    Ok(())
}
fn range_base(input: &SubmitAudioRangeWav) -> SubmitAudioWav {
    SubmitAudioWav {
        idempotency_key: input.idempotency_key.clone(),
        project_id: input.project_id.clone(),
        revision: input.revision.clone(),
        sequence_id: input.sequence_id.clone(),
        sample_rate: input.sample_rate,
    }
}
/// Canonical conversion does not change sample indices or quantize time.
pub fn classic_output_range(
    input: &PreparedAudioWav,
) -> Option<crate::classic_audio::ClassicAudioRange> {
    input
        .output_range
        .as_ref()
        .map(|r| crate::classic_audio::ClassicAudioRange {
            start_sample: r.start_sample.clone(),
            frame_count: r.frame_count.clone(),
        })
}

pub fn validate_policy(policy: &LocalAudioExecutionPolicy) -> Result<(), EditorError> {
    if policy.max_pcm_bytes == 0
        || policy.max_pcm_bytes > 512 * 1024 * 1024
        || policy.max_encoded_bytes == 0
        || policy.max_encoded_bytes > 32 * 1024 * 1024
        || policy.max_rss_bytes == 0
        || !(1..=300_000).contains(&policy.deadline_ms)
    {
        return Err(invalid("Invalid host Classic audio admission"));
    }
    Ok(())
}
fn validate_authority(authority: &TrustedJobAuthority) -> Result<(), EditorError> {
    if let TrustedJobAuthority::Credential {
        principal_id,
        credential_id,
    } = authority
        && (!valid_id(principal_id) || !valid_id(credential_id))
    {
        return Err(invalid("Invalid trusted job authority"));
    }
    Ok(())
}
fn prepare(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    sample_rate: u32,
) -> Result<PreparedAudio, EditorError> {
    if !matches!(sample_rate, 44_100 | 48_000)
        || !valid_id(sequence_id)
        || !valid_id(&snapshot.revision)
    {
        return Err(invalid("Invalid V4 audio preparation identity"));
    }
    prepare_audio_scene(
        snapshot,
        sequence_id,
        AudioExecutionProfile::ClassicV31,
        PcmFormat {
            sample_rate,
            channels: 2,
        },
    )
    .map_err(|e| EditorError::new(e.code, e.message))
}
fn expected_assets(prepared: &PreparedAudio) -> BTreeMap<String, (String, u64)> {
    prepared
        .resources()
        .map(|r| {
            (
                r.asset_id.clone(),
                (r.version.content_hash.clone(), r.version.size),
            )
        })
        .collect()
}
fn validate_lock(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    prepared: &PreparedAudio,
    lock: &AssetLockManifest,
) -> Result<(), EditorError> {
    let expected = expected_assets(prepared);
    let observed: BTreeMap<_, _> = lock
        .assets
        .iter()
        .map(|a| (a.asset_id.clone(), (a.content_hash.clone(), a.size)))
        .collect();
    if lock.version != 1
        || lock.revision != snapshot.revision
        || lock.sequence_id != sequence_id
        || observed.len() != lock.assets.len()
        || observed != expected
    {
        return Err(EditorError::new(
            "ASSET_LOCK_INVALID",
            "Audio lock differs from fixed V4 participating resources",
        ));
    }
    Ok(())
}
/// Complete snapshot serialization preserves authored values/timebases and all
/// non-audio fields. Hashing a reduced voice plan would not identify the revision.
pub fn render_input_sha256(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    sample_rate: u32,
) -> Result<String, EditorError> {
    #[derive(Serialize)]
    struct Input<'a> {
        schema_version: u32,
        snapshot: &'a EditorSnapshot,
        sequence_id: &'a str,
        sample_rate: u32,
        channels: u32,
        profile: AudioProfile,
        sample_format: AudioSampleFormat,
    }
    hash(&Input {
        schema_version: 1,
        snapshot,
        sequence_id,
        sample_rate,
        channels: 2,
        profile: AudioProfile::ClassicExportV31WebAudio,
        sample_format: AudioSampleFormat::IeeeFloat32Le,
    })
}
pub fn render_range_input_sha256(
    snapshot: &EditorSnapshot,
    sequence_id: &str,
    sample_rate: u32,
    range: &AudioSampleRange,
) -> Result<String, EditorError> {
    // Keep the established whole-scene digest untouched and bind the range in a
    // separate versioned domain. Neither authority nor machine policy is public.
    #[derive(Serialize)]
    struct RangeInput<'a> {
        schema_version: u32,
        kind: &'static str,
        whole_scene_sha256: String,
        range: &'a AudioSampleRange,
    }
    hash(&RangeInput {
        schema_version: 1,
        kind: AUDIO_RANGE_WAV_KIND,
        whole_scene_sha256: render_input_sha256(snapshot, sequence_id, sample_rate)?,
        range,
    })
}

pub fn validate_prepared(input: &PreparedAudioWav) -> Result<PreparedAudio, EditorError> {
    validate_policy(&input.processing_policy)?;
    validate_authority(&input.authority)?;
    let prepared = prepare(&input.snapshot, &input.sequence_id, input.sample_rate)?;
    crate::classic_audio::ClassicAudioSession::new_with_range(
        &prepared,
        input.processing_policy.max_pcm_bytes,
        input.processing_policy.max_encoded_bytes,
        classic_output_range(input),
    )?;
    validate_lock(
        &input.snapshot,
        &input.sequence_id,
        &prepared,
        &input.asset_lock,
    )?;
    Ok(prepared)
}
pub fn validate_prepared_request(
    request: &JobRequest,
    prepared: &PreparedAudioWav,
) -> Result<(), EditorError> {
    let input = match request.kind.as_str() {
        AUDIO_WAV_KIND => {
            let input: SubmitAudioWav = serde_json::from_value(request.input.clone())
                .map_err(|_| invalid("Invalid public V4 audio request"))?;
            validate_submission(&input)?;
            if prepared.output_range.is_some() {
                return Err(invalid("Whole-scene input cannot contain an output range"));
            }
            input
        }
        AUDIO_RANGE_WAV_KIND => {
            let input: SubmitAudioRangeWav = serde_json::from_value(request.input.clone())
                .map_err(|_| invalid("Invalid public V4 audio range request"))?;
            validate_range_submission(&input)?;
            if prepared.output_range.as_ref().is_none_or(|r| {
                r.start_sample != input.range.start_sample
                    || r.frame_count != input.range.frame_count
            }) {
                return Err(invalid(
                    "Range request differs from its private preparation",
                ));
            }
            range_base(&input)
        }
        _ => return Err(invalid("Unsupported V4 audio job kind")),
    };
    if request.idempotency_key != input.idempotency_key
        || input.project_id != prepared.snapshot.project.id
        || input.revision != prepared.snapshot.revision
        || input.sequence_id != prepared.sequence_id
        || input.sample_rate != prepared.sample_rate
    {
        return Err(invalid(
            "V4 audio request differs from its private preparation",
        ));
    }
    validate_prepared(prepared)?;
    Ok(())
}
pub fn submit_audio_wav(
    store: &mut impl AudioJobSubmissionStore,
    assets: &impl AudioAssetCapture,
    input: SubmitAudioWav,
) -> Result<JobRecord, EditorError> {
    validate_submission(&input)?;
    let request = JobRequest {
        idempotency_key: input.idempotency_key.clone(),
        kind: AUDIO_WAV_KIND.into(),
        input: serde_json::to_value(&input)
            .map_err(|_| invalid("Could not serialize V4 audio request"))?,
    };
    submit(store, assets, input, request, None)
}
pub fn submit_audio_range_wav(
    store: &mut impl AudioJobSubmissionStore,
    assets: &impl AudioAssetCapture,
    input: SubmitAudioRangeWav,
) -> Result<JobRecord, EditorError> {
    validate_range_submission(&input)?;
    let request = JobRequest {
        idempotency_key: input.idempotency_key.clone(),
        kind: AUDIO_RANGE_WAV_KIND.into(),
        input: serde_json::to_value(&input)
            .map_err(|_| invalid("Could not serialize V4 audio range request"))?,
    };
    let range = PreparedAudioSampleRange {
        start_sample: input.range.start_sample.clone(),
        frame_count: input.range.frame_count.clone(),
    };
    submit(store, assets, range_base(&input), request, Some(range))
}
fn submit(
    store: &mut impl AudioJobSubmissionStore,
    assets: &impl AudioAssetCapture,
    input: SubmitAudioWav,
    request: JobRequest,
    output_range: Option<PreparedAudioSampleRange>,
) -> Result<JobRecord, EditorError> {
    let authorized = store.fixed_scene_audio_snapshot(&input.project_id, &input.revision)?;
    if authorized.snapshot.project.id != input.project_id
        || authorized.snapshot.revision != input.revision
    {
        return Err(invalid(
            "Authorized snapshot differs from requested fixed revision",
        ));
    }
    opencut_editor_core::scene::validate_project_scene(&authorized.snapshot.project)?;
    if let Some(existing) = store.find_accepted_scene_audio_job(&request)? {
        return Ok(existing);
    }
    validate_policy(&authorized.processing_policy)?;
    validate_authority(&authorized.authority)?;
    let prepared = prepare(&authorized.snapshot, &input.sequence_id, input.sample_rate)?;
    crate::classic_audio::ClassicAudioSession::new_with_range(
        &prepared,
        authorized.processing_policy.max_pcm_bytes,
        authorized.processing_policy.max_encoded_bytes,
        output_range
            .as_ref()
            .map(|r| crate::classic_audio::ClassicAudioRange {
                start_sample: r.start_sample.clone(),
                frame_count: r.frame_count.clone(),
            }),
    )?;
    let ids: Vec<_> = expected_assets(&prepared).into_keys().collect();
    let asset_lock = assets.capture(&authorized.snapshot, &input.sequence_id, &ids)?;
    validate_lock(
        &authorized.snapshot,
        &input.sequence_id,
        &prepared,
        &asset_lock,
    )?;
    store.accept_scene_audio_job(
        request,
        PreparedAudioWav {
            snapshot: authorized.snapshot,
            sequence_id: input.sequence_id,
            sample_rate: input.sample_rate,
            asset_lock,
            authority: authorized.authority,
            processing_policy: authorized.processing_policy,
            output_range,
        },
    )
}

pub fn validate_stored_result(
    input: &PreparedAudioWav,
    stored: &StoredAudioWavResult,
) -> Result<AudioWavResult, EditorError> {
    if input.output_range.is_some() {
        return Err(stored_invalid());
    }
    let prepared = validate_prepared(input).map_err(|_| stored_invalid())?;
    let public = &stored.public;
    let identity = &public.identity;
    if public.schema_version != 1
        || identity.project_id != input.snapshot.project.id
        || identity.revision != input.snapshot.revision
        || identity.sequence_id != input.sequence_id
        || identity.sample_rate != input.sample_rate
        || identity.channels != 2
        || identity.frame_count != prepared.duration_samples().to_string()
    {
        return Err(stored_invalid());
    }
    validate_result_common(
        input,
        &prepared,
        ResultEvidence {
            asset_lock: &public.asset_lock,
            input_sha256: &public.render_input_sha256,
            expected_sha256: &render_input_sha256(
                &input.snapshot,
                &input.sequence_id,
                input.sample_rate,
            )?,
            execution: &public.execution,
            decoder_evidence: stored.audio_decoder_evidence_v1.as_ref(),
            public_artifact: &public.artifact,
            artifact: &stored.artifact,
        },
        prepared.duration_samples(),
    )?;
    Ok(public.clone())
}
pub fn validate_stored_range_result(
    input: &PreparedAudioWav,
    stored: &StoredAudioRangeWavResult,
) -> Result<AudioRangeWavResult, EditorError> {
    let prepared = validate_prepared(input).map_err(|_| stored_invalid())?;
    let range = input.output_range.as_ref().ok_or_else(stored_invalid)?;
    let public = &stored.public;
    let identity = &public.identity;
    if public.schema_version != 1
        || identity.project_id != input.snapshot.project.id
        || identity.revision != input.snapshot.revision
        || identity.sequence_id != input.sequence_id
        || identity.sample_rate != input.sample_rate
        || identity.channels != 2
        || identity.start_sample != range.start_sample
        || identity.frame_count != range.frame_count
        || identity.whole_scene_frame_count != prepared.duration_samples().to_string()
    {
        return Err(stored_invalid());
    }
    let public_range = AudioSampleRange {
        start_sample: range.start_sample.clone(),
        frame_count: range.frame_count.clone(),
    };
    validate_result_common(
        input,
        &prepared,
        ResultEvidence {
            asset_lock: &public.asset_lock,
            input_sha256: &public.render_input_sha256,
            expected_sha256: &render_range_input_sha256(
                &input.snapshot,
                &input.sequence_id,
                input.sample_rate,
                &public_range,
            )?,
            execution: &public.execution,
            decoder_evidence: stored.audio_decoder_evidence_v1.as_ref(),
            public_artifact: &public.artifact,
            artifact: &stored.artifact,
        },
        crate::classic_audio::decimal(&range.frame_count)?,
    )?;
    Ok(public.clone())
}
struct ResultEvidence<'a> {
    asset_lock: &'a AssetLockManifest,
    input_sha256: &'a str,
    expected_sha256: &'a str,
    execution: &'a AudioExecutionSummary,
    decoder_evidence: Option<&'a crate::classic_audio::ClassicAudioDecoderEvidence>,
    public_artifact: &'a AudioArtifactMetadata,
    artifact: &'a ArtifactDescriptor,
}
fn validate_result_common(
    input: &PreparedAudioWav,
    prepared: &PreparedAudio,
    evidence: ResultEvidence<'_>,
    output_frames: u64,
) -> Result<(), EditorError> {
    let ResultEvidence {
        asset_lock,
        input_sha256,
        expected_sha256,
        execution,
        decoder_evidence,
        public_artifact,
        artifact,
    } = evidence;
    // Validate the exact Float32 stereo container size from the pure layout.
    // Actual RIFF/RF64 bytes still require independent container acceptance.
    let expected_wav_size =
        crate::classic_audio::ClassicAudioWavLayout::new(input.sample_rate, output_frames)
            .map_err(|_| stored_invalid())?
            .total_bytes;
    validate_execution_summary(prepared, execution, decoder_evidence)?;
    if asset_lock != &input.asset_lock
        || input_sha256 != expected_sha256
        || artifact.mime_type != "audio/wav"
        || public_artifact.mime_type != "audio/wav"
        || artifact.size != expected_wav_size
        || public_artifact.size_bytes != artifact.size.to_string()
        || public_artifact.content_hash != artifact.content_hash
        || !valid_hash(&artifact.content_hash)
    {
        return Err(stored_invalid());
    }
    Ok(())
}

fn validate_decoder_evidence(
    prepared: &PreparedAudio,
    evidence: &crate::classic_audio::ClassicAudioDecoderEvidence,
    native: bool,
) -> Result<BTreeSet<AudioDecoderProfile>, EditorError> {
    let resources: BTreeMap<_, _> = prepared.resources().map(|r| (&r.asset_id, r)).collect();
    if evidence.sources.len() != resources.len() || evidence.sources.len() > 32 {
        return Err(stored_invalid());
    }
    let mut seen = BTreeSet::new();
    let mut profiles = BTreeSet::new();
    let mut negotiated_pcm = false;
    for source in &evidence.sources {
        let r = resources
            .get(&source.resource.asset_id)
            .ok_or_else(stored_invalid)?;
        let d = &source.decoder;
        let kind = match r.kind {
            AssetKind::Audio => "audio",
            AssetKind::Video => "video",
            _ => return Err(stored_invalid()),
        };
        if !seen.insert(&r.asset_id)
            || source.resource.content_hash != r.version.content_hash
            || source.resource.size != r.version.size
            || source.resource.kind != kind
            || d.asset_id != r.asset_id
            || d.sample_rate != prepared.identity().format.sample_rate
            || !(1..=64).contains(&d.channels)
            || crate::classic_audio::decimal(&d.frame_count).map_err(|_| stored_invalid())? == 0
        {
            return Err(stored_invalid());
        }
        let profile = if native {
            match (&r.kind, d.profile.as_str(), d.primary.as_ref()) {
                (AssetKind::Audio, "native-libav-sinc-at-target-rate", None) => {
                    AudioDecoderProfile::NativeLibavSincAtTargetRate
                }
                (
                    AssetKind::Audio | AssetKind::Video,
                    mode @ ("native-libav-concatenate-linear-resample"
                    | "native-libav-windowed-concatenate-linear-resample"),
                    Some(p),
                ) if !p.codec.is_empty()
                    && p.codec.len() <= 128
                    && !p.codec.chars().any(char::is_control)
                    && (1..=64).contains(&p.channels)
                    && (1..=384000).contains(&p.sample_rate)
                    && d.channels == p.channels.min(2) =>
                {
                    if mode == "native-libav-windowed-concatenate-linear-resample" {
                        AudioDecoderProfile::NativeLibavWindowedConcatenateLinearResample
                    } else {
                        AudioDecoderProfile::NativeLibavConcatenateLinearResample
                    }
                }
                _ => return Err(stored_invalid()),
            }
        } else {
            match (&r.kind, d.profile.as_str(), d.primary.as_ref()) {
                (AssetKind::Audio, "web-audio-decode-at-target-rate", None) => {
                    AudioDecoderProfile::WebAudioDecodeAtTargetRate
                }
                (
                    AssetKind::Audio,
                    "mediabunny-1.41.0-windowed-concatenate-offline-resample-v1",
                    Some(p),
                ) => {
                    let sample_bytes = crate::classic_audio::native_pcm_sample_bytes(&p.codec)
                        .ok_or_else(stored_invalid)?;
                    if p.sample_rate != d.sample_rate
                        || !(1..=2).contains(&p.channels)
                        || d.channels != p.channels
                        || crate::classic_audio::decimal(&d.frame_count)
                            .map_err(|_| stored_invalid())?
                            > r.version.size / (u64::from(p.channels) * sample_bytes)
                    {
                        return Err(stored_invalid());
                    }
                    negotiated_pcm = true;
                    AudioDecoderProfile::Mediabunny141ConcatenateOfflineResample
                }
                (
                    AssetKind::Video,
                    "mediabunny-1.41.0-concatenate-offline-resample"
                    | "mediabunny-1.41.0-windowed-concatenate-offline-resample-v1",
                    Some(p),
                ) if !p.codec.is_empty()
                    && (1..=64).contains(&p.channels)
                    && (8_000..=192_000).contains(&p.sample_rate)
                    && d.channels == p.channels.min(2) =>
                {
                    AudioDecoderProfile::Mediabunny141ConcatenateOfflineResample
                }
                _ => return Err(stored_invalid()),
            }
        };
        profiles.insert(profile);
    }
    if !native && !negotiated_pcm {
        return Err(stored_invalid());
    }
    Ok(profiles)
}

/// Shared whole-scene Classic execution receipt validation for audio and video.
pub(crate) fn validate_execution_summary(
    prepared: &PreparedAudio,
    execution: &AudioExecutionSummary,
    evidence: Option<&crate::classic_audio::ClassicAudioDecoderEvidence>,
) -> Result<(), EditorError> {
    let native = matches!(&execution.runtime, AudioRuntimeIdentity::Native(_));
    let decoder_profiles: BTreeSet<_> = if let Some(evidence) = evidence {
        validate_decoder_evidence(prepared, evidence, native)?
    } else {
        if native {
            return Err(stored_invalid());
        }
        prepared
            .resources()
            .map(|r| match r.kind {
                AssetKind::Audio => Ok(AudioDecoderProfile::WebAudioDecodeAtTargetRate),
                AssetKind::Video => {
                    Ok(AudioDecoderProfile::Mediabunny141ConcatenateOfflineResample)
                }
                _ => Err(stored_invalid()),
            })
            .collect::<Result<_, _>>()?
    };
    let pitch_profiles: BTreeSet<_> = prepared
        .voices()
        .map(|v| match v.processing {
            AudioProcessing::LinearInterpolationZeroTail => Ok(AudioPitchProfile::UnpreparedLinear),
            AudioProcessing::ClassicPreparedLinear => Ok(AudioPitchProfile::PreparedLinear),
            AudioProcessing::NativeMaintainPitch => Ok(AudioPitchProfile::PreparedSoundtouch),
            _ => Err(stored_invalid()),
        })
        .collect::<Result<_, _>>()?;
    let observed_decoders: BTreeSet<_> = execution.decoder_profiles.iter().copied().collect();
    let observed_pitch: BTreeSet<_> = execution.pitch_profiles.iter().copied().collect();
    match &execution.runtime {
        AudioRuntimeIdentity::Native(identity) => {
            crate::classic_audio::validate_native_identity(identity)
                .map_err(|_| stored_invalid())?
        }
        AudioRuntimeIdentity::HistoricalBrowser(identity) => {
            if identity.playwright_version != "1.63.0"
                || identity.mediabunny_version != "1.41.0"
                || identity.soundtouch_version != "0.3.0"
                || !valid_id(&identity.browser_version)
                || !valid_hash(&identity.browser_sha256)
                || !valid_hash(&identity.module_sha256)
            {
                return Err(stored_invalid());
            }
        }
    }
    if !valid_hash(&execution.host_sha256)
        || observed_decoders != decoder_profiles
        || observed_decoders.len() != execution.decoder_profiles.len()
        || observed_pitch != pitch_profiles
        || observed_pitch.len() != execution.pitch_profiles.len()
    {
        return Err(stored_invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
