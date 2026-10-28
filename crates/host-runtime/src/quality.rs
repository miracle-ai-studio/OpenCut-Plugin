//! Host assembly binds current authority monitoring to the native quality port.
use crate::Automation;
use opencut_api_contracts::quality::*;
use opencut_contracts::EditorError;
use opencut_editor_application::{quality::*, source_art::SourceArtRead};
struct Native {
    database: std::path::PathBuf,
    artifacts: std::path::PathBuf,
    context: opencut_editor_application::audit::ExecutionContext,
}
impl QualityNative for Native {
    fn measure(&self, input: &PreparedQuality) -> Result<MeasuredQuality, EditorError> {
        let database = self.database.clone();
        let project = input.source.snapshot.project.id.clone();
        let revision = input.source.snapshot.revision.clone();
        let authority = input.source.authority.clone();
        let job_id = input.job_id.clone();
        let context = self.context.clone();
        opencut_native_services::quality::measure(
            &self.database,
            &self.artifacts,
            input,
            || {
                Ok((
                    crate::media_tool_runtime::ffmpeg()?,
                    crate::media_tool_runtime::ffprobe()?,
                ))
            },
            move || {
                let mut app = opencut_sqlite_store::EditorApplication::open_managed(&database)?;
                app.set_execution_context(context.clone());
                app.revalidate_source_art(&project, &revision, &authority)?;
                app.artifact_job(&job_id).map(|_| ())
            },
        )
    }
}
pub(super) fn inspect(
    host: &Automation,
    input: QualityInspect,
) -> Result<QualityReport, EditorError> {
    let (job_id, _) = opencut_editor_application::job_views::parse_artifact_id(&input.artifact_id)?;
    let job_id = job_id.to_owned();
    // Inspection consumes published bytes, so ordinary task Read is insufficient.
    // Repeat ReadArtifacts under the original caller during native checkpoints.
    host.app.artifact_job(&job_id)?;
    let artifacts = host
        .database
        .parent()
        .ok_or_else(|| EditorError::new("STORAGE_ERROR", "Workspace root unavailable"))?
        .join("artifacts");
    let report = opencut_editor_application::quality::inspect(
        &host.app,
        &Native {
            database: host.database.clone(),
            artifacts,
            context: host.app.execution_context().clone(),
        },
        input,
    )?;
    host.app.artifact_job(&job_id)?;
    Ok(report)
}
