//! Queue/publication fixtures use real tiny CAS bytes, not DSP output acceptance.
use super::*;
use opencut_contracts::{
    Asset, AssetKind, CreateProject, FrameRate, Time, TimeRange, scene::*, scene_ops::*,
};
use opencut_editor_application::access::{Caller, PrincipalPolicy, ProjectGrant};
use opencut_job_contracts::scene_audio::*;
use std::collections::BTreeMap;

fn time(ticks: i64, timebase: u32) -> Time {
    Time {
        ticks: ticks.to_string(),
        timebase,
    }
}
fn now() -> i64 {
    lifecycle::JobClock::now_ms(&sqlite_jobs::SystemClock).unwrap()
}
struct Fixture {
    app: EditorApplication,
    directory: tempfile::TempDir,
    fixed: EditorSnapshot,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut app = EditorApplication::open(directory.path().join("workspace.sqlite")).unwrap();
        let initial = app
            .create_scene_project(&CreateProjectRequest {
                project: CreateProject {
                    id: "audio-project".into(),
                    name: "Queued audio".into(),
                    sequence_id: "scene".into(),
                    width: 64,
                    height: 48,
                    fps: FrameRate {
                        numerator: 30,
                        denominator: 1,
                    },
                },
                idempotency_key: "create".into(),
            })
            .unwrap();
        // Valid tiny mono PCM16 WAV. Timeline zero-tail behavior is not rendered
        // by this fixture: it verifies immutable byte capture and SQL fences.
        let source = directory.path().join("tiny.wav");
        let mut bytes = b"RIFF".to_vec();
        bytes.extend(68u32.to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(48000u32.to_le_bytes());
        bytes.extend(96000u32.to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(32u32.to_le_bytes());
        bytes.extend([0; 32]);
        std::fs::write(&source, &bytes).unwrap();
        let observed = Asset {
            id: "audio-asset".into(),
            name: "Tiny WAV".into(),
            kind: AssetKind::Audio,
            uri: source.to_string_lossy().into(),
            duration: None,
            version: None,
            metadata: BTreeMap::new(),
        };
        let pinned = opencut_asset_store::AssetStore::open(app.asset_store_root())
            .unwrap()
            .pin_asset(&observed)
            .unwrap();
        let asset = ProjectAsset {
            id: observed.id,
            name: observed.name,
            kind: AssetKind::Audio,
            version: pinned.version.unwrap(),
            duration: None,
            origin: ResourceOrigin::LegacyLocator {
                uri: source.to_string_lossy().into(),
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
        let track = SceneTrack {
            id: "sound".into(),
            name: "Sound".into(),
            kind: SceneTrackKind::Audio,
            enabled: true,
            muted: false,
            locked: false,
            opacity: 1.0,
            gain: Gain::Decibels { value: 0.0 },
            channels: vec![],
            elements: vec![TimelineElement {
                id: "voice".into(),
                name: "Voice".into(),
                enabled: true,
                timeline_range: TimeRange {
                    start: time(0, 1),
                    duration: time(1, 10),
                },
                source_span: ElementSourceSpan::ClassicTrimmed {
                    trim_start: time(0, 1),
                    trim_end: time(0, 1),
                    source_duration: None,
                },
                content: ElementContent::Audio {
                    asset_id: asset.id.clone(),
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
            }],
        };
        app.apply_scene_project(&ApplySceneProject {
            project_id: "audio-project".into(),
            expected_revision: initial.revision,
            idempotency_key: "bind-audio".into(),
            label: "Bind exact CAS audio".into(),
            operations: vec![
                ProjectOperation::AssetPut { asset },
                ProjectOperation::TrackPut {
                    sequence_id: "scene".into(),
                    track,
                },
            ],
        })
        .unwrap();
        let fixed = app
            .inspect_scene_project(&InspectProject {
                project_id: "audio-project".into(),
                revision: None,
            })
            .unwrap();
        Self {
            app,
            directory,
            fixed,
        }
    }
    fn path(&self) -> std::path::PathBuf {
        self.directory.path().join("workspace.sqlite")
    }
    fn request(&self, key: &str) -> SubmitAudioWav {
        SubmitAudioWav {
            idempotency_key: key.into(),
            project_id: self.fixed.project.id.clone(),
            revision: self.fixed.revision.clone(),
            sequence_id: "scene".into(),
            sample_rate: 48000,
        }
    }
    fn credential(&mut self) -> (Caller, String) {
        self.set_policy("0", vec![ProjectAction::Read, ProjectAction::RunJobs]);
        let issued = self.app.issue_credential("audio-agent").unwrap();
        (
            self.app.authenticate(&issued.secret).unwrap(),
            issued.credential_id,
        )
    }
    fn set_policy(&mut self, revision: &str, actions: Vec<ProjectAction>) {
        self.app
            .set_principal_policy(PrincipalPolicy {
                principal_id: "audio-agent".into(),
                expected_revision: revision.into(),
                enabled: true,
                grants: if actions.is_empty() {
                    vec![]
                } else {
                    vec![ProjectGrant {
                        project_id: "audio-project".into(),
                        actions,
                    }]
                },
            })
            .unwrap();
    }
    fn submit(&mut self, key: &str) -> JobRecord {
        let request = self.request(key);
        start_audio_wav(&mut self.app, request).unwrap()
    }
    fn claim(&mut self, job: &JobRecord) -> JobRecord {
        self.app.set_caller(Caller::LocalOwner);
        let running = self
            .app
            .claim_job("audio-worker", now(), 30000)
            .unwrap()
            .unwrap();
        assert_eq!(running.job_id, job.job_id);
        running
    }
}
fn prepared(job: &JobRecord) -> PreparedAudioWav {
    serde_json::from_value(job.input.clone()).unwrap()
}
fn counts(app: &EditorApplication) -> Vec<i64> {
    [
        "jobs",
        "job_projects",
        "idempotency",
        "job_events",
        "outbox",
        "audit_records",
    ]
    .into_iter()
    .map(|table| {
        app.connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
    .collect()
}
fn receipt(app: &EditorApplication, key: &str) -> String {
    app.connection.query_row("SELECT result_json FROM idempotency WHERE operation_family='create_job' AND scope='workspace' AND idempotency_key=?1", [key], |r| r.get(0)).unwrap()
}
/// Deliberately metadata-only success for testing the SQL publication boundary.
/// A real Worker must produce and verify PCM/WAV before constructing this result.
fn legal_result(job: &JobRecord) -> Value {
    let input = prepared(job);
    let pcm = audio::validate_prepared(&input).unwrap();
    let size = 58 + pcm.duration_samples() * 8;
    let hash = "a".repeat(64);
    let result = audio::StoredAudioWavResult {
        audio_decoder_evidence_v1: None,
        public: AudioWavResult {
            schema_version: 1,
            identity: AudioWavIdentity {
                project_id: input.snapshot.project.id.clone(),
                revision: input.snapshot.revision.clone(),
                sequence_id: input.sequence_id.clone(),
                profile: AudioProfile::ClassicExportV31WebAudio,
                sample_rate: input.sample_rate,
                channels: 2,
                frame_count: pcm.duration_samples().to_string(),
                sample_format: AudioSampleFormat::IeeeFloat32Le,
            },
            asset_lock: input.asset_lock.clone(),
            render_input_sha256: audio::render_input_sha256(
                &input.snapshot,
                &input.sequence_id,
                input.sample_rate,
            )
            .unwrap(),
            execution: AudioExecutionSummary {
                host_sha256: "b".repeat(64),
                runtime:
                    opencut_job_contracts::scene_audio::AudioRuntimeIdentity::HistoricalBrowser(
                        opencut_job_contracts::scene_audio::HistoricalBrowserAudioIdentity {
                            browser_version: "fixture-browser".into(),
                            browser_sha256: "c".repeat(64),
                            module_sha256: "d".repeat(64),
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
                size_bytes: size.to_string(),
                content_hash: hash.clone(),
            },
        },
        artifact: opencut_job_contracts::ArtifactDescriptor {
            path: format!(
                "/private/fixture/{}/attempt-{}/render.wav",
                job.job_id, job.attempt
            ),
            mime_type: "audio/wav".into(),
            size,
            content_hash: hash,
        },
    };
    audio::validate_stored_result(&input, &result).unwrap();
    serde_json::to_value(result).unwrap()
}

#[test]
fn historical_revision_cas_scope_and_original_authority_survive_idempotent_reopen() {
    let mut fixture = Fixture::new();
    let old = fixture.fixed.clone();
    fixture
        .app
        .apply_scene_project(&ApplySceneProject {
            project_id: old.project.id.clone(),
            expected_revision: old.revision.clone(),
            idempotency_key: "new-head".into(),
            label: "Advance head".into(),
            operations: vec![ProjectOperation::Rename {
                name: "Later head".into(),
            }],
        })
        .unwrap();
    let (caller, credential_id) = fixture.credential();
    fixture.app.set_caller(caller.clone());
    let job = fixture.submit("fixed-history");
    let input = prepared(&job);
    assert_eq!(input.snapshot, old);
    assert_eq!(input.processing_policy, policy());
    assert_eq!(
        input.authority,
        TrustedJobAuthority::Credential {
            principal_id: "audio-agent".into(),
            credential_id
        }
    );
    assert_eq!(input.asset_lock.assets.len(), 1);
    let locked = &input.asset_lock.assets[0];
    let object = fixture
        .app
        .asset_store_root()
        .join(&locked.content_hash[..2])
        .join(&locked.content_hash);
    let bytes = std::fs::read(&object).unwrap();
    assert_eq!(bytes.len() as u64, locked.size);
    assert_eq!(&bytes[..4], b"RIFF");
    let binding: (String, String) = fixture
        .app
        .connection
        .query_row(
            "SELECT project_id,revision FROM job_projects WHERE job_id=?1",
            [&job.job_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(binding, (old.project.id, old.revision));
    let mut reopened = EditorApplication::open(fixture.path()).unwrap();
    reopened.set_caller(caller);
    assert_eq!(
        start_audio_wav(&mut reopened, fixture.request("fixed-history")).unwrap(),
        job
    );
    assert_eq!(
        prepared(&reopened.get_job(&job.job_id).unwrap()).authority,
        input.authority
    );
    assert_eq!(
        reopened
            .job_view(&job.job_id)
            .unwrap()
            .target
            .unwrap()
            .revision,
        input.snapshot.revision
    );
    // A matching replay does not recapture bytes. A new task cannot fall back
    // to the still-present legacy source when its immutable CAS object is gone.
    std::fs::remove_file(object).unwrap();
    assert_eq!(
        start_audio_wav(&mut reopened, fixture.request("fixed-history")).unwrap(),
        job
    );
    let before = counts(&reopened);
    assert_eq!(
        start_audio_wav(&mut reopened, fixture.request("missing-cas"))
            .unwrap_err()
            .code,
        "ASSET_IO_ERROR"
    );
    assert_eq!(counts(&reopened), before);
}

#[test]
fn original_credential_revocation_or_missing_read_run_grant_blocks_owner_worker_and_completion() {
    for missing in ["credential", "read", "run"] {
        let mut fixture = Fixture::new();
        let (caller, credential_id) = fixture.credential();
        fixture.app.set_caller(caller);
        let job = fixture.submit("grant-fence");
        let running = fixture.claim(&job);
        let authority = prepared(&running).authority;
        fixture
            .app
            .revalidate_scene_audio_job(&job.job_id, "audio-worker", running.attempt, &authority)
            .unwrap();
        assert_eq!(
            fixture
                .app
                .revalidate_scene_audio_job(
                    &job.job_id,
                    "audio-worker",
                    running.attempt,
                    &TrustedJobAuthority::LocalOwner
                )
                .unwrap_err()
                .code,
            "NOT_AUTHORIZED"
        );
        match missing {
            "credential" => fixture.app.revoke_credential(&credential_id).unwrap(),
            "read" => fixture.set_policy("1", vec![]),
            _ => fixture.set_policy("1", vec![ProjectAction::Read]),
        }
        let before = counts(&fixture.app);
        let original_receipt = receipt(&fixture.app, "grant-fence");
        assert_eq!(
            fixture
                .app
                .revalidate_scene_audio_job(
                    &job.job_id,
                    "audio-worker",
                    running.attempt,
                    &authority
                )
                .unwrap_err()
                .code,
            "NOT_AUTHORIZED"
        );
        assert_eq!(
            fixture
                .app
                .complete_job(
                    &job.job_id,
                    "audio-worker",
                    running.attempt,
                    now(),
                    legal_result(&running)
                )
                .unwrap_err()
                .code,
            "NOT_AUTHORIZED"
        );
        assert_eq!(fixture.app.get_job(&job.job_id).unwrap(), running);
        assert_eq!(counts(&fixture.app), before);
        assert_eq!(receipt(&fixture.app, "grant-fence"), original_receipt);
    }
}

#[test]
fn committed_cancellation_wins_over_valid_invalid_completion_and_late_failure_without_a_result() {
    for outcome in ["valid", "invalid", "late-fail"] {
        let mut fixture = Fixture::new();
        let job = fixture.submit("cancel-fence");
        let running = fixture.claim(&job);
        fixture.app.cancel_job(&job.job_id).unwrap();
        let authority = prepared(&running).authority;
        assert_eq!(
            fixture
                .app
                .revalidate_scene_audio_job(
                    &job.job_id,
                    "audio-worker",
                    running.attempt,
                    &authority
                )
                .unwrap_err()
                .code,
            "JOB_CANCELLED"
        );
        let done = if outcome == "late-fail" {
            fixture
                .app
                .fail_job(
                    &job.job_id,
                    "audio-worker",
                    running.attempt,
                    now(),
                    json!({"code":"LATE_ERROR"}),
                )
                .unwrap()
        } else {
            let value = if outcome == "invalid" {
                json!({"not":"a legal result"})
            } else {
                legal_result(&running)
            };
            fixture
                .app
                .complete_job(&job.job_id, "audio-worker", running.attempt, now(), value)
                .unwrap()
        };
        assert_eq!(done.state, JobState::Cancelled);
        assert!(done.result.is_none() && done.error.is_none());
        let view = fixture.app.job_view(&job.job_id).unwrap();
        assert!(view.result.is_none() && view.artifacts.is_empty());
        let reopened = EditorApplication::open(fixture.path()).unwrap();
        assert_eq!(reopened.get_job(&job.job_id).unwrap(), done);
    }
}

#[test]
fn audit_failure_rolls_back_queue_acceptance_and_complete_with_original_receipt_unchanged() {
    let mut fixture = Fixture::new();
    fixture.app.connection.execute_batch("CREATE TRIGGER reject_audio_audit BEFORE INSERT ON audit_records WHEN NEW.action='job.accepted' BEGIN SELECT RAISE(ABORT,'injected audit failure'); END;").unwrap();
    let before = counts(&fixture.app);
    let request = fixture.request("audit-fence");
    assert_eq!(
        start_audio_wav(&mut fixture.app, request.clone())
            .unwrap_err()
            .code,
        "STORAGE_ERROR"
    );
    assert_eq!(counts(&fixture.app), before);
    let intent = JobRequest {
        idempotency_key: request.idempotency_key.clone(),
        kind: AUDIO_WAV_KIND.into(),
        input: serde_json::to_value(&request).unwrap(),
    };
    assert!(fixture.app.find_existing_job(&intent).unwrap().is_none());
    fixture
        .app
        .connection
        .execute_batch("DROP TRIGGER reject_audio_audit")
        .unwrap();
    let job = fixture.submit("audit-fence");
    let running = fixture.claim(&job);
    let original_receipt = receipt(&fixture.app, "audit-fence");
    let before = counts(&fixture.app);
    fixture.app.connection.execute_batch("CREATE TRIGGER reject_audio_audit BEFORE INSERT ON audit_records WHEN NEW.action='job.succeeded' BEGIN SELECT RAISE(ABORT,'injected audit failure'); END;").unwrap();
    assert_eq!(
        fixture
            .app
            .complete_job(
                &job.job_id,
                "audio-worker",
                running.attempt,
                now(),
                legal_result(&running)
            )
            .unwrap_err()
            .code,
        "STORAGE_ERROR"
    );
    let reopened = EditorApplication::open(fixture.path()).unwrap();
    assert_eq!(reopened.get_job(&job.job_id).unwrap(), running);
    assert_eq!(counts(&reopened), before);
    assert_eq!(receipt(&reopened, "audit-fence"), original_receipt);
    drop(reopened);
    fixture
        .app
        .connection
        .execute_batch("DROP TRIGGER reject_audio_audit")
        .unwrap();
    let done = fixture
        .app
        .complete_job(
            &job.job_id,
            "audio-worker",
            running.attempt,
            now(),
            legal_result(&running),
        )
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(receipt(&fixture.app, "audit-fence"), original_receipt);
    assert_eq!(
        fixture.app.job_view(&job.job_id).unwrap().artifacts.len(),
        1
    );
}

fn range_request(f: &Fixture, key: &str) -> SubmitAudioRangeWav {
    let whole = f.request(key);
    SubmitAudioRangeWav {
        idempotency_key: whole.idempotency_key,
        project_id: whole.project_id,
        revision: whole.revision,
        sequence_id: whole.sequence_id,
        sample_rate: whole.sample_rate,
        range: AudioSampleRange {
            start_sample: "17".into(),
            frame_count: "120".into(),
        },
    }
}
// Metadata-only SQL publication evidence, independent of actual decoder tests.
fn legal_range_result(job: &JobRecord) -> Value {
    let input = prepared(job);
    let range = input.output_range.as_ref().unwrap();
    let mut whole = job.clone();
    let mut base = input.clone();
    base.output_range = None;
    whole.input = serde_json::to_value(base).unwrap();
    let mut value = legal_result(&whole);
    let frames: u64 = range.frame_count.parse().unwrap();
    let whole_frames = value["public"]["identity"]["frame_count"].clone();
    value["public"]["identity"]["whole_scene_frame_count"] = whole_frames;
    value["public"]["identity"]["start_sample"] = json!(range.start_sample);
    value["public"]["identity"]["frame_count"] = json!(range.frame_count);
    value["public"]["artifact"]["size_bytes"] = json!((58 + 8 * frames).to_string());
    value["artifact"]["size"] = json!(58 + 8 * frames);
    value["public"]["render_input_sha256"] = json!(
        audio::render_range_input_sha256(
            &input.snapshot,
            &input.sequence_id,
            input.sample_rate,
            &AudioSampleRange {
                start_sample: range.start_sample.clone(),
                frame_count: range.frame_count.clone()
            },
        )
        .unwrap()
    );
    value
}

#[test]
fn range_job_is_atomic_replayable_and_publishes_only_exact_selected_identity() {
    let mut f = Fixture::new();
    let (caller, _) = f.credential();
    f.app.set_caller(caller.clone());
    let request = range_request(&f, "range");
    f.app.connection.execute_batch("CREATE TRIGGER reject_range_audit BEFORE INSERT ON audit_records WHEN NEW.action='job.accepted' BEGIN SELECT RAISE(ABORT,'range audit fault'); END;").unwrap();
    let before = counts(&f.app);
    assert_eq!(
        start_audio_range_wav(&mut f.app, request.clone())
            .unwrap_err()
            .code,
        "STORAGE_ERROR"
    );
    assert_eq!(counts(&f.app), before);
    f.app
        .connection
        .execute_batch("DROP TRIGGER reject_range_audit")
        .unwrap();
    let job = start_audio_range_wav(&mut f.app, request.clone()).unwrap();
    assert_eq!(job.kind, AUDIO_RANGE_WAV_KIND);
    let input = prepared(&job);
    assert_eq!(input.output_range.as_ref().unwrap().start_sample, "17");
    assert_eq!(
        start_audio_range_wav(&mut f.app, request.clone()).unwrap(),
        job
    );
    let mut changed = request;
    changed.range.start_sample = "18".into();
    assert!(start_audio_range_wav(&mut f.app, changed).is_err());
    let running = f.claim(&job);
    f.app
        .revalidate_scene_audio_job(
            &job.job_id,
            "audio-worker",
            running.attempt,
            &input.authority,
        )
        .unwrap();
    let value = legal_range_result(&running);
    let mut wrong = value.clone();
    wrong["public"]["identity"]["start_sample"] = json!("18");
    assert_eq!(
        f.app
            .complete_job(&job.job_id, "audio-worker", running.attempt, now(), wrong)
            .unwrap_err()
            .code,
        "INVALID_STORED_RESULT"
    );
    assert_eq!(f.app.get_job(&job.job_id).unwrap().state, JobState::Running);
    let done = f
        .app
        .complete_job(&job.job_id, "audio-worker", running.attempt, now(), value)
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    let mut reopened = EditorApplication::open(f.path()).unwrap();
    reopened.set_caller(caller);
    let view = reopened.job_view(&job.job_id).unwrap();
    let Some(opencut_api_contracts::job_results::JobResultView::Audio(public)) = view.result else {
        panic!("Expected typed audio result")
    };
    assert_eq!(public.identity.start_sample, "17");
    assert_eq!(public.identity.frame_count, "120");
    assert_eq!(view.artifacts.len(), 1);
    assert!(
        !serde_json::to_string(&public)
            .unwrap()
            .contains("/private/fixture")
    );
}

#[test]
fn range_job_original_caller_revocation_and_cancel_win_publication() {
    let mut f = Fixture::new();
    let (caller, _) = f.credential();
    f.app.set_caller(caller);
    let request = range_request(&f, "range-cancel");
    let job = start_audio_range_wav(&mut f.app, request).unwrap();
    let running = f.claim(&job);
    let input = prepared(&job);
    f.set_policy("1", vec![ProjectAction::Read]);
    assert_eq!(
        f.app
            .revalidate_scene_audio_job(
                &job.job_id,
                "audio-worker",
                running.attempt,
                &input.authority
            )
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
    assert_eq!(
        f.app
            .complete_job(
                &job.job_id,
                "audio-worker",
                running.attempt,
                now(),
                legal_range_result(&running)
            )
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
    f.set_policy("2", vec![ProjectAction::Read, ProjectAction::RunJobs]);
    f.app.cancel_job(&job.job_id).unwrap();
    let cancelled = f
        .app
        .complete_job(
            &job.job_id,
            "audio-worker",
            running.attempt,
            now(),
            legal_range_result(&running),
        )
        .unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert!(cancelled.result.is_none());
    let view = f.app.job_view(&job.job_id).unwrap();
    assert!(view.artifacts.is_empty() && view.result.is_none());
}

#[test]
fn ephemeral_audio_preview_uses_fixed_revision_without_persistent_jobs_or_edits() {
    use opencut_editor_application::audio_preview::AudioPreviewRead;
    let mut fixture = Fixture::new();
    let (caller, _) = fixture.credential();
    fixture.app.set_caller(caller);
    let before = counts(&fixture.app);
    let selected = fixture
        .app
        .fixed_audio_preview_snapshot("audio-project", &fixture.fixed.revision)
        .unwrap();
    assert_eq!(selected.snapshot.revision, fixture.fixed.revision);
    fixture
        .app
        .revalidate_audio_preview(
            "audio-project",
            &fixture.fixed.revision,
            &selected.authority,
        )
        .unwrap();
    assert_eq!(counts(&fixture.app), before);
    assert!(
        fixture
            .app
            .fixed_audio_preview_snapshot("audio-project", "missing-revision")
            .is_err()
    );
    assert_eq!(counts(&fixture.app), before);
}

#[test]
fn ephemeral_audio_preview_rechecks_original_credential_even_in_owner_host() {
    use opencut_editor_application::audio_preview::AudioPreviewRead;
    let mut fixture = Fixture::new();
    let (caller, credential_id) = fixture.credential();
    fixture.app.set_caller(caller);
    let selected = fixture
        .app
        .fixed_audio_preview_snapshot("audio-project", &fixture.fixed.revision)
        .unwrap();
    fixture.app.set_caller(Caller::LocalOwner);
    fixture.app.revoke_credential(&credential_id).unwrap();
    assert!(
        fixture
            .app
            .revalidate_audio_preview(
                "audio-project",
                &fixture.fixed.revision,
                &selected.authority
            )
            .is_err()
    );
    // The surrounding host remains authorized; it cannot substitute itself for
    // the session's captured principal and credential.
    fixture
        .app
        .revalidate_scene_audio_preview("audio-project", &fixture.fixed.revision)
        .unwrap();
}
