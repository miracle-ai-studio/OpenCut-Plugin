use super::*;
use opencut_contracts::{Asset, AssetKind, scene::*, scene_ops::*};
use opencut_editor_application::access::{PrincipalPolicy, ProjectAction, ProjectGrant};
use serde_json::json;
use std::collections::BTreeMap;
fn grant(host: &mut Automation, expected_revision: &str, actions: Vec<ProjectAction>) {
    host.app.set_execution_context(ExecutionContext::for_caller(
        crate::CallSource::Embedded,
        crate::Caller::LocalOwner,
    ));
    host.app
        .set_principal_policy(PrincipalPolicy {
            principal_id: "source-reader".into(),
            expected_revision: expected_revision.into(),
            enabled: true,
            grants: if actions.is_empty() {
                vec![]
            } else {
                vec![ProjectGrant {
                    project_id: "p".into(),
                    actions,
                }]
            },
        })
        .unwrap();
}

fn source(bytes: &[u8]) -> (tempfile::TempDir, Automation, SourceArtRequest) {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Automation::open(temp.path().join("editor.db")).unwrap();
    let response = host.call("opencut_project_create", json!({"project":{"id":"p","name":"Source art","sequence_id":"main","width":64,"height":48,"fps":{"numerator":25,"denominator":1}},"idempotency_key":"create"}));
    assert!(response.error.is_none(), "{:?}", response.error);
    let revision = response.data.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = temp.path().join("source.wav");
    std::fs::write(&path, bytes).unwrap();
    let asset = opencut_asset_store::AssetStore::open(host.app.asset_store_root())
        .unwrap()
        .pin_asset(&Asset {
            id: "source".into(),
            name: "Source".into(),
            kind: AssetKind::Audio,
            uri: path.to_string_lossy().into_owned(),
            duration: None,
            version: None,
            metadata: BTreeMap::new(),
        })
        .unwrap();
    host.app
        .apply_scene_project(&ApplySceneProject {
            project_id: "p".into(),
            expected_revision: revision,
            idempotency_key: "asset".into(),
            label: "Asset".into(),
            operations: vec![ProjectOperation::AssetPut {
                asset: ProjectAsset {
                    id: asset.id,
                    name: asset.name,
                    kind: asset.kind,
                    version: asset.version.unwrap(),
                    duration: None,
                    origin: ResourceOrigin::LegacyLocator { uri: asset.uri },
                    metadata: ProjectMediaMetadata {
                        mime_type: Some("audio/wav".into()),
                        intrinsic_size: None,
                        frame_rate: None,
                        rotation_degrees: None,
                        audio: AudioPresence::Present { metadata: None },
                    },
                    license: None,
                },
            }],
        })
        .unwrap();
    let fixed = host
        .app
        .inspect_scene_project(&InspectProject {
            project_id: "p".into(),
            revision: None,
        })
        .unwrap();
    (
        temp,
        host,
        SourceArtRequest {
            project_id: "p".into(),
            revision: fixed.revision,
            asset_id: "source".into(),
            request_id: "first".into(),
            query: SourceArtQuery::Waveform {
                sample_rate: 48000,
                projection: WaveformProjection::Source {
                    range: None,
                    bins: 375,
                },
            },
        },
    )
}
fn identity() -> SourceArtIdentity {
    SourceArtIdentity::Browser(BrowserSourceArtIdentity {
        algorithm: SOURCE_PEAKS_ALGORITHM.into(),
        decode_profile: SOURCE_ART_DECODE_PROFILE.into(),
        browser_version: "153.0.8010.12".into(),
        browser_sha256: "b".repeat(64),
        node_sha256: "c".repeat(64),
        module_sha256: "d".repeat(64),
    })
}
fn decoded(size: usize) -> Arc<DecodedSourceArt> {
    Arc::new(DecodedSourceArt::Waveform {
        version: opencut_contracts::AssetVersion {
            id: "fixture".into(),
            content_hash: "0".repeat(64),
            size: 1,
        },
        identity: identity(),
        sample_rate: 48000,
        channels: 1,
        total_samples: size as u64 * 128,
        bucket_size: 128,
        amplitudes: vec![0.25; size],
    })
}
fn ticket(host: &Automation, request: &SourceArtRequest) -> Arc<Ticket> {
    Arc::new(Ticket::new(Arc::new(
        prepare_source_art(&host.app, request).unwrap(),
    )))
}
fn admission(registry: &Registry, key: &str, ticket: Arc<Ticket>) -> Admission {
    registry
        .admit(key.into(), TicketKey::new(&ticket.prepared), ticket)
        .unwrap()
}
fn flight(admission: Admission) -> (Arc<Flight>, bool) {
    let Admission::Flight { flight, start } = admission else {
        panic!("expected flight")
    };
    (flight, start)
}

