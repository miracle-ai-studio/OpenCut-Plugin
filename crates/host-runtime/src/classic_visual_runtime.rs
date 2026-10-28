//! Trusted native renderer assembly, separate from public project inputs.
use opencut_contracts::EditorError;
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;
use opencut_native_services::classic_visual::ClassicVisualProcessConfig;
use std::{
    env,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
fn unavailable() -> EditorError {
    EditorError::new("CAPABILITY_UNAVAILABLE", "NATIVE_RENDERER_UNAVAILABLE")
}
fn worker_at(path: &Path) -> Result<PathBuf, EditorError> {
    if !path.is_absolute() {
        return Err(unavailable());
    }
    let path = opencut_native_services::classic_video::resolve_tool(path.into())
        .map_err(|_| unavailable())?;
    let mut header = [0u8; 4];
    std::fs::File::open(&path)
        .and_then(|mut f| f.read_exact(&mut header))
        .map_err(|_| unavailable())?;
    if !matches!(
        header,
        [0x7f, b'E', b'L', b'F']
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
    ) {
        return Err(unavailable());
    }
    Ok(path)
}
fn worker() -> Result<PathBuf, EditorError> {
    opencut_native_services::native_visual::platform_backend()?;
    let path = match env::var_os("OPENCUT_RENDER_WORKER") {
        Some(path) => PathBuf::from(path),
        None => env::current_exe()
            .ok()
            .as_deref()
            .and_then(Path::parent)
            .map(|p| p.join("opencut-render-worker"))
            .ok_or_else(unavailable)?,
    };
    worker_at(&path)
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
    for (variable, default) in [("OPENCUT_FFMPEG", "ffmpeg"), ("OPENCUT_FFPROBE", "ffprobe")] {
        opencut_native_services::classic_video::resolve_tool(
            env::var_os(variable)
                .map(Into::into)
                .unwrap_or_else(|| default.into()),
        )?;
    }
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
