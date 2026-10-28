//! Trusted visual runtime configuration, separate from all public frame inputs.
use crate::browser_runtime::{absolute, dependency, executable, metadata, package_file};
use opencut_contracts::EditorError;
use opencut_job_contracts::media_frame::MediaFramePngCapability;
use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;
use opencut_native_services::classic_visual::ClassicVisualProcessConfig;
use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

fn unavailable() -> EditorError {
    EditorError::new("CAPABILITY_UNAVAILABLE", "VISUAL_RUNTIME_UNAVAILABLE")
}
fn paths() -> Result<(PathBuf, PathBuf, PathBuf), EditorError> {
    let package = match env::var_os("OPENCUT_CLASSIC_VISUAL_PACKAGE") {
        Some(path) => PathBuf::from(path),
        None => env::current_exe()
            .ok()
            .as_deref()
            .and_then(Path::parent)
            .map(|parent| parent.join("libexec/classic-visual-renderer"))
            .filter(|path| path.is_dir())
            .ok_or_else(|| {
                EditorError::new("CAPABILITY_UNAVAILABLE", "VISUAL_RUNTIME_NOT_CONFIGURED")
            })?,
    };
    let package = absolute(&package).map_err(|_| unavailable())?;
    let chromium = match env::var_os("OPENCUT_CLASSIC_VISUAL_CHROMIUM") {
        Some(path) => PathBuf::from(path),
        None => crate::bundled_runtime::executable("chromium")?.ok_or_else(|| {
            EditorError::new("CAPABILITY_UNAVAILABLE", "VISUAL_RUNTIME_NOT_CONFIGURED")
        })?,
    };
    let chromium = executable(Path::new(&chromium)).map_err(|_| unavailable())?;
    let node = match env::var_os("OPENCUT_CLASSIC_VISUAL_NODE") {
        Some(path) => executable(Path::new(&path)).map_err(|_| unavailable())?,
        None => match crate::bundled_runtime::executable("node")? {
            Some(path) => executable(&path).map_err(|_| unavailable())?,
            None => env::var_os("PATH")
                .as_deref()
                .into_iter()
                .flat_map(env::split_paths)
                .filter(|path| path.is_absolute())
                .find_map(|path| executable(&path.join("node")).ok())
                .ok_or_else(unavailable)?,
        },
    };
    let manifest = metadata(&package.join("package.json")).map_err(|_| unavailable())?;
    if manifest["name"] != "@opencut/classic-visual-renderer"
        || manifest["version"] != "0.1.0"
        || manifest["type"] != "module"
        || manifest["dependencies"]["playwright-core"] != "1.63.0"
        || manifest["dependencies"]["mediabunny"] != "1.41.0"
        || manifest["dependencies"]["@opencut/classic-audio-renderer"]
            != "file:../classic-audio-renderer"
    {
        return Err(unavailable());
    }
    dependency(&package, "playwright-core", "1.63.0", "index.mjs").map_err(|_| unavailable())?;
    dependency(
        &package,
        "mediabunny",
        "1.41.0",
        "dist/bundles/mediabunny.mjs",
    )
    .map_err(|_| unavailable())?;
    dependency(
        &package,
        "@opencut/classic-audio-renderer",
        "0.1.0",
        "host/shared.mjs",
    )
    .map_err(|_| unavailable())?;
    let shared = absolute(&package.join("node_modules/@opencut/classic-audio-renderer"))
        .map_err(|_| unavailable())?;
    let shared_manifest = metadata(&shared.join("package.json")).map_err(|_| unavailable())?;
    if shared_manifest["exports"]["./internal-host-runtime"] != "./host/shared.mjs" {
        return Err(unavailable());
    }
    for file in [
        "host/runner.mjs",
        "host/browser.mjs",
        "host/modules.mjs",
        "host/limits.mjs",
        "host/protocol.mjs",
        "host/page.mjs",
        "src/validation.mjs",
    ] {
        package_file(&shared, file).map_err(|_| unavailable())?;
    }
    for file in [
        "host/browser.mjs",
        "host/modules.mjs",
        "host/page.mjs",
        "host/protocol.mjs",
        "src/index.mjs",
        "src/decoder.mjs",
        "src/image-format.mjs",
        "src/gpu.mjs",
        "src/legacy-color.mjs",
        "src/validation.mjs",
        "vendor/classic-v31/layer.wgsl",
        "vendor/classic-v31/blend.wgsl",
        "vendor/classic-v31/fullscreen.wgsl",
    ] {
        package_file(&package, file).map_err(|_| unavailable())?;
    }
    let runner = package_file(&package, "host/runner.mjs").map_err(|_| unavailable())?;
    Ok((node, chromium, runner))
}
pub(crate) fn configured(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    opencut_editor_application::media_frame_jobs::validate_policy(policy)?;
    let (node, chromium, runner) = paths()?;
    Ok(ClassicVisualProcessConfig {
        node,
        chromium,
        runner,
        deadline: Duration::from_millis(policy.deadline_ms),
        policy: policy.clone(),
        strict_cgroup: None,
    })
}
pub(crate) fn capability() -> MediaFramePngCapability {
    let result = paths();
    MediaFramePngCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_frame_png".into(),
        unavailable_reason: result.err().map(|error| error.message),
    }
}

