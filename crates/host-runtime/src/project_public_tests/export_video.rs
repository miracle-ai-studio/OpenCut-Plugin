//! Public dispatch/queue tests only. Native codec and pixel evidence is separate.
use super::*;

fn shape(host: &mut Automation, scene_effect: bool) -> Value {
    let before = create(host);
    let mut request = json!({"project_id":"p","expected_revision":before["revision"],"sequence_id":"main","idempotency_key":"shape",
        "command":{"kind":"defaults","geometry":"rectangle","start":{"kind":"exact","time":{"ticks":"0","timebase":1}}}});
    let defaults = data(host.call("opencut_graphic_plan", request.clone()));
    let initial = &defaults["initial"];
    request["command"] = json!({"kind":"create","element_id":"shape","track":{"kind":"new","track_id":"graphics"},"values":{
        "name":"Shape","graphic":initial["graphic"],"transform":initial["transform"],"opacity":initial["opacity"],
        "start":{"kind":"exact","time":{"ticks":"0","timebase":1}},"duration":{"kind":"exact","time":{"ticks":"1","timebase":5}}}});
    let planned = data(host.call("opencut_graphic_plan", request));
    let receipt = data(host.call("opencut_edit_apply", planned["transaction"].clone()));
    if !scene_effect {
        return receipt;
    }
    let current = inspect(host);
    let mut element = current["project"]["sequences"][0]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "graphics")
        .unwrap()["elements"][0]
        .clone();
    element["visual"]["effects"] =
        json!([{"id":"blur","enabled":true,"effect":{"kind":"classic_blur","intensity":12}}]);
    apply(
        host,
        "effect",
        vec![
            json!({"type":"element_put","sequence_id":"main","track_id":"graphics","element":element}),
        ],
    )
}

#[test]
fn configurable_export_public_and_domain_dispatch_keep_fixed_options_and_authority() {
    let directory = tempfile::tempdir().unwrap();
    let mut host = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    let revision = shape(&mut host, false)["revision"].clone();
    let request = json!({"kind":"video","request":{"project_id":"p","revision":revision,"sequence_id":"main","idempotency_key":"video-v2",
        "options":{"format":"webm","quality":"very_high","include_audio":false}}});
    let job = data(host.call("opencut_render", json!({"action":"submit","input":request})));
    assert_eq!(job["kind"], "render.video");
    assert_eq!(job["target"]["revision"], revision);
    assert_eq!(
        data(host.call("opencut_render_submit", request.clone())),
        job
    );
    no_locator(&job, directory.path());
    let private = host.app.get_job(job["job_id"].as_str().unwrap()).unwrap();
    assert_eq!(private.kind, "render.export_video.v2");
    assert_eq!(private.input["options"], request["request"]["options"]);
    let mut changed = request.clone();
    changed["request"]["options"]["quality"] = json!("low");
    error(
        host.call("opencut_render_submit", changed),
        "IDEMPOTENCY_KEY_REUSED",
    );
    let capability = data(host.call("opencut_render", json!({"action":"capability","input":{}})));
    assert!(capability["frame"]["available"].is_boolean());
    assert!(capability["video"]["available"].is_boolean());
    assert_eq!(capability["submit_tool"], "opencut_render_submit");
    policy(&mut host, "0", vec![ProjectAction::Read]);
    let credential = host.app.issue_credential("agent").unwrap();
    host.caller = host.app.authenticate(&credential.secret).unwrap();
    error(
        host.call("opencut_render_submit", request),
        "NOT_AUTHORIZED",
    );
}

#[test]
fn scene_png_video_and_configurable_export_select_the_complete_fixed_scene() {
    let directory = tempfile::tempdir().unwrap();
    let mut host = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    let revision = shape(&mut host, true)["revision"].clone();
    let frame = json!({"kind":"frame","request":{"project_id":"p","revision":revision,"sequence_id":"main","idempotency_key":"scene-frame","time":{"ticks":"0","timebase":1}}});
    let video = json!({"kind":"video","request":{"project_id":"p","revision":revision,"sequence_id":"main","idempotency_key":"scene-video","options":{"format":"mp4","quality":"high","include_audio":false}}});
    for (tool, request, kind) in [
        ("opencut_render_submit", frame.clone(), "render.frame"),
        ("opencut_render_submit", video.clone(), "render.video"),
    ] {
        let job = data(host.call(tool, request.clone()));
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
        assert_eq!(job["target"]["revision"], revision);
        assert_eq!(data(host.call(tool, request)), job);
        no_locator(&job, directory.path());
    }
    for retired in [
        "opencut_job_scene_frame_png",
        "opencut_job_scene_video_mp4",
        "opencut_job_mask_frame_png",
        "opencut_job_mask_video_mp4",
        "opencut_job_export_video",
    ] {
        error(host.call(retired, json!({})), "NOT_FOUND");
    }
    let request = json!({"kind":"video","request":{"project_id":"p","revision":revision,"sequence_id":"main","idempotency_key":"scene-export",
        "options":{"format":"mp4","quality":"medium","include_audio":false}}});
    let job = data(host.call("opencut_render_submit", request));
    let private = host.app.get_job(job["job_id"].as_str().unwrap()).unwrap();
    assert_eq!(private.kind, "render.export_video.v2");
    assert_eq!(private.input["visual_profile"], "scene_v1");
    policy(&mut host, "0", vec![ProjectAction::Read]);
    let credential = host.app.issue_credential("agent").unwrap();
    host.caller = host.app.authenticate(&credential.secret).unwrap();
    error(host.call("opencut_render_submit", frame), "NOT_AUTHORIZED");
    error(host.call("opencut_render_submit", video), "NOT_AUTHORIZED");
}