#[test]
fn hundred_waiters_share_one_decode_and_cache_with_bounded_lru() {
    let (_temp, host, request) = source(b"source fixture");
    let registry = Registry::default();
    let first = ticket(&host, &request);
    let (shared, start) = flight(admission(&registry, "same-source", first.clone()));
    assert!(start);
    let mut tickets = vec![first];
    for index in 1..100 {
        let mut input = request.clone();
        input.request_id = index.to_string();
        let selected = ticket(&host, &input);
        let (joined, start) = flight(admission(&registry, "same-source", selected.clone()));
        assert!(!start);
        assert!(Arc::ptr_eq(&joined, &shared));
        tickets.push(selected);
    }
    registry
        .complete("same-source", &shared, Ok(decoded(375)))
        .unwrap();
    for selected in tickets {
        registry
            .release(&TicketKey::new(&selected.prepared), &selected, &shared)
            .unwrap();
    }
    assert_eq!(registry.stats().0, 1);
    let mut next = request.clone();
    next.request_id = "warm".into();
    assert!(matches!(
        admission(&registry, "same-source", ticket(&host, &next)),
        Admission::Cached(_)
    ));
    for index in 0..25 {
        let mut input = request.clone();
        input.request_id = format!("large-{index}");
        let selected = ticket(&host, &input);
        let key = format!("key-{index}");
        let (active, _) = flight(admission(&registry, &key, selected.clone()));
        registry
            .complete(&key, &active, Ok(decoded(524288)))
            .unwrap();
        registry
            .release(&TicketKey::new(&selected.prepared), &selected, &active)
            .unwrap();
    }
    let stats = registry.stats();
    assert!(stats.1 < 25);
    assert!(stats.2 <= 32 * 1024 * 1024);
    assert_eq!(stats.3, 0);
}
#[test]
fn scoped_cancel_before_get_and_last_waiter_cleanup_are_fenced() {
    let (_temp, host, request) = source(b"source fixture");
    let registry = Registry::default();
    let selected = ticket(&host, &request);
    let key = TicketKey::new(&selected.prepared);
    assert!(registry.cancel(key.clone()).unwrap().is_none());
    assert_eq!(
        registry
            .admit("source".into(), key, selected)
            .err()
            .unwrap()
            .code,
        "SOURCE_ART_CANCELLED"
    );
    let mut first = request.clone();
    first.request_id = "one".into();
    let first = ticket(&host, &first);
    let (shared, _) = flight(admission(&registry, "source", first.clone()));
    let mut second = request.clone();
    second.request_id = "two".into();
    let second = ticket(&host, &second);
    let (same, start) = flight(admission(&registry, "source", second.clone()));
    assert!(!start);
    assert!(Arc::ptr_eq(&same, &shared));
    assert!(
        registry
            .cancel(TicketKey::new(&first.prepared))
            .unwrap()
            .is_none()
    );
    assert!(shared.has_other_active(&first).unwrap());
    assert!(
        registry
            .cancel(TicketKey::new(&second.prepared))
            .unwrap()
            .is_some()
    );
    assert!(!shared.has_other_active(&second).unwrap());
    assert!(
        lock(&shared.result).unwrap().is_none(),
        "cancellation is not a cleanup receipt"
    );
    let mut third = request.clone();
    third.request_id = "three".into();
    let third = ticket(&host, &third);
    let third_key = TicketKey::new(&third.prepared);
    let Admission::Draining(draining) = admission(&registry, "source", third.clone()) else {
        panic!("replacement must wait for actual cleanup");
    };
    assert!(Arc::ptr_eq(&draining, &shared));
    assert!(!shared.has_other_active(&second).unwrap());
    assert!(third.failure().unwrap().is_none());
    assert_eq!(registry.stats().3, 1);
    registry
        .complete("source", &shared, Err(error("SOURCE_ART_CANCELLED")))
        .unwrap();
    assert_eq!(registry.stats(), (1, 0, 0, 0));
    wait_cleanup(&host.app, &draining, &third).unwrap();
    registry.release_cached(&third_key, &third).unwrap();
    let (replacement, start) = flight(admission(&registry, "source", third.clone()));
    assert!(start);
    assert!(!Arc::ptr_eq(&replacement, &shared));
    registry.cancel(third_key.clone()).unwrap();
    let mut fourth = request.clone();
    fourth.request_id = "cancelled-while-waiting".into();
    let fourth = ticket(&host, &fourth);
    let fourth_key = TicketKey::new(&fourth.prepared);
    assert!(matches!(
        admission(&registry, "source", fourth.clone()),
        Admission::Draining(_)
    ));
    assert!(registry.cancel(fourth_key.clone()).unwrap().is_none());
    assert_eq!(
        wait_cleanup(&host.app, &replacement, &fourth)
            .unwrap_err()
            .code,
        "SOURCE_ART_CANCELLED"
    );
    assert!(registry.release_cached(&fourth_key, &fourth).is_err());
    registry
        .complete("source", &replacement, Err(error("SOURCE_ART_CANCELLED")))
        .unwrap();
    assert!(registry.release(&third_key, &third, &replacement).is_err());
}
#[test]
fn decoder_slots_are_bounded_and_shutdown_drops_cached_content() {
    let (_temp, host, request) = source(b"source fixture");
    let registry = Registry::default();
    for index in 0..1 {
        let mut input = request.clone();
        input.request_id = index.to_string();
        assert!(
            flight(admission(
                &registry,
                &index.to_string(),
                ticket(&host, &input)
            ))
            .1
        );
    }
    let mut second = request.clone();
    second.request_id = "second".into();
    let second = ticket(&host, &second);
    assert!(matches!(
        admission(&registry, "second-source", second.clone()),
        Admission::Queued
    ));
    let flights = registry.shutdown().unwrap();
    assert_eq!(flights.len(), 1);
    for flight in flights {
        assert!(!flight.has_other_active(&ticket(&host, &request)).unwrap());
    }
    assert_eq!(
        await_admission(&host.app, &registry, "second-source", &second)
            .err()
            .unwrap()
            .code,
        "SOURCE_ART_STOPPED"
    );
    assert_eq!(registry.waiting(), 0);
    assert_eq!(registry.stats().2, 0);
}
#[test]
fn pinned_completion_payloads_stay_charged_and_rebuild_after_release() {
    let (_temp, host, request) = source(b"source fixture");
    let registry = Registry::default();
    let mut pinned = Vec::new();
    // Fifteen 2 MiB summaries fit; a sixteenth exceeds the unchanged 32 MiB
    // retained-payload budget once metadata is included.
    for index in 0..15 {
        let mut input = request.clone();
        input.request_id = format!("pinned-{index}");
        let selected = ticket(&host, &input);
        let key = format!("source-{index}");
        let (active, _) = flight(admission(&registry, &key, selected.clone()));
        registry
            .complete(&key, &active, Ok(decoded(524288)))
            .unwrap();
        registry
            .release(&TicketKey::new(&selected.prepared), &selected, &active)
            .unwrap();
        pinned.push(active);
    }
    let charged = registry.stats().2;
    let mut input = request.clone();
    input.request_id = "cannot-evict-borrowed".into();
    let selected = ticket(&host, &input);
    let (active, _) = flight(admission(&registry, "next-source", selected.clone()));
    registry
        .complete("next-source", &active, Ok(decoded(524288)))
        .unwrap();
    assert_eq!(
        wait(&host.app, &active, &selected).unwrap_err().code,
        "SOURCE_ART_BUDGET_EXCEEDED"
    );
    registry
        .release(&TicketKey::new(&selected.prepared), &selected, &active)
        .unwrap();
    assert_eq!(
        registry.stats().2,
        charged,
        "borrowed payloads cannot disappear from accounting"
    );
    assert!(charged <= 32 * 1024 * 1024);
    drop(pinned);

    input.request_id = "after-borrow-release".into();
    let selected = ticket(&host, &input);
    let (next, _) = flight(admission(&registry, "next-source", selected.clone()));
    registry
        .complete("next-source", &next, Ok(decoded(524288)))
        .unwrap();
    assert!(wait(&host.app, &next, &selected).is_ok());
    registry
        .release(&TicketKey::new(&selected.prepared), &selected, &next)
        .unwrap();
    input.request_id = "rebuild-evicted".into();
    let rebuild = ticket(&host, &input);
    assert!(flight(admission(&registry, "source-0", rebuild)).1);
    assert!(registry.stats().2 <= 32 * 1024 * 1024);
}

