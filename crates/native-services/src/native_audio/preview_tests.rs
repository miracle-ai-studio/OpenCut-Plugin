//! The native realtime candidate uses the existing owned fixtures and independent
//! browser PCM. These tests are built with the integrated preview feature slice.
use super::*;

#[test]
fn native_wave_midstream_seek_keeps_exact_packet_origin_at_fractional_device_rate() {
    let input: Vec<f32> = (0..65537)
        .map(|i| ((i % 1024) as f32 - 512.0) / 4096.0)
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let (root, snapshot) = fixture(dir.path(), &input, 1, 44100, AssetKind::Audio, true);
    with_preview(
        &snapshot,
        &root,
        48000,
        |engine, session, resources, config| {
            let start = 33437;
            let block = engine
                .raw_block(session, resources, start, 4096, config)
                .unwrap();
            for n in 0..4096usize {
                let position = (start + n as u64) * 44100;
                let lower = (position / 48000) as usize;
                let fraction = (position % 48000) as f64 / 48000.0;
                // The independent retained Wave reader uses fixed 2048-frame
                // segments. Interpolation clamps only at those real segment ends.
                let upper = (lower + 1).min((lower / 2048 + 1) * 2048 - 1);
                let expected = (f64::from(input[lower]) * (1.0 - fraction)
                    + f64::from(input[upper]) * fraction) as f32;
                assert_eq!(
                    block.pcm.as_slice()[n * 2].to_bits(),
                    expected.to_bits(),
                    "output sample {n}"
                );
                assert_eq!(
                    block.pcm.as_slice()[n * 2 + 1].to_bits(),
                    expected.to_bits()
                );
            }
        },
    );
}
fn worker_config() -> NativeAudioPreviewProcessConfig {
    NativeAudioPreviewProcessConfig {
        worker: PathBuf::from(
            std::env::var_os("OPENCUT_RENDER_WORKER").expect("explicit native worker binary"),
        ),
        preparation_deadline: Duration::from_secs(120),
        operation_deadline: Duration::from_secs(30),
        max_pcm_bytes: 64 * 1024 * 1024,
        max_encoded_bytes: 32 * 1024 * 1024,
        max_rss_bytes: 1024 * 1024 * 1024,
        strict_cgroup: None,
    }
}

