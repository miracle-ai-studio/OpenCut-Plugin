//! Observed renderer facts, independent of project formats and product releases.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NativeGraphicsApi {
    Metal,
    Vulkan,
    DirectX,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeLibraryIdentity {
    pub name: String,
    pub version: String,
    pub configuration_sha256: String,
    pub binary_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativePlatformIdentity {
    pub os: String,
    pub architecture: String,
    pub kernel_release: String,
    pub system_build: String,
    pub font_backend: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeVisualIdentity {
    pub renderer_sha256: String,
    pub shader_sha256: String,
    pub decoder_libraries: Vec<NativeLibraryIdentity>,
    pub wgpu_version: String,
    pub rasterizer: String,
    /// Absent only in immutable proofs captured before platform observation.
    /// New execution requires the observed OS/font/driver platform identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<NativePlatformIdentity>,
    pub gpu_backend: NativeGraphicsApi,
    pub gpu_device: String,
    pub gpu_driver: String,
    pub gpu_vendor_id: u32,
    pub gpu_device_id: u32,
    pub texture_format: String,
    pub decoder_profile: String,
}
