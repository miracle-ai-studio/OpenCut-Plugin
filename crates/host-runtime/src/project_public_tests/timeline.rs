//! Public fixed-head planning and atomic application. The parent's synthetic
//! CAS bytes exercise editing/resource identity only, never media decoding.
use super::*;

fn request(snapshot: &Value, key: &str, command: Value) -> Value {
    json!({"project_id":"p","expected_revision":snapshot["revision"],
        "sequence_id":"main","idempotency_key":key,"label":key,"command":command})
}
fn plan(automation: &mut Automation, input: Value) -> Value {
    data(automation.call("opencut_edit_timeline_plan", input))
}
fn split(track_id: &str, at: &str) -> Value {
    json!({"kind":"split","targets":[{"target":{"track_id":track_id,"element_id":"video"},
        "right_element_id":"right"}],"at":{"ticks":at,"timebase":1},"retain":"both","ripple":false})
}
fn movement(track_id: &str, element_id: &str, start: &str) -> Value {
    json!({"target":{"track_id":track_id,"element_id":element_id},"target_track_id":track_id,
        "new_start":{"ticks":start,"timebase":1}})
}
fn element<'a>(snapshot: &'a Value, id: &str) -> &'a Value {
    snapshot["project"]["sequences"][0]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|track| track["elements"].as_array().unwrap())
        .find(|element| element["id"] == id)
        .unwrap()
}
fn seconds(value: &Value, expected: i64) {
    let actual: opencut_contracts::Time = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        opencut_time::RationalTime::try_from(&actual)
            .unwrap()
            .cmp_exact(opencut_time::RationalTime::new(expected, 1).unwrap()),
        std::cmp::Ordering::Equal
    );
}

#[test]
fn timeline_public_and_domain_plans_share_one_profile_and_recheck_edit_authority() {
    let directory = tempfile::tempdir().unwrap();
    let mut automation = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    create(&mut automation);
    let fixed = inspect(&mut automation);
    let input = request(
        &fixed,
        "text-track",
        json!({
            "kind":"add_track", "id":"titles", "name":"Titles", "track_kind":"text"
        }),
    );
    let domain = data(automation.call(
        "opencut_edit",
        json!({"action":"timeline.plan","input":input.clone()}),
    ));
    let tool = "opencut_edit_timeline_plan";
    let planned = data(automation.call(tool, input.clone()));
    assert_eq!(domain, planned);
    assert_eq!(data(automation.call(tool, input.clone())), planned);
    assert_eq!(inspect(&mut automation), fixed);
    assert_eq!(planned["decision"]["created_ids"], json!(["titles"]));
    policy(&mut automation, "0", vec![ProjectAction::Read]);
    let issued = automation.app.issue_credential("agent").unwrap();
    let caller = automation.app.authenticate(&issued.secret).unwrap();
    automation.caller = caller.clone();
    error(automation.call(tool, input.clone()), "NOT_AUTHORIZED");
    error(
        automation.call("opencut_edit_apply", planned["transaction"].clone()),
        "NOT_AUTHORIZED",
    );
    assert_eq!(inspect(&mut automation), fixed);
    policy(
        &mut automation,
        "1",
        vec![ProjectAction::Read, ProjectAction::Edit],
    );
    automation.caller = caller;
    assert_eq!(data(automation.call(tool, input)), planned);
    let receipt = data(automation.call("opencut_edit_apply", planned["transaction"].clone()));
    assert_eq!(
        data(automation.call("opencut_edit_apply", planned["transaction"].clone())),
        receipt
    );
    let after = inspect(&mut automation);
    assert_eq!(
        after["project"]["sequences"][0]["tracks"][1]["kind"],
        "text"
    );
    assert_eq!(after["project"]["assets"], fixed["project"]["assets"]);
    assert_eq!(after["project"]["fonts"], fixed["project"]["fonts"]);
}

