//! Exercise public tool dispatch rather than calling application planners as
//! stand-ins for a transport. Private CAS setup is deliberately native-only.
use super::*;
use opencut_contracts::{Asset, AssetKind, scene::*, scene_ops::*};
use opencut_editor_application::access::{PrincipalPolicy, ProjectAction, ProjectGrant};
use serde_json::json;
use std::collections::BTreeMap;
mod blend;
mod clipboard;
mod export_video;
mod font_derived;
mod graphic;
mod mask;
mod subtitle_text;
mod timeline;

fn data(response: Response) -> Value {
    assert!(response.error.is_none(), "{:?}", response.error);
    response.data.unwrap()
}
fn error(response: Response, expected: &str) {
    assert!(response.data.is_none());
    assert_eq!(response.error.unwrap().code, expected);
}
fn create(automation: &mut Automation) -> Value {
    let created = data(automation.call(
        "opencut_project_create",
        json!({"project":{"id":"p","name":"Original","sequence_id":"main","width":64,"height":48,
        "fps":{"numerator":25,"denominator":1}},"idempotency_key":"create"}),
    ));
    let snapshot = inspect(automation);
    assert_eq!(created["project_id"], snapshot["project"]["id"]);
    assert_eq!(created["revision"], snapshot["revision"]);
    snapshot
}
fn inspect(automation: &mut Automation) -> Value {
    let view = data(automation.call(
        "opencut_project_inspect",
        json!({"project_id":"p","revision":null}),
    ));
    assert_eq!(view["kind"], "editor");
    view["snapshot"].clone()
}
fn marker(id: &str) -> Value {
    json!({"type":"bookmark","operation":{"kind":"put","bookmark":{"id":id,"sequence_id":"main","time":{"ticks":"1","timebase":1},"duration":null,"color":null,"note":null}}})
}
fn apply(automation: &mut Automation, key: &str, operations: Vec<Value>) -> Value {
    let snapshot = inspect(automation);
    data(automation.call("opencut_edit_apply",json!({"project_id":"p","expected_revision":snapshot["revision"],"idempotency_key":key,"label":key,"operations":operations})))
}
fn undo_input(automation: &mut Automation, transaction: &Value, key: &str) -> Value {
    json!({"project_id":"p","expected_revision":inspect(automation)["revision"],"target_transaction_id":transaction,"idempotency_key":key})
}
fn no_locator(value: &Value, locator: &std::path::Path) {
    let serialized = serde_json::to_string(value).unwrap();
    assert!(!serialized.contains(locator.to_str().unwrap()));
    assert!(!serialized.contains("legacy_locator"));
    assert!(!serialized.contains("\"uri\""));
}
fn policy(automation: &mut Automation, generation: &str, actions: Vec<ProjectAction>) {
    automation.caller = Caller::LocalOwner;
    automation
        .app
        .set_execution_context(ExecutionContext::for_caller(
            CallSource::Embedded,
            Caller::LocalOwner,
        ));
    automation
        .app
        .set_principal_policy(PrincipalPolicy {
            principal_id: "agent".into(),
            expected_revision: generation.into(),
            enabled: true,
            grants: if actions.is_empty() {
                Vec::new()
            } else {
                vec![ProjectGrant {
                    project_id: "p".into(),
                    actions,
                }]
            },
        })
        .unwrap();
}

