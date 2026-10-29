use super::*;
use opencut_contracts::{CreateProject, FrameRate, project_history::*, scene_ops::*};
fn source() -> ArchiveSource {
    let doc = crate::scene::new_document(&CreateProject {
        id: "source".into(),
        name: "source".into(),
        sequence_id: "scene".into(),
        width: 64,
        height: 48,
        fps: FrameRate {
            numerator: 30,
            denominator: 1,
        },
    })
    .unwrap();
    let mut a = ProjectArchive {
        schema_version: 1,
        source_project_id: "source".into(),
        main_head_revision: "r4".into(),
        revisions: vec![ArchivedRevision {
            revision: "r0".into(),
            parent_revision: None,
            transaction_id: None,
            document: doc.clone().into(),
            created_at: "2026-01-01 00:00:00".into(),
        }],
        transactions: vec![],
        branches: vec![ArchivedBranch {
            name: "saved".into(),
            head_revision: "r1".into(),
            created_from_revision: "r0".into(),
            created_at: "2026-01-01 00:00:00".into(),
        }],
        merges: vec![ArchivedMerge {
            revision: "r4".into(),
            source_revision: "r1".into(),
        }],
    };
    for i in 1..=4 {
        let revision = format!("r{i}");
        let base = format!("r{}", i - 1);
        let transaction = format!("t{i}");
        let mut document = doc.clone();
        document.name = if i == 2 { "source" } else { "r0" }.into();
        let journal = match i {
            2 => ArchivedJournal::Causal(CausalJournal::Undo {
                schema_version: 4,
                request: UndoRequest {
                    project_id: "source".into(),
                    expected_revision: base.clone(),
                    target_transaction_id: "t1".into(),
                    idempotency_key: "undo".into(),
                },
                changes: vec![],
            }),
            3 => ArchivedJournal::Causal(CausalJournal::Redo {
                schema_version: 4,
                request: RedoRequest {
                    project_id: "source".into(),
                    expected_revision: base.clone(),
                    undo_transaction_id: "t2".into(),
                    idempotency_key: "redo".into(),
                },
                target_transaction_id: "t1".into(),
                changes: vec![],
            }),
            4 => ArchivedJournal::Restore(RestoreProject {
                branch_name: None,
                label: None,
                project_id: "source".into(),
                expected_revision: base.clone(),
                target_revision: "r1".into(),
                idempotency_key: "restore".into(),
                asset_versions: Default::default(),
            }),
            _ => ArchivedJournal::Apply(ApplySceneProject {
                project_id: "source".into(),
                expected_revision: base.clone(),
                idempotency_key: "edit".into(),
                label: "source r0 t1".into(),
                operations: vec![ProjectOperation::Rename { name: "r0".into() }],
            }),
        };
        a.revisions.push(ArchivedRevision {
            revision: revision.clone(),
            parent_revision: Some(base.clone()),
            transaction_id: Some(transaction.clone()),
            document: document.into(),
            created_at: "2026-01-01 00:00:00".into(),
        });
        a.transactions.push(ArchivedTransaction {
            transaction_id: transaction.clone(),
            base_revision: base.clone(),
            revision: revision.clone(),
            journal,
            result: TransactionResult {
                transaction_id: transaction,
                base_revision: base,
                revision,
                created_ids: vec![],
                changed_ids: vec!["source".into()],
                deleted_ids: vec![],
            },
            created_at: "2026-01-01 00:00:00".into(),
            source_order: None,
        });
    }
    ArchiveSource {
        archive: a,
        derived_fonts: vec![],
    }
}
fn prepared(s: &ArchiveSource) -> PreparedHistoricalArchive {
    let manifest = make_manifest(s).unwrap();
    PreparedHistoricalArchive {
        manifest_sha256: digest(&canonical_bytes(&manifest, MAX_MANIFEST_BYTES).unwrap()),
        manifest,
        archive: s.archive.clone(),
    }
}
#[test]
fn deterministic_mapping_preserves_content_and_every_typed_causal_reference() {
    let p = prepared(&source());
    let mapped = prepare_import(&p, "target").unwrap();
    assert_eq!(
        canonical_bytes(&mapped, MAX_HISTORY_BYTES).unwrap(),
        canonical_bytes(&prepare_import(&p, "target").unwrap(), MAX_HISTORY_BYTES).unwrap()
    );
    assert_ne!(
        mapped.main_head_revision,
        prepare_import(&p, "other").unwrap().main_head_revision
    );
    let edit = mapped
        .transactions
        .iter()
        .find(|t| matches!(t.journal, ArchivedJournal::Apply(_)))
        .unwrap();
    let ArchivedJournal::Apply(r) = &edit.journal else {
        unreachable!()
    };
    assert_eq!(r.project_id, "target");
    assert_eq!(r.label, "source r0 t1");
    assert!(matches!(&r.operations[0],ProjectOperation::Rename{name} if name=="r0"));
    assert_eq!(edit.result.changed_ids, vec!["target"]);
    let undo = mapped
        .transactions
        .iter()
        .find(|t| {
            matches!(
                t.journal,
                ArchivedJournal::Causal(CausalJournal::Undo { .. })
            )
        })
        .unwrap();
    let ArchivedJournal::Causal(CausalJournal::Undo { request, .. }) = &undo.journal else {
        unreachable!()
    };
    assert_eq!(request.target_transaction_id, edit.transaction_id);
    let redo = mapped
        .transactions
        .iter()
        .find(|t| {
            matches!(
                t.journal,
                ArchivedJournal::Causal(CausalJournal::Redo { .. })
            )
        })
        .unwrap();
    let ArchivedJournal::Causal(CausalJournal::Redo {
        request,
        target_transaction_id,
        ..
    }) = &redo.journal
    else {
        unreachable!()
    };
    assert_eq!(request.undo_transaction_id, undo.transaction_id);
    assert_eq!(target_transaction_id, &edit.transaction_id);
    assert_eq!(mapped.branches[0].head_revision, edit.revision);
    assert_eq!(mapped.merges[0].source_revision, edit.revision);
    assert!(
        mapped
            .revisions
            .iter()
            .all(|r| r.document.project_id() == "target"
                && ["source", "r0"].contains(&r.document.name()))
    );
    // One current envelope keeps each actual original shape, then maps a
    // promotion through the same pure kernel used by normal authoring.
    let mut legacy = opencut_editor_core::create_project(&CreateProject {
        id: "source".into(),
        name: "Original source".into(),
        sequence_id: "scene".into(),
        width: 64,
        height: 48,
        fps: FrameRate {
            numerator: 30,
            denominator: 1,
        },
    })
    .unwrap();
    let original_version = opencut_contracts::AssetVersion {
        id: format!("av_{}", "b".repeat(64)),
        content_hash: "b".repeat(64),
        size: 13,
    };
    legacy.assets.push(opencut_contracts::Asset {
        id: "original-media".into(),
        name: "Original metadata identity".into(),
        kind: opencut_contracts::AssetKind::Image,
        uri: "/missing/provenance-is-not-authority".into(),
        duration: None,
        version: None,
        metadata: BTreeMap::from([
            (
                "content_hash".into(),
                serde_json::json!(original_version.content_hash),
            ),
            ("size".into(), serde_json::json!(original_version.size)),
        ]),
    });
    let versions = BTreeMap::from([("original-media".into(), original_version.clone())]);
    for document in [
        ArchivedDocument::Legacy(legacy.clone()),
        ArchivedDocument::HistoricalVisual(
            opencut_contracts::historical_visual::HistoricalVisualDocument {
                schema_version: 3,
                base: legacy.clone(),
                visual: vec![],
                keyframe_curves: vec![],
            },
        ),
    ] {
        let mut mixed = source();
        mixed.archive.revisions.truncate(1);
        mixed.archive.revisions[0].document = document.clone();
        mixed.archive.transactions.clear();
        mixed.archive.branches.clear();
        mixed.archive.merges.clear();
        mixed.archive.main_head_revision = "r0".into();
        validate_content_for_exchange(&mixed, true).unwrap();
        let (blobs, assets, fonts) = content_inventory(&mixed.archive).unwrap();
        assert_eq!((assets, fonts), (1, 0));
        assert_eq!(
            (blobs[0].content_hash.as_str(), blobs[0].size_bytes),
            (
                original_version.content_hash.as_str(),
                original_version.size
            )
        );
        // Missing or malformed observations cannot become valid by URI.
        for metadata in [
            BTreeMap::new(),
            BTreeMap::from([
                ("content_hash".into(), serde_json::json!("not-a-hash")),
                ("size".into(), serde_json::json!(13)),
            ]),
        ] {
            let mut invalid = mixed.archive.clone();
            match &mut invalid.revisions[0].document {
                ArchivedDocument::Legacy(p) => p.assets[0].metadata = metadata,
                ArchivedDocument::HistoricalVisual(p) => p.base.assets[0].metadata = metadata,
                _ => unreachable!(),
            }
            assert!(content_inventory(&invalid).is_err());
        }
        let bytes = crate::project_exchange::archive_bytes(&mixed.archive).unwrap();
        let decoded = crate::project_exchange::decode_archive(&bytes).unwrap();
        assert_eq!(decoded.revisions[0].document, document);
        assert!(validate_source(&mixed).is_err()); // Original envelope stays frozen.
        let fixed = match document {
            ArchivedDocument::Legacy(p) => crate::scene::StoredProjectDocument::Legacy(Box::new(p)),
            ArchivedDocument::HistoricalVisual(p) => {
                crate::scene::StoredProjectDocument::HistoricalVisual(Box::new(p))
            }
            _ => unreachable!(),
        };
        let project = crate::scene::converted(&fixed, &versions).unwrap();
        mixed.archive.revisions.push(ArchivedRevision {
            revision: "r1".into(),
            parent_revision: Some("r0".into()),
            transaction_id: Some("t1".into()),
            document: project.into(),
            created_at: "2026-01-02".into(),
        });
        mixed.archive.main_head_revision = "r1".into();
        mixed.archive.transactions.push(ArchivedTransaction {
            transaction_id: "t1".into(),
            base_revision: "r0".into(),
            revision: "r1".into(),
            journal: ArchivedJournal::Promote(PromoteProject {
                project_id: "source".into(),
                branch_name: None,
                expected_revision: "r0".into(),
                idempotency_key: "promote-original".into(),
                asset_versions: versions.clone(),
            }),
            result: TransactionResult {
                transaction_id: "t1".into(),
                base_revision: "r0".into(),
                revision: "r1".into(),
                created_ids: vec![],
                changed_ids: vec!["source".into()],
                deleted_ids: vec![],
            },
            created_at: "2026-01-02".into(),
            source_order: None,
        });
        validate_content_for_exchange(&mixed, true).unwrap();
        let mut missing = mixed.clone();
        match &mut missing.archive.revisions[0].document {
            ArchivedDocument::Legacy(p) => p.assets[0].metadata.clear(),
            ArchivedDocument::HistoricalVisual(p) => p.base.assets[0].metadata.clear(),
            _ => unreachable!(),
        }
        // The promoted child already has this logical asset and immutable
        // version; it is not evidence for an identity absent from its parent.
        assert!(validate_content_for_exchange(&missing, true).is_err());
        let restored = remap_archive(
            &mixed.archive,
            &[],
            &"a".repeat(64),
            "target",
            "project-package",
            true,
        )
        .unwrap();
        assert!(
            restored
                .revisions
                .iter()
                .all(|r| r.document.project_id() == "target")
        );
        let ArchivedJournal::Promote(request) = &restored.transactions[0].journal else {
            unreachable!();
        };
        assert_eq!(request.project_id, "target");
        assert_eq!(
            request.expected_revision,
            restored.transactions[0].base_revision
        );
        let ArchivedDocument::Editable(project) = &mut mixed.archive.revisions[1].document else {
            unreachable!();
        };
        project.name = "forged promotion".into();
        assert!(validate_content_for_exchange(&mixed, true).is_err());
    }
}
#[test]
fn malformed_closure_causal_edges_and_digest_are_rejected_without_dropping_rows() {
    let original = source();
    validate_source(&original).unwrap();
    let mut bad = original.clone();
    bad.archive.revisions.remove(1);
    assert!(validate_source(&bad).is_err());
    let mut bad = original.clone();
    bad.archive.merges[0].source_revision = "r4".into();
    assert!(validate_source(&bad).is_err());
    let mut bad = original.clone();
    bad.archive.transactions[0].journal = ArchivedJournal::Restore(RestoreProject {
        branch_name: None,
        label: None,
        project_id: "source".into(),
        expected_revision: "r0".into(),
        target_revision: "r4".into(),
        idempotency_key: "impossible-future".into(),
        asset_versions: Default::default(),
    });
    assert!(validate_source(&bad).is_err());
    let mut bad = original.clone();
    bad.archive.transactions[2]
        .result
        .changed_ids
        .push("ordinary-content".into());
    let mut p = prepared(&bad);
    p.archive.transactions[0].result.revision = "r3".into();
    assert!(validate_prepared(&p).is_err());
    let mut bad = original.clone();
    bad.archive.branches[0].created_from_revision = "outside".into();
    assert!(validate_source(&bad).is_err());
    let mut bad = original.clone();
    if let ArchivedJournal::Causal(CausalJournal::Redo { request, .. }) =
        &mut bad.archive.transactions[2].journal
    {
        request.undo_transaction_id = "t1".into();
    }
    assert!(validate_source(&bad).is_err());
    assert_eq!(
        ArchivedJournal::from_storage(r#"{"kind":"future_edit","project_id":"source"}"#)
            .unwrap_err()
            .code,
        "PACKAGE_HISTORY_UNSUPPORTED"
    );
}
#[test]
fn public_resource_selector_revisions_are_mapped_but_provenance_ids_are_not() {
    use opencut_contracts::project_public::*;
    let mut s = source();
    s.archive.transactions[0].journal = ArchivedJournal::ApplyPublic(ApplyProject {
        branch_name: None,
        project_id: "source".into(),
        expected_revision: "r0".into(),
        idempotency_key: "original-key".into(),
        label: "source".into(),
        operations: vec![
            ProjectEditOperation::AssetBind {
                id: "asset".into(),
                name: "r0".into(),
                source: AssetSelector::ProjectAsset {
                    revision: "r0".into(),
                    asset_id: "r0".into(),
                },
            },
            ProjectEditOperation::FontBind {
                id: "font".into(),
                source: FontSelector::ProjectFont {
                    revision: "r0".into(),
                    font_id: "t1".into(),
                },
            },
        ],
    });
    let mapped = prepare_import(&prepared(&s), "target").unwrap();
    let initial = &mapped
        .revisions
        .iter()
        .find(|r| r.parent_revision.is_none())
        .unwrap()
        .revision;
    let edit = mapped
        .transactions
        .iter()
        .find(|t| matches!(t.journal, ArchivedJournal::ApplyPublic(_)))
        .unwrap();
    let ArchivedJournal::ApplyPublic(r) = &edit.journal else {
        unreachable!()
    };
    assert!(
        matches!(&r.operations[0],ProjectEditOperation::AssetBind{name,source:AssetSelector::ProjectAsset{revision,asset_id},..} if name=="r0" && revision==initial && asset_id=="r0")
    );
    assert!(
        matches!(&r.operations[1],ProjectEditOperation::FontBind{source:FontSelector::ProjectFont{revision,font_id},..} if revision==initial && font_id=="t1")
    );
    assert_eq!(r.idempotency_key, "original-key");
}
#[test]
fn known_font_caption_and_catalog_journals_roundtrip_all_original_fields() {
    use opencut_contracts::{catalog_resources::*, font_import::ImportFont, subtitles::*};
    let style = SceneCaptionStyle {
        font: FontBinding::Locked {
            font_id: "font".into(),
        },
        font_size: FontSize::CanvasPixels { value: 24.0 },
        color: Color {
            encoding: ColorEncoding::Srgb,
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha: 1.0,
        },
        background_color: None,
        max_width_px: None,
        line_height_px: None,
        text_align: SceneTextAlign::Center,
        layout: CaptionLayoutProfile::PinnedCaptionV4,
    };
    let journals = vec![
        ArchivedJournal::FontImport(ImportFont {
            project_id: "source".into(),
            expected_revision: "r0".into(),
            idempotency_key: "f".into(),
            catalog_id: "pinned".into(),
            font_id: "font".into(),
            required_text: "source r0".into(),
        }),
        ArchivedJournal::CaptionGenerate(GenerateCaptions {
            project_id: "source".into(),
            expected_revision: "r0".into(),
            sequence_id: "scene".into(),
            track_id: "captions".into(),
            analysis_job_id: "original-analysis-fact".into(),
            idempotency_key: "cg".into(),
            words_per_caption: 5,
            style: style.clone(),
        }),
        ArchivedJournal::CaptionImport(SubtitleImport {
            project_id: "source".into(),
            expected_revision: "r0".into(),
            sequence_id: "scene".into(),
            track_id: "captions".into(),
            idempotency_key: "ci".into(),
            format: SubtitleFormat::Srt,
            content: "source r0".into(),
            style,
        }),
        ArchivedJournal::CatalogImport(ImportCatalogResource {
            project_id: "source".into(),
            expected_revision: "r0".into(),
            idempotency_key: "i".into(),
            catalog_id: "kenney-ui-click1".into(),
            asset_id: "sound".into(),
            uploaded: None,
        }),
        ArchivedJournal::CatalogGenerate(GenerateCatalogSound {
            project_id: "source".into(),
            expected_revision: "r0".into(),
            idempotency_key: "g".into(),
            asset_id: "sound".into(),
            recipe: LocalSoundRecipe {
                kind: LocalSoundKind::Whoosh,
                duration_ms: 300,
                seed: 42,
                level_db: -6.0,
            },
        }),
    ];
    for journal in journals {
        let raw = journal.storage_json().unwrap();
        assert_eq!(
            ArchivedJournal::from_storage(&raw)
                .unwrap()
                .storage_json()
                .unwrap(),
            raw
        );
    }
    // Null in an original serialized change must remain null; it cannot be
    // reinterpreted as a current digest or silently lose an original field.
    for original in [
        serde_json::json!({"kind":"selective_undo","project_id":"source","expected_revision":"r0","target_transaction_id":"t0","undo_transaction_id":null,"idempotency_key":"original","changes":[{"path":"/assets/a/metadata/nullable","before":null,"after":null}]}),
        serde_json::json!({"kind":"selective_redo","schema_version":3,"target_transaction_id":"t0","undo_transaction_id":"undo","idempotency_key":"original","changes":[{"path":"/assets/a/metadata/nullable","before":null,"after":null}]}),
        serde_json::json!({"kind":"project_v3.promote","request":{"project_id":"source","expected_revision":"r0","idempotency_key":"original"}}),
    ] {
        let journal = ArchivedJournal::from_storage(&original.to_string()).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&journal.storage_json().unwrap()).unwrap(),
            original
        );
    }
    for malformed in [
        serde_json::json!({"kind":"selective_undo","schema_version":3,"project_id":"source","target_transaction_id":"t0","undo_transaction_id":null,"idempotency_key":"original","changes":[]}),
        serde_json::json!({"kind":"selective_undo","schema_version":4,"target_transaction_id":"t0","undo_transaction_id":null,"idempotency_key":"original","changes":[]}),
        serde_json::json!({"kind":"selective_redo","schema_version":3,"target_transaction_id":"t0","undo_transaction_id":null,"idempotency_key":"original","changes":[]}),
    ] {
        assert!(ArchivedJournal::from_storage(&malformed.to_string()).is_err());
    }
}

