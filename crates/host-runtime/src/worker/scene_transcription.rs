//! Fixed V4 Classic mix -> local Whisper inference inside one leased attempt.
use super::output_attempt::{cancelled, error};
use super::*;
use opencut_editor_application::{scene_audio_jobs as audio, scene_transcription as transcription};
use opencut_job_contracts::scene_transcription::*;
use opencut_storage_budget::AttemptLease;
use std::sync::Mutex;

pub(super) fn execute(
    database: &Path,
    root: &Path,
    worker: &str,
    job: &JobRecord,
    cancel: &CancellationToken,
    held: &Mutex<Option<AttemptLease>>,
) -> Result<Value, EditorError> {
    cancelled(cancel)?;
    let input: transcription::PreparedSceneTranscription = parse(job.input.clone())?;
    transcription::validate_prepared(&input)?;
    let app = EditorApplication::open_managed(database)?;
    let checkpoint = || {
        cancelled(cancel)?;
        app.revalidate_scene_transcription(&job.job_id, worker, job.attempt, &input.audio.authority)
    };
    checkpoint()?;
    // Configuration must exist before spending resources on the whole-scene mix.
    let model = std::env::var_os("OPENCUT_WHISPER_MODEL")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            error(
                "CAPABILITY_UNAVAILABLE",
                "Local transcription model is not configured",
            )
        })?;
    let cli = executable("OPENCUT_WHISPER_CLI", "whisper-cli").ok_or_else(|| {
        error(
            "CAPABILITY_UNAVAILABLE",
            "Local Whisper executable is unavailable",
        )
    })?;
    let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
    let engine = WhisperBackend::new(cli, model, ffmpeg).map_err(media_error)?;
    let mut audio_job = job.clone();
    audio_job.kind = audio::AUDIO_WAV_KIND.into();
    audio_job.input = serde_json::to_value(&input.audio).map_err(|_| {
        error(
            "SERIALIZATION_ERROR",
            "Cannot prepare Classic inference audio",
        )
    })?;
    let stored = scene_audio::execute_with_checkpoint(
        database,
        root,
        &audio_job,
        cancel,
        held,
        &checkpoint,
    )?;
    let stored: audio::StoredAudioWavResult = parse(stored)?;
    let audio_path = PathBuf::from(&stored.artifact.path);
    let directory = audio_path.parent().ok_or_else(|| {
        error(
            "INVALID_STORED_RESULT",
            "Inference audio has no private attempt directory",
        )
    })?;
    let private_audio = PreviewFiles {
        intermediate: None,
        output: audio_path.clone(),
        keep_output: false,
    };
    #[cfg(unix)]
    let engine = {
        let attempt = held
            .lock()
            .map_err(|_| error("INTERNAL_ERROR", "Attempt guard is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| error("JOB_LEASE_LOST", "Inference output attempt is not held"))?;
        let admission = Arc::new(super::media_output::TaskOutput {
            budget: opencut_storage_budget::Budget::new(
                root.parent()
                    .ok_or_else(|| error("STORAGE_BUDGET_ERROR", "Missing managed workspace"))?,
            ),
            attempt,
            directory: directory.to_path_buf(),
            writer: opencut_editor_application::budget::Writer {
                job_id: job.job_id.clone(),
                attempt: job.attempt,
            },
        });
        engine.with_output_admission(admission, directory.to_path_buf())
    };
    #[cfg(not(unix))]
    return Err(error(
        "CAPABILITY_UNAVAILABLE",
        "Bounded local inference output requires Unix attempt admission",
    ));
    checkpoint()?;
    let result = engine
        .transcribe(
            &TranscribeRequest {
                input: audio_path,
                language: input.language.clone(),
                word_timestamps: false,
            },
            cancel,
        )
        .map_err(media_error)?;
    checkpoint()?;
    let public = SceneTranscriptionResult {
        schema_version: 1,
        project_id: input.audio.snapshot.project.id.clone(),
        revision: input.audio.snapshot.revision.clone(),
        sequence_id: input.audio.sequence_id.clone(),
        duration: opencut_contracts::Time {
            ticks: stored.public.identity.frame_count.clone(),
            timebase: input.audio.sample_rate,
        },
        audio: stored.public,
        transcription: result,
    };
    let value = if let Some(evidence) = stored.audio_decoder_evidence_v1 {
        serde_json::to_value(transcription::StoredSceneTranscription {
            public,
            audio_decoder_evidence_v1: evidence,
        })
    } else {
        serde_json::to_value(public)
    }
    .map_err(|_| {
        error(
            "SERIALIZATION_ERROR",
            "Cannot serialize V4 scene transcription",
        )
    })?;
    transcription::validate_stored_result(&input, value.clone())?;
    drop(private_audio);
    checkpoint()?;
    Ok(value)
}

fn executable(variable: &str, fallback: &str) -> Option<PathBuf> {
    let value = std::env::var_os(variable)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.into());
    let path = PathBuf::from(value);
    if path.components().count() > 1 || path.is_absolute() {
        return crate::browser_runtime::executable(&path).ok();
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .find_map(|directory| crate::browser_runtime::executable(&directory.join(&path)).ok())
}
pub(super) fn capability() -> SceneTranscriptionCapability {
    let audio = crate::classic_audio_runtime::capability();
    let model = std::env::var_os("OPENCUT_WHISPER_MODEL").filter(|v| !v.is_empty());
    let readable_model = model.as_ref().is_some_and(|p| {
        std::fs::File::open(p)
            .ok()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|m| m.is_file() && m.len() > 0)
    });
    let whisper = executable("OPENCUT_WHISPER_CLI", "whisper-cli");
    let ffmpeg = crate::media_tool_runtime::ffmpeg().ok();
    let reason = if !cfg!(unix) {
        Some("Bounded local transcription is unavailable on this host".into())
    } else if !audio.available {
        audio.unavailable_reason
    } else if !readable_model {
        Some("A readable nonempty local OPENCUT_WHISPER_MODEL is required".into())
    } else if whisper.is_none() || ffmpeg.is_none() {
        Some("Configured local whisper-cli and FFmpeg executables must be available".into())
    } else {
        None
    };
    let english_only = model.as_ref().is_some_and(|p| {
        Path::new(p)
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains(".en."))
    });
    SceneTranscriptionCapability {
        schema_version:1, available:reason.is_none(), submit_tool:"opencut_analysis_sequence_transcribe".into(),
        supported_languages:if english_only {vec!["auto".into(),"en".into()]}else{vec!["auto".into(),"en".into(),"zh".into()]},
        timing:"Language controls request local inference; accuracy requires per-language acceptance. Caption word splits estimate time within inferred segments and are not measured word timestamps.".into(),
        unavailable_reason:reason,
    }
}
