use super::*;
#[test]
fn all_channels_share_source_buckets_across_chunk_boundaries_and_observed_eof() {
    let memory = PcmMemory::new(1024 * 1024).unwrap();
    let mut stream = WaveformStream::new(48000, 48000, 4, 512, 8192, 4096, memory.clone()).unwrap();
    let mut input = vec![vec![0.0; 257]; 4];
    input[0][0] = 0.25;
    input[1][127] = -0.75;
    input[2][128] = 0.875;
    input[3][256] = 0.125;
    let mut peaks = Vec::new();
    for (start, end) in [(0, 127), (127, 257)] {
        let chunk: Vec<_> = input.iter().map(|p| &p[start..end]).collect();
        stream
            .push(&chunk, &mut || Ok(()), &mut |p| {
                peaks.extend_from_slice(p);
                Ok(())
            })
            .unwrap();
    }
    let observed = stream
        .finish(&mut || Ok(()), &mut |p| {
            peaks.extend_from_slice(p);
            Ok(())
        })
        .unwrap();
    assert_eq!(observed, (257, 257));
    assert_eq!(peaks, vec![0.75, 0.875, 0.125]);
    assert!(stream.finish(&mut || Ok(()), &mut |_| Ok(())).is_err());
    drop(stream);
    assert_eq!(memory.live_bytes(), 0);
}
#[test]
fn empty_source_has_one_zero_bucket_and_failed_streams_retire_without_success() {
    let memory = PcmMemory::new(1024 * 1024).unwrap();
    let mut empty = WaveformStream::new(48000, 44100, 1, 512, 8192, 4, memory.clone()).unwrap();
    let mut peaks = Vec::new();
    assert_eq!(
        empty
            .finish(&mut || Ok(()), &mut |p| {
                peaks.extend_from_slice(p);
                Ok(())
            })
            .unwrap(),
        (0, 0)
    );
    assert_eq!(peaks, vec![0.0]);
    drop(empty);
    let mut limited = WaveformStream::new(48000, 48000, 1, 512, 8192, 4, memory.clone()).unwrap();
    limited
        .push(&[vec![0.25; 257]], &mut || Ok(()), &mut |_| Ok(()))
        .unwrap();
    let err = limited
        .finish(&mut || Ok(()), &mut |_| {
            panic!("an over-budget summary must not be emitted")
        })
        .unwrap_err();
    assert_eq!(err.code, "SOURCE_ART_BUDGET_EXCEEDED");
    drop(limited);
    let mut cancelled =
        WaveformStream::new(48000, 48000, 1, 512, 8192, 4096, memory.clone()).unwrap();
    let err = cancelled
        .push(
            &[vec![0.25; 257]],
            &mut || Err(error("CANCELLED", "Source waiter cancelled")),
            &mut |_| panic!(),
        )
        .unwrap_err();
    assert_eq!(err.code, "CANCELLED");
    assert!(cancelled.finish(&mut || Ok(()), &mut |_| panic!()).is_err());
    drop(cancelled);
    assert_eq!(memory.live_bytes(), 0);
}

#[test]
#[ignore = "requires retained independent browser buffer-source PCM; no browser is started"]
fn streamed_source_peaks_match_retained_independent_resampling_pcm() {
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;
    let directory = PathBuf::from(
        std::env::var_os("OPENCUT_AUDIO_PCM_REFERENCE").expect("retained reference directory"),
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["browser_sha256"],
        "b5626938028f718a6cdb0c8eff9d0346b186d68b4e04b43df4f9f579c81ecf28"
    );
    let floats = |path: PathBuf| -> Vec<f32> {
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.len().is_multiple_of(4));
        bytes
            .chunks_exact(4)
            .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
            .collect()
    };
    let mut results = Vec::new();
    for row in manifest["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "preparation")
    {
        let name = row["name"].as_str().unwrap();
        for file in row["files"].as_array().unwrap() {
            let name = file["name"].as_str().unwrap();
            assert!(!name.contains('/') && !name.contains('\\'));
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(directory.join(name)).unwrap())
                ),
                file["sha256"].as_str().unwrap()
            );
        }
        let input = floats(directory.join(format!("{name}-input.f32le")));
        let reference = floats(directory.join(format!("{name}-expected.f32le")));
        let expected: Vec<_> = reference
            .chunks(128)
            .map(|p| p.iter().map(|v| v.abs()).fold(0.0f32, f32::max))
            .collect();
        let memory = PcmMemory::new(4 * 1024 * 1024).unwrap();
        let mut stream = WaveformStream::new(
            row["source_rate"].as_u64().unwrap() as u32,
            row["target_rate"].as_u64().unwrap() as u32,
            1,
            1024,
            16384,
            65536,
            memory.clone(),
        )
        .unwrap();
        let mut actual = Vec::new();
        for chunk in input.chunks(997) {
            stream
                .push(&[chunk], &mut || Ok(()), &mut |p| {
                    actual.extend_from_slice(p);
                    Ok(())
                })
                .unwrap();
        }
        let observed = stream
            .finish(&mut || Ok(()), &mut |p| {
                actual.extend_from_slice(p);
                Ok(())
            })
            .unwrap();
        assert_eq!(observed, (input.len() as u64, reference.len() as u64));
        assert_eq!(actual.len(), expected.len());
        let mut maximum = 0.0f64;
        let mut noise = 0.0f64;
        let mut signal = 0.0f64;
        for (a, b) in actual.iter().zip(&expected) {
            let delta = f64::from(*a) - f64::from(*b);
            maximum = maximum.max(delta.abs());
            noise += delta * delta;
            signal += f64::from(*b) * f64::from(*b);
        }
        let snr = if noise == 0.0 {
            300.0
        } else {
            10.0 * (signal.max(f64::MIN_POSITIVE) / noise).log10()
        };
        results.push(json!({"case":name,"maximum_absolute_error":maximum,"snr_db":snr,"passed":maximum<=0.000001&&snr>=100.0}));
        drop(stream);
        assert_eq!(memory.live_bytes(), 0);
    }
    assert_eq!(results.len(), 6);
    let passed = results.iter().all(|r| r["passed"] == true);
    let hash = format!(
        "{:x}",
        Sha256::digest(std::fs::read(std::env::current_exe().unwrap()).unwrap())
    );
    let report = json!({"test_binary_sha256":hash,"cases":results,"scope":"streamed source buckets against retained independent whole-buffer browser preparation PCM; six native/target rate pairs, EOF extent and original 1e-6/100dB gates; no fitted gain/time and no codec/host/thumbnail acceptance","passed":passed});
    let path = directory.join(format!(
        "native-source-stream-{}-{}.json",
        &hash[..12],
        std::process::id()
    ));
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(
        passed,
        "Independent native source-stream PCM differs: {}",
        path.display()
    );
}
