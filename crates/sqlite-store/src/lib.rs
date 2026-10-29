mod agent_render;
pub mod analysis;
mod asset_analysis;
#[cfg(unix)]
mod control_vfs;
pub mod export_video;
mod job_submission;
pub mod job_views;
pub mod jobs;
mod recovery_migrations;
mod revision_actors;
pub mod speaker_turns;
mod sqlite_access;
mod sqlite_artifact_gc;
mod sqlite_artifact_publication;
mod sqlite_artifact_references;
mod sqlite_artifact_retention;
mod sqlite_audio_preview_recovery;
mod sqlite_audit;
mod sqlite_branching;
mod sqlite_captions;
mod sqlite_collaboration;
mod sqlite_denials;
mod sqlite_edit_control;
mod sqlite_edit_recovery;
mod sqlite_edit_recovery_shared;
mod sqlite_font_derivations;
mod sqlite_gc;
mod sqlite_historical_snapshot_package;
mod sqlite_historical_visual;
#[cfg(test)]
mod sqlite_historical_visual_branching;
mod sqlite_historical_visual_collaboration;
pub mod sqlite_historical_visual_jobs;
mod sqlite_historical_visual_package;
mod sqlite_history_navigation;
mod sqlite_jobs;
mod sqlite_maintenance;
pub mod sqlite_media_frame_jobs;
pub mod sqlite_media_video_jobs;
mod sqlite_new_project;
mod sqlite_presets;
mod sqlite_project_archive;
mod sqlite_project_branches;
mod sqlite_project_document;
mod sqlite_project_edit;
mod sqlite_project_exchange;
mod sqlite_project_history;
mod sqlite_project_inspection;
mod sqlite_project_library;
mod sqlite_project_package;
mod sqlite_project_plugins;
mod sqlite_project_readonly;
mod sqlite_projects;
mod sqlite_queries;
mod sqlite_render;
mod sqlite_resources;
mod sqlite_scene;
pub mod sqlite_scene_audio_jobs;
pub mod sqlite_scene_blend_video_jobs;
pub mod sqlite_scene_caption_video_jobs;
pub mod sqlite_scene_graphic_video_jobs;
mod sqlite_scene_jobs;
pub mod sqlite_scene_mask_video_jobs;
pub mod sqlite_scene_text_frame_jobs;
pub mod sqlite_scene_transcription;
pub mod sqlite_scene_video_jobs;
mod sqlite_source_art;
pub mod sqlite_source_preview;
mod sqlite_upload_retention;
mod sqlite_uploads;
use std::{path::Path, time::Duration};

use identity::{RequestIdentity, request_identity as request_hash};
use opencut_contracts::{
    CreateProject, EditPlan, EditorError, ProjectSnapshot, TransactionRequest, TransactionResult,
};
use opencut_editor_application::access::{self, ProjectAction};
pub use opencut_editor_application::{JobRequest, identity, lifecycle, projects, queries};
use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

