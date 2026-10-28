//! Exact licensed assets shared with the previous compositor, not copies.
use sha2::{Digest, Sha256};
pub const VERTEX: &str =
    include_str!("../../../packages/classic-visual-renderer/vendor/classic-v31/fullscreen.wgsl");
pub const LAYER: &str =
    include_str!("../../../packages/classic-visual-renderer/vendor/classic-v31/layer.wgsl");
pub const BLEND: &str =
    include_str!("../../../packages/classic-visual-renderer/vendor/classic-v31/blend.wgsl");
pub const MASK: &str =
    include_str!("../../../packages/classic-visual-renderer/vendor/classic-v31/mask.wgsl");
pub const INIT: &str =
    include_str!("../../../packages/classic-visual-renderer/vendor/classic-v31/jfa_init.wgsl");
pub const STEP: &str =
    include_str!("../../../packages/classic-visual-renderer/vendor/classic-v31/jfa_step.wgsl");
pub const DISTANCE: &str =
    include_str!("../../../packages/classic-visual-renderer/vendor/classic-v31/jfa_distance.wgsl");
pub const BLUR: &str =
    include_str!("../../../packages/classic-visual-renderer/vendor/classic-v31/gaussian_blur.wgsl");
// The authored WGSL remains in its existing source asset until the browser
// package is removed at cutover. This embeds data; no JavaScript is executed.
pub fn color() -> &'static str {
    include_str!("../../../packages/classic-visual-renderer/src/legacy-color.mjs")
        .split_once("export const legacyColorShader = `")
        .expect("fixed shader start")
        .1
        .split_once("`;")
        .expect("fixed shader end")
        .0
}

pub fn identity() -> String {
    let mut digest = Sha256::new();
    for (name, source) in [
        ("fullscreen.wgsl", VERTEX),
        ("layer.wgsl", LAYER),
        ("blend.wgsl", BLEND),
        ("mask.wgsl", MASK),
        ("jfa_init.wgsl", INIT),
        ("jfa_step.wgsl", STEP),
        ("jfa_distance.wgsl", DISTANCE),
        ("gaussian_blur.wgsl", BLUR),
        ("legacy-color.wgsl", color()),
    ] {
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((source.len() as u64).to_le_bytes());
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