pub(super) fn seed_source_video(
    automation: &mut Automation,
    directory: &std::path::Path,
) -> (std::path::PathBuf, String) {
    let created = create(automation);
    let locator = directory.join("source.bin");
    std::fs::write(&locator, b"bounded source-audio editing fixture").unwrap();
    let asset = opencut_asset_store::AssetStore::open(automation.app.asset_store_root())
        .unwrap()
        .pin_asset(&Asset {
            id: "asset".into(),
            name: "Video".into(),
            kind: AssetKind::Video,
            uri: locator.to_string_lossy().into(),
            duration: None,
            version: None,
            metadata: BTreeMap::new(),
        })
        .unwrap();
    let snapshot = automation
        .app
        .inspect_scene_project(&InspectProject {
            project_id: "p".into(),
            revision: None,
        })
        .unwrap();
    let track_id = snapshot.project.sequences[0].main_track_id.clone();
    let time = |ticks: &str| opencut_contracts::Time {
        ticks: ticks.into(),
        timebase: 1,
    };
    let element = TimelineElement {
        id: "video".into(),
        name: "Video".into(),
        timeline_range: opencut_contracts::TimeRange {
            start: time("0"),
            duration: time("2"),
        },
        source_span: ElementSourceSpan::ClassicTrimmed {
            trim_start: time("1"),
            trim_end: time("1"),
            source_duration: Some(time("4")),
        },
        enabled: true,
        content: ElementContent::Video {
            asset_id: "asset".into(),
            video_stream: None,
            source_audio_enabled: true,
            audio: AudioStyle {
                stream: None,
                gain: Gain::Decibels { value: -6.0 },
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
        visual: Some(VisualStyle {
            profile: VisualProfile::ClassicCanvasV31,
            transform: Transform {
                position_x_px: 0.0,
                position_y_px: 0.0,
                scale_x: 1.0,
                scale_y: 1.0,
                rotation_degrees: 0.0,
            },
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            effects: vec![],
            masks: vec![],
        }),
        channels: vec![],
    };
    automation
        .app
        .apply_scene_project(&ApplySceneProject {
            project_id: "p".into(),
            expected_revision: created["revision"].as_str().unwrap().into(),
            idempotency_key: "seed-source".into(),
            label: "Seed".into(),
            operations: vec![
                ProjectOperation::AssetPut {
                    asset: ProjectAsset {
                        id: "asset".into(),
                        name: "Video".into(),
                        kind: AssetKind::Video,
                        version: asset.version.unwrap(),
                        duration: Some(time("4")),
                        origin: ResourceOrigin::LegacyLocator {
                            uri: locator.to_string_lossy().into(),
                        },
                        metadata: ProjectMediaMetadata {
                            mime_type: None,
                            intrinsic_size: None,
                            frame_rate: None,
                            rotation_degrees: None,
                            audio: AudioPresence::Present { metadata: None },
                        },
                        license: None,
                    },
                },
                ProjectOperation::ElementPut {
                    sequence_id: "main".into(),
                    track_id: track_id.clone(),
                    element: Box::new(element),
                },
            ],
        })
        .unwrap();
    (locator, track_id)
}

#[test]
fn audio_gain_public_dispatch_is_fixed_atomic_replayable_and_reversible() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workspace.sqlite");
    let mut automation = Automation::open(&database).unwrap();
    let (locator, track_id) = seed_source_video(&mut automation, directory.path());
    policy(
        &mut automation,
        "0",
        vec![
            ProjectAction::Read,
            ProjectAction::Edit,
            ProjectAction::Restore,
        ],
    );
    let issued = automation.app.issue_credential("agent").unwrap();
    let caller = automation.app.authenticate(&issued.secret).unwrap();
    automation.caller = caller.clone();
    let fixed = inspect(&mut automation);
    let intent = json!({"project_id":"p","expected_revision":fixed["revision"],"sequence_id":"main","track_id":track_id,"element_id":"video","idempotency_key":"gain-public","label":"Adjust volume","command":{"kind":"set_decibels","decibels_tenths":-123}});
    let planned = data(automation.call("opencut_project_audio_gain_plan", intent.clone()));
    no_locator(&planned, &locator);
    assert_eq!(inspect(&mut automation), fixed);
    assert_eq!(
        planned["transaction"]["operations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        planned["decision"]["new_gain"],
        json!({"unit":"decibels","value":-12.3})
    );
    // Fail at the last transactional audit boundary; neither head nor receipts
    // may escape the SQLite rollback.
    let injection = rusqlite::Connection::open(&database).unwrap();
    injection.execute_batch("CREATE TRIGGER fail_gain BEFORE INSERT ON audit_records BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        automation
            .call("opencut_edit_apply", planned["transaction"].clone())
            .error
            .is_some()
    );
    assert_eq!(inspect(&mut automation), fixed);
    injection.execute_batch("DROP TRIGGER fail_gain").unwrap();
    let committed = data(automation.call("opencut_edit_apply", planned["transaction"].clone()));
    assert_eq!(
        data(automation.call("opencut_edit_apply", planned["transaction"].clone())),
        committed
    );
    error(
        automation.call("opencut_project_audio_gain_plan", intent),
        "REVISION_CONFLICT",
    );
    drop(automation);
    let mut automation = Automation::open(&database).unwrap();
    automation.caller = caller.clone();
    let changed = inspect(&mut automation);
    let mut expected = fixed["project"].clone();
    expected["sequences"][0]["tracks"][0]["elements"][0]["content"]["audio"]["gain"] =
        json!({"unit":"decibels","value":-12.3});
    assert_eq!(changed["project"], expected);
    let no_change = json!({"project_id":"p","expected_revision":changed["revision"],"sequence_id":"main","track_id":track_id,"element_id":"video","idempotency_key":"gain-unchanged","label":"Unchanged","command":{"kind":"set_decibels","decibels_tenths":-123}});
    assert!(
        data(automation.call("opencut_project_audio_gain_plan", no_change))["transaction"]
            .is_null()
    );
    let mute = data(automation.call("opencut_project_audio_gain_plan", json!({"project_id":"p","expected_revision":changed["revision"],"sequence_id":"main","track_id":track_id,"element_id":"video","idempotency_key":"gain-mute","label":"Mute","command":{"kind":"set_muted","muted":true}})));
    // Read-only planning grants no future editing authority.
    policy(&mut automation, "1", vec![ProjectAction::Read]);
    automation.caller = caller.clone();
    error(
        automation.call("opencut_edit_apply", mute["transaction"].clone()),
        "NOT_AUTHORIZED",
    );
    assert_eq!(inspect(&mut automation), changed);
    policy(
        &mut automation,
        "2",
        vec![
            ProjectAction::Read,
            ProjectAction::Edit,
            ProjectAction::Restore,
        ],
    );
    automation.caller = caller;
    let undo = undo_input(&mut automation, &committed["transaction_id"], "undo-gain");
    data(automation.call("opencut_edit_undo_apply", undo));
    assert_eq!(inspect(&mut automation)["project"], fixed["project"]);
}

#[test]
fn source_audio_tool_dispatch_uses_one_public_transaction_and_current_edit_authority() {
    let directory = tempfile::tempdir().unwrap();
    let mut automation = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    let (locator, track_id) = seed_source_video(&mut automation, directory.path());
    policy(
        &mut automation,
        "0",
        vec![ProjectAction::Read, ProjectAction::Edit],
    );
    let issued = automation.app.issue_credential("agent").unwrap();
    let caller = automation.app.authenticate(&issued.secret).unwrap();
    automation.caller = caller.clone();
    let fixed = inspect(&mut automation);
    let intent = json!({"project_id":"p","expected_revision":fixed["revision"],"sequence_id":"main","track_id":track_id,"element_id":"video","idempotency_key":"separate-public","label":"Separate audio","command":{"kind":"separate","audio_element_id":"detached","destination":{"kind":"first_available","fallback_track_id":"audio"}}});
    let planned = data(automation.call("opencut_project_source_audio_plan", intent.clone()));
    no_locator(&planned, &locator);
    assert_eq!(inspect(&mut automation)["revision"], fixed["revision"]);
    let committed = data(automation.call("opencut_edit_apply", planned["transaction"].clone()));
    assert_eq!(
        data(automation.call("opencut_edit_apply", planned["transaction"].clone())),
        committed
    );
    error(
        automation.call("opencut_project_source_audio_plan", intent),
        "REVISION_CONFLICT",
    );
    let separated = inspect(&mut automation);
    assert_eq!(
        separated["project"]["sequences"][0]["tracks"][0]["elements"][0]["content"]["source_audio_enabled"],
        false
    );
    assert_eq!(
        separated["project"]["sequences"][0]["tracks"][1]["elements"][0]["id"],
        "detached"
    );
    policy(&mut automation, "1", vec![ProjectAction::Read]);
    automation.caller = caller;
    error(automation.call("opencut_project_source_audio_plan", json!({"project_id":"p","expected_revision":separated["revision"],"sequence_id":"main","track_id":track_id,"element_id":"video","idempotency_key":"restore-denied","label":"Restore audio","command":{"kind":"restore"}})), "NOT_AUTHORIZED");
}

#[test]
fn real_public_dispatch_preserves_selective_edits_and_never_returns_resource_locators() {
    let directory = tempfile::tempdir().unwrap();
    let mut automation = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    let created = create(&mut automation);
    assert!(created["project"].get("schema_version").is_none());
    let locator = directory.path().join("private-locator.bin");
    std::fs::write(&locator, b"tiny CAS fixture").unwrap();
    let pinned = opencut_asset_store::AssetStore::open(automation.app.asset_store_root())
        .unwrap()
        .pin_asset(&Asset {
            id: "asset".into(),
            name: "Asset".into(),
            kind: AssetKind::Image,
            uri: locator.to_string_lossy().into(),
            duration: None,
            version: None,
            metadata: BTreeMap::new(),
        })
        .unwrap();
    let target = automation
        .app
        .apply_scene_project(&ApplySceneProject {
            project_id: "p".into(),
            expected_revision: created["revision"].as_str().unwrap().into(),
            idempotency_key: "native-bind".into(),
            label: "Native immutable binding".into(),
            operations: vec![ProjectOperation::AssetPut {
                asset: ProjectAsset {
                    id: "asset".into(),
                    name: "Asset".into(),
                    kind: AssetKind::Image,
                    version: pinned.version.unwrap(),
                    duration: None,
                    origin: ResourceOrigin::LegacyLocator {
                        uri: locator.to_string_lossy().into(),
                    },
                    license: None,
                    metadata: ProjectMediaMetadata {
                        mime_type: None,
                        intrinsic_size: None,
                        frame_rate: None,
                        rotation_degrees: None,
                        audio: AudioPresence::Unknown {},
                    },
                },
            }],
        })
        .unwrap();
    let snapshot = inspect(&mut automation);
    assert_eq!(snapshot["project"]["assets"].as_array().unwrap().len(), 1);
    no_locator(&snapshot, &locator);
    let later = apply(
        &mut automation,
        "public-edit",
        vec![
            json!({"type":"rename","name":"Public name"}),
            marker("later"),
        ],
    );
    no_locator(&later, &locator);
    let input = undo_input(
        &mut automation,
        &json!(target.transaction_id),
        "undo-binding",
    );
    let planned = data(automation.call("opencut_edit_undo_plan", input.clone()));
    assert!(planned["conflicts"].as_array().unwrap().is_empty());
    no_locator(&planned, &locator);
    assert!(planned["changes"].as_array().unwrap().iter().any(|change| {
        change["path"] == "/assets/asset"
            && change["before_sha256"]
                .as_str()
                .is_some_and(|v| v.len() == 64)
    }));
    let undone = data(automation.call("opencut_edit_undo_apply", input));
    no_locator(&undone, &locator);
    let snapshot = inspect(&mut automation);
    assert_eq!(snapshot["project"]["name"], "Public name");
    assert!(snapshot["project"]["assets"].as_array().unwrap().is_empty());
    assert_eq!(
        snapshot["project"]["sequences"][0]["bookmarks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let navigation = data(automation.call(
        "opencut_project_history_navigation",
        json!({"project_id":"p","revision":null}),
    ));
    assert_eq!(
        navigation["undo"]["transaction_id"],
        later["transaction_id"]
    );
    assert_eq!(
        navigation["redo"]["undo_transaction_id"],
        undone["transaction"]["transaction_id"]
    );
    no_locator(&navigation, &locator);
    let redo = json!({"project_id":"p","expected_revision":snapshot["revision"],"undo_transaction_id":undone["transaction"]["transaction_id"],"idempotency_key":"redo-binding"});
    let planned = data(automation.call("opencut_edit_redo_plan", redo.clone()));
    assert!(planned["conflicts"].as_array().unwrap().is_empty());
    no_locator(&planned, &locator);
    let redone = data(automation.call("opencut_edit_redo_apply", redo));
    no_locator(&redone, &locator);
    let restored = inspect(&mut automation);
    assert_eq!(restored["project"]["assets"].as_array().unwrap().len(), 1);
    assert_eq!(restored["project"]["name"], "Public name");
    no_locator(&restored, &locator);
}

#[test]
fn public_dispatch_requires_current_read_edit_restore_authority_and_revocation_overrides_receipts()
{
    let directory = tempfile::tempdir().unwrap();
    let mut automation = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    create(&mut automation);
    let target = apply(&mut automation, "target", vec![marker("target")]);
    policy(&mut automation, "0", vec![ProjectAction::Read]);
    let issued = automation.app.issue_credential("agent").unwrap();
    let caller = automation.authenticate(&issued.secret).unwrap();
    automation.caller = caller.clone();
    let snapshot = inspect(&mut automation);
    let edit = json!({"project_id":"p","expected_revision":snapshot["revision"],"idempotency_key":"restricted-edit","label":"Restricted edit","operations":[{"type":"rename","name":"Agent name"}]});
    error(
        automation.call("opencut_edit_apply", edit.clone()),
        "NOT_AUTHORIZED",
    );
    let undo = undo_input(
        &mut automation,
        &target["transaction_id"],
        "restricted-undo",
    );
    error(
        automation.call("opencut_edit_undo_apply", undo),
        "NOT_AUTHORIZED",
    );
    data(automation.call(
        "opencut_project_history_navigation",
        json!({"project_id":"p","revision":null}),
    ));
    policy(&mut automation, "1", Vec::new());
    automation.caller = caller.clone();
    error(
        automation.call(
            "opencut_project_inspect",
            json!({"project_id":"p","revision":null}),
        ),
        "NOT_AUTHORIZED",
    );
    error(
        automation.call("opencut_edit_apply", edit.clone()),
        "NOT_AUTHORIZED",
    );
    policy(
        &mut automation,
        "2",
        vec![ProjectAction::Read, ProjectAction::Edit],
    );
    automation.caller = caller.clone();
    data(automation.call("opencut_edit_apply", edit));
    let undo = undo_input(
        &mut automation,
        &target["transaction_id"],
        "restricted-undo",
    );
    error(
        automation.call("opencut_edit_undo_apply", undo.clone()),
        "NOT_AUTHORIZED",
    );
    policy(
        &mut automation,
        "3",
        vec![ProjectAction::Read, ProjectAction::Restore],
    );
    automation.caller = caller.clone();
    let undone = data(automation.call("opencut_edit_undo_apply", undo.clone()));
    let snapshot = inspect(&mut automation);
    assert_eq!(snapshot["project"]["name"], "Agent name");
    assert!(
        snapshot["project"]["sequences"][0]["bookmarks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let redo = json!({"project_id":"p","expected_revision":snapshot["revision"],"undo_transaction_id":undone["transaction"]["transaction_id"],"idempotency_key":"restricted-redo"});
    data(automation.call("opencut_edit_redo_apply", redo));
    assert_eq!(
        inspect(&mut automation)["project"]["sequences"][0]["bookmarks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    automation.caller = Caller::LocalOwner;
    automation
        .app
        .set_execution_context(ExecutionContext::for_caller(
            CallSource::Embedded,
            Caller::LocalOwner,
        ));
    automation
        .app
        .revoke_credential(&issued.credential_id)
        .unwrap();
    automation.caller = caller;
    error(
        automation.call("opencut_edit_undo_apply", undo),
        "NOT_AUTHORIZED",
    );
}

#[test]
fn media_workspace_dispatch_reuses_fixed_assets_and_commits_new_track_once() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workspace.sqlite");
    let mut automation = Automation::open(&database).unwrap();
    let (locator, _) = seed_source_video(&mut automation, directory.path());
    policy(
        &mut automation,
        "0",
        vec![ProjectAction::Read, ProjectAction::Edit],
    );
    let issued = automation.app.issue_credential("agent").unwrap();
    let caller = automation.app.authenticate(&issued.secret).unwrap();
    automation.caller = caller.clone();
    let fixed = inspect(&mut automation);
    let input = json!({
        "project_id":"p","expected_revision":fixed["revision"],"sequence_id":"main",
        "idempotency_key":"insert-existing-audio","label":"Insert existing source audio",
        "bindings":[],"ripple":false,
        "new_tracks":[{"id":"audio-placement","name":"Audio","kind":"audio"}],
        "placements":[{"element_id":"audio-placed","name":"Existing source","track_id":"audio-placement","asset_id":"asset",
          "start":{"ticks":"1","timebase":1},"content":{"kind":"audio","timing":{"kind":"source_range",
            "range":{"start":{"ticks":"1","timebase":1},"duration":{"ticks":"2","timebase":1}},
            "rate":{"numerator":1,"denominator":1},"maintain_pitch":false}}}]
    });
    let planned = data(automation.call("opencut_media_import_plan", input.clone()));
    no_locator(&planned, &locator);
    assert_eq!(inspect(&mut automation)["revision"], fixed["revision"]);
    assert!(
        planned["decisions"]["bound_asset_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(planned["decisions"]["raised_fps"].is_null());
    let transaction = planned["transaction"].clone();
    let committed = data(automation.call("opencut_edit_apply", transaction.clone()));
    assert_eq!(
        data(automation.call("opencut_edit_apply", transaction)),
        committed
    );
    error(
        automation.call("opencut_media_import_plan", input.clone()),
        "REVISION_CONFLICT",
    );
    let after = inspect(&mut automation);
    assert_eq!(after["project"]["assets"].as_array().unwrap().len(), 1);
    let tracks = after["project"]["sequences"][0]["tracks"]
        .as_array()
        .unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[1]["elements"][0]["id"], "audio-placed");
    assert_eq!(
        tracks[1]["elements"][0]["timeline_range"]["start"],
        json!({"ticks":"1","timebase":1})
    );
    drop(automation);
    let mut automation = Automation::open(&database).unwrap();
    automation.caller = caller.clone();
    assert_eq!(inspect(&mut automation), after);
    let mut current = input;
    current["expected_revision"] = after["revision"].clone();
    current["idempotency_key"] = json!("denied-insert");
    policy(&mut automation, "1", vec![ProjectAction::Read]);
    automation.caller = caller;
    error(
        automation.call("opencut_media_import_plan", current),
        "NOT_AUTHORIZED",
    );
}

#[test]
fn frame_submission_preserves_fixed_scope_and_requires_current_read_and_jobs() {
    let directory = tempfile::tempdir().unwrap();
    let mut automation = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    seed_source_video(&mut automation, directory.path());
    let snapshot = inspect(&mut automation);
    for retired in [
        "opencut_job_frame_png",
        "opencut_job_text_frame_png",
        "opencut_project_frame_capability",
        "opencut_project_text_frame_capability",
    ] {
        error(automation.call(retired, json!({})), "NOT_FOUND");
    }
    policy(
        &mut automation,
        "0",
        vec![ProjectAction::Read, ProjectAction::RunJobs],
    );
    let issued = automation.app.issue_credential("agent").unwrap();
    let caller = automation.app.authenticate(&issued.secret).unwrap();
    automation.caller = caller.clone();
    let capability = data(automation.call("opencut_render_capability", json!({})));
    assert_eq!(capability["submit_tool"], "opencut_render_submit");
    let request = json!({"kind":"frame","request":{"project_id":"p","revision":snapshot["revision"],"sequence_id":"main","idempotency_key":"frame","time":{"ticks":"1","timebase":7}}});
    let accepted = data(automation.call("opencut_render_submit", request.clone()));
    assert_eq!(accepted["kind"], "render.frame");
    assert_eq!(
        automation
            .app
            .get_job(accepted["job_id"].as_str().unwrap())
            .unwrap()
            .kind,
        "render.project_scene_frame.v4"
    );
    assert_eq!(accepted["target"]["revision"], snapshot["revision"]);
    assert_eq!(accepted["target"]["sequence_id"], "main");
    assert_eq!(accepted["artifacts"], json!([]));
    assert_eq!(
        data(automation.call("opencut_render_submit", request.clone())),
        accepted
    );
    no_locator(&accepted, directory.path());
    let mut changed = request.clone();
    changed["request"]["time"]["ticks"] = json!("2");
    error(
        automation.call("opencut_render_submit", changed),
        "IDEMPOTENCY_KEY_REUSED",
    );
    for (generation, actions) in [("1", vec![ProjectAction::Read]), ("2", vec![])] {
        policy(&mut automation, generation, actions);
        automation.caller = caller.clone();
        error(
            automation.call("opencut_render_submit", request.clone()),
            "NOT_AUTHORIZED",
        );
    }
}

mod package;

#[test]
fn retired_classic_recovery_has_no_registry_or_dispatch_entry() {
    let directory = tempfile::tempdir().unwrap();
    let mut host = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    let before = create(&mut host);
    for name in ["opencut_classic_analyze", "opencut_classic_import"] {
        assert!(!opencut_api_contracts::tools::is_tool(name));
        assert!(opencut_api_contracts::tools::tool_spec(name).is_none());
        error(host.call(name, json!({})), "NOT_FOUND");
    }
    assert_eq!(inspect(&mut host), before);
}