const DATABASE_SCHEMA_VERSION: i64 = 33;
const SUPPORTED_JOB_KINDS: &[&str] = &[
    "media.preview_source",
    "media.probe",
    "preview.frame",
    "preview.project_frame",
    "preview.composited_frame",
    "preview.project_range",
    "preview.project_frame.v3",
    "preview.project_range.v3",
    "render.export",
    "render.project",
    "render.project.v3",
    "render.project_audio.v4",
    "render.project_audio_range.v4",
    "render.project_frame.v4",
    "render.project_text_frame.v4",
    "render.project_caption_frame.v4",
    "render.project_graphic_frame.v4",
    "render.project_blend_frame.v4",
    "render.project_mask_frame.v4",
    "render.project_scene_frame.v4",
    "render.project_video.v4",
    "render.project_caption_video.v4",
    "render.project_graphic_video.v4",
    "render.project_blend_video.v4",
    "render.project_mask_video.v4",
    "render.project_scene_video.v4",
    "render.export_video.v2",
    "render.export_video_range.v1",
    "artifact.verify",
    "analysis.transcribe",
    "analysis.sequence_transcribe",
    "analysis.sequence_transcribe.v4",
    "analysis.asset.v4",
    "analysis.silence",
    "analysis.shots",
    "analysis.waveform",
    "analysis.speaker_turns",
];
type HistoryRow = (
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

/// SQLite-backed authority for the first, single-branch editor implementation.
///
/// A separate instance may open the same database. SQLite serializes commits and
/// every writer re-checks idempotency and the project head while holding an
/// immediate write transaction.
pub struct EditorApplication {
    connection: SqliteConnection,
    asset_store_root: std::path::PathBuf,
    _memory_store: Option<tempfile::TempDir>,
    managed_vfs: bool,
    repair_mode: bool,
    scene_job_input: std::cell::RefCell<Option<sqlite_scene_jobs::FixedJobInput>>,
}

pub use opencut_api_contracts::{JobState, RestoreRevisionRequest, RevisionPage, RevisionSummary};
pub use opencut_editor_application::{JobEvent, JobProgress, JobRecord, StoredEvent};

impl EditorApplication {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EditorError> {
        Self::open_inner(path.as_ref(), false, false)
    }

    /// Open a Host/Worker workspace through the control-admitted native VFS.
    /// Embedders using `open` retain the unmanaged compatibility path.
    pub fn open_managed(path: impl AsRef<Path>) -> Result<Self, EditorError> {
        Self::open_inner(path.as_ref(), true, false)
    }

    /// Open an existing, current-schema workspace for a whitelisted local
    /// repair operation. This path never initializes or migrates a database.
    pub fn open_managed_repair(path: impl AsRef<Path>) -> Result<Self, EditorError> {
        Self::open_inner(path.as_ref(), true, true)
    }

    fn open_inner(path: &Path, managed: bool, repair: bool) -> Result<Self, EditorError> {
        if repair && (path == Path::new(":memory:") || !cfg!(unix)) {
            return Err(EditorError::new(
                "CAPABILITY_UNAVAILABLE",
                "Database repair requires a Unix file-backed workspace",
            ));
        }
        let managed_vfs = managed && path != Path::new(":memory:") && cfg!(unix);
        if path != Path::new(":memory:")
            && let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                EditorError::new(
                    "STORAGE_ERROR",
                    format!("could not create database directory: {error}"),
                )
            })?;
        }
        #[cfg(unix)]
        let mut connection = if !managed || path == Path::new(":memory:") {
            Connection::open(path).map_err(storage_error)?
        } else if repair {
            control_vfs::open_repair(path)?
        } else {
            control_vfs::open(path)?
        };
        #[cfg(not(unix))]
        let mut connection = Connection::open(path).map_err(storage_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(storage_error)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA synchronous = FULL;",
            )
            .map_err(storage_error)?;

        match inspect_database(&connection)? {
            DatabaseState::Version(DATABASE_SCHEMA_VERSION) => {
                if repair {
                    let mode: String = connection
                        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                        .map_err(storage_error)?;
                    if !mode.eq_ignore_ascii_case("wal") {
                        return Err(EditorError::new(
                            "CAPABILITY_UNAVAILABLE",
                            "Repair requires an existing WAL database",
                        ));
                    }
                } else {
                    ensure_wal_for_file_database(&connection)?;
                }
                return Self::finish_open(connection, path, managed_vfs, repair);
            }
            DatabaseState::Version(version) if !(1..DATABASE_SCHEMA_VERSION).contains(&version) => {
                return Err(unsupported_database_version(version));
            }
            DatabaseState::UnversionedWithTables => {
                return Err(EditorError::new(
                    "UNSUPPORTED_DATABASE_VERSION",
                    "database has no OpenCut schema version marker",
                ));
            }
            DatabaseState::Empty | DatabaseState::Version(_) => {}
        }
        if repair {
            return Err(EditorError::new(
                "UNSUPPORTED_DATABASE_VERSION",
                "Repair cannot initialize or migrate an OpenCut database",
            ));
        }

        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;

        // Another process may have initialized or migrated while this connection
        // waited for the write lock, so all decisions are repeated inside it.
        let mut version = match inspect_database(&tx)? {
            DatabaseState::Version(DATABASE_SCHEMA_VERSION) => {
                tx.commit().map_err(storage_error)?;
                ensure_wal_for_file_database(&connection)?;
                return Self::finish_open(connection, path, managed_vfs, repair);
            }
            DatabaseState::Version(version) if (1..DATABASE_SCHEMA_VERSION).contains(&version) => {
                version
            }
            DatabaseState::Version(version) => return Err(unsupported_database_version(version)),
            DatabaseState::UnversionedWithTables => {
                return Err(EditorError::new(
                    "UNSUPPORTED_DATABASE_VERSION",
                    "database has no OpenCut schema version marker",
                ));
            }
            DatabaseState::Empty => 0,
        };

        if version == 0 {
            tx.execute_batch(
                "CREATE TABLE metadata (
                   key TEXT PRIMARY KEY,
                   value TEXT NOT NULL
                 );
                 CREATE TABLE projects (
                   project_id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   head_revision TEXT NOT NULL
                 );
                 CREATE TABLE revisions (
                   revision TEXT PRIMARY KEY,
                   project_id TEXT NOT NULL REFERENCES projects(project_id),
                   parent_revision TEXT,
                   transaction_id TEXT,
                   document_json TEXT NOT NULL,
                   created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE INDEX revisions_project_idx
                   ON revisions(project_id, created_at);
                 CREATE TABLE editor_transactions (
                   transaction_id TEXT PRIMARY KEY,
                   project_id TEXT NOT NULL REFERENCES projects(project_id),
                   base_revision TEXT NOT NULL,
                   revision TEXT NOT NULL REFERENCES revisions(revision),
                   request_json TEXT NOT NULL,
                   result_json TEXT NOT NULL,
                   created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE TABLE idempotency (
                   operation_family TEXT NOT NULL,
                   scope TEXT NOT NULL,
                   idempotency_key TEXT NOT NULL,
                   request_hash TEXT NOT NULL,
                   result_kind TEXT NOT NULL,
                   result_json TEXT NOT NULL,
                   created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                   PRIMARY KEY(operation_family, scope, idempotency_key)
                 );
                 CREATE TABLE outbox (
                   sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                   event_id TEXT NOT NULL UNIQUE,
                   project_id TEXT NOT NULL REFERENCES projects(project_id),
                   revision TEXT NOT NULL REFERENCES revisions(revision),
                   event_type TEXT NOT NULL,
                   payload_json TEXT NOT NULL,
                   created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                 );
                 INSERT INTO metadata(key, value) VALUES ('schema_version', '1');",
            )
            .map_err(storage_error)?;
            version = 1;
        }
        if version == 1 {
            tx.execute_batch(
                "CREATE TABLE jobs (
                   job_id TEXT PRIMARY KEY,
                   state TEXT NOT NULL,
                   attempt INTEGER NOT NULL,
                   kind TEXT NOT NULL,
                   input_json TEXT NOT NULL,
                   result_json TEXT,
                   error_json TEXT,
                   lease_owner TEXT,
                   lease_until INTEGER,
                   cancel_requested INTEGER NOT NULL DEFAULT 0,
                   created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE INDEX jobs_claim_idx
                   ON jobs(state, cancel_requested, created_at);
                 UPDATE metadata SET value = '2'
                   WHERE key = 'schema_version' AND value = '1';",
            )
            .map_err(storage_error)?;
            version = 2;
        }
        if version == 2 {
            tx.execute_batch(
                "ALTER TABLE jobs ADD COLUMN progress_json TEXT;
                     CREATE TABLE IF NOT EXISTS job_events (
                       sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                       event_id TEXT NOT NULL UNIQUE,
                       job_id TEXT NOT NULL REFERENCES jobs(job_id),
                       event_type TEXT NOT NULL,
                       state TEXT NOT NULL,
                       attempt INTEGER NOT NULL,
                       payload_json TEXT NOT NULL,
                       created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                     );
                     CREATE INDEX IF NOT EXISTS job_events_job_idx ON job_events(job_id, sequence);
                     UPDATE metadata SET value = '3' WHERE key = 'schema_version' AND value = '2';
                     ",
            )
            .map_err(storage_error)?;
            version = 3;
        }
        if version == 3 {
            tx.execute_batch(sqlite_audit::MIGRATION)
                .map_err(storage_error)?;
            version = 4;
        }
        if version == 4 {
            sqlite_access::migrate(&tx)?;
            version = 5;
        }
        if version == 5 {
            tx.execute_batch(sqlite_maintenance::MIGRATION)
                .map_err(storage_error)?;
            version = 6;
        }
        if version == 6 {
            tx.execute_batch(sqlite_maintenance::MIGRATION7)
                .map_err(storage_error)?;
            version = 7;
        }
        if version == 7 {
            tx.execute_batch(sqlite_uploads::MIGRATION)
                .map_err(storage_error)?;
            version = 8;
        }
        if version == 8 {
            tx.execute_batch(sqlite_upload_retention::MIGRATION)
                .map_err(storage_error)?;
            version = 9;
        }
        if version == 9 {
            tx.execute_batch(sqlite_project_plugins::MIGRATION)
                .map_err(storage_error)?;
            version = 10;
        }
        if version == 10 {
            tx.execute_batch(sqlite_denials::MIGRATION)
                .map_err(storage_error)?;
            version = 11;
        }
        if version == 11 {
            tx.execute_batch(sqlite_gc::MIGRATION)
                .map_err(storage_error)?;
            version = 12;
        }
        if version == 12 {
            // The workspace write protocol now requires native durable admission.
            // Even with unchanged business tables, older hosts must not reopen it
            // and silently bypass reservations. Already running hosts must stop.
            tx.execute(
                "UPDATE metadata SET value='13' WHERE key='schema_version' AND value='12'",
                [],
            )
            .map_err(storage_error)?;
            version = 13;
        }
        if version == 13 {
            // Global media, plugin snapshots and anonymous download copies now
            // participate in admission. Schema-13 hosts bypass these writers.
            tx.execute(
                "UPDATE metadata SET value='14' WHERE key='schema_version' AND value='13'",
                [],
            )
            .map_err(storage_error)?;
            version = 14;
        }
        if version == 14 {
            // Media child writers now retain attempt/admission locks and obey
            // output ceilings. Older hosts cannot safely reopen this workspace.
            tx.execute(
                "UPDATE metadata SET value='15' WHERE key='schema_version' AND value='14'",
                [],
            )
            .map_err(storage_error)?;
            version = 15;
        }
        if version == 15 {
            tx.execute_batch(sqlite_artifact_retention::MIGRATION)
                .map_err(storage_error)?;
            version = 16;
        }
        if version == 16 {
            tx.execute_batch(sqlite_artifact_gc::MIGRATION)
                .map_err(storage_error)?;
            version = 17;
        }
        if version == 17 {
            tx.execute_batch(sqlite_artifact_publication::MIGRATION)
                .map_err(storage_error)?;
            version = 18;
        }
        if version == 18 {
            // Executable artifact plans and recovery receipts require the
            // durable publication fence from schema 18. Older hosts must not
            // reopen a workspace that exposes this collection protocol.
            tx.execute(
                "UPDATE metadata SET value='19' WHERE key='schema_version' AND value='18'",
                [],
            )
            .map_err(storage_error)?;
            version = 19;
        }
        if version == 19 {
            tx.execute_batch(sqlite_branching::MIGRATION)
                .map_err(storage_error)?;
            version = 20;
        }
        if version == 20 {
            tx.execute_batch(sqlite_edit_control::MIGRATION)
                .map_err(storage_error)?;
            version = 21;
        }
        if version == 21 {
            tx.execute_batch(sqlite_presets::MIGRATION)
                .map_err(storage_error)?;
            version = 22;
        }
        if version == 22 {
            tx.execute_batch(sqlite_historical_visual::MIGRATION)
                .map_err(storage_error)?;
            version = 23;
        }
        if version == 23 {
            // Deferred upload digests are a new write protocol. Older hosts must
            // not expose pending sessions as legacy known-hash uploads.
            tx.execute(
                "UPDATE metadata SET value='24' WHERE key='schema_version' AND value='23'",
                [],
            )
            .map_err(storage_error)?;
            version = 24;
        }
        if version == 24 {
            tx.execute_batch(sqlite_project_library::MIGRATION)
                .map_err(storage_error)?;
            version = 25;
        }
        if version == 25 {
            tx.execute_batch(sqlite_project_library::DELETE_MIGRATION)
                .map_err(storage_error)?;
            version = 26;
        }
        if version == 26 {
            tx.execute_batch(sqlite_scene::MIGRATION)
                .map_err(storage_error)?;
            version = 27;
        }
        if version == 27 {
            tx.execute_batch(recovery_migrations::CREATE_TIMELINE_STORE)
                .map_err(storage_error)?;
            version = 28;
        }
        if version == 28 {
            tx.execute_batch(recovery_migrations::CREATE_INTENT_STORE)
                .map_err(storage_error)?;
            version = 29;
        }
        if version == 29 {
            tx.execute_batch(sqlite_font_derivations::MIGRATION)
                .map_err(storage_error)?;
            version = 30;
        }
        if version == 30 {
            tx.execute_batch(sqlite_audio_preview_recovery::MIGRATION)
                .map_err(storage_error)?;
            version = 31;
        }
        if version == 31 {
            // Protocol barrier: older hosts must not write schema5 preservation documents.
            tx.execute(
                "UPDATE metadata SET value='32' WHERE key='schema_version' AND value='31'",
                [],
            )
            .map_err(storage_error)?;
            version = 32;
        }
        if version == 32 {
            tx.execute_batch(recovery_migrations::CONSOLIDATE)
                .map_err(storage_error)?;
        }
        tx.commit().map_err(storage_error)?;
        ensure_wal_for_file_database(&connection)?;
        Self::finish_open(connection, path, managed_vfs, repair)
    }

    fn finish_open(
        connection: Connection,
        database_path: &Path,
        managed_vfs: bool,
        repair_mode: bool,
    ) -> Result<Self, EditorError> {
        let (memory_store, asset_store_root) = if database_path == Path::new(":memory:") {
            let directory = tempfile::tempdir().map_err(|error| {
                EditorError::new(
                    "STORAGE_ERROR",
                    format!("could not create private in-memory asset store: {error}"),
                )
            })?;
            let root = directory.path().join("asset-store");
            (Some(directory), root)
        } else {
            (
                None,
                database_path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."))
                    .join("asset-store"),
            )
        };
        Ok(Self {
            connection: SqliteConnection(
                connection,
                opencut_editor_application::audit::ExecutionContext::local(
                    opencut_api_contracts::audit::CallSource::Embedded,
                ),
            ),
            asset_store_root,
            _memory_store: memory_store,
            managed_vfs,
            repair_mode,
            scene_job_input: std::cell::RefCell::new(None),
        })
    }

    pub fn asset_store_root(&self) -> &Path {
        &self.asset_store_root
    }

    pub fn create_project(
        &mut self,
        input: CreateProject,
        idempotency_key: &str,
    ) -> Result<ProjectSnapshot, EditorError> {
        projects::create(&mut self.connection, input, idempotency_key)
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectSnapshot>, EditorError> {
        queries::WorkspaceRead::read_projects(&self.connection)
    }

    pub fn snapshot(&self, project_id: &str) -> Result<ProjectSnapshot, EditorError> {
        projects::ProjectRead::head(&self.connection, project_id)
    }

    pub fn snapshot_at(
        &self,
        project_id: &str,
        revision: &str,
    ) -> Result<ProjectSnapshot, EditorError> {
        projects::ProjectRead::revision(&self.connection, project_id, revision)
    }

    pub fn plan(
        &self,
        project_id: &str,
        request: &TransactionRequest,
    ) -> Result<EditPlan, EditorError> {
        projects::plan(&self.connection, project_id, request)
    }

    pub fn history(
        &self,
        project_id: &str,
        from_revision: Option<&str>,
        limit: usize,
    ) -> Result<RevisionPage, EditorError> {
        queries::history(&self.connection, project_id, from_revision, limit)
    }

    /// Restore contents into a new revision; never rewrite accepted history.
    pub fn restore_revision(
        &mut self,
        project_id: &str,
        request: &RestoreRevisionRequest,
    ) -> Result<TransactionResult, EditorError> {
        projects::restore(&mut self.connection, project_id, request)
    }

    pub fn apply(
        &mut self,
        project_id: &str,
        request: &TransactionRequest,
    ) -> Result<TransactionResult, EditorError> {
        let mut assets = sqlite_projects::NativeAssetPinning::new(&self.asset_store_root);
        projects::apply(&mut self.connection, &mut assets, project_id, request)
    }

    pub fn create_job(&mut self, request: JobRequest) -> Result<JobRecord, EditorError> {
        let execution_input = request.input.clone();
        self.create_prepared_job(request, execution_input)
    }

    pub fn find_existing_job(
        &self,
        request: &JobRequest,
    ) -> Result<Option<JobRecord>, EditorError> {
        sqlite_access::require_submission(&self.connection, &self.connection.1, request)?;
        validate_job_request(request)?;
        let hash = request_hash("create_job", "workspace", request)?;
        let Some(stored) = load_idempotent(
            &self.connection,
            "create_job",
            "workspace",
            &request.idempotency_key,
            &hash,
            "job",
        )?
        else {
            return Ok(None);
        };
        let original: JobRecord = decode_json(&stored, "stored job result")?;
        load_job(&self.connection, &original.job_id).map(Some)
    }

    pub fn create_prepared_job(
        &mut self,
        request: JobRequest,
        execution_input: Value,
    ) -> Result<JobRecord, EditorError> {
        let _cas_gate = opencut_asset_store::gate::Gate::shared(&self.asset_store_root)?;
        validate_job_request(&request)?;
        let hash = request_hash("create_job", "workspace", &request)?;
        let context = self.connection.1.clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        sqlite_access::require_submission(&tx, &context, &request)?;
        if request.kind == opencut_editor_application::source_preview::KIND {
            sqlite_source_preview::validate_acceptance(&tx, &context, &request, &execution_input)?;
        }
        if request.kind == "analysis.sequence_transcribe.v4" {
            sqlite_scene_transcription::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if matches!(
            request.kind.as_str(),
            "render.project_audio.v4" | "render.project_audio_range.v4"
        ) {
            sqlite_scene_audio_jobs::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if request.kind == "render.project_video.v4" {
            sqlite_media_video_jobs::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if request.kind == "render.project_caption_video.v4" {
            sqlite_scene_caption_video_jobs::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if request.kind == "render.project_graphic_video.v4" {
            sqlite_scene_graphic_video_jobs::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if matches!(
            request.kind.as_str(),
            "render.export_video.v2" | "render.export_video_range.v1"
        ) {
            export_video::validate_acceptance(&tx, &context, &request, &execution_input)?;
        }
        if request.kind == "analysis.asset.v4" {
            asset_analysis::validate_acceptance(&tx, &context, &request, &execution_input)?;
        }
        if request.kind == "render.project_scene_video.v4" {
            sqlite_scene_video_jobs::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if request.kind == "render.project_mask_video.v4" {
            sqlite_scene_mask_video_jobs::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if request.kind == "render.project_blend_video.v4" {
            sqlite_scene_blend_video_jobs::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if matches!(
            request.kind.as_str(),
            "render.project_text_frame.v4"
                | "render.project_caption_frame.v4"
                | "render.project_graphic_frame.v4"
                | "render.project_mask_frame.v4"
                | "render.project_scene_frame.v4"
                | "render.project_blend_frame.v4"
        ) {
            sqlite_scene_text_frame_jobs::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if request.kind == "render.project_frame.v4" {
            sqlite_media_frame_jobs::validate_acceptance(
                &tx,
                &context,
                &request,
                &execution_input,
            )?;
        }
        if context.caller().is_owner() {
            sqlite_access::require_live_input(&tx, &execution_input)?;
        }
        if !context.caller().is_owner()
            // This source-only canonical declaration deliberately omits the
            // producer revision. Its typed identity, fixed snapshot and caller
            // are compared above in this same acceptance transaction.
            && request.kind != opencut_editor_application::source_preview::KIND
            && sqlite_access::verified_target(&tx, &request.kind, &execution_input)?
                != sqlite_access::verified_target(&tx, &request.kind, &request.input)?
        {
            return Err(access::denied());
        }
        if let Some(stored) = load_idempotent(
            &tx,
            "create_job",
            "workspace",
            &request.idempotency_key,
            &hash,
            "job",
        )? {
            let original: JobRecord = decode_json(&stored, "stored job result")?;
            return load_job(&tx, &original.job_id);
        }
        let record = JobRecord {
            job_id: format!("job_{}", Uuid::new_v4().simple()),
            state: JobState::Queued,
            attempt: 0,
            kind: request.kind.clone(),
            input: execution_input,
            result: None,
            error: None,
            lease_owner: None,
            lease_until: None,
            cancel_requested: false,
            progress: None,
        };
        tx.execute(
            "INSERT INTO jobs(job_id, state, attempt, kind, input_json, cancel_requested)
             VALUES (?1, 'queued', 0, ?2, ?3, 0)",
            params![
                record.job_id,
                record.kind,
                encode_json(&record.input, "job input")?
            ],
        )
        .map_err(storage_error)?;
        sqlite_access::bind_job(&tx, &record.job_id, &record.kind, &record.input)?;
        tx.execute(
            "INSERT INTO idempotency(operation_family, scope, idempotency_key, request_hash, result_kind, result_json)
             VALUES ('create_job', 'workspace', ?1, ?2, 'job', ?3)",
            params![
                request.idempotency_key,
                hash.current_digest(),
                encode_json(&record, "job result")?
            ],
        )
        .map_err(idempotency_insert_error)?;
        insert_job_event(&tx, &record, "queued", json!({}))?;
        sqlite_audit::job(&tx, &context, &record, "job.accepted", true)?;
        tx.commit().map_err(storage_error)?;
        Ok(record)
    }

    pub fn get_job(&self, job_id: &str) -> Result<JobRecord, EditorError> {
        sqlite_access::require_job(
            &self.connection,
            &self.connection.1,
            job_id,
            ProjectAction::Read,
        )?;
        load_job(&self.connection, job_id)
    }

    /// Native artifact adapter only. Raw execution data must not be serialized
    /// to a scoped client; the adapter verifies and serves the published bytes.
    pub fn artifact_job(&self, job_id: &str) -> Result<JobRecord, EditorError> {
        sqlite_access::require_job(
            &self.connection,
            &self.connection.1,
            job_id,
            ProjectAction::ReadArtifacts,
        )?;
        load_job(&self.connection, job_id)
    }

    pub fn list_jobs(&self) -> Result<Vec<JobRecord>, EditorError> {
        queries::JobRead::read_jobs(&self.connection)
    }

    /// Production lease operations read wall time after acquiring storage's atomic boundary.
    pub fn live_jobs(
        &mut self,
    ) -> lifecycle::JobService<
        '_,
        impl lifecycle::JobLifecycleStore + use<>,
        impl lifecycle::JobClock + use<>,
    > {
        lifecycle::JobService::new(&mut self.connection, sqlite_jobs::SystemClock)
    }

    pub fn cancel_job(&mut self, job_id: &str) -> Result<JobRecord, EditorError> {
        self.live_jobs().cancel(job_id)
    }

    /// Owner-only repair cancellation. The active repair gate excludes normal
    /// Worker connections; the attempt lock also excludes an orphaned FFmpeg
    /// child that outlived its supervisor. Keep the exclusive lock until the
    /// state and cancellation event commit atomically.
    pub fn repair_cancel_job(&mut self, job_id: &str) -> Result<JobRecord, EditorError> {
        if !self.repair_mode {
            return Err(EditorError::new(
                "CAPABILITY_UNAVAILABLE",
                "Immediate cancellation requires an active owner repair session",
            ));
        }
        sqlite_access::owner(&self.connection.1)?;
        let job = self.get_job(job_id)?;
        if job.state != JobState::Running {
            return self.cancel_job(job_id);
        }
        let attempt_directory = self
            .asset_store_root
            .parent()
            .ok_or_else(|| EditorError::new("STORAGE_ERROR", "Invalid workspace root"))?
            .join("artifacts")
            .join(&job.job_id)
            .join(format!("attempt-{}", job.attempt));
        let guard = opencut_storage_budget::AttemptLease::try_cleanup(&attempt_directory)
            .map_err(|_| EditorError::new("STORAGE_BUDGET_ERROR", "Could not inspect job attempt"))?
            .ok_or_else(|| {
                EditorError::new(
                    "STORAGE_BUDGET_BUSY",
                    "Job attempt still has a live media writer",
                )
            });
        // A not-yet-created attempt directory is possible after claim, but a
        // live managed Worker would still hold a database connection and block
        // entry into repair. Missing directories have no writer lock to hold.
        let guard = match std::fs::symlink_metadata(&attempt_directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            _ => Some(guard?),
        };
        if let Some(guard) = &guard
            && !guard.still_names(&attempt_directory).map_err(|_| {
                EditorError::new("STORAGE_BUDGET_ERROR", "Could not recheck job attempt")
            })?
        {
            return Err(EditorError::new(
                "STORAGE_BUDGET_BUSY",
                "Job attempt changed during repair cancellation",
            ));
        }
        self.live_jobs().repair_cancel(job_id)
    }

    /// Explicit-time compatibility APIs are retained for deterministic embedded
    /// callers and tests. Native workers must use live_jobs() instead.
    pub fn claim_job(
        &mut self,
        worker: &str,
        now_ms: i64,
        lease_ms: i64,
    ) -> Result<Option<JobRecord>, EditorError> {
        lifecycle::JobService::new(&mut self.connection, lifecycle::FixedClock(now_ms))
            .claim(worker, lease_ms)
    }
    pub fn heartbeat(
        &mut self,
        job_id: &str,
        worker: &str,
        attempt: u32,
        now_ms: i64,
        lease_ms: i64,
    ) -> Result<JobRecord, EditorError> {
        lifecycle::JobService::new(&mut self.connection, lifecycle::FixedClock(now_ms))
            .heartbeat(job_id, worker, attempt, lease_ms)
    }
    pub fn complete_job(
        &mut self,
        job_id: &str,
        worker: &str,
        attempt: u32,
        now_ms: i64,
        result: Value,
    ) -> Result<JobRecord, EditorError> {
        lifecycle::JobService::new(&mut self.connection, lifecycle::FixedClock(now_ms))
            .complete(job_id, worker, attempt, result)
    }
    pub fn fail_job(
        &mut self,
        job_id: &str,
        worker: &str,
        attempt: u32,
        now_ms: i64,
        error: Value,
    ) -> Result<JobRecord, EditorError> {
        lifecycle::JobService::new(&mut self.connection, lifecycle::FixedClock(now_ms))
            .fail(job_id, worker, attempt, error)
    }
    pub fn finish_cancelled(
        &mut self,
        job_id: &str,
        worker: &str,
        attempt: u32,
        now_ms: i64,
    ) -> Result<JobRecord, EditorError> {
        lifecycle::JobService::new(&mut self.connection, lifecycle::FixedClock(now_ms))
            .finish_cancelled(job_id, worker, attempt)
    }
    pub fn update_job_progress(
        &mut self,
        job_id: &str,
        worker: &str,
        attempt: u32,
        now_ms: i64,
        progress: JobProgress,
    ) -> Result<JobRecord, EditorError> {
        lifecycle::JobService::new(&mut self.connection, lifecycle::FixedClock(now_ms))
            .progress(job_id, worker, attempt, progress)
    }

    pub fn read_resource(
        &self,
        input: opencut_api_contracts::context::ResourceReadInput,
        capabilities: impl FnOnce() -> opencut_api_contracts::capabilities::HostCapabilities,
    ) -> Result<opencut_api_contracts::context::ResourceView, EditorError> {
        sqlite_access::principal(&self.connection, &self.connection.1)?;
        if let opencut_editor_application::resources::Target::Artifact(id) =
            opencut_editor_application::resources::target(&input.uri)?
        {
            let (job, _) = opencut_editor_application::job_views::parse_artifact_id(&id)?;
            sqlite_access::require_job(
                &self.connection,
                &self.connection.1,
                job,
                ProjectAction::ReadArtifacts,
            )?;
        }
        opencut_editor_application::resources::read(self, input, capabilities)
    }
    pub fn event_changes(
        &self,
        input: opencut_api_contracts::event_stream::EventChangesInput,
    ) -> Result<opencut_api_contracts::event_stream::EventChangesPage, EditorError> {
        opencut_editor_application::events::changes(&self.connection, input)
    }

    pub fn job_events_after(
        &self,
        after_sequence: i64,
        limit: usize,
        job_id: Option<&str>,
    ) -> Result<Vec<JobEvent>, EditorError> {
        queries::job_events(&self.connection, after_sequence, limit, job_id)
    }

    pub fn recover_expired(&mut self, now_ms: i64) -> Result<usize, EditorError> {
        lifecycle::JobService::new(&mut self.connection, lifecycle::FixedClock(now_ms)).recover()
    }

    /// Ordered, read-only outbox access for a future event publisher or SSE host.
    pub fn events_after(
        &self,
        after_sequence: i64,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, EditorError> {
        queries::project_events(&self.connection, after_sequence, limit)
    }
}

enum DatabaseState {
    Empty,
    UnversionedWithTables,
    Version(i64),
}

fn inspect_database(connection: &Connection) -> Result<DatabaseState, EditorError> {
    let has_metadata: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'metadata')",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if !has_metadata {
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        return Ok(if tables == 0 {
            DatabaseState::Empty
        } else {
            DatabaseState::UnversionedWithTables
        });
    }
    let version: Option<String> = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    let version = version.ok_or_else(|| {
        EditorError::new(
            "UNSUPPORTED_DATABASE_VERSION",
            "database has no OpenCut schema version value",
        )
    })?;
    let version = version.parse().map_err(|_| {
        EditorError::new(
            "UNSUPPORTED_DATABASE_VERSION",
            format!("database schema version {version} is invalid"),
        )
    })?;
    Ok(DatabaseState::Version(version))
}

fn unsupported_database_version(version: i64) -> EditorError {
    EditorError::new(
        "UNSUPPORTED_DATABASE_VERSION",
        format!("database schema version {version} is not supported"),
    )
}

fn ensure_wal_for_file_database(connection: &Connection) -> Result<(), EditorError> {
    // Concurrent DELETE->WAL converters may receive immediate SQLITE_BUSY even
    // with busy_timeout. Retry only this autocommit configuration step, after
    // releasing its statements; never replay a business transaction.
    let original_ms: u64 = connection
        .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
        .map_err(storage_error)?;
    let deadline = std::time::Instant::now() + Duration::from_millis(original_ms);
    let result = loop {
        let attempt = (|| -> rusqlite::Result<String> {
            connection
                .busy_timeout(deadline.saturating_duration_since(std::time::Instant::now()))?;
            let current: String = connection.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
            if current.eq_ignore_ascii_case("wal") || current.eq_ignore_ascii_case("memory") {
                return Ok(current);
            }
            connection
                .busy_timeout(deadline.saturating_duration_since(std::time::Instant::now()))?;
            connection.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        })();
        match attempt {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") || mode.eq_ignore_ascii_case("memory") => {
                break Ok(());
            }
            Ok(mode) => {
                break Err(EditorError::new(
                    "STORAGE_ERROR",
                    format!("could not enable WAL journal mode; SQLite selected {mode}"),
                ));
            }
            Err(rusqlite::Error::SqliteFailure(ref error, _))
                if matches!(
                    error.code,
                    ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
                ) && connection.is_autocommit()
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(std::time::Instant::now())),
                );
            }
            Err(error) => break Err(storage_error(error)),
        }
    };
    connection
        .busy_timeout(Duration::from_millis(original_ms))
        .map_err(storage_error)?;
    result
}

fn load_snapshot(
    connection: &Connection,
    project_id: &str,
) -> Result<ProjectSnapshot, EditorError> {
    sqlite_project_library::require_live(connection, project_id)?;
    let stored = connection
        .query_row(
            "SELECT r.document_json, p.head_revision
             FROM projects p JOIN revisions r ON r.revision = p.head_revision
             WHERE p.project_id = ?1",
            [project_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(storage_error)?
        .ok_or_else(|| {
            EditorError::new("NOT_FOUND", format!("project {project_id} was not found"))
        })?;
    Ok(ProjectSnapshot {
        project: sqlite_project_document::decode_legacy_project(&stored.0)?,
        revision: stored.1,
    })
}

fn load_job(connection: &Connection, job_id: &str) -> Result<JobRecord, EditorError> {
    let stored = connection
        .query_row(
            "SELECT state, attempt, kind, input_json, result_json, error_json,
                    lease_owner, lease_until, cancel_requested, progress_json
             FROM jobs WHERE job_id = ?1",
            [job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, bool>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()
        .map_err(storage_error)?
        .ok_or_else(|| EditorError::new("NOT_FOUND", format!("job {job_id} was not found")))?;
    Ok(JobRecord {
        job_id: job_id.into(),
        state: parse_job_state(&stored.0)?,
        attempt: stored.1,
        kind: stored.2,
        input: decode_json(&stored.3, "job input")?,
        result: stored
            .4
            .map(|value| decode_json(&value, "job result"))
            .transpose()?,
        error: stored
            .5
            .map(|value| decode_json(&value, "job error"))
            .transpose()?,
        lease_owner: stored.6,
        lease_until: stored.7,
        cancel_requested: stored.8,
        progress: stored
            .9
            .map(|value| decode_json(&value, "job progress"))
            .transpose()?,
    })
}

fn parse_job_state(value: &str) -> Result<JobState, EditorError> {
    match value {
        "queued" => Ok(JobState::Queued),
        "running" => Ok(JobState::Running),
        "succeeded" => Ok(JobState::Succeeded),
        "failed" => Ok(JobState::Failed),
        "cancelled" => Ok(JobState::Cancelled),
        _ => Err(EditorError::new(
            "STORAGE_CORRUPT",
            format!("unknown stored job state {value}"),
        )),
    }
}

fn job_state_name(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Succeeded => "succeeded",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

fn insert_job_event(
    connection: &Connection,
    job: &JobRecord,
    event_type: &str,
    payload: Value,
) -> Result<(), EditorError> {
    connection
        .execute(
            "INSERT INTO job_events(event_id,job_id,event_type,state,attempt,payload_json)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                format!("jevt_{}", Uuid::new_v4().simple()),
                job.job_id,
                event_type,
                job_state_name(job.state),
                job.attempt,
                encode_json(&payload, "job event payload")?
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn load_idempotent(
    connection: &Connection,
    operation_family: &str,
    scope: &str,
    key: &str,
    identity: &RequestIdentity,
    result_kind: &str,
) -> Result<Option<String>, EditorError> {
    // Legacy families keep their published independent key spaces. New V4/V5
    // project intents share a key scope across families; neither direction may
    // reuse one of their receipts as a different mutation.
    let v4_collision: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM idempotency
             WHERE scope=?1 AND idempotency_key=?2 AND operation_family<>?3
             AND (?4 OR operation_family GLOB 'project_v4_*' OR operation_family GLOB 'project_v5_*' OR operation_family IN ('project_branch_create','project_branch_merge')))",
            params![
                scope,
                key,
                operation_family,
                (operation_family.starts_with("project_v4_") || operation_family.starts_with("project_v5_") || matches!(operation_family,"project_branch_create"|"project_branch_merge"))
            ],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if v4_collision {
        return Err(EditorError::new(
            "IDEMPOTENCY_KEY_REUSED",
            "idempotency key was already used for a different project operation",
        ));
    }
    let stored: Option<(String, String, String)> = connection
        .query_row(
            "SELECT request_hash, result_kind, result_json FROM idempotency
             WHERE operation_family = ?1 AND scope = ?2 AND idempotency_key = ?3",
            params![operation_family, scope, key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(storage_error)?;
    if let Some((stored_hash, stored_kind, result)) = &stored
        && !stored_hash.starts_with("v2:")
        && stored_kind == "transaction"
        && result_kind == "transaction"
    {
        let transaction_id =
            decode_json::<TransactionResult>(result, "stored transaction result")?.transaction_id;
        let stored_request: Option<String> = connection
            .query_row(
                "SELECT request_json FROM editor_transactions WHERE transaction_id = ?1",
                [&transaction_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        if let Some(stored_request) = stored_request {
            // Transactions may persist prepared asset operations, not the original
            // public request. Only recover a semantic identity when re-serialization
            // also proves it matches the recorded legacy digest.
            let recovered = match operation_family {
                "apply" => request_hash(
                    operation_family,
                    scope,
                    &decode_json::<TransactionRequest>(
                        &stored_request,
                        "stored transaction request",
                    )?,
                )?,
                "restore_revision" => request_hash(
                    operation_family,
                    scope,
                    &decode_json::<RestoreRevisionRequest>(
                        &stored_request,
                        "stored restore request",
                    )?,
                )?,
                _ => {
                    return Err(EditorError::new(
                        "STORAGE_ERROR",
                        "Unknown transaction family",
                    ));
                }
            };
            if identity.matches(stored_hash, Some(&recovered)) {
                return Ok(Some(result.clone()));
            }
        }
    }
    match stored {
        None => Ok(None),
        Some((stored_hash, stored_kind, result))
            if identity.matches(&stored_hash, None) && stored_kind == result_kind =>
        {
            Ok(Some(result))
        }
        Some(_) => Err(EditorError::new(
            "IDEMPOTENCY_KEY_REUSED",
            "idempotency key was already used for a different request",
        )),
    }
}

fn encode_json<T: Serialize>(value: &T, description: &str) -> Result<String, EditorError> {
    serde_json::to_string(value).map_err(|error| {
        EditorError::new(
            "SERIALIZATION_ERROR",
            format!("could not encode {description}: {error}"),
        )
    })
}

fn decode_json<T: DeserializeOwned>(value: &str, description: &str) -> Result<T, EditorError> {
    serde_json::from_str(value).map_err(|error| {
        EditorError::new(
            "STORAGE_CORRUPT",
            format!("could not decode {description}: {error}"),
        )
    })
}

fn require_idempotency_key(key: &str) -> Result<(), EditorError> {
    if key.trim().is_empty() {
        Err(EditorError::new(
            "INVALID_ARGUMENT",
            "idempotency key must not be empty",
        ))
    } else {
        Ok(())
    }
}

fn validate_job_request(request: &JobRequest) -> Result<(), EditorError> {
    require_idempotency_key(&request.idempotency_key)?;
    if !SUPPORTED_JOB_KINDS.contains(&request.kind.as_str()) {
        return Err(EditorError::new(
            "UNSUPPORTED_JOB_KIND",
            format!("job kind {} is not supported", request.kind),
        ));
    }
    Ok(())
}

fn revision_conflict(expected: &str, actual: &str) -> EditorError {
    EditorError {
        code: "REVISION_CONFLICT".into(),
        message: "project has changed; refresh the snapshot and re-plan".into(),
        details: json!({
            "expected_revision": expected,
            "actual_revision": actual,
            "recovery_action": "refresh_and_replan"
        }),
    }
}

fn idempotency_insert_error(error: rusqlite::Error) -> EditorError {
    if matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::ConstraintViolation)
    ) {
        EditorError::new(
            "IDEMPOTENCY_KEY_REUSED",
            "idempotency key was concurrently used by another request",
        )
    } else {
        storage_error(error)
    }
}

fn constraint_or_storage(error: rusqlite::Error) -> EditorError {
    if is_artifact_publication_fence(&error) {
        return storage_error(error);
    }
    if matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::ConstraintViolation)
    ) {
        EditorError::new("ALREADY_EXISTS", "project already exists")
    } else {
        storage_error(error)
    }
}

fn is_artifact_publication_fence(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(_, Some(message)) if message == "artifact collection publication fenced")
}

fn storage_error(error: rusqlite::Error) -> EditorError {
    if std::env::var_os("OPENCUT_TRACE_SQLITE").is_some() {
        eprintln!("OpenCut SQLite failure: {error:?}");
    }
    if is_artifact_publication_fence(&error) {
        return EditorError::new(
            "ARTIFACT_COLLECTION_RECOVERY_REQUIRED",
            "An unfinished artifact removal is fencing new references; resolve its durable result before retrying the original request",
        );
    }
    if error.sqlite_error_code() == Some(ErrorCode::DiskFull) {
        return EditorError {
            code: "DATABASE_CAPACITY_EXHAUSTED".into(),
            message: "SQLite could not allocate database or journal space. Restore capacity, inspect durable state, then explicitly retry the original request with its original idempotency key; do not assume cancellation or completion was recorded.".into(),
            details: json!({"recovery_action":"restore_capacity_inspect_and_replay_original_request"}),
        };
    }
    if matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    ) {
        return EditorError::new(
            "DATABASE_BUSY",
            "SQLite is temporarily locked; inspect the current result and retry with the original request identity",
        );
    }
    EditorError::new("STORAGE_ERROR", format!("SQLite operation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencut_contracts::{FrameRate, Operation, Track, TrackKind};
    use std::{
        collections::BTreeMap,
        path::PathBuf,
        sync::{Arc, Barrier},
        thread,
    };

    fn database_path() -> PathBuf {
        std::env::temp_dir().join(format!("opencut-application-{}.sqlite", Uuid::new_v4()))
    }

    fn create_input(id: &str) -> CreateProject {
        CreateProject {
            id: id.into(),
            name: "Test project".into(),
            sequence_id: "sequence-1".into(),
            width: 1920,
            height: 1080,
            fps: FrameRate {
                numerator: 30,
                denominator: 1,
            },
        }
    }

    fn add_track_request(revision: &str, key: &str, track_id: &str) -> TransactionRequest {
        TransactionRequest {
            expected_revision: revision.into(),
            idempotency_key: key.into(),
            label: format!("add {track_id}"),
            operations: vec![Operation::AddTrack {
                sequence_id: "sequence-1".into(),
                track: Track {
                    id: track_id.into(),
                    name: "Video".into(),
                    kind: TrackKind::Video,
                    enabled: true,
                    muted: false,
                    locked: false,
                    clips: vec![],
                    params: BTreeMap::new(),
                },
            }],
        }
    }

    #[test]
    fn duplicate_apply_returns_the_original_result_before_version_check() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let created = app
            .create_project(create_input("project-1"), "create-1")
            .unwrap();
        let request = add_track_request(&created.revision, "edit-1", "track-1");
        let first = app.apply("project-1", &request).unwrap();

        let second = app.apply("project-1", &request).unwrap();

        assert_eq!(first, second);
        assert_eq!(app.events_after(0, 10).unwrap().len(), 2);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn equivalent_rational_times_share_identity_across_and_legacy_records() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let created = app
            .create_project(create_input("rational-idempotency"), "create")
            .unwrap();
        let request = |ticks: &str, timebase: u32| {
            serde_json::from_value::<TransactionRequest>(json!({
                "expected_revision": created.revision,
                "idempotency_key": "equivalent-time",
                "label": "caption",
                "operations": [{
                    "type": "caption.insert",
                    "sequence_id": "sequence-1",
                    "cue": {
                        "id": "cue-1",
                        "timeline_range": {
                            "start": {"ticks": ticks, "timebase": timebase},
                            "duration": {"ticks": ticks, "timebase": timebase}
                        },
                        "text": "caption",
                        "style": {
                            "font_family": "Arial",
                            "font_size": 16.0,
                            "color": "#ffffff",
                            "background_color": null,
                            "font_policy": {"mode": "strict"},
                            "text_align": "center",
                            "max_width": null,
                            "line_height": null
                        }
                    }
                }]
            }))
            .unwrap()
        };
        let half = request("1", 2);
        let first = app.apply("rational-idempotency", &half).unwrap();
        assert_eq!(
            app.apply("rational-idempotency", &request("2", 4)).unwrap(),
            first
        );

        let legacy = request_hash("apply", "rational-idempotency", &half)
            .unwrap()
            .legacy_digest()
            .to_owned();
        app.connection
            .execute(
                "UPDATE idempotency SET request_hash=?1 WHERE operation_family='apply' AND scope='rational-idempotency' AND idempotency_key='equivalent-time'",
                [legacy],
            )
            .unwrap();
        assert_eq!(
            app.apply("rational-idempotency", &request("2", 4)).unwrap(),
            first
        );
        assert_eq!(
            app.apply("rational-idempotency", &request("3", 4))
                .unwrap_err()
                .code,
            "IDEMPOTENCY_KEY_REUSED"
        );
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn asset_bind_and_replace_pin_bytes_while_plan_remains_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workspace.sqlite");
        let source = directory.path().join("source.media");
        std::fs::write(&source, b"first bytes").unwrap();
        let mut app = EditorApplication::open(&path).unwrap();
        let created = app
            .create_project(create_input("project-1"), "create-1")
            .unwrap();
        let bind: TransactionRequest = serde_json::from_value(json!({
            "expected_revision":created.revision,
            "idempotency_key":"bind-1",
            "label":"bind versioned asset",
            "operations":[{"type":"asset.bind","asset":{
                "id":"asset-1","name":"Media","kind":"video","uri":source,
                "duration":{"ticks":"1","timebase":1},"metadata":{}
            }}]
        }))
        .unwrap();

        let plan = app.plan("project-1", &bind).unwrap();
        assert_eq!(plan.unresolved_asset_ids, vec!["asset-1"]);
        assert!(plan.proposed_project.assets[0].version.is_none());
        assert!(!path.parent().unwrap().join("asset-store").exists());

        let bound = app.apply("project-1", &bind).unwrap();
        let bound_snapshot = app.snapshot("project-1").unwrap();
        let bound_asset = &bound_snapshot.project.assets[0];
        let first_version = bound_asset.version.clone().unwrap();
        assert_eq!(bound_asset.uri, source.to_string_lossy());
        assert_eq!(
            first_version.id,
            format!("av_{}", first_version.content_hash)
        );
        assert_eq!(first_version.size, 11);

        std::fs::write(&source, b"second bytes").unwrap();
        let replace: TransactionRequest = serde_json::from_value(json!({
            "expected_revision":bound.revision,
            "idempotency_key":"replace-1",
            "label":"replace versioned asset",
            "operations":[{"type":"asset.replace","asset":{
                "id":"asset-1","name":"Media v2","kind":"video","uri":source,
                "duration":{"ticks":"1","timebase":1},"metadata":{}
            }}]
        }))
        .unwrap();
        let replaced = app.apply("project-1", &replace).unwrap();
        let replaced_snapshot = app.snapshot("project-1").unwrap();
        let second_version = replaced_snapshot.project.assets[0].version.clone().unwrap();
        assert_ne!(second_version.content_hash, first_version.content_hash);
        assert_eq!(
            app.snapshot_at("project-1", &bound.revision)
                .unwrap()
                .project
                .assets[0]
                .version,
            Some(first_version.clone())
        );

        std::fs::remove_file(&source).unwrap();
        assert_eq!(app.apply("project-1", &bind).unwrap(), bound);
        app.restore_revision(
            "project-1",
            &RestoreRevisionRequest {
                expected_revision: replaced.revision,
                target_revision: bound.revision,
                idempotency_key: "restore-version".into(),
                label: "restore old media version".into(),
            },
        )
        .unwrap();
        assert_eq!(
            app.snapshot("project-1").unwrap().project.assets[0].version,
            Some(first_version)
        );
        drop(app);
    }

    #[test]
    fn restore_creates_new_history_and_retry_survives_later_edits() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let original = app
            .create_project(create_input("restore-project"), "create")
            .unwrap();
        let first = app
            .apply(
                "restore-project",
                &add_track_request(&original.revision, "edit", "track-1"),
            )
            .unwrap();
        let restore = RestoreRevisionRequest {
            expected_revision: first.revision.clone(),
            target_revision: original.revision.clone(),
            idempotency_key: "restore".into(),
            label: "Restore initial project".into(),
        };
        let restored = app.restore_revision("restore-project", &restore).unwrap();
        assert_ne!(restored.revision, original.revision);
        assert_eq!(restored.base_revision, first.revision);
        assert_eq!(restored.deleted_ids, vec!["track-1"]);
        assert_eq!(
            app.snapshot("restore-project").unwrap().project,
            original.project
        );
        assert_eq!(
            app.snapshot_at("restore-project", &first.revision)
                .unwrap()
                .project
                .sequences[0]
                .tracks
                .len(),
            1
        );
        let page = app.history("restore-project", None, 2).unwrap();
        assert_eq!(
            page.revisions[0].label.as_deref(),
            Some("Restore initial project")
        );
        assert_eq!(
            page.next_cursor.as_deref(),
            Some(original.revision.as_str())
        );
        let later = app
            .apply(
                "restore-project",
                &add_track_request(&restored.revision, "later", "track-2"),
            )
            .unwrap();
        assert_eq!(
            app.restore_revision("restore-project", &restore).unwrap(),
            restored
        );
        assert_eq!(
            app.snapshot("restore-project").unwrap().revision,
            later.revision
        );
        let next = app
            .history("restore-project", page.next_cursor.as_deref(), 2)
            .unwrap();
        assert_eq!(next.revisions.len(), 1);
        assert_eq!(next.revisions[0].revision, original.revision);
        assert!(next.next_cursor.is_none());
        assert_eq!(app.events_after(0, 100).unwrap().len(), 4);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn restore_checks_revision_project_scope_and_key_semantics_without_side_effects() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let original = app
            .create_project(create_input("restore-project"), "create")
            .unwrap();
        let other = app
            .create_project(create_input("other-project"), "other")
            .unwrap();
        let request = RestoreRevisionRequest {
            expected_revision: original.revision.clone(),
            target_revision: other.revision.clone(),
            idempotency_key: "restore".into(),
            label: "Restore".into(),
        };
        assert_eq!(
            app.restore_revision("restore-project", &request)
                .unwrap_err()
                .code,
            "NOT_FOUND"
        );
        assert_eq!(
            app.history("restore-project", Some(&other.revision), 10)
                .unwrap_err()
                .code,
            "NOT_FOUND"
        );
        let request = RestoreRevisionRequest {
            target_revision: original.revision.clone(),
            ..request
        };
        let restored = app.restore_revision("restore-project", &request).unwrap();
        let reused = RestoreRevisionRequest {
            label: "Different semantic request".into(),
            ..request.clone()
        };
        assert_eq!(
            app.restore_revision("restore-project", &reused)
                .unwrap_err()
                .code,
            "IDEMPOTENCY_KEY_REUSED"
        );
        let stale = RestoreRevisionRequest {
            idempotency_key: "stale".into(),
            ..request
        };
        assert_eq!(
            app.restore_revision("restore-project", &stale)
                .unwrap_err()
                .code,
            "REVISION_CONFLICT"
        );
        assert_eq!(
            app.snapshot("restore-project").unwrap().revision,
            restored.revision
        );
        assert_eq!(app.events_after(0, 100).unwrap().len(), 3);
        drop(app);
        let reopened = EditorApplication::open(&path).unwrap();
        assert_eq!(
            reopened
                .history("restore-project", None, 100)
                .unwrap()
                .revisions
                .len(),
            2
        );
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn duplicate_create_returns_the_persisted_snapshot() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let input = create_input("project-1");

        let first = app.create_project(input.clone(), "create-1").unwrap();
        let second = app.create_project(input, "create-1").unwrap();

        assert_eq!(first, second);
        assert_eq!(app.list_projects().unwrap().len(), 1);
        assert_eq!(app.events_after(0, 10).unwrap().len(), 1);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn same_idempotency_key_with_a_different_request_conflicts() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let created = app
            .create_project(create_input("project-1"), "create-1")
            .unwrap();
        app.apply(
            "project-1",
            &add_track_request(&created.revision, "edit-1", "track-1"),
        )
        .unwrap();

        let error = app
            .apply(
                "project-1",
                &add_track_request(&created.revision, "edit-1", "track-2"),
            )
            .unwrap_err();

        assert_eq!(error.code, "IDEMPOTENCY_KEY_REUSED");
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stale_revision_is_rejected_without_writing() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let created = app
            .create_project(create_input("project-1"), "create-1")
            .unwrap();
        let first = app
            .apply(
                "project-1",
                &add_track_request(&created.revision, "edit-1", "track-1"),
            )
            .unwrap();

        let error = app
            .apply(
                "project-1",
                &add_track_request(&created.revision, "edit-2", "track-2"),
            )
            .unwrap_err();

        assert_eq!(error.code, "REVISION_CONFLICT");
        assert_eq!(app.snapshot("project-1").unwrap().revision, first.revision);
        assert_eq!(app.events_after(0, 10).unwrap().len(), 2);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn outbox_failure_rolls_back_revision_transaction_and_idempotency() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let created = app
            .create_project(create_input("project-1"), "create-1")
            .unwrap();
        app.connection
            .execute_batch(
                "CREATE TRIGGER reject_transaction_event BEFORE INSERT ON outbox
             WHEN NEW.event_type = 'transaction.committed'
             BEGIN SELECT RAISE(ABORT, 'injected outbox failure'); END;",
            )
            .unwrap();
        let request = add_track_request(&created.revision, "edit-1", "track-1");

        assert_eq!(
            app.apply("project-1", &request).unwrap_err().code,
            "STORAGE_ERROR"
        );
        assert_eq!(app.snapshot("project-1").unwrap(), created);
        assert_eq!(
            app.connection
                .query_row("SELECT COUNT(*) FROM revisions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            app.connection
                .query_row("SELECT COUNT(*) FROM editor_transactions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            app.connection
                .query_row("SELECT COUNT(*) FROM idempotency", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn concurrent_connections_allow_only_one_writer_for_a_base_revision() {
        let path = database_path();
        let mut setup = EditorApplication::open(&path).unwrap();
        let created = setup
            .create_project(create_input("project-1"), "create-1")
            .unwrap();
        drop(setup);
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = [("edit-1", "track-1"), ("edit-2", "track-2")]
            .into_iter()
            .map(|(key, track)| {
                let path = path.clone();
                let revision = created.revision.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut app = EditorApplication::open(path).unwrap();
                    let request = add_track_request(&revision, key, track);
                    barrier.wait();
                    app.apply("project-1", &request)
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(error) if error.code == "REVISION_CONFLICT"))
                .count(),
            1
        );
        let app = EditorApplication::open(&path).unwrap();
        assert_eq!(
            app.snapshot("project-1").unwrap().project.sequences[0]
                .tracks
                .len(),
            1
        );
        assert_eq!(app.events_after(0, 10).unwrap().len(), 2);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn committed_state_and_idempotency_survive_reopen() {
        let path = database_path();
        let (request, result) = {
            let mut app = EditorApplication::open(&path).unwrap();
            let created = app
                .create_project(create_input("project-1"), "create-1")
                .unwrap();
            let request = add_track_request(&created.revision, "edit-1", "track-1");
            let result = app.apply("project-1", &request).unwrap();
            (request, result)
        };

        let mut reopened = EditorApplication::open(&path).unwrap();
        assert_eq!(
            reopened.snapshot("project-1").unwrap().revision,
            result.revision
        );
        assert_eq!(reopened.apply("project-1", &request).unwrap(), result);
        assert_eq!(reopened.list_projects().unwrap().len(), 1);
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_creates_parent_directories_and_uses_full_synchronous_mode() {
        let root = std::env::temp_dir().join(format!("opencut-parent-{}", Uuid::new_v4()));
        let path = root.join(".opencut/workspace.sqlite");

        let app = EditorApplication::open(&path).unwrap();

        assert!(path.exists());
        let synchronous: i64 = app
            .connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 2);
        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn apply_idempotency_keys_are_scoped_to_project() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let first = app
            .create_project(create_input("project-1"), "create-1")
            .unwrap();
        let second = app
            .create_project(create_input("project-2"), "create-2")
            .unwrap();

        app.apply(
            "project-1",
            &add_track_request(&first.revision, "shared-edit-key", "track-1"),
        )
        .unwrap();
        app.apply(
            "project-2",
            &add_track_request(&second.revision, "shared-edit-key", "track-2"),
        )
        .unwrap();

        assert_eq!(
            app.snapshot("project-1").unwrap().project.sequences[0]
                .tracks
                .len(),
            1
        );
        assert_eq!(
            app.snapshot("project-2").unwrap().project.sequences[0]
                .tracks
                .len(),
            1
        );
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unknown_database_is_rejected_without_migration() {
        let path = database_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("CREATE TABLE foreign_data(value TEXT)", [])
            .unwrap();
        drop(connection);

        let error = EditorApplication::open(&path).err().unwrap();

        assert_eq!(error.code, "UNSUPPORTED_DATABASE_VERSION");
        let connection = Connection::open(&path).unwrap();
        let metadata_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'metadata')",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(!metadata_exists);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn metadata_without_schema_version_is_rejected_without_changes() {
        let path = database_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE sentinel (value TEXT NOT NULL);
                 INSERT INTO sentinel(value) VALUES ('preserve-me');",
            )
            .unwrap();
        drop(connection);

        let error = EditorApplication::open(&path).err().unwrap();

        assert_eq!(error.code, "UNSUPPORTED_DATABASE_VERSION");
        let connection = Connection::open(&path).unwrap();
        let sentinel: String = connection
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        let application_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('projects', 'jobs', 'job_events')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sentinel, "preserve-me");
        assert_eq!(application_tables, 0);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn concurrent_first_open_initializes_schema_once() {
        let path = database_path();
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    EditorApplication::open(path)
                })
            })
            .collect();
        barrier.wait();

        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        let app = EditorApplication::open(&path).unwrap();
        assert!(app.list_projects().unwrap().is_empty());
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn current_schema_in_delete_mode_is_switched_to_wal() {
        let path = database_path();
        drop(EditorApplication::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        let mode: String = connection
            .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "delete");
        drop(connection);

        drop(EditorApplication::open(&path).unwrap());

        let connection = Connection::open(&path).unwrap();
        let mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    fn job_request(key: &str) -> JobRequest {
        JobRequest {
            idempotency_key: key.into(),
            kind: "media.probe".into(),
            input: json!({"uri": "file:///tmp/input.mp4"}),
        }
    }

    #[test]
    fn job_creation_is_idempotent_and_reopens() {
        let path = database_path();
        let job = {
            let mut app = EditorApplication::open(&path).unwrap();
            let first = app.create_job(job_request("job-key")).unwrap();
            let second = app.create_job(job_request("job-key")).unwrap();
            assert_eq!(first, second);
            first
        };

        let app = EditorApplication::open(&path).unwrap();
        assert_eq!(app.get_job(&job.job_id).unwrap(), job);
        assert_eq!(app.list_jobs().unwrap(), vec![job]);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn composited_frame_is_a_supported_durable_job_kind() {
        let mut app = EditorApplication::open(":memory:").unwrap();
        let job = app
            .create_job(JobRequest {
                idempotency_key: "composited-frame".into(),
                kind: "preview.composited_frame".into(),
                input: json!({"fixture": true}),
            })
            .unwrap();
        assert_eq!(job.kind, "preview.composited_frame");
    }

    #[test]
    fn project_range_is_a_supported_durable_job_kind() {
        let mut app = EditorApplication::open(":memory:").unwrap();
        let job = app
            .create_job(JobRequest {
                idempotency_key: "project-range".into(),
                kind: "preview.project_range".into(),
                input: json!({"fixture": true}),
            })
            .unwrap();
        assert_eq!(job.kind, "preview.project_range");
    }

    #[test]
    fn prepared_job_hashes_public_request_and_persists_execution_input() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let request = job_request("prepared-key");
        assert!(app.find_existing_job(&request).unwrap().is_none());

        let first = app
            .create_prepared_job(
                request.clone(),
                json!({"snapshot":"locked","asset_lock":{"version":1}}),
            )
            .unwrap();
        assert_eq!(first.input["snapshot"], "locked");
        assert_eq!(
            app.find_existing_job(&request).unwrap().unwrap().job_id,
            first.job_id
        );

        let retry = app
            .create_prepared_job(request, json!({"must_not_replace":"original"}))
            .unwrap();
        assert_eq!(retry, first);
        assert_eq!(app.job_events_after(0, 10, None).unwrap().len(), 1);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn prepared_job_rejects_reused_key_from_public_request_before_preparation() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let original = job_request("prepared-key");
        app.create_prepared_job(original, json!({"locked":true}))
            .unwrap();
        let changed = JobRequest {
            idempotency_key: "prepared-key".into(),
            kind: "media.probe".into(),
            input: json!({"uri":"file:///different.mp4"}),
        };

        let error = app.find_existing_job(&changed).unwrap_err();

        assert_eq!(error.code, "IDEMPOTENCY_KEY_REUSED");
        assert_eq!(app.list_jobs().unwrap().len(), 1);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn expired_lease_is_recovered_and_old_attempt_is_fenced() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let job = app.create_job(job_request("job-key")).unwrap();
        let first = app.claim_job("worker-a", 100, 10).unwrap().unwrap();
        assert_eq!(first.attempt, 1);

        assert_eq!(app.recover_expired(111).unwrap(), 1);
        let second = app.claim_job("worker-b", 112, 10).unwrap().unwrap();
        assert_eq!(second.job_id, job.job_id);
        assert_eq!(second.attempt, 2);
        assert_eq!(
            app.complete_job(&job.job_id, "worker-a", 1, 112, json!({"bad": true}))
                .unwrap_err()
                .code,
            "JOB_LEASE_LOST"
        );
        let completed = app
            .complete_job(&job.job_id, "worker-b", 2, 115, json!({"ok": true}))
            .unwrap();
        assert_eq!(completed.state, JobState::Succeeded);
        assert_eq!(completed.result, Some(json!({"ok": true})));
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cancellation_has_stable_terminal_semantics() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let queued = app.create_job(job_request("queued-key")).unwrap();
        assert_eq!(
            app.cancel_job(&queued.job_id).unwrap().state,
            JobState::Cancelled
        );
        assert!(app.claim_job("worker", 0, 10).unwrap().is_none());

        let running = app.create_job(job_request("running-key")).unwrap();
        let claim = app.claim_job("worker", 10, 10).unwrap().unwrap();
        assert_eq!(claim.job_id, running.job_id);
        let requested = app.cancel_job(&running.job_id).unwrap();
        assert_eq!(requested.state, JobState::Running);
        assert!(requested.cancel_requested);
        let cancelled = app
            .finish_cancelled(&running.job_id, "worker", claim.attempt, 11)
            .unwrap();
        assert_eq!(cancelled.state, JobState::Cancelled);
        assert_eq!(
            app.cancel_job(&running.job_id).unwrap().state,
            JobState::Cancelled
        );
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn completion_wins_before_a_late_cancel() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let job = app.create_job(job_request("job-key")).unwrap();
        let claim = app.claim_job("worker", 10, 10).unwrap().unwrap();
        let completed = app
            .complete_job(
                &job.job_id,
                "worker",
                claim.attempt,
                11,
                json!({"ok": true}),
            )
            .unwrap();

        let after_cancel = app.cancel_job(&job.job_id).unwrap();

        assert_eq!(after_cancel, completed);
        assert_eq!(after_cancel.state, JobState::Succeeded);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn version_one_database_is_migrated_without_losing_projects() {
        let path = database_path();
        let original = {
            let mut app = EditorApplication::open(&path).unwrap();
            app.create_project(create_input("project-1"), "create-1")
                .unwrap()
        };
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(sqlite_artifact_publication::DROP_FOR_TEST)
            .unwrap();
        connection
            .execute_batch(
                "DROP TABLE audio_preview_recovery_v1; DROP TABLE font_derivations;
DROP TABLE edit_recovery; DROP TABLE deleted_projects; DROP VIEW available_project_media; DROP TABLE project_media_bindings; DROP TABLE project_preset_grants; DROP TABLE project_edit_control; DROP TABLE project_merge_parents; DROP TABLE project_branches; DROP TABLE artifact_gc_plans; DROP TABLE artifact_retention; DROP TABLE cas_gc_events; DROP TABLE cas_gc_plans; DROP TABLE denial_records; DROP TABLE project_plugin_grants; DROP TABLE upload_retention; DROP TABLE project_media; DROP TABLE upload_chunks; DROP TABLE upload_sessions; DROP TABLE plugin_events; DROP TABLE plugin_publications; DROP TABLE maintenance_events; DROP TABLE maintenance_publications; DROP TABLE job_projects;
             DROP TABLE access_events;
             DROP TABLE access_credentials;
             DROP TABLE access_grants;
             DROP TABLE access_principals;
             DROP TABLE audit_records;
             DROP TABLE jobs;
             UPDATE metadata SET value = '1' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(connection);

        let mut migrated = EditorApplication::open(&path).unwrap();

        assert_eq!(migrated.snapshot("project-1").unwrap(), original);
        assert_eq!(
            migrated.create_job(job_request("job-key")).unwrap().state,
            JobState::Queued
        );
        drop(migrated);
        let _ = std::fs::remove_file(path);
    }

    fn progress(stage: &str, fraction: f64) -> JobProgress {
        JobProgress {
            stage: stage.into(),
            fraction: Some(fraction),
            media_time: Some(opencut_contracts::Time {
                ticks: "30".into(),
                timebase: 30,
            }),
        }
    }

    #[test]
    fn job_progress_and_lifecycle_events_persist_across_reopen() {
        let path = database_path();
        let job_id = {
            let mut app = EditorApplication::open(&path).unwrap();
            let job = app.create_job(job_request("job-key")).unwrap();
            assert_eq!(
                app.create_job(job_request("job-key")).unwrap().job_id,
                job.job_id
            );
            let running = app.claim_job("worker", 100, 50).unwrap().unwrap();
            let updated = app
                .update_job_progress(
                    &job.job_id,
                    "worker",
                    running.attempt,
                    110,
                    progress("decode", 0.5),
                )
                .unwrap();
            assert_eq!(updated.progress, Some(progress("decode", 0.5)));
            app.complete_job(
                &job.job_id,
                "worker",
                running.attempt,
                120,
                json!({"ok":true}),
            )
            .unwrap();
            job.job_id
        };

        let reopened = EditorApplication::open(&path).unwrap();
        assert_eq!(
            reopened.get_job(&job_id).unwrap().state,
            JobState::Succeeded
        );
        let events = reopened.job_events_after(0, 20, Some(&job_id)).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec!["queued", "running", "progress", "succeeded"]
        );
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stale_attempt_cannot_publish_progress_or_an_event() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let job = app.create_job(job_request("job-key")).unwrap();
        let first = app.claim_job("worker-a", 100, 10).unwrap().unwrap();
        app.recover_expired(111).unwrap();
        let second = app.claim_job("worker-b", 112, 20).unwrap().unwrap();
        let before = app
            .job_events_after(0, 20, Some(&job.job_id))
            .unwrap()
            .len();

        let error = app
            .update_job_progress(
                &job.job_id,
                "worker-a",
                first.attempt,
                113,
                progress("stale", 0.9),
            )
            .unwrap_err();

        assert_eq!(error.code, "JOB_LEASE_LOST");
        assert_eq!(
            app.job_events_after(0, 20, Some(&job.job_id))
                .unwrap()
                .len(),
            before
        );
        assert_eq!(app.get_job(&job.job_id).unwrap().progress, None);
        app.update_job_progress(
            &job.job_id,
            "worker-b",
            second.attempt,
            114,
            progress("active", 0.1),
        )
        .unwrap();
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cancellation_events_are_stable_under_repeated_requests() {
        let path = database_path();
        let mut app = EditorApplication::open(&path).unwrap();
        let job = app.create_job(job_request("job-key")).unwrap();
        let running = app.claim_job("worker", 100, 20).unwrap().unwrap();
        app.cancel_job(&job.job_id).unwrap();
        app.cancel_job(&job.job_id).unwrap();
        app.finish_cancelled(&job.job_id, "worker", running.attempt, 110)
            .unwrap();
        app.cancel_job(&job.job_id).unwrap();

        let events = app.job_events_after(0, 20, Some(&job.job_id)).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec!["queued", "running", "cancel_requested", "cancelled"]
        );
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn version_two_jobs_migrate_with_data_intact() {
        let path = database_path();
        let original = {
            let mut app = EditorApplication::open(&path).unwrap();
            app.create_job(job_request("job-key")).unwrap()
        };
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(sqlite_artifact_publication::DROP_FOR_TEST)
            .unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
             DROP TABLE audio_preview_recovery_v1; DROP TABLE font_derivations;
DROP TABLE edit_recovery; DROP TABLE deleted_projects; DROP VIEW available_project_media; DROP TABLE project_media_bindings; DROP TABLE project_preset_grants; DROP TABLE project_edit_control;
             DROP TABLE project_merge_parents;
             DROP TABLE project_branches;
             DROP TABLE artifact_gc_plans; DROP TABLE artifact_retention; DROP TABLE cas_gc_events; DROP TABLE cas_gc_plans; DROP TABLE denial_records; DROP TABLE project_plugin_grants; DROP TABLE upload_retention; DROP TABLE project_media; DROP TABLE upload_chunks; DROP TABLE upload_sessions; DROP TABLE plugin_events; DROP TABLE plugin_publications; DROP TABLE maintenance_events;
             DROP TABLE maintenance_publications;
             DROP TABLE job_projects;
             DROP TABLE access_events;
             DROP TABLE access_credentials;
             DROP TABLE access_grants;
             DROP TABLE access_principals;
             DROP TABLE audit_records;
             DROP TABLE job_events;
             ALTER TABLE jobs RENAME TO jobs_v3;
             CREATE TABLE jobs (
               job_id TEXT PRIMARY KEY, state TEXT NOT NULL, attempt INTEGER NOT NULL,
               kind TEXT NOT NULL, input_json TEXT NOT NULL, result_json TEXT, error_json TEXT,
               lease_owner TEXT, lease_until INTEGER, cancel_requested INTEGER NOT NULL DEFAULT 0,
               created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );
             INSERT INTO jobs(job_id,state,attempt,kind,input_json,result_json,error_json,
                              lease_owner,lease_until,cancel_requested,created_at)
               SELECT job_id,state,attempt,kind,input_json,result_json,error_json,
                      lease_owner,lease_until,cancel_requested,created_at FROM jobs_v3;
             DROP TABLE jobs_v3;
             UPDATE metadata SET value = '2' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(connection);

        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let job_id = original.job_id.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let app = EditorApplication::open(path)?;
                    app.get_job(&job_id)
                })
            })
            .collect();
        barrier.wait();
        for handle in handles {
            assert_eq!(handle.join().unwrap().unwrap(), original);
        }
        let migrated = EditorApplication::open(&path).unwrap();
        assert!(
            migrated
                .job_events_after(0, 10, Some(&original.job_id))
                .unwrap()
                .is_empty()
        );
        drop(migrated);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn opening_current_database_reads_while_another_connection_holds_write_lock() {
        let path = database_path();
        let job = {
            let mut app = EditorApplication::open(&path).unwrap();
            app.create_job(job_request("job-key")).unwrap()
        };
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE;").unwrap();
        assert_eq!(
            writer
                .execute(
                    "UPDATE jobs SET kind = 'uncommitted' WHERE job_id = ?1",
                    [&job.job_id],
                )
                .unwrap(),
            1
        );
        let (sender, receiver) = std::sync::mpsc::channel();
        let read_path = path.clone();
        let job_id = job.job_id.clone();
        let reader = thread::spawn(move || {
            let result = EditorApplication::open(read_path).and_then(|app| app.get_job(&job_id));
            sender.send(result).unwrap();
        });

        let read = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("v3 open/read must not wait for an unrelated writer")
            .unwrap();

        assert_eq!(read, job);
        writer.execute_batch("ROLLBACK;").unwrap();
        reader.join().unwrap();
        drop(writer);
        assert_eq!(
            EditorApplication::open(&path)
                .unwrap()
                .get_job(&job.job_id)
                .unwrap(),
            job
        );
        let _ = std::fs::remove_file(path);
    }
}

/// Owned native connection adapter. Portable application ports never name rusqlite.
struct SqliteConnection(
    Connection,
    opencut_editor_application::audit::ExecutionContext,
);
impl std::ops::Deref for SqliteConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.0
    }
}
impl std::ops::DerefMut for SqliteConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.0
    }
}
#[cfg(test)]
mod audit_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod project_tests;

impl projects::ProjectRead for EditorApplication {
    fn authorize_edit(
        &self,
        project_id: &str,
        intent: projects::EditIntent<'_>,
    ) -> Result<(), EditorError> {
        projects::ProjectRead::authorize_edit(&self.connection, project_id, intent)
    }
    fn head(&self, id: &str) -> Result<ProjectSnapshot, EditorError> {
        projects::ProjectRead::head(&self.connection, id)
    }
    fn revision(&self, id: &str, revision: &str) -> Result<ProjectSnapshot, EditorError> {
        projects::ProjectRead::revision(&self.connection, id, revision)
    }
    fn replay_create(
        &self,
        input: &CreateProject,
        key: &str,
    ) -> Result<Option<ProjectSnapshot>, EditorError> {
        projects::ProjectRead::replay_create(&self.connection, input, key)
    }
    fn replay_edit(
        &self,
        id: &str,
        intent: projects::EditIntent<'_>,
    ) -> Result<Option<TransactionResult>, EditorError> {
        projects::ProjectRead::replay_edit(&self.connection, id, intent)
    }
}

#[cfg(test)]
mod access_tests;

#[cfg(test)]
mod upload_tests;

#[cfg(test)]
mod project_plugin_tests;

#[cfg(test)]
mod denial_tests;

mod sqlite_database;