#[test]
#[ignore = "requires native worker and retained independent SoundTouch/prepared preview PCM"]
fn native_prepared_pitch_preview_matches_independent_soundtouch_and_mastered_pcm() {
    let directory = PathBuf::from(
        std::env::var_os("OPENCUT_AUDIO_PCM_REFERENCE").expect("reference directory"),
    );
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(directory.join("prepared-preview-manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["browser_sha256"],
        "b5626938028f718a6cdb0c8eff9d0346b186d68b4e04b43df4f9f579c81ecf28"
    );
    assert_eq!(
        manifest["soundtouch_sha256"],
        "5143b5e14251a1e2068c83a7851ffe4bee27a0628cb0e587e2d941e0f06bf9c0"
    );
    let mut results = Vec::new();
    for row in manifest["cases"].as_array().unwrap() {
        eprintln!("Independent prepared pitch case: {}", row["name"]);
        for key in ["input", "prepared", "mastered"] {
            let name = row[format!("{key}_file")].as_str().unwrap();
            assert!(!name.contains('/') && !name.contains('\\'));
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(directory.join(name)).unwrap())
                ),
                row[format!("{key}_sha256")].as_str().unwrap()
            );
        }
        let input = floats(&directory.join(row["input_file"].as_str().unwrap()));
        let expected_raw = floats(&directory.join(row["prepared_file"].as_str().unwrap()));
        let expected_mastered = floats(&directory.join(row["mastered_file"].as_str().unwrap()));
        let rate = row["rate"].as_u64().unwrap() as u32;
        let frames = row["frames"].as_u64().unwrap();
        assert_eq!(
            input.len() as u64,
            row["source_frames"].as_u64().unwrap() * 2
        );
        assert_eq!(expected_raw.len() as u64, frames * 2);
        let dir = tempfile::tempdir().unwrap();
        let (root, mut snapshot) = fixture(dir.path(), &input, 2, rate, AssetKind::Audio, false);
        voice(&mut snapshot).timeline_range.duration = time(frames, rate);
        let ElementContent::Audio { retime, .. } = &mut voice(&mut snapshot).content else {
            panic!()
        };
        retime.rate = RationalRate {
            numerator: row["numerator"].as_u64().unwrap() as u32,
            denominator: row["denominator"].as_u64().unwrap() as u32,
        };
        retime.maintain_pitch = true;
        with_preview(
            &snapshot,
            &root,
            rate,
            |engine, session, resources, config| {
                assert_eq!(engine.extent(), frames);
                let mut actual = Vec::new();
                let mut start = 0;
                while start < frames {
                    let block = engine
                        .raw_block(
                            session,
                            resources,
                            start,
                            (frames - start).min(4096) as u32,
                            config,
                        )
                        .unwrap();
                    start += block.pcm.len() as u64 / 2;
                    actual.extend_from_slice(block.pcm.as_slice());
                }
                let (maximum, snr) = error(&actual, &expected_raw);
                results.push(json!({"case":row["name"],"stage":"native prepared pitch/mix","maximum_absolute_error":maximum,"snr_db":snr,"passed":maximum<=0.000001&&snr>=100.0}));
            },
        );
        let prepared = prepare_preview_audio_scene(
            &snapshot,
            "s",
            PcmFormat {
                sample_rate: rate,
                channels: 2,
            },
        )
        .unwrap();
        let mut session =
            ClassicAudioSession::new_preview(&prepared, 64 * 1024 * 1024, 32 * 1024 * 1024)
                .unwrap();
        session.set_preview_state(0, None).unwrap();
        let mut resources =
            VerifiedClassicAudioResources::new(&root, prepared.audio(), || Ok(())).unwrap();
        let handles = resources.handles();
        let mut producer = NativeAudioPreviewProducer::start(
            &worker_config(),
            &mut session,
            &mut resources,
            &handles,
        )
        .unwrap();
        let pid = producer.worker_pid().unwrap();
        let mut actual = Vec::new();
        let mut start = 0;
        while start < frames {
            let count = (frames - start).min(4096) as u32;
            let block = producer
                .next(&mut session, &mut resources, 1, 0, start, count)
                .unwrap();
            for (a, b) in block.channels[0].iter().zip(&block.channels[1]) {
                actual.extend([*a, *b]);
            }
            start += u64::from(count);
        }
        let (maximum, snr) = error(&actual, &expected_mastered);
        results.push(json!({"case":row["name"],"stage":"isolated native prepared preview/master","maximum_absolute_error":maximum,"snr_db":snr,"passed":maximum<=0.000001&&snr>=100.0}));
        producer.stop(&mut session, &mut resources).unwrap();
        #[cfg(unix)]
        reaped(pid);
    }
    let passed = results.iter().all(|r| r["passed"] == true);
    let hash = format!(
        "{:x}",
        Sha256::digest(std::fs::read(std::env::current_exe().unwrap()).unwrap())
    );
    let report = json!({"test_binary_sha256":hash,"cases":results,"maximum_absolute_error_gate":0.000001,"minimum_snr_db":100.0,"scope":"six retained stereo SoundTouch cases, native prepared preview and isolated continuous master; independent expected PCM, no fitted gain or time","passed":passed});
    let path = directory.join(format!(
        "native-prepared-preview-{}-{}.json",
        &hash[..12],
        std::process::id()
    ));
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(
        passed,
        "Independent prepared preview PCM differs: {}",
        path.display()
    );
}

