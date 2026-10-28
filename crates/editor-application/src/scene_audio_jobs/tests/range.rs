use super::*;
use crate::classic_audio::*;
use opencut_composition::scene::PreparedPreviewAudio;
use serde_json::json;

fn range_request(start: &str, count: &str) -> SubmitAudioRangeWav {
    let base = request();
    SubmitAudioRangeWav {
        idempotency_key: base.idempotency_key,
        project_id: base.project_id,
        revision: base.revision,
        sequence_id: base.sequence_id,
        sample_rate: base.sample_rate,
        range: AudioSampleRange {
            start_sample: start.into(),
            frame_count: count.into(),
        },
    }
}
fn range_result(input: &PreparedAudioWav) -> StoredAudioRangeWavResult {
    let mut whole = input.clone();
    whole.output_range = None;
    let stored = result(&whole);
    let old = stored.public.identity;
    let range = input.output_range.as_ref().unwrap();
    let bytes = 58 + decimal(&range.frame_count).unwrap() * 8;
    let mut public_artifact = stored.public.artifact;
    public_artifact.size_bytes = bytes.to_string();
    let mut artifact = stored.artifact;
    artifact.size = bytes;
    StoredAudioRangeWavResult {
        audio_decoder_evidence_v1: None,
        public: AudioRangeWavResult {
            schema_version: 1,
            identity: AudioRangeWavIdentity {
                project_id: old.project_id,
                revision: old.revision,
                sequence_id: old.sequence_id,
                profile: old.profile,
                sample_rate: old.sample_rate,
                channels: old.channels,
                start_sample: range.start_sample.clone(),
                frame_count: range.frame_count.clone(),
                whole_scene_frame_count: old.frame_count,
                sample_format: old.sample_format,
            },
            asset_lock: stored.public.asset_lock,
            render_input_sha256: render_range_input_sha256(
                &input.snapshot,
                &input.sequence_id,
                input.sample_rate,
                &AudioSampleRange {
                    start_sample: range.start_sample.clone(),
                    frame_count: range.frame_count.clone(),
                },
            )
            .unwrap(),
            execution: stored.public.execution,
            artifact: public_artifact,
        },
        artifact,
    }
}
#[test]
fn range_submission_preserves_full_fixed_scene_and_whole_resource_lock() {
    let mut store = Store::new();
    let capture = Capture::default();
    let input = range_request("48001", "17");
    let accepted = submit_audio_range_wav(&mut store, &capture, input.clone()).unwrap();
    assert_eq!(accepted.kind, AUDIO_RANGE_WAV_KIND);
    let prepared: PreparedAudioWav = serde_json::from_value(accepted.input).unwrap();
    assert_eq!(prepared.snapshot, store.snapshot);
    assert_eq!(*capture.ids.borrow(), vec!["audible"]);
    let audio = validate_prepared(&prepared).unwrap();
    assert_eq!(audio.voices().count(), 2);
    assert_eq!(audio.duration_samples(), 102858);
    let session = ClassicAudioSession::new_with_range(
        &audio,
        policy().max_pcm_bytes,
        policy().max_encoded_bytes,
        classic_output_range(&prepared),
    )
    .unwrap();
    assert_eq!(session.output_frame_count(), 17);
    assert_eq!(session.plan().unwrap()["frameCount"], "102858");
    assert_eq!(session.plan().unwrap()["outputFrameCount"], "17");
    assert_eq!(session.plan().unwrap()["range"]["startSample"], "48001");
    assert_eq!(
        submit_audio_range_wav(&mut store, &capture, input.clone())
            .unwrap()
            .job_id,
        "job_".to_owned() + &"c".repeat(32)
    );
    assert_eq!(capture.calls.get(), 1);
    store.denied = true;
    assert_eq!(
        submit_audio_range_wav(&mut store, &capture, input)
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
}
#[test]
fn canonical_range_and_job_family_binding_cannot_be_reinterpreted() {
    for (start, count) in [
        ("00", "1"),
        ("0", "01"),
        ("+1", "1"),
        ("-1", "1"),
        ("0", "0"),
        ("18446744073709551615", "1"),
    ] {
        assert!(validate_range_submission(&range_request(start, count)).is_err());
    }
    for (start, count) in [("102858", "1"), ("102857", "2")] {
        let mut store = Store::new();
        let capture = Capture::default();
        assert!(submit_audio_range_wav(&mut store, &capture, range_request(start, count)).is_err());
        assert_eq!(capture.calls.get(), 0);
        assert_eq!(store.commits, 0);
    }
    let input = range_request("102857", "1");
    let mut store = Store::new();
    let accepted = submit_audio_range_wav(&mut store, &Capture::default(), input.clone()).unwrap();
    let mut prepared: PreparedAudioWav = serde_json::from_value(accepted.input).unwrap();
    let intent = JobRequest {
        kind: AUDIO_RANGE_WAV_KIND.into(),
        idempotency_key: input.idempotency_key.clone(),
        input: serde_json::to_value(&input).unwrap(),
    };
    validate_prepared_request(&intent, &prepared).unwrap();
    prepared.output_range.as_mut().unwrap().start_sample = "102856".into();
    assert!(validate_prepared_request(&intent, &prepared).is_err());
    let whole = JobRequest {
        kind: AUDIO_WAV_KIND.into(),
        idempotency_key: request().idempotency_key.clone(),
        input: serde_json::to_value(request()).unwrap(),
    };
    assert!(validate_prepared_request(&whole, &prepared).is_err());
    prepared.output_range = None;
    assert!(validate_prepared_request(&intent, &prepared).is_err());
}
#[test]
fn range_result_binds_exact_interval_complete_input_and_exact_wav_size() {
    let mut store = Store::new();
    let accepted =
        submit_audio_range_wav(&mut store, &Capture::default(), range_request("4095", "3"))
            .unwrap();
    let prepared: PreparedAudioWav = serde_json::from_value(accepted.input).unwrap();
    let stored = range_result(&prepared);
    assert_eq!(stored.artifact.size, 82);
    assert_eq!(
        validate_stored_range_result(&prepared, &stored).unwrap(),
        stored.public
    );
    let json = serde_json::to_string(&stored.public).unwrap();
    assert!(
        !json.contains("/private") && !json.contains("\"path\"") && !json.contains("credential")
    );
    let mut wrong = stored.clone();
    wrong.public.identity.start_sample = "4096".into();
    assert!(validate_stored_range_result(&prepared, &wrong).is_err());
    let mut wrong = stored.clone();
    wrong.artifact.size += 8;
    wrong.public.artifact.size_bytes = wrong.artifact.size.to_string();
    assert!(validate_stored_range_result(&prepared, &wrong).is_err());
    let mut changed = prepared.clone();
    changed.snapshot.project.name.push('x');
    assert!(validate_stored_range_result(&changed, &stored).is_err());
    let mut whole = prepared.clone();
    whole.output_range = None;
    assert!(validate_stored_range_result(&whole, &stored).is_err());
    assert!(validate_stored_result(&prepared, &result(&whole)).is_err());
    let mut changed = prepared.clone();
    changed.output_range.as_mut().unwrap().start_sample = "4096".into();
    assert!(validate_stored_range_result(&changed, &stored).is_err());
}

#[derive(Default)]
struct Resources {
    denied: bool,
}
impl ClassicAudioResources for Resources {
    fn resolve(&mut self, _: &AudioResource, _: &str) -> Result<(String, u64), EditorError> {
        unreachable!()
    }
    fn read(&mut self, _: &str, _: u64, _: u32) -> Result<(Vec<u8>, bool), EditorError> {
        unreachable!()
    }
    fn release(&mut self, _: &str) -> Result<(), EditorError> {
        unreachable!()
    }
    fn checkpoint(&mut self) -> Result<(), EditorError> {
        if self.denied {
            Err(crate::access::denied())
        } else {
            Ok(())
        }
    }
}
#[test]
fn dense_fixed_scene_admission_counts_unique_resources_separately() {
    let mut snapshot = snapshot();
    let original = snapshot.project.sequences[0].tracks[1].elements[0].clone();
    snapshot.project.sequences[0].tracks[1].elements = (0..100)
        .map(|index| {
            let mut element = original.clone();
            element.id = format!("dense-{index}");
            element.timeline_range.start = time(index, 1);
            element
        })
        .collect();
    let audio = prepare(&snapshot, "scene", 48000).unwrap();
    // The range transport must still reject a budget below its minimum cache.
    let under_budget = ClassicAudioSession::new(&audio, 256 * 1024 * 1024, 1024).unwrap();
    assert!(under_budget.plan().is_err());
    let session = ClassicAudioSession::new(&audio, 256 * 1024 * 1024, 32 * 1024 * 1024).unwrap();
    let plan = session.plan().unwrap();
    assert_eq!(plan["voices"].as_array().unwrap().len(), 100);
    assert_eq!(plan["resources"].as_array().unwrap().len(), 1);

    let asset = snapshot.project.assets[0].clone();
    snapshot.project.assets = (0..33)
        .map(|index| {
            let mut asset = asset.clone();
            asset.id = format!("resource-{index}");
            asset
        })
        .collect();
    let elements = &mut snapshot.project.sequences[0].tracks[1].elements;
    elements.truncate(33);
    for (index, element) in elements.iter_mut().enumerate() {
        let ElementContent::Audio { asset_id, .. } = &mut element.content else {
            unreachable!()
        };
        *asset_id = format!("resource-{index}");
    }
    let audio = prepare(&snapshot, "scene", 48000).unwrap();
    assert!(ClassicAudioSession::new(&audio, 256 * 1024 * 1024, 32 * 1024 * 1024).is_err());
}

#[test]
fn wav_callback_returns_budgeted_prefix_without_legalizing_invalid_requests() {
    let mut snapshot = snapshot();
    let original = snapshot.project.sequences[0].tracks[1].elements[0].clone();
    let original_track = snapshot.project.sequences[0].tracks[1].clone();
    snapshot.project.sequences[0].tracks.truncate(1);
    snapshot.project.sequences[0]
        .tracks
        .extend((0..32).map(|index| {
            let mut element = original.clone();
            element.id = format!("sustained-{index}");
            let mut track = original_track.clone();
            track.id = format!("sustained-track-{index}");
            track.elements = vec![element];
            track
        }));
    let audio = prepare(&snapshot, "scene", 48000).unwrap();
    let mut session = ClassicAudioSession::new(&audio, 16 * 1024 * 1024, 1024).unwrap();
    let mut resources = Resources::default();
    session
        .callback(
            AudioCallback::Admit(DecodeAdmission {
                resource: session.resource("audible").unwrap(),
                primary: None,
            }),
            &mut resources,
        )
        .unwrap();
    for index in 0..32 {
        session
            .callback(
                AudioCallback::Pitch(PitchRequest {
                    element_id: format!("sustained-{index}"),
                    decoded_sample_rate: 48000,
                    decoded_frame_count: "48000".into(),
                    decoded_channels: 1,
                    resident_frame_count: None,
                }),
                &mut resources,
            )
            .unwrap();
    }
    assert!(
        session
            .callback(
                AudioCallback::ReleaseDecoded(DecodedRelease {
                    resource: session.resource("audible").unwrap()
                }),
                &mut resources
            )
            .is_err(),
        "Ordinary voices still borrow the decoded source during mixing"
    );
    for start in [0, 2048, 4096, 6144] {
        let remaining = (audio.duration_samples() - start) as u32;
        let expected = remaining.min(2048);
        let block = serde_json::to_value(
            session
                .compile_export_block_typed(
                    BlockRequest {
                        start_sample: start.to_string(),
                        frame_count: remaining.min(4096),
                        source_formats: (0..32)
                            .map(|index| FinalSourceFormat {
                                element_id: format!("sustained-{index}"),
                                sample_rate: 48000,
                                frame_count: "48000".into(),
                                prepared: false,
                                channels: 1,
                            })
                            .collect(),
                    },
                    &mut resources,
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(block["startSample"], start.to_string());
        assert_eq!(block["frameCount"], expected);
        let voices = block["voices"].as_array().unwrap();
        assert_eq!(voices.len(), 32);
        assert!(voices.iter().all(|voice| voice["frameCount"] == expected));
    }
    assert_eq!(
        session
            .callback(
                AudioCallback::ReleaseDecoded(DecodedRelease {
                    resource: session.resource("audible").unwrap()
                }),
                &mut resources
            )
            .unwrap()["released"],
        true
    );
    for (start, count) in [
        (0, 0),
        (0, 4097),
        (audio.duration_samples(), 1),
        (audio.duration_samples() - 1, 2),
    ] {
        assert!(
            session
                .callback(
                    AudioCallback::Block(BlockRequest {
                        start_sample: start.to_string(),
                        frame_count: count,
                        source_formats: vec![],
                    }),
                    &mut resources,
                )
                .is_err()
        );
    }
}
#[test]
fn actual_primary_overrides_first_stream_admission_and_receipt_cannot_change_selection_or_range() {
    let mut snapshot = snapshot();
    snapshot.project.assets[0].kind = AssetKind::Video;
    for element in &mut snapshot.project.sequences[0].tracks[1].elements {
        if let ElementContent::Audio { retime, .. } = &mut element.content {
            retime.maintain_pitch = true;
        }
    }
    // Historical probe describes the first mono stream; actual primary is stereo.
    let audio = prepare(&snapshot, "scene", 48000).unwrap();
    let mut multichannel =
        ClassicAudioSession::new(&audio, policy().max_pcm_bytes, policy().max_encoded_bytes)
            .unwrap();
    let mut multichannel_resources = Resources::default();
    let six = ClassicAudioPrimarySelection {
        track_id: 2,
        codec: "aac".into(),
        sample_rate: 44100,
        channels: 6,
    };
    let admitted = multichannel
        .callback(
            AudioCallback::Admit(DecodeAdmission {
                resource: multichannel.resource("audible").unwrap(),
                primary: Some(six),
            }),
            &mut multichannel_resources,
        )
        .unwrap();
    assert_eq!(admitted["maxChannels"], 6);
    let decoded = PitchRequest {
        element_id: "voice-a".into(),
        decoded_sample_rate: 48000,
        decoded_frame_count: "48000".into(),
        decoded_channels: 2,
        resident_frame_count: None,
    };
    let mut wrong = decoded.clone();
    wrong.decoded_channels = 6;
    assert!(
        multichannel
            .callback(AudioCallback::Pitch(wrong), &mut multichannel_resources)
            .is_err()
    );
    multichannel
        .callback(AudioCallback::Pitch(decoded), &mut multichannel_resources)
        .unwrap();
    let range = ClassicAudioRange {
        start_sample: "17".into(),
        frame_count: "4096".into(),
    };
    let mut session = ClassicAudioSession::new_with_range(
        &audio,
        policy().max_pcm_bytes,
        policy().max_encoded_bytes,
        Some(range.clone()),
    )
    .unwrap();
    let resource = session.resource("audible").unwrap();
    let mut resources = Resources::default();
    assert!(
        session
            .callback(
                AudioCallback::Admit(DecodeAdmission {
                    resource: resource.clone(),
                    primary: None
                }),
                &mut resources
            )
            .is_err()
    );
    let primary = ClassicAudioPrimarySelection {
        track_id: 2,
        codec: "pcm-f32".into(),
        sample_rate: 48000,
        channels: 2,
    };
    let reply = session
        .callback(
            AudioCallback::Admit(DecodeAdmission {
                resource: resource.clone(),
                primary: Some(primary.clone()),
            }),
            &mut resources,
        )
        .unwrap();
    assert_eq!(reply["maxChannels"], 2);
    for altered in [
        ClassicAudioPrimarySelection {
            track_id: 3,
            ..primary.clone()
        },
        ClassicAudioPrimarySelection {
            channels: 65,
            ..primary.clone()
        },
        ClassicAudioPrimarySelection {
            sample_rate: 0,
            ..primary.clone()
        },
    ] {
        assert!(
            session
                .callback(
                    AudioCallback::Admit(DecodeAdmission {
                        resource: resource.clone(),
                        primary: Some(altered)
                    }),
                    &mut resources
                )
                .is_err()
        );
    }
    resources.denied = true;
    assert_eq!(
        session
            .callback(
                AudioCallback::Admit(DecodeAdmission {
                    resource: resource.clone(),
                    primary: Some(primary.clone())
                }),
                &mut resources
            )
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
    resources.denied = false;
    assert!(
        session
            .callback(
                AudioCallback::ReleaseDecoded(DecodedRelease {
                    resource: resource.clone()
                }),
                &mut resources
            )
            .is_err(),
        "All preparation consumers must finish before decoder release"
    );
    let mut preparation = vec![];
    for id in ["voice-a", "voice-b"] {
        let value = session
            .callback(
                AudioCallback::Pitch(PitchRequest {
                    element_id: id.into(),
                    decoded_sample_rate: 48000,
                    decoded_frame_count: "96000".into(),
                    decoded_channels: 2,
                    resident_frame_count: None,
                }),
                &mut resources,
            )
            .unwrap();
        preparation.push(ClassicAudioPreparationReceipt {
            element_id: id.into(),
            facts: serde_json::from_value(value).unwrap(),
        });
        if id == "voice-a" {
            assert!(
                session
                    .callback(
                        AudioCallback::ReleaseDecoded(DecodedRelease {
                            resource: resource.clone()
                        }),
                        &mut resources
                    )
                    .is_err()
            );
        }
    }
    resources.denied = true;
    assert_eq!(
        session
            .callback(
                AudioCallback::ReleaseDecoded(DecodedRelease {
                    resource: resource.clone()
                }),
                &mut resources
            )
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
    resources.denied = false;
    assert_eq!(
        session
            .callback(
                AudioCallback::ReleaseDecoded(DecodedRelease {
                    resource: resource.clone()
                }),
                &mut resources
            )
            .unwrap()["released"],
        true
    );
    assert!(
        session
            .callback(
                AudioCallback::Admit(DecodeAdmission {
                    resource: resource.clone(),
                    primary: Some(primary.clone())
                }),
                &mut resources
            )
            .is_err(),
        "A retired decoder cannot allocate or reopen in the same execution"
    );
    let mut receipt = ClassicAudioRenderReceipt {
        identity: ClassicAudioIdentity {
            project_id: "project".into(),
            sequence_id: "scene".into(),
            revision: "fixed-r1".into(),
            profile: "classic-export-v31-web-audio".into(),
            sample_rate: 48000,
            channels: 2,
        },
        engine_identity: "actual-engine".into(),
        decoders: vec![ClassicAudioDecoderReceipt {
            asset_id: "audible".into(),
            profile: "mediabunny-1.41.0-concatenate-offline-resample".into(),
            sample_rate: 48000,
            frame_count: "96000".into(),
            channels: 2,
            primary: Some(primary),
        }],
        preparation,
        mastering: ClassicAudioMasteringReceipt {
            profile: "classic-export-whole-scene-web-audio-v31".into(),
            whole_scene_frames: "102858".into(),
            input_peak: 0.5,
            applied: false,
        },
        range: Some(range.clone()),
        peak_accounted_bytes: 0,
    };
    session.validate_receipt(&receipt).unwrap();
    receipt.range = None;
    assert!(session.validate_receipt(&receipt).is_err());
    receipt.range = Some(range);
    receipt.decoders[0].primary.as_mut().unwrap().track_id = 3;
    assert!(session.validate_receipt(&receipt).is_err());
}

#[test]
fn decoder_windows_preserves_global_taps_resample_phase_budget_and_live_authority() {
    let mut source = snapshot();
    source.project.assets[0].kind = AssetKind::Video;
    source.project.assets[0].duration = Some(time(300, 1));
    for (index, element) in source.project.sequences[0].tracks[1]
        .elements
        .iter_mut()
        .take(2)
        .enumerate()
    {
        element.timeline_range.duration = time(1, 1);
        element.source_span = ElementSourceSpan::ClassicTrimmed {
            trim_start: time(2940 + index as i64, 10),
            trim_end: time(50 - index as i64, 10),
            source_duration: Some(time(300, 1)),
        };
    }
    let audio = prepare(&source, "scene", 48000).unwrap();
    let mut session =
        ClassicAudioSession::new(&audio, 256 * 1024 * 1024, 32 * 1024 * 1024).unwrap();
    let mut resources = Resources::default();
    let request = DecodeAdmission {
        resource: session.resource("audible").unwrap(),
        primary: Some(ClassicAudioPrimarySelection {
            track_id: 2,
            codec: "aac".into(),
            sample_rate: 44100,
            channels: 2,
        }),
    };
    let reply = session
        .callback(
            AudioCallback::AdmitContainer(request.clone()),
            &mut resources,
        )
        .unwrap();
    assert_eq!(reply["sourceSampleRate"], 44100);
    assert_eq!(reply["targetSampleRate"], 48000);
    assert_eq!(reply["maxChannels"], 2);
    assert_eq!(reply["mode"], "windowed");
    assert_eq!(
        reply["windows"].as_array().unwrap().len(),
        1,
        "overlapping source/filter ranges must merge"
    );
    let window = &reply["windows"][0];
    let start: u64 = window["sourceStartFrame"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let end: u64 = window["sourceEndFrame"].as_str().unwrap().parse().unwrap();
    let target: u64 = window["targetStartFrame"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(start % 147, 0);
    assert_eq!((start + 147) * 48000, target * 44100);
    assert!(target < 294 * 48000 && end > 294 * 44100);
    let working = reply["workingFrames"].as_u64().unwrap();
    assert!(working * 2 * 32 < 256 * 1024 * 1024);
    assert!(reply["maxFrames"].as_u64().unwrap() > 300 * 48000);
    assert_eq!(
        session
            .callback(
                AudioCallback::AdmitContainer(request.clone()),
                &mut resources
            )
            .unwrap(),
        reply
    );
    assert!(
        session
            .callback(AudioCallback::Admit(request.clone()), &mut resources)
            .is_err()
    );
    let mut changed = request.clone();
    changed.primary.as_mut().unwrap().track_id += 1;
    assert!(
        session
            .callback(AudioCallback::AdmitContainer(changed), &mut resources)
            .is_err()
    );
    let decoded = PitchRequest {
        element_id: "voice-a".into(),
        decoded_sample_rate: 48000,
        decoded_frame_count: "14400000".into(),
        decoded_channels: 2,
        resident_frame_count: None,
    };
    assert!(
        session
            .callback(AudioCallback::Pitch(decoded.clone()), &mut resources)
            .is_err()
    );
    let observed = PitchRequest {
        resident_frame_count: Some("48000".into()),
        ..decoded
    };
    let facts = session
        .callback(AudioCallback::Pitch(observed.clone()), &mut resources)
        .unwrap();
    assert_eq!(facts["decodedBytes"], "384000");
    let mut changed = observed;
    changed.resident_frame_count = Some("48001".into());
    assert!(
        session
            .callback(AudioCallback::Pitch(changed), &mut resources)
            .is_err()
    );
    resources.denied = true;
    assert_eq!(
        session
            .callback(AudioCallback::AdmitContainer(request), &mut resources)
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
    resources.denied = false;
    // One asset feeds ordinary PTS playback and a prepared pitch voice.
    source.project.sequences[0].tracks[1].elements[1]
        .timeline_range
        .duration = time(1, 2);
    if let ElementContent::Audio { retime, .. } =
        &mut source.project.sequences[0].tracks[1].elements[1].content
    {
        retime.rate = RationalRate {
            numerator: 2,
            denominator: 1,
        };
        retime.maintain_pitch = true;
    }
    let preview = PreparedPreviewAudio::new(
        &source,
        "scene",
        PcmFormat {
            sample_rate: 48000,
            channels: 2,
        },
    )
    .unwrap();
    let mut session =
        ClassicAudioSession::new_preview(&preview, 256 * 1024 * 1024, 32 * 1024 * 1024).unwrap();
    let resource = session.resource("audible").unwrap();
    let request = DecodeAdmission {
        resource: resource.clone(),
        primary: Some(ClassicAudioPrimarySelection {
            track_id: 2,
            codec: "aac".into(),
            sample_rate: 44100,
            channels: 2,
        }),
    };
    assert!(
        session
            .callback(
                AudioCallback::AdmitContainer(request.clone()),
                &mut resources
            )
            .is_err()
    );
    let admitted = session
        .callback(AudioCallback::AdmitPreviewDecode(request), &mut resources)
        .unwrap();
    assert!(admitted["stream"].is_object());
    assert_eq!(admitted["preparation"]["mode"], "windowed");
    let decoded = PreviewDecodedObservation {
        resource,
        sample_rate: 44100,
        frame_count: "13230080".into(),
        channels: 2,
    };
    assert!(
        session
            .callback(
                AudioCallback::ObservePreviewDecoded(decoded.clone()),
                &mut resources
            )
            .is_err()
    );
    let observation = || WindowedPreviewObservation {
        decoded: decoded.clone(),
        resident_frame_count: "0".into(),
    };
    session
        .callback(
            AudioCallback::ObserveWindowedPreview(observation()),
            &mut resources,
        )
        .unwrap();
    session
        .callback(
            AudioCallback::ObserveWindowedPreview(observation()),
            &mut resources,
        )
        .unwrap();
    let mut changed = observation();
    changed.resident_frame_count = "1".into();
    assert!(
        session
            .callback(
                AudioCallback::ObserveWindowedPreview(changed),
                &mut resources
            )
            .is_err()
    );
    let windows = session
        .callback(
            AudioCallback::PreviewWindows(PreviewBlockRequest {
                start_sample: "48000".into(),
                frame_count: 128,
            }),
            &mut resources,
        )
        .unwrap();
    assert_eq!(
        windows,
        json!([]),
        "Only the prepared shared-source voice participates at1s;it must not request ordinary PTS windows"
    );
    resources.denied = true;
    assert_eq!(
        session
            .callback(
                AudioCallback::ObserveWindowedPreview(observation()),
                &mut resources
            )
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
}

#[test]
fn pcm_window_probe_keeps_legacy_state_and_long_source_admission_fences() {
    let short = snapshot();
    let audio = prepare(&short, "scene", 48000).unwrap();
    let mut session =
        ClassicAudioSession::new(&audio, 256 * 1024 * 1024, 32 * 1024 * 1024).unwrap();
    let mut resources = Resources::default();
    let request = DecodeAdmission {
        resource: session.resource("audible").unwrap(),
        primary: Some(ClassicAudioPrimarySelection {
            track_id: 1,
            codec: "pcm-s16".into(),
            sample_rate: 48000,
            channels: 1,
        }),
    };
    assert_eq!(
        session
            .callback(
                AudioCallback::AdmitPcmAudioWindowed(request.clone()),
                &mut resources
            )
            .unwrap()["mode"],
        "whole"
    );
    // Declining the new path cannot leave a selected primary or admission that
    // changes the old Web Audio callback contract.
    assert!(
        session
            .callback(
                AudioCallback::Admit(DecodeAdmission {
                    resource: request.resource.clone(),
                    primary: None
                }),
                &mut resources
            )
            .is_ok()
    );
    assert!(
        session
            .callback(AudioCallback::Admit(request.clone()), &mut resources)
            .is_err()
    );

    let mut long = snapshot();
    long.project.assets[0].duration = Some(time(300, 1));
    long.project.assets[0].version.size = 28_800_044;
    for (index, element) in long.project.sequences[0].tracks[1]
        .elements
        .iter_mut()
        .take(2)
        .enumerate()
    {
        element.timeline_range.duration = time(1, 1);
        element.source_span = ElementSourceSpan::ClassicTrimmed {
            trim_start: time(2940 + index as i64, 10),
            trim_end: time(50 - index as i64, 10),
            source_duration: Some(time(300, 1)),
        };
    }
    let audio = prepare(&long, "scene", 48000).unwrap();
    let mut session =
        ClassicAudioSession::new(&audio, 256 * 1024 * 1024, 32 * 1024 * 1024).unwrap();
    let request = DecodeAdmission {
        resource: session.resource("audible").unwrap(),
        ..request
    };
    // Demuxed audio containers also use bounded windows. Exercise that
    // admission separately so it cannot pre-populate the PCM probe below.
    let mut container_session =
        ClassicAudioSession::new(&audio, 256 * 1024 * 1024, 32 * 1024 * 1024).unwrap();
    let container_reply = container_session
        .callback(AudioCallback::AdmitContainer(request.clone()), &mut resources)
        .unwrap();
    assert_eq!(container_reply["mode"], "windowed");
    let reply = session
        .callback(
            AudioCallback::AdmitPcmAudioWindowed(request.clone()),
            &mut resources,
        )
        .unwrap();
    assert_eq!(reply["mode"], "windowed");
    assert_eq!(reply["admission"]["mode"], "windowed");
    assert_eq!(reply["admission"]["maxChannels"], 1);
    assert!(reply["admission"]["maxFrames"].as_u64().unwrap() > 300 * 48000);
    assert!(reply["admission"]["workingFrames"].as_u64().unwrap() < 300 * 48000);
    assert_eq!(
        session
            .callback(
                AudioCallback::AdmitPcmAudioWindowed(request.clone()),
                &mut resources
            )
            .unwrap(),
        reply
    );
    let facts = session
        .callback(
            AudioCallback::Pitch(PitchRequest {
                element_id: "voice-a".into(),
                decoded_sample_rate: 48000,
                decoded_frame_count: "14400000".into(),
                decoded_channels: 1,
                resident_frame_count: Some("48000".into()),
            }),
            &mut resources,
        )
        .unwrap();
    assert_eq!(facts["decodedBytes"], "192000");
    for (codec, rate, channels) in [
        ("aac", 48000, 1),
        ("pcm-s16", 44100, 1),
        ("pcm-s16", 48000, 3),
    ] {
        let mut changed = request.clone();
        let primary = changed.primary.as_mut().unwrap();
        primary.codec = codec.into();
        primary.sample_rate = rate;
        primary.channels = channels;
        assert!(
            session
                .callback(
                    AudioCallback::AdmitPcmAudioWindowed(changed),
                    &mut resources
                )
                .is_err()
        );
    }
    assert!(
        session
            .callback(AudioCallback::Admit(request.clone()), &mut resources)
            .is_err()
    );
    resources.denied = true;
    assert_eq!(
        session
            .callback(
                AudioCallback::AdmitPcmAudioWindowed(request),
                &mut resources
            )
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
}


#[test]
fn native_rate_float_pcm_reuses_bounded_windows_without_changing_asset_metadata() {
    let mut fixed = snapshot();
    fixed.project.assets[0].duration = Some(time(42, 1));
    fixed.project.assets[0].version.size = 16_132_096;
    fixed.project.assets[0].metadata.audio = AudioPresence::Unknown {};
    let before = fixed.clone();
    let audio = prepare(&fixed, "scene", 48000).unwrap();
    let mut session =
        ClassicAudioSession::new(&audio, 256 * 1024 * 1024, 32 * 1024 * 1024).unwrap();
    let mut resources = Resources::default();
    let request = DecodeAdmission {
        resource: session.resource("audible").unwrap(),
        primary: Some(ClassicAudioPrimarySelection {
            track_id: 1,
            codec: "pcm-f32".into(),
            sample_rate: 48000,
            channels: 2,
        }),
    };
    let reply = session
        .callback(
            AudioCallback::AdmitPcmAudioWindowed(request.clone()),
            &mut resources,
        )
        .unwrap();
    assert_eq!(reply["mode"], "windowed");
    assert_eq!(reply["admission"]["mode"], "windowed");
    assert_eq!(reply["admission"]["maxChannels"], 2);
    assert_eq!(fixed, before);
    let unique = audio
        .resources()
        .map(|r| (r.asset_id.as_str(), r))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut evidence = ClassicAudioDecoderEvidence {
        sources: unique
            .values()
            .map(|r| ClassicAudioDecoderSource {
                resource: session.resource(&r.asset_id).unwrap(),
                decoder: ClassicAudioDecoderReceipt {
                    asset_id: r.asset_id.clone(),
                    profile: "mediabunny-1.41.0-windowed-concatenate-offline-resample-v1".into(),
                    sample_rate: 48000,
                    frame_count: "2016000".into(),
                    channels: 2,
                    primary: request.primary.clone(),
                },
            })
            .collect(),
    };
    assert!(validate_decoder_evidence(&audio, &evidence).is_ok());
    // A float source has four bytes per channel sample; its proof cannot claim
    // the larger frame extent that a signed16 interpretation would allow.
    evidence.sources[0].decoder.frame_count = "2016513".into();
    assert!(validate_decoder_evidence(&audio, &evidence).is_err());
    let mut denied = request;
    denied.primary.as_mut().unwrap().codec = "pcm-f64".into();
    assert!(
        session
            .callback(AudioCallback::AdmitPcmAudioWindowed(denied), &mut resources)
            .is_err()
    );
}
