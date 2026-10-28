use super::*;
use crate::{JobState, scene::new_document};
use opencut_contracts::{
    AssetVersion, CreateProject, FrameRate, LockedAsset, Time, TimeRange, scene::*,
};
use std::cell::{Cell, RefCell};

fn time(ticks: i64, timebase: u32) -> Time {
    Time {
        ticks: ticks.to_string(),
        timebase,
    }
}
fn snapshot() -> EditorSnapshot {
    let mut project = new_document(&CreateProject {
        id: "project".into(),
        name: "Sound".into(),
        sequence_id: "scene".into(),
        width: 160,
        height: 90,
        fps: FrameRate {
            numerator: 30,
            denominator: 1,
        },
    })
    .unwrap();
    project.sequences[0].main_track_id = "main".into();
    project.sequences[0].tracks[0].id = "main".into();
    let resource = |id: &str, hash: &str| ProjectAsset {
        id: id.into(),
        name: id.into(),
        kind: AssetKind::Audio,
        version: AssetVersion {
            id: format!("av_{hash}"),
            content_hash: hash.into(),
            size: 16,
        },
        duration: Some(time(2, 1)),
        origin: ResourceOrigin::LegacyLocator {
            uri: "/private/source-do-not-publish.wav".into(),
        },
        metadata: ProjectMediaMetadata {
            mime_type: Some("audio/wav".into()),
            intrinsic_size: None,
            frame_rate: None,
            rotation_degrees: None,
            audio: AudioPresence::Present {
                metadata: Some(AudioMetadata {
                    sample_rate: 48000,
                    channels: 1,
                    channel_layout: None,
                }),
            },
        },
        license: None,
    };
    project.assets = vec![
        resource("audible", &"a".repeat(64)),
        resource("unused", &"b".repeat(64)),
    ];
    let element = |id: &str, start: i64, asset: &str, enabled: bool| TimelineElement {
        id: id.into(),
        name: id.into(),
        timeline_range: TimeRange {
            start: time(start, 1),
            duration: time(1, 7),
        },
        source_span: ElementSourceSpan::ClassicTrimmed {
            trim_start: time(1, 4),
            trim_end: time(0, 1),
            source_duration: None,
        },
        enabled,
        content: ElementContent::Audio {
            asset_id: asset.into(),
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
                mapping: RetimeMapping::ClassicRoundedTicks { timebase: 120000 },
            },
        },
        visual: None,
        channels: vec![],
    };
    let mut track = project.sequences[0].tracks[0].clone();
    track.id = "sound".into();
    track.kind = SceneTrackKind::Audio;
    track.elements = vec![
        element("voice-a", 0, "audible", true),
        element("voice-b", 1, "audible", true),
        element("disabled", 2, "unused", false),
    ];
    project.sequences[0].tracks.push(track);
    EditorSnapshot {
        project,
        revision: "fixed-r1".into(),
    }
}
fn policy() -> LocalAudioExecutionPolicy {
    LocalAudioExecutionPolicy {
        profile: LocalAudioProcessingProfile::LocalClassicAudioV1,
        max_pcm_bytes: 16 * 1024 * 1024,
        max_encoded_bytes: 1024 * 1024,
        max_rss_bytes: 64 * 1024 * 1024,
        deadline_ms: 20000,
    }
}
fn request() -> SubmitAudioWav {
    SubmitAudioWav {
        idempotency_key: "audio-key".into(),
        project_id: "project".into(),
        revision: "fixed-r1".into(),
        sequence_id: "scene".into(),
        sample_rate: 48000,
    }
}
struct Store {
    snapshot: EditorSnapshot,
    receipt: Option<JobRecord>,
    commits: usize,
    denied: bool,
}
impl Store {
    fn new() -> Self {
        Self {
            snapshot: snapshot(),
            receipt: None,
            commits: 0,
            denied: false,
        }
    }
}
impl AudioJobSubmissionStore for Store {
    fn fixed_scene_audio_snapshot(
        &self,
        _: &str,
        _: &str,
    ) -> Result<AuthorizedAudioSnapshot, EditorError> {
        if self.denied {
            return Err(crate::access::denied());
        }
        Ok(AuthorizedAudioSnapshot {
            snapshot: self.snapshot.clone(),
            authority: TrustedJobAuthority::Credential {
                principal_id: "agent".into(),
                credential_id: "credential".into(),
            },
            processing_policy: policy(),
        })
    }
    fn find_accepted_scene_audio_job(
        &self,
        _: &JobRequest,
    ) -> Result<Option<JobRecord>, EditorError> {
        Ok(self.receipt.clone())
    }
    fn accept_scene_audio_job(
        &mut self,
        request: JobRequest,
        prepared: PreparedAudioWav,
    ) -> Result<JobRecord, EditorError> {
        validate_prepared_request(&request, &prepared)?;
        self.commits += 1;
        let record = JobRecord {
            job_id: format!("job_{}", "c".repeat(32)),
            state: JobState::Queued,
            attempt: 0,
            kind: request.kind,
            input: serde_json::to_value(prepared).unwrap(),
            result: None,
            error: None,
            lease_owner: None,
            lease_until: None,
            cancel_requested: false,
            progress: None,
        };
        self.receipt = Some(record.clone());
        Ok(record)
    }
}
#[derive(Default)]
struct Capture {
    calls: Cell<usize>,
    ids: RefCell<Vec<String>>,
    corrupt: bool,
}
impl AudioAssetCapture for Capture {
    fn capture(
        &self,
        snapshot: &EditorSnapshot,
        sequence: &str,
        ids: &[String],
    ) -> Result<AssetLockManifest, EditorError> {
        self.calls.set(self.calls.get() + 1);
        *self.ids.borrow_mut() = ids.to_vec();
        Ok(AssetLockManifest {
            version: 1,
            revision: snapshot.revision.clone(),
            sequence_id: sequence.into(),
            assets: ids
                .iter()
                .map(|id| {
                    let asset = snapshot
                        .project
                        .assets
                        .iter()
                        .find(|a| a.id == *id)
                        .unwrap();
                    LockedAsset {
                        asset_id: id.clone(),
                        content_hash: asset.version.content_hash.clone(),
                        size: if self.corrupt { 99 } else { asset.version.size },
                    }
                })
                .collect(),
        })
    }
}

