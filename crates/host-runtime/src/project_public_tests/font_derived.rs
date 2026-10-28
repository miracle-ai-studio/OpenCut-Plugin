use super::*;

fn derived_request(host: &mut Automation, snapshot: &Value) -> Value {
    let catalog = data(host.call("opencut_font_catalog", json!({})));
    let face = catalog["fonts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|face| face["kind"] == "derived")
        .expect("approved derived source");
    json!({"project_id":"p","expected_revision":snapshot["revision"],"idempotency_key":"derived-font","catalog_id":face["id"],"font_id":"derived","required_text":"AV To"})
}

#[test]
fn font_derived_metadata_and_recovery_dispatch_preserve_fixed_request_without_loading() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = Automation::open(dir.path().join("workspace.sqlite")).unwrap();
    let before = create(&mut host);
    let request = derived_request(&mut host, &before);
    let source = data(host.call(
        "opencut_catalog",
        json!({"action":"font.source","input":{"catalog_id":request["catalog_id"]}}),
    ));
    assert_eq!(source["face"]["id"], request["catalog_id"]);
    assert_eq!(source["details"]["kind"], "derived");
    assert_eq!(
        source["details"]["source"]["source_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_ne!(
        source["details"]["source"]["source_sha256"],
        source["face"]["content_hash"]
    );
    no_locator(&source, dir.path());
    error(
        host.call(
            "opencut_font_source",
            json!({"catalog_id":"https://arbitrary.test/font.ttf"}),
        ),
        "NOT_FOUND",
    );
    let staged = data(host.call("opencut_edit", json!({"action":"recovery.stage","input":{"context":{"intent":{"kind":"font_import","request":request},"sequence_id":"main"}}})));
    assert_eq!(staged["context"]["intent"]["request"], request);
    assert!(staged["receipt"].is_null());
    assert_eq!(inspect(&mut host), before);
    assert_eq!(
        data(host.call(
            "opencut_edit_recovery_inspect",
            json!({"project_id":"p","recovery_id":staged["recovery_id"]})
        ))["record"],
        staged
    );
    assert_eq!(
        data(host.call(
            "opencut_edit_recovery_inspect",
            json!({"project_id":"p","recovery_id":null})
        ))["record"],
        staged
    );
    // Advancing the head proves this uncommitted fixed request can no longer
    // publish; acknowledgement never runs its Python provider or imports a font.
    apply(&mut host, "advance", vec![marker("other")]);
    let advanced = inspect(&mut host);
    let ack = data(host.call(
        "opencut_edit_recovery_ack",
        json!({"project_id":"p","recovery_id":staged["recovery_id"]}),
    ));
    assert_eq!(ack["resolution"], "not_saved");
    assert_eq!(inspect(&mut host), advanced);
    assert!(
        data(host.call(
            "opencut_edit_recovery_inspect",
            json!({"project_id":"p","recovery_id":null})
        ))["record"]
            .is_null()
    );
}

#[test]
fn font_derived_import_stage_and_fixed_proof_enforce_current_public_authority() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = Automation::open(dir.path().join("workspace.sqlite")).unwrap();
    let before = create(&mut host);
    let request = derived_request(&mut host, &before);
    policy(&mut host, "0", vec![ProjectAction::Read]);
    let credential = host.app.issue_credential("agent").unwrap();
    host.caller = host.app.authenticate(&credential.secret).unwrap();
    error(
        host.call("opencut_font_import", request.clone()),
        "NOT_AUTHORIZED",
    );
    error(
        host.call(
            "opencut_edit_recovery_stage",
            json!({"context":{"intent":{"kind":"font_import","request":request},"sequence_id":"main"}}),
        ),
        "NOT_AUTHORIZED",
    );
    assert_eq!(inspect(&mut host), before);
    policy(&mut host, "1", vec![]);
    host.caller = host.app.authenticate(&credential.secret).unwrap();
    error(
        host.call(
            "opencut_font_derived_proof",
            json!({"project_id":"p","revision":before["revision"],"font_id":"derived"}),
        ),
        "NOT_AUTHORIZED",
    );
}
