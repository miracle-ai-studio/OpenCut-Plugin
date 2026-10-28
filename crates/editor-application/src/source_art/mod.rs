//! Read-only source-art preparation and projection. Native adapters decode the
//! source; this module owns fixed selection, receipts and exact display clocks.
use opencut_contracts::{AssetKind, EditorError, scene::*, source_art::*};
use opencut_job_execution::scene_audio::TrustedJobAuthority;
use opencut_time::{RationalTime, validate_range};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;

mod ports;
mod projection;
mod validation;
pub use ports::*;
pub use validation::{
    thumbnail_size, validate_cancel, validate_policy, validate_request, validate_runtime_identity,
};
#[cfg(test)]
mod tests;

pub const SOURCE_PEAKS_ALGORITHM: &str = "source-peaks128-v1";
pub const SOURCE_FRAME_ALGORITHM: &str = "source-frame-at-jpeg08-v2";
pub const THUMBNAIL_ALGORITHM: &str = "classic-thumbnail-1s-jpeg08-v1";
pub const SOURCE_ART_DECODE_PROFILE: &str = "fixed-cas-streamed-concat-webaudio-canvas";
pub const NATIVE_SOURCE_ART_DECODE_PROFILE: &str = "fixed-cas-native-libav-concat-linear-skia-jpeg";
pub const NATIVE_SOURCE_ART_RASTERIZER: &str = "skia-safe-0.153.3-srgb-jpeg80";
pub const SOURCE_ART_BUCKET_SIZE: u32 = 128;
pub const SOURCE_ART_MAX_JPEG_BYTES: usize = 1024 * 1024;

fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", message)
}
fn receipt(message: &str) -> EditorError {
    EditorError::new("ARTIFACT_VERIFICATION_FAILED", message)
}
fn budget(message: &str) -> EditorError {
    EditorError::new("SOURCE_ART_BUDGET_EXCEEDED", message)
}
fn overflow() -> EditorError {
    EditorError::new("TIME_OVERFLOW", "Source-art sample clock overflow")
}

#[derive(Clone, Debug)]
pub struct PreparedSourceArt {
    request: SourceArtRequest,
    asset: ProjectAsset,
    authority: TrustedJobAuthority,
    scope: String,
    policy: SourceArtPolicy,
    element_clock: Option<ElementClock>,
    thumbnail_time: Option<opencut_contracts::Time>,
}

#[derive(Clone, Debug)]
struct ElementClock {
    source_start: RationalTime,
    rate: RationalRate,
}

impl PreparedSourceArt {
    /// Audio/video are decoded through one bounded verified range source.
    /// Encoded file length is never a grant to allocate full-file copies.
    pub fn range_cache_bytes(&self) -> Result<Option<u64>, EditorError> {
        if !matches!(self.asset.kind, AssetKind::Audio | AssetKind::Video) {
            return Ok(None);
        }
        let maximum = self.policy.max_encoded_bytes.min(self.policy.max_pcm_bytes);
        let cache = maximum
            .checked_sub(6 * 65536)
            .map(|n| n / 3)
            .filter(|n| *n >= 65536)
            .ok_or_else(|| budget("No bounded source-frame cache fits admission"))?
            .min(8 * 1024 * 1024);
        Ok(Some(cache))
    }
    /// PCM working memory is independent of the decoded source extent. The
    /// remaining summary share covers packed Vec growth and parsed results.
    pub fn waveform_working_bytes(&self) -> Result<u64, EditorError> {
        self.policy
            .max_pcm_bytes
            .checked_sub(self.encoded_resident_bytes()?)
            .map(|bytes| bytes / 2)
            .filter(|bytes| *bytes >= 65536)
            .ok_or_else(|| budget("No streamed waveform working memory fits admission"))
    }
    pub fn waveform_output_bytes(&self) -> Result<u64, EditorError> {
        Ok(self.waveform_working_bytes()? / 3 / 4 * 4)
    }
    pub fn encoded_resident_bytes(&self) -> Result<u64, EditorError> {
        if let Some(cache) = self.range_cache_bytes()? {
            return Ok(self.asset.version.size.min(cache) * 3 + 6 * 65536);
        }
        self.asset
            .version
            .size
            .checked_mul(3)
            .ok_or_else(|| budget("Encoded source reserve overflow"))
    }
    pub fn thumbnail_time(&self) -> Option<&opencut_contracts::Time> {
        self.thumbnail_time.as_ref()
    }
    pub fn request(&self) -> &SourceArtRequest {
        &self.request
    }
    pub fn asset(&self) -> &ProjectAsset {
        &self.asset
    }
    pub fn authority(&self) -> &TrustedJobAuthority {
        &self.authority
    }
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub fn policy(&self) -> &SourceArtPolicy {
        &self.policy
    }
}

