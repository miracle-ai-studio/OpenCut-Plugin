//! Durable, owner-only artifact collection plans and execution receipts.
use super::*;
use opencut_api_contracts::{
    artifact_gc::{
        ExecuteInput, ExecutionState, ExecutionView, Page, Plan, PlanInput, Query, Summary,
    },
    artifact_retention::ReferenceRoot,
};
use opencut_editor_application::artifact_gc::{
    ExecutionSubject, Native, Observation, Removal, Snapshot, Source, Store,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(super) const MIGRATION: &str = "
CREATE TABLE artifact_gc_plans(
  plan_id TEXT PRIMARY KEY,
  idempotency_key TEXT NOT NULL UNIQUE,
  input_json TEXT NOT NULL,
  plan_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
UPDATE metadata SET value='17' WHERE key='schema_version' AND value='16';
";

#[derive(Serialize, Deserialize)]
struct StoredPlan {
    view: Plan,
    observation: Observation,
    all_roots: BTreeSet<ReferenceRoot>,
    alias_roots: BTreeSet<ReferenceRoot>,
}

fn identity_hash(stored: &StoredPlan) -> Result<String, EditorError> {
    let mut view = stored.view.clone();
    view.plan_hash.clear();
    Ok(format!(
        "{:x}",
        Sha256::digest(
            encode_json(
                &(
                    &view,
                    &stored.observation,
                    &stored.all_roots,
                    &stored.alias_roots,
                ),
                "artifact plan identity"
            )?
            .as_bytes()
        )
    ))
}

fn load_stored(connection: &Connection, plan_id: &str) -> Result<StoredPlan, EditorError> {
    let text: Option<String> = connection
        .query_row(
            "SELECT plan_json FROM artifact_gc_plans WHERE plan_id=?1",
            [plan_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    let stored: StoredPlan = decode_json(
        &text.ok_or_else(|| EditorError::new("NOT_FOUND", "Artifact plan not found"))?,
        "artifact plan",
    )?;
    if stored.view.plan_id != plan_id
        || stored.view.plan_hash != identity_hash(&stored)?
        || stored.view.root_count != stored.all_roots.len().to_string()
        || stored.view.roots
            != stored
                .all_roots
                .iter()
                .take(100)
                .cloned()
                .collect::<Vec<_>>()
        || stored.view.roots_truncated != (stored.all_roots.len() > 100)
        || stored.view.native_alias_root_count != stored.alias_roots.len().to_string()
        || stored.view.size_bytes != stored.observation.size_bytes
        || stored.view.execution_available != stored.observation.directory_identity.is_some()
    {
        return Err(EditorError::new(
            "STORAGE_ERROR",
            "Artifact plan identity is invalid",
        ));
    }
    Ok(stored)
}

fn load(connection: &Connection, plan_id: &str) -> Result<Plan, EditorError> {
    Ok(load_stored(connection, plan_id)?.view)
}

fn receipt(connection: &Connection, stored: &StoredPlan) -> Result<ExecutionView, EditorError> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT state,updated_at FROM artifact_gc_removals WHERE plan_id=?1",
            [&stored.view.plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage_error)?;
    let state = match row.as_ref().map(|(state, _)| state.as_str()) {
        None => ExecutionState::Planned,
        Some("removing") => ExecutionState::Removing,
        Some("removed") => ExecutionState::Removed,
        Some("absent") => ExecutionState::Absent,
        Some("changed") => ExecutionState::Changed,
        _ => {
            return Err(EditorError::new(
                "STORAGE_ERROR",
                "Invalid artifact removal state",
            ));
        }
    };
    Ok(ExecutionView {
        plan_id: stored.view.plan_id.clone(),
        plan_hash: stored.view.plan_hash.clone(),
        artifact_id: stored.view.artifact_id.clone(),
        state,
        durable: row.is_some(),
        updated_at: row.map(|(_, updated_at)| updated_at),
    })
}

fn checked_stored(
    connection: &Connection,
    input: &ExecuteInput,
) -> Result<StoredPlan, EditorError> {
    let stored = load_stored(connection, &input.plan_id)?;
    if stored.view.plan_hash != input.expected_plan_hash {
        return Err(EditorError::new(
            "PLAN_HASH_MISMATCH",
            "Artifact plan changed or the expected hash is incorrect",
        ));
    }
    if !stored.view.execution_available
        || stored.observation.directory_identity.is_none()
        || stored.view.default_retained
        || !stored.all_roots.is_empty()
    {
        return Err(EditorError::new(
            "ARTIFACT_PLAN_PROTECTED",
            "Artifact plan cannot authorize deletion; create a new unprotected plan",
        ));
    }
    Ok(stored)
}

fn replay(connection: &Connection, input: &PlanInput) -> Result<Option<Plan>, EditorError> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT plan_id,input_json FROM artifact_gc_plans WHERE idempotency_key=?1",
            [&input.idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage_error)?;
    row.map(|(plan_id, prior)| {
        if prior != encode_json(input, "artifact plan input")? {
            return Err(EditorError::new(
                "IDEMPOTENCY_KEY_REUSED",
                "Artifact plan key belongs to different parameters",
            ));
        }
        load(connection, &plan_id)
    })
    .transpose()
}

impl Store for EditorApplication {
    fn artifact_plan_context(&self) -> opencut_editor_application::audit::ExecutionContext {
        self.connection.1.clone()
    }

    fn artifact_plan_replay(&self, input: &PlanInput) -> Result<Option<Plan>, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        replay(&self.connection, input)
    }

    fn artifact_plan_snapshot(&self, artifact_id: &str) -> Result<Snapshot, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        let (job, reference) = sqlite_artifact_retention::published(&tx, artifact_id)?;
        let path = opencut_editor_application::job_views::private_artifact_path(&job, &reference)?;
        let references = sqlite_artifact_references::scan(&tx, &job.job_id, artifact_id, path)?;
        let default_retained: bool = tx
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM artifact_retention WHERE artifact_id=?1)",
                [artifact_id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        Ok(Snapshot {
            source: Source {
                job_id: job.job_id.clone(),
                attempt: job.attempt,
                path: path.into(),
            },
            default_retained,
            roots: references.roots,
            native_sources: references.native_paths,
            examined_rows: references.examined_rows,
        })
    }

    fn artifact_plan_create(
        &mut self,
        input: &PlanInput,
        observation: Observation,
        snapshot: Snapshot,
        alias_roots: BTreeSet<ReferenceRoot>,
    ) -> Result<Plan, EditorError> {
        let context = self.connection.1.clone();
        sqlite_access::owner(&context)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        if let Some(previous) = replay(&tx, input)? {
            return Ok(previous);
        }
        let mut all_roots = snapshot.roots;
        all_roots.extend(alias_roots.iter().cloned());
        let observed_at: String = tx
            .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
                row.get(0)
            })
            .map_err(storage_error)?;
        let view = Plan {
            plan_id: format!("agc_{}", Uuid::new_v4().simple()),
            plan_hash: String::new(),
            artifact_id: input.artifact_id.clone(),
            size_bytes: observation.size_bytes.clone(),
            observed_at,
            default_retained: snapshot.default_retained,
            roots: all_roots.iter().take(100).cloned().collect(),
            root_count: all_roots.len().to_string(),
            roots_truncated: all_roots.len() > 100,
            native_alias_root_count: alias_roots.len().to_string(),
            examined_rows: snapshot.examined_rows.to_string(),
            native_paths_examined: snapshot.native_sources.len().to_string(),
            origin_request_id: context.request_id().into(),
            actor: context.actor().clone(),
            execution_available: observation.directory_identity.is_some(),
        };
        let mut stored = StoredPlan {
            view,
            observation,
            all_roots,
            alias_roots,
        };
        stored.view.plan_hash = identity_hash(&stored)?;
        tx.execute(
            "INSERT INTO artifact_gc_plans(plan_id,idempotency_key,input_json,plan_json) VALUES (?1,?2,?3,?4)",
            params![
                stored.view.plan_id,
                input.idempotency_key,
                encode_json(input, "artifact plan input")?,
                encode_json(&stored, "artifact plan")?
            ],
        )
        .map_err(storage_error)?;
        sqlite_audit::insert(
            &tx,
            &context,
            "artifact.gc.planned",
            sqlite_audit::Links::default(),
        )?;
        tx.commit().map_err(storage_error)?;
        Ok(stored.view)
    }

    fn artifact_plan_load(&self, plan_id: &str) -> Result<Plan, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        load(&self.connection, plan_id)
    }

    fn artifact_plan_query(&self, input: &Query) -> Result<Page, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        let after = if let Some(cursor) = &input.cursor {
            let plan_id = cursor.strip_prefix("agc1:").ok_or_else(|| {
                EditorError::new("INVALID_CURSOR", "Artifact plan cursor has another format")
            })?;
            tx.query_row(
                "SELECT rowid FROM artifact_gc_plans WHERE plan_id=?1",
                [plan_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| EditorError::new("INVALID_CURSOR", "Artifact plan cursor is absent"))?
        } else {
            0
        };
        let mut statement = tx
            .prepare("SELECT plan_id FROM artifact_gc_plans WHERE rowid>?1 ORDER BY rowid LIMIT ?2")
            .map_err(storage_error)?;
        let mut ids = statement
            .query_map(params![after, (input.limit + 1) as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)?;
        let more = ids.len() > input.limit;
        ids.truncate(input.limit);
        let next_cursor = if more {
            ids.last().map(|id| format!("agc1:{id}"))
        } else {
            None
        };
        let examined = ids.len();
        let plans = ids
            .into_iter()
            .map(|id| {
                let plan = load(&tx, &id)?;
                Ok(Summary {
                    plan_id: plan.plan_id,
                    plan_hash: plan.plan_hash,
                    artifact_id: plan.artifact_id,
                    observed_at: plan.observed_at,
                    default_retained: plan.default_retained,
                    root_count: plan.root_count,
                    origin_request_id: plan.origin_request_id,
                    actor: plan.actor,
                })
            })
            .collect::<Result<Vec<_>, EditorError>>()?;
        Ok(Page {
            plans,
            next_cursor,
            examined,
        })
    }

    fn artifact_execution_subject(&self, plan_id: &str) -> Result<ExecutionSubject, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        let stored = load_stored(&tx, plan_id)?;
        let receipt = receipt(&tx, &stored)?;
        Ok(ExecutionSubject {
            plan: stored.view,
            observation: stored.observation,
            receipt,
        })
    }

    fn artifact_execution_mark(
        &mut self,
        input: &ExecuteInput,
        native: &dyn Native,
    ) -> Result<bool, EditorError> {
        let context = self.connection.1.clone();
        sqlite_access::owner(&context)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let stored = checked_stored(&tx, input)?;
        let current = receipt(&tx, &stored)?;
        if matches!(
            current.state,
            ExecutionState::Removed | ExecutionState::Absent | ExecutionState::Changed
        ) {
            return Ok(false);
        }
        let other_pending: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM artifact_gc_removals WHERE state='removing' AND plan_id<>?1)",
                [&input.plan_id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if other_pending {
            return Err(EditorError::new(
                "ARTIFACT_COLLECTION_RECOVERY_REQUIRED",
                "Recover the previous artifact removal before starting another",
            ));
        }
        let (job, reference) = sqlite_artifact_retention::published(&tx, &stored.view.artifact_id)?;
        let path = opencut_editor_application::job_views::private_artifact_path(&job, &reference)?;
        if (Source {
            job_id: job.job_id.clone(),
            attempt: job.attempt,
            path: path.into(),
        }) != stored.observation.source
        {
            return Ok(false);
        }
        let retained: bool = tx
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM artifact_retention WHERE artifact_id=?1)",
                [&stored.view.artifact_id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if retained {
            return Ok(false);
        }
        let references =
            sqlite_artifact_references::scan(&tx, &job.job_id, &stored.view.artifact_id, path)?;
        let alias_roots = native.matching_roots(&stored.observation, &references.native_paths)?;
        if !references.roots.is_empty() || !alias_roots.is_empty() {
            return Ok(false);
        }
        if current.state == ExecutionState::Removing {
            return Ok(true);
        }
        tx.execute(
            "INSERT INTO artifact_gc_removals(plan_id,state) VALUES (?1,'removing')",
            [&input.plan_id],
        )
        .map_err(storage_error)?;
        sqlite_audit::insert(
            &tx,
            &context,
            "artifact.gc.removing",
            sqlite_audit::Links {
                job_id: Some(&job.job_id),
                ..Default::default()
            },
        )?;
        tx.commit().map_err(storage_error)?;
        Ok(true)
    }

    fn artifact_execution_finish(
        &mut self,
        input: &ExecuteInput,
        outcome: Removal,
    ) -> Result<ExecutionView, EditorError> {
        let context = self.connection.1.clone();
        sqlite_access::owner(&context)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let stored = checked_stored(&tx, input)?;
        let current = receipt(&tx, &stored)?;
        if matches!(
            current.state,
            ExecutionState::Removed | ExecutionState::Absent | ExecutionState::Changed
        ) {
            return Ok(current);
        }
        let terminal = match outcome {
            Removal::Busy => {
                return Ok(ExecutionView {
                    state: ExecutionState::Busy,
                    durable: false,
                    ..current
                });
            }
            Removal::Retained if current.state != ExecutionState::Removing => {
                return Ok(ExecutionView {
                    state: ExecutionState::Retained,
                    durable: false,
                    ..current
                });
            }
            Removal::Retained | Removal::Changed => "changed",
            Removal::Absent => "absent",
            Removal::Removed => {
                if current.state != ExecutionState::Removing {
                    return Err(EditorError::new(
                        "STORAGE_ERROR",
                        "Artifact bytes were removed without a durable intent",
                    ));
                }
                "removed"
            }
        };
        if current.state == ExecutionState::Removing {
            tx.execute(
                "UPDATE artifact_gc_removals SET state=?1,updated_at=CURRENT_TIMESTAMP WHERE plan_id=?2 AND state='removing'",
                params![terminal, input.plan_id],
            )
            .map_err(storage_error)?;
        } else {
            tx.execute(
                "INSERT INTO artifact_gc_removals(plan_id,state) VALUES (?1,?2)",
                params![input.plan_id, terminal],
            )
            .map_err(storage_error)?;
        }
        sqlite_audit::insert(
            &tx,
            &context,
            &format!("artifact.gc.{terminal}"),
            sqlite_audit::Links {
                job_id: Some(&stored.observation.source.job_id),
                ..Default::default()
            },
        )?;
        let result = receipt(&tx, &stored)?;
        tx.commit().map_err(storage_error)?;
        Ok(result)
    }

    fn artifact_execution_inspect(&self, plan_id: &str) -> Result<ExecutionView, EditorError> {
        sqlite_access::owner(&self.connection.1)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(storage_error)?;
        let stored = load_stored(&tx, plan_id)?;
        receipt(&tx, &stored)
    }
}