/// The V2 capability requires its additional source modules. V1 configuration
/// remains valid for older installed runtimes that lack text rendering.
pub(crate) fn text_capability() -> opencut_job_contracts::scene_text_frame::TextFramePngCapability {
    let result = paths().and_then(|(_, _, runner)| check_text_modules(&runner));
    opencut_job_contracts::scene_text_frame::TextFramePngCapability {
        schema_version: 2,
        available: result.is_ok(),
        submit_tool: "opencut_job_text_frame_png".into(),
        unavailable_reason: result.err().map(|error| error.message),
    }
}

fn check_text_modules(runner: &Path) -> Result<(), EditorError> {
    let package = runner
        .parent()
        .and_then(Path::parent)
        .ok_or_else(unavailable)?;
    package_file(package, "src/text-frame.mjs").map_err(|_| unavailable())?;
    package_file(package, "src/text-layout.mjs").map_err(|_| unavailable())?;
    Ok(())
}
pub(crate) fn configured_text(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    let config = configured(policy)?;
    check_text_modules(&config.runner)?;
    Ok(config)
}

/// Configurable export can omit audio. Its visual preflight still requires the
/// persistent video entrypoints, rather than advertising from PNG support alone.
pub(crate) fn check_export_video_runtime() -> Result<(), EditorError> {
    let (_, _, runner) = paths()?;
    check_export_video_modules(&runner)
}
fn check_export_video_modules(runner: &Path) -> Result<(), EditorError> {
    check_scene_modules(runner)?;
    let package = runner
        .parent()
        .and_then(Path::parent)
        .ok_or_else(unavailable)?;
    for file in ["host/video-runner.mjs", "host/video-browser.mjs"] {
        package_file(package, file).map_err(|_| unavailable())?;
    }
    Ok(())
}

/// Whole-scene video requires both renderer runtimes and the local encoding tools.
pub(crate) fn video_capability() -> opencut_job_contracts::media_video::MediaVideoMp4Capability {
    let result = (|| -> Result<(), EditorError> {
        let (_, _, runner) = paths()?;
        check_text_modules(&runner)?;
        let package = runner
            .parent()
            .and_then(Path::parent)
            .ok_or_else(unavailable)?;
        for file in ["host/video-runner.mjs", "host/video-browser.mjs"] {
            package_file(package, file).map_err(|_| unavailable())?;
        }
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
    })();
    opencut_job_contracts::media_video::MediaVideoMp4Capability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_video_mp4".into(),
        unavailable_reason: result.err().map(|error| error.message),
    }
}

pub(crate) fn caption_video_capability()
-> opencut_job_contracts::scene_caption_video::CaptionVideoMp4Capability {
    let video = video_capability();
    let caption_error = paths()
        .and_then(|(_, _, runner)| check_caption_modules(&runner))
        .err();
    let unavailable_reason = video
        .unavailable_reason
        .or_else(|| caption_error.map(|error| error.message));
    opencut_job_contracts::scene_caption_video::CaptionVideoMp4Capability {
        schema_version: 2,
        available: video.available && unavailable_reason.is_none(),
        submit_tool: "opencut_job_caption_video_mp4".into(),
        unavailable_reason,
    }
}

