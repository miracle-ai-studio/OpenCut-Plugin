//! Shared portable contracts; no database, codecs or plugin runtime implementation.
pub mod access;
pub mod agent_evidence;
pub mod agent_read;
pub mod agent_render;
pub mod analysis_evidence;
pub mod analysis_query;
pub mod artifact_gc;
pub mod artifact_retention;
pub mod audio_preview;
pub mod audio_preview_recovery;
pub mod audit;
pub mod blend_frame_plan;
pub mod branching;
pub mod capabilities;
pub mod caption_frame_plan;
pub mod collaboration;
pub mod context;
pub mod edit_control;
pub mod edit_recovery;
pub mod event_stream;
pub mod graphic_frame_plan;
pub mod historical_snapshot_package;
pub mod historical_visual_collaboration;
pub mod historical_visual_package;
pub mod history_navigation;
pub mod job_results;
pub mod job_submission;
pub mod job_views;
pub mod maintenance;
pub mod mask_frame_plan;
pub mod media;
pub mod media_library;
pub mod plugin_packages;
pub mod plugin_publication;
pub mod plugins;
pub mod presets;
pub mod preview;
pub mod project_archive;
pub mod project_exchange;
pub mod project_library;
pub mod project_package;
pub mod quality;
pub mod render;
pub mod revision_actors;
pub mod scene_frame_plan;
pub mod text_frame_plan;
pub mod visual_edit_draft;
pub mod visual_preview;
pub use opencut_contracts::source_art;
pub mod source_preview;
pub use opencut_contracts::source_art_waveform_display;
pub mod subtitle_exchange;
pub mod subtitles;
pub mod text_canvas;
pub use opencut_contracts::timeline;
pub mod timeline_clipboard;
pub mod tools;
use opencut_contracts::{CreateProject, EditorError, TransactionRequest};
use opencut_job_contracts as jobs;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreRevisionRequest {
    pub expected_revision: String,
    pub target_revision: String,
    pub idempotency_key: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RevisionSummary {
    pub revision: String,
    pub parent_revision: Option<String>,
    pub transaction_id: Option<String>,
    pub label: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RevisionPage {
    pub project_id: String,
    pub head_revision: String,
    pub revisions: Vec<RevisionSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

pub use opencut_contracts::project_catalog::CreateProjectRequest;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectInput {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryInput {
    pub project_id: String,
    pub from_revision: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditInput {
    pub project_id: String,
    pub request: TransactionRequest,
}

fn default_limit() -> usize {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyInput {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Response<T = Value> {
    pub request_id: String,
    pub data: Option<T>,
    pub error: Option<EditorError>,
    pub warnings: Vec<String>,
}

impl Response {
    pub fn from_result_with_id(request_id: String, result: Result<Value, EditorError>) -> Self {
        let (data, error) = match result {
            Ok(data) => (Some(data), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            request_id,
            data,
            error,
            warnings: vec![],
        }
    }
}

pub mod uploads;

pub mod project_plugins;

pub mod denials;
pub mod domains;

pub mod storage;

pub mod gc;

pub mod budget;

pub mod database;

pub mod reproducibility;

pub mod execution_receipt;

pub use opencut_contracts::project_readonly;