#[test]
fn timeline_split_public_plan_is_read_only_replayable_and_restores_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workspace.sqlite");
    let mut automation = Automation::open(&database).unwrap();
    let (locator, track_id) = seed_source_video(&mut automation, directory.path());
    let fixed = inspect(&mut automation);
    let input = request(&fixed, "split-public", split(&track_id, "1"));
    let planned = plan(&mut automation, input.clone());
    assert_eq!(plan(&mut automation, input), planned);
    assert_eq!(inspect(&mut automation), fixed);
    no_locator(&planned, &locator);
    assert_eq!(planned["decision"]["created_ids"], json!(["right"]));
    let committed = data(automation.call("opencut_edit_apply", planned["transaction"].clone()));
    assert_eq!(
        data(automation.call("opencut_edit_apply", planned["transaction"].clone())),
        committed
    );
    let after = inspect(&mut automation);
    let left = element(&after, "video");
    let right = element(&after, "right");
    // Original timeline [0,2), visible source [1,3), full extent 4.
    // Splitting at 1 produces independent source windows [1,2) and [2,3).
    for (part, start, trim_start, trim_end) in [(left, 0, 1, 2), (right, 1, 2, 1)] {
        seconds(&part["timeline_range"]["start"], start);
        seconds(&part["timeline_range"]["duration"], 1);
        assert_eq!(part["source_span"]["profile"], "classic_trimmed");
        seconds(&part["source_span"]["trim_start"], trim_start);
        seconds(&part["source_span"]["trim_end"], trim_end);
        seconds(&part["source_span"]["source_duration"], 4);
        assert_eq!(part["content"], element(&fixed, "video")["content"]);
        assert_eq!(part["visual"], element(&fixed, "video")["visual"]);
    }
    assert_eq!(after["project"]["assets"], fixed["project"]["assets"]);
    drop(automation);
    let mut automation = Automation::open(&database).unwrap();
    assert_eq!(inspect(&mut automation), after);
    let undo = undo_input(&mut automation, &committed["transaction_id"], "undo-split");
    let undone = data(automation.call("opencut_edit_undo_apply", undo));
    assert_eq!(inspect(&mut automation)["project"], fixed["project"]);
    let current = inspect(&mut automation);
    let redo = json!({"project_id":"p","expected_revision":current["revision"],
        "undo_transaction_id":undone["transaction"]["transaction_id"],"idempotency_key":"redo-split"});
    let redone = data(automation.call("opencut_edit_redo_apply", redo));
    assert_eq!(inspect(&mut automation)["project"], after["project"]);
    no_locator(&redone, &locator);
}

#[test]
fn timeline_simultaneous_swap_rejects_collision_and_locked_source() {
    let directory = tempfile::tempdir().unwrap();
    let mut automation = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    let (locator, track_id) = seed_source_video(&mut automation, directory.path());
    let fixed = inspect(&mut automation);
    let halves = plan(
        &mut automation,
        request(&fixed, "prepare-halves", split(&track_id, "1")),
    );
    data(automation.call("opencut_edit_apply", halves["transaction"].clone()));
    let before = inspect(&mut automation);
    // Each individual move would overlap its neighbor; the complete swap is
    // legal only when both changes are validated and committed together.
    let swap = json!({"kind":"move_selection","moves":[movement(&track_id,"video","1"),
        movement(&track_id,"right","0")],"ripple":false});
    let swapped = plan(&mut automation, request(&before, "swap-halves", swap));
    assert_eq!(inspect(&mut automation), before);
    data(automation.call("opencut_edit_apply", swapped["transaction"].clone()));
    let after = inspect(&mut automation);
    seconds(&element(&after, "video")["timeline_range"]["start"], 1);
    seconds(&element(&after, "right")["timeline_range"]["start"], 0);
    for id in ["video", "right"] {
        assert_eq!(
            element(&after, id)["source_span"],
            element(&before, id)["source_span"]
        );
    }
    let collision = json!({"kind":"move_selection","moves":[movement(&track_id,"video","0"),
        movement(&track_id,"right","0")],"ripple":false});
    error(
        automation.call(
            "opencut_edit_timeline_plan",
            request(&after, "collision", collision),
        ),
        "INVALID_PROJECT",
    );
    assert_eq!(inspect(&mut automation), after);
    let lock = plan(
        &mut automation,
        request(
            &after,
            "lock-main",
            json!({"kind":"set_track_locked",
        "track_id":track_id,"locked":true}),
        ),
    );
    data(automation.call("opencut_edit_apply", lock["transaction"].clone()));
    let locked = inspect(&mut automation);
    // Even an unchanged move must validate its explicit source lock.
    let same =
        json!({"kind":"move_selection","moves":[movement(&track_id,"right","0")],"ripple":false});
    error(
        automation.call(
            "opencut_edit_timeline_plan",
            request(&locked, "locked-noop", same),
        ),
        "CONSTRAINT_VIOLATION",
    );
    assert_eq!(inspect(&mut automation), locked);
    no_locator(&swapped, &locator);
}