pub(crate) fn graphic_video_capability()
-> opencut_job_contracts::scene_graphic_video::GraphicVideoMp4Capability {
    let video = video_capability();
    let graphic_error = paths()
        .and_then(|(_, _, runner)| check_graphic_modules(&runner))
        .err();
    let unavailable_reason = video
        .unavailable_reason
        .or_else(|| graphic_error.map(|error| error.message));
    opencut_job_contracts::scene_graphic_video::GraphicVideoMp4Capability {
        schema_version: 1,
        available: video.available && unavailable_reason.is_none(),
        submit_tool: "opencut_job_graphic_video_mp4".into(),
        unavailable_reason,
    }
}
pub(crate) fn scene_video_capability() -> opencut_job_contracts::scene_video::SceneVideoMp4Capability
{
    let video = video_capability();
    let scene_error = paths()
        .and_then(|(_, _, runner)| check_scene_modules(&runner))
        .err();
    let unavailable_reason = video
        .unavailable_reason
        .or_else(|| scene_error.map(|error| error.message));
    opencut_job_contracts::scene_video::SceneVideoMp4Capability {
        schema_version: 1,
        available: video.available && unavailable_reason.is_none(),
        submit_tool: "opencut_job_scene_video_mp4".into(),
        unavailable_reason,
    }
}
pub(crate) fn mask_video_capability()
-> opencut_job_contracts::scene_mask_video::MaskVideoMp4Capability {
    let video = video_capability();
    let mask_error = paths()
        .and_then(|(_, _, runner)| check_mask_modules(&runner))
        .err();
    let unavailable_reason = video
        .unavailable_reason
        .or_else(|| mask_error.map(|error| error.message));
    opencut_job_contracts::scene_mask_video::MaskVideoMp4Capability {
        schema_version: 1,
        available: video.available && unavailable_reason.is_none(),
        submit_tool: "opencut_job_mask_video_mp4".into(),
        unavailable_reason,
    }
}
pub(crate) fn blend_video_capability()
-> opencut_job_contracts::scene_blend_video::BlendVideoMp4Capability {
    let video = video_capability();
    let blend_error = paths()
        .and_then(|(_, _, runner)| check_blend_modules(&runner))
        .err();
    let unavailable_reason = video
        .unavailable_reason
        .or_else(|| blend_error.map(|error| error.message));
    opencut_job_contracts::scene_blend_video::BlendVideoMp4Capability {
        schema_version: 1,
        available: video.available && unavailable_reason.is_none(),
        submit_tool: "opencut_job_blend_video_mp4".into(),
        unavailable_reason,
    }
}

