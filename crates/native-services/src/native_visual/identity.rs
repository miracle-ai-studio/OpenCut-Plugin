use super::*;
use opencut_contracts::renderer_identity::{
    NativeGraphicsApi, NativePlatformIdentity, NativeVisualIdentity,
};
use opencut_editor_application::classic_visual::{NATIVE_DECODER_PROFILE, NATIVE_RASTERIZER};

pub(crate) use crate::native_process::file_hash;
pub(super) fn observe_text()
-> Result<opencut_contracts::renderer_identity::NativeTextRuntimeIdentity, EditorError> {
    Ok(
        opencut_contracts::renderer_identity::NativeTextRuntimeIdentity {
            worker_sha256: file_hash(
                &std::env::current_exe()
                    .map_err(|_| invalid("Cannot identify native text worker"))?,
            )?,
            rasterizer: NATIVE_RASTERIZER.into(),
            platform: platform()?,
        },
    )
}
pub(crate) fn observe(
    gpu: &opencut_renderer::Identity,
) -> Result<NativeVisualIdentity, EditorError> {
    let decoder_libraries = crate::native_process::library_identities()?;
    Ok(NativeVisualIdentity {
        renderer_sha256: file_hash(
            &std::env::current_exe().map_err(|_| invalid("Cannot identify native worker"))?,
        )?,
        shader_sha256: gpu.shaders_sha256.clone(),
        decoder_libraries,
        wgpu_version: gpu.wgpu.into(),
        rasterizer: NATIVE_RASTERIZER.into(),
        platform: Some(platform()?),
        gpu_backend: match gpu.backend.as_str() {
            "Metal" => NativeGraphicsApi::Metal,
            "Vulkan" => NativeGraphicsApi::Vulkan,
            "Dx12" => NativeGraphicsApi::DirectX,
            _ => return Err(invalid("Unexpected native graphics API")),
        },
        gpu_device: gpu.adapter.clone(),
        gpu_driver: gpu.driver.clone(),
        gpu_vendor_id: gpu.vendor,
        gpu_device_id: gpu.device,
        texture_format: "bgra8unorm".into(),
        decoder_profile: NATIVE_DECODER_PROFILE.into(),
    })
}

pub(crate) fn platform() -> Result<NativePlatformIdentity, EditorError> {
    let system = crate::native_process::system_identity()?;
    Ok(NativePlatformIdentity {
        os: system.os,
        architecture: system.architecture,
        kernel_release: system.kernel_release,
        system_build: system.system_build,
        font_backend: if cfg!(target_os = "macos") {
            "coretext"
        } else {
            "freetype"
        }
        .into(),
    })
}
