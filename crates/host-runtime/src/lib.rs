//! Shared typed automation entry points. Transports must not implement editing rules.
mod quality;
mod render_preflight;
mod source_evidence;
mod text_canvas;
pub use opencut_api_contracts::audio_preview as audio_preview_contracts;
pub use opencut_api_contracts::preview;
pub use opencut_api_contracts::visual_preview as visual_preview_contracts;
pub use opencut_api_contracts::{context, event_stream};
pub use opencut_automation::capabilities;
pub use opencut_editor_application::access::Caller;
pub use opencut_editor_application::audit::{CallSource, ExecutionContext};
pub mod audio_preview;
mod brand_resources;
mod browser_runtime;
mod bundled_runtime;
mod catalog_resources;
mod classic_audio_runtime;
mod classic_visual_runtime;
pub mod denials;
mod export_video;
mod font_catalog;
mod font_color_static;
mod font_derived;
mod font_import;
mod font_named;
mod font_synthesis;
mod source_art;
mod source_art_runtime;
mod subtitle_ass;
mod subtitle_classic_ass;
mod subtitle_text;
pub use opencut_editor_application::uploads;
use std::path::Path;
use uploads::{ImportStore, UploadRetentionStore};

use opencut_contracts::EditorError;
use opencut_native_services::{media, plugins};
use opencut_sqlite_store::EditorApplication;
use opencut_sqlite_store::jobs;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

pub use opencut_api_contracts::domains;
pub use opencut_api_contracts::tools::{ToolSpec, tool_spec, tool_specs};
pub use opencut_api_contracts::{
    CreateProjectRequest, EmptyInput, HistoryInput, ProjectInput, Response,
};

pub use opencut_automation::response;

pub struct Automation {
    app: EditorApplication,
    database: std::path::PathBuf,
    plugin_root: std::path::PathBuf,
    media_root: std::path::PathBuf,
    source: CallSource,
    caller: Caller,
}

impl Automation {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EditorError> {
        let plugin_root = path
            .as_ref()
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("plugins");
        let app = EditorApplication::open_managed(path.as_ref())?;
        let media_root = app
            .asset_store_root()
            .parent()
            .expect("asset store has a parent")
            .join("media");
        Ok(Self {
            database: path.as_ref().to_path_buf(),
            app,
            plugin_root,
            media_root,
            source: CallSource::Embedded,
            caller: Caller::LocalOwner,
        })
    }

    pub fn call(&mut self, name: &str, arguments: Value) -> Response {
        let context = ExecutionContext::for_caller(self.source, self.caller.clone());
        opencut_automation::dispatch(self, context, name, arguments)
    }

    /// Native data-plane entry; never registered as a JSON/base64 control tool.
    pub fn put_upload_chunk(&mut self, input: &uploads::ChunkDescriptor, bytes: &[u8]) -> Response {
        let context = ExecutionContext::for_caller(self.source, self.caller.clone());
        let request_id = context.request_id().to_owned();
        self.app.set_execution_context(context.clone());
        let root = self
            .app
            .asset_store_root()
            .parent()
            .expect("asset store parent")
            .join("uploads");
        let result = opencut_native_services::uploads::put(&root, &mut self.app, input, bytes)
            .and_then(to_value)
            .map_err(uploads::public_error);
        denials::observe_response(
            &self.app,
            Some(context.caller()),
            self.source,
            denials::DeniedOperation::UploadChunk,
            denials::DenialReason::AuthorizationDenied,
            Response::from_result_with_id(request_id, result),
        )
    }

    /// Trusted native host metadata. Never populate from client identity claims.
    pub fn with_source(mut self, source: CallSource) -> Self {
        self.source = source;
        self
    }

    pub fn with_caller(mut self, caller: Caller) -> Self {
        self.caller = caller;
        self
    }

    pub fn authenticate(&self, secret: &str) -> Result<Caller, EditorError> {
        self.app.authenticate(secret)
    }

    pub fn audio_preview_stream(
        &mut self,
        input: opencut_api_contracts::audio_preview::AudioPreviewStream,
    ) -> Result<audio_preview::AudioPreviewReader, EditorError> {
        audio_preview::open_stream(self, input)
    }
    pub fn read_visual_preview_resource(
        &mut self,
        input: &visual_preview_contracts::VisualPreviewResourceRead,
    ) -> Result<(Vec<u8>, bool), EditorError> {
        audio_preview::resources::read(self, input)
    }
    pub fn open_visual_preview_picture(
        &mut self,
        input: visual_preview_contracts::VisualPreviewPictureRead,
    ) -> Result<audio_preview::VisualPictureReader, EditorError> {
        audio_preview::open_picture(self, input)
    }
    pub fn open_visual_preview_font(
        &mut self,
        input: visual_preview_contracts::VisualPreviewFont,
    ) -> Result<(std::fs::File, u64), EditorError> {
        audio_preview::open_font(self, input)
    }
    pub fn authorize_visual_preview_font(
        &mut self,
        input: &visual_preview_contracts::VisualPreviewFont,
    ) -> Result<(), EditorError> {
        audio_preview::authorize_font(self, input)
    }
    pub fn authorize_audio_preview(&mut self, session_id: &str) -> Result<(), EditorError> {
        audio_preview::authorize_stream(self, session_id)
    }

    pub fn authorize_preview(&mut self, session: &str, run: bool) -> Result<(), EditorError> {
        self.app.set_execution_context(ExecutionContext::for_caller(
            self.source,
            self.caller.clone(),
        ));
        self.app.access_view()?;
        let (project, revision) =
            opencut_native_services::preview::target(&self.database, session)?;
        if opencut_native_services::preview::is_historical_visual(&self.database, session)? {
            if run {
                self.app
                    .execution_snapshot_historical_visual(&project, &revision)?;
            } else {
                self.app.inspect_historical_visual_project(
                    &opencut_contracts::historical_visual_ops::InspectProject {
                        project_id: project,
                        revision: Some(revision),
                    },
                )?;
            }
        } else if run {
            opencut_editor_application::access::ProjectExecutionRead::execution_snapshot(
                &self.app, &project, &revision,
            )?;
        } else {
            self.app.snapshot_at(&project, &revision)?;
        }
        Ok(())
    }
    pub fn preview_stream(
        &mut self,
        input: opencut_api_contracts::preview::StreamInput,
    ) -> Result<transport_io::PreviewStream, EditorError> {
        self.authorize_preview(&input.session_id, true)?;
        opencut_native_services::preview::stream(
            &self.database,
            self.caller.principal_id(),
            &input.session_id,
            &input.generation,
            &input.owner_id,
            &input.lease_id,
        )
        .map(transport_io::PreviewStream)
    }

