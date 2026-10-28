use super::*;
use opencut_contracts::{CreateProject, FrameRate, scene_ops::CreateProjectRequest};
#[test]
fn approved_named_source_uses_one_public_catalog_and_import_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = Automation::open(tmp.path().join("workspace.sqlite")).unwrap();
    let snapshot = host
        .app
        .create_scene_project(&CreateProjectRequest {
            project: CreateProject {
                id: "p".into(),
                name: "p".into(),
                sequence_id: "s".into(),
                width: 64,
                height: 48,
                fps: FrameRate {
                    numerator: 25,
                    denominator: 1,
                },
            },
            idempotency_key: "project".into(),
        })
        .unwrap();
    let pin = application::compiled_sources().unwrap().first().unwrap();
    let public = host.call(
        "opencut_font_source",
        serde_json::json!({"catalog_id":pin.source.face.id}),
    );
    assert!(public.error.is_none());
    let source = public.data.unwrap();
    assert_eq!(source["details"]["kind"], "derived");
    assert_eq!(
        source["details"]["source"],
        serde_json::to_value(&pin.source).unwrap()
    );
    assert_eq!(source["face"]["id"], pin.source.face.id);
    let repeated = host.call(
        "opencut_font_source",
        serde_json::json!({"catalog_id":pin.source.face.id}),
    );
    assert!(repeated.error.is_none());
    assert_eq!(repeated.data.unwrap(), source);
    let request = ImportFont {
        project_id: "p".into(),
        expected_revision: snapshot.revision.clone(),
        idempotency_key: "original-font".into(),
        catalog_id: pin.source.face.id.clone(),
        font_id: "font".into(),
        required_text: "A".into(),
    };
    let before = host
        .call(
            "opencut_project_history_navigation",
            serde_json::json!({"project_id":"p","revision":null}),
        )
        .data;
    let mut unknown = request.clone();
    unknown.catalog_id = "unapproved-font-source".into();
    assert_eq!(
        host.call(
            "opencut_font_import",
            serde_json::to_value(unknown).unwrap()
        )
        .error
        .unwrap()
        .code,
        "NOT_FOUND"
    );
    let mut stale = request.clone();
    stale.expected_revision = "wrong-revision".into();
    assert_eq!(
        host.call("opencut_font_import", serde_json::to_value(stale).unwrap())
            .error
            .unwrap()
            .code,
        "REVISION_CONFLICT"
    );
    assert_eq!(
        before,
        host.call(
            "opencut_project_history_navigation",
            serde_json::json!({"project_id":"p","revision":null})
        )
        .data
    );
}
