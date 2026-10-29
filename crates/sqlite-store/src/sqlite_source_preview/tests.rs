//! Atomic queue/projection/access checks only; these bytes are not decoded.
use super::*;
use opencut_contracts::{
    Asset, AssetKind, CreateProject, FrameRate, Time, renderer_identity::*, scene::*, scene_ops::*,
};
use opencut_editor_application::access::{Caller, PrincipalPolicy, ProjectGrant};
use std::collections::BTreeMap;
struct Fixture {
    app: EditorApplication,
    _directory: tempfile::TempDir,
    revision: String,
    runtime: SourcePreviewRuntimeIdentity,
    asset: ProjectAsset,
}
fn runtime() -> SourcePreviewRuntimeIdentity {
    SourcePreviewRuntimeIdentity {
        host_sha256: "a".repeat(64),
        decoder: SourcePreviewDecoderIdentity {
            worker_sha256: "b".repeat(64),
            decoder_libraries: ["avcodec", "avformat", "avutil", "swscale", "swresample"]
                .into_iter()
                .map(|name| NativeLibraryIdentity {
                    name: name.into(),
                    version: "test".into(),
                    configuration_sha256: "c".repeat(64),
                    binary_sha256: "d".repeat(64),
                })
                .collect(),
            platform: NativeSystemIdentity {
                os: "test".into(),
                architecture: "test".into(),
                kernel_release: "test".into(),
                system_build: "test".into(),
            },
        },
        ffmpeg_sha256: "e".repeat(64),
        ffprobe_sha256: "f".repeat(64),
    }
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut app = EditorApplication::open(directory.path().join("workspace.sqlite")).unwrap();
        let created = app
            .create_scene_project(&CreateProjectRequest {
                project: CreateProject {
                    id: "source-project".into(),
                    name: "Source queue".into(),
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
        let path = directory.path().join("queue-only-source");
        std::fs::write(&path, b"queue fixture without media decoding").unwrap();
        let original = Asset {
            id: "video".into(),
            name: "Video".into(),
            kind: AssetKind::Video,
            uri: path.to_string_lossy().into(),
            duration: Some(Time {
                ticks: "1".into(),
                timebase: 1,
            }),
            version: None,
            metadata: BTreeMap::new(),
        };
        let pinned = opencut_asset_store::AssetStore::open(app.asset_store_root())
            .unwrap()
            .pin_asset(&original)
            .unwrap();
        let asset = ProjectAsset {
            id: "video".into(),
            name: "Video".into(),
            kind: AssetKind::Video,
            version: pinned.version.unwrap(),
            duration: original.duration,
            origin: ResourceOrigin::LocalImport {
                original_name: "queue-only-source".into(),
            },
            metadata: ProjectMediaMetadata {
                mime_type: None,
                intrinsic_size: None,
                frame_rate: None,
                rotation_degrees: None,
                audio: AudioPresence::Absent {},
            },
            license: None,
        };
        app.apply_scene_project(&ApplySceneProject {
            project_id: "source-project".into(),
            expected_revision: created.revision,
            idempotency_key: "asset".into(),
            label: "Bind source".into(),
            operations: vec![ProjectOperation::AssetPut {
                asset: asset.clone(),
            }],
        })
        .unwrap();
        let revision = app
            .inspect_scene_project(&InspectProject {
                project_id: "source-project".into(),
                revision: None,
            })
            .unwrap()
            .revision;
        Self {
            app,
            _directory: directory,
            revision,
            runtime: runtime(),
            asset,
        }
    }
    fn prepare(&mut self) -> JobRecord {
        let input = SourcePreviewPrepare {
            project_id: "source-project".into(),
            revision: self.revision.clone(),
            sequence_id: "scene".into(),
            asset_id: Some("video".into()),
            selected_video_stream: Some(1),
        };
        prepare(
            &mut self.app,
            input,
            &source::runtime_fingerprint(&self.runtime).unwrap(),
        )
        .unwrap()
        .remove(0)
    }
    fn credential(&mut self) -> Caller {
        self.app
            .set_principal_policy(PrincipalPolicy {
                principal_id: "preview-agent".into(),
                expected_revision: "0".into(),
                enabled: true,
                grants: vec![ProjectGrant {
                    project_id: "source-project".into(),
                    actions: vec![
                        ProjectAction::Read,
                        ProjectAction::RunJobs,
                        ProjectAction::ReadArtifacts,
                    ],
                }],
            })
            .unwrap();
        let issued = self.app.issue_credential("preview-agent").unwrap();
        self.app.authenticate(&issued.secret).unwrap()
    }
    fn claim(&mut self) -> JobRecord {
        self.app.set_caller(Caller::LocalOwner);
        self.app
            .live_jobs()
            .claim("preview-worker", 30_000)
            .unwrap()
            .unwrap()
    }
    fn stored(&self, job: &JobRecord) -> StoredSourcePreviewResult {
        let prepared: PreparedSourcePreview = serde_json::from_value(job.input.clone()).unwrap();
        let artifact = ArtifactDescriptor {
            path: "/private/source-preview.mkv".into(),
            mime_type: "video/x-matroska".into(),
            size: 100,
            content_hash: "a".repeat(64),
        };
        let index = ArtifactDescriptor {
            path: "/private/source-preview.idx".into(),
            mime_type: "application/vnd.opencut.source-preview-index".into(),
            size: 208,
            content_hash: "b".repeat(64),
        };
        StoredSourcePreviewResult {
            public: SourcePreviewResult {
                identity: prepared.identity,
                producer_revision: prepared.snapshot.revision,
                actual_stream_index: 1,
                actual_track_id: 7,
                source_time_base: SourcePreviewTimeBase {
                    numerator: 1,
                    denominator: 1000,
                },
                proxy_time_base: SourcePreviewTimeBase {
                    numerator: 1,
                    denominator: 1000,
                },
                encoding: SourcePreviewEncoding::Ffv1Rgba,
                frame_count: "3".into(),
                first_source_pts: "0".into(),
                last_source_pts: "200".into(),
                last_source_duration: "50".into(),
                video: SourcePreviewArtifact {
                    content_hash: artifact.content_hash.clone(),
                    size_bytes: "100".into(),
                    mime_type: artifact.mime_type.clone(),
                },
                frame_index: SourcePreviewArtifact {
                    content_hash: index.content_hash.clone(),
                    size_bytes: "208".into(),
                    mime_type: index.mime_type.clone(),
                },
                runtime: self.runtime.clone(),
            },
            artifact,
            frame_index: index,
        }
    }
}
#[test]
fn same_source_reuses_original_producer_across_edits_and_partitions_callers() {
    let mut fixture = Fixture::new();
    let original = fixture.prepare();
    assert_eq!(fixture.prepare().job_id, original.job_id);
    fixture.asset.name = "Rename only".into();
    fixture
        .app
        .apply_scene_project(&ApplySceneProject {
            project_id: "source-project".into(),
            expected_revision: fixture.revision.clone(),
            idempotency_key: "rename".into(),
            label: "Rename".into(),
            operations: vec![ProjectOperation::AssetPut {
                asset: fixture.asset.clone(),
            }],
        })
        .unwrap();
    fixture.revision = fixture
        .app
        .inspect_scene_project(&InspectProject {
            project_id: "source-project".into(),
            revision: None,
        })
        .unwrap()
        .revision;
    assert_ne!(
        fixture.revision,
        original.input["snapshot"]["revision"].as_str().unwrap()
    );
    assert_eq!(fixture.prepare().job_id, original.job_id);
    let caller = fixture.credential();
    fixture.app.set_caller(caller);
    assert_ne!(fixture.prepare().job_id, original.job_id);
}
#[test]
fn source_completion_projects_both_artifacts_and_fences_mutable_publication() {
    let mut fixture = Fixture::new();
    fixture.prepare();
    let running = fixture.claim();
    let stored = fixture.stored(&running);
    fixture
        .app
        .live_jobs()
        .complete(
            &running.job_id,
            "preview-worker",
            running.attempt,
            serde_json::to_value(&stored).unwrap(),
        )
        .unwrap();
    let view = fixture.app.job_view(&running.job_id).unwrap();
    assert_eq!(view.artifacts.len(), 2);
    assert!(view.result.is_some());
    for reference in &view.artifacts {
        let record = fixture.app.get_job(&running.job_id).unwrap();
        let path = opencut_editor_application::job_views::private_artifact_path(&record, reference)
            .unwrap();
        if matches!(
            reference.role,
            opencut_api_contracts::job_views::ArtifactRole::FrameIndex
        ) {
            assert!(path.ends_with(".idx"));
        } else {
            assert!(path.ends_with(".mkv"));
        }
    }
    let mut captured = fixture
        .app
        .capture_source_preview_job(&running.job_id)
        .unwrap();
    assert_eq!(
        fixture
            .app
            .source_preview_job_status(&mut captured)
            .unwrap()
            .0,
        JobState::Succeeded
    );
    fixture.app.require_source_preview_read(&captured).unwrap();
    fixture
        .app
        .connection
        .execute(
            "UPDATE jobs SET result_json='{}' WHERE job_id=?1",
            [&running.job_id],
        )
        .unwrap();
    assert!(fixture.app.require_source_preview_read(&captured).is_err());
    assert!(
        fixture
            .app
            .source_preview_job_status(&mut captured)
            .is_err()
    );
}
#[test]
fn revocation_blocks_source_checkpoints_and_completion_in_the_original_lease() {
    let mut fixture = Fixture::new();
    let caller = fixture.credential();
    fixture.app.set_caller(caller);
    fixture.prepare();
    let running = fixture.claim();
    let input: PreparedSourcePreview = serde_json::from_value(running.input.clone()).unwrap();
    fixture
        .app
        .revalidate_source_preview_job(
            &running.job_id,
            "preview-worker",
            running.attempt,
            &input.authority,
        )
        .unwrap();
    fixture
        .app
        .connection
        .execute(
            "UPDATE access_grants SET action_mask=0 WHERE principal_id='preview-agent'",
            [],
        )
        .unwrap();
    assert_eq!(
        fixture
            .app
            .revalidate_source_preview_job(
                &running.job_id,
                "preview-worker",
                running.attempt,
                &input.authority
            )
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
    let stored = fixture.stored(&running);
    assert_eq!(
        fixture
            .app
            .live_jobs()
            .complete(
                &running.job_id,
                "preview-worker",
                running.attempt,
                serde_json::to_value(stored).unwrap()
            )
            .unwrap_err()
            .code,
        "NOT_AUTHORIZED"
    );
    assert_eq!(
        fixture.app.get_job(&running.job_id).unwrap().state,
        JobState::Running
    );
}

#[test]
fn native_preview_keeps_original_execution_scope_while_download_requires_artifact_access() {
    let mut fixture = Fixture::new();
    let caller = fixture.credential();
    let mask = ProjectAction::Read.mask() | ProjectAction::RunJobs.mask();
    fixture
        .app
        .connection
        .execute(
            "UPDATE access_grants SET action_mask=?1 WHERE principal_id='preview-agent'",
            [mask],
        )
        .unwrap();
    fixture.app.set_caller(caller.clone());
    fixture.prepare();
    let running = fixture.claim();
    let stored = fixture.stored(&running);
    fixture
        .app
        .live_jobs()
        .complete(
            &running.job_id,
            "preview-worker",
            running.attempt,
            serde_json::to_value(stored).unwrap(),
        )
        .unwrap();
    fixture.app.set_caller(caller);
    let mut captured = fixture
        .app
        .capture_source_preview_job(&running.job_id)
        .unwrap();
    fixture
        .app
        .source_preview_job_status(&mut captured)
        .unwrap();
    fixture.app.require_source_preview_read(&captured).unwrap();
    assert_eq!(
        fixture.app.artifact_job(&running.job_id).unwrap_err().code,
        "NOT_AUTHORIZED"
    );
}