    fn execute(&mut self, name: &str, arguments: Value) -> Result<Value, EditorError> {
        match name {
            "opencut_edit_recovery_stage" => {
                to_value(self.app.stage_edit_recovery(&parse(arguments)?)?)
            }
            "opencut_edit_recovery_inspect" => {
                to_value(self.app.inspect_edit_recovery(&parse(arguments)?)?)
            }
            "opencut_edit_recovery_ack" => {
                to_value(self.app.ack_edit_recovery(&parse(arguments)?)?)
            }

            "opencut_text_canvas_plan" => to_value(text_canvas::plan(self, &parse(arguments)?)?),
            "opencut_analysis_sequence_transcribe_capability" => {
                let _: EmptyInput = parse(arguments)?;
                to_value(worker::sequence_transcribe_capability())
            }
            "opencut_analysis_sequence_transcribe" => {
                let job = opencut_sqlite_store::sqlite_scene_transcription::start(
                    &mut self.app,
                    parse(arguments)?,
                )
                .map_err(opencut_editor_application::job_views::public_error)?;
                to_value(opencut_editor_application::job_views::project(job))
            }
            "opencut_subtitle_generate" => {
                to_value(self.app.generate_scene_captions(&parse(arguments)?)?)
            }
            "opencut_subtitle_text_plan" => {
                to_value(subtitle_text::plan(self, &parse(arguments)?)?)
            }
            "opencut_subtitle_text_export" => {
                to_value(subtitle_text::export(self, &parse(arguments)?)?)
            }
            "opencut_font_catalog" => {
                let _: EmptyInput = parse(arguments)?;
                to_value(font_catalog::catalog()?)
            }
            "opencut_font_source" => to_value(font_catalog::source(&parse(arguments)?)?),
            "opencut_font_import" => to_value(font_catalog::import_font(self, &parse(arguments)?)?),
            "opencut_font_derived_proof" => {
                to_value(opencut_editor_application::font_derived::proof::read(
                    &self.app,
                    &parse(arguments)?,
                )?)
            }
            "opencut_font_synthesis_proof" => {
                to_value(font_synthesis::proof(self, &parse(arguments)?)?)
            }
            "opencut_subtitle_classic_ass_plan" => {
                to_value(subtitle_classic_ass::plan(self, &parse(arguments)?)?)
            }
            "opencut_subtitle_ass_plan" => to_value(subtitle_ass::plan(self, &parse(arguments)?)?),
            "opencut_subtitle_import" => {
                to_value(self.app.import_scene_subtitles(&parse(arguments)?)?)
            }
            "opencut_brand_resources_list" => {
                to_value(brand_resources::catalog(&parse(arguments)?)?)
            }
            "opencut_brand_resources_source" => {
                to_value(brand_resources::source(&parse(arguments)?)?)
            }
            "opencut_brand_resources_import" => {
                to_value(brand_resources::import(self, &parse(arguments)?)?)
            }
            "opencut_catalog_resources_list" => {
                let _: EmptyInput = parse(arguments)?;
                to_value(catalog_resources::catalog(self))
            }
            "opencut_catalog_resources_preview" => {
                to_value(catalog_resources::preview(self, &parse(arguments)?)?)
            }
            "opencut_catalog_resources_generate" => {
                to_value(catalog_resources::generate(self, &parse(arguments)?)?)
            }
            "opencut_catalog_resources_recipe" => to_value(
                opencut_editor_application::catalog_resources::inspect_recipe(
                    &self.app,
                    &parse(arguments)?,
                )?,
            ),
            "opencut_catalog_resources_import" => {
                to_value(catalog_resources::import(self, &parse(arguments)?)?)
            }
            "opencut_edit_timeline_plan" => to_value(opencut_editor_application::timeline::plan(
                &self.app,
                &parse(arguments)?,
            )?),
            "opencut_element_properties_plan" => {
                to_value(opencut_editor_application::element_properties::plan(
                    &self.app,
                    &parse(arguments)?,
                )?)
            }
            "opencut_keyframe_sample" => to_value(
                opencut_editor_application::keyframe_sample::sample(&self.app, &parse(arguments)?)?,
            ),
            "opencut_keyframe_legacy_conversion_plan" => to_value(
                opencut_editor_application::keyframe_legacy_conversion::plan(
                    &self.app,
                    &parse(arguments)?,
                )?,
            ),
            "opencut_keyframe_plan" => to_value(opencut_editor_application::keyframe_edit::plan(
                &self.app,
                &parse(arguments)?,
            )?),
            "opencut_scene_geometry" => to_value(
                opencut_editor_application::scene_authoring::plan(&self.app, &parse(arguments)?)?,
            ),
            "opencut_mask_geometry" => to_value(opencut_editor_application::mask_authoring::plan(
                &self.app,
                &parse(arguments)?,
            )?),
            "opencut_graphic_plan" => to_value(opencut_editor_application::graphic_edit::plan(
                &self.app,
                &parse(arguments)?,
            )?),
            "opencut_text_plan" => to_value(opencut_editor_application::text_authoring::plan(
                &self.app,
                &parse(arguments)?,
            )?),
            "opencut_job_execution_receipt" => {
                let input: opencut_api_contracts::execution_receipt::InspectExecution =
                    parse(arguments)?;
                let job = self.app.artifact_job(&input.job_id)?;
                to_value(worker::receipts::inspect(&self.database, &job)?)
            }
            "opencut_edit_recipe_plan" => to_value(opencut_editor_application::agent_recipe::plan(
                &self.app,
                &parse(arguments)?,
            )?),
            "opencut_project_revision_actors" => to_value(
                opencut_editor_application::revision_actors::read(&self.app, parse(arguments)?)?,
            ),
            "opencut_quality_evidence_plan" => to_value(
                opencut_editor_application::quality::evidence_plan(&self.app, parse(arguments)?)?,
            ),
            "opencut_quality_inspect" => to_value(quality::inspect(self, parse(arguments)?)?),
            "opencut_analysis_asset" => {
                let job = opencut_editor_application::asset_analysis::submit(
                    &mut self.app,
                    parse(arguments)?,
                )
                .map_err(opencut_editor_application::job_views::public_error)?;
                to_value(opencut_editor_application::job_views::project(job))
            }
            "opencut_media_content_search" => to_value(
                opencut_editor_application::agent_evidence::search(&self.app, parse(arguments)?)?,
            ),
            "opencut_media_map_time" => to_value(opencut_editor_application::agent_evidence::map(
                &self.app,
                parse(arguments)?,
            )?),
            "opencut_render_capability" => {
                let _: EmptyInput = parse(arguments)?;
                to_value(render_capability())
            }
            "opencut_render_submit" => {
                let job = opencut_editor_application::agent_render::submit(
                    &mut self.app,
                    parse(arguments)?,
                )
                .map_err(opencut_editor_application::job_views::public_submission_error)?;
                to_value(opencut_editor_application::job_views::project(job))
            }

            "opencut_project_summary" => to_value(opencut_editor_application::agent_read::summary(
                &self.app,
                &parse(arguments)?,
            )?),
            "opencut_edit_agent_plan" => to_value(opencut_editor_application::agent_edit::plan(
                &self.app,
                &parse(arguments)?,
            )?),
            "opencut_project_timeline_query" => to_value(
                opencut_editor_application::agent_read::timeline(&self.app, &parse(arguments)?)?,
            ),
            "opencut_artifact_get" => to_value(opencut_editor_application::agent_read::artifact(
                &self.app,
                &parse(arguments)?,
            )?),
            "opencut_project_promote" => to_value(self.app.promote_project(&parse(arguments)?)?),

            "opencut_media_import_plan" => {
                to_value(self.app.plan_scene_media_import(&parse(arguments)?)?)
            }
            "opencut_project_audio_gain_plan" => {
                to_value(self.app.plan_scene_audio_gain(&parse(arguments)?)?)
            }
            "opencut_timeline_copy" => to_value(
                opencut_editor_application::timeline_clipboard::copy_selection(
                    &self.app,
                    &parse(arguments)?,
                )?,
            ),
            "opencut_timeline_paste_plan" => {
                to_value(opencut_editor_application::timeline_clipboard::plan_paste(
                    &self.app,
                    &parse(arguments)?,
                )?)
            }
            "opencut_timeline_duplicate_plan" => to_value(
                opencut_editor_application::timeline_clipboard::plan_duplicate(
                    &self.app,
                    &parse(arguments)?,
                )?,
            ),
            "opencut_project_source_audio_plan" => {
                to_value(self.app.plan_scene_source_audio(&parse(arguments)?)?)
            }
            "opencut_project_resource_inspect" => {
                to_value(self.app.inspect_project_resource(&parse(arguments)?)?)
            }
            "opencut_project_copy" => {
                let snapshot = self.app.copy_scene_project(&parse(arguments)?)?;
                to_value(opencut_editor_application::scene::public::project(
                    &snapshot,
                ))
            }
            "opencut_project_restore" => to_value(self.app.restore_project(&parse(arguments)?)?),
            "opencut_edit_plan" => to_value(self.app.plan_editor_edit(&parse(arguments)?)?),
            "opencut_edit_apply" => to_value(self.app.apply_editor_edit(&parse(arguments)?)?),
            "opencut_edit_undo_plan" => to_value(self.app.plan_project_undo(&parse(arguments)?)?),
            "opencut_edit_undo_apply" => to_value(self.app.apply_project_undo(&parse(arguments)?)?),
            "opencut_edit_redo_plan" => to_value(self.app.plan_project_redo(&parse(arguments)?)?),
            "opencut_edit_redo_apply" => to_value(self.app.apply_project_redo(&parse(arguments)?)?),
            "opencut_project_history_navigation" => {
                to_value(self.app.editor_history_navigation(&parse(arguments)?)?)
            }

            "opencut_element_transform_sample" => {
                to_value(opencut_editor_application::element_transform::sample(
                    &self.app,
                    &parse(arguments)?,
                )?)
            }
            "opencut_visual_preview_edit_draft_frame" => {
                to_value(audio_preview::edit_draft_picture(self, parse(arguments)?)?)
            }
            "opencut_track_remove_plan" => to_value(
                opencut_editor_application::track_removal::plan(&self.app, &parse(arguments)?)?,
            ),
            "opencut_media_remove_plan" => {
                to_value(opencut_editor_application::media_import::plan_removal(
                    &self.app,
                    &parse(arguments)?,
                )?)
            }

            "opencut_subtitle_export" => to_value(self.app.export_subtitles(&parse(arguments)?)?),
            "opencut_audio_preview_capability" => {
                let _: EmptyInput = parse(arguments)?;
                to_value(classic_audio_runtime::preview_capability())
            }
            "opencut_source_art_frame" => {
                to_value(source_art::source_frame(self, parse(arguments)?)?)
            }
            "opencut_source_art_evidence_cancel" => {
                to_value(source_evidence::cancel(self, parse(arguments)?)?)
            }
            "opencut_source_art_evidence" => {
                to_value(source_evidence::create(self, parse(arguments)?)?)
            }
            "opencut_source_art_get" => to_value(source_art::get(self, parse(arguments)?)?),
            "opencut_source_art_cancel" => to_value(source_art::cancel(self, parse(arguments)?)?),
            "opencut_source_art_waveform_display_get" => {
                to_value(source_art::waveform_display(self, parse(arguments)?)?)
            }
            "opencut_audio_preview_create" => {
                if matches!(self.source, CallSource::Cli | CallSource::McpStdio) {
                    return Err(EditorError::new(
                        "LONG_LIVED_HOST_REQUIRED",
                        "Audio preview requires a long-lived host",
                    ));
                }
                to_value(audio_preview::create(self, parse(arguments)?)?)
            }

            "opencut_audio_preview_recovery_stage" => {
                to_value(audio_preview::recovery::stage(self, parse(arguments)?)?)
            }
            "opencut_audio_preview_recovery_inspect" => {
                to_value(audio_preview::recovery::inspect(self, parse(arguments)?)?)
            }
            "opencut_audio_preview_recovery_dispatch" => {
                to_value(audio_preview::recovery::dispatch(self, parse(arguments)?)?)
            }
            "opencut_audio_preview_recovery_ack" => {
                to_value(audio_preview::recovery::ack(self, parse(arguments)?)?)
            }
            "opencut_audio_preview_recovery_retire" => {
                to_value(audio_preview::recovery::retire(self, parse(arguments)?)?)
            }

            "opencut_visual_preview_resource_open" => {
                to_value(audio_preview::resources::open(self, parse(arguments)?)?)
            }
            "opencut_visual_preview_resource_release" => {
                to_value(audio_preview::resources::release(self, parse(arguments)?)?)
            }
            "opencut_visual_preview_resource_read" => {
                let input: visual_preview_contracts::VisualPreviewResourceRead = parse(arguments)?;
                let (bytes, eof) = self.read_visual_preview_resource(&input)?;
                to_value(visual_preview_contracts::VisualPreviewResourceChunk {
                    offset: input.offset,
                    bytes_hex: bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
                    eof,
                })
            }
            "opencut_visual_preview_font_authorize" => {
                let input: visual_preview_contracts::VisualPreviewFont = parse(arguments)?;
                self.authorize_visual_preview_font(&input)?;
                to_value(visual_preview_contracts::VisualPreviewFontAuthorized {
                    font_id: input.font_id,
                })
            }
            "opencut_visual_preview_font_batch_authorize" => to_value(
                audio_preview::authorize_font_batch(self, parse(arguments)?)?,
            ),
            "opencut_visual_preview_frame" => {
                to_value(audio_preview::scene_picture(self, parse(arguments)?)?)
            }
            "opencut_visual_preview_cache_prepare" => {
                to_value(audio_preview::cache::prepare(self, parse(arguments)?)?)
            }
            "opencut_audio_preview_inspect" => {
                to_value(audio_preview::inspect(self, parse(arguments)?)?)
            }
            "opencut_audio_preview_events" => {
                to_value(audio_preview::events(self, parse(arguments)?)?)
            }
            "opencut_audio_preview_control" => {
                to_value(audio_preview::control(self, parse(arguments)?)?)
            }
            "opencut_audio_preview_acquire" => {
                to_value(audio_preview::acquire(self, parse(arguments)?)?)
            }
            "opencut_preview_create" => {
                if matches!(self.source, CallSource::Cli | CallSource::McpStdio) {
                    return Err(EditorError::new(
                        "CAPABILITY_UNAVAILABLE",
                        "Preview media requires an HTTP host; use CLI --endpoint or Streamable HTTP MCP for session control",
                    ));
                }
                use opencut_editor_application::scene::{SceneRead, StoredProjectDocument};
                use opencut_native_services::preview::{self, PreviewSource};
                let input: opencut_api_contracts::preview::Create = parse(arguments)?;
                // Persisted document facts select the existing graph. Callers
                // cannot select a generation or bypass execution authorization.
                match self
                    .app
                    .revision_scene_source(&input.project_id, &input.revision)?
                    .document
                {
                    StoredProjectDocument::HistoricalVisual(_) => {
                        let authorized = self.app.execution_snapshot_historical_visual(
                            &input.project_id,
                            &input.revision,
                        )?;
                        to_value(preview::create(
                            &self.database,
                            self.app.asset_store_root(),
                            self.caller.principal_id(),
                            &input,
                            PreviewSource::Visual(authorized.snapshot),
                            authorized.sources,
                            || {
                                self.app
                                    .execution_snapshot_historical_visual(
                                        &input.project_id,
                                        &input.revision,
                                    )
                                    .map(|_| ())
                            },
                        )?)
                    }
                    StoredProjectDocument::Legacy(_) => {
                        use opencut_editor_application::access::ProjectExecutionRead;
                        let authorized = self
                            .app
                            .execution_snapshot(&input.project_id, &input.revision)?;
                        to_value(preview::create(
                            &self.database,
                            self.app.asset_store_root(),
                            self.caller.principal_id(),
                            &input,
                            PreviewSource::Basic(authorized.snapshot),
                            authorized.sources,
                            || {
                                self.app
                                    .execution_snapshot(&input.project_id, &input.revision)
                                    .map(|_| ())
                            },
                        )?)
                    }
                    StoredProjectDocument::Editable(_) => Err(EditorError::new(
                        "UNSUPPORTED_RENDER_FEATURE",
                        "This document requires the authorized sound-clock and GPU preview capabilities",
                    )),
                }
            }
            "opencut_preview_acquire" => {
                let input: opencut_api_contracts::preview::Acquire = parse(arguments)?;
                self.authorize_preview(&input.session_id, true)?;
                to_value(opencut_native_services::preview::acquire(
                    &self.database,
                    self.caller.principal_id(),
                    &input,
                )?)
            }
            "opencut_preview_inspect" => {
                let input: opencut_api_contracts::preview::SessionInput = parse(arguments)?;
                self.authorize_preview(&input.session_id, false)?;
                to_value(opencut_native_services::preview::inspect(
                    &self.database,
                    &input.session_id,
                )?)
            }
            "opencut_preview_events" => {
                let input: opencut_api_contracts::preview::EventsInput = parse(arguments)?;
                self.authorize_preview(&input.session_id, false)?;
                to_value(opencut_native_services::preview::events(
                    &self.database,
                    &input,
                )?)
            }
            "opencut_preview_control" => {
                let input: opencut_api_contracts::preview::Control = parse(arguments)?;
                self.authorize_preview(&input.session_id, true)?;
                to_value(opencut_native_services::preview::control(
                    &self.database,
                    self.caller.principal_id(),
                    &input,
                )?)
            }
            "opencut_database_inspect" => {
                let _: EmptyInput = parse(arguments)?;
                to_value(opencut_editor_application::database::inspect(
                    &self.caller,
                    &self.app,
                )?)
            }
            "opencut_database_checkpoint" => {
                to_value(opencut_editor_application::database::checkpoint(
                    &self.caller,
                    &self.app,
                    parse(arguments)?,
                )?)
            }
            "opencut_storage_budget_inspect" | "opencut_storage_budget_recover" => {
                let _: opencut_api_contracts::EmptyInput = parse(arguments)?;
                to_value(opencut_editor_application::budget::inspect(
                    &self.caller,
                    &opencut_native_services::budget::NativeBudget(&self.database),
                    name == "opencut_storage_budget_recover",
                )?)
            }
            "opencut_storage_budget_configure" => {
                to_value(opencut_editor_application::budget::configure(
                    self.app.execution_context(),
                    &opencut_native_services::budget::NativeBudget(&self.database),
                    parse(arguments)?,
                )?)
            }
            "opencut_cas_gc_query" => to_value(opencut_editor_application::gc::query(
                &self.app,
                parse(arguments)?,
            )?),
            "opencut_cas_gc_plan" => {
                let root = self.app.asset_store_root().to_path_buf();
                to_value(opencut_editor_application::gc::plan(
                    &mut self.app,
                    &opencut_native_services::gc::NativeGc(&root, &self.database),
                    parse(arguments)?,
                )?)
            }
            "opencut_cas_gc_execute" => {
                let root = self.app.asset_store_root().to_path_buf();
                to_value(opencut_editor_application::gc::execute(
                    &mut self.app,
                    &opencut_native_services::gc::NativeGc(&root, &self.database),
                    parse(arguments)?,
                )?)
            }
            "opencut_cas_gc_inspect" => to_value(opencut_editor_application::gc::inspect(
                &self.app,
                parse(arguments)?,
            )?),
            "opencut_artifact_retention_inspect" => {
                to_value(opencut_editor_application::artifact_retention::inspect(
                    &self.app,
                    parse(arguments)?,
                )?)
            }
            "opencut_artifact_references_inspect" => {
                to_value(opencut_editor_application::artifact_retention::references(
                    &self.app,
                    parse(arguments)?,
                )?)
            }
            "opencut_artifact_gc_plan" => {
                let artifact_root = self
                    .app
                    .asset_store_root()
                    .parent()
                    .expect("workspace root")
                    .join("artifacts");
                to_value(opencut_editor_application::artifact_gc::plan(
                    &mut self.app,
                    &opencut_native_services::artifacts::ArtifactPlanner(&artifact_root),
                    parse(arguments)?,
                )?)
            }
            "opencut_artifact_gc_inspect" => to_value(
                opencut_editor_application::artifact_gc::inspect(&self.app, parse(arguments)?)?,
            ),
            "opencut_artifact_gc_query" => to_value(
                opencut_editor_application::artifact_gc::query(&self.app, parse(arguments)?)?,
            ),
            "opencut_artifact_gc_execute" => {
                let artifact_root = self
                    .app
                    .asset_store_root()
                    .parent()
                    .expect("workspace root")
                    .join("artifacts");
                to_value(opencut_editor_application::artifact_gc::execute(
                    &mut self.app,
                    &opencut_native_services::artifacts::ArtifactPlanner(&artifact_root),
                    parse(arguments)?,
                )?)
            }
            "opencut_artifact_gc_execution_inspect" => {
                to_value(opencut_editor_application::artifact_gc::execution_inspect(
                    &self.app,
                    parse(arguments)?,
                )?)
            }
            "opencut_artifact_retention_release" => {
                to_value(opencut_editor_application::artifact_retention::release(
                    &mut self.app,
                    parse(arguments)?,
                )?)
            }
            "opencut_storage_inspect" => to_value(opencut_editor_application::storage::inspect(
                &self.caller,
                &opencut_native_services::storage::NativeStorage(&self.database),
                parse(arguments)?,
            )?),
            "opencut_upload_begin" => to_value(
                uploads::begin(&mut self.app, parse(arguments)?).map_err(uploads::public_error)?,
            ),
            "opencut_upload_inspect" => to_value(
                uploads::inspect(&self.app, parse(arguments)?).map_err(uploads::public_error)?,
            ),
            "opencut_upload_complete" => to_value(
                uploads::complete(&mut self.app, parse(arguments)?)
                    .map_err(uploads::public_error)?,
            ),
            "opencut_upload_abort" => {
                let root = self
                    .app
                    .asset_store_root()
                    .parent()
                    .expect("asset store parent")
                    .join("uploads");
                to_value(
                    opencut_native_services::uploads::abort(
                        &root,
                        &mut self.app,
                        parse(arguments)?,
                    )
                    .map_err(uploads::public_error)?,
                )
            }
            "opencut_upload_retention" => {
                let input: uploads::UploadInput = parse(arguments)?;
                to_value(
                    self.app
                        .upload_retention(&input.upload_id)
                        .map_err(uploads::public_error)?,
                )
            }
            "opencut_upload_query" => to_value(
                uploads::query_uploads(&self.app, parse(arguments)?)
                    .map_err(uploads::public_error)?,
            ),
            "opencut_upload_quota" => {
                let input: ProjectInput = parse(arguments)?;
                to_value(
                    self.app
                        .upload_quota(&input.project_id)
                        .map_err(uploads::public_error)?,
                )
            }
            "opencut_upload_release" => {
                let root = self
                    .app
                    .asset_store_root()
                    .parent()
                    .expect("asset store parent")
                    .join("uploads");
                to_value(
                    opencut_native_services::uploads::release(
                        &root,
                        &mut self.app,
                        parse(arguments)?,
                    )
                    .map_err(uploads::public_error)?,
                )
            }
            "opencut_project_media_inspect" => to_value(
                self.app
                    .project_media(&parse(arguments)?)
                    .map_err(uploads::public_error)?,
            ),
            "opencut_project_media_query" => to_value(
                uploads::query_media(&self.app, parse(arguments)?)
                    .map_err(uploads::public_error)?,
            ),
            "opencut_access_inspect" => {
                let _: EmptyInput = parse(arguments)?;
                to_value(self.app.access_view()?)
            }
            "opencut_denial_query" => {
                use opencut_editor_application::denials::DenialStore;
                to_value(self.app.read_denials(&parse(arguments)?)?)
            }
            "opencut_audit_query" => to_value(self.app.audit_query(parse(arguments)?)?),
            "opencut_access_audit_query" => {
                to_value(self.app.access_audit_query(parse(arguments)?)?)
            }
            "opencut_plugin_publication_query" => {
                to_value(self.app.plugin_publication_query(parse(arguments)?)?)
            }
            "opencut_plugin_publication_reconcile" => to_value(plugins::reconcile(
                &self.plugin_root,
                parse(arguments)?,
                &self.app,
            )?),
            "opencut_maintenance_query" => to_value(self.app.maintenance_query(parse(arguments)?)?),
            "opencut_maintenance_reconcile" => to_value(media::reconcile(
                &self.media_root,
                parse(arguments)?,
                &self.app,
            )?),
            "opencut_job_view" => {
                let input: jobs::JobInput = parse(arguments)?;
                to_value(self.app.job_view(&input.job_id)?)
            }
            "opencut_resource_read" => to_value(
                self.app
                    .read_resource(parse(arguments)?, capabilities::document)?,
            ),
            "opencut_event_changes" => to_value(self.app.event_changes(parse(arguments)?)?),
            "opencut_job_query" => to_value(
                self.app
                    .job_query(parse(arguments)?)
                    .map_err(opencut_editor_application::job_views::public_error)?,
            ),
            "opencut_job_changes" => to_value(
                self.app
                    .job_changes(parse(arguments)?)
                    .map_err(opencut_editor_application::job_views::public_error)?,
            ),
            "opencut_job_submit" => {
                let record = opencut_editor_application::job_submission::submit(
                    &mut self.app,
                    parse(arguments)?,
                )
                .map_err(opencut_editor_application::job_views::public_submission_error)?;
                to_value(opencut_editor_application::job_views::project(record))
            }
            "opencut_analysis_speaker_turns" => {
                let root = self.app.asset_store_root().to_path_buf();
                let record = opencut_sqlite_store::speaker_turns::start(
                    &mut self.app,
                    &root,
                    parse(arguments)?,
                )
                .map_err(opencut_editor_application::job_views::public_error)?;
                to_value(opencut_editor_application::job_views::project(record))
            }
            "opencut_analysis_transcribe" => {
                let asset_store_root = self.app.asset_store_root().to_path_buf();
                let record = opencut_sqlite_store::jobs::start_transcribe(
                    &mut self.app,
                    &asset_store_root,
                    parse(arguments)?,
                )
                .map_err(opencut_editor_application::job_views::public_error)?;
                to_value(opencut_editor_application::job_views::project(record))
            }

            "opencut_analysis_query" => to_value(self.app.analysis_query(parse(arguments)?)?),
            "opencut_analysis_evidence_query" => {
                to_value(self.app.analysis_evidence_query(parse(arguments)?)?)
            }
            "opencut_analysis_silence" | "opencut_analysis_shots" | "opencut_analysis_waveform" => {
                let root = self.app.asset_store_root().to_path_buf();
                let record = match name {
                    "opencut_analysis_silence" => opencut_sqlite_store::analysis::start_silence(
                        &mut self.app,
                        &root,
                        parse(arguments)?,
                    ),
                    "opencut_analysis_shots" => opencut_sqlite_store::analysis::start_shots(
                        &mut self.app,
                        &root,
                        parse(arguments)?,
                    ),
                    _ => opencut_sqlite_store::analysis::start_waveform(
                        &mut self.app,
                        &root,
                        parse(arguments)?,
                    ),
                }
                .map_err(opencut_editor_application::job_views::public_error)?;
                to_value(opencut_editor_application::job_views::project(record))
            }

            "opencut_edit_branch_create" => to_value(self.app.create_branch(&parse(arguments)?)?),
            "opencut_edit_branch_list" => to_value(self.app.list_branches(&parse(arguments)?)?),
            "opencut_edit_branch_compare" => {
                to_value(self.app.compare_branches(&parse(arguments)?)?)
            }
            "opencut_edit_branch_merge" => to_value(self.app.merge_branches(&parse(arguments)?)?),
            "opencut_edit_control_get" => to_value(self.app.get_edit_control(&parse(arguments)?)?),
            "opencut_edit_control_set" => to_value(self.app.set_edit_control(&parse(arguments)?)?),
            "opencut_project_package_reproducibility" => {
                self.app.package_require_owner()?;
                to_value(opencut_editor_application::reproducibility::inspect(
                    &parse(arguments)?,
                    &self.caller,
                    &opencut_native_services::reproducibility::NativeReproducibility,
                )?)
            }
            "opencut_project_package_choose_location" => {
                self.app.package_require_owner()?;
                to_value(
                    opencut_editor_application::project_exchange::choose_location(
                        &self.caller,
                        &opencut_native_services::package_location::NativePackageLocation,
                        parse(arguments)?,
                    )?,
                )
            }
            "opencut_project_package_export" => {
                self.app.package_require_owner()?;
                let input: opencut_api_contracts::project_exchange::ExportPackage =
                    parse(arguments)?;
                let source = self.app.project_package_source(&input)?;
                to_value(opencut_native_services::project_exchange::export_package(
                    &input,
                    &source,
                    self.app.asset_store_root(),
                    &self.plugin_root,
                )?)
            }
            "opencut_project_package_inspect" => {
                self.app.package_require_owner()?;
                to_value(opencut_native_services::project_exchange::inspect_package(
                    &parse(arguments)?,
                )?)
            }
            "opencut_project_package_import" => {
                self.app.package_require_owner()?;
                let input: opencut_api_contracts::project_exchange::ImportPackage =
                    parse(arguments)?;
                if let Some(receipt) = self.app.project_package_replay(&input)? {
                    return to_value(receipt);
                }
                let prepared = opencut_native_services::project_exchange::read_package(
                    &input,
                    self.app.asset_store_root(),
                )?;
                to_value(self.app.project_package_commit(&input, &prepared)?)
            }
            "opencut_job_request_cancel" => {
                let input: jobs::JobInput = parse(arguments)?;
                let record = self
                    .app
                    .cancel_job(&input.job_id)
                    .map_err(opencut_editor_application::job_views::public_error)?;
                to_value(opencut_editor_application::job_views::project(record))
            }

            "opencut_media_inspect" => {
                let input: media::InspectMediaInput = parse(arguments)?;
                to_value(media::inspect(&self.media_root, input)?)
            }
            "opencut_media_repair" => to_value(media::repair_observed(
                &self.media_root,
                parse(arguments)?,
                &self.app,
            )?),
            "opencut_project_media_remove" => {
                to_value(self.app.remove_project_media(&parse(arguments)?)?)
            }
            "opencut_project_duplicate" => {
                to_value(self.app.duplicate_projects(&parse(arguments)?)?)
            }
            "opencut_project_delete" => {
                let root = self
                    .app
                    .asset_store_root()
                    .parent()
                    .expect("asset store parent")
                    .join("uploads");
                to_value(opencut_native_services::uploads::delete_projects(
                    &root,
                    &mut self.app,
                    &parse(arguments)?,
                )?)
            }

            "opencut_project_history" => {
                let input: HistoryInput = parse(arguments)?;
                to_value(self.app.history(
                    &input.project_id,
                    input.from_revision.as_deref(),
                    input.limit,
                )?)
            }
            "opencut_project_plugin_grant" => {
                use opencut_editor_application::project_plugins::{
                    self as scoped, ProjectPluginStore,
                };
                to_value(
                    self.app
                        .set_plugin_grant(&parse(arguments)?)
                        .map_err(scoped::public_error)?,
                )
            }
            "opencut_project_plugin_list" => {
                use opencut_editor_application::project_plugins::{
                    self as scoped, ProjectPluginStore,
                };
                let input: ProjectInput = parse(arguments)?;
                to_value(
                    self.app
                        .plugin_grants(&input.project_id)
                        .map_err(scoped::public_error)?,
                )
            }
            "opencut_project_plugin_inspect" => {
                use opencut_editor_application::project_plugins as scoped;
                to_value(
                    scoped::inspect(
                        &self.app,
                        &plugins::NativePlugins(&self.plugin_root, None),
                        parse(arguments)?,
                    )
                    .map_err(scoped::public_error)?,
                )
            }
            "opencut_project_readonly_create" => {
                use opencut_editor_application::project_readonly as ro;
                let input = parse(arguments)?;
                let created = ro::create(&mut self.app, &input)?;
                to_value(ro::inspect(
                    &self.app,
                    &plugins::NativePlugins(&self.plugin_root, None),
                    &opencut_contracts::project_inspection::InspectProject {
                        project_id: created.document.id,
                        revision: Some(created.revision),
                    },
                )?)
            }

            "opencut_project_readonly_manifest" => {
                use opencut_editor_application::project_readonly::{
                    self as ro, ReadOnlyProjectStore,
                };
                let snapshot = self.app.read_readonly_project(&parse(arguments)?)?;
                let manifest = ro::manifest(&snapshot)?;
                self.app
                    .checkpoint_readonly_read(&snapshot.document.id, &snapshot.revision)?;
                to_value(manifest)
            }
            "opencut_project_plugin_plan" => {
                use opencut_editor_application::project_plugins as scoped;
                to_value(
                    scoped::plan(
                        &self.app,
                        &plugins::NativePlugins(&self.plugin_root, None),
                        parse(arguments)?,
                    )
                    .map_err(scoped::public_error)?,
                )
            }
            "opencut_plugin_grant"
            | "opencut_plugin_grants"
            | "opencut_plugin_preset_inspect"
            | "opencut_plugin_preset_plan" => {
                use opencut_editor_application::presets::{self as presets, PresetGrantStore};
                let workspace = self
                    .app
                    .asset_store_root()
                    .parent()
                    .expect("managed workspace");
                let catalog = opencut_native_services::presets::PresetPackages::new(workspace);
                match name {
                    "opencut_plugin_grant" => to_value(presets::set_grant(
                        &mut self.app,
                        &catalog,
                        &parse(arguments)?,
                    )?),
                    "opencut_plugin_grants" => {
                        let input: ProjectInput = parse(arguments)?;
                        to_value(self.app.preset_grants(&input.project_id)?)
                    }
                    "opencut_plugin_preset_inspect" => {
                        to_value(presets::inspect(&self.app, &catalog, parse(arguments)?)?)
                    }
                    _ => to_value(presets::plan(&self.app, &catalog, parse(arguments)?)?),
                }
            }
            "opencut_plugin_install" | "opencut_plugin_list" | "opencut_plugin_inspect" => {
                use opencut_editor_application::plugin_packages as packages;
                let workspace = self
                    .app
                    .asset_store_root()
                    .parent()
                    .expect("managed workspace");
                let presets = opencut_native_services::presets::PresetPackages::new(workspace);
                let wasm = plugins::NativePlugins(&self.plugin_root, Some(&self.app));
                match name {
                    "opencut_plugin_install" => to_value(packages::install(
                        &wasm,
                        &presets,
                        &opencut_native_services::plugin_packages::NativePluginPackageSource,
                        &parse(arguments)?,
                    )?),
                    "opencut_plugin_inspect" => {
                        to_value(packages::inspect(&wasm, &presets, &parse(arguments)?)?)
                    }
                    _ => {
                        let _: EmptyInput = parse(arguments)?;
                        to_value(packages::list(&wasm, &presets)?)
                    }
                }
            }

            "opencut_render_preflight" => {
                let asset_store_root = self.app.asset_store_root().to_path_buf();
                to_value(render_preflight::preflight(
                    &self.app,
                    &asset_store_root,
                    parse(arguments)?,
                )?)
            }

            "opencut_capabilities" => {
                let _: EmptyInput = parse(arguments)?;
                Ok(capabilities())
            }
            "opencut_project_inspect" => {
                to_value(opencut_editor_application::project_inspection::inspect(
                    &self.app,
                    &plugins::NativePlugins(&self.plugin_root, None),
                    &parse(arguments)?,
                )?)
            }
            "opencut_project_list" => to_value(self.app.project_library(&parse(arguments)?)?),
            "opencut_project_create" => {
                let input: CreateProjectRequest = parse(arguments)?;
                to_value(self.app.create_project_request(&input)?)
            }

            _ => Err(EditorError::new(
                "NOT_FOUND",
                format!("Unknown operation: {name}"),
            )),
        }
    }
}