#[test]
#[ignore = "requires native worker and retained independent ordinary PTS/rate/retime captures"]
fn native_ordinary_wave_packet_boundaries_rates_and_retime_match_independent_browser_pcm() {
    let directory = PathBuf::from(
        std::env::var_os("OPENCUT_AUDIO_PCM_REFERENCE").expect("reference directory"),
    );
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(directory.join("ordinary-preview-manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["browser_sha256"],
        "b5626938028f718a6cdb0c8eff9d0346b186d68b4e04b43df4f9f579c81ecf28"
    );
    assert_eq!(
        manifest["mediabunny_sha256"],
        "0e978df952d606fdeafef1e90ad695886ffa1a4e64f4a39d77654ada006b4b99"
    );
    let mut results = Vec::new();
    for row in manifest["cases"].as_array().unwrap() {
        eprintln!("Independent ordinary PTS case: {}", row["name"]);
        for key in ["input", "raw", "mastered"] {
            let name = row[format!("{key}_file")].as_str().unwrap();
            assert!(!name.contains('/') && !name.contains('\\'));
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(directory.join(name)).unwrap())
                ),
                row[format!("{key}_sha256")].as_str().unwrap()
            );
        }
        let input = floats(&directory.join(row["input_file"].as_str().unwrap()));
        let expected_raw = floats(&directory.join(row["raw_file"].as_str().unwrap()));
        let expected_mastered = floats(&directory.join(row["mastered_file"].as_str().unwrap()));
        let rate = row["source_rate"].as_u64().unwrap() as u32;
        let target = row["target_rate"].as_u64().unwrap() as u32;
        let numerator = row["numerator"].as_u64().unwrap() as u32;
        let denominator = row["denominator"].as_u64().unwrap() as u32;
        let dir = tempfile::tempdir().unwrap();
        let (root, mut snapshot) = fixture(dir.path(), &input, 1, rate, AssetKind::Audio, true);
        voice(&mut snapshot).timeline_range.duration = time(
            input.len() as u64 * u64::from(denominator),
            rate * numerator,
        );
        let ElementContent::Audio { retime, .. } = &mut voice(&mut snapshot).content else {
            panic!()
        };
        retime.rate = RationalRate {
            numerator,
            denominator,
        };
        with_preview(
            &snapshot,
            &root,
            target,
            |engine, session, resources, config| {
                assert_eq!(engine.extent(), row["frames"].as_u64().unwrap());
                let mut actual = Vec::new();
                let mut start = 0;
                while start < engine.extent() {
                    let count = (engine.extent() - start).min(4096) as u32;
                    let block = engine
                        .raw_block(session, resources, start, count, config)
                        .unwrap();
                    let frames = block.pcm.len() / 2;
                    actual.extend_from_slice(block.pcm.as_slice());
                    start += frames as u64;
                }
                let (maximum, snr) = error(&actual, &expected_raw);
                results.push(json!({"case":row["name"],"stage":"native ordinary PTS/mix","maximum_absolute_error":maximum,"snr_db":snr,"gate":0.000001,"passed":maximum<=0.000001&&snr>=100.0}));
            },
        );
        let prepared = prepare_preview_audio_scene(
            &snapshot,
            "s",
            PcmFormat {
                sample_rate: target,
                channels: 2,
            },
        )
        .unwrap();
        let mut session =
            ClassicAudioSession::new_preview(&prepared, 64 * 1024 * 1024, 32 * 1024 * 1024)
                .unwrap();
        session.set_preview_state(0, None).unwrap();
        let mut resources =
            VerifiedClassicAudioResources::new(&root, prepared.audio(), || Ok(())).unwrap();
        let handles = resources.handles();
        let mut producer = NativeAudioPreviewProducer::start(
            &worker_config(),
            &mut session,
            &mut resources,
            &handles,
        )
        .unwrap();
        let pid = producer.worker_pid().unwrap();
        let mut actual = Vec::new();
        let mut start = 0;
        while start < prepared.extent() {
            let count = (prepared.extent() - start).min(4096) as u32;
            let block = producer
                .next(&mut session, &mut resources, 1, 0, start, count)
                .unwrap();
            for (a, b) in block.channels[0].iter().zip(&block.channels[1]) {
                actual.extend([*a, *b]);
            }
            start += u64::from(count);
        }
        let (maximum, snr) = error(&actual, &expected_mastered);
        results.push(json!({"case":row["name"],"stage":"isolated native ordinary preview/master","maximum_absolute_error":maximum,"snr_db":snr,"gate":0.000001,"passed":maximum<=0.000001&&snr>=100.0}));
        producer.stop(&mut session, &mut resources).unwrap();
        #[cfg(unix)]
        reaped(pid);
    }
    let passed = results.iter().all(|r| r["passed"] == true);
    let hash = format!(
        "{:x}",
        Sha256::digest(std::fs::read(std::env::current_exe().unwrap()).unwrap())
    );
    let report = json!({"test_binary_sha256":hash,"cases":results,"scope":"retained mono float32 Wave across native rates 22050/44100/48000/96000, device rates 44100/48000 and ordinary retime 3/4,5/4; independent upstream PCM segments/manual timeline sampling/browser master; no fitted gain or time","passed":passed});
    let path = directory.join(format!(
        "native-ordinary-preview-{}-{}.json",
        &hash[..12],
        std::process::id()
    ));
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(
        passed,
        "Independent ordinary native preview PCM differs: {}",
        path.display()
    );
}
#[cfg(unix)]
fn reaped(pid: u32) {
    assert_eq!(
        unsafe { libc::kill(pid as i32, 0) },
        -1,
        "Owned native preview child must exit and be reaped"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
#[ignore = "requires explicit native worker and retained independent browser mastering PCM"]
fn isolated_native_realtime_worker_matches_independent_pcm_and_reaps_after_reset_and_stop() {
    let directory = PathBuf::from(
        std::env::var_os("OPENCUT_AUDIO_PCM_REFERENCE").expect("reference directory"),
    );
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(directory.join("preview-master-manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["browser_sha256"],
        "b5626938028f718a6cdb0c8eff9d0346b186d68b4e04b43df4f9f579c81ecf28"
    );
    let mut results = Vec::new();
    for row in manifest["cases"].as_array().unwrap() {
        let input_name = row["input_file"].as_str().unwrap();
        let expected_name = row["expected_file"].as_str().unwrap();
        for (name, key) in [
            (input_name, "input_sha256"),
            (expected_name, "expected_sha256"),
        ] {
            assert!(!name.contains('/') && !name.contains('\\'));
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(directory.join(name)).unwrap())
                ),
                row[key].as_str().unwrap()
            );
        }
        let input = floats(&directory.join(input_name));
        let expected = floats(&directory.join(expected_name));
        let rate = row["rate"].as_u64().unwrap() as u32;
        let dir = tempfile::tempdir().unwrap();
        let (root, snapshot) = fixture(dir.path(), &input, 2, rate, AssetKind::Audio, true);
        let prepared = prepare_preview_audio_scene(
            &snapshot,
            "s",
            PcmFormat {
                sample_rate: rate,
                channels: 2,
            },
        )
        .unwrap();
        let mut session =
            ClassicAudioSession::new_preview(&prepared, 64 * 1024 * 1024, 32 * 1024 * 1024)
                .unwrap();
        session.set_preview_state(0, None).unwrap();
        let mut resources =
            VerifiedClassicAudioResources::new(&root, prepared.audio(), || Ok(())).unwrap();
        let handles = resources.handles();
        let mut producer = NativeAudioPreviewProducer::start(
            &worker_config(),
            &mut session,
            &mut resources,
            &handles,
        )
        .unwrap();
        let pid = producer.worker_pid().unwrap();
        assert_eq!(
            producer.runtime_identity().decoder_profile,
            NATIVE_AUDIO_DECODER
        );
        session
            .validate_preview_receipt(producer.ready_receipt())
            .unwrap();
        let mut actual = Vec::new();
        let mut start = 0;
        let mut index = 0;
        while start < prepared.extent() {
            let count = (prepared.extent() - start).min([4096, 17, 2048, 127][index % 4]) as u32;
            let block = producer
                .next(&mut session, &mut resources, 1, 0, start, count)
                .unwrap();
            assert_eq!(
                (
                    block.start_sample,
                    block.frame_count,
                    block.generation,
                    block.overlay_epoch
                ),
                (start, count, 1, 0)
            );
            for (a, b) in block.channels[0].iter().zip(&block.channels[1]) {
                actual.extend([*a, *b]);
            }
            start += u64::from(count);
            index += 1;
        }
        let (maximum, snr) = error(&actual, &expected);
        results.push(json!({"case":row["name"],"maximum_absolute_error":maximum,"snr_db":snr,"gate":0.000001,"passed":maximum<=0.000001&&snr>=100.0}));
        session.set_preview_state(0, None).unwrap();
        let reset = producer
            .next(&mut session, &mut resources, 2, 1, 0, 4096)
            .unwrap();
        let reset: Vec<f32> = reset.channels[0]
            .iter()
            .zip(&reset.channels[1])
            .flat_map(|(a, b)| [*a, *b])
            .collect();
        let (maximum, snr) = error(&reset, &expected[..8192]);
        results.push(json!({"case":format!("{}-reset",row["name"].as_str().unwrap()),"maximum_absolute_error":maximum,"snr_db":snr,"gate":0.000001,"passed":maximum<=0.000001&&snr>=100.0}));
        producer.stop(&mut session, &mut resources).unwrap();
        #[cfg(unix)]
        reaped(pid);
    }
    let passed = results.iter().all(|r| r["passed"] == true);
    let hash = format!(
        "{:x}",
        Sha256::digest(std::fs::read(std::env::current_exe().unwrap()).unwrap())
    );
    let report = json!({"test_binary_sha256":hash,"cases":results,"scope":"actual persistent isolated native worker, shared application/CAS, continuous native master with independent browser PCM, generation reset and parent-owned reaping; no HTTP/device/long-play acceptance","passed":passed});
    let path = directory.join(format!(
        "native-preview-worker-{}-{}.json",
        &hash[..12],
        std::process::id()
    ));
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(
        passed,
        "Independent native preview worker PCM differs: {}",
        path.display()
    );
}

