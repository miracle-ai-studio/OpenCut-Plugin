use super::*;

use opencut_contracts::scene::BlendMode;
fn fixture() -> Fixture {
    let mut f = graphic_tests::graphic_fixture();
    let snapshot = f
        .app
        .inspect_scene_project(&InspectProject {
            project_id: "frame-project".into(),
            revision: None,
        })
        .unwrap();
    let track = snapshot.project.sequences[0]
        .tracks
        .iter()
        .find(|t| t.id == "graphics")
        .unwrap();
    let mut element = track.elements[0].clone();
    element.visual.as_mut().unwrap().blend_mode = BlendMode::Hue;
    element.visual.as_mut().unwrap().masks.push(serde_json::from_value(serde_json::json!({
        "id":"mask", "enabled":true, "inverted":false,
        "shape":{"kind":"ellipse","geometry":{"center_x":0.1,"center_y":-0.1,"width":0.6,"height":0.4,"rotation_degrees":23.0,"scale":3.0}},
        "feather":{"unit":"classic_pixels","value":8.0},
        "stroke":{"color":{"encoding":"srgb","red":1.0,"green":1.0,"blue":1.0,"alpha":1.0},"width_px":0.0,"align":"center"}
    })).unwrap());
    f.revision = f
        .app
        .apply_scene_project(&ApplySceneProject {
            project_id: "frame-project".into(),
            expected_revision: f.revision.clone(),
            idempotency_key: "mask-edit".into(),
            label: "Hue".into(),
            operations: vec![ProjectOperation::ElementPut {
                sequence_id: "scene".into(),
                track_id: "graphics".into(),
                element: Box::new(element),
            }],
        })
        .unwrap()
        .revision;
    f
}
#[test]
fn mask_frame_pins_original_request_across_reopen_and_rejects_old_blend_profile() {
    let mut f = fixture();
    let request = f.request("mask-pinned");
    let job = start_mask_frame_png(&mut f.app, request.clone()).unwrap();
    assert_eq!(job.kind, "render.project_mask_frame.v4");
    let fixed: PreparedTextFramePng = serde_json::from_value(job.input.clone()).unwrap();
    assert!(
        opencut_editor_application::scene_mask_frame_jobs::validate_prepared(&fixed)
            .unwrap()
            .masks()
            .iter()
            .any(|entry| entry.mask.is_some())
    );
    let mut conflicting = request.clone();
    conflicting.time.ticks = "2".into();
    let before = counts(&f.app);
    assert!(start_mask_frame_png(&mut f.app, conflicting).is_err());
    assert_eq!(counts(&f.app), before);
    let old = f.request("old-normal");
    assert!(start_blend_frame_png(&mut f.app, old).is_err());
    f.app
        .apply_scene_project(&ApplySceneProject {
            project_id: "frame-project".into(),
            expected_revision: f.revision.clone(),
            idempotency_key: "later".into(),
            label: "Later".into(),
            operations: vec![ProjectOperation::Rename {
                name: "Later".into(),
            }],
        })
        .unwrap();
    let mut reopened =
        EditorApplication::open(f.directory.path().join("workspace.sqlite")).unwrap();
    assert_eq!(start_mask_frame_png(&mut reopened, request).unwrap(), job);
    assert_eq!(
        reopened
            .job_view(&job.job_id)
            .unwrap()
            .target
            .unwrap()
            .revision,
        f.revision
    );
}