#[test]
fn timeline_planning_and_apply_recheck_restricted_authority() {
    let directory = tempfile::tempdir().unwrap();
    let mut automation = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    let (locator, track_id) = seed_source_video(&mut automation, directory.path());
    policy(&mut automation, "0", vec![ProjectAction::Read]);
    let issued = automation.app.issue_credential("agent").unwrap();
    let caller = automation.app.authenticate(&issued.secret).unwrap();
    automation.caller = caller.clone();
    let fixed = inspect(&mut automation);
    let input = request(&fixed, "restricted-split", split(&track_id, "1"));
    error(
        automation.call("opencut_edit_timeline_plan", input.clone()),
        "NOT_AUTHORIZED",
    );
    assert_eq!(inspect(&mut automation), fixed);
    policy(
        &mut automation,
        "1",
        vec![ProjectAction::Read, ProjectAction::Edit],
    );
    automation.caller = caller.clone();
    let planned = plan(&mut automation, input.clone());
    no_locator(&planned, &locator);
    assert_eq!(inspect(&mut automation), fixed);
    policy(&mut automation, "2", vec![ProjectAction::Read]);
    automation.caller = caller.clone();
    error(
        automation.call("opencut_edit_apply", planned["transaction"].clone()),
        "NOT_AUTHORIZED",
    );
    error(
        automation.call("opencut_edit_timeline_plan", input.clone()),
        "NOT_AUTHORIZED",
    );
    assert_eq!(inspect(&mut automation), fixed);
    policy(
        &mut automation,
        "3",
        vec![ProjectAction::Read, ProjectAction::Edit],
    );
    automation.caller = caller.clone();
    let committed = data(automation.call("opencut_edit_apply", planned["transaction"].clone()));
    assert_eq!(
        data(automation.call("opencut_edit_apply", planned["transaction"].clone())),
        committed
    );
    policy(&mut automation, "4", vec![]);
    automation.caller = caller.clone();
    error(
        automation.call("opencut_edit_timeline_plan", input.clone()),
        "NOT_AUTHORIZED",
    );
    error(
        automation.call("opencut_edit_apply", planned["transaction"].clone()),
        "NOT_AUTHORIZED",
    );
    policy(
        &mut automation,
        "5",
        vec![ProjectAction::Read, ProjectAction::Edit],
    );
    automation
        .app
        .revoke_credential(&issued.credential_id)
        .unwrap();
    automation.caller = caller;
    error(
        automation.call("opencut_edit_timeline_plan", input),
        "NOT_AUTHORIZED",
    );
    error(
        automation.call("opencut_edit_apply", planned["transaction"].clone()),
        "NOT_AUTHORIZED",
    );
}

#[test]
fn timeline_noop_is_nullable_and_stale_plan_or_apply_conflicts() {
    let directory = tempfile::tempdir().unwrap();
    let mut automation = Automation::open(directory.path().join("workspace.sqlite")).unwrap();
    let (locator, track_id) = seed_source_video(&mut automation, directory.path());
    let fixed = inspect(&mut automation);
    let endpoint = plan(
        &mut automation,
        request(&fixed, "split-endpoint", split(&track_id, "2")),
    );
    assert!(endpoint["transaction"].is_null());
    assert_eq!(endpoint["decision"]["created_ids"], json!([]));
    let unchanged = plan(
        &mut automation,
        request(
            &fixed,
            "enabled-noop",
            json!({"kind":"set_track_enabled",
        "track_id":track_id,"enabled":true}),
        ),
    );
    assert!(unchanged["transaction"].is_null());
    assert_eq!(inspect(&mut automation), fixed);
    let input = request(&fixed, "split-stale", split(&track_id, "1"));
    let planned = plan(&mut automation, input.clone());
    apply(
        &mut automation,
        "intervening-edit",
        vec![marker("intervening")],
    );
    let changed = inspect(&mut automation);
    error(
        automation.call("opencut_edit_timeline_plan", input),
        "REVISION_CONFLICT",
    );
    error(
        automation.call("opencut_edit_apply", planned["transaction"].clone()),
        "REVISION_CONFLICT",
    );
    assert_eq!(inspect(&mut automation), changed);
    no_locator(&endpoint, &locator);
    no_locator(&planned, &locator);
}
