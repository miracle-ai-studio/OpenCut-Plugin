//! Native execution adapter. Callers own authorization, source sampling and
//! publication; this crate only executes already prepared rendering facts.
mod effects;
mod gpu;
pub mod raster;
mod shaders;

pub use gpu::Renderer;
pub use opencut_contracts::scene::SceneEffect;
pub use opencut_contracts::{scene::BlendMode, visual_frame::VisualQuad};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Metal,
    Vulkan,
    DirectX,
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_frame_pixels: u64,
    pub max_gpu_bytes: u64,
    pub max_cpu_bytes: u64,
    pub max_layers: usize,
}

/// Straight sRGB bytes with top-left origin. Source decode/color conversion
/// must establish this representation before invoking the compositor.
#[derive(Clone, Copy)]
pub struct Raster<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

pub struct Layer<'a> {
    pub source: Raster<'a>,
    pub quad: VisualQuad,
    pub blend: BlendMode,
    pub masks: &'a [Mask<'a>],
    pub effects: &'a [SceneEffect],
    /// Rounded logical element bounds, before stroke padding. Text uses the
    /// canvas bounds. These are supplied by the prepared composition.
    pub effect_size: [u32; 2],
    pub color_matrix: ColorMatrix,
    /// Decoded orientation, applied before placement. A native decoder may
    /// supply already oriented pixels with a value of zero.
    pub rotation_degrees: u16,
}

#[derive(Clone, Copy, Debug)]
pub enum ColorMatrix {
    Bt601,
    Bt709,
    Unspecified,
}

pub struct Mask<'a> {
    /// The full-canvas mask raster. RGB and alpha remain distinct: feathering
    /// seeds use red, while a hard mask reads alpha.
    pub source: Raster<'a>,
    pub feather: f32,
    pub inverted: bool,
}

pub enum Step<'a> {
    Layer(usize),
    Effects(&'a [SceneEffect]),
}

pub enum Background<'a> {
    Color([f32; 4]),
    Layer(Layer<'a>),
}

pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub clear: [f32; 4],
    pub background: Background<'a>,
    pub layers: &'a [Layer<'a>],
    /// Exact prepared stacking order, including adjustment layers. Each
    /// source layer must occur once; an omitted layer is never inferred.
    pub steps: &'a [Step<'a>],
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub backend: String,
    pub adapter: String,
    pub driver: String,
    pub device: u32,
    pub vendor: u32,
    pub wgpu: &'static str,
    pub shaders_sha256: String,
}

#[derive(Clone, Debug)]
pub struct Receipt {
    pub rgba_bytes: u64,
    pub accounted_gpu_bytes: u64,
    pub accounted_cpu_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct Error {
    pub code: String,
    pub message: String,
}
impl Error {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Checked before submissions, while waiting for GPU completion and before
/// every output chunk. A failed checkpoint must not publish a partial frame.
pub trait Execution {
    fn checkpoint(&mut self) -> Result<()>;
    fn deadline(&self) -> Instant;
    fn rgba(&mut self, offset: u64, bytes: &[u8]) -> Result<()>;
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("INVALID_VISUAL_PLAN", message)
}
fn budget(message: impl Into<String>) -> Error {
    Error::new("VISUAL_BUDGET_EXCEEDED", message)
}
