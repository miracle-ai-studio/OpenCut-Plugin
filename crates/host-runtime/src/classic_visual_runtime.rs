//! Trusted native renderer assembly, separate from public project inputs.
#[cfg(test)]
use crate::native_worker_runtime::worker_at;
use opencut_contracts::EditorError;
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;
use opencut_native_services::classic_visual::ClassicVisualProcessConfig;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
fn worker() -> Result<PathBuf, EditorError> {
    opencut_native_services::native_visual::platform_backend()?;
    crate::native_worker_runtime::worker()
}
pub(crate) fn configured(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    opencut_editor_application::media_frame_jobs::validate_policy(policy)?;
    Ok(ClassicVisualProcessConfig {
        worker: worker()?,
        deadline: Duration::from_millis(policy.deadline_ms),
        policy: policy.clone(),
        strict_cgroup: None,
    })
}
pub(crate) fn check_export_video_runtime() -> Result<(), EditorError> {
    worker().map(|_| ())
}
fn video_runtime() -> Result<(), EditorError> {
    worker()?;
    let audio = crate::classic_audio_runtime::capability();
    if !audio.available {
        return Err(EditorError::new(
            "CAPABILITY_UNAVAILABLE",
            audio
                .unavailable_reason
                .unwrap_or_else(|| "AUDIO_RUNTIME_UNAVAILABLE".into()),
        ));
    }
    crate::media_tool_runtime::ffmpeg()?;
    crate::media_tool_runtime::ffprobe()?;
    Ok(())
}
pub(crate) fn configured_text(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    configured(policy)
}
pub(crate) fn configured_caption(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    configured(policy)
}
pub(crate) fn configured_graphic(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    configured(policy)
}
pub(crate) fn configured_blend(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    configured(policy)
}
pub(crate) fn configured_mask(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    configured(policy)
}
pub(crate) fn configured_scene(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    configured(policy)
}
pub(crate) fn capability() -> opencut_job_contracts::media_frame::MediaFramePngCapability {
    let result = worker().map(|_| ());
    opencut_job_contracts::media_frame::MediaFramePngCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn text_capability() -> opencut_job_contracts::scene_text_frame::TextFramePngCapability {
    let result = worker().map(|_| ());
    opencut_job_contracts::scene_text_frame::TextFramePngCapability {
        schema_version: 2,
        available: result.is_ok(),
        submit_tool: "opencut_job_text_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn caption_capability()
-> opencut_job_contracts::scene_caption_frame::CaptionFrameCapability {
    let result = worker().map(|_| ());
    opencut_job_contracts::scene_caption_frame::CaptionFrameCapability {
        schema_version: 3,
        available: result.is_ok(),
        submit_tool: "opencut_job_caption_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn graphic_capability()
-> opencut_job_contracts::scene_graphic_frame::GraphicFrameCapability {
    let result = worker().map(|_| ());
    opencut_job_contracts::scene_graphic_frame::GraphicFrameCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_graphic_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn blend_capability() -> opencut_job_contracts::scene_blend_frame::BlendFrameCapability {
    let result = worker().map(|_| ());
    opencut_job_contracts::scene_blend_frame::BlendFrameCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_blend_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn mask_capability() -> opencut_job_contracts::scene_mask_frame::MaskFrameCapability {
    let result = worker().map(|_| ());
    opencut_job_contracts::scene_mask_frame::MaskFrameCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_mask_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn scene_capability() -> opencut_job_contracts::scene_frame::SceneFrameCapability {
    let result = worker().map(|_| ());
    opencut_job_contracts::scene_frame::SceneFrameCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_scene_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn video_capability() -> opencut_job_contracts::media_video::MediaVideoMp4Capability {
    let result = video_runtime();
    opencut_job_contracts::media_video::MediaVideoMp4Capability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_video_mp4".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn caption_video_capability()
-> opencut_job_contracts::scene_caption_video::CaptionVideoMp4Capability {
    let result = video_runtime();
    opencut_job_contracts::scene_caption_video::CaptionVideoMp4Capability {
        schema_version: 2,
        available: result.is_ok(),
        submit_tool: "opencut_job_caption_video_mp4".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn graphic_video_capability()
-> opencut_job_contracts::scene_graphic_video::GraphicVideoMp4Capability {
    let result = video_runtime();
    opencut_job_contracts::scene_graphic_video::GraphicVideoMp4Capability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_graphic_video_mp4".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn blend_video_capability()
-> opencut_job_contracts::scene_blend_video::BlendVideoMp4Capability {
    let result = video_runtime();
    opencut_job_contracts::scene_blend_video::BlendVideoMp4Capability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_blend_video_mp4".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn mask_video_capability()
-> opencut_job_contracts::scene_mask_video::MaskVideoMp4Capability {
    let result = video_runtime();
    opencut_job_contracts::scene_mask_video::MaskVideoMp4Capability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_mask_video_mp4".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn scene_video_capability() -> opencut_job_contracts::scene_video::SceneVideoMp4Capability
{
    let result = video_runtime();
    opencut_job_contracts::scene_video::SceneVideoMp4Capability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_scene_video_mp4".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_modules_cannot_advertise_a_native_renderer() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("opencut-render-worker");
        assert!(worker_at(&path).is_err());
        std::fs::write(&path, b"#!/usr/bin/env node\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(worker_at(&path).is_err());
        assert!(worker_at(Path::new("opencut-render-worker")).is_err());
    }
}