#[test]
fn explicit_stream_zero_is_not_implicitly_rebound_to_the_primary_audio_track() {
    for stream in [0, 1] {
        let mut store = Store::new();
        let ElementContent::Audio { audio, .. } =
            &mut store.snapshot.project.sequences[0].tracks[1].elements[0].content
        else {
            panic!("Audio fixture changed");
        };
        audio.stream = Some(stream);
        let error = submit_audio_wav(&mut store, &Capture::default(), request()).unwrap_err();
        assert_eq!(error.code, "UNSUPPORTED_AUDIO_CAPABILITY");
        assert_eq!(store.commits, 0);
    }
    let mut store = Store::new();
    submit_audio_wav(&mut store, &Capture::default(), request()).unwrap();
    assert_eq!(store.commits, 1, "None retains the primary-track profile");
}

#[test]
fn submission_locks_unique_participating_resources_preserves_fixed_aggregate_and_replay_reauthorizes()
 {
    let mut store = Store::new();
    let capture = Capture::default();
    let original = request();
    let accepted = submit_audio_wav(&mut store, &capture, original.clone()).unwrap();
    let prepared: PreparedAudioWav = serde_json::from_value(accepted.input.clone()).unwrap();
    assert!(prepared.output_range.is_none());
    assert!(
        !accepted
            .input
            .as_object()
            .unwrap()
            .contains_key("output_range")
    );
    assert_eq!(prepared.snapshot, store.snapshot);
    assert_eq!(*capture.ids.borrow(), vec!["audible"]);
    assert_eq!(prepared.asset_lock.assets.len(), 1);
    let audio = validate_prepared(&prepared).unwrap();
    assert_eq!(audio.voices().count(), 2);
    // The disabled tail still contributes to authored scene duration, while its
    // resource is not captured or decoded. 15/7 seconds => ceil(48000*15/7).
    assert_eq!(audio.duration_samples(), 102858);
    assert_eq!(
        audio
            .voices()
            .next()
            .unwrap()
            .source
            .clock
            .nominal_request
            .start,
        time(1, 4)
    );
    assert_eq!(
        submit_audio_wav(&mut store, &capture, original.clone()).unwrap(),
        accepted
    );
    assert_eq!(capture.calls.get(), 1);
    assert_eq!(store.commits, 1);
    store.denied = true;
    assert_eq!(
        submit_audio_wav(&mut store, &capture, original)
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
}

mod range;

#[test]
fn malformed_lock_rate_and_request_preparation_binding_never_accept_a_job() {
    let mut store = Store::new();
    let capture = Capture {
        corrupt: true,
        ..Capture::default()
    };
    assert_eq!(
        submit_audio_wav(&mut store, &capture, request())
            .unwrap_err()
            .code,
        "ASSET_LOCK_INVALID"
    );
    assert_eq!(store.commits, 0);
    let mut wrong_rate = request();
    wrong_rate.sample_rate = 96000;
    assert_eq!(
        submit_audio_wav(&mut store, &capture, wrong_rate)
            .unwrap_err()
            .code,
        "INVALID_ARGUMENT"
    );
    assert_eq!(capture.calls.get(), 1);
    let capture = Capture::default();
    let record = submit_audio_wav(&mut store, &capture, request()).unwrap();
    let prepared: PreparedAudioWav = serde_json::from_value(record.input).unwrap();
    let mut changed = request();
    changed.revision = "different-fixed-revision".into();
    let intent = JobRequest {
        kind: AUDIO_WAV_KIND.into(),
        idempotency_key: changed.idempotency_key.clone(),
        input: serde_json::to_value(changed).unwrap(),
    };
    assert_eq!(
        validate_prepared_request(&intent, &prepared)
            .unwrap_err()
            .code,
        "INVALID_ARGUMENT"
    );
}

fn result(prepared: &PreparedAudioWav) -> StoredAudioWavResult {
    let audio = validate_prepared(prepared).unwrap();
    let hash = "d".repeat(64);
    StoredAudioWavResult {
        audio_decoder_evidence_v1: None,
        public: AudioWavResult {
            schema_version: 1,
            identity: AudioWavIdentity {
                project_id: prepared.snapshot.project.id.clone(),
                revision: prepared.snapshot.revision.clone(),
                sequence_id: prepared.sequence_id.clone(),
                profile: AudioProfile::ClassicExportV31WebAudio,
                sample_rate: prepared.sample_rate,
                channels: 2,
                frame_count: audio.duration_samples().to_string(),
                sample_format: AudioSampleFormat::IeeeFloat32Le,
            },
            asset_lock: prepared.asset_lock.clone(),
            render_input_sha256: render_input_sha256(
                &prepared.snapshot,
                &prepared.sequence_id,
                prepared.sample_rate,
            )
            .unwrap(),
            execution: AudioExecutionSummary {
                host_sha256: "e".repeat(64),
                runtime:
                    opencut_job_contracts::scene_audio::AudioRuntimeIdentity::HistoricalBrowser(
                        opencut_job_contracts::scene_audio::HistoricalBrowserAudioIdentity {
                            browser_version: "Chromium pinned".into(),
                            browser_sha256: "f".repeat(64),
                            module_sha256: "a".repeat(64),
                            playwright_version: "1.63.0".into(),
                            mediabunny_version: "1.41.0".into(),
                            soundtouch_version: "0.3.0".into(),
                        },
                    ),
                decoder_profiles: vec![AudioDecoderProfile::WebAudioDecodeAtTargetRate],
                pitch_profiles: vec![AudioPitchProfile::UnpreparedLinear],
                mastering_profile: AudioMasteringProfile::ClassicExportWholeSceneWebAudioV31,
                mastering_applied: false,
            },
            artifact: AudioArtifactMetadata {
                mime_type: "audio/wav".into(),
                size_bytes: "822922".into(),
                content_hash: hash.clone(),
            },
        },
        artifact: ArtifactDescriptor {
            path: "/private/attempt/render.wav".into(),
            mime_type: "audio/wav".into(),
            size: 822922,
            content_hash: hash,
        },
    }
}

#[test]
fn public_result_is_path_free_and_full_snapshot_digest_rejects_non_audio_changes() {
    let mut store = Store::new();
    let mut record = submit_audio_wav(&mut store, &Capture::default(), request()).unwrap();
    let mut prepared: PreparedAudioWav = serde_json::from_value(record.input.clone()).unwrap();
    let stored = result(&prepared);
    record.state = JobState::Succeeded;
    record.attempt = 1;
    record.result = Some(serde_json::to_value(&stored).unwrap());
    let view = crate::job_views::project(record.clone());
    assert_eq!(view.target.unwrap().revision, "fixed-r1");
    assert_eq!(view.artifacts.len(), 1);
    let json = serde_json::to_string(&view.result).unwrap();
    assert!(
        !json.contains("/private/")
            && !json.contains("credential")
            && !json.contains("\"path\"")
            && !json.contains("\"uri\"")
    );
    let Some(opencut_api_contracts::job_results::JobResultView::Audio(data)) = view.result else {
        panic!("Expected typed audio result")
    };
    assert_eq!(
        serde_json::to_value(data).unwrap(),
        serde_json::to_value(opencut_api_contracts::job_results::AudioRenderResult::from(
            stored.public.clone()
        ))
        .unwrap()
    );
    // Matching public/private descriptors cannot authorize a short or padded
    // file: its exact byte count comes independently from the fixed scene.
    for size in [stored.artifact.size - 8, stored.artifact.size + 8] {
        let mut wrong_size = stored.clone();
        wrong_size.artifact.size = size;
        wrong_size.public.artifact.size_bytes = size.to_string();
        assert_eq!(
            validate_stored_result(&prepared, &wrong_size)
                .unwrap_err()
                .code,
            "INVALID_STORED_RESULT"
        );
        let mut invalid_record = record.clone();
        invalid_record.result = Some(serde_json::to_value(wrong_size).unwrap());
        let unavailable = crate::job_views::project(invalid_record);
        assert!(
            unavailable.result.is_none()
                && unavailable.artifacts.is_empty()
                && unavailable.result_unavailable.is_some()
        );
    }
    prepared.snapshot.project.sequences[0].bookmarks.push(
        opencut_contracts::scene_bookmarks::SceneBookmark {
            id: "note".into(),
            sequence_id: "scene".into(),
            time: time(0, 1),
            duration: None,
            color: None,
            note: Some("Non-audio edit".into()),
        },
    );
    assert_ne!(
        render_input_sha256(
            &prepared.snapshot,
            &prepared.sequence_id,
            prepared.sample_rate
        )
        .unwrap(),
        stored.public.render_input_sha256
    );
    assert_eq!(
        validate_stored_result(&prepared, &stored).unwrap_err().code,
        "INVALID_STORED_RESULT"
    );
    record.input = serde_json::to_value(prepared).unwrap();
    let denied = crate::job_views::project(record);
    assert!(
        denied.result.is_none()
            && denied.artifacts.is_empty()
            && denied.result_unavailable.is_some()
    );
}

#[test]
fn negotiated_pcm_provenance_is_exact_private_and_cannot_relabel_legacy_results() {
    use crate::classic_audio::*;
    let mut store = Store::new();
    let mut record = submit_audio_wav(&mut store, &Capture::default(), request()).unwrap();
    let input: PreparedAudioWav = serde_json::from_value(record.input.clone()).unwrap();
    let prepared = validate_prepared(&input).unwrap();
    let resource = prepared.resources().next().unwrap();
    let mut stored = result(&input);
    // No evidence keeps the legacy representation and validation unchanged.
    let legacy = serde_json::to_value(&stored).unwrap();
    assert!(legacy.get("audio_decoder_evidence_v1").is_none());
    assert!(validate_stored_result(&input, &serde_json::from_value(legacy).unwrap()).is_ok());
    stored.public.execution.decoder_profiles =
        vec![AudioDecoderProfile::Mediabunny141ConcatenateOfflineResample];
    assert!(validate_stored_result(&input, &stored).is_err());
    stored.audio_decoder_evidence_v1 = Some(ClassicAudioDecoderEvidence {
        sources: vec![ClassicAudioDecoderSource {
            resource: AudioResource {
                asset_id: resource.asset_id.clone(),
                content_hash: resource.version.content_hash.clone(),
                size: resource.version.size,
                kind: "audio".into(),
            },
            decoder: ClassicAudioDecoderReceipt {
                asset_id: resource.asset_id.clone(),
                profile: "mediabunny-1.41.0-windowed-concatenate-offline-resample-v1".into(),
                sample_rate: input.sample_rate,
                frame_count: "4".into(),
                channels: 1,
                primary: Some(ClassicAudioPrimarySelection {
                    track_id: 1,
                    codec: "pcm-s16".into(),
                    sample_rate: input.sample_rate,
                    channels: 1,
                }),
            },
        }],
    });
    assert!(validate_stored_result(&input, &stored).is_ok());
    for mutation in 0..10 {
        let mut changed = stored.clone();
        let evidence = changed.audio_decoder_evidence_v1.as_mut().unwrap();
        match mutation {
            0 => evidence.sources.clear(),
            1 => evidence.sources.push(evidence.sources[0].clone()),
            2 => evidence.sources[0].resource.content_hash = "f".repeat(64),
            3 => evidence.sources[0].resource.size += 1,
            4 => evidence.sources[0].resource.kind = "video".into(),
            5 => evidence.sources[0].decoder.primary.as_mut().unwrap().codec = "aac".into(),
            6 => {
                evidence.sources[0]
                    .decoder
                    .primary
                    .as_mut()
                    .unwrap()
                    .sample_rate = 44100
            }
            7 => evidence.sources[0].decoder.profile = "web-audio-decode-at-target-rate".into(),
            8 => {
                changed.public.execution.decoder_profiles =
                    vec![AudioDecoderProfile::WebAudioDecodeAtTargetRate]
            }
            9 => evidence.sources[0].decoder.frame_count = "9".into(),
            _ => unreachable!(),
        }
        assert!(
            validate_stored_result(&input, &changed).is_err(),
            "mutation {mutation}"
        );
    }
    let transcription_input = crate::scene_transcription::PreparedSceneTranscription {
        audio: input.clone(),
        language: Some("en".into()),
    };
    let transcript = opencut_job_contracts::scene_transcription::SceneTranscriptionResult {
        schema_version: 1, project_id: input.snapshot.project.id.clone(), revision: input.snapshot.revision.clone(), sequence_id: input.sequence_id.clone(),
        duration: time(prepared.duration_samples() as i64, input.sample_rate),
        audio: stored.public.clone(),
        transcription: serde_json::from_value(serde_json::json!({"language":"en","provider":"whisper.cpp","model":"test","segments":[]})).unwrap(),
    };
    let private = crate::scene_transcription::StoredSceneTranscription {
        public: transcript.clone(),
        audio_decoder_evidence_v1: stored.audio_decoder_evidence_v1.clone().unwrap(),
    };
    let transcript_value = serde_json::to_value(&transcript).unwrap();
    assert!(
        crate::scene_transcription::validate_stored_result(
            &transcription_input,
            transcript_value.clone()
        )
        .is_err()
    );
    let private_value = serde_json::to_value(private).unwrap();
    assert_eq!(
        crate::scene_transcription::validate_stored_result(
            &transcription_input,
            private_value.clone()
        )
        .unwrap(),
        transcript
    );
    let mut transcription_record = record.clone();
    transcription_record.kind = "analysis.sequence_transcribe.v4".into();
    transcription_record.input = serde_json::to_value(&transcription_input).unwrap();
    transcription_record.result = Some(private_value);
    transcription_record.state = JobState::Succeeded;
    transcription_record.attempt = 1;
    let projected = crate::job_views::project(transcription_record);
    let Some(opencut_api_contracts::job_results::JobResultView::SceneTranscription(public)) =
        projected.result
    else {
        panic!("Expected typed scene transcription")
    };
    assert_eq!(
        serde_json::to_value(public).unwrap(),
        serde_json::to_value(
            opencut_api_contracts::job_results::SceneTranscriptionResult::from(transcript.clone())
        )
        .unwrap()
    );
    let mut old_transcript = transcript;
    old_transcript.audio = result(&input).public;
    assert!(
        crate::scene_transcription::validate_stored_result(
            &transcription_input,
            serde_json::to_value(old_transcript).unwrap()
        )
        .is_ok()
    );
    record.state = JobState::Succeeded;
    record.attempt = 1;
    record.result = Some(serde_json::to_value(&stored).unwrap());
    let view = crate::job_views::project(record);
    let Some(opencut_api_contracts::job_results::JobResultView::Audio(public)) = view.result else {
        panic!("Expected typed audio result")
    };
    assert_eq!(
        serde_json::to_value(public).unwrap(),
        serde_json::to_value(opencut_api_contracts::job_results::AudioRenderResult::from(
            stored.public.clone()
        ))
        .unwrap()
    );
}