pub fn prepare_source_art(
    store: &impl SourceArtRead,
    request: &SourceArtRequest,
) -> Result<PreparedSourceArt, EditorError> {
    validate_request(request)?;
    let authorized = store.fixed_source_art_snapshot(&request.project_id, &request.revision)?;
    validate_policy(&authorized.policy)?;
    validation::scope(&authorized.scope)?;
    if authorized.snapshot.project.id != request.project_id
        || authorized.snapshot.revision != request.revision
    {
        return Err(EditorError::new(
            "STORAGE_ERROR",
            "Source-art fixed scope differs",
        ));
    }
    opencut_editor_core::scene::validate_project_scene(&authorized.snapshot.project)?;
    let project = &authorized.snapshot.project;
    let asset = project
        .assets
        .iter()
        .find(|a| a.id == request.asset_id)
        .ok_or_else(|| EditorError::new("NOT_FOUND", "Fixed source-art asset does not exist"))?;
    let ranged = matches!(asset.kind, AssetKind::Audio | AssetKind::Video);
    if asset.version.size > 9_007_199_254_740_991
        || (!ranged && asset.version.size > authorized.policy.max_encoded_bytes)
    {
        return Err(budget("Encoded source exceeds source-art admission"));
    }
    let element_clock = match &request.query {
        SourceArtQuery::Waveform { projection, .. } => {
            if !matches!(asset.kind, AssetKind::Audio | AssetKind::Video) {
                return Err(invalid("Source peaks require an audio or video resource"));
            }
            if matches!(asset.metadata.audio, AudioPresence::Absent {}) {
                return Err(EditorError::new(
                    "NO_AUDIO_TRACK",
                    "Source resource is known to have no audio",
                ));
            }
            if let WaveformProjection::Element {
                sequence_id,
                track_id,
                element_id,
                local_range,
                ..
            } = projection
            {
                let element = project
                    .sequences
                    .iter()
                    .find(|s| s.id == *sequence_id)
                    .and_then(|s| s.tracks.iter().find(|t| t.id == *track_id))
                    .and_then(|t| t.elements.iter().find(|e| e.id == *element_id))
                    .ok_or_else(|| {
                        EditorError::new("NOT_FOUND", "Source-art element scope does not exist")
                    })?;
                let (bound, retime) = match &element.content {
                    ElementContent::Video {
                        asset_id, retime, ..
                    }
                    | ElementContent::Audio {
                        asset_id, retime, ..
                    } => (asset_id, retime),
                    _ => {
                        return Err(invalid(
                            "Element source peaks require audio or video content",
                        ));
                    }
                };
                if bound != &request.asset_id {
                    return Err(invalid(
                        "Element source art differs from its fixed resource",
                    ));
                }
                let end = RationalTime::try_from(&local_range.start)?
                    .checked_add(RationalTime::try_from(&local_range.duration)?)?;
                if end.cmp_exact(RationalTime::try_from(&element.timeline_range.duration)?)
                    == Ordering::Greater
                {
                    return Err(invalid("Source-art local window exceeds element placement"));
                }
                let source = opencut_editor_core::scene::resolve_element_source_request_scene(
                    element,
                    Some(asset),
                )?;
                let rate = opencut_composition::scene::classic_effective_rate_scene(retime.rate)
                    .map_err(|e| EditorError::new(e.code, e.message))?;
                Some(ElementClock {
                    source_start: RationalTime::try_from(&source.start)?,
                    rate,
                })
            } else {
                None
            }
        }
        SourceArtQuery::Thumbnail {} => {
            if !matches!(asset.kind, AssetKind::Video | AssetKind::Image) {
                return Err(invalid("Thumbnail requires a video or image resource"));
            }
            None
        }
    };
    store.revalidate_source_art(
        &request.project_id,
        &request.revision,
        &authorized.authority,
    )?;
    Ok(PreparedSourceArt {
        request: request.clone(),
        asset: asset.clone(),
        authority: authorized.authority,
        scope: authorized.scope,
        policy: authorized.policy,
        element_clock,
        thumbnail_time: None,
    })
}

pub fn validate_decoded_source_art(
    prepared: &PreparedSourceArt,
    decoded: &DecodedSourceArt,
) -> Result<(), EditorError> {
    validation::decoded(prepared, decoded)
}

