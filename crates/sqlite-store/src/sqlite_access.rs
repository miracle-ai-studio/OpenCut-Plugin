//! Credential persistence and native implementation of shared project policy.
use super::*;
use opencut_editor_application::{
    access::{self, *},
    audit::ExecutionContext,
};
use sha2::{Digest, Sha256};

pub(super) fn migrate(tx: &Connection) -> Result<(), EditorError> {
    tx.execute_batch("CREATE TABLE access_principals(principal_id TEXT PRIMARY KEY,revision INTEGER NOT NULL,enabled INTEGER NOT NULL);
      CREATE TABLE access_grants(principal_id TEXT NOT NULL REFERENCES access_principals(principal_id),project_id TEXT NOT NULL REFERENCES projects(project_id),action_mask INTEGER NOT NULL,PRIMARY KEY(principal_id,project_id));
      CREATE TABLE access_credentials(credential_id TEXT PRIMARY KEY,principal_id TEXT NOT NULL REFERENCES access_principals(principal_id),secret_digest TEXT NOT NULL UNIQUE,revoked INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE access_events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,principal_id TEXT NOT NULL,credential_id TEXT,action TEXT NOT NULL,request_id TEXT NOT NULL,actor_json TEXT NOT NULL,policy_json TEXT,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
      CREATE TABLE job_projects(job_id TEXT PRIMARY KEY REFERENCES jobs(job_id),project_id TEXT NOT NULL REFERENCES projects(project_id),revision TEXT NOT NULL REFERENCES revisions(revision));
      CREATE INDEX job_projects_project_idx ON job_projects(project_id,job_id);").map_err(storage_error)?;
    // Stream historical inputs rather than retaining every task payload in RAM.
    {
        let mut statement = tx
            .prepare("SELECT job_id,kind,input_json FROM jobs")
            .map_err(storage_error)?;
        let records = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(storage_error)?;
        for record in records {
            let (id, kind, input) = record.map_err(storage_error)?;
            // Invalid historical inputs retain owner-only access, never gain a grant.
            if let Ok(input) = serde_json::from_str(&input) {
                bind_job(tx, &id, &kind, &input)?;
            }
        }
    }
    tx.execute(
        "UPDATE metadata SET value='5' WHERE key='schema_version' AND value='4'",
        [],
    )
    .map_err(storage_error)?;
    Ok(())
}
const ACTIONS: [ProjectAction; 7] = [
    ProjectAction::Read,
    ProjectAction::Edit,
    ProjectAction::Restore,
    ProjectAction::RunJobs,
    ProjectAction::CancelJobs,
    ProjectAction::ReadArtifacts,
    ProjectAction::ReadAudit,
];
fn grants(connection: &Connection, principal: &str) -> Result<Vec<ProjectGrant>, EditorError> {
    let mut statement=connection.prepare_cached("SELECT project_id,action_mask FROM access_grants WHERE principal_id=?1 ORDER BY project_id").map_err(storage_error)?;
    statement
        .query_map([principal], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))
        })
        .map_err(storage_error)?
        .map(|row| {
            let (project_id, mask) = row.map_err(storage_error)?;
            Ok(ProjectGrant {
                project_id,
                actions: ACTIONS
                    .iter()
                    .copied()
                    .filter(|a| mask & a.mask() != 0)
                    .collect(),
            })
        })
        .collect()
}
struct Read<'a>(&'a Connection);
impl AuthorizationRead for Read<'_> {
    fn current_grants(&self, caller: &Caller) -> Result<Vec<ProjectGrant>, EditorError> {
        let Caller::Credential(caller) = caller else {
            return Ok(vec![]);
        };
        let valid:bool=self.0.prepare_cached("SELECT EXISTS(SELECT 1 FROM access_credentials c JOIN access_principals p USING(principal_id) WHERE c.credential_id=?1 AND c.principal_id=?2 AND c.revoked=0 AND p.enabled=1)").map_err(storage_error)?.query_row(params![caller.credential_id(),caller.principal_id()],|r|r.get(0)).map_err(storage_error)?;
        if !valid {
            return Err(access::denied());
        }
        grants(self.0, caller.principal_id())
    }
}
pub(super) fn require(
    connection: &Connection,
    context: &ExecutionContext,
    project: &str,
    action: ProjectAction,
) -> Result<(), EditorError> {
    access::require(&Read(connection), context.caller(), project, action)?;
    sqlite_project_library::require_live(connection, project)
}
pub(super) fn principal(
    connection: &Connection,
    context: &ExecutionContext,
) -> Result<Option<String>, EditorError> {
    if context.caller().is_owner() {
        return Ok(None);
    }
    Read(connection).current_grants(context.caller())?;
    Ok(Some(context.caller().principal_id().into()))
}
pub(super) fn owner(context: &ExecutionContext) -> Result<(), EditorError> {
    if context.caller().is_owner() {
        Ok(())
    } else {
        Err(access::denied())
    }
}
pub(super) fn job_project(
    connection: &Connection,
    id: &str,
) -> Result<Option<String>, EditorError> {
    connection
        .query_row(
            "SELECT project_id FROM job_projects WHERE job_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage_error)
}
pub(super) fn require_job(
    connection: &Connection,
    context: &ExecutionContext,
    id: &str,
    action: ProjectAction,
) -> Result<(), EditorError> {
    if context.caller().is_owner() {
        return Ok(());
    }
    let project = job_project(connection, id)?.ok_or_else(access::denied)?;
    require(connection, context, &project, action)
}
pub(super) fn verified_target(
    connection: &Connection,
    kind: &str,
    input: &Value,
) -> Result<Option<(String, String)>, EditorError> {
    if matches!(
        kind,
        "render.project_audio.v4" | "render.project_audio_range.v4"
    ) {
        return sqlite_scene_audio_jobs::verified_target(connection, input);
    }
    if kind == "analysis.asset.v4" {
        return asset_analysis::verified_target(connection, input);
    }
    if kind == "analysis.sequence_transcribe.v4" {
        return sqlite_scene_transcription::verified_target(connection, input);
    }
    if kind == "render.project_video.v4" {
        return sqlite_media_video_jobs::verified_target(connection, input);
    }
    if kind == "render.project_caption_video.v4" {
        return sqlite_scene_caption_video_jobs::verified_target(connection, input);
    }
    if kind == "render.project_graphic_video.v4" {
        return sqlite_scene_graphic_video_jobs::verified_target(connection, input);
    }
    if matches!(
        kind,
        "render.export_video.v2" | "render.export_video_range.v1"
    ) {
        return export_video::verified_target(connection, input);
    }
    if kind == "render.project_scene_video.v4" {
        return sqlite_scene_video_jobs::verified_target(connection, input);
    }
    if kind == "render.project_mask_video.v4" {
        return sqlite_scene_mask_video_jobs::verified_target(connection, input);
    }
    if kind == "render.project_blend_video.v4" {
        return sqlite_scene_blend_video_jobs::verified_target(connection, input);
    }
    if matches!(
        kind,
        "render.project_text_frame.v4"
            | "render.project_caption_frame.v4"
            | "render.project_graphic_frame.v4"
            | "render.project_mask_frame.v4"
            | "render.project_scene_frame.v4"
            | "render.project_blend_frame.v4"
    ) {
        return sqlite_scene_text_frame_jobs::verified_target(connection, input);
    }
    if kind == "render.project_frame.v4" {
        return sqlite_media_frame_jobs::verified_target(connection, input);
    }
    if matches!(
        kind,
        "preview.project_frame.v3"
            | "preview.project_range.v3"
            | "render.project.v3"
            | "analysis.sequence_transcribe"
    ) {
        let Some(snapshot) = input.get("snapshot") else {
            return Ok(None);
        };
        let Ok(snapshot) = serde_json::from_value::<
            opencut_contracts::historical_visual::HistoricalVisualSnapshot,
        >(snapshot.clone()) else {
            return Ok(None);
        };
        let stored = sqlite_historical_visual::load_historical_visual_snapshot(
            connection,
            &snapshot.project.base.id,
            Some(&snapshot.revision),
        );
        return match stored {
            Ok(stored) if stored == snapshot => {
                Ok(Some((snapshot.project.base.id, snapshot.revision)))
            }
            Ok(_) => Ok(None),
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "NOT_FOUND" | "UNSUPPORTED_PROJECT_VERSION"
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        };
    }
    if !matches!(
        kind,
        "preview.project_frame"
            | "preview.composited_frame"
            | "preview.project_range"
            | "render.project"
            | "analysis.transcribe"
            | "analysis.silence"
            | "analysis.shots"
            | "analysis.waveform"
            | "analysis.speaker_turns"
    ) {
        return Ok(None);
    }
    let Some(snapshot) = input.get("snapshot") else {
        return Ok(None);
    };
    let Ok(snapshot) = serde_json::from_value::<ProjectSnapshot>(snapshot.clone()) else {
        return Ok(None);
    };
    let stored: Option<String> = connection
        .query_row(
            "SELECT document_json FROM revisions WHERE project_id=?1 AND revision=?2",
            params![snapshot.project.id, snapshot.revision],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let stored = if kind.starts_with("analysis.") {
        match sqlite_project_document::decode_stored_project(&stored)? {
            sqlite_project_document::ProjectStorageDocument::Legacy(project) => project,
            sqlite_project_document::ProjectStorageDocument::HistoricalVisual(project) => {
                project.base
            }
            sqlite_project_document::ProjectStorageDocument::ReadOnly(_) => {
                return Err(opencut_editor_application::project_readonly::readonly_error());
            }
            sqlite_project_document::ProjectStorageDocument::Editable(_) => {
                return Err(EditorError::new(
                    "UNSUPPORTED_PROJECT_VERSION",
                    "Schema 4 jobs require a schema 4 execution snapshot",
                ));
            }
        }
    } else {
        sqlite_project_document::decode_legacy_project(&stored)?
    };
    if stored != snapshot.project {
        return Ok(None);
    }
    Ok(Some((snapshot.project.id, snapshot.revision)))
}
pub(super) fn bind_job(
    connection: &Connection,
    id: &str,
    kind: &str,
    input: &Value,
) -> Result<(), EditorError> {
    if let Some((project, revision)) = verified_target(connection, kind, input)? {
        connection
            .execute(
                "INSERT INTO job_projects(job_id,project_id,revision) VALUES (?1,?2,?3)",
                params![id, project, revision],
            )
            .map_err(storage_error)?;
    }
    Ok(())
}
pub(super) fn require_live_input(
    connection: &Connection,
    input: &Value,
) -> Result<(), EditorError> {
    if let Some(project) = input.get("snapshot").and_then(|s| s.get("project"))
        && let Some(id) = project
            .get("id")
            .or_else(|| project.get("base").and_then(|p| p.get("id")))
            .and_then(Value::as_str)
    {
        sqlite_project_library::require_live(connection, id)?;
    }
    Ok(())
}
pub(super) fn require_submission(
    connection: &Connection,
    context: &ExecutionContext,
    request: &JobRequest,
) -> Result<(), EditorError> {
    if matches!(
        request.kind.as_str(),
        "render.project_audio.v4"
            | "render.project_audio_range.v4"
            | "render.project_frame.v4"
            | "render.project_text_frame.v4"
            | "render.project_caption_frame.v4"
            | "render.project_graphic_frame.v4"
            | "render.project_blend_frame.v4"
            | "render.project_scene_frame.v4"
            | "render.project_video.v4"
            | "render.project_caption_video.v4"
            | "render.project_graphic_video.v4"
            | "render.project_mask_video.v4"
            | "render.project_scene_video.v4"
            | "render.export_video.v2"
            | "render.export_video_range.v1"
            | "render.project_blend_video.v4"
            | "analysis.sequence_transcribe.v4"
            | "analysis.asset.v4"
    ) {
        let (project, _) = verified_target(connection, &request.kind, &request.input)?
            .ok_or_else(access::denied)?;
        require(connection, context, &project, ProjectAction::Read)?;
        require(connection, context, &project, ProjectAction::RunJobs)?;
    }
    if context.caller().is_owner() {
        require_live_input(connection, &request.input)?;
        return Ok(());
    }
    let (project, _) =
        verified_target(connection, &request.kind, &request.input)?.ok_or_else(access::denied)?;
    require(connection, context, &project, ProjectAction::RunJobs)?;
    // A workspace-scoped receipt is not a resource capability. Check its original
    // job before comparing request hashes or returning an idempotent result.
    let stored:Option<String>=connection.query_row("SELECT result_json FROM idempotency WHERE operation_family='create_job' AND scope='workspace' AND idempotency_key=?1",[&request.idempotency_key],|r|r.get(0)).optional().map_err(storage_error)?;
    if let Some(stored) = stored {
        let job: JobRecord = decode_json(&stored, "job receipt")?;
        require_job(connection, context, &job.job_id, ProjectAction::RunJobs)?;
    }
    Ok(())
}
fn event(
    connection: &Connection,
    context: &ExecutionContext,
    principal: &str,
    credential: Option<&str>,
    action: &str,
    policy: Option<String>,
) -> Result<(), EditorError> {
    connection.execute("INSERT INTO access_events(principal_id,credential_id,action,request_id,actor_json,policy_json) VALUES (?1,?2,?3,?4,?5,?6)",params![principal,credential,action,context.request_id(),encode_json(context.actor(),"access actor")?,policy]).map_err(storage_error)?;
    Ok(())
}
impl EditorApplication {
    pub fn access_audit_query(
        &self,
        input: AccessAuditQuery,
    ) -> Result<AccessAuditPage, EditorError> {
        opencut_editor_application::access_audit::query(
            &self.connection,
            self.connection.1.caller(),
            input,
        )
    }
    pub fn set_caller(&mut self, caller: Caller) {
        let source = self.connection.1.actor().source;
        self.connection.1 = ExecutionContext::for_caller(source, caller);
    }
    pub fn authenticate(&self, secret: &str) -> Result<Caller, EditorError> {
        if secret.len() != 64 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(access::denied());
        }
        let digest = format!("{:x}", Sha256::digest(secret.as_bytes()));
        let identity:Option<(String,String)>=self.connection.query_row("SELECT c.principal_id,c.credential_id FROM access_credentials c JOIN access_principals p USING(principal_id) WHERE c.secret_digest=?1 AND c.revoked=0 AND p.enabled=1",[digest],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(storage_error)?;
        identity
            .map(|(p, c)| Caller::Credential(VerifiedCredential::from_verified(p, c)))
            .ok_or_else(access::denied)
    }
    pub fn access_view(&self) -> Result<AccessView, EditorError> {
        let context = &self.connection.1;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        let grants = Read(&tx).current_grants(context.caller())?;
        let (credential_id, policy_revision) = match context.caller() {
            Caller::LocalOwner => (None, None),
            Caller::Credential(c) => (
                Some(c.credential_id().into()),
                Some(
                    tx.query_row::<i64, _, _>(
                        "SELECT revision FROM access_principals WHERE principal_id=?1",
                        [c.principal_id()],
                        |r| r.get(0),
                    )
                    .map_err(storage_error)?
                    .to_string(),
                ),
            ),
        };
        Ok(AccessView {
            principal_id: context.caller().principal_id().into(),
            credential_id,
            owner: context.caller().is_owner(),
            policy_revision,
            grants,
        })
    }
    pub fn set_principal_policy(
        &mut self,
        policy: PrincipalPolicy,
    ) -> Result<PrincipalView, EditorError> {
        owner(&self.connection.1)?;
        access::validate_policy(&policy)?;
        let context = self.connection.1.clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT revision FROM access_principals WHERE principal_id=?1",
                [&policy.principal_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        let expected: i64 = policy
            .expected_revision
            .parse()
            .expect("validated revision");
        if current.unwrap_or(0) != expected {
            return Err(EditorError::new(
                "REVISION_CONFLICT",
                "Principal policy changed; inspect before updating",
            ));
        }
        for grant in &policy.grants {
            sqlite_project_document::project_head_revision(&tx, &grant.project_id)?;
        }
        let revision = expected + 1;
        tx.execute("INSERT INTO access_principals(principal_id,revision,enabled) VALUES (?1,?2,?3) ON CONFLICT(principal_id) DO UPDATE SET revision=excluded.revision,enabled=excluded.enabled",params![policy.principal_id,revision,policy.enabled]).map_err(storage_error)?;
        tx.execute(
            "DELETE FROM access_grants WHERE principal_id=?1",
            [&policy.principal_id],
        )
        .map_err(storage_error)?;
        for g in &policy.grants {
            let mask = g.actions.iter().fold(0, |m, a| m | a.mask());
            tx.execute(
                "INSERT INTO access_grants(principal_id,project_id,action_mask) VALUES (?1,?2,?3)",
                params![policy.principal_id, g.project_id, mask],
            )
            .map_err(storage_error)?;
        }
        let view = PrincipalView {
            principal_id: policy.principal_id,
            revision: revision.to_string(),
            enabled: policy.enabled,
            grants: policy.grants,
        };
        event(
            &tx,
            &context,
            &view.principal_id,
            None,
            "policy.updated",
            Some(encode_json(&view, "principal policy")?),
        )?;
        tx.commit().map_err(storage_error)?;
        Ok(view)
    }
    pub fn principal_policy(&self, id: &str) -> Result<PrincipalView, EditorError> {
        owner(&self.connection.1)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        let (revision, enabled) = tx
            .query_row(
                "SELECT revision,enabled FROM access_principals WHERE principal_id=?1",
                [id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?)),
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| EditorError::new("NOT_FOUND", "Principal was not found"))?;
        Ok(PrincipalView {
            principal_id: id.into(),
            revision: revision.to_string(),
            enabled,
            grants: grants(&tx, id)?,
        })
    }
    pub fn issue_credential(&mut self, id: &str) -> Result<IssuedCredential, EditorError> {
        owner(&self.connection.1)?;
        let context = self.connection.1.clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let enabled: Option<bool> = tx
            .query_row(
                "SELECT enabled FROM access_principals WHERE principal_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        if enabled != Some(true) {
            return Err(access::denied());
        }
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let credential_id = format!("credential_{}", Uuid::new_v4().simple());
        tx.execute("INSERT INTO access_credentials(credential_id,principal_id,secret_digest) VALUES (?1,?2,?3)",params![credential_id,id,format!("{:x}",Sha256::digest(secret.as_bytes()))]).map_err(storage_error)?;
        event(
            &tx,
            &context,
            id,
            Some(&credential_id),
            "credential.issued",
            None,
        )?;
        tx.commit().map_err(storage_error)?;
        Ok(IssuedCredential {
            principal_id: id.into(),
            credential_id,
            secret,
        })
    }
    pub fn revoke_credential(&mut self, id: &str) -> Result<(), EditorError> {
        owner(&self.connection.1)?;
        let context = self.connection.1.clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let row: Option<(String, bool)> = tx
            .query_row(
                "SELECT principal_id,revoked FROM access_credentials WHERE credential_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(storage_error)?;
        let (principal, revoked) =
            row.ok_or_else(|| EditorError::new("NOT_FOUND", "Credential was not found"))?;
        if !revoked {
            tx.execute(
                "UPDATE access_credentials SET revoked=1 WHERE credential_id=?1",
                [id],
            )
            .map_err(storage_error)?;
            event(
                &tx,
                &context,
                &principal,
                Some(id),
                "credential.revoked",
                None,
            )?;
        }
        tx.commit().map_err(storage_error)?;
        Ok(())
    }
}

impl opencut_editor_application::access_audit::AccessAuditRead for SqliteConnection {
    fn visit_access_audit(
        &self,
        target: &AccessAuditTarget,
        after: i64,
        limit: usize,
        visitor: &mut dyn FnMut(AccessAuditRecord) -> Result<bool, EditorError>,
    ) -> Result<(), EditorError> {
        owner(&self.1)?;
        let tx = self.unchecked_transaction().map_err(storage_error)?;
        let (filter, id) = match target {
            AccessAuditTarget::Workspace => ("1=1", ""),
            AccessAuditTarget::Principal(id) => ("principal_id=?2", id.as_str()),
            AccessAuditTarget::Credential(id) => ("credential_id=?2", id.as_str()),
            AccessAuditTarget::Request(id) => ("request_id=?2", id.as_str()),
        };
        let mut statement = tx.prepare(&format!("SELECT sequence,principal_id,credential_id,action,request_id,actor_json,policy_json,created_at FROM access_events WHERE sequence>?1 AND {filter} AND ?2 IS NOT NULL ORDER BY sequence LIMIT ?3")).map_err(storage_error)?;
        let mut rows = statement
            .query(params![after, id, limit as i64])
            .map_err(storage_error)?;
        while let Some(row) = rows.next().map_err(storage_error)? {
            let actor: String = row.get(5).map_err(storage_error)?;
            let policy: Option<String> = row.get(6).map_err(storage_error)?;
            let action: String = row.get(3).map_err(storage_error)?;
            let record = AccessAuditRecord {
                sequence: row.get::<_, i64>(0).map_err(storage_error)?.to_string(),
                principal_id: row.get(1).map_err(storage_error)?,
                credential_id: row.get(2).map_err(storage_error)?,
                action: serde_json::from_value(json!(action)).map_err(|_| {
                    EditorError::new("STORAGE_ERROR", "Invalid stored access audit action")
                })?,
                request_id: row.get(4).map_err(storage_error)?,
                actor: decode_json(&actor, "access audit actor")?,
                policy: policy
                    .map(|value| decode_json(&value, "access audit policy"))
                    .transpose()?,
                created_at: row.get(7).map_err(storage_error)?,
            };
            if !visitor(record)? {
                break;
            }
        }
        Ok(())
    }
}
