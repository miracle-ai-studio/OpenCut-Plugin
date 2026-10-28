use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
fn floats(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(bytes.len() % 4, 0);
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}
fn errors(actual: &[f32], expected: &[f32]) -> (f64, f64) {
    assert_eq!(actual.len(), expected.len());
    let (mut peak, mut noise, mut signal) = (0.0f64, 0.0f64, 0.0f64);
    for (a, b) in actual.iter().zip(expected) {
        assert!(a.is_finite() && b.is_finite());
        let delta = f64::from(*a) - f64::from(*b);
        peak = peak.max(delta.abs());
        noise += delta * delta;
        signal += f64::from(*b) * f64::from(*b);
    }
    let snr = if noise == 0.0 {
        300.0
    } else {
        10.0 * (signal.max(f64::MIN_POSITIVE) / noise).log10()
    };
    (peak, snr)
}
#[test]
fn peak_gate_keeps_js_number_threshold_and_processor_memory_is_released() {
    assert_eq!(mastering_required(&[0.5, -0.5]).unwrap(), (0.5, false));
    assert!(mastering_required(&[0.98f32]).unwrap().1);
    assert!(mastering_required(&[f32::NAN]).is_err());
    let memory = PcmMemory::new(65536).unwrap();
    {
        let compressor = ClassicCompressor::new(44100, &memory).unwrap();
        assert_eq!(compressor.lookahead_frames(), 264);
        assert_eq!(memory.live_bytes(), 8192);
    }
    assert_eq!(memory.live_bytes(), 0);
    {
        let compressor = ClassicCompressor::new(48000, &memory).unwrap();
        assert_eq!(compressor.lookahead_frames(), 288);
    }
    assert_eq!(memory.live_bytes(), 0);
}
#[test]
fn native_dsp_limits_and_checkpoint_errors_are_not_silent_fallbacks() {
    let memory = PcmMemory::new(8192).unwrap();
    assert!(SoundTouch::new(f64::NAN, &memory).is_err());
    let mut processor = SoundTouch::new(2.0, &memory).unwrap();
    let mut output = [0.0; 32];
    let error = processor
        .extract(&[0.0; 2], &mut output, &mut || Err(invalid("revoked")))
        .unwrap_err();
    assert_eq!(error.message, "revoked");
    drop(processor);
    assert_eq!(memory.live_bytes(), 0);
    let mut compressor = ClassicCompressor::new(48000, &memory).unwrap();
    let error = compressor
        .process(&mut output, &mut || Err(invalid("cancelled")))
        .unwrap_err();
    assert_eq!(error.message, "cancelled");
    assert!(compressor.process(&mut [0.0; 3], &mut || Ok(())).is_err());
}
/// Captured once from the pinned independent Chromium/SoundTouch implementations,
/// in an external owned fixture directory. No product JS renderer is imported.
#[test]
#[ignore = "requires the owned pinned PCM reference fixture"]
fn native_dsp_matches_pinned_browser_and_soundtouch_pcm_without_gain_or_lag_fitting() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("OPENCUT_AUDIO_PCM_REFERENCE").expect("explicit PCM reference directory"),
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["soundtouch_sha256"],
        "5143b5e14251a1e2068c83a7851ffe4bee27a0628cb0e587e2d941e0f06bf9c0"
    );
    let mut results = Vec::new();
    for row in manifest["cases"].as_array().unwrap() {
        for file in row["files"].as_array().unwrap() {
            let name = file["name"].as_str().unwrap();
            assert!(!name.contains('/') && !name.contains('\\'));
            let bytes = std::fs::read(directory.join(name)).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(bytes)),
                file["sha256"].as_str().unwrap()
            );
        }
        let name = row["name"].as_str().unwrap();
        let kind = row["kind"].as_str().unwrap();
        let input = floats(&directory.join(format!("{name}-input.f32le")));
        let expected = floats(&directory.join(format!("{name}-expected.f32le")));
        let memory = PcmMemory::new(256 * 1024 * 1024).unwrap();
        let mut actual = vec![0.0; expected.len()];
        let mut offline_errors = None;
        match kind {
            "stretch" => {
                let mut processor =
                    SoundTouch::new(row["tempo"].as_f64().unwrap(), &memory).unwrap();
                let mut block = vec![0.0; 8192];
                for at in (0..actual.len()).step_by(8192) {
                    let frames = processor
                        .extract(&input, &mut block, &mut || Ok(()))
                        .unwrap();
                    let count = (frames * 2).min(actual.len() - at);
                    actual[at..at + count].copy_from_slice(&block[..count]);
                }
                drop(processor);
                let rendered = render_preserved_pitch(
                    &input,
                    row["tempo"].as_f64().unwrap(),
                    row["output_frames"].as_u64().unwrap() as usize,
                    &memory,
                    &mut || Ok(()),
                )
                .unwrap();
                let expected = floats(&directory.join(format!("{name}-offline.f32le")));
                offline_errors = Some(errors(rendered.as_slice(), &expected));
            }
            "compressor" => {
                let rate = row["rate"].as_u64().unwrap() as u32;
                let mut reference_native = None;
                for partition in [1usize, 7, 31, 128, 4096] {
                    let mut compressor = ClassicCompressor::new(rate, &memory).unwrap();
                    actual.copy_from_slice(&input);
                    for block in actual.chunks_mut(partition * 2) {
                        compressor.process(block, &mut || Ok(())).unwrap();
                    }
                    if let Some(prior) = &reference_native {
                        assert_eq!(&actual, prior, "DSP state depends on caller block size");
                    } else {
                        reference_native = Some(actual.clone());
                    }
                }
            }
            "decode" => {
                DecodeResampler::new(
                    row["source_rate"].as_u64().unwrap() as u32,
                    row["target_rate"].as_u64().unwrap() as u32,
                    &memory,
                )
                .unwrap()
                .process(&input, &mut actual, &mut || Ok(()))
                .unwrap();
            }
            "preparation" => {
                resample_preparation(
                    &input,
                    &mut actual,
                    row["source_rate"].as_u64().unwrap() as u32,
                    row["target_rate"].as_u64().unwrap() as u32,
                    &mut || Ok(()),
                )
                .unwrap();
            }
            _ => panic!("unknown independent reference operation"),
        }
        let (maximum, snr) = errors(&actual, &expected);
        let limit = if kind == "stretch" { 0.0 } else { 0.000001 };
        let passed = maximum <= limit
            && snr >= 100.0
            && offline_errors.is_none_or(|(maximum, snr)| maximum == 0.0 && snr >= 100.0);
        results.push(json!({"case":name,"kind":kind,"maximum_absolute_error":maximum,"snr_db":snr,"offline_error":offline_errors,"maximum_gate":limit,"minimum_snr_db":100,"passed":passed,"peak_accounted_bytes":memory.peak_bytes()}));
        assert_eq!(
            memory.live_bytes(),
            0,
            "DSP arrays were retained after processing"
        );
    }
    let passed = results.iter().all(|r| r["passed"] == true);
    let report = json!({"browser_sha256":manifest["browser_sha256"],"soundtouch_sha256":manifest["soundtouch_sha256"],"fitting":"none; exact sample origin and amplitude","cases":results,"passed":passed,"scope":"native DSP primitives; not assembled preview/export or complete native audio acceptance"});
    let binary = std::env::current_exe().unwrap();
    let hash = format!("{:x}", Sha256::digest(std::fs::read(binary).unwrap()));
    let path = directory.join(format!(
        "native-dsp-{}-{}.json",
        &hash[..12],
        std::process::id()
    ));
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(passed, "Independent PCM gates failed: {}", path.display());
}
