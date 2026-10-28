//! Compiled analysis of the exact public source inventory. No decoder, project
//! authority, I/O or alternative cache is introduced for a catalog list read.
use super::*;
use crate::source_art::{SOURCE_ART_BUCKET_SIZE, SOURCE_PEAKS_ALGORITHM};
use opencut_contracts::source_art::SourceArtIdentity;
use sha2::{Digest, Sha256};

const DECODE_PROFILE: &str = "fixed-catalog-webaudio-decode-source-peaks";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    archive_sha256: String,
    producer_sha256: String,
    identity: SourceArtIdentity,
    entries: Vec<Analysis>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Analysis {
    name: String,
    source_sha256: String,
    sample_rate: u32,
    channels: u32,
    total_samples: String,
    bucket_size: u32,
    amplitudes: Vec<f32>,
    audition_duration_seconds: f64,
}

fn verified_inventory(raw: &str) -> Result<Vec<(String, CatalogSoundAnalysis)>, EditorError> {
    let inventory: Inventory =
        serde_json::from_str(raw).map_err(|_| invalid("Invalid compiled sound analysis"))?;
    let SourceArtIdentity::Browser(identity) = &inventory.identity else {
        return Err(invalid(
            "Compiled sound inventory requires its exact historical proof",
        ));
    };
    let hash = |s: &str| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if inventory.archive_sha256 != KENNEY_SHA256
        || inventory.producer_sha256
            != format!(
                "{:x}",
                Sha256::digest(include_bytes!(
                    "../../../../scripts/generate-catalog-sound-art.mjs"
                ))
            )
        || inventory.entries.len() != sound_sources().len()
        || identity.algorithm != SOURCE_PEAKS_ALGORITHM
        || identity.decode_profile != DECODE_PROFILE
        || identity.browser_version.is_empty()
        || identity.browser_version.len() > 128
        || !identity
            .browser_version
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
        || [
            &identity.browser_sha256,
            &identity.node_sha256,
            &identity.module_sha256,
        ]
        .iter()
        .any(|s| !hash(s))
    {
        return Err(invalid("Compiled sound analysis identity differs"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(inventory.entries.len());
    for analysis in inventory.entries {
        let source = sound_source(&format!("kenney-ui-audio:{}", analysis.name))?;
        let samples = analysis
            .total_samples
            .parse::<u64>()
            .map_err(|_| invalid("Invalid compiled sound sample count"))?;
        if !seen.insert(analysis.name.clone())
            || analysis.source_sha256 != source.sha256
            || analysis.sample_rate != 48000
            || !(1..=64).contains(&analysis.channels)
            || samples == 0
            || samples > 48000 * 60
            || samples.to_string() != analysis.total_samples
            || analysis.bucket_size != SOURCE_ART_BUCKET_SIZE
            || !analysis.audition_duration_seconds.is_finite()
            || analysis.audition_duration_seconds <= 0.0
            || analysis.audition_duration_seconds > 60.0
            || analysis.amplitudes.len() as u64
                != samples.div_ceil(u64::from(SOURCE_ART_BUCKET_SIZE))
            || analysis
                .amplitudes
                .iter()
                .any(|p| !p.is_finite() || *p < 0.0)
        {
            return Err(invalid(
                "Compiled sound analysis does not match pinned source",
            ));
        }
        result.push((
            analysis.name,
            CatalogSoundAnalysis {
                source_sha256: source.sha256.clone(),
                source_size: source.size as u64,
                audition_duration_seconds: analysis.audition_duration_seconds,
                producer_sha256: inventory.producer_sha256.clone(),
                identity: inventory.identity.clone(),
                sample_rate: analysis.sample_rate,
                channels: analysis.channels,
                total_samples: analysis.total_samples,
                bucket_size: analysis.bucket_size,
                amplitudes: analysis.amplitudes,
            },
        ));
    }
    Ok(result)
}

pub(super) fn for_source(source: &PinnedSound) -> CatalogSoundAnalysis {
    static ANALYSES: OnceLock<Vec<(String, CatalogSoundAnalysis)>> = OnceLock::new();
    ANALYSES
        .get_or_init(|| {
            verified_inventory(include_str!("sound-art.json"))
                .expect("reviewed pinned sound analysis must match its complete source inventory")
        })
        .iter()
        .find(|(name, _)| name == &source.name)
        .expect("every pinned sound needs its own analysis")
        .1
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_pinned_sounds_have_exact_source_clocks_and_unmodified_peak_extent() {
        let inventory = verified_inventory(include_str!("sound-art.json")).unwrap();
        assert_eq!(inventory.len(), 51);
        for source in sound_sources() {
            let analysis = for_source(source);
            assert_eq!(analysis.source_sha256, source.sha256);
            assert_eq!(analysis.source_size, source.size as u64);
            assert!(analysis.audition_duration_seconds > 0.0);
            assert_eq!(analysis.producer_sha256.len(), 64);
        }
    }
    #[test]
    fn changed_pin_clock_bucket_identity_or_missing_entry_is_never_reused() {
        let original: serde_json::Value =
            serde_json::from_str(include_str!("sound-art.json")).unwrap();
        for mutation in 0..8 {
            let mut value = original.clone();
            match mutation {
                0 => value["entries"][0]["source_sha256"] = serde_json::json!("a".repeat(64)),
                1 => value["entries"][0]["total_samples"] = serde_json::json!("01"),
                2 => value["entries"][0]["bucket_size"] = serde_json::json!(256),
                3 => {
                    value["entries"].as_array_mut().unwrap().pop();
                }
                4 => value["identity"]["algorithm"] = serde_json::json!("invented-peaks"),
                5 => value["entries"][0]["amplitudes"][0] = serde_json::json!(-0.1),
                6 => value["entries"][0]["sample_rate"] = serde_json::json!(44100),
                _ => value["entries"][1] = value["entries"][0].clone(),
            }
            assert!(
                verified_inventory(&value.to_string()).is_err(),
                "mutation {mutation}"
            );
        }
    }
}