#[test]
fn original_visual_edit_keeps_bare_and_wrapped_shapes_without_accepting_unknown_fields() {
    let request = serde_json::json!({
        "project_id":"source","expected_revision":"r0","idempotency_key":"original",
        "label":"Original edit label","operations":[{"type":"base.apply","operation":{
            "type":"sequence.update_settings","sequence_id":"scene","name":"Edited",
            "width":null,"height":null,"fps":null}}]
    });
    let wrapped = serde_json::json!({"kind":"project_v3.apply","request":request});
    for original in [&request, &wrapped] {
        let journal = ArchivedJournal::from_storage(&original.to_string()).unwrap();
        assert!(matches!(
            &journal,
            ArchivedJournal::HistoricalVisualApply(_)
        ));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&journal.storage_json().unwrap()).unwrap(),
            *original
        );
        let restored: ArchivedJournal =
            serde_json::from_value(serde_json::to_value(&journal).unwrap()).unwrap();
        assert_eq!(
            restored.storage_json().unwrap(),
            journal.storage_json().unwrap()
        );
    }
    let mut extra = wrapped.clone();
    extra["unrecognized"] = true.into();
    let mut nested = wrapped.clone();
    nested["request"]["unrecognized"] = true.into();
    let mut kind = wrapped.clone();
    kind["kind"] = "project_v3.unrecognized".into();
    for invalid in [
        extra,
        nested,
        kind,
        serde_json::json!({"kind":"project_v3.apply","request":null}),
    ] {
        assert!(ArchivedJournal::from_storage(&invalid.to_string()).is_err());
        let envelope = serde_json::json!({"type":"historical_visual_apply","request":invalid});
        assert!(serde_json::from_value::<ArchivedJournal>(envelope).is_err());
    }
}

