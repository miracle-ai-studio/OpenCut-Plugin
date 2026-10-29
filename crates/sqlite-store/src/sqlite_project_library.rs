//! Project library reads and atomic copies with explicit, scoped immutable-media references.
use super::*;
use opencut_api_contracts::project_library::*;
use opencut_editor_application::project_library;
use sqlite_project_document::ProjectStorageDocument;

pub(super) const MIGRATION: &str = "
CREATE TABLE project_media_bindings(
 project_id TEXT NOT NULL REFERENCES projects(project_id),
 media_id TEXT NOT NULL REFERENCES project_media(media_id),
 PRIMARY KEY(project_id,media_id));
CREATE VIEW available_project_media AS
 SELECT project_id,media_id,rowid AS ordinal FROM project_media
 UNION SELECT b.project_id,b.media_id,m.rowid AS ordinal FROM project_media_bindings b JOIN project_media m USING(media_id);
UPDATE metadata SET value='25' WHERE key='schema_version' AND value='24';
";

pub(super) const DELETE_MIGRATION: &str = "
CREATE TABLE deleted_projects(
 project_id TEXT PRIMARY KEY REFERENCES projects(project_id),
 revision TEXT NOT NULL REFERENCES revisions(revision),
 request_id TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
UPDATE metadata SET value='26' WHERE key='schema_version' AND value='25';
";

pub(super) fn require_live(c: &Connection, project: &str) -> Result<(), EditorError> {
    let deleted: bool = c
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM deleted_projects WHERE project_id=?1)")
        .map_err(storage_error)?
        .query_row([project], |r| r.get(0))
        .map_err(storage_error)?;
    check_live(deleted)
}
pub(super) fn check_live(deleted: bool) -> Result<(), EditorError> {
    if deleted {
        return Err(EditorError::new("NOT_FOUND", "Project does not exist"));
    }
    Ok(())
}

