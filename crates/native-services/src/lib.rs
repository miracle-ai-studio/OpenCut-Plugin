//! Native capability adapters for portable application use cases.
pub mod classic_mask_frame;
pub mod classic_scene_frame;
pub mod export_video;
mod historical_snapshot_package;
mod historical_visual_package;
pub mod media;
mod package_io;
pub mod package_location;
pub mod plugin_packages;
pub mod plugins;
pub mod presets;
pub mod preview;
pub mod preview_cache;
mod project_archive;
pub mod project_exchange;
mod project_package;
pub mod quality;
pub mod render;
pub mod reproducibility;
pub mod uploads;

pub mod storage;

mod browser_process;
pub mod browser_resources;
pub mod classic_audio;
pub mod classic_audio_preview;
pub mod classic_video;
pub mod classic_visual;
pub mod native_visual;
pub mod native_audio;
pub mod font_catalog;
pub mod font_color_static;
pub mod font_derived;
pub mod font_import;
pub mod font_inspection;
pub mod font_named;
pub mod font_synthesis;
pub mod gc;

pub mod artifacts;
pub mod budget;

mod preview_inputs;
pub mod source_art;

mod classic_blend_frame;
pub mod classic_caption_frame;
pub mod classic_graphic_frame;

pub mod catalog_resources;