impl opencut_editor_application::tool_execution::ToolExecution for Automation {
    fn begin(&mut self, context: ExecutionContext) -> Result<(), EditorError> {
        self.source = context.actor().source;
        self.caller = context.caller().clone();
        self.app.set_execution_context(context);
        self.app.access_view().map(|_| ())
    }
    fn execute_tool(
        &mut self,
        request: opencut_api_contracts::tools::ToolRequest,
    ) -> Result<Value, EditorError> {
        let (name, arguments) = request.into_parts()?;
        self.execute(name, arguments)
    }
    fn record_tool_denial(
        &self,
        denial: &opencut_editor_application::denials::Denial<'_>,
    ) -> Result<(), EditorError> {
        opencut_editor_application::denials::DenialStore::record_denial(&self.app, denial)
    }
}

fn render_capability() -> opencut_api_contracts::agent_render::RenderCapability {
    use opencut_api_contracts::agent_render::{RenderAvailability, RenderCapability};
    let frame = classic_visual_runtime::scene_capability();
    let audio = classic_audio_runtime::capability();
    let video = export_video::capability();
    RenderCapability {
        frame: RenderAvailability {
            available: frame.available,
            unavailable_reason: frame.unavailable_reason,
        },
        audio: RenderAvailability {
            available: audio.available,
            unavailable_reason: audio.unavailable_reason,
        },
        video: RenderAvailability {
            available: video.available,
            unavailable_reason: video.unavailable_reason,
        },
        video_audio: video.audio_available,
        formats: video.formats,
        qualities: video.qualities,
        sample_rates: audio.sample_rates,
        submit_tool: "opencut_render_submit".into(),
    }
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, EditorError> {
    serde_json::from_value(value).map_err(invalid)
}
fn invalid(error: impl std::fmt::Display) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", error.to_string())
}
fn to_value(value: impl Serialize) -> Result<Value, EditorError> {
    serde_json::to_value(value).map_err(|e| EditorError::new("INTERNAL_ERROR", e.to_string()))
}