impl EditorApplication {
    pub fn remove_project_media(
        &mut self,
        input: &opencut_api_contracts::media_library::RemoveMedia,
    ) -> Result<TransactionResult, EditorError> {
        opencut_editor_application::media_library::validate(input)?;
        require_idempotency_key(&input.idempotency_key)?;
        let raw = {
            let tx = self
                .connection
                .unchecked_transaction()
                .map_err(storage_error)?;
            sqlite_access::require(
                &tx,
                &self.connection.1,
                &input.project_id,
                ProjectAction::Read,
            )?;
            sqlite_access::require(
                &tx,
                &self.connection.1,
                &input.project_id,
                ProjectAction::Edit,
            )?;
            let raw = tx
                .query_row(
                    "SELECT document_json FROM revisions WHERE project_id=?1 AND revision=?2",
                    params![input.project_id, input.expected_revision],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(storage_error)?
                .ok_or_else(|| EditorError::new("NOT_FOUND", "Project revision does not exist"))?;
            tx.commit().map_err(storage_error)?;
            raw
        };
        match sqlite_project_document::decode_stored_project(&raw)? {
            ProjectStorageDocument::Legacy(document) => self.apply(
                &input.project_id,
                &TransactionRequest {
                    expected_revision: input.expected_revision.clone(),
                    idempotency_key: input.idempotency_key.clone(),
                    label: "Remove media".into(),
                    operations: opencut_editor_application::media_library::base_operations(
                        &document, input,
                    )?,
                },
            ),
            ProjectStorageDocument::HistoricalVisual(document) => self
                .apply_historical_visual_project(
                    &opencut_contracts::historical_visual_ops::ApplyHistoricalVisualProject {
                        project_id: input.project_id.clone(),
                        expected_revision: input.expected_revision.clone(),
                        idempotency_key: input.idempotency_key.clone(),
                        label: "Remove media".into(),
                        operations: opencut_editor_application::media_library::visual_operations(
                            &document, input,
                        )?,
                    },
                ),
            ProjectStorageDocument::ReadOnly(_) => {
                return Err(opencut_editor_application::project_readonly::readonly_error());
            }
            ProjectStorageDocument::Editable(_) => Err(EditorError::new(
                "UNSUPPORTED_PROJECT_VERSION",
                "Schema 4 media removal requires schema 4 aggregate operations",
            )),
        }
    }
    pub fn delete_projects(
        &mut self,
        input: &DeleteProjects,
    ) -> Result<DeleteReceipt, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        project_library::validate_delete(input)?;
        require_idempotency_key(&input.idempotency_key)?;
        let identity = request_hash("project_delete", "workspace", input)?;
        let context = self.connection.1.clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        if let Some(raw) = load_idempotent(
            &tx,
            "project_delete",
            "workspace",
            &input.idempotency_key,
            &identity,
            "project_deletions",
        )? {
            return decode_json(&raw, "project deletion receipt");
        }
        let mut staging_upload_ids = Vec::new();
        for item in &input.projects {
            let head = sqlite_project_document::project_head_revision(&tx, &item.project_id)?;
            if head != item.expected_revision {
                return Err(revision_conflict(&item.expected_revision, &head));
            }
            // Submission and deletion share the SQLite write fence. Import jobs
            // are scoped through upload_sessions rather than job_projects.
            let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs j WHERE j.state IN ('queued','running') AND (EXISTS(SELECT 1 FROM job_projects p WHERE p.job_id=j.job_id AND p.project_id=?1) OR EXISTS(SELECT 1 FROM upload_sessions u WHERE u.job_id=j.job_id AND u.project_id=?1)))", [&item.project_id], |r| r.get(0)).map_err(storage_error)?;
            if busy {
                return Err(EditorError::new(
                    "PROJECT_BUSY",
                    "Wait for project tasks to finish or cancel them before deleting this project",
                ));
            }
            {
                let mut s=tx.prepare("SELECT u.upload_id FROM upload_sessions u JOIN upload_retention r USING(upload_id) WHERE u.project_id=?1 AND r.released=0 ORDER BY u.upload_id").map_err(storage_error)?;
                staging_upload_ids.extend(
                    s.query_map([&item.project_id], |r| r.get::<_, String>(0))
                        .map_err(storage_error)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(storage_error)?,
                );
            }
            let uploads: Vec<String> = {
                let mut s = tx.prepare("SELECT upload_id FROM upload_sessions WHERE project_id=?1 AND state='receiving'").map_err(storage_error)?;
                s.query_map([&item.project_id], |r| r.get(0))
                    .map_err(storage_error)?
                    .collect::<Result<_, _>>()
                    .map_err(storage_error)?
            };
            for id in uploads {
                tx.execute(
                    "UPDATE upload_sessions SET state='aborted' WHERE upload_id=?1",
                    [&id],
                )
                .map_err(storage_error)?;
                let view =
                    sqlite_uploads::read(&tx, &id)?.expect("upload selected in write transaction");
                sqlite_uploads::event(&tx, &context, &view, "upload.aborted", None)?;
            }
            tx.execute(
                "INSERT INTO deleted_projects(project_id,revision,request_id) VALUES (?1,?2,?3)",
                params![item.project_id, head, context.request_id()],
            )
            .map_err(storage_error)?;
            tx.execute(
                "DELETE FROM access_grants WHERE project_id=?1",
                [&item.project_id],
            )
            .map_err(storage_error)?;
            tx.execute(
                "DELETE FROM project_media_bindings WHERE project_id=?1",
                [&item.project_id],
            )
            .map_err(storage_error)?;
            // Ephemeral preview credentials have no place in deleted-project
            // history or packages. Clear every caller's host-only pending slot
            // under the same delete fence; late dispatch then sees NOT_FOUND.
            tx.execute(
                "DELETE FROM audio_preview_recovery_v1 WHERE project_id=?1",
                [&item.project_id],
            )
            .map_err(storage_error)?;
            tx.execute("INSERT INTO outbox(event_id,project_id,revision,event_type,payload_json) VALUES (?1,?2,?3,'project.deleted',?4)", params![format!("evt_{}",Uuid::new_v4().simple()),item.project_id,head,encode_json(item,"project deletion")?]).map_err(storage_error)?;
            sqlite_audit::insert(
                &tx,
                &context,
                "project.deleted",
                sqlite_audit::Links {
                    project_id: Some(&item.project_id),
                    revision: Some(&head),
                    ..Default::default()
                },
            )?;
        }
        let receipt = DeleteReceipt {
            projects: input.projects.clone(),
            staging_upload_ids,
        };
        tx.execute("INSERT INTO idempotency(operation_family,scope,idempotency_key,request_hash,result_kind,result_json) VALUES ('project_delete','workspace',?1,?2,'project_deletions',?3)", params![input.idempotency_key,identity.current_digest(),encode_json(&receipt,"project deletion receipt")?]).map_err(idempotency_insert_error)?;
        tx.commit().map_err(storage_error)?;
        Ok(receipt)
    }
    pub fn project_library(&self, input: &LibraryInput) -> Result<LibraryPage, EditorError> {
        if !(1..=100).contains(&input.limit) {
            return Err(EditorError::new(
                "INVALID_ARGUMENT",
                "Library page limit must be 1..100",
            ));
        }
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        let principal = sqlite_access::principal(&tx, &self.connection.1)?;
        let after = if let Some(cursor) = &input.cursor {
            let (kind, identity, after): (String, Option<String>, String) =
                serde_json::from_str(cursor)
                    .map_err(|_| EditorError::new("INVALID_CURSOR", "Invalid library cursor"))?;
            if kind != "project-directory" || identity != principal {
                return Err(EditorError::new(
                    "INVALID_CURSOR",
                    "Library cursor belongs to another identity",
                ));
            }
            after
        } else {
            String::new()
        };
        // One bounded authorized directory includes every stored content format.
        let mut statement = tx.prepare("SELECT p.project_id,p.head_revision,r.document_json,
            strftime('%Y-%m-%dT%H:%M:%SZ',(SELECT min(created_at) FROM revisions c WHERE c.project_id=p.project_id)),
            strftime('%Y-%m-%dT%H:%M:%SZ',r.created_at)
            FROM projects p JOIN revisions r ON r.project_id=p.project_id AND r.revision=p.head_revision
            WHERE p.project_id>?1
            AND NOT EXISTS(SELECT 1 FROM deleted_projects d WHERE d.project_id=p.project_id) AND (?2 IS NULL OR EXISTS(SELECT 1 FROM access_grants g WHERE g.project_id=p.project_id AND g.principal_id=?2 AND g.action_mask & 1 != 0))
            ORDER BY p.project_id LIMIT ?3").map_err(storage_error)?;
        let rows = statement
            .query_map(params![after, principal, input.limit + 1], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(storage_error)?;
        let mut items = Vec::new();
        for row in rows {
            let (id, revision, raw, created_at, modified_at) = row.map_err(storage_error)?;
            let document = sqlite_project_document::decode_stored_project(&raw)?;
            let (base, visual, format) = match &document {
                ProjectStorageDocument::Legacy(base) => (base, None, ProjectFormat::Base),
                ProjectStorageDocument::HistoricalVisual(visual) => {
                    (&visual.base, Some(visual), ProjectFormat::Visual)
                }
                ProjectStorageDocument::ReadOnly(document) => {
                    if document.id != id {
                        return Err(EditorError::new(
                            "STORAGE_ERROR",
                            "Library project scope changed",
                        ));
                    }
                    items.push(project_library::describe_current(
                        &document.base,
                        revision,
                        created_at,
                        modified_at,
                        ProjectFormat::Readonly,
                    )?);
                    continue;
                }
                ProjectStorageDocument::Editable(document) => {
                    if document.id != id {
                        return Err(EditorError::new(
                            "STORAGE_ERROR",
                            "Library project scope changed",
                        ));
                    }
                    items.push(project_library::describe_current(
                        document,
                        revision,
                        created_at,
                        modified_at,
                        ProjectFormat::Editor,
                    )?);
                    continue;
                }
            };
            if base.id != id {
                return Err(EditorError::new(
                    "STORAGE_ERROR",
                    "Project identity differs from library scope",
                ));
            }
            let duration = project_library::duration(base, visual)?;
            let main = base
                .sequences
                .iter()
                .find(|s| s.id == base.default_sequence_id)
                .expect("duration validated main scene");
            items.push(LibraryProject {
                project_id: id,
                name: base.name.clone(),
                revision,
                format,
                created_at,
                modified_at,
                duration,
                width: main.width,
                height: main.height,
                fps: main.fps.clone(),
                scene_count: base.sequences.len(),
                asset_count: base.assets.len(),
            });
        }
        drop(statement);
        let next_cursor = if items.len() > input.limit {
            items.truncate(input.limit);
            Some(encode_json(
                &(
                    "project-directory",
                    principal,
                    &items.last().expect("positive page").project_id,
                ),
                "library cursor",
            )?)
        } else {
            None
        };
        tx.commit().map_err(storage_error)?;
        Ok(LibraryPage { items, next_cursor })
    }

    pub fn duplicate_projects(
        &mut self,
        input: &DuplicateProjects,
    ) -> Result<DuplicateReceipt, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        project_library::validate(input)?;
        require_idempotency_key(&input.idempotency_key)?;
        let identity = request_hash("project_duplicate", "workspace", input)?;
        let context = self.connection.1.clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        if let Some(raw) = load_idempotent(
            &tx,
            "project_duplicate",
            "workspace",
            &input.idempotency_key,
            &identity,
            "project_copies",
        )? {
            return decode_json(&raw, "project copy receipt");
        }
        let mut receipt = DuplicateReceipt {
            projects: Vec::new(),
        };
        for item in &input.projects {
            require_live(&tx, &item.source_project_id)?;
            let (head,raw):(String,String)=tx.query_row("SELECT p.head_revision,r.document_json FROM projects p JOIN revisions r ON r.revision=p.head_revision AND r.project_id=p.project_id WHERE p.project_id=?1",[&item.source_project_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(storage_error)?.ok_or_else(||EditorError::new("NOT_FOUND","Source project does not exist"))?;
            if head != item.expected_revision {
                return Err(revision_conflict(&item.expected_revision, &head));
            }
            let payload = json!({"source_project_id":item.source_project_id,"source_revision":head,"project_id":item.target_project_id});
            let revision = match sqlite_project_document::decode_stored_project(&raw)? {
                ProjectStorageDocument::Legacy(base) => {
                    let document = project_library::copy_base(&base, item)?;
                    let revision = project_library::base_revision(&document)?;
                    sqlite_new_project::publish_document(
                        &tx,
                        &context,
                        &document,
                        &revision,
                        "project.duplicated",
                        &payload,
                    )?;
                    revision
                }
                ProjectStorageDocument::HistoricalVisual(visual) => {
                    let document = project_library::copy_visual(&visual, item)?;
                    let revision = sqlite_historical_visual::revision_id(&document, None, None)?;
                    sqlite_new_project::publish_document(
                        &tx,
                        &context,
                        &document,
                        &revision,
                        "project.duplicated",
                        &payload,
                    )?;
                    revision
                }
                ProjectStorageDocument::ReadOnly(_) => {
                    return Err(opencut_editor_application::project_readonly::readonly_error());
                }
                ProjectStorageDocument::Editable(_) => {
                    return Err(EditorError::new(
                        "UNSUPPORTED_PROJECT_VERSION",
                        "Schema 4 copies require schema 4 project operations",
                    ));
                }
            };
            tx.execute("INSERT INTO project_media_bindings(project_id,media_id) SELECT ?1,media_id FROM available_project_media WHERE project_id=?2",params![item.target_project_id,item.source_project_id]).map_err(storage_error)?;
            copy_grants(&tx, &item.source_project_id, &item.target_project_id)?;
            receipt.projects.push(ProjectCopy {
                source_project_id: item.source_project_id.clone(),
                source_revision: head,
                project_id: item.target_project_id.clone(),
                revision,
            });
        }
        tx.execute("INSERT INTO idempotency(operation_family,scope,idempotency_key,request_hash,result_kind,result_json) VALUES ('project_duplicate','workspace',?1,?2,'project_copies',?3)",params![input.idempotency_key,identity.current_digest(),encode_json(&receipt,"project copy receipt")?]).map_err(idempotency_insert_error)?;
        tx.commit().map_err(storage_error)?;
        Ok(receipt)
    }
}

impl project_library::ProjectDeletionStore for EditorApplication {
    fn delete_projects_atomic(
        &mut self,
        input: &DeleteProjects,
    ) -> Result<DeleteReceipt, EditorError> {
        self.delete_projects(input)
    }
}

fn copy_grants(
    tx: &rusqlite::Transaction<'_>,
    source: &str,
    target: &str,
) -> Result<(), EditorError> {
    for table in ["project_plugin_grants", "project_preset_grants"] {
        let mut statement = tx
            .prepare(&format!(
                "SELECT plugin_id,version,grant_json FROM {table} WHERE project_id=?1"
            ))
            .map_err(storage_error)?;
        let rows = statement
            .query_map([source], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(storage_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)?;
        drop(statement);
        for (id, version, raw) in rows {
            let grant = if table == "project_plugin_grants" {
                let mut grant: opencut_api_contracts::project_plugins::Grant =
                    decode_json(&raw, "copied planner grant")?;
                grant.target.project_id = target.into();
                grant.revision = "1".into();
                encode_json(&grant, "copied planner grant")?
            } else {
                let mut grant: opencut_api_contracts::presets::Grant =
                    decode_json(&raw, "copied preset grant")?;
                grant.target.project_id = target.into();
                grant.revision = "1".into();
                encode_json(&grant, "copied preset grant")?
            };
            tx.execute(&format!("INSERT INTO {table}(project_id,plugin_id,version,revision,grant_json) VALUES (?1,?2,?3,'1',?4)"),params![target,id,version,grant]).map_err(storage_error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencut_contracts::historical_visual_ops::{CreateProjectRequest, InspectProject};
    use opencut_editor_application::{
        access::*,
        uploads::{ImportStore, ProjectMediaInput, ProjectMediaQuery},
    };
    fn create(app: &mut EditorApplication, id: &str) -> String {
        app.create_historical_visual_project(&serde_json::from_value::<CreateProjectRequest>(json!({"project":{"id":id,"name":id,"sequence_id":"main","width":640,"height":360,"fps":{"numerator":25,"denominator":1}},"idempotency_key":id})).unwrap()).unwrap().revision
    }
    fn request(source: &str, revision: &str, target: &str) -> DuplicateProjects {
        DuplicateProjects {
            idempotency_key: target.into(),
            projects: vec![CopyProject {
                source_project_id: source.into(),
                expected_revision: revision.into(),
                target_project_id: target.into(),
                name: format!("Copy {source}"),
            }],
        }
    }
    fn inspect(
        app: &EditorApplication,
        id: &str,
    ) -> opencut_contracts::historical_visual::HistoricalVisualSnapshot {
        app.inspect_historical_visual_project(&InspectProject {
            project_id: id.into(),
            revision: None,
        })
        .unwrap()
    }
    fn deletion(project: &str, revision: &str, key: &str) -> DeleteProjects {
        DeleteProjects {
            projects: vec![DeleteProject {
                project_id: project.into(),
                expected_revision: revision.into(),
            }],
            idempotency_key: key.into(),
        }
    }
    #[test]
    fn deletion_is_atomic_fixed_head_idempotent_and_cannot_resurrect() {
        let mut app = EditorApplication::open(":memory:").unwrap();
        let a = create(&mut app, "a");
        let b = create(&mut app, "b");
        let before = inspect(&app, "a");
        let mut batch = deletion("a", &a, "delete-batch");
        batch.projects.push(DeleteProject {
            project_id: "b".into(),
            expected_revision: "stale".into(),
        });
        assert_eq!(
            app.delete_projects(&batch).unwrap_err().code,
            "REVISION_CONFLICT"
        );
        assert_eq!(inspect(&app, "a"), before);
        assert_eq!(
            app.connection
                .query_row::<i64, _, _>("SELECT count(*) FROM deleted_projects", [], |r| r.get(0))
                .unwrap(),
            0
        );
        batch.projects[1].expected_revision = b;
        app.connection.execute_batch("CREATE TRIGGER fail_deletion BEFORE INSERT ON audit_records WHEN NEW.action='project.deleted' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        assert!(app.delete_projects(&batch).is_err());
        assert_eq!(inspect(&app, "a"), before);
        app.connection
            .execute_batch("DROP TRIGGER fail_deletion")
            .unwrap();
        let receipt = app.delete_projects(&batch).unwrap();
        assert_eq!(app.delete_projects(&batch).unwrap(), receipt);
        assert_eq!(
            app.connection
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM audit_records WHERE action='project.deleted'",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            2
        );
        let mut changed = batch.clone();
        changed.projects.pop();
        assert_eq!(
            app.delete_projects(&changed).unwrap_err().code,
            "IDEMPOTENCY_KEY_REUSED"
        );
        for revision in [None, Some(a.clone())] {
            assert_eq!(
                app.inspect_historical_visual_project(&InspectProject {
                    project_id: "a".into(),
                    revision
                })
                .unwrap_err()
                .code,
                "NOT_FOUND"
            );
        }
        assert!(
            app.project_library(&opencut_contracts::project_catalog::LibraryInput {
                cursor: None,
                limit: 100
            })
            .unwrap()
            .items
            .is_empty()
        );
        assert!(
            app.project_library(&LibraryInput {
                cursor: None,
                limit: 100
            })
            .unwrap()
            .items
            .is_empty()
        );
        assert_eq!(
            app.duplicate_projects(&request("a", &a, "copy-deleted"))
                .unwrap_err()
                .code,
            "NOT_FOUND"
        );
        assert_eq!(app.create_historical_visual_project(&serde_json::from_value::<CreateProjectRequest>(json!({"project":{"id":"a","name":"a","sequence_id":"main","width":640,"height":360,"fps":{"numerator":25,"denominator":1}},"idempotency_key":"a"})).unwrap()).unwrap_err().code,"NOT_FOUND");
        assert_eq!(
            app.create_job(JobRequest {
                kind: "render.project.v3".into(),
                input: json!({"snapshot":before}),
                idempotency_key: "stale-job".into()
            })
            .unwrap_err()
            .code,
            "NOT_FOUND"
        );
        assert_eq!(
            app.create_prepared_job(
                JobRequest {
                    kind: "render.project.v3".into(),
                    input: json!({}),
                    idempotency_key: "late-prepared".into()
                },
                json!({"snapshot":before})
            )
            .unwrap_err()
            .code,
            "NOT_FOUND"
        );
        let foreign_keys: i64 = app
            .connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(foreign_keys, 0);
    }
    #[test]
    fn deletion_rejects_active_jobs_and_aborts_receiving_uploads() {
        let mut app = EditorApplication::open(":memory:").unwrap();
        let revision = create(&mut app, "source");
        let snapshot = inspect(&app, "source");
        let job = app
            .create_job(JobRequest {
                kind: "render.project.v3".into(),
                input: json!({"snapshot":snapshot}),
                idempotency_key: "render".into(),
            })
            .unwrap();
        let input = deletion("source", &revision, "delete");
        assert_eq!(
            app.delete_projects(&input).unwrap_err().code,
            "PROJECT_BUSY"
        );
        app.cancel_job(&job.job_id).unwrap();
        use opencut_editor_application::uploads::{UploadRetentionStore, UploadStore};
        let upload = app
            .begin_upload(&opencut_api_contracts::uploads::BeginUpload {
                project_id: "source".into(),
                idempotency_key: "upload".into(),
                name: "Fixture".into(),
                size_bytes: "10".into(),
                content_hash: None,
            })
            .unwrap();
        struct Cleanup(bool);
        impl opencut_editor_application::uploads::UploadCleanup for Cleanup {
            fn discard_chunks(
                &self,
                _: &str,
                authorize: &dyn Fn() -> Result<(), EditorError>,
            ) -> Result<(), EditorError> {
                authorize()?;
                if self.0 {
                    Ok(())
                } else {
                    Err(EditorError::new("STORAGE_ERROR", "injected"))
                }
            }
        }
        let failed = project_library::delete_projects(&mut app, &Cleanup(false), &input).unwrap();
        assert_eq!(failed.cleanup_pending, vec![upload.upload_id.clone()]);
        assert_eq!(
            app.upload_retention(&upload.upload_id)
                .unwrap()
                .reserved_bytes,
            "10"
        );
        let released = project_library::delete_projects(&mut app, &Cleanup(true), &input).unwrap();
        assert_eq!(failed.receipt, released.receipt);
        assert!(released.cleanup_pending.is_empty());
        assert_eq!(
            app.upload_retention(&upload.upload_id)
                .unwrap()
                .reserved_bytes,
            "0"
        );
        assert_eq!(
            sqlite_uploads::read(&app.connection, &upload.upload_id)
                .unwrap()
                .unwrap()
                .state,
            opencut_api_contracts::uploads::UploadState::Aborted
        );
        assert_eq!(
            app.connection
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM outbox WHERE event_type='upload.aborted'",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            1
        );
    }
    #[test]
    fn deletion_migration_preserves_projects_and_rolls_back_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.sqlite");
        let mut app = EditorApplication::open(&path).unwrap();
        let revision = create(&mut app, "source");
        app.connection.execute_batch("DROP TABLE audio_preview_recovery_v1; DROP TABLE font_derivations;
DROP TABLE edit_recovery; DROP TABLE deleted_projects; UPDATE metadata SET value='25' WHERE key='schema_version'; CREATE TABLE deleted_projects(collision TEXT);").unwrap();
        drop(app);
        assert!(EditorApplication::open(&path).is_err());
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row::<String, _, _>(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            "25"
        );
        conn.execute_batch("DROP TABLE deleted_projects").unwrap();
        drop(conn);
        let mut app = EditorApplication::open(&path).unwrap();
        assert_eq!(inspect(&app, "source").revision, revision);
        app.delete_projects(&deletion("source", &revision, "delete"))
            .unwrap();
        drop(app);
        let app = EditorApplication::open(&path).unwrap();
        assert!(
            app.project_library(&LibraryInput {
                cursor: None,
                limit: 100
            })
            .unwrap()
            .items
            .is_empty()
        );
    }
    #[test]
    fn whole_document_copy_is_independent_atomic_and_replayable() {
        let mut app = EditorApplication::open(":memory:").unwrap();
        let initial = create(&mut app, "source");
        let edited=app.apply_project_edit(&serde_json::from_value(json!({"project_id":"source","expected_revision":initial,"label":"Scene","idempotency_key":"scene","operations":[{"type":"create_scene","sequence_id":"second","name":"B roll","video_track_id":"b-video"}]})).unwrap()).unwrap();
        let before = inspect(&app, "source");
        let input = request("source", &edited.revision, "copy");
        let copied = app.duplicate_projects(&input).unwrap();
        assert_eq!(app.duplicate_projects(&input).unwrap(), copied);
        let mut expected = before.project.clone();
        expected.base.id = "copy".into();
        expected.base.name = "Copy source".into();
        assert_eq!(inspect(&app, "copy").project, expected);
        app.apply_project_edit(&serde_json::from_value(json!({"project_id":"copy","expected_revision":copied.projects[0].revision,"label":"Rename","idempotency_key":"rename","operations":[{"type":"rename_scene","sequence_id":"second","name":"Independent"}]})).unwrap()).unwrap();
        assert_eq!(inspect(&app, "source"), before);
        let mut conflict = input.clone();
        conflict.projects[0].name = "Changed".into();
        assert_eq!(
            app.duplicate_projects(&conflict).unwrap_err().code,
            "IDEMPOTENCY_KEY_REUSED"
        );
        let mut batch = request("source", &edited.revision, "rolled-back");
        batch.projects.push(CopyProject {
            target_project_id: "copy".into(),
            ..batch.projects[0].clone()
        });
        assert_eq!(
            app.duplicate_projects(&batch).unwrap_err().code,
            "ALREADY_EXISTS"
        );
        assert!(
            app.inspect_historical_visual_project(&InspectProject {
                project_id: "rolled-back".into(),
                revision: None
            })
            .is_err()
        );
        app.connection.execute_batch("CREATE TRIGGER deny_copy BEFORE INSERT ON audit_records WHEN NEW.action='project.duplicated' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        let failed = request("source", &edited.revision, "audit-failure");
        assert!(app.duplicate_projects(&failed).is_err());
        assert!(
            app.inspect_historical_visual_project(&InspectProject {
                project_id: "audit-failure".into(),
                revision: None
            })
            .is_err()
        );
        app.connection
            .execute_batch("DROP TRIGGER deny_copy")
            .unwrap();
        app.duplicate_projects(&failed).unwrap();
        let stale = request("source", "not-head", "stale");
        assert_eq!(
            app.duplicate_projects(&stale).unwrap_err().code,
            "REVISION_CONFLICT"
        );
    }
    #[test]
    fn copied_media_membership_is_scoped_and_survives_a_second_copy() {
        let mut app = EditorApplication::open(":memory:").unwrap();
        let source = create(&mut app, "source");
        create(&mut app, "unrelated");
        let asset = json!({"id":"media_00000000000000000000000000000001","name":"Fixture","kind":"image","uri":"opencut-media://media_00000000000000000000000000000001","version":{"id":"fixed","content_hash":"a".repeat(64),"size":10},"duration":null,"metadata":{}});
        app.connection.execute("INSERT INTO jobs(job_id,state,attempt,kind,input_json,cancel_requested) VALUES ('job','succeeded',1,'media.import','{}',0)",[]).unwrap();
        app.connection.execute("INSERT INTO upload_sessions(upload_id,project_id,idempotency_key,request_hash,name,size_bytes,content_hash,state,job_id) VALUES ('upload','source','upload','hash','Fixture',10,?1,'sealed','job')",["a".repeat(64)]).unwrap();
        app.connection.execute("INSERT INTO project_media(media_id,project_id,upload_id,job_id,asset_json) VALUES ('media_00000000000000000000000000000001','source','upload','job',?1)",[asset.to_string()]).unwrap();
        let copy = app
            .duplicate_projects(&request("source", &source, "copy"))
            .unwrap();
        app.duplicate_projects(&request("copy", &copy.projects[0].revision, "second-copy"))
            .unwrap();
        assert_eq!(
            app.connection
                .query_row::<i64, _, _>("SELECT count(*) FROM project_media", [], |r| r.get(0))
                .unwrap(),
            1
        );
        for id in ["copy", "second-copy"] {
            let media = app
                .project_media(&ProjectMediaInput {
                    project_id: id.into(),
                    media_id: "media_00000000000000000000000000000001".into(),
                })
                .unwrap();
            assert_eq!(media.project_id, id);
            assert_eq!(serde_json::to_value(media.asset).unwrap(), asset);
            let page = opencut_editor_application::uploads::query_media(
                &app,
                ProjectMediaQuery {
                    project_id: id.into(),
                    cursor: None,
                    limit: 1,
                },
            )
            .unwrap();
            assert_eq!(page.media.len(), 1);
        }
        assert!(
            app.project_media(&ProjectMediaInput {
                project_id: "unrelated".into(),
                media_id: "media_00000000000000000000000000000001".into()
            })
            .is_err()
        );
        app.set_principal_policy(PrincipalPolicy {
            principal_id: "reader".into(),
            expected_revision: "0".into(),
            enabled: true,
            grants: vec![ProjectGrant {
                project_id: "copy".into(),
                actions: vec![ProjectAction::Read, ProjectAction::Edit],
            }],
        })
        .unwrap();
        let issued = app.issue_credential("reader").unwrap();
        app.set_caller(app.authenticate(&issued.secret).unwrap());
        assert!(
            app.project_media(&ProjectMediaInput {
                project_id: "copy".into(),
                media_id: "media_00000000000000000000000000000001".into()
            })
            .is_ok()
        );
        assert!(
            app.project_media(&ProjectMediaInput {
                project_id: "source".into(),
                media_id: "media_00000000000000000000000000000001".into()
            })
            .is_err()
        );
        assert_eq!(
            app.duplicate_projects(&request("copy", &copy.projects[0].revision, "denied"))
                .unwrap_err()
                .code,
            "NOT_AUTHORIZED"
        );
        let page = app
            .project_library(&LibraryInput {
                cursor: None,
                limit: 100,
            })
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].project_id, "copy");
        assert_eq!(
            app.delete_projects(&deletion(
                "copy",
                &copy.projects[0].revision,
                "denied-delete"
            ))
            .unwrap_err()
            .code,
            "NOT_AUTHORIZED"
        );
        app.set_caller(Caller::LocalOwner);
        app.delete_projects(&deletion("source", &source, "delete-original"))
            .unwrap();
        assert!(
            app.project_media(&ProjectMediaInput {
                project_id: "copy".into(),
                media_id: "media_00000000000000000000000000000001".into()
            })
            .is_ok()
        );
        app.delete_projects(&deletion("copy", &copy.projects[0].revision, "delete-copy"))
            .unwrap();
        assert!(
            app.project_media(&ProjectMediaInput {
                project_id: "second-copy".into(),
                media_id: "media_00000000000000000000000000000001".into()
            })
            .is_ok()
        );
        assert_eq!(
            app.connection
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM access_grants WHERE project_id='copy'",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            0
        );
        app.set_caller(app.authenticate(&issued.secret).unwrap());
        assert!(
            app.project_library(&LibraryInput {
                cursor: None,
                limit: 100
            })
            .unwrap()
            .items
            .is_empty()
        );
    }
    #[test]
    fn one_directory_paginates_all_formats_without_losing_scope_or_later_rows() {
        use opencut_contracts::{project_catalog::LibraryInput, scene_ops::CreateProjectRequest};
        let mut app = EditorApplication::open(":memory:").unwrap();
        for id in ["a", "d", "f"] {
            create(&mut app, id);
        }
        for id in ["b", "c", "e"] {
            let request: CreateProjectRequest = serde_json::from_value(json!({
                "project":{"id":id,"name":id,"sequence_id":"main",
                    "width":64,"height":48,"fps":{"numerator":25,"denominator":1}},
                "idempotency_key":format!("v4-{id}")
            }))
            .unwrap();
            app.create_scene_project(&request).unwrap();
        }
        let mut ids = Vec::new();
        let mut cursor = None;
        loop {
            let page = app
                .project_library(&LibraryInput { cursor, limit: 1 })
                .unwrap();
            assert_eq!(page.items.len(), 1);
            ids.push(page.items[0].project_id.clone());
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(ids, ["a", "b", "c", "d", "e", "f"]);
        assert_eq!(
            app.project_library(&LibraryInput {
                cursor: None,
                limit: 100
            })
            .unwrap()
            .items
            .len(),
            6
        );
        app.set_principal_policy(PrincipalPolicy {
            principal_id: "mixed-reader".into(),
            expected_revision: "0".into(),
            enabled: true,
            grants: ["b", "d", "e", "f"]
                .into_iter()
                .map(|id| ProjectGrant {
                    project_id: id.into(),
                    actions: vec![ProjectAction::Read],
                })
                .collect(),
        })
        .unwrap();
        let credential = app.issue_credential("mixed-reader").unwrap();
        app.set_caller(app.authenticate(&credential.secret).unwrap());
        let first = app
            .project_library(&LibraryInput {
                cursor: None,
                limit: 1,
            })
            .unwrap();
        assert_eq!(first.items[0].project_id, "b");
        let cursor = first.next_cursor.unwrap();
        let second = app
            .project_library(&LibraryInput {
                cursor: Some(cursor.clone()),
                limit: 1,
            })
            .unwrap();
        assert_eq!(second.items[0].project_id, "d");
        assert!(second.next_cursor.is_some());
        let modern = app
            .project_library(&LibraryInput {
                cursor: None,
                limit: 100,
            })
            .unwrap();
        assert_eq!(
            modern
                .items
                .iter()
                .map(|p| p.project_id.as_str())
                .collect::<Vec<_>>(),
            ["b", "d", "e", "f"]
        );
        app.set_caller(Caller::LocalOwner);
        assert_eq!(
            app.project_library(&LibraryInput {
                cursor: Some(cursor),
                limit: 1
            })
            .unwrap_err()
            .code,
            "INVALID_CURSOR"
        );
    }

    #[test]
    fn library_duration_uses_exact_main_scene_end_and_metadata() {
        let mut app = EditorApplication::open(":memory:").unwrap();
        let initial = create(&mut app, "source");
        let applied=app.apply_historical_visual_project(&serde_json::from_value(json!({"project_id":"source","expected_revision":initial,"label":"Shape","idempotency_key":"shape","operations":[{"type":"base.apply","operation":{"type":"track.create","sequence_id":"main","track":{"id":"graphics","name":"Graphics","kind":"graphic","enabled":true,"muted":false,"locked":false,"clips":[],"params":{}}}},{"type":"shape.put","sequence_id":"main","shape":{"id":"shape","track_id":"graphics","timeline_range":{"start":{"ticks":"1","timebase":3},"duration":{"ticks":"1","timebase":6}},"primitive":{"kind":"rectangle","width_px":20.0,"height_px":20.0},"fill":{"red":255,"green":255,"blue":255,"alpha":255},"position_x":0.0,"position_y":0.0,"rotation_degrees":0.0,"opacity":1.0}}]})).unwrap()).unwrap();
        let page = app
            .project_library(&LibraryInput {
                cursor: None,
                limit: 1,
            })
            .unwrap();
        let row = &page.items[0];
        assert_eq!(
            row.duration.ticks.parse::<i64>().unwrap() * 2,
            i64::from(row.duration.timebase)
        );
        assert_eq!(row.width, 640);
        assert_eq!(row.scene_count, 1);
        assert!(row.created_at.ends_with('Z'));
        assert!(row.modified_at.ends_with('Z'));
        let copy = app
            .duplicate_projects(&request("source", &applied.revision, "copy"))
            .unwrap();
        let snapshot = inspect(&app, "copy");
        assert_eq!(
            snapshot.project.visual,
            inspect(&app, "source").project.visual
        );
        assert_eq!(snapshot.revision, copy.projects[0].revision);
        let first = app
            .project_library(&LibraryInput {
                cursor: None,
                limit: 1,
            })
            .unwrap();
        let second = app
            .project_library(&LibraryInput {
                cursor: first.next_cursor,
                limit: 1,
            })
            .unwrap();
        assert_ne!(first.items[0].project_id, second.items[0].project_id);
        assert!(second.next_cursor.is_none());
    }
    #[test]
    fn legacy_project_copy_preserves_pinned_project_grants_without_credentials() {
        use opencut_editor_application::{
            presets::PresetGrantStore, project_plugins::ProjectPluginStore,
        };
        let mut app = EditorApplication::open(":memory:").unwrap();
        let source=app.create_project(serde_json::from_value(json!({"id":"source","name":"Legacy","sequence_id":"main","width":640,"height":360,"fps":{"numerator":25,"denominator":1}})).unwrap(),"source").unwrap();
        let target = json!({"project_id":"source","id":"org.opencut.fixture","version":"1.0.0"});
        let planner=app.set_plugin_grant(&serde_json::from_value(json!({"target":target,"package_sha256":"a".repeat(64),"operations":["org.opencut.fixture.insert"],"enabled":false,"expected_revision":"0","idempotency_key":"planner"})).unwrap()).unwrap();
        let preset=app.set_preset_grant(&serde_json::from_value(json!({"target":target,"package_sha256":"b".repeat(64),"preset_names":["title"],"enabled":true,"expected_revision":"0","idempotency_key":"preset"})).unwrap()).unwrap();
        app.duplicate_projects(&request("source", &source.revision, "copy"))
            .unwrap();
        let mut expected = source.project;
        expected.id = "copy".into();
        expected.name = "Copy source".into();
        assert_eq!(app.snapshot("copy").unwrap().project, expected);
        let copied_planner = app.plugin_grants("copy").unwrap().remove(0);
        let copied_preset = app.preset_grants("copy").unwrap().remove(0);
        assert_eq!(copied_planner.target.project_id, "copy");
        assert_eq!(copied_planner.package_sha256, planner.package_sha256);
        assert_eq!(copied_planner.operations, planner.operations);
        assert!(!copied_planner.enabled);
        assert_eq!(copied_preset.target.project_id, "copy");
        assert_eq!(copied_preset.package_sha256, preset.package_sha256);
        assert_eq!(copied_preset.preset_names, preset.preset_names);
        assert!(copied_preset.enabled);
        assert_eq!(
            app.connection
                .query_row::<i64, _, _>(
                    "SELECT count(*) FROM access_grants WHERE project_id='copy'",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            app.project_library(&LibraryInput {
                cursor: None,
                limit: 100
            })
            .unwrap()
            .items[0]
                .format,
            ProjectFormat::Base
        );
    }

    #[test]
    fn migration_from_previous_database_is_atomic_and_reopens() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("workspace.sqlite");
        let mut app = EditorApplication::open(&db).unwrap();
        let revision = create(&mut app, "source");
        app.connection.execute_batch("DROP TABLE audio_preview_recovery_v1; DROP TABLE font_derivations;
DROP TABLE edit_recovery; DROP TABLE deleted_projects; DROP VIEW available_project_media; DROP TABLE project_media_bindings; UPDATE metadata SET value='24' WHERE key='schema_version'; CREATE TABLE project_media_bindings(collision TEXT);").unwrap();
        drop(app);
        assert!(EditorApplication::open(&db).is_err());
        let conn = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(
            conn.query_row::<String, _, _>(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            "24"
        );
        conn.execute_batch("DROP TABLE project_media_bindings")
            .unwrap();
        drop(conn);
        let mut app = EditorApplication::open(&db).unwrap();
        let result = app
            .duplicate_projects(&request("source", &revision, "copy"))
            .unwrap();
        drop(app);
        let app = EditorApplication::open(&db).unwrap();
        assert_eq!(inspect(&app, "copy").revision, result.projects[0].revision);
    }
}