pub fn project_source_art(
    prepared: &PreparedSourceArt,
    decoded: &DecodedSourceArt,
) -> Result<SourceArtResponse, EditorError> {
    validate_decoded_source_art(prepared, decoded)?;
    let data = match decoded {
        DecodedSourceArt::Waveform {
            sample_rate,
            channels,
            total_samples,
            bucket_size,
            amplitudes,
            ..
        } => {
            let SourceArtQuery::Waveform { projection, .. } = &prepared.request.query else {
                return Err(receipt("Source-art decoded kind differs"));
            };
            SourceArtData::Waveform {
                sample_rate: *sample_rate,
                channels: *channels,
                total_samples: total_samples.to_string(),
                bucket_size: *bucket_size,
                projection: projection.clone(),
                amplitudes: projection::project(
                    prepared,
                    *sample_rate,
                    *total_samples,
                    amplitudes,
                )?,
            }
        }
        DecodedSourceArt::ThumbnailReady {
            width,
            height,
            jpeg_bytes,
            observation,
            ..
        } => {
            let mut jpeg_bytes_hex = String::with_capacity(jpeg_bytes.len() * 2);
            use std::fmt::Write;
            for byte in jpeg_bytes {
                write!(jpeg_bytes_hex, "{byte:02x}")
                    .map_err(|_| receipt("Could not encode source-art bytes"))?;
            }
            SourceArtData::ThumbnailReady {
                width: *width,
                height: *height,
                content_hash: format!("{:x}", Sha256::digest(jpeg_bytes)),
                jpeg_bytes_hex,
                observation: observation.clone(),
            }
        }
        DecodedSourceArt::ThumbnailNoFrame {
            observation,
            reason,
            ..
        } => SourceArtData::ThumbnailNoFrame {
            observation: observation.clone(),
            reason: *reason,
        },
    };
    Ok(SourceArtResponse {
        project_id: prepared.request.project_id.clone(),
        revision: prepared.request.revision.clone(),
        asset_id: prepared.request.asset_id.clone(),
        version: prepared.asset.version.clone(),
        identity: decoded.identity().clone(),
        data,
    })
}

/// Raw summaries can be shared across fixed revisions in one trusted scope.
/// Projection and request/waiter identity are excluded; all execution budgets,
/// exact CAS version and runtime/decoder profile remain in the key.
pub fn source_art_decode_key(
    prepared: &PreparedSourceArt,
    identity: &SourceArtIdentity,
) -> Result<String, EditorError> {
    validation::identity(prepared, identity)?;
    let sample_rate = match prepared.request.query {
        SourceArtQuery::Waveform { sample_rate, .. } => Some(sample_rate),
        SourceArtQuery::Thumbnail {} => None,
    };
    let value = serde_json::json!({
        "scope": prepared.scope,
        "version": prepared.asset.version, "asset_kind": prepared.asset.kind,
        "metadata": prepared.asset.metadata, "duration": prepared.asset.duration,
        "identity": identity, "sample_rate": sample_rate,
        "max_encoded_bytes": prepared.policy.max_encoded_bytes,
        "max_pcm_bytes": prepared.policy.max_pcm_bytes,
        "max_rss_bytes": prepared.policy.max_rss_bytes,
        "deadline_ms": prepared.policy.deadline_ms,
    });
    let value = if let Some(time) = prepared.thumbnail_time() {
        serde_json::json!({"source_frame":value,"requested_time":time})
    } else {
        value
    };
    let bytes = serde_json::to_vec(&value)
        .map_err(|_| receipt("Could not identify source-art execution"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// New source-clock operation; the frozen one-second thumbnail request is unchanged.
pub fn prepare_source_frame(
    store: &impl SourceArtRead,
    input: &opencut_contracts::source_evidence::SourceFrameRequest,
) -> Result<PreparedSourceArt, EditorError> {
    let at = RationalTime::try_from(&input.time)?;
    if at.ticks() < 0 {
        return Err(invalid("Source frame time must be nonnegative"));
    }
    let mut prepared = prepare_source_art(
        store,
        &SourceArtRequest {
            project_id: input.project_id.clone(),
            revision: input.revision.clone(),
            asset_id: input.asset_id.clone(),
            request_id: input.request_id.clone(),
            query: SourceArtQuery::Thumbnail {},
        },
    )?;
    if prepared.asset.duration.as_ref().is_some_and(|duration| {
        RationalTime::try_from(duration).is_ok_and(|d| !at.cmp_exact(d).is_lt())
    }) {
        return Err(invalid(
            "Source frame point must precede the declared source end",
        ));
    }
    prepared.thumbnail_time = Some(input.time.clone());
    Ok(prepared)
}