#[test]
fn cache_counts_allocated_capacity_and_evicts_at_256_entries() {
    let (_temp, host, request) = source(b"source fixture");
    let registry = Registry::default();
    let selected = ticket(&host, &request);
    let (active, _) = flight(admission(&registry, "capacity", selected.clone()));
    let mut oversized = decoded(0).as_ref().clone();
    let DecodedSourceArt::Waveform { amplitudes, .. } = &mut oversized else {
        unreachable!()
    };
    *amplitudes = Vec::with_capacity(32 * 1024 * 1024 / 4);
    amplitudes.push(0.25);
    registry
        .complete("capacity", &active, Ok(Arc::new(oversized)))
        .unwrap();
    assert_eq!(
        wait(&host.app, &active, &selected).unwrap_err().code,
        "SOURCE_ART_BUDGET_EXCEEDED"
    );
    registry
        .release(&TicketKey::new(&selected.prepared), &selected, &active)
        .unwrap();
    assert_eq!(registry.stats().2, 0);
    for index in 0..257 {
        let mut input = request.clone();
        input.request_id = format!("entry-{index}");
        let selected = ticket(&host, &input);
        let key = index.to_string();
        let (active, _) = flight(admission(&registry, &key, selected.clone()));
        registry.complete(&key, &active, Ok(decoded(1))).unwrap();
        registry
            .release(&TicketKey::new(&selected.prepared), &selected, &active)
            .unwrap();
    }
    assert_eq!(registry.stats().1, 256);
    let mut input = request.clone();
    input.request_id = "evicted-entry-rebuild".into();
    assert!(flight(admission(&registry, "0", ticket(&host, &input))).1);
}

