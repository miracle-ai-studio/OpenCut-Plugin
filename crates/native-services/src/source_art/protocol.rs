//! One private source-execution plan; projections and authored clocks never
//! enter the worker. Every source byte still crosses the parent's live gate.
use super::*;
use opencut_contracts::{AssetKind, AssetVersion, Time};
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub asset_id: String,
    pub version: AssetVersion,
    pub kind: AssetKind,
    pub identity: SourceArtIdentity,
    pub policy: SourceArtPolicy,
    pub encoded_resident_bytes: u64,
    pub query: Query,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Query {
    Waveform {
        sample_rate: u32,
        working_bytes: u64,
        output_bytes: u64,
    },
    Thumbnail {
        time: Option<Time>,
    },
}
impl Plan {
    pub fn from_prepared(
        value: &PreparedSourceArt,
        identity: SourceArtIdentity,
    ) -> Result<Self, EditorError> {
        Ok(Self {
            asset_id: value.asset().id.clone(),
            version: value.asset().version.clone(),
            kind: value.asset().kind.clone(),
            identity,
            policy: value.policy().clone(),
            encoded_resident_bytes: value.encoded_resident_bytes()?,
            query: match value.request().query {
                SourceArtQuery::Waveform { sample_rate, .. } => Query::Waveform {
                    sample_rate,
                    working_bytes: value.waveform_working_bytes()?,
                    output_bytes: value.waveform_output_bytes()?,
                },
                SourceArtQuery::Thumbnail {} => Query::Thumbnail {
                    time: value.thumbnail_time().cloned(),
                },
            },
        })
    }
    pub fn validate(&self) -> Result<(), EditorError> {
        validate_policy(&self.policy)?;
        validate_identity(&self.identity)?;
        if self.asset_id.is_empty()
            || self.asset_id.len() > 1024
            || self.version.size > (1u64 << 53) - 1
            || self.version.content_hash.len() != 64
            || self.encoded_resident_bytes >= self.policy.max_pcm_bytes
        {
            return Err(protocol(
                "Native source-art plan exceeds its captured scope",
            ));
        }
        let algorithm = match self.query {
            Query::Waveform {
                sample_rate,
                working_bytes,
                output_bytes,
            } => {
                if !matches!(self.kind, AssetKind::Audio | AssetKind::Video)
                    || ![44100, 48000].contains(&sample_rate)
                    || working_bytes
                        != (self.policy.max_pcm_bytes - self.encoded_resident_bytes) / 2
                    || working_bytes < 65536
                    || output_bytes != working_bytes / 3 / 4 * 4
                {
                    return Err(protocol("Native waveform working facts changed"));
                }
                SOURCE_PEAKS_ALGORITHM
            }
            Query::Thumbnail { ref time } => {
                if !matches!(self.kind, AssetKind::Image | AssetKind::Video) {
                    return Err(protocol("Native thumbnail asset kind changed"));
                }
                if let Some(time) = time {
                    let at = opencut_time::RationalTime::try_from(time)?;
                    if at.ticks() < 0 {
                        return Err(protocol("Negative native source frame time"));
                    }
                    SOURCE_FRAME_ALGORITHM
                } else {
                    THUMBNAIL_ALGORITHM
                }
            }
        };
        if self.identity.algorithm() != algorithm {
            return Err(protocol(
                "Native source-art algorithm differs from its request",
            ));
        }
        Ok(())
    }
    pub fn output_limit(&self) -> u64 {
        match self.query {
            Query::Waveform { output_bytes, .. } => output_bytes,
            Query::Thumbnail { .. } => SOURCE_ART_MAX_JPEG_BYTES as u64,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Receipt {
    Waveform {
        identity: SourceArtIdentity,
        sample_rate: u32,
        channels: u32,
        total_samples: String,
        bucket_size: u32,
        output_bytes: usize,
    },
    ThumbnailReady {
        identity: SourceArtIdentity,
        width: u32,
        height: u32,
        observation: SourceArtThumbnailObservation,
        output_bytes: usize,
    },
    ThumbnailNoFrame {
        identity: SourceArtIdentity,
        observation: SourceArtThumbnailObservation,
        reason: SourceArtNoFrameReason,
        output_bytes: usize,
    },
}
