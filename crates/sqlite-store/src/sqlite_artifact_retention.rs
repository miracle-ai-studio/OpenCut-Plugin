//! Durable owner release of the default retention root for published artifacts.
use super::*;
use opencut_editor_application::artifact_retention::{
    self, ReferencesView, RetentionState, Store, View,
};

pub(super) const MIGRATION: &str = "
CREATE TABLE artifact_retention(
  artifact_id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES jobs(job_id),
  idempotency_key TEXT NOT NULL UNIQUE,
  released_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  request_id TEXT NOT NULL,
  actor_json TEXT NOT NULL
);
CREATE INDEX artifact_retention_job_idx ON artifact_retention(job_id);
UPDATE metadata SET value='16' WHERE key='schema_version' AND value='15';
";

pub(super) fn published(
    c: &Connection,
    artifact_id: &str,
) -> Result<
    (
        JobRecord,
        opencut_api_contracts::job_views::ArtifactReference,
    ),
    EditorError,
> {
    let (job_id, _) = opencut_editor_application::job_views::parse_artifact_id(artifact_id)?;
    let job = load_job(c, job_id)?;
    if job.state != JobState::Succeeded {
        return Err(EditorError::new(
            "NOT_FOUND",
            "Published artifact not found",
        ));
    }
    let reference = opencut_editor_application::job_views::project(job.clone())
        .artifacts
        .into_iter()
        .find(|reference| reference.artifact_id == artifact_id)
        .ok_or_else(|| EditorError::new("NOT_FOUND", "Published artifact not found"))?;
    Ok((job, reference))
}

fn view(c: &Connection, artifact_id: &str) -> Result<View, EditorError> {
    let (job, reference) = published(c, artifact_id)?;
    let released: Option<(String, String)> = c
        .query_row(
            "SELECT released_at,request_id FROM artifact_retention WHERE artifact_id=?1",
            [artifact_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage_error)?;
    Ok(View {
        artifact_id: artifact_id.into(),
        job_id: job.job_id,
        content_hash: reference.content_hash,
        size_bytes: reference.size_bytes,
        state: if released.is_some() {
            RetentionState::Released
        } else {
            RetentionState::Retained
        },
        released_at: released.as_ref().map(|row| row.0.clone()),
        release_request_id: released.map(|row| row.1),
    })
}

impl Store for EditorApplication {
    fn retention_context(&self) -> opencut_editor_application::audit::ExecutionContext {
        self.connection.1.clone()
    }

    fn retention_inspect(&self, artifact_id: &str) -> Result<View, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        view(&tx, artifact_id)
    }

    fn retention_references(&self, artifact_id: &str) -> Result<ReferencesView, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        let (job, reference) = published(&tx, artifact_id)?;
        let path = opencut_editor_application::job_views::private_artifact_path(&job, &reference)?;
        let references = sqlite_artifact_references::scan(&tx, &job.job_id, artifact_id, path)?;
        let retained: bool = tx
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM artifact_retention WHERE artifact_id=?1)",
                [artifact_id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        let root_count = references.roots.len();
        const MAX_VISIBLE_ROOTS: usize = 100;
        Ok(ReferencesView {
            artifact_id: artifact_id.into(),
            default_retained: retained,
            roots: references
                .roots
                .into_iter()
                .take(MAX_VISIBLE_ROOTS)
                .collect(),
            root_count: root_count.to_string(),
            roots_truncated: root_count > MAX_VISIBLE_ROOTS,
            examined_rows: references.examined_rows.to_string(),
            native_source_candidates: references.native_paths.len().to_string(),
        })
    }

    fn retention_release(
        &mut self,
        input: &artifact_retention::ReleaseInput,
    ) -> Result<View, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        let context = self.connection.1.clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let (job, _) = published(&tx, &input.artifact_id)?;
        let prior_key_target: Option<String> = tx
            .query_row(
                "SELECT artifact_id FROM artifact_retention WHERE idempotency_key=?1",
                [&input.idempotency_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        if let Some(target) = prior_key_target {
            if target != input.artifact_id {
                return Err(EditorError::new(
                    "IDEMPOTENCY_KEY_REUSED",
                    "Retention key belongs to another artifact",
                ));
            }
            return view(&tx, &input.artifact_id);
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT idempotency_key FROM artifact_retention WHERE artifact_id=?1",
                [&input.artifact_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        if existing.is_some() {
            return Err(EditorError::new(
                "ARTIFACT_ALREADY_RELEASED",
                "Artifact was released under another idempotency key; inspect its retention state",
            ));
        }
        tx.execute(
            "INSERT INTO artifact_retention(artifact_id,job_id,idempotency_key,request_id,actor_json) VALUES (?1,?2,?3,?4,?5)",
            params![
                input.artifact_id,
                job.job_id,
                input.idempotency_key,
                context.request_id(),
                encode_json(context.actor(), "artifact release actor")?
            ],
        )
        .map_err(storage_error)?;
        sqlite_audit::job(&tx, &context, &job, "artifact.released", false)?;
        let result = view(&tx, &input.artifact_id)?;
        tx.commit().map_err(storage_error)?;
        Ok(result)
    }
}
