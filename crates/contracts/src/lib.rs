pub mod agent_recipe;
pub mod export_video;
pub mod keyframe_edit;
pub mod keyframe_legacy_conversion;
pub mod mask_authoring;
pub mod mask_frame;
pub mod scene_frame;
pub mod source_evidence;
pub mod text_canvas;
use std::{collections::BTreeMap, error::Error, fmt};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod agent_edit;
pub mod agent_read;
pub mod audio_gain;
pub mod blend_frame;
pub mod branching;
pub mod edit_recovery;
pub mod font_catalog;
pub mod font_derived;
pub mod font_import;
pub mod font_inventory;
pub mod font_synthesis;
pub mod graphic_edit;
pub mod graphic_frame;
pub mod historical_visual;
pub mod historical_visual_ops;
pub mod media_import;
pub mod project_edit;
pub mod project_history;
pub mod project_inspection;
pub mod project_public;
pub mod project_resources;
pub mod scene;
pub mod scene_bookmarks;
pub mod scene_ops;
pub mod source_art;
pub mod source_art_waveform_display;
pub mod source_audio;
pub mod text_authoring;
pub mod text_frame;
pub mod timeline;
pub mod timeline_clipboard;
pub mod timeline_edit;
pub mod track_removal;
pub mod visual_frame;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Time {
    #[serde(deserialize_with = "deserialize_ticks")]
    pub ticks: String,
    pub timebase: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    pub start: Time,
    pub duration: Time,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectDocument {
    pub id: String,
    pub name: String,
    pub schema_version: u32,
    pub default_sequence_id: String,
    pub sequences: Vec<Sequence>,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectSnapshot {
    pub project: ProjectDocument,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateProject {
    pub id: String,
    pub name: String,
    pub sequence_id: String,
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub caption_cues: Vec<CaptionCue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Caption,
    Graphic,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub kind: TrackKind,
    pub enabled: bool,
    pub muted: bool,
    pub locked: bool,
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub params: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Video,
    Audio,
    Image,
    Text,
    Subtitle,
    Graphic,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub id: String,
    pub name: String,
    pub kind: AssetKind,
    pub uri: String,
    pub duration: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<AssetVersion>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetVersion {
    pub id: String,
    pub content_hash: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: String,
    pub asset_id: Option<String>,
    pub timeline_range: TimeRange,
    pub source_range: Option<TimeRange>,
    #[serde(default)]
    pub params: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub channels: Vec<KeyframeChannel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionCue {
    pub id: String,
    pub timeline_range: TimeRange,
    pub text: String,
    pub source_anchor: Option<CaptionSourceAnchor>,
    pub style: CaptionStyle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionSourceAnchor {
    pub asset_id: String,
    pub source_range: TimeRange,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionStyle {
    pub font_family: String,
    pub font_size: f64,
    pub color: String,
    pub background_color: Option<String>,
    #[serde(default, skip_serializing_if = "FontResolutionPolicy::is_strict")]
    pub font_policy: FontResolutionPolicy,
    /// Maximum layout width in canvas pixels. None uses the backend's canvas margin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    /// Line height in canvas pixels. None derives it from font_size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f32>,
    #[serde(default, skip_serializing_if = "TextAlign::is_center")]
    pub text_align: TextAlign,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum FontResolutionPolicy {
    Strict {},
    ExplicitFallback { families: Vec<String> },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

impl FontResolutionPolicy {
    fn is_strict(&self) -> bool {
        matches!(self, Self::Strict {})
    }
}

impl Default for FontResolutionPolicy {
    fn default() -> Self {
        Self::Strict {}
    }
}

impl TextAlign {
    fn is_center(&self) -> bool {
        matches!(self, Self::Center)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeInterpolation {
    Hold,
    Linear,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Keyframe {
    pub id: String,
    pub time: Time,
    pub value: serde_json::Value,
    pub interpolation: KeyframeInterpolation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyframeChannel {
    pub parameter: String,
    pub keyframes: Vec<Keyframe>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransactionRequest {
    pub expected_revision: String,
    pub idempotency_key: String,
    pub label: String,
    pub operations: Vec<Operation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Operation {
    #[serde(rename = "sequence.update_settings")]
    UpdateSequenceSettings {
        sequence_id: String,
        name: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
        fps: Option<FrameRate>,
    },
    #[serde(rename = "track.create")]
    AddTrack { sequence_id: String, track: Track },
    #[serde(rename = "track.delete")]
    RemoveTrack {
        sequence_id: String,
        track_id: String,
    },
    #[serde(rename = "asset.bind")]
    BindAsset { asset: Asset },
    #[serde(rename = "asset.replace")]
    ReplaceAsset { asset: Asset },
    #[serde(rename = "asset.unbind")]
    RemoveAsset { asset_id: String },
    #[serde(rename = "clip.insert")]
    InsertClip {
        sequence_id: String,
        track_id: String,
        clip: Clip,
    },
    #[serde(rename = "clip.split")]
    SplitClip {
        sequence_id: String,
        track_id: String,
        clip_id: String,
        at: Time,
        right_clip_id: String,
    },
    #[serde(rename = "clip.move")]
    MoveClip {
        sequence_id: String,
        track_id: String,
        clip_id: String,
        destination_track_id: String,
        start: Time,
    },
    #[serde(rename = "clip.trim")]
    TrimClip {
        sequence_id: String,
        track_id: String,
        clip_id: String,
        timeline_range: TimeRange,
        source_range: Option<TimeRange>,
    },
    #[serde(rename = "clip.delete")]
    DeleteClip {
        sequence_id: String,
        track_id: String,
        clip_id: String,
    },
    #[serde(rename = "parameter.set_clip")]
    UpdateClipParams {
        sequence_id: String,
        track_id: String,
        clip_id: String,
        params: BTreeMap<String, serde_json::Value>,
    },
    #[serde(rename = "parameter.set_track")]
    UpdateTrackParams {
        sequence_id: String,
        track_id: String,
        params: BTreeMap<String, serde_json::Value>,
    },
    #[serde(rename = "track.update")]
    UpdateTrack {
        sequence_id: String,
        track_id: String,
        name: Option<String>,
        enabled: Option<bool>,
        muted: Option<bool>,
        locked: Option<bool>,
    },
    #[serde(rename = "clip.retime")]
    RetimeClip {
        sequence_id: String,
        track_id: String,
        clip_id: String,
        timeline_duration: Time,
    },
    #[serde(rename = "keyframe.channel_set")]
    SetKeyframeChannel {
        sequence_id: String,
        track_id: String,
        clip_id: String,
        channel: KeyframeChannel,
    },
    #[serde(rename = "keyframe.channel_delete")]
    DeleteKeyframeChannel {
        sequence_id: String,
        track_id: String,
        clip_id: String,
        parameter: String,
    },
    #[serde(rename = "caption.insert")]
    InsertCaption {
        sequence_id: String,
        cue: CaptionCue,
    },
    #[serde(rename = "caption.update")]
    UpdateCaption {
        sequence_id: String,
        cue: CaptionCue,
    },
    #[serde(rename = "caption.delete")]
    DeleteCaption { sequence_id: String, cue_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransactionResult {
    pub transaction_id: String,
    pub base_revision: String,
    pub revision: String,
    pub created_ids: Vec<String>,
    pub changed_ids: Vec<String>,
    pub deleted_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditPlan {
    pub base_revision: String,
    pub proposed_project: ProjectDocument,
    pub created_ids: Vec<String>,
    pub changed_ids: Vec<String>,
    pub deleted_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_asset_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditorError {
    pub code: String,
    pub message: String,
    pub details: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParameterDescriptor {
    pub name: String,
    pub version: u32,
    pub target: String,
    pub value_type: String,
    pub unit: String,
    pub default_value: serde_json::Value,
    pub animatable: bool,
    pub interpolation: Vec<String>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationDescriptor {
    pub name: String,
    pub version: u32,
    pub kind: String,
    pub required_capabilities: Vec<String>,
    pub declared_effects: Vec<String>,
    pub deterministic: bool,
    pub idempotency_policy: String,
    pub conflict_policy: String,
}

impl EditorError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: serde_json::Value::Object(Default::default()),
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }
}

impl fmt::Display for EditorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for EditorError {}

fn deserialize_ticks<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    value
        .parse::<i64>()
        .map_err(|_| serde::de::Error::custom("ticks must be a signed 64-bit decimal integer"))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_uses_namespaced_tag_and_rejects_unknown_fields() {
        let value = serde_json::to_value(Operation::DeleteClip {
            sequence_id: "s".into(),
            track_id: "t".into(),
            clip_id: "c".into(),
        })
        .unwrap();
        assert_eq!(value["type"], "clip.delete");
        assert!(serde_json::from_value::<Operation>(serde_json::json!({"type":"clip.delete","sequence_id":"s","track_id":"t","clip_id":"c","surprise":true})).is_err());
    }
    #[test]
    fn time_rejects_non_integer_ticks_during_deserialization() {
        assert!(
            serde_json::from_value::<Time>(serde_json::json!({"ticks":"1.5","timebase":24}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<Time>(serde_json::json!({"ticks":1,"timebase":24})).is_err()
        );
    }

    #[test]
    fn legacy_assets_default_to_unresolved_and_omit_the_version_field() {
        let asset: Asset = serde_json::from_value(serde_json::json!({
            "id":"a", "name":"Legacy", "kind":"image", "uri":"file:///a.png",
            "duration":null
        }))
        .unwrap();
        assert_eq!(asset.version, None);
        assert!(
            serde_json::to_value(&asset)
                .unwrap()
                .get("version")
                .is_none()
        );
    }

    #[test]
    fn asset_replace_has_a_namespaced_tag_and_version_is_strict() {
        let version = AssetVersion {
            id: format!("av_{}", "a".repeat(64)),
            content_hash: "a".repeat(64),
            size: 0,
        };
        let operation = Operation::ReplaceAsset {
            asset: Asset {
                id: "a".into(),
                name: "Replacement".into(),
                kind: AssetKind::Image,
                uri: "asset://a".into(),
                duration: None,
                version: Some(version),
                metadata: BTreeMap::new(),
            },
        };
        assert_eq!(
            serde_json::to_value(operation).unwrap()["type"],
            "asset.replace"
        );
        assert!(
            serde_json::from_value::<AssetVersion>(serde_json::json!({
                "id":"av_x", "content_hash":"x", "size":0, "extra":true
            }))
            .is_err()
        );
    }

    #[test]
    fn legacy_caption_style_defaults_to_explicit_strict_layout() {
        let style: CaptionStyle = serde_json::from_value(serde_json::json!({
            "font_family":"Arial", "font_size":24, "color":"white", "background_color":null,
        }))
        .unwrap();
        assert_eq!(style.font_policy, FontResolutionPolicy::Strict {});
        assert_eq!(style.text_align, TextAlign::Center);
        assert_eq!(style.max_width, None);
        assert_eq!(style.line_height, None);
        // Default additions must not change canonical legacy request hashes.
        assert_eq!(
            serde_json::to_value(&style).unwrap(),
            serde_json::json!({
                "font_family":"Arial", "font_size":24.0, "color":"white", "background_color":null,
            })
        );
        assert!(
            serde_json::from_value::<FontResolutionPolicy>(serde_json::json!({
                "mode":"strict", "families":["Arial"],
            }))
            .is_err()
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetLockManifest {
    pub version: u32,
    pub revision: String,
    pub sequence_id: String,
    pub assets: Vec<LockedAsset>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedAsset {
    pub asset_id: String,
    pub content_hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub content_hash: String,
    /// Compare-and-repair token over exact stored JSON, including malformed JSON.
    pub metadata_hash: Option<String>,
    pub asset: Option<Asset>,
}

pub mod subtitle_ass;
pub mod subtitle_text;
pub mod subtitles;

pub mod caption_frame;

pub mod element_properties;
pub mod element_transform;
pub mod visual_edit_draft;

pub mod scene_authoring;

pub mod keyframe_sample;

pub mod brand_resources;
pub mod catalog_resources;

pub mod subtitle_classic_ass;

pub mod project_readonly;

pub mod project_catalog;
pub mod renderer_identity;