fn check_caption_modules(runner: &Path) -> Result<(), EditorError> {
    check_text_modules(runner)?;
    let package = runner
        .parent()
        .and_then(Path::parent)
        .ok_or_else(unavailable)?;
    for name in [
        "src/caption-frame.mjs",
        "src/caption-plan.mjs",
        "src/caption-hooks.mjs",
    ] {
        package_file(package, name).map_err(|_| unavailable())?;
    }
    Ok(())
}
pub(crate) fn configured_caption(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    let config = configured(policy)?;
    check_caption_modules(&config.runner)?;
    Ok(config)
}
pub(crate) fn caption_capability()
-> opencut_job_contracts::scene_caption_frame::CaptionFrameCapability {
    let result = paths().and_then(|(_, _, runner)| check_caption_modules(&runner));
    opencut_job_contracts::scene_caption_frame::CaptionFrameCapability {
        schema_version: 3,
        available: result.is_ok(),
        submit_tool: "opencut_job_caption_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}

fn check_graphic_modules(runner: &Path) -> Result<(), EditorError> {
    check_caption_modules(runner)?;
    let package = runner
        .parent()
        .and_then(Path::parent)
        .ok_or_else(unavailable)?;
    for name in [
        "src/graphic-frame.mjs",
        "src/graphic-source.mjs",
        "src/graphic-plan.mjs",
        "src/caption-hooks.mjs",
    ] {
        package_file(package, name).map_err(|_| unavailable())?;
    }
    Ok(())
}
pub(crate) fn configured_graphic(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    let config = configured(policy)?;
    check_graphic_modules(&config.runner)?;
    Ok(config)
}
pub(crate) fn configured_scene(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    let config = configured(policy)?;
    check_scene_modules(&config.runner)?;
    Ok(config)
}
pub(crate) fn configured_mask(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    let config = configured(policy)?;
    check_mask_modules(&config.runner)?;
    Ok(config)
}
pub(crate) fn configured_blend(
    policy: &LocalVisualExecutionPolicy,
) -> Result<ClassicVisualProcessConfig, EditorError> {
    let config = configured(policy)?;
    check_blend_modules(&config.runner)?;
    Ok(config)
}
pub(crate) fn graphic_capability()
-> opencut_job_contracts::scene_graphic_frame::GraphicFrameCapability {
    let result = paths().and_then(|(_, _, runner)| check_graphic_modules(&runner));
    opencut_job_contracts::scene_graphic_frame::GraphicFrameCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_graphic_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn scene_capability() -> opencut_job_contracts::scene_frame::SceneFrameCapability {
    let result = paths().and_then(|(_, _, runner)| check_scene_modules(&runner));
    opencut_job_contracts::scene_frame::SceneFrameCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_scene_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn mask_capability() -> opencut_job_contracts::scene_mask_frame::MaskFrameCapability {
    let result = paths().and_then(|(_, _, runner)| check_mask_modules(&runner));
    opencut_job_contracts::scene_mask_frame::MaskFrameCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_mask_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}
pub(crate) fn blend_capability() -> opencut_job_contracts::scene_blend_frame::BlendFrameCapability {
    let result = paths().and_then(|(_, _, runner)| check_blend_modules(&runner));
    opencut_job_contracts::scene_blend_frame::BlendFrameCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_blend_frame_png".into(),
        unavailable_reason: result.err().map(|e| e.message),
    }
}

fn check_scene_modules(runner: &Path) -> Result<(), EditorError> {
    check_mask_modules(runner)?;
    let package = runner
        .parent()
        .and_then(Path::parent)
        .ok_or_else(unavailable)?;
    for name in [
        "src/scene-frame.mjs",
        "src/scene-plan.mjs",
        "src/scene-source.mjs",
        "src/scene-effects.mjs",
        "src/scene-text.mjs",
        "vendor/classic-v31/gaussian_blur.wgsl",
    ] {
        package_file(package, name).map_err(|_| unavailable())?;
    }
    Ok(())
}
fn check_mask_modules(runner: &Path) -> Result<(), EditorError> {
    check_blend_modules(runner)?;
    let package = runner
        .parent()
        .and_then(Path::parent)
        .ok_or_else(unavailable)?;
    for name in [
        "src/mask-frame.mjs",
        "src/mask-plan.mjs",
        "src/mask-source.mjs",
        "vendor/classic-v31/mask.wgsl",
        "vendor/classic-v31/jfa_init.wgsl",
        "vendor/classic-v31/jfa_step.wgsl",
        "vendor/classic-v31/jfa_distance.wgsl",
    ] {
        package_file(package, name).map_err(|_| unavailable())?;
    }
    Ok(())
}
fn check_blend_modules(runner: &Path) -> Result<(), EditorError> {
    check_graphic_modules(runner)?;
    let package = runner
        .parent()
        .and_then(Path::parent)
        .ok_or_else(unavailable)?;
    for name in [
        "src/blend-frame.mjs",
        "src/blend-plan.mjs",
        "src/blend-modes.mjs",
    ] {
        package_file(package, name).map_err(|_| unavailable())?;
    }
    Ok(())
}

#[cfg(test)]
mod export_runtime_tests {
    use super::*;
    #[test]
    fn scene_png_modules_alone_cannot_advertise_persistent_export() {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        for name in [
            "src/text-frame.mjs",
            "src/text-layout.mjs",
            "src/caption-frame.mjs",
            "src/caption-plan.mjs",
            "src/caption-hooks.mjs",
            "src/graphic-frame.mjs",
            "src/graphic-source.mjs",
            "src/graphic-plan.mjs",
            "src/blend-frame.mjs",
            "src/blend-plan.mjs",
            "src/blend-modes.mjs",
            "src/mask-frame.mjs",
            "src/mask-plan.mjs",
            "src/mask-source.mjs",
            "vendor/classic-v31/mask.wgsl",
            "vendor/classic-v31/jfa_init.wgsl",
            "vendor/classic-v31/jfa_step.wgsl",
            "vendor/classic-v31/jfa_distance.wgsl",
            "src/scene-frame.mjs",
            "src/scene-plan.mjs",
            "src/scene-source.mjs",
            "src/scene-effects.mjs",
            "src/scene-text.mjs",
            "vendor/classic-v31/gaussian_blur.wgsl",
            "host/runner.mjs",
        ] {
            let file = root.join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "fixture").unwrap();
        }
        let runner = root.join("host/runner.mjs");
        check_scene_modules(&runner).unwrap();
        assert!(check_export_video_modules(&runner).is_err());
        std::fs::write(root.join("host/video-runner.mjs"), "fixture").unwrap();
        assert!(check_export_video_modules(&runner).is_err());
        std::fs::write(root.join("host/video-browser.mjs"), "fixture").unwrap();
        check_export_video_modules(&runner).unwrap();
        std::fs::remove_file(root.join("src/scene-text.mjs")).unwrap();
        assert!(check_export_video_modules(&runner).is_err());
    }
}
