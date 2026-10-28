//! Real application compiler -> verified CAS -> libav -> native preparation /
//! mixing / mastering. Expected PCM comes from authored samples or retained
//! independent Chromium captures, never another invocation of this engine.
use super::*;
use crate::classic_audio::VerifiedClassicAudioResources;
use opencut_asset_store::AssetStore;
use opencut_composition::scene::*;
use opencut_contracts::{Asset, AssetKind, CreateProject, FrameRate, Time, TimeRange, scene::*};
use opencut_editor_application::{classic_audio::*, scene::new_document};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
fn time(n: u64, d: u32) -> Time {
    Time {
        ticks: n.to_string(),
        timebase: d,
    }
}
fn fixture(
    dir: &Path,
    pcm: &[f32],
    channels: u32,
    rate: u32,
    kind: AssetKind,
    known_extent: bool,
) -> (PathBuf, EditorSnapshot) {
    assert_eq!(pcm.len() % channels as usize, 0);
    let frames = pcm.len() as u64 / u64::from(channels);
    let path = dir.join("native-audio-oracle.wav");
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + pcm.len() as u32 * 4).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(3u16.to_le_bytes());
    bytes.extend((channels as u16).to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * channels * 4).to_le_bytes());
    bytes.extend((channels as u16 * 4).to_le_bytes());
    bytes.extend(32u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((pcm.len() as u32 * 4).to_le_bytes());
    bytes.extend(pcm.iter().flat_map(|v| v.to_le_bytes()));
    std::fs::write(&path, bytes).unwrap();
    let root = dir.join("cas");
    let duration = known_extent.then(|| time(frames, rate));
    let pinned = AssetStore::open(&root)
        .unwrap()
        .pin_asset(&Asset {
            id: "source".into(),
            name: "Authored native PCM".into(),
            kind: kind.clone(),
            uri: path.to_string_lossy().into(),
            duration: duration.clone(),
            version: None,
            metadata: BTreeMap::new(),
        })
        .unwrap();
    let mut project = new_document(&CreateProject {
        id: "native-pcm".into(),
        name: "Native PCM acceptance".into(),
        sequence_id: "s".into(),
        width: 320,
        height: 180,
        fps: FrameRate {
            numerator: 24,
            denominator: 1,
        },
    })
    .unwrap();
    project.assets.push(ProjectAsset {
        id: "source".into(),
        name: "Authored native PCM".into(),
        kind,
        version: pinned.version.unwrap(),
        duration,
        origin: ResourceOrigin::LegacyLocator {
            uri: "unused-source-provenance.wav".into(),
        },
        metadata: ProjectMediaMetadata {
            mime_type: Some("audio/wav".into()),
            intrinsic_size: None,
            frame_rate: None,
            rotation_degrees: None,
            audio: AudioPresence::Present {
                metadata: Some(AudioMetadata {
                    sample_rate: rate,
                    channels,
                    channel_layout: None,
                }),
            },
        },
        license: None,
    });
    let mut track = project.sequences[0].tracks[0].clone();
    track.id = "sound".into();
    track.kind = SceneTrackKind::Audio;
    track.name = "Sound".into();
    track.elements.push(TimelineElement {
        id: "voice".into(),
        name: "Voice".into(),
        enabled: true,
        timeline_range: TimeRange {
            start: time(0, 1),
            duration: time(frames, rate),
        },
        source_span: ElementSourceSpan::ClassicTrimmed {
            trim_start: time(0, 1),
            trim_end: time(0, 1),
            source_duration: None,
        },
        content: ElementContent::Audio {
            asset_id: "source".into(),
            audio: AudioStyle {
                stream: None,
                gain: Gain::Decibels { value: 0.0 },
                muted: false,
            },
            retime: Retime {
                rate: RationalRate {
                    numerator: 1,
                    denominator: 1,
                },
                maintain_pitch: false,
                mapping: RetimeMapping::ExactRational,
            },
        },
        visual: None,
        channels: vec![],
    });
    project.sequences[0].tracks.push(track);
    (
        root,
        EditorSnapshot {
            project,
            revision: "fixed-native-pcm-reference".into(),
        },
    )
}
#[derive(Default)]
struct Pcm {
    channels: [Vec<f32>; 2],
}
impl ClassicAudioPcmSink for Pcm {
    fn chunk(&mut self, c: u32, offset: u64, samples: &[f32]) -> Result<(), EditorError> {
        assert_eq!(offset, self.channels[c as usize].len() as u64);
        assert!(samples.len() <= 16384 && samples.iter().all(|v| v.is_finite()));
        self.channels[c as usize].extend_from_slice(samples);
        Ok(())
    }
}
fn execute(
    snapshot: &EditorSnapshot,
    root: &Path,
    rate: u32,
    range: Option<ClassicAudioRange>,
) -> (Pcm, ClassicAudioRenderReceipt) {
    let prepared = prepare_audio_scene(
        snapshot,
        "s",
        AudioExecutionProfile::ClassicV31,
        PcmFormat {
            sample_rate: rate,
            channels: 2,
        },
    )
    .unwrap();
    let mut resources = VerifiedClassicAudioResources::new(root, &prepared, || Ok(())).unwrap();
    let handles = resources.handles();
    let mut session =
        ClassicAudioSession::new_with_range(&prepared, 64 * 1024 * 1024, 32 * 1024 * 1024, range)
            .unwrap();
    let mut pcm = Pcm::default();
    let receipt = render::render(
        &mut session,
        &mut resources,
        &handles,
        &mut pcm,
        &source::DecodeConfig {
            sample_rate: rate,
            max_pcm_bytes: 64 * 1024 * 1024,
            max_encoded_bytes: 32 * 1024 * 1024,
            deadline: Instant::now() + Duration::from_secs(30),
        },
        "native-test-execution",
    )
    .unwrap();
    session.validate_native_receipt(&receipt).unwrap();
    assert!(
        session.validate_receipt(&receipt).is_err(),
        "Native provenance must not pass as browser execution"
    );
    assert!(receipt.peak_accounted_bytes <= 64 * 1024 * 1024);
    (pcm, receipt)
}
fn voice(snapshot: &mut EditorSnapshot) -> &mut TimelineElement {
    &mut snapshot.project.sequences[0].tracks[1].elements[0]
}
#[test]
fn native_cas_decode_trim_placement_shared_voice_float_writes_and_eof_match_authored_pcm() {
    let dir = tempfile::tempdir().unwrap();
    let input: Vec<f32> = (0..16384)
        .flat_map(|i| {
            [
                if i % 37 == 0 { 0.25 } else { 0.0 },
                if i % 61 == 0 { -0.125 } else { 0.0 },
            ]
        })
        .collect();
    // Actual EOF is intentionally shorter than the authored request. Unknown
    // declared extent is required for this legal zero-tail scenario.
    let (root, mut snapshot) = fixture(dir.path(), &input, 2, 48000, AssetKind::Audio, false);
    voice(&mut snapshot).timeline_range = TimeRange {
        start: time(17, 48000),
        duration: time(20000, 48000),
    };
    voice(&mut snapshot).source_span = ElementSourceSpan::ClassicTrimmed {
        trim_start: time(1024, 48000),
        trim_end: time(0, 1),
        source_duration: None,
    };
    let mut second = voice(&mut snapshot).clone();
    second.id = "second".into();
    second.timeline_range.start = time(29, 48000);
    let ElementContent::Audio { audio, .. } = &mut second.content else {
        panic!()
    };
    audio.gain = Gain::LinearLegacy { value: 0.5 };
    let mut second_track = snapshot.project.sequences[0].tracks[1].clone();
    second_track.id = "second-sound".into();
    second_track.elements = vec![second];
    snapshot.project.sequences[0].tracks.push(second_track);
    let (pcm, receipt) = execute(&snapshot, &root, 48000, None);
    assert_eq!(receipt.decoders.len(), 1);
    assert_eq!(
        receipt.decoders[0].profile,
        "native-libav-windowed-concatenate-linear-resample"
    );
    assert!(!receipt.mastering.applied);
    for c in 0..2 {
        for (i, actual) in pcm.channels[c].iter().enumerate() {
            let mut expected = 0.0f32;
            for (offset, gain) in [(17usize, 1.0f64), (29, 0.5)] {
                if i >= offset && i - offset < 20000 {
                    let sample = input
                        .get((i - offset + 1024) * 2 + c)
                        .copied()
                        .unwrap_or(0.0);
                    expected = (f64::from(expected) + f64::from(sample) * gain) as f32;
                }
            }
            assert_eq!(*actual, expected, "channel={c} sample={i}");
        }
    }
}
#[test]
fn native_sparse_far_trim_preserves_actual_eof_and_never_allocates_full_source_pcm() {
    let dir = tempfile::tempdir().unwrap();
    let input: Vec<f32> = (0..192000)
        .map(|i| if i % 101 == 0 { 0.25 } else { -0.0625 })
        .collect();
    let (root, mut snapshot) = fixture(dir.path(), &input, 1, 48000, AssetKind::Audio, false);
    voice(&mut snapshot).timeline_range.duration = time(4800, 48000);
    voice(&mut snapshot).source_span = ElementSourceSpan::ClassicTrimmed {
        trim_start: time(144000, 48000),
        trim_end: time(0, 1),
        source_duration: None,
    };
    let (pcm, receipt) = execute(&snapshot, &root, 48000, None);
    assert_eq!(
        receipt.decoders[0].profile,
        "native-libav-windowed-concatenate-linear-resample"
    );
    assert_eq!(receipt.decoders[0].frame_count, "192000");
    assert!(receipt.preparation[0].facts.decoded_bytes < 192000 * 4 / 2);
    for c in &pcm.channels {
        assert_eq!(c, &input[144000..148800]);
    }
}
#[test]
fn native_prepared_identity_keeps_the_separate_float32_buffer_and_mono_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let input: Vec<f32> = (0..4096)
        .map(|i| if i % 17 == 0 { 0.125 } else { -0.0625 })
        .collect();
    let (root, mut snapshot) = fixture(dir.path(), &input, 1, 48000, AssetKind::Audio, true);
    let ElementContent::Audio { retime, .. } = &mut voice(&mut snapshot).content else {
        panic!()
    };
    retime.maintain_pitch = true;
    let (pcm, receipt) = execute(&snapshot, &root, 48000, None);
    assert_eq!(
        receipt.preparation[0].facts.mode,
        ClassicAudioPreparationMode::PreparedLinear
    );
    for channel in &pcm.channels {
        assert_eq!(channel, &input);
    }
}
#[test]
fn native_pcm_missing_window_and_noncanonical_fraction_are_rejected() {
    let memory = dsp::PcmMemory::new(1024).unwrap();
    let input = pcm::PcmSource::new(
        48000,
        100,
        1,
        true,
        vec![pcm::Window {
            start: 10,
            samples: pcm::Samples::Planar(vec![memory.array(20).unwrap()]),
        }],
    )
    .unwrap();
    assert!(input.value(0, 9).is_err());
    assert!(input.value(0, 30).is_err());
    assert_eq!(input.value(0, 100).unwrap(), 0.0);
    assert!(input.linear(0, 10, 1, 0).is_err());
    assert!(input.linear(0, 10, 2, 2).is_err());
    assert_eq!(input.linear(0, u64::MAX, 0, 1).unwrap(), 0.0);
}
struct RevokeAfterRead<R> {
    inner: R,
    revoked: bool,
    code: &'static str,
}
impl<R: ClassicAudioResources> ClassicAudioResources for RevokeAfterRead<R> {
    fn checkpoint(&mut self) -> Result<(), EditorError> {
        if self.revoked {
            Err(EditorError::new(self.code, "Owned native audio revocation"))
        } else {
            self.inner.checkpoint()
        }
    }
    fn resolve(&mut self, r: &AudioResource, h: &str) -> Result<(String, u64), EditorError> {
        self.checkpoint()?;
        self.inner.resolve(r, h)
    }
    fn read(&mut self, t: &str, o: u64, n: u32) -> Result<(Vec<u8>, bool), EditorError> {
        self.checkpoint()?;
        let result = self.inner.read(t, o, n)?;
        self.revoked = true;
        Ok(result)
    }
    fn release(&mut self, t: &str) -> Result<(), EditorError> {
        self.inner.release(t)
    }
}
struct ChangedBlock<'a>(ClassicAudioSession<'a>);
impl<R: ClassicAudioResources + ?Sized> AudioSession<R> for ChangedBlock<'_> {
    fn plan(&self) -> Result<Value, EditorError> {
        self.0.plan()
    }
    fn callback(&mut self, request: Value, resources: &mut R) -> Result<Value, EditorError> {
        let block = request["op"] == "compileBlock";
        let mut result = self
            .0
            .callback(serde_json::from_value(request).unwrap(), resources)?;
        if block {
            result["identity"]["revision"] = json!("foreign-revision");
        }
        Ok(result)
    }
}
#[test]
fn native_revocation_foreign_block_and_small_budget_never_emit_a_successful_pcm_result() {
    let dir = tempfile::tempdir().unwrap();
    let (root, snapshot) = fixture(
        dir.path(),
        &vec![0.25; 8192],
        1,
        48000,
        AssetKind::Audio,
        true,
    );
    let prepared = prepare_audio_scene(
        &snapshot,
        "s",
        AudioExecutionProfile::ClassicV31,
        PcmFormat {
            sample_rate: 48000,
            channels: 2,
        },
    )
    .unwrap();
    let config = source::DecodeConfig {
        sample_rate: 48000,
        max_pcm_bytes: 64 * 1024 * 1024,
        max_encoded_bytes: 32 * 1024 * 1024,
        deadline: Instant::now() + Duration::from_secs(30),
    };
    for code in ["NOT_AUTHORIZED", "PERMISSION_DENIED", "JOB_CANCELLED"] {
        let inner = VerifiedClassicAudioResources::new(&root, &prepared, || Ok(())).unwrap();
        let handles = inner.handles();
        let mut resources = RevokeAfterRead {
            inner,
            revoked: false,
            code,
        };
        let mut session =
            ClassicAudioSession::new(&prepared, config.max_pcm_bytes, config.max_encoded_bytes)
                .unwrap();
        let mut pcm = Pcm::default();
        let error = render::render(
            &mut session,
            &mut resources,
            &handles,
            &mut pcm,
            &config,
            "native-test-execution",
        )
        .unwrap_err();
        assert_eq!(error.code, code);
        assert!(pcm.channels.iter().all(Vec::is_empty));
    }
    {
        let mut resources =
            VerifiedClassicAudioResources::new(&root, &prepared, || Ok(())).unwrap();
        let handles = resources.handles();
        let mut session = ChangedBlock(
            ClassicAudioSession::new(&prepared, config.max_pcm_bytes, config.max_encoded_bytes)
                .unwrap(),
        );
        let mut pcm = Pcm::default();
        let error = render::render(
            &mut session,
            &mut resources,
            &handles,
            &mut pcm,
            &config,
            "native-test-execution",
        )
        .unwrap_err();
        assert_eq!(error.code, "INVALID_AUDIO_PLAN");
        assert!(pcm.channels.iter().all(Vec::is_empty));
    }
    {
        let mut resources =
            VerifiedClassicAudioResources::new(&root, &prepared, || Ok(())).unwrap();
        let handles = resources.handles();
        let mut session =
            ClassicAudioSession::new(&prepared, config.max_pcm_bytes, config.max_encoded_bytes)
                .unwrap();
        let mut pcm = Pcm::default();
        let config = source::DecodeConfig {
            max_pcm_bytes: 4096,
            ..config
        };
        let error = render::render(
            &mut session,
            &mut resources,
            &handles,
            &mut pcm,
            &config,
            "native-test-execution",
        )
        .unwrap_err();
        assert_eq!(error.code, "AUDIO_BUDGET_EXCEEDED");
        assert!(pcm.channels.iter().all(Vec::is_empty));
    }
}
fn floats(path: &Path) -> Vec<f32> {
    std::fs::read(path)
        .unwrap()
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}
fn error(actual: &[f32], expected: &[f32]) -> (f64, f64) {
    assert_eq!(actual.len(), expected.len());
    let (mut peak, mut noise, mut signal) = (0.0f64, 0.0, 0.0);
    for (a, b) in actual.iter().zip(expected) {
        let d = f64::from(*a) - f64::from(*b);
        peak = peak.max(d.abs());
        noise += d * d;
        signal += f64::from(*b).powi(2);
    }
    (
        peak,
        if noise == 0.0 {
            300.0
        } else {
            10.0 * (signal.max(f64::MIN_POSITIVE) / noise).log10()
        },
    )
}
#[test]
#[ignore = "requires retained independent browser PCM; does not start a browser"]
fn native_application_export_matches_independent_decode_pitch_mastering_and_post_master_crop() {
    let directory = PathBuf::from(
        std::env::var_os("OPENCUT_AUDIO_PCM_REFERENCE").expect("explicit reference directory"),
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["browser_sha256"],
        "b5626938028f718a6cdb0c8eff9d0346b186d68b4e04b43df4f9f579c81ecf28"
    );
    let mut results = Vec::new();
    for row in manifest["cases"].as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        let kind = row["kind"].as_str().unwrap();
        eprintln!("Native engine independent case: {name}");
        for f in row["files"].as_array().unwrap() {
            let name = f["name"].as_str().unwrap();
            assert!(!name.contains('/') && !name.contains('\\'));
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(directory.join(name)).unwrap())
                ),
                f["sha256"].as_str().unwrap()
            );
        }
        let input = floats(&directory.join(format!("{name}-input.f32le")));
        let expected = floats(&directory.join(format!(
            "{name}-{}.f32le",
            if kind == "stretch" {
                "offline"
            } else {
                "expected"
            }
        )));
        let channels = if matches!(kind, "stretch" | "compressor") {
            2
        } else {
            1
        };
        let rate = row["rate"]
            .as_u64()
            .or_else(|| row["source_rate"].as_u64())
            .unwrap() as u32;
        let target = row["target_rate"].as_u64().unwrap_or(u64::from(rate)) as u32;
        let dir = tempfile::tempdir().unwrap();
        let (root, mut snapshot) = fixture(
            dir.path(),
            &input,
            channels,
            rate,
            if kind == "preparation" {
                AssetKind::Video
            } else {
                AssetKind::Audio
            },
            kind != "stretch",
        );
        if kind == "stretch" {
            voice(&mut snapshot).timeline_range.duration =
                time((expected.len() / channels as usize) as u64, target);
            let ElementContent::Audio { retime, .. } = &mut voice(&mut snapshot).content else {
                panic!()
            };
            let tempo = row["tempo"].as_f64().unwrap();
            retime.rate = RationalRate {
                numerator: (tempo * 100.0) as u32,
                denominator: 100,
            };
            retime.maintain_pitch = true;
        }
        // decodeAudioData truncates its buffer; the authored native duration
        // can ceil to one more output sample. Select that actual decoder extent
        // through the existing range contract, keeping the valid source clock.
        let range = (kind == "decode").then(|| ClassicAudioRange {
            start_sample: "0".into(),
            frame_count: (expected.len() / channels as usize).to_string(),
        });
        let (pcm, receipt) = execute(&snapshot, &root, target, range);
        let actual: Vec<f32> = if channels == 2 {
            pcm.channels[0]
                .iter()
                .zip(&pcm.channels[1])
                .flat_map(|(a, b)| [*a, *b])
                .collect()
        } else {
            assert_eq!(pcm.channels[0], pcm.channels[1]);
            pcm.channels[0].clone()
        };
        let expected = if kind == "compressor" && name.ends_with("small") {
            assert!(!receipt.mastering.applied);
            input.clone()
        } else if kind == "compressor" {
            assert!(receipt.mastering.applied);
            expected.iter().map(|v| v.clamp(-0.98, 0.98)).collect()
        } else {
            assert!(!receipt.mastering.applied);
            expected
        };
        let (maximum, snr) = error(&actual, &expected);
        let gate = if kind == "compressor" { 0.000001 } else { 0.0 };
        results.push(json!({"case":name,"maximum_absolute_error":maximum,"snr_db":snr,"maximum_gate":gate,"passed":maximum<=gate&&snr>=100.0,
            "decoder":receipt.decoders[0].profile,"peak_accounted_bytes":receipt.peak_accounted_bytes}));
        if kind == "compressor" && !name.ends_with("small") {
            let start = rate as u64 * 3 / 4;
            let count = 1024;
            let (cropped, crop_receipt) = execute(
                &snapshot,
                &root,
                target,
                Some(ClassicAudioRange {
                    start_sample: start.to_string(),
                    frame_count: count.to_string(),
                }),
            );
            let actual: Vec<f32> = cropped.channels[0]
                .iter()
                .zip(&cropped.channels[1])
                .flat_map(|(a, b)| [*a, *b])
                .collect();
            let (maximum, snr) = error(
                &actual,
                &expected[start as usize * 2..(start + count) as usize * 2],
            );
            assert!(crop_receipt.mastering.applied);
            results.push(json!({"case":format!("{name}-post-master-crop"),"maximum_absolute_error":maximum,"snr_db":snr,"maximum_gate":gate,"passed":maximum<=gate&&snr>=100.0}));
        }
    }
    let passed = results.iter().all(|r| r["passed"] == true);
    let binary = std::env::current_exe().unwrap();
    let hash = format!("{:x}", Sha256::digest(std::fs::read(binary).unwrap()));
    let report = json!({"test_binary_sha256":hash,"reference_manifest_sha256":format!("{:x}",Sha256::digest(std::fs::read(directory.join("manifest.json")).unwrap())),
        "scope":"real application and verified CAS through libav, native preparation, mixing, whole-scene mastering and selected output; no isolated worker or production host cutover", "fitting":"none", "cases":results,"passed":passed});
    let path = directory.join(format!(
        "native-engine-{}-{}.json",
        &hash[..12],
        std::process::id()
    ));
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(
        passed,
        "Independent native engine PCM differs; see {}",
        path.display()
    );
}