pub fn exit_code(error: &EditorError) -> i32 {
    error_policy(&error.code).exit_code
}

pub use opencut_automation::{ErrorPolicy, error_policy};

#[cfg(test)]
mod integration_tests {
    use super::*;
    use serde_json::json;

    fn data(response: Response) -> Value {
        assert!(response.error.is_none(), "{:?}", response.error);
        response.data.unwrap()
    }

    #[test]
    fn in_memory_bind_preflight_and_job_share_private_asset_store() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("media.bin");
        std::fs::write(&source, b"captured bytes").unwrap();
        let mut automation = Automation::open(":memory:").unwrap();
        let asset_store_root = automation.app.asset_store_root().to_path_buf();
        assert!(asset_store_root.starts_with(std::env::temp_dir()));
        assert_ne!(
            asset_store_root,
            std::env::current_dir().unwrap().join("asset-store")
        );

        let created = data(automation.call(
            "opencut_project_create",
            json!({"project":{"id":"memory","name":"Memory","sequence_id":"main","width":64,"height":48,"fps":{"numerator":10,"denominator":1}},"idempotency_key":"create"}),
        ));
        let span =
            json!({"start":{"ticks":"0","timebase":10},"duration":{"ticks":"10","timebase":10}});
        let applied = data(automation.call(
            "opencut_edit_apply",
            json!({"project_id":"memory","request":{"expected_revision":created["revision"],"idempotency_key":"bind","label":"bind","operations":[
                {"type":"asset.bind","asset":{"id":"asset","name":"Asset","kind":"video","uri":source,"duration":{"ticks":"10","timebase":10},"metadata":{}}},
                {"type":"track.create","sequence_id":"main","track":{"id":"video","name":"Video","kind":"video","enabled":true,"muted":false,"locked":false,"clips":[],"params":{}}},
                {"type":"clip.insert","sequence_id":"main","track_id":"video","clip":{"id":"clip","asset_id":"asset","timeline_range":span,"source_range":span,"params":{},"channels":[]}}
            ]}}),
        ));
        std::fs::remove_file(&source).unwrap();

        let preflight = data(automation.call(
            "opencut_render_preflight",
            json!({"project_id":"memory","revision":applied["revision"],"sequence_id":"main","container":"mp4","video_codec":"h264"}),
        ));
        assert_eq!(preflight["revision"], applied["revision"]);
        assert!(
            preflight["issues"]
                .as_array()
                .unwrap()
                .iter()
                .all(|issue| { !issue["code"].as_str().unwrap().starts_with("ASSET_") })
        );

        let job = data(automation.call(
            "opencut_job_start",
            json!({"kind":"render.project","idempotency_key":"render","input":{"project_id":"memory","revision":applied["revision"],"sequence_id":"main","container":"mp4","video_codec":"h264"}}),
        ));
        assert_eq!(job["input"]["asset_lock"]["assets"][0]["asset_id"], "asset");
        assert!(asset_store_root.exists());
        drop(automation);
        assert!(!asset_store_root.exists());
    }
}

pub mod worker;

pub mod transport_io;
pub mod workspace;

#[cfg(test)]
mod project_public_tests;
