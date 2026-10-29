//! Configurable export uses the existing fixed revision capture and durable
//! queue. Global acceptance/completion/target registries delegate here.
use super::sqlite_media_video_jobs::{Assets, policy, require_prepared_authority};
use super::sqlite_scene_jobs::context;
use super::*;
use opencut_editor_application::{audit::ExecutionContext, export_video as export};
use opencut_job_contracts::export_video::{EXPORT_VIDEO_KIND, SubmitExportVideo};
use opencut_job_execution::scene_audio::TrustedJobAuthority;

pub fn start_export(
    app: &mut EditorApplication,
    input: SubmitExportVideo,
) -> Result<JobRecord, EditorError> {
    let assets = Assets(opencut_asset_store::AssetStore::read_only(
        app.asset_store_root(),
    )?);
    export::submit(app, &assets, input)
}
pub fn start_range(
    app: &mut EditorApplication,
    input: export::SubmitExportVideoRange,
) -> Result<JobRecord, EditorError> {
    let assets = Assets(opencut_asset_store::AssetStore::read_only(
        app.asset_store_root(),
    )?);
    export::submit_range(app, &assets, input)
}
impl export::ExportVideoStore for EditorApplication {
    fn accept_export_video(
        &mut self,
        request: JobRequest,
        prepared: export::PreparedExportVideo,
    ) -> Result<JobRecord, EditorError> {
        self.create_prepared_job(
            request,
            serde_json::to_value(prepared).map_err(|_| {
                EditorError::new(
                    "SERIALIZATION_ERROR",
                    "Cannot persist configurable export input",
                )
            })?,
        )
    }
}
pub(super) fn verified_target(
    connection: &Connection,
    input: &Value,
) -> Result<Option<(String, String)>, EditorError> {
    let fixed = if let Ok(public) = serde_json::from_value::<SubmitExportVideo>(input.clone()) {
        export::validate_submission(&public)?;
        serde_json::to_value(opencut_job_contracts::media_video::FixedVideoInput {
            idempotency_key: public.idempotency_key,
            project_id: public.project_id,
            revision: public.revision,
            sequence_id: public.sequence_id,
        })
    } else if let Ok(public) =
        serde_json::from_value::<export::SubmitExportVideoRange>(input.clone())
    {
        serde_json::to_value(opencut_job_contracts::media_video::FixedVideoInput {
            idempotency_key: public.idempotency_key,
            project_id: public.project_id,
            revision: public.revision,
            sequence_id: public.sequence_id,
        })
    } else if let Ok(prepared) =
        serde_json::from_value::<export::PreparedExportVideo>(input.clone())
    {
        serde_json::to_value(prepared.execution)
    } else {
        return Ok(None);
    }
    .map_err(|_| EditorError::new("INVALID_ARGUMENT", "Invalid export target"))?;
    super::sqlite_media_video_jobs::verified_target(connection, &fixed)
}
pub(super) fn validate_acceptance(
    connection: &Connection,
    caller: &ExecutionContext,
    request: &JobRequest,
    input: &Value,
) -> Result<(), EditorError> {
    let prepared: export::PreparedExportVideo =
        serde_json::from_value(input.clone()).map_err(|_| {
            EditorError::new("INVALID_ARGUMENT", "Invalid prepared configurable export")
        })?;
    export::validate_prepared_request(request, &prepared)?;
    require_prepared_authority(connection, caller, &prepared.execution)
}
pub(super) fn require_completion(
    connection: &Connection,
    job: &JobRecord,
) -> Result<(), EditorError> {
    if !matches!(
        job.kind.as_str(),
        EXPORT_VIDEO_KIND | export::EXPORT_VIDEO_RANGE_KIND
    ) {
        return Err(EditorError::new(
            "INVALID_STORED_RESULT",
            "Export completion kind differs",
        ));
    }
    let prepared: export::PreparedExportVideo =
        serde_json::from_value(job.input.clone()).map_err(|_| {
            EditorError::new("INVALID_ARGUMENT", "Invalid persisted configurable export")
        })?;
    require_prepared_authority(
        connection,
        &context(&prepared.execution.authority),
        &prepared.execution,
    )?;
    let stored: export::StoredExportVideo =
        serde_json::from_value(job.result.clone().ok_or_else(|| {
            EditorError::new(
                "INVALID_STORED_RESULT",
                "Missing configurable export result",
            )
        })?)
        .map_err(|_| {
            EditorError::new(
                "INVALID_STORED_RESULT",
                "Invalid configurable export result",
            )
        })?;
    export::validate_stored_result(&prepared, &stored)?;
    if prepared.range.is_some() != (job.kind == export::EXPORT_VIDEO_RANGE_KIND) {
        return Err(EditorError::new(
            "INVALID_STORED_RESULT",
            "Export range kind differs",
        ));
    }
    Ok(())
}
impl EditorApplication {
    pub fn revalidate_export_video(
        &self,
        id: &str,
        worker: &str,
        attempt: u32,
        authority: &TrustedJobAuthority,
    ) -> Result<(), EditorError> {
        // The shared revalidator selects the nested execution authority/policy
        // only for this registered kind; lease/cancel/permission logic is shared.
        super::sqlite_scene_jobs::revalidate(
            self,
            EXPORT_VIDEO_KIND,
            id,
            worker,
            attempt,
            authority,
            &policy(),
        )
    }
}

