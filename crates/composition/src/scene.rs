//! Independent fixed-revision, per-frame V4 execution graph. No V3 projection,
//! files, decoders or renderer capability claims live in this compiler.
use crate::{CompositionError, CompositionScope};
use opencut_contracts::{
    AssetKind, AssetVersion, EditorError, FrameRate, Time, TimeRange, scene::*,
};
use opencut_editor_core::scene::{
    SampledChannel, apply_sample, element_local_time_scene, gain_amplitude,
    sample_animation_channel_scene, validate_project_scene,
};
use opencut_time::RationalTime;
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

mod animation;
mod audio;
pub use audio::*;
mod capabilities;
mod compiler;
mod preview_raster;
pub use preview_raster::{preview_canvas, project_preview_raster, raster_canvas, project_raster};
mod coverage;
mod source;
#[cfg(test)]
mod tests;
mod text_execution;
mod text_frame_execution;
mod visual_execution;
pub use capabilities::*;
pub use compiler::{compile_frame_scene, resolve_scene_format};
pub use coverage::*;
pub use source::*;
pub use text_execution::*;
pub use text_frame_execution::*;
pub use visual_execution::*;

#[derive(Clone, Debug, PartialEq)]
pub struct SceneComposition {
    pub revision: String,
    pub project_id: String,
    pub sequence_id: String,
    pub scope: CompositionScope,
    /// Exact caller-selected presentation time, not converted through seconds.
    pub time: Time,
    pub format: CompiledFormat,
    pub tracks: Vec<CompiledTrack>,
    /// Bottom-to-top execution order; effect layers act on the accumulated image.
    pub layers: Vec<CompiledLayer>,
    pub audio: Vec<CompiledAudio>,
    pub transitions: Vec<CompiledTransition>,
    pub background: CompiledBackground,
    pub resources: Vec<CasResource>,
    pub fonts: Vec<FontResource>,
    pub coverage: Vec<SourceCoverage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledFormat {
    pub fps: FrameRate,
    pub canvas_size: CanvasSize,
    pub profile: FormatProfile,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatProfile {
    ClassicGlobal,
    LegacyPerScene,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CasResource {
    pub asset_id: String,
    pub version: AssetVersion,
    pub kind: AssetKind,
    pub duration: Option<Time>,
    pub metadata: ProjectMediaMetadata,
    pub license: Option<ResourceLicense>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledTrack {
    pub id: String,
    pub order: usize,
    pub kind: SceneTrackKind,
    pub opacity: f64,
    /// Visual-only scope intentionally has no evaluated audio gain.
    pub gain: Option<CompiledGain>,
    pub muted: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompiledGain {
    /// Physical projection in the authored unit, followed by amplitude conversion.
    pub physical: Gain,
    pub amplitude: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledLayer {
    pub element_id: String,
    pub track_id: String,
    pub track_order: usize,
    pub element_order: usize,
    pub local_time: Time,
    pub visual: Option<SampledVisual>,
    pub content: SampledContent,
}
#[derive(Clone, Debug, PartialEq)]
pub struct SampledVisual {
    pub profile: VisualProfile,
    pub transform: Transform,
    pub element_opacity: f64,
    pub track_opacity: f64,
    /// Legacy shapes deliberately bypass track opacity, as old V3 rendering did.
    pub effective_opacity: f64,
    pub opacity_policy: LayerOpacityPolicy,
    pub blend_mode: BlendMode,
    pub effects: Vec<SceneEffect>,
    pub masks: Vec<Mask>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerOpacityPolicy {
    MultiplyTrack,
    LegacyShapeElementOnly,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SampledContent {
    Video {
        asset_id: String,
        stream: Option<u32>,
        source: SourceClock,
        source_audio: SourceAudioDecision,
    },
    Image {
        asset_id: String,
    },
    Sticker {
        asset_id: String,
        intrinsic_size: CanvasSize,
    },
    Text {
        text: Text,
    },
    Graphic {
        graphic: Graphic,
    },
    Caption {
        text: String,
        style: SceneCaptionStyle,
    },
    EffectLayer {
        effect: SceneEffect,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub enum SourceAudioDecision {
    ScopeExcluded,
    Disabled,
    Muted,
    KnownAbsent,
    RequiresProbe,
    Emitted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledAudio {
    pub element_id: String,
    pub track_id: String,
    pub asset_id: String,
    pub stream: Option<u32>,
    pub source: SourceClock,
    pub presence: AudioPresence,
    pub maintain_pitch: bool,
    pub element_gain: CompiledGain,
    pub track_gain: CompiledGain,
    pub amplitude: f64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledTransition {
    pub id: String,
    pub outgoing_element_id: String,
    pub incoming_element_id: String,
    pub profile: TransitionKind,
    pub range: TimeRange,
    pub incoming_weight: f64,
    /// Legacy V3 leaves the outgoing layer intact; alpha-over of the incoming
    /// faded layer supplies the outgoing attenuation. Do not fade it twice.
    pub outgoing_weight: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CompiledBackground {
    Color {
        color: Color,
    },
    MainTrackBlur {
        intensity: f64,
        main_track_id: String,
        /// Raw media source nodes, not the finished foreground composition.
        source_layers: Vec<String>,
    },
}

fn error(value: EditorError) -> CompositionError {
    CompositionError {
        code: "INVALID_PROJECT",
        message: value.message,
    }
}
fn invalid(message: impl Into<String>) -> CompositionError {
    CompositionError {
        code: "INVALID_PROJECT",
        message: message.into(),
    }
}
fn unsupported(message: impl Into<String>) -> CompositionError {
    CompositionError {
        code: "UNSUPPORTED_V4",
        message: message.into(),
    }
}
/// Frozen raster profiles have no explicit actual/requested font descriptor.
/// Reject the marker before partitions can discard unused or blank layers.
fn reject_legacy_synthesis_fonts(fonts: &[FontResource]) -> Result<(), CompositionError> {
    if fonts
        .iter()
        .any(opencut_editor_core::scene::is_font_synthesis_resource)
    {
        return Err(unsupported(
            "Explicit font synthesis requires the versioned Scene or Canvas profile",
        ));
    }
    Ok(())
}
fn time(value: &Time) -> Result<RationalTime, CompositionError> {
    RationalTime::try_from(value).map_err(error)
}
fn property(parameter: ScalarParameter, raw: f64) -> Result<f64, CompositionError> {
    apply_sample(&ScalarBinding::Property { parameter }, raw).map_err(error)
}
fn finite(value: f64) -> Result<f64, CompositionError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(invalid("Compiled numeric value is not finite"))
    }
}
fn end(range: &TimeRange) -> Result<RationalTime, CompositionError> {
    time(&opencut_time::range_end(range).map_err(error)?)
}
fn contains(range: &TimeRange, at: RationalTime) -> Result<bool, CompositionError> {
    Ok(time(&range.start)?.cmp_exact(at) != Ordering::Greater
        && end(range)?.cmp_exact(at) == Ordering::Greater)
}

mod caption_frame_execution;
pub use caption_frame_execution::*;

mod graphic_frame_execution;
pub use graphic_frame_execution::*;
mod blend_frame_execution;
pub use blend_frame_execution::*;
mod mask_frame_execution;
pub use mask_frame_execution::*;

mod scene_frame_execution;
pub use scene_frame_execution::*;
