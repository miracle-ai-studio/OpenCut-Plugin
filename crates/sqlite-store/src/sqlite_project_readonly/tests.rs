use super::*;
use opencut_contracts::{FrameRate, scene::*};
use opencut_editor_application::gc::ReferenceStore;

fn source(app: &mut EditorApplication) -> EditorSnapshot {
    let initial = app
        .create_scene_project(&CreateProjectRequest {
            project: CreateProject {
                id: "source".into(),
                name: "Source".into(),
                sequence_id: "main".into(),
                width: 64,
                height: 48,
                fps: FrameRate {
                    numerator: 25,
                    denominator: 1,
                },
            },
            idempotency_key: "source-create".into(),
        })
        .unwrap();
    let track: SceneTrack = serde_json::from_value(json!({
        "id":"graphics","name":"Graphics","kind":"graphic","enabled":true,
        "muted":false,"locked":false,"opacity":1.0,"gain":{"unit":"decibels","value":0.0},"channels":[],
        "elements":[{"id":"shape","name":"Shape","timeline_range":{"start":{"ticks":"0","timebase":1},"duration":{"ticks":"1","timebase":1}},
            "source_span":{"profile":"classic_trimmed","trim_start":{"ticks":"0","timebase":1},"trim_end":{"ticks":"0","timebase":1},"source_duration":null},
            "enabled":true,"channels":[],"content":{"kind":"graphic","graphic":{
                "geometry":{"kind":"rectangle","corner_radius_percent":0.0},"source_size":{"width":64,"height":48},
                "fill":{"encoding":"srgb","red":1.0,"green":0.0,"blue":0.0,"alpha":1.0},
                "stroke":{"color":{"encoding":"srgb","red":0.0,"green":0.0,"blue":0.0,"alpha":1.0},"width_px":0.0,"align":"center"}}},
            "visual":{"profile":"classic_canvas_v31","transform":{"position_x_px":0.0,"position_y_px":0.0,"scale_x":1.0,"scale_y":1.0,"rotation_degrees":0.0},"opacity":1.0,"blend_mode":"normal","effects":[],"masks":[]}}]
    })).unwrap();
    app.apply_scene_project(&ApplySceneProject {
        project_id: "source".into(),
        expected_revision: initial.revision,
        idempotency_key: "source-shape".into(),
        label: "Shape".into(),
        operations: vec![ProjectOperation::TrackPut {
            sequence_id: "main".into(),
            track,
        }],
    })
    .unwrap();
    app.inspect_scene_project(&InspectProject {
        project_id: "source".into(),
        revision: None,
    })
    .unwrap()
}
fn input(source: &EditorSnapshot, payload: String) -> CreateReadOnlyProject {
    CreateReadOnlyProject {
        source_project_id: source.project.id.clone(),
        source_revision: source.revision.clone(),
        project_id: "preserved".into(),
        name: "Preserved".into(),
        idempotency_key: "preserved-create".into(),
        opaque_effects: vec![OpaqueEffectNode {
            id: "unknown-effect".into(),
            target: OpaqueEffectTarget {
                sequence_id: "main".into(),
                track_id: "graphics".into(),
                element_id: "shape".into(),
            },
            position: 0,
            enabled: true,
            plugin: EffectPluginRef {
                id: "org.example.missing".into(),
                version: "1.0.0".into(),
                package_sha256: "a".repeat(64),
                contribution: "org.example.missing.effect".into(),
            },
            payload_json: payload,
        }],
    }
}
fn counts(app: &EditorApplication) -> Vec<i64> {
    [
        "projects",
        "revisions",
        "idempotency",
        "outbox",
        "audit_records",
        "project_media_bindings",
        "jobs",
    ]
    .iter()
    .map(|table| {
        app.connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
    .collect()
}
fn raw(app: &EditorApplication, revision: &str) -> String {
    app.connection
        .query_row(
            "SELECT document_json FROM revisions WHERE revision=?1",
            [revision],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn readonly_preservation_is_atomic_exact_and_replayable_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.sqlite");
    let mut store = EditorApplication::open(&path).unwrap();
    let source = source(&mut store);
    let source_bytes = raw(&store, &source.revision);
    let request = input(
        &source,
        "{ \"duplicate\":1, \"duplicate\":2, \"future\": [\"未识别\", true] }\n".into(),
    );
    let before = counts(&store);
    store.connection.execute_batch("CREATE TRIGGER fail_preserved_audit BEFORE INSERT ON audit_records WHEN NEW.action='project.readonly_created' BEGIN SELECT RAISE(ABORT,'injected audit failure'); END;").unwrap();
    assert!(app::create(&mut store, &request).is_err());
    assert_eq!(counts(&store), before);
    store
        .connection
        .execute_batch("DROP TRIGGER fail_preserved_audit")
        .unwrap();
    let snapshot = app::create(&mut store, &request).unwrap();
    let committed = counts(&store);
    assert_eq!(app::create(&mut store, &request).unwrap(), snapshot);
    assert_eq!(counts(&store), committed);
    let collision = CreateProjectRequest {
        project: CreateProject {
            id: "collision".into(),
            name: "Collision".into(),
            sequence_id: "main".into(),
            width: 64,
            height: 48,
            fps: FrameRate {
                numerator: 25,
                denominator: 1,
            },
        },
        idempotency_key: request.idempotency_key.clone(),
    };
    assert_eq!(
        store.create_scene_project(&collision).unwrap_err().code,
        "IDEMPOTENCY_KEY_REUSED"
    );
    let inspect = InspectProject {
        project_id: request.project_id.clone(),
        revision: None,
    };
    assert_eq!(
        store.inspect_scene_project(&inspect).unwrap_err().code,
        "MISSING_PLUGIN_DEPENDENCY"
    );
    assert_eq!(counts(&store), committed);
    assert_eq!(raw(&store, &source.revision), source_bytes);
    for variant in 0..4 {
        let mut invalid = request.clone();
        match variant {
            0 => invalid.opaque_effects[0].position = 1,
            1 => invalid.opaque_effects[0].id = "shape".into(),
            2 => invalid.opaque_effects[0].payload_json = "{".into(),
            _ => invalid.opaque_effects[0].target.element_id = "other".into(),
        }
        assert!(app::prepare(&invalid, source.clone()).is_err());
    }
    drop(store);
    let mut reopened = EditorApplication::open(&path).unwrap();
    assert_eq!(app::create(&mut reopened, &request).unwrap(), snapshot);
    let loaded = reopened.read_readonly_project(&inspect).unwrap();
    assert_eq!(loaded, snapshot);
    assert_eq!(
        app::manifest(&loaded).unwrap().opaque_effects,
        request.opaque_effects
    );
    assert_eq!(raw(&reopened, &source.revision), source_bytes);
}

#[test]
fn readonly_escaped_payload_references_are_protected_without_rewriting_revisions() {
    let mut store = EditorApplication::open(":memory:").unwrap();
    let source = source(&mut store);
    let artifact = "artifact_opaque_dependency";
    let escaped = artifact
        .chars()
        .map(|c| format!("\\u{:04x}", c as u32))
        .collect::<String>();
    let request = input(
        &source,
        format!("{{ \"future_resource\": \"{escaped}\", \"future_resource\": null }}"),
    );
    let snapshot = app::create(&mut store, &request).unwrap();
    let original = raw(&store, &snapshot.revision);
    assert!(!original.contains(artifact));
    let references = sqlite_artifact_references::scan(
        &store.connection,
        "other-job",
        artifact,
        "/unpublished/placeholder",
    )
    .unwrap();
    assert!(references.roots.iter().any(|root| root.kind
        == opencut_api_contracts::artifact_retention::ReferenceKind::Revision
        && root.resource_id == snapshot.revision));
    store.gc_protected(&[]).unwrap();
    assert_eq!(raw(&store, &snapshot.revision), original);
    assert_eq!(
        app::manifest(&snapshot).unwrap().opaque_effects[0].payload_json,
        request.opaque_effects[0].payload_json
    );
    let schema: String = store
        .connection
        .query_row(
            "SELECT value FROM metadata WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(schema, DATABASE_SCHEMA_VERSION.to_string());
}