#[test]
fn shutdown_waits_for_warm_ticket_and_its_payload_before_rebuilding() {
    let (_temp, host, request) = source(b"source fixture");
    let registry = Arc::new(Registry::default());
    let selected = ticket(&host, &request);
    let (active, _) = flight(admission(&registry, "source", selected.clone()));
    registry
        .complete("source", &active, Ok(decoded(1)))
        .unwrap();
    registry
        .release(&TicketKey::new(&selected.prepared), &selected, &active)
        .unwrap();
    drop(active);
    let mut input = request.clone();
    input.request_id = "warm-shutdown".into();
    let warm = ticket(&host, &input);
    let Admission::Cached(payload) = admission(&registry, "source", warm.clone()) else {
        unreachable!()
    };
    assert!(registry.shutdown().unwrap().is_empty());
    assert_eq!(
        registry
            .admit("other".into(), TicketKey::new(&selected.prepared), selected)
            .err()
            .unwrap()
            .code,
        "SOURCE_ART_STOPPED"
    );
    let worker = registry.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let join = std::thread::spawn(move || {
        done_tx.send(worker.drain()).unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
    assert_eq!(
        registry
            .release_cached(&TicketKey::new(&warm.prepared), &warm)
            .unwrap_err()
            .code,
        "SOURCE_ART_STOPPED"
    );
    assert!(
        done_rx.recv_timeout(Duration::from_millis(20)).is_err(),
        "the payload is still borrowed after ticket release"
    );
    drop(payload);
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    join.join().unwrap();
    assert_eq!(registry.stats(), (1, 0, 0, 0));
}

#[test]
fn warm_completion_rechecks_original_credential_before_projection() {
    let (_temp, mut host, request) = source(b"source fixture");
    grant(
        &mut host,
        "0",
        vec![ProjectAction::Read, ProjectAction::RunJobs],
    );
    let issued = host.app.issue_credential("source-reader").unwrap();
    let caller = host.app.authenticate(&issued.secret).unwrap();
    host.app.set_execution_context(ExecutionContext::for_caller(
        crate::CallSource::Embedded,
        caller,
    ));
    let registry = Registry::default();
    let selected = ticket(&host, &request);
    let (active, _) = flight(admission(&registry, "source", selected.clone()));
    registry
        .complete("source", &active, Ok(decoded(1)))
        .unwrap();
    registry
        .release(&TicketKey::new(&selected.prepared), &selected, &active)
        .unwrap();
    let mut input = request.clone();
    input.request_id = "revoked-warm-publication".into();
    let warm = ticket(&host, &input);
    let Admission::Cached(payload) = admission(&registry, "source", warm.clone()) else {
        unreachable!()
    };
    host.app.set_execution_context(ExecutionContext::for_caller(
        crate::CallSource::Embedded,
        crate::Caller::LocalOwner,
    ));
    host.app.revoke_credential(&issued.credential_id).unwrap();
    assert_eq!(
        publish_cached(
            &host.app,
            &registry,
            &warm,
            payload,
            |_| -> Result<(), EditorError> { panic!("revoked bytes must not be projected") }
        )
        .unwrap_err()
        .code,
        "NOT_AUTHORIZED"
    );
}
#[test]
fn warm_waiters_are_registered_and_cancelled_before_publication() {
    let (_temp, host, request) = source(b"source fixture");
    let registry = Registry::default();
    let first = ticket(&host, &request);
    let (shared, _) = flight(admission(&registry, "source", first.clone()));
    registry
        .complete("source", &shared, Ok(decoded(375)))
        .unwrap();
    registry
        .release(&TicketKey::new(&first.prepared), &first, &shared)
        .unwrap();
    let mut warm = request.clone();
    warm.request_id = "warm".into();
    let warm = ticket(&host, &warm);
    let key = TicketKey::new(&warm.prepared);
    assert!(matches!(
        admission(&registry, "source", warm.clone()),
        Admission::Cached(_)
    ));
    assert_eq!(
        registry
            .admit("source".into(), key.clone(), warm.clone())
            .err()
            .unwrap()
            .code,
        "SOURCE_ART_REQUEST_CONFLICT"
    );
    assert!(registry.cancel(key.clone()).unwrap().is_none());
    assert_eq!(
        registry.release_cached(&key, &warm).unwrap_err().code,
        "SOURCE_ART_CANCELLED"
    );
}
#[test]
fn source_art_requires_both_permissions_and_original_credential_even_in_owner_host() {
    let (_temp, mut host, request) = source(b"source fixture");
    grant(
        &mut host,
        "0",
        vec![ProjectAction::Read, ProjectAction::RunJobs],
    );
    let issued = host.app.issue_credential("source-reader").unwrap();
    let caller = host.app.authenticate(&issued.secret).unwrap();
    host.app.set_execution_context(ExecutionContext::for_caller(
        crate::CallSource::Embedded,
        caller,
    ));
    let selected = prepare_source_art(&host.app, &request).unwrap();
    grant(&mut host, "1", vec![ProjectAction::Read]);
    assert_eq!(
        authorize(&host.app, &selected).unwrap_err().code,
        "NOT_AUTHORIZED"
    );
    // A RunJobs-only grant is structurally invalid. Removing the complete
    // grant is the valid way to test loss of Read authority.
    grant(&mut host, "2", vec![]);
    assert_eq!(
        authorize(&host.app, &selected).unwrap_err().code,
        "NOT_AUTHORIZED"
    );
    grant(
        &mut host,
        "3",
        vec![ProjectAction::Read, ProjectAction::RunJobs],
    );
    authorize(&host.app, &selected).unwrap();
    host.app.revoke_credential(&issued.credential_id).unwrap();
    assert_eq!(
        authorize(&host.app, &selected).unwrap_err().code,
        "NOT_AUTHORIZED"
    );
    host.app
        .fixed_source_art_snapshot(&request.project_id, &request.revision)
        .unwrap();
}

#[test]
fn source_cache_and_display_share_raw_identity_without_following_head_or_overlay() {
    use opencut_contracts::{Time, TimeRange};
    let (_temp, mut host, source_input) = source(b"projection-only fixture");
    let fixed = host
        .app
        .inspect_scene_project(&InspectProject {
            project_id: "p".into(),
            revision: None,
        })
        .unwrap();
    let mut track = fixed.project.sequences[0].tracks[0].clone();
    track.id = "audio".into();
    track.kind = SceneTrackKind::Audio;
    let time = |ticks: &str| Time {
        ticks: ticks.into(),
        timebase: 1,
    };
    let element = TimelineElement {
        id: "voice".into(),
        name: "Waveform".into(),
        timeline_range: TimeRange {
            start: time("0"),
            duration: time("1"),
        },
        source_span: ElementSourceSpan::ClassicTrimmed {
            trim_start: time("0"),
            trim_end: time("0"),
            source_duration: Some(time("1")),
        },
        enabled: true,
        content: ElementContent::Audio {
            asset_id: "source".into(),
            audio: AudioStyle {
                stream: None,
                gain: Gain::Decibels { value: 0.0 },
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
        visual: None,
        channels: vec![],
    };
    track.elements.push(element.clone());
    host.app
        .apply_scene_project(&ApplySceneProject {
            project_id: "p".into(),
            expected_revision: fixed.revision,
            idempotency_key: "audio-track".into(),
            label: "Audio track".into(),
            operations: vec![ProjectOperation::TrackPut {
                sequence_id: "main".into(),
                track,
            }],
        })
        .unwrap();
    let display_fixed = host
        .app
        .inspect_scene_project(&InspectProject {
            project_id: "p".into(),
            revision: None,
        })
        .unwrap();
    let mut display_input = SourceArtWaveformDisplayRequest {
        project_id: "p".into(),
        revision: display_fixed.revision.clone(),
        asset_id: "source".into(),
        request_id: "display-fixed".into(),
        sample_rate: 48000,
        sequence_id: "main".into(),
        track_id: "audio".into(),
        element_id: "voice".into(),
        viewport: SourceArtWaveformViewport {
            clip_left_px: 0.0,
            clip_right_px: 2.0,
            pixels_per_second: 100.0,
            css_height: 20.0,
            device_pixel_ratio: 1.0,
        },
        temporary_gain: None,
    };
    let source_prepared = Arc::new(prepare_source_art(&host.app, &source_input).unwrap());
    let raw_key = source_art_decode_key(&source_prepared, &identity()).unwrap();
    let registry = Registry::default();
    let source_ticket = Arc::new(Ticket::new(source_prepared.clone()));
    let (source_flight, start) = flight(admission(&registry, &raw_key, source_ticket.clone()));
    assert!(start);
    // Typed cache/lifecycle fixture only; this is not a decoder or PCM oracle.
    let raw = Arc::new(DecodedSourceArt::Waveform {
        version: source_prepared.asset().version.clone(),
        identity: identity(),
        sample_rate: 48000,
        channels: 1,
        total_samples: 48000,
        bucket_size: 128,
        amplitudes: vec![0.125; 375],
    });
    validate_decoded_source_art(&source_prepared, &raw).unwrap();
    registry
        .complete(&raw_key, &source_flight, Ok(raw.clone()))
        .unwrap();
    registry
        .release(
            &TicketKey::new(&source_prepared),
            &source_ticket,
            &source_flight,
        )
        .unwrap();

    // A later stored gain must not change the fixed display or its raw cache key.
    let mut changed = element;
    if let ElementContent::Audio { audio, .. } = &mut changed.content {
        audio.gain = Gain::Decibels { value: -20.0 };
    }
    host.app
        .apply_scene_project(&ApplySceneProject {
            project_id: "p".into(),
            expected_revision: display_fixed.revision.clone(),
            idempotency_key: "new-gain-head".into(),
            label: "New gain".into(),
            operations: vec![ProjectOperation::ElementPut {
                sequence_id: "main".into(),
                track_id: "audio".into(),
                element: Box::new(changed),
            }],
        })
        .unwrap();
    let head_after_edit = host
        .app
        .inspect_scene_project(&InspectProject {
            project_id: "p".into(),
            revision: None,
        })
        .unwrap();
    for (request_id, overlay, expected, burn) in [
        ("display-fixed", None, 0.125, false),
        ("display-overlay", Some(200), 1.25, true),
    ] {
        display_input.request_id = request_id.into();
        display_input.temporary_gain = overlay;
        let prepared = prepare_waveform_display(&host.app, &display_input).unwrap();
        assert_eq!(
            source_art_decode_key(prepared.source_art(), &identity()).unwrap(),
            raw_key
        );
        let ticket = Arc::new(Ticket::new(Arc::new(prepared.source_art().clone())));
        let Admission::Cached(cached) = admission(&registry, &raw_key, ticket.clone()) else {
            panic!("display must reuse the source summary");
        };
        assert!(Arc::ptr_eq(&cached, &raw));
        let response = publish_cached(&host.app, &registry, &ticket, cached, |decoded| {
            project_waveform_display(&prepared, decoded)
        })
        .unwrap();
        assert_eq!(response.revision, display_fixed.revision);
        assert_eq!(response.bars[0].output_amplitude, expected);
        assert_eq!(response.bars[0].burn, burn);
    }
    assert_eq!(
        registry.stats().0,
        1,
        "display and overlay never re-decode raw source"
    );
    assert_eq!(
        host.app
            .inspect_scene_project(&InspectProject {
                project_id: "p".into(),
                revision: None
            })
            .unwrap(),
        head_after_edit
    );

    // The common warm publisher also fences a cancellation arriving after admission.
    display_input.request_id = "display-cancelled".into();
    let prepared = prepare_waveform_display(&host.app, &display_input).unwrap();
    let ticket = Arc::new(Ticket::new(Arc::new(prepared.source_art().clone())));
    let Admission::Cached(cached) = admission(&registry, &raw_key, ticket.clone()) else {
        unreachable!()
    };
    registry.cancel(TicketKey::new(&ticket.prepared)).unwrap();
    assert_eq!(
        publish_cached(&host.app, &registry, &ticket, cached, |decoded| {
            project_waveform_display(&prepared, decoded)
        })
        .unwrap_err()
        .code,
        "SOURCE_ART_CANCELLED"
    );
}
fn stereo_wave() -> Vec<u8> {
    let samples = 48000u32;
    let bytes = samples * 4;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + bytes).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&48000u32.to_le_bytes());
    out.extend_from_slice(&192000u32.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&bytes.to_le_bytes());
    for index in 0..samples {
        out.extend_from_slice(&(if index == 0 { 32767i16 } else { 0i16 }).to_le_bytes());
        out.extend_from_slice(&(if index == 128 { -16384i16 } else { 0i16 }).to_le_bytes());
    }
    out
}
#[test]
#[ignore = "Requires the explicit native render worker; run serially with --include-ignored"]
fn real_public_source_art_uses_all_channels_and_reuses_hundred_requests() {
    let (_temp, mut host, mut request) = source(&stereo_wave());
    for index in 0..100 {
        request.request_id = format!("actual-{index}");
        let response = host.call(
            "opencut_source_art_get",
            serde_json::to_value(&request).unwrap(),
        );
        assert!(response.error.is_none(), "{:?}", response.error);
        let response: SourceArtResponse = serde_json::from_value(response.data.unwrap()).unwrap();
        let SourceArtData::Waveform {
            amplitudes,
            channels,
            total_samples,
            ..
        } = response.data
        else {
            panic!("wrong art kind")
        };
        assert_eq!(channels, 2);
        assert_eq!(total_samples, "48000");
        assert_eq!(amplitudes.len(), 375);
        assert!((amplitudes[0] - 32767.0 / 32768.0).abs() < 0.0001);
        assert!((amplitudes[1] - 0.5).abs() < 0.0001);
        assert!(amplitudes[2..].iter().all(|value| *value == 0.0));
    }
    let (_, registry) = registry(&host.database).unwrap();
    assert_eq!(
        registry.stats().0,
        1,
        "one actual decode for hundred requests"
    );
    grant(
        &mut host,
        "0",
        vec![ProjectAction::Read, ProjectAction::RunJobs],
    );
    let issued = host.app.issue_credential("source-reader").unwrap();
    host.caller = host.app.authenticate(&issued.secret).unwrap();
    request.request_id = "credential-cold".into();
    let response = host.call(
        "opencut_source_art_get",
        serde_json::to_value(&request).unwrap(),
    );
    assert!(response.error.is_none(), "{:?}", response.error);
    assert_eq!(
        registry.stats().0,
        2,
        "credential scope cannot borrow owner's cached bytes"
    );
    host.app.set_execution_context(ExecutionContext::for_caller(
        crate::CallSource::Embedded,
        crate::Caller::LocalOwner,
    ));
    host.app.revoke_credential(&issued.credential_id).unwrap();
    request.request_id = "credential-warm-revoked".into();
    assert_eq!(
        host.call(
            "opencut_source_art_get",
            serde_json::to_value(&request).unwrap()
        )
        .error
        .unwrap()
        .code,
        "NOT_AUTHORIZED"
    );
    shutdown(&host.database).unwrap();
    opencut_asset_store::gate::Gate::exclusive(host.app.asset_store_root()).unwrap();
}

#[test]
fn busy_sources_queue_fifo_without_adding_decoder_slots_and_join_shared_work() {
    let (_temp, host, request) = source(b"queue fixture");
    let registry = Registry::default();
    let make = |id: &str| {
        let mut request = request.clone();
        request.request_id = id.into();
        ticket(&host, &request)
    };
    let a = make("a");
    let (active, _) = flight(admission(&registry, "a", a.clone()));
    let b = make("b");
    let c = make("c");
    assert!(matches!(
        admission(&registry, "b", b.clone()),
        Admission::Queued
    ));
    assert!(matches!(
        admission(&registry, "c", c.clone()),
        Admission::Queued
    ));
    assert_eq!(registry.waiting(), 2);
    assert_eq!(registry.stats().3, 1);
    let conflicting = make("b");
    assert_eq!(
        registry
            .admit(
                "b".into(),
                TicketKey::new(&conflicting.prepared),
                conflicting
            )
            .err()
            .unwrap()
            .code,
        "SOURCE_ART_REQUEST_CONFLICT"
    );
    registry.complete("a", &active, Ok(decoded(1))).unwrap();
    registry
        .release(&TicketKey::new(&a.prepared), &a, &active)
        .unwrap();
    // A later waiter cannot take a free slot ahead of the oldest live request.
    assert!(matches!(
        admission(&registry, "c", c.clone()),
        Admission::Queued
    ));
    let warm = make("warm");
    assert!(matches!(
        admission(&registry, "a", warm.clone()),
        Admission::Cached(_)
    ));
    registry
        .release_cached(&TicketKey::new(&warm.prepared), &warm)
        .unwrap();
    let (next, start) = flight(admission(&registry, "b", b.clone()));
    assert!(start);
    let joined = make("b-joined");
    let (same, start) = flight(admission(&registry, "b", joined.clone()));
    assert!(!start);
    assert!(Arc::ptr_eq(&next, &same));
    assert_eq!(registry.stats().3, 1);
    registry.complete("b", &next, Ok(decoded(1))).unwrap();
    for selected in [&b, &joined] {
        registry
            .release(&TicketKey::new(&selected.prepared), selected, &next)
            .unwrap();
    }
    let (last, start) = flight(await_admission(&host.app, &registry, "c", &c).unwrap());
    assert!(start);
    assert_eq!(registry.waiting(), 0);
    registry.complete("c", &last, Ok(decoded(1))).unwrap();
    registry
        .release(&TicketKey::new(&c.prepared), &c, &last)
        .unwrap();
    drop((active, next, same, last));
    registry.drain().unwrap();
}

#[test]
fn queued_requests_wake_after_completion_and_can_cancel_before_dispatch() {
    let (_temp, host, request) = source(b"queue fixture");
    let registry = Arc::new(Registry::default());
    let first = ticket(&host, &request);
    let (active, _) = flight(admission(&registry, "first", first.clone()));
    let mut input = request.clone();
    input.request_id = "waiting".into();
    let waiting = ticket(&host, &input);
    assert!(matches!(
        admission(&registry, "waiting", waiting.clone()),
        Admission::Queued
    ));
    let database = host.database.clone();
    let owner = registry.clone();
    let selected = waiting.clone();
    let thread = std::thread::spawn(move || {
        let app = EditorApplication::open_managed(database).unwrap();
        await_admission(&app, &owner, "waiting", &selected)
    });
    registry.complete("first", &active, Ok(decoded(1))).unwrap();
    registry
        .release(&TicketKey::new(&first.prepared), &first, &active)
        .unwrap();
    let (next, start) = flight(thread.join().unwrap().unwrap());
    assert!(start);
    assert_eq!(registry.stats().3, 1);
    input.request_id = "cancelled-in-queue".into();
    let cancelled = ticket(&host, &input);
    assert!(matches!(
        admission(&registry, "cancelled", cancelled.clone()),
        Admission::Queued
    ));
    assert!(
        registry
            .cancel(TicketKey::new(&cancelled.prepared))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        await_admission(&host.app, &registry, "cancelled", &cancelled)
            .err()
            .unwrap()
            .code,
        "SOURCE_ART_CANCELLED"
    );
    assert_eq!(registry.waiting(), 0);
    assert_eq!(registry.stats().3, 1);
    registry.complete("waiting", &next, Ok(decoded(1))).unwrap();
    registry
        .release(&TicketKey::new(&waiting.prepared), &waiting, &next)
        .unwrap();
    drop((active, next));
    registry.drain().unwrap();
}

#[test]
fn queued_deadlines_and_revoked_credentials_are_checked_before_dispatch() {
    let (_temp, mut host, request) = source(b"queue fixture");
    let registry = Registry::default();
    let first = ticket(&host, &request);
    let (active, _) = flight(admission(&registry, "first", first.clone()));
    let mut input = request.clone();
    input.request_id = "expired-in-queue".into();
    let mut expired = ticket(&host, &input);
    let deadline = expired.prepared.policy().deadline_ms;
    Arc::get_mut(&mut expired).unwrap().started = Instant::now() - Duration::from_millis(deadline);
    assert!(matches!(
        admission(&registry, "expired", expired.clone()),
        Admission::Queued
    ));
    assert_eq!(
        await_admission(&host.app, &registry, "expired", &expired)
            .err()
            .unwrap()
            .code,
        "SOURCE_ART_DEADLINE_EXCEEDED"
    );
    assert_eq!(registry.waiting(), 0);
    grant(
        &mut host,
        "0",
        vec![ProjectAction::Read, ProjectAction::RunJobs],
    );
    let credential = host.app.issue_credential("source-reader").unwrap();
    let caller = host.app.authenticate(&credential.secret).unwrap();
    host.app.set_execution_context(ExecutionContext::for_caller(
        crate::CallSource::Embedded,
        caller,
    ));
    input.request_id = "revoked-in-queue".into();
    let revoked = ticket(&host, &input);
    assert!(matches!(
        admission(&registry, "revoked", revoked.clone()),
        Admission::Queued
    ));
    host.app.set_execution_context(ExecutionContext::for_caller(
        crate::CallSource::Embedded,
        crate::Caller::LocalOwner,
    ));
    host.app
        .revoke_credential(&credential.credential_id)
        .unwrap();
    assert_eq!(
        await_admission(&host.app, &registry, "revoked", &revoked)
            .err()
            .unwrap()
            .code,
        "NOT_AUTHORIZED"
    );
    assert_eq!(registry.waiting(), 0);
    assert_eq!(registry.stats().3, 1);
    registry.complete("first", &active, Ok(decoded(1))).unwrap();
    registry
        .release(&TicketKey::new(&first.prepared), &first, &active)
        .unwrap();
    drop(active);
    registry.drain().unwrap();
}

#[test]
fn queued_requests_share_the_bounded_ticket_capacity() {
    let (_temp, host, request) = source(b"queue fixture");
    let registry = Registry::default();
    let first = ticket(&host, &request);
    let (active, _) = flight(admission(&registry, "first", first.clone()));
    let mut queued = Vec::new();
    for index in 1..256 {
        let mut input = request.clone();
        input.request_id = index.to_string();
        let selected = ticket(&host, &input);
        assert!(matches!(
            admission(&registry, &input.request_id, selected.clone()),
            Admission::Queued
        ));
        queued.push(selected);
    }
    let mut input = request.clone();
    input.request_id = "overflow".into();
    let overflow = ticket(&host, &input);
    let error = registry
        .admit(
            "overflow".into(),
            TicketKey::new(&overflow.prepared),
            overflow,
        )
        .err()
        .unwrap();
    assert_eq!(error.code, "SOURCE_ART_BUDGET_EXCEEDED");
    assert!(error.message.contains("queue is full"));
    assert_eq!(registry.waiting(), 255);
    assert_eq!(registry.stats().3, 1);
    for selected in queued {
        registry.cancel(TicketKey::new(&selected.prepared)).unwrap();
        assert!(
            registry
                .release_cached(&TicketKey::new(&selected.prepared), &selected)
                .is_err()
        );
    }
    registry.complete("first", &active, Ok(decoded(1))).unwrap();
    registry
        .release(&TicketKey::new(&first.prepared), &first, &active)
        .unwrap();
    drop(active);
    registry.drain().unwrap();
}
