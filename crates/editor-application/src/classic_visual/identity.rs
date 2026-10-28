use super::*;
use opencut_job_contracts::media_frame::{MediaFrameExecutionSummary, VisualRuntimeIdentity};

pub const NATIVE_DECODER_PROFILE: &str = "libav-pts-half-open;verified-avio";
pub const NATIVE_RASTERIZER: &str = "skia-safe=0.153.3;tiny-skia=0.11.4;ttf-parser=0.25.1";

pub fn validate_text_identity(
    identity: &opencut_contracts::renderer_identity::NativeTextRuntimeIdentity,
) -> Result<(), EditorError> {
    use crate::media_frame_jobs::{valid_hash, valid_id};
    let platform = &identity.platform;
    if !valid_hash(&identity.worker_sha256)
        || identity.rasterizer != NATIVE_RASTERIZER
        || [
            &platform.os,
            &platform.architecture,
            &platform.kernel_release,
            &platform.system_build,
            &platform.font_backend,
        ]
        .into_iter()
        .any(|v| !valid_id(v))
    {
        return Err(EditorError::new(
            "VISUAL_ENGINE_CHANGED",
            "Native text measurement identity is incomplete",
        ));
    }
    Ok(())
}

pub fn validate_native_identity(identity: &ClassicVisualEngineIdentity) -> Result<(), EditorError> {
    if !valid_native_proof(identity)
        || identity.platform.is_none()
        || identity.decoder_profile != NATIVE_DECODER_PROFILE
        || identity.wgpu_version != "30.0.1"
        || identity.rasterizer != NATIVE_RASTERIZER
    {
        return Err(EditorError::new(
            "VISUAL_ENGINE_CHANGED",
            "Native renderer identity is incomplete",
        ));
    }
    Ok(())
}

fn valid_native_proof(identity: &ClassicVisualEngineIdentity) -> bool {
    use crate::media_frame_jobs::{valid_hash, valid_id};
    let names: Vec<_> = identity
        .decoder_libraries
        .iter()
        .map(|library| library.name.as_str())
        .collect();
    let invalid = !valid_hash(&identity.renderer_sha256)
        || !valid_hash(&identity.shader_sha256)
        || !valid_id(&identity.gpu_device)
        || identity.gpu_driver.len() > 4096
        || identity.texture_format != "bgra8unorm"
        || !valid_id(&identity.decoder_profile)
        || !valid_id(&identity.wgpu_version)
        || !valid_id(&identity.rasterizer)
        || identity.platform.as_ref().is_some_and(|platform| {
            [
                &platform.os,
                &platform.architecture,
                &platform.kernel_release,
                &platform.system_build,
                &platform.font_backend,
            ]
            .into_iter()
            .any(|value| !valid_id(value))
        })
        || names != ["avcodec", "avformat", "avutil", "swresample", "swscale"]
        || identity.decoder_libraries.iter().any(|library| {
            !valid_id(&library.version)
                || !valid_hash(&library.configuration_sha256)
                || !valid_hash(&library.binary_sha256)
        });
    !invalid
}

/// Validate immutable stored evidence, including the exact historical shape.
/// This reader does not create or select an execution implementation.
pub fn validate_visual_summary(summary: &MediaFrameExecutionSummary) -> Result<(), EditorError> {
    use crate::media_frame_jobs::{valid_hash, valid_id};
    let valid = valid_hash(&summary.host_sha256)
        && match &summary.runtime {
            // Stored dependency facts must not be rewritten or rejected merely
            // because this host now contains a different renderer build.
            VisualRuntimeIdentity::Native(identity) => valid_native_proof(identity),
            VisualRuntimeIdentity::HistoricalBrowser(identity) => {
                identity.playwright_version == "1.63.0"
                    && identity.mediabunny_version == "1.41.0"
                    && identity.gpu_backend == "webgpu"
                    && identity.texture_format == "bgra8unorm"
                    && identity.decoder_profile
                        == crate::media_frame_jobs::CLASSIC_VISUAL_DECODER_PROFILE
                    && valid_id(&identity.browser_version)
                    && valid_id(&identity.gpu_device)
                    && [
                        &identity.browser_sha256,
                        &identity.module_sha256,
                        &identity.shader_sha256,
                    ]
                    .into_iter()
                    .all(|h| valid_hash(h))
            }
        };
    if valid {
        Ok(())
    } else {
        Err(EditorError::new(
            "VISUAL_ENGINE_CHANGED",
            "Stored renderer identity is incomplete",
        ))
    }
}

/// Historical visual/audio proofs shared a browser. Native execution records
/// its own decoder and GPU identity; only the publishing host is common.
pub fn validate_visual_audio_identity(
    visual: &MediaFrameExecutionSummary,
    audio: &crate::scene_audio_jobs::AudioExecutionSummary,
) -> bool {
    visual.host_sha256 == audio.host_sha256
        && match &visual.runtime {
            VisualRuntimeIdentity::HistoricalBrowser(identity) => {
                identity.browser_sha256 == audio.browser_sha256
                    && identity.browser_version == audio.browser_version
            }
            VisualRuntimeIdentity::Native(_) => true,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencut_contracts::renderer_identity::{
        NativeGraphicsApi, NativeLibraryIdentity, NativePlatformIdentity,
    };

    #[test]
    fn renderer_upgrade_preserves_stored_proofs_without_admitting_old_live_engines() {
        let mut identity = ClassicVisualEngineIdentity {
            renderer_sha256: "a".repeat(64),
            shader_sha256: "b".repeat(64),
            decoder_libraries: ["avcodec", "avformat", "avutil", "swresample", "swscale"]
                .into_iter()
                .map(|name| NativeLibraryIdentity {
                    name: name.into(),
                    version: "captured library".into(),
                    configuration_sha256: "c".repeat(64),
                    binary_sha256: "d".repeat(64),
                })
                .collect(),
            wgpu_version: "captured dependency".into(),
            rasterizer: "captured rasterizer".into(),
            platform: None,
            gpu_backend: NativeGraphicsApi::Metal,
            gpu_device: "captured GPU".into(),
            gpu_driver: "captured driver".into(),
            gpu_vendor_id: 0,
            gpu_device_id: 0,
            texture_format: "bgra8unorm".into(),
            decoder_profile: NATIVE_DECODER_PROFILE.into(),
        };
        let stored = MediaFrameExecutionSummary {
            host_sha256: "e".repeat(64),
            runtime: VisualRuntimeIdentity::Native(identity.clone()),
        };
        let bytes = serde_json::to_vec(&stored).unwrap();
        validate_visual_summary(&stored).unwrap();
        assert_eq!(serde_json::to_vec(&stored).unwrap(), bytes);
        assert!(validate_native_identity(&identity).is_err());
        identity.wgpu_version = "30.0.1".into();
        identity.rasterizer = NATIVE_RASTERIZER.into();
        assert!(validate_native_identity(&identity).is_err());
        identity.platform = Some(NativePlatformIdentity {
            os: "macos".into(),
            architecture: "x86_64".into(),
            kernel_release: "captured kernel".into(),
            system_build: "captured system".into(),
            font_backend: "coretext".into(),
        });
        validate_native_identity(&identity).unwrap();
        identity.platform.as_mut().unwrap().system_build.clear();
        assert!(validate_native_identity(&identity).is_err());
        assert!(!valid_native_proof(&identity));
    }
}
