//! Portable application use cases and ports; native storage and host I/O are adapters.
pub mod access;
pub mod access_audit;
pub mod agent_edit;
pub mod agent_evidence;
pub mod agent_read;
pub mod agent_recipe;
pub mod agent_render;
pub mod analysis;
pub mod analysis_evidence;
pub mod analysis_query;
pub mod artifact_gc;
pub mod artifact_retention;
pub mod asset_analysis;
pub mod audio_gain;
pub mod audio_preview;
pub mod audio_preview_recovery;
pub mod audit;
mod blend_preview_binding;
pub mod branching;
mod browser_callback_encoding;
mod caption_preview_binding;
pub mod captions;
pub mod classic_blend_frame;
pub mod classic_mask_frame;
pub mod classic_scene_frame;
pub mod collaboration;
pub mod edit_recovery;
pub mod events;
pub mod export_video;
pub mod font_catalog;
pub mod font_color_static;
pub mod font_derived;
pub mod font_import;
pub mod font_named;
pub mod font_synthesis;
pub mod graphic_edit;
mod graphic_preview_binding;
pub mod historical_snapshot_package;
pub mod historical_visual;
pub mod historical_visual_collaboration;
pub mod historical_visual_jobs;
pub mod historical_visual_package;
pub mod history_navigation;
pub mod identity;
pub mod job_submission;
pub mod job_views;
pub mod jobs;
pub mod keyframe_edit;
pub mod keyframe_legacy_conversion;
pub mod lifecycle;
pub mod maintenance;
pub mod mask_authoring;
pub mod media;
pub mod media_facts;
pub mod media_frame_jobs;
pub mod media_import;
pub mod media_library;
pub mod media_video_jobs;
pub mod plugin_packages;
pub mod plugins;
pub mod presets;
pub mod preview;
pub mod preview_cache;
pub mod project_archive;
pub mod project_catalog;
pub mod project_edit;
pub mod project_exchange;
pub mod project_inspection;
pub mod project_library;
pub mod project_package;
pub mod project_resources;
pub mod projects;
pub mod quality;
pub mod queries;
pub mod render;
pub mod resources;
pub mod revision_actors;
pub mod scene;
pub mod scene_audio_jobs;
pub mod scene_blend_frame_jobs;
pub mod scene_blend_video_jobs;
pub mod scene_bookmarks;
pub mod scene_caption_video_jobs;
pub mod scene_frame_jobs;
pub mod scene_graphic_video_jobs;
pub mod scene_mask_frame_jobs;
pub mod scene_mask_video_jobs;
pub mod scene_text_frame_jobs;
pub mod scene_transcription;
pub mod source_art;
pub mod source_art_waveform_display;
pub mod source_audio;
pub mod source_evidence;
pub mod speaker_turns;
mod storage_records;
pub mod subtitle_ass;
pub mod subtitle_exchange;
pub mod subtitle_text;
pub mod subtitle_text_export;
pub mod subtitles;
pub mod text_authoring;
pub mod text_canvas;
mod text_preview_binding;
pub mod timeline;
pub mod timeline_clipboard;
pub mod timeline_edit;
pub mod track_removal;
pub mod visual_preview;
use identity::content_id;
pub use opencut_api_contracts::{JobState, RestoreRevisionRequest, RevisionPage, RevisionSummary};
use opencut_contracts::EditorError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub use storage_records::{JobEvent, JobProgress, JobRecord, StoredEvent};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobRequest {
    pub idempotency_key: String,
    pub kind: String,
    pub input: Value,
}

fn encode_json<T: Serialize>(value: &T, description: &str) -> Result<String, EditorError> {
    serde_json::to_string(value).map_err(|error| {
        EditorError::new(
            "SERIALIZATION_ERROR",
            format!("could not encode {description}: {error}"),
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

pub mod uploads;

pub mod project_plugins;

pub mod denials;

pub mod storage;

pub mod gc;

pub mod budget;

pub mod database;

pub mod reproducibility;

pub mod tool_execution;

pub mod classic_audio;
pub mod classic_text_frame;
pub mod classic_visual;
pub mod workspace_port;

pub mod classic_caption_frame;
pub mod classic_graphic_frame;
pub mod scene_graphic_frame_jobs;

pub mod scene_caption_frame_jobs;

pub mod scene_video_jobs;

pub mod element_properties;
pub mod element_transform;
pub mod visual_edit_draft;

pub mod scene_authoring;

pub mod keyframe_sample;

pub mod brand_resources;
pub mod catalog_resources;

pub mod subtitle_classic_ass;

pub mod project_readonly;

pub mod subtitle_measurement;