#[test]
fn mask_frame_stored_result_binds_mask_scope_hash_and_private_artifact() {
    use opencut_editor_application::scene_mask_frame_jobs as mask;
    use opencut_job_contracts::{
        ArtifactDescriptor,
        media_frame::{MediaFrameArtifactMetadata, MediaFrameExecutionSummary},
    };
    let mut f = fixture();
    let request = f.request("mask-result");
    let job = start_mask_frame_png(&mut f.app, request).unwrap();
    let input: PreparedTextFramePng = serde_json::from_value(job.input.clone()).unwrap();
    let frame = mask::validate_prepared(&input).unwrap();
    let digest = "a".repeat(64);
    let stored = mask::StoredMaskFramePngResult {
        public: mask::MaskFramePngResult {
            schema_version: 1,
            identity: mask::public_identity(&frame),
            asset_lock: input.asset_lock.clone(),
            font_lock: input.font_lock.clone(),
            render_input_sha256: mask::render_input_sha256(
                &input.snapshot,
                &input.sequence_id,
                &input.time,
            )
            .unwrap(),
            execution: MediaFrameExecutionSummary {
                host_sha256: digest.clone(),
                runtime: opencut_job_contracts::media_frame::VisualRuntimeIdentity::HistoricalBrowser(opencut_job_contracts::media_frame::HistoricalBrowserVisualIdentity {
browser_version: "test-browser".into(),
                browser_sha256: digest.clone(),
                module_sha256: digest.clone(),
                shader_sha256: digest.clone(),
                playwright_version: "1.63.0".into(),
                mediabunny_version: "1.41.0".into(),
                gpu_backend: "webgpu".into(),
                gpu_device: "test-gpu".into(),
                texture_format: "bgra8unorm".into(),
                decoder_profile:
                    opencut_editor_application::media_frame_jobs::CLASSIC_VISUAL_DECODER_PROFILE
                        .into(),
            }),
},
            sources: vec![],
            texts: vec![],
            captions: vec![],
            // The shared fixture is the default 512-square polygon with no stroke.
            graphics: vec![opencut_contracts::graphic_frame::GraphicRasterReceipt {
                element_id: "polygon".into(),
                width: 512,
                height: 512,
                origin_x: 0,
                origin_y: 0,
            }],
            blends: frame.blends().to_vec(),
            masks: frame.masks().to_vec(),
            artifact: MediaFrameArtifactMetadata {
                mime_type: "image/png".into(),
                size_bytes: "100".into(),
                content_hash: digest.clone(),
            },
        },
        artifact: ArtifactDescriptor {
            path: "/private/attempt/mask.png".into(),
            mime_type: "image/png".into(),
            size: 100,
            content_hash: digest,
        },
    };
    let public = mask::validate_stored_result(&input, &stored).unwrap();
    assert!(
        !serde_json::to_string(&public)
            .unwrap()
            .contains("/private/")
    );
    for case in 0..7 {
        let mut changed = stored.clone();
        match case {
            0 => changed.public.masks[0].mask = None,
            1 => changed.public.masks[0].element_id = "other".into(),
            2 => changed.public.identity.revision = "other".into(),
            3 => {
                changed.public.render_input_sha256 =
                    opencut_editor_application::scene_blend_frame_jobs::render_input_sha256(
                        &input.snapshot,
                        &input.sequence_id,
                        &input.time,
                    )
                    .unwrap()
            }
            4 => changed.public.graphics[0].origin_x += 1,
            5 => changed.artifact.size += 1,
            _ => changed.public.blends.clear(),
        }
        assert!(
            mask::validate_stored_result(&input, &changed).is_err(),
            "case {case}"
        );
    }
    let mut wrong_profile = serde_json::to_value(stored).unwrap();
    wrong_profile["public"]["identity"]["profile"] =
        serde_json::json!("classic-v31-gpu-blend-frame-v1");
    assert!(serde_json::from_value::<mask::StoredMaskFramePngResult>(wrong_profile).is_err());
}
#[test]
fn mask_frame_retains_original_credential_and_cancel_publication_fences() {
    for revoked in [false, true] {
        let mut f = fixture();
        let (caller, credential) = f.credential();
        f.app.set_caller(caller);
        let request = f.request("mask-fenced");
        let job = start_mask_frame_png(&mut f.app, request).unwrap();
        let running = f.claim(&job);
        let input: PreparedTextFramePng = serde_json::from_value(running.input.clone()).unwrap();
        f.app
            .revalidate_scene_mask_frame_job(
                &job.job_id,
                "frame-worker",
                running.attempt,
                &input.authority,
            )
            .unwrap();
        assert!(
            f.app
                .revalidate_scene_blend_frame_job(
                    &job.job_id,
                    "frame-worker",
                    running.attempt,
                    &input.authority
                )
                .is_err()
        );
        for (worker, attempt, authority) in [
            ("different-worker", running.attempt, input.authority.clone()),
            ("frame-worker", running.attempt + 1, input.authority.clone()),
            (
                "frame-worker",
                running.attempt,
                TrustedJobAuthority::LocalOwner,
            ),
        ] {
            assert!(
                f.app
                    .revalidate_scene_mask_frame_job(&job.job_id, worker, attempt, &authority)
                    .is_err()
            );
        }
        if revoked {
            f.app.revoke_credential(&credential).unwrap();
            let before = counts(&f.app);
            assert_eq!(
                f.app
                    .complete_job(
                        &job.job_id,
                        "frame-worker",
                        running.attempt,
                        now(),
                        Value::Null
                    )
                    .unwrap_err()
                    .code,
                "NOT_AUTHORIZED"
            );
            assert_eq!(counts(&f.app), before);
            assert_eq!(f.app.get_job(&job.job_id).unwrap(), running);
        } else {
            f.app.cancel_job(&job.job_id).unwrap();
            let done = f
                .app
                .complete_job(
                    &job.job_id,
                    "frame-worker",
                    running.attempt,
                    now(),
                    Value::Null,
                )
                .unwrap();
            assert_eq!(done.state, JobState::Cancelled);
            assert!(
                done.result.is_none() && f.app.job_view(&job.job_id).unwrap().artifacts.is_empty()
            );
        }
    }
}