#[test]
#[ignore = "requires explicit native worker binary"]
fn isolated_native_preview_admits_prefetch_prefixes_and_reaps_on_live_cancellation() {
    use std::{cell::Cell, rc::Rc};
    let input: Vec<f32> = (0..16384)
        .map(|i| if i % 23 == 0 { 0.125 } else { 0.0 })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let (root, mut snapshot) = fixture(dir.path(), &input, 1, 48000, AssetKind::Audio, true);
    // Thirty-two simultaneous voices fit the work envelope only in prefixes
    // smaller than a 4096-frame device block. Read-ahead must ask Rust too.
    for n in 1..32 {
        let mut track = snapshot.project.sequences[0].tracks[1].clone();
        track.id = format!("sound-{n}");
        track.elements[0].id = format!("voice-{n}");
        snapshot.project.sequences[0].tracks.push(track);
    }
    let prepared = prepare_preview_audio_scene(
        &snapshot,
        "s",
        PcmFormat {
            sample_rate: 48000,
            channels: 2,
        },
    )
    .unwrap();
    let mut session =
        ClassicAudioSession::new_preview(&prepared, 64 * 1024 * 1024, 32 * 1024 * 1024).unwrap();
    session.set_preview_state(0, None).unwrap();
    let remaining = Rc::new(Cell::new(None::<usize>));
    let check = remaining.clone();
    let mut resources = VerifiedClassicAudioResources::new(&root, prepared.audio(), move || {
        if let Some(n) = check.get() {
            if n == 0 {
                return Err(opencut_contracts::EditorError::new(
                    "JOB_CANCELLED",
                    "Owned native preview revoked",
                ));
            }
            check.set(Some(n - 1));
        }
        Ok(())
    })
    .unwrap();
    let handles = resources.handles();
    let mut producer =
        NativeAudioPreviewProducer::start(&worker_config(), &mut session, &mut resources, &handles)
            .unwrap();
    let pid = producer.worker_pid().unwrap();
    let block = producer
        .next(&mut session, &mut resources, 1, 0, 0, 4096)
        .unwrap();
    assert_eq!(block.channels[0].len(), 4096);
    assert!(block.channels[0].iter().all(|v| v.is_finite()));
    let inputs = block.receipt["inputs"].as_array().unwrap();
    assert!(inputs.len() > 1);
    assert!(
        inputs
            .iter()
            .all(|v| v["frameCount"].as_u64().unwrap() <= 2048)
    );
    remaining.set(Some(15));
    let failure = producer
        .next(&mut session, &mut resources, 1, 0, 4096, 4096)
        .err()
        .expect("Live cancellation must fail before a PCM block is published");
    assert_eq!(failure.code, "JOB_CANCELLED");
    #[cfg(unix)]
    reaped(pid);
    assert!(producer.worker_pid().is_none());
}