impl EditorApplication {
    pub fn revalidate_export_video_range(
        &self,
        id: &str,
        worker: &str,
        attempt: u32,
        authority: &TrustedJobAuthority,
    ) -> Result<(), EditorError> {
        super::sqlite_scene_jobs::revalidate(
            self,
            export::EXPORT_VIDEO_RANGE_KIND,
            id,
            worker,
            attempt,
            authority,
            &policy(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencut_contracts::{CreateProject, FrameRate, Time, graphic_edit::*, scene_ops::*};
    use opencut_editor_application::access::{Caller, PrincipalPolicy, ProjectGrant};
    use opencut_job_contracts::export_video::{ExportFormat, ExportQuality, ExportVideoOptions};
    fn now() -> i64 {
        lifecycle::JobClock::now_ms(&sqlite_jobs::SystemClock).unwrap()
    }
    fn fixture() -> (tempfile::TempDir, EditorApplication, SubmitExportVideo) {
        let directory = tempfile::tempdir().unwrap();
        let mut app = EditorApplication::open(directory.path().join("workspace.sqlite")).unwrap();
        let initial = app
            .create_scene_project(&CreateProjectRequest {
                idempotency_key: "create".into(),
                project: CreateProject {
                    id: "project".into(),
                    name: "Configurable export".into(),
                    sequence_id: "scene".into(),
                    width: 64,
                    height: 48,
                    fps: FrameRate {
                        numerator: 30,
                        denominator: 1,
                    },
                },
            })
            .unwrap();
        let time = |ticks: &str, timebase| Time {
            ticks: ticks.into(),
            timebase,
        };
        let mut request = GraphicPlanRequest {
            project_id: "project".into(),
            expected_revision: initial.revision,
            sequence_id: "scene".into(),
            idempotency_key: "graphic".into(),
            command: GraphicPlanCommand::Defaults {
                geometry: GraphicKind::Rectangle,
                start: GraphicTimeInput::Exact { time: time("0", 1) },
            },
        };
        let defaults = opencut_editor_application::graphic_edit::plan(&app, &request)
            .unwrap()
            .initial
            .unwrap();
        request.command = GraphicPlanCommand::Create {
            element_id: "shape".into(),
            track: GraphicTrackTarget::New {
                track_id: "shapes".into(),
            },
            values: GraphicValues {
                name: defaults.name,
                graphic: defaults.graphic,
                transform: defaults.transform,
                opacity: defaults.opacity,
                start: GraphicTimeInput::Exact { time: time("0", 1) },
                duration: GraphicTimeInput::Exact {
                    time: time("1", 10),
                },
            },
        };
        let transaction = opencut_editor_application::graphic_edit::plan(&app, &request)
            .unwrap()
            .transaction
            .unwrap();
        let revision = app.apply_editor_edit(&transaction).unwrap().revision;
        (
            directory,
            app,
            SubmitExportVideo {
                idempotency_key: "export".into(),
                project_id: "project".into(),
                revision,
                sequence_id: "scene".into(),
                options: ExportVideoOptions {
                    runtime_fingerprint: None,
                    resolution: opencut_contracts::export_video::RasterResolution::Source,
                    format: ExportFormat::Webm,
                    quality: ExportQuality::VeryHigh,
                    include_audio: false,
                },
            },
        )
    }
    #[test]
    fn preview_cache_reuses_immutable_receipt_after_editorial_revision_and_rejects_poisoning() {
        use opencut_editor_application::{agent_render::RenderSubmission,media_video_jobs::MediaVideoJobSubmissionStore,preview_cache};
        let (_directory,mut app,request)=fixture();
        let runtime="a".repeat(64);let zero=Time{ticks:"0".into(),timebase:1};
        let fixed=app.fixed_scene_video_snapshot("project",&request.revision).unwrap();
        let before=preview_cache::prepare(&mut app,&fixed.snapshot,"scene",&fixed.processing_policy,&runtime,&zero,false,||Ok(())).unwrap();
        let edited=app.apply_scene_project(&ApplySceneProject {
            project_id:"project".into(),expected_revision:request.revision.clone(),idempotency_key:"rename-preview".into(),label:"Editorial change".into(),
            operations:vec![ProjectOperation::Rename{name:"Renamed".into()}],
        }).unwrap();
        let fixed=app.fixed_scene_video_snapshot("project",&edited.revision).unwrap();
        let after=preview_cache::prepare(&mut app,&fixed.snapshot,"scene",&fixed.processing_policy,&runtime,&zero,false,||Ok(())).unwrap();
        assert_ne!(before.revision,after.revision);assert_eq!(before.segments[0].job_id,after.segments[0].job_id);
        let job=app.get_job(&after.segments[0].job_id).unwrap();
        let prepared:export::PreparedExportVideo=serde_json::from_value(job.input).unwrap();
        assert_eq!(prepared.execution.snapshot.revision,request.revision);
        let key:String=app.connection.query_row("SELECT idempotency_key FROM idempotency WHERE operation_family='create_job'",[],|r|r.get(0)).unwrap();
        let input=opencut_api_contracts::agent_render::VideoRenderInput {idempotency_key:key,project_id:"project".into(),revision:edited.revision,sequence_id:"scene".into(),options:prepared.options,range:prepared.range};
        assert_eq!(app.preview_video(input,&"f".repeat(64),true).unwrap_err().code,"INVALID_STORED_RESULT");
    }
    #[test]
    fn durable_options_and_fixed_revision_survive_head_change_and_reopen() {
        let (directory, mut app, request) = fixture();
        let job = start_export(&mut app, request.clone()).unwrap();
        let prepared: export::PreparedExportVideo =
            serde_json::from_value(job.input.clone()).unwrap();
        assert_eq!(prepared.options, request.options);
        assert!(prepared.execution.asset_lock.assets.is_empty());
        assert!(export::prepare(&prepared).unwrap().audio.is_none());
        app.apply_scene_project(&ApplySceneProject {
            project_id: "project".into(),
            expected_revision: request.revision.clone(),
            idempotency_key: "rename".into(),
            label: "New head".into(),
            operations: vec![ProjectOperation::Rename {
                name: "Changed".into(),
            }],
        })
        .unwrap();
        assert_eq!(start_export(&mut app, request.clone()).unwrap(), job);
        let mut changed = request.clone();
        changed.options.include_audio = true;
        assert!(start_export(&mut app, changed).is_err());
        let mut reopened =
            EditorApplication::open(directory.path().join("workspace.sqlite")).unwrap();
        assert_eq!(start_export(&mut reopened, request).unwrap(), job);
        assert_eq!(
            reopened
                .job_view(&job.job_id)
                .unwrap()
                .target
                .unwrap()
                .revision,
            prepared.execution.snapshot.revision
        );
    }
    #[test]
    fn nested_authority_keeps_credential_kind_lease_and_cancellation_fences() {
        for revoke in [false, true] {
            let (_directory, mut app, request) = fixture();
            app.set_principal_policy(PrincipalPolicy {
                principal_id: "agent".into(),
                expected_revision: "0".into(),
                enabled: true,
                grants: vec![ProjectGrant {
                    project_id: "project".into(),
                    actions: vec![ProjectAction::Read, ProjectAction::RunJobs],
                }],
            })
            .unwrap();
            let credential = app.issue_credential("agent").unwrap();
            app.set_caller(app.authenticate(&credential.secret).unwrap());
            let job = start_export(&mut app, request).unwrap();
            app.set_caller(Caller::LocalOwner);
            let running = app
                .claim_job("export-worker", now(), 30_000)
                .unwrap()
                .unwrap();
            let input: export::PreparedExportVideo =
                serde_json::from_value(running.input.clone()).unwrap();
            app.revalidate_export_video(
                &job.job_id,
                "export-worker",
                running.attempt,
                &input.execution.authority,
            )
            .unwrap();
            assert!(
                app.revalidate_export_video(
                    &job.job_id,
                    "wrong-worker",
                    running.attempt,
                    &input.execution.authority
                )
                .is_err()
            );
            assert!(
                app.revalidate_scene_video_job(
                    &job.job_id,
                    "export-worker",
                    running.attempt,
                    &input.execution.authority
                )
                .is_err()
            );
            assert!(
                app.revalidate_export_video(
                    &job.job_id,
                    "export-worker",
                    running.attempt,
                    &TrustedJobAuthority::LocalOwner
                )
                .is_err()
            );
            if revoke {
                app.revoke_credential(&credential.credential_id).unwrap();
                assert_eq!(
                    app.complete_job(
                        &job.job_id,
                        "export-worker",
                        running.attempt,
                        now(),
                        Value::Null
                    )
                    .unwrap_err()
                    .code,
                    "NOT_AUTHORIZED"
                );
                assert_eq!(app.get_job(&job.job_id).unwrap(), running);
            } else {
                app.cancel_job(&job.job_id).unwrap();
                let done = app
                    .complete_job(
                        &job.job_id,
                        "export-worker",
                        running.attempt,
                        now(),
                        Value::Null,
                    )
                    .unwrap();
                assert_eq!(done.state, JobState::Cancelled);
                assert!(done.result.is_none());
            }
            assert!(app.job_view(&job.job_id).unwrap().artifacts.is_empty());
        }
    }
    #[test]
    fn cached_fixed_input_rechecks_every_live_fence_and_entire_input() {
        for fault in [
            "cancel",
            "attempt",
            "owner",
            "lease",
            "state",
            "kind",
            "input",
            "authority",
            "policy",
            "binding",
            "credential",
            "principal",
            "read",
            "run",
            "deleted",
        ] {
            let (_directory, mut app, request) = fixture();
            app.set_principal_policy(PrincipalPolicy {
                principal_id: "agent".into(),
                expected_revision: "0".into(),
                enabled: true,
                grants: vec![ProjectGrant {
                    project_id: "project".into(),
                    actions: vec![ProjectAction::Read, ProjectAction::RunJobs],
                }],
            })
            .unwrap();
            let credential = app.issue_credential("agent").unwrap();
            app.set_caller(app.authenticate(&credential.secret).unwrap());
            let job = start_export(&mut app, request).unwrap();
            app.set_caller(Caller::LocalOwner);
            let running = app
                .claim_job("export-worker", now(), 30_000)
                .unwrap()
                .unwrap();
            let input: export::PreparedExportVideo = serde_json::from_value(running.input).unwrap();
            let validate = |app: &EditorApplication| {
                app.revalidate_export_video(
                    &job.job_id,
                    "export-worker",
                    running.attempt,
                    &input.execution.authority,
                )
            };
            validate(&app).unwrap();
            validate(&app).unwrap();
            assert!(app.scene_job_input.borrow().is_some());
            // A warm typed-policy comparison must not reuse admission after
            // even a nested field changes. Denial cannot poison the valid key.
            let mut changed = policy();
            changed.audio.max_pcm_bytes -= 1;
            assert_eq!(
                sqlite_scene_jobs::revalidate(
                    &app,
                    EXPORT_VIDEO_KIND,
                    &job.job_id,
                    "export-worker",
                    running.attempt,
                    &input.execution.authority,
                    &changed,
                )
                .unwrap_err()
                .code,
                "NOT_AUTHORIZED"
            );
            validate(&app).unwrap();
            let sql = match fault {
                "cancel" => "UPDATE jobs SET cancel_requested=1",
                "attempt" => "UPDATE jobs SET attempt=attempt+1",
                "owner" => "UPDATE jobs SET lease_owner='other'",
                "lease" => "UPDATE jobs SET lease_until=0",
                "state" => "UPDATE jobs SET state='queued'",
                "kind" => "UPDATE jobs SET kind='render.video_mp4.v1'",
                "input" => {
                    "UPDATE jobs SET input_json=json_set(input_json,'$.options.quality','changed')"
                }
                "authority" => {
                    "UPDATE jobs SET input_json=json_set(input_json,'$.execution.authority.type','local_owner')"
                }
                "policy" => {
                    "UPDATE jobs SET input_json=json_set(input_json,'$.execution.processing_policy.max_frames',1)"
                }
                "binding" => {
                    "UPDATE job_projects SET revision=(SELECT revision FROM revisions WHERE revision<>job_projects.revision LIMIT 1)"
                }
                "credential" => "UPDATE access_credentials SET revoked=1",
                "principal" => "UPDATE access_principals SET enabled=0",
                "read" => "UPDATE access_grants SET action_mask=8",
                "run" => "UPDATE access_grants SET action_mask=1",
                "deleted" => {
                    "INSERT INTO deleted_projects(project_id,revision,request_id) SELECT project_id,head_revision,'deleted' FROM projects"
                }
                _ => unreachable!(),
            };
            app.connection.execute_batch(sql).unwrap();
            assert!(
                validate(&app).is_err(),
                "cached validation admitted {fault}"
            );
            assert!(app.job_view(&job.job_id).unwrap().artifacts.is_empty());
        }
    }
}
