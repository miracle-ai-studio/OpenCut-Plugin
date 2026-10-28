use super::*;
use opencut_contracts::renderer_identity::{
    NativeGraphicsApi, NativeLibraryIdentity, NativePlatformIdentity, NativeVisualIdentity,
};
use opencut_editor_application::classic_visual::{NATIVE_DECODER_PROFILE, NATIVE_RASTERIZER};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub(crate) fn file_hash(path: &Path) -> Result<String, EditorError> {
    let mut file = File::open(path).map_err(|_| invalid("Cannot read native runtime identity"))?;
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let n = file
            .read(&mut bytes)
            .map_err(|_| invalid("Cannot hash native runtime identity"))?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub(crate) fn observe(
    gpu: &opencut_renderer::Identity,
) -> Result<NativeVisualIdentity, EditorError> {
    let decoder_libraries = opencut_media::native_decode::identity::libraries()
        .map_err(media_error)?
        .into_iter()
        .map(|library| {
            Ok(NativeLibraryIdentity {
                name: library.name.into(),
                version: library.version,
                configuration_sha256: format!(
                    "{:x}",
                    Sha256::digest(library.configuration.as_bytes())
                ),
                binary_sha256: file_hash(&library.path)?,
            })
        })
        .collect::<Result<Vec<_>, EditorError>>()?;
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

#[cfg(unix)]
pub(crate) fn platform() -> Result<NativePlatformIdentity, EditorError> {
    let mut info = std::mem::MaybeUninit::<libc::utsname>::zeroed();
    // SAFETY: uname writes exactly one initialized utsname record.
    if unsafe { libc::uname(info.as_mut_ptr()) } != 0 {
        return Err(invalid("Cannot identify native operating system"));
    }
    let info = unsafe { info.assume_init() };
    fn field(bytes: &[libc::c_char]) -> Result<String, EditorError> {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("Unterminated native platform identity"))?;
        String::from_utf8(bytes[..end].iter().map(|v| *v as u8).collect())
            .map_err(|_| invalid("Invalid native platform identity"))
    }
    #[cfg(target_os = "macos")]
    let system_build = {
        let mut bytes = [0u8; 256];
        let mut size = bytes.len();
        // The macOS build pins the CoreText and Metal system components,
        // including dylibs supplied by the shared system cache.
        let status = unsafe {
            libc::sysctlbyname(
                c"kern.osversion".as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 || size == 0 || size > bytes.len() || bytes[size - 1] != 0 {
            return Err(invalid("Cannot identify native system build"));
        }
        String::from_utf8(bytes[..size - 1].to_vec())
            .map_err(|_| invalid("Invalid native system build"))?
    };
    #[cfg(not(target_os = "macos"))]
    let system_build = field(&info.version)?;
    Ok(NativePlatformIdentity {
        os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        kernel_release: field(&info.release)?,
        system_build,
        font_backend: if cfg!(target_os = "macos") {
            "coretext"
        } else {
            "freetype"
        }
        .into(),
    })
}
#[cfg(not(unix))]
pub(crate) fn platform() -> Result<NativePlatformIdentity, EditorError> {
    Err(invalid("Native platform identity is unavailable"))
}