fn with_preview(
    snapshot: &EditorSnapshot,
    root: &Path,
    rate: u32,
    run: impl FnOnce(
        &mut preview::PreviewEngine,
        &mut ClassicAudioSession<'_>,
        &mut dyn ClassicAudioResources,
        &source::DecodeConfig,
    ),
) {
    let prepared = prepare_preview_audio_scene(
        snapshot,
        "s",
        PcmFormat {
            sample_rate: rate,
            channels: 2,
        },
    )
    .unwrap();
    let mut session =
        ClassicAudioSession::new_preview(&prepared, 64 * 1024 * 1024, 32 * 1024 * 1024).unwrap();
    session.set_preview_state(0, None).unwrap();
    let mut resources =
        VerifiedClassicAudioResources::new(root, prepared.audio(), || Ok(())).unwrap();
    let handles = resources.handles();
    let config = source::DecodeConfig {
        sample_rate: rate,
        max_pcm_bytes: 64 * 1024 * 1024,
        max_encoded_bytes: 32 * 1024 * 1024,
        deadline: Instant::now() + Duration::from_secs(60),
    };
    let mut engine = preview::PreviewEngine::prepare(
        &mut session,
        &mut resources,
        &handles,
        &config,
        &"a".repeat(64),
    )
    .unwrap();
    session.validate_preview_receipt(&engine.receipt).unwrap();
    let memory = engine.memory().clone();
    run(&mut engine, &mut session, &mut resources, &config);
    engine.close(&mut resources).unwrap();
    assert_eq!(
        memory.live_bytes(),
        0,
        "All rolling, prepared, codec and metadata owners must release"
    );
}