#[test]
fn brand_journal_restores_typed_scope_without_rewriting_catalog_or_upload_identity() {
    use opencut_contracts::{
        AssetVersion, brand_resources::ImportBrandResource, catalog_resources::CatalogUploadedMedia,
    };
    let original = ImportBrandResource {
        project_id: "source".into(),
        expected_revision: "r0".into(),
        idempotency_key: "brand-key".into(),
        catalog_id: "simple-icons:16.35.0:dotnet".into(),
        asset_id: "r0".into(),
        uploaded: CatalogUploadedMedia {
            media_id: "source".into(),
            version: AssetVersion {
                id: format!("av_{}", "a".repeat(64)),
                content_hash: "a".repeat(64),
                size: 1024,
            },
        },
    };
    let raw = serde_json::to_string(&original).unwrap();
    let mut journal = ArchivedJournal::from_storage(&raw).unwrap();
    assert!(matches!(&journal, ArchivedJournal::BrandImport(_)));
    assert_eq!(journal.storage_json().unwrap(), raw);
    // Exact original journals retain their marker; the current writer has none.
    let historical =
        serde_json::to_string(&crate::brand_resources::original_request(&original)).unwrap();
    assert_eq!(
        ArchivedJournal::from_storage(&historical)
            .unwrap()
            .storage_json()
            .unwrap(),
        historical
    );
    let mut invalid: serde_json::Value = serde_json::from_str(&historical).unwrap();
    invalid["schema_version"] = 2.into();
    assert!(ArchivedJournal::from_storage(&invalid.to_string()).is_err());
    let flag = opencut_contracts::catalog_resources::ImportCatalogResource {
        project_id: "source".into(),
        expected_revision: "r0".into(),
        idempotency_key: "flag-key".into(),
        catalog_id: "flags:fr".into(),
        asset_id: "flag".into(),
        uploaded: Some(original.uploaded.clone()),
    };
    assert!(matches!(
        ArchivedJournal::from_storage(&serde_json::to_string(&flag).unwrap()).unwrap(),
        ArchivedJournal::CatalogImport(_)
    ));
    journal
        .remap(
            "target",
            &BTreeMap::from([("r0".into(), "mapped".into())]),
            &BTreeMap::new(),
        )
        .unwrap();
    let ArchivedJournal::BrandImport(mapped) = journal else {
        unreachable!()
    };
    let mapped = mapped.request();
    assert_eq!(mapped.project_id, "target");
    assert_eq!(mapped.expected_revision, "mapped");
    assert_eq!(mapped.asset_id, "r0");
    assert_eq!(mapped.uploaded, original.uploaded);
    assert_eq!(mapped.catalog_id, original.catalog_id);
    assert_eq!(mapped.idempotency_key, original.idempotency_key);
}
