use super::*;

#[test]
fn mask_geometry_and_jobs_dispatch_with_fixed_revision_and_original_authority() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = Automation::open(dir.path().join("workspace.sqlite")).unwrap();
    let before = create(&mut host);
    let base = json!({"project_id":"p","expected_revision":before["revision"],"sequence_id":"main","idempotency_key":"shape",
        "command":{"kind":"defaults","geometry":"rectangle","start":{"kind":"exact","time":{"ticks":"0","timebase":1}}}});
    let defaults = data(host.call("opencut_graphic_plan", base.clone()));
    let initial = &defaults["initial"];
    let mut request = base;
    request["command"] = json!({"kind":"create","element_id":"rect","track":{"kind":"new","track_id":"graphics"},"values":{
        "name":"Rectangle","graphic":initial["graphic"],"transform":initial["transform"],"opacity":initial["opacity"],
        "start":{"kind":"exact","time":initial["timeline_range"]["start"]},"duration":{"kind":"exact","time":initial["timeline_range"]["duration"]}}});
    let planned = data(host.call("opencut_graphic_plan", request));
    data(host.call("opencut_edit_apply", planned["transaction"].clone()));
    let current = inspect(&mut host);
    let geometry = json!({"project_id":"p","expected_revision":current["revision"],"sequence_id":"main","element_id":"rect","time":{"ticks":"0","timebase":1}});
    let logical = data(host.call("opencut_mask_geometry", geometry.clone()));
    assert_eq!(logical["revision"], current["revision"]);
    assert_eq!(inspect(&mut host), current, "geometry is read only");
    no_locator(&logical, dir.path());
    let mut element = current["project"]["sequences"][0]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "graphics")
        .unwrap()["elements"][0]
        .clone();
    element["visual"]["masks"] = json!([{"id":"mask","enabled":true,"shape":{"kind":"rectangle","geometry":logical["default_geometry"]},
        "inverted":false,"feather":{"unit":"classic_pixels","value":12},
        "stroke":{"width_px":0,"align":"inside","color":{"encoding":"srgb","red":0,"green":0,"blue":0,"alpha":1}}}]);
    let receipt = apply(
        &mut host,
        "mask",
        vec![
            json!({"type":"element_put","sequence_id":"main","track_id":"graphics","element":element}),
        ],
    );
    error(
        host.call("opencut_mask_geometry", geometry.clone()),
        "REVISION_CONFLICT",
    );
    let frame = json!({"kind":"frame","request":{"project_id":"p","revision":receipt["revision"],"sequence_id":"main","time":{"ticks":"0","timebase":1},"idempotency_key":"mask-png"}});
    let video = json!({"kind":"video","request":{"project_id":"p","revision":receipt["revision"],"sequence_id":"main","idempotency_key":"mask-video","options":{"format":"mp4","quality":"high","include_audio":false}}});
    for (tool, input, kind) in [
        ("opencut_render_submit", frame.clone(), "render.frame"),
        ("opencut_render_submit", video.clone(), "render.video"),
    ] {
        let job = data(host.call(tool, input.clone()));
        assert_eq!(job["kind"], kind);
        assert_eq!(
            host.app
                .get_job(job["job_id"].as_str().unwrap())
                .unwrap()
                .kind,
            if kind == "render.frame" {
                "render.project_scene_frame.v4"
            } else {
                "render.export_video.v2"
            }
        );
        assert_eq!(job["target"]["revision"], receipt["revision"]);
        assert_eq!(data(host.call(tool, input)), job);
        no_locator(&job, dir.path());
    }
    policy(&mut host, "0", vec![ProjectAction::Read]);
    let credential = host.app.issue_credential("agent").unwrap();
    host.caller = host.app.authenticate(&credential.secret).unwrap();
    let mut geometry = geometry;
    geometry["expected_revision"] = receipt["revision"].clone();
    error(
        host.call("opencut_mask_geometry", geometry),
        "NOT_AUTHORIZED",
    );
    error(host.call("opencut_render_submit", frame), "NOT_AUTHORIZED");
    error(host.call("opencut_render_submit", video), "NOT_AUTHORIZED");
}