#[test]
fn native_realtime_ordinary_pts_forward_seek_rewind_and_observed_eof_preserve_authored_pcm() {
    for (rate, channels) in [(48000, 2), (44100, 1)] {
        let input: Vec<f32> = (0..8193)
            .flat_map(|i| {
                (0..channels).map(move |c| {
                    if (i + c * 17) % 41 == 0 {
                        0.125
                    } else {
                        -0.0625
                    }
                })
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let (root, mut snapshot) =
            fixture(dir.path(), &input, channels, rate, AssetKind::Audio, false);
        voice(&mut snapshot).timeline_range.duration = time(24576, rate);
        with_preview(
            &snapshot,
            &root,
            rate,
            |engine, session, resources, config| {
                assert!(engine.receipt.decoders[0].frame_count.is_none());
                assert_eq!(engine.receipt.source_formats[0].frame_count, 0);
                for (start, count) in [
                    (0, 4096),
                    (4096, 4096),
                    (8192, 4096),
                    (20480, 4096),
                    (0, 127),
                    (4096, 17),
                ] {
                    let block = engine
                        .raw_block(session, resources, start, count, config)
                        .unwrap();
                    assert_eq!(block.pcm.len(), count as usize * 2);
                    for i in 0..count as usize {
                        for c in 0..2 {
                            let sample = input
                                .get(
                                    (start as usize + i) * channels as usize
                                        + c.min(channels as usize - 1),
                                )
                                .copied()
                                .unwrap_or(0.0);
                            assert_eq!(
                                block.pcm.as_slice()[i * 2 + c].to_bits(),
                                sample.to_bits(),
                                "start={start}, frame={i}, channel={c}"
                            );
                        }
                    }
                }
            },
        );
    }
}

#[test]
fn native_realtime_shared_ordinary_and_prepared_consumers_keep_independent_buffers_and_stored_mix_order()
 {
    let input: Vec<f32> = (0..20000)
        .map(|i| if i % 43 == 0 { 0.125 } else { -0.03125 })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let (root, mut snapshot) = fixture(dir.path(), &input, 1, 48000, AssetKind::Audio, true);
    let mut prepared = voice(&mut snapshot).clone();
    prepared.id = "prepared".into();
    prepared.channels.push(AnimationChannel::Scalar {
        id: "volume".into(),
        target: ChannelTarget::Element {
            element_id: prepared.id.clone(),
        },
        binding: ScalarBinding::Property {
            parameter: ScalarParameter::GainDecibels,
        },
        keys: vec![ScalarKey {
            id: "constant-volume".into(),
            segment_id: None,
            time: time(0, 1),
            value: 0.0,
            left_handle: None,
            right_handle: None,
            segment_to_next: ScalarSegment::Linear,
            tangent_mode: TangentMode::Flat,
        }],
        extrapolation: Extrapolation {
            before: ExtrapolationMode::Hold,
            after: ExtrapolationMode::Hold,
        },
    });
    let mut track = snapshot.project.sequences[0].tracks[1].clone();
    track.id = "prepared-track".into();
    track.elements = vec![prepared];
    snapshot.project.sequences[0].tracks.push(track);
    with_preview(
        &snapshot,
        &root,
        48000,
        |engine, session, resources, config| {
            assert_eq!(engine.receipt.decoders.len(), 1);
            assert_eq!(engine.receipt.preparation.len(), 1);
            assert_eq!(
                engine.receipt.preparation[0].facts.mode,
                ClassicAudioPreparationMode::PreparedLinear
            );
            assert_eq!(
                engine
                    .receipt
                    .source_formats
                    .iter()
                    .map(|f| f.prepared)
                    .collect::<Vec<_>>(),
                [false, true]
            );
            for start in [0, 4096, 16000] {
                let block = engine
                    .raw_block(session, resources, start, 1024, config)
                    .unwrap();
                for i in 0..1024 {
                    for c in 0..2 {
                        let expected = (f64::from(input[start as usize + i])
                            + f64::from(input[start as usize + i]))
                            as f32;
                        assert_eq!(
                            block.pcm.as_slice()[i * 2 + c].to_bits(),
                            expected.to_bits()
                        );
                    }
                }
            }
        },
    );
}

#[test]
#[ignore = "requires retained independent browser preview-master captures; does not start a browser"]
fn native_realtime_compressor_continuity_reset_and_real_eof_tail_match_independent_browser() {
    let directory = PathBuf::from(
        std::env::var_os("OPENCUT_AUDIO_PCM_REFERENCE").expect("explicit reference directory"),
    );
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(directory.join("preview-master-manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["browser_sha256"],
        "b5626938028f718a6cdb0c8eff9d0346b186d68b4e04b43df4f9f579c81ecf28"
    );
    let mut results = Vec::new();
    for row in manifest["cases"].as_array().unwrap() {
        let input_name = row["input_file"].as_str().unwrap();
        let output_name = row["expected_file"].as_str().unwrap();
        for (name, key) in [
            (input_name, "input_sha256"),
            (output_name, "expected_sha256"),
        ] {
            assert!(!name.contains('/') && !name.contains('\\'));
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(directory.join(name)).unwrap())
                ),
                row[key].as_str().unwrap()
            );
        }
        let input = floats(&directory.join(input_name));
        let expected = floats(&directory.join(output_name));
        let rate = row["rate"].as_u64().unwrap() as u32;
        let dir = tempfile::tempdir().unwrap();
        let (root, snapshot) = fixture(dir.path(), &input, 2, rate, AssetKind::Audio, true);
        with_preview(
            &snapshot,
            &root,
            rate,
            |engine, session, resources, config| {
                let mut master =
                    preview::PreviewMaster::new(rate, engine.memory(), 1, 0, 0).unwrap();
                let mut actual = Vec::new();
                let mut start = 0u64;
                let mut index = 0;
                while start < engine.extent() {
                    let count = (engine.extent() - start)
                        .min([1, 127, 4096, 17, 511, 2048][index % 6])
                        as u32;
                    let block = master
                        .next(engine, session, resources, start, count, config)
                        .unwrap();
                    assert_eq!(
                        u64::from(block.lookahead_frames),
                        row["lookahead_frames"].as_u64().unwrap()
                    );
                    actual.extend_from_slice(block.pcm.as_slice());
                    start += u64::from(count);
                    index += 1;
                }
                let (maximum, snr) = error(&actual, &expected);
                results.push(json!({"case":row["name"],"maximum_absolute_error":maximum,"snr_db":snr,"gate":0.000001,"passed":maximum<=0.000001&&snr>=100.0}));
                // A new generation must have a fresh detector, envelope and delay
                // ring even while the same verified native sources remain warm.
                session.set_preview_state(0, None).unwrap();
                let mut reset =
                    preview::PreviewMaster::new(rate, engine.memory(), 2, 1, 0).unwrap();
                assert!(!master.matches(2, 1));
                assert!(reset.matches(2, 1));
                let block = reset
                    .next(engine, session, resources, 0, 4096, config)
                    .unwrap();
                let (maximum, snr) = error(block.pcm.as_slice(), &expected[..8192]);
                results.push(json!({"case":format!("{}-generation-reset",row["name"].as_str().unwrap()),"maximum_absolute_error":maximum,"snr_db":snr,"gate":0.000001,"passed":maximum<=0.000001&&snr>=100.0}));
            },
        );
    }
    let hash = format!(
        "{:x}",
        Sha256::digest(std::fs::read(std::env::current_exe().unwrap()).unwrap())
    );
    let passed = results.iter().all(|row| row["passed"] == true);
    let report = json!({"test_binary_sha256":hash,"cases":results,"scope":"real application and verified CAS through native rolling sources, shared mix and continuous compressor; fixed declared predelay, no fitted time or gain; no process/HTTP/device acceptance","passed":passed});
    let path = directory.join(format!(
        "native-preview-master-{}-{}.json",
        &hash[..12],
        std::process::id()
    ));
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(
        passed,
        "Independent realtime master PCM differs: {}",
        path.display()
    );
}
