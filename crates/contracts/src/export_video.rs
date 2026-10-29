//! Additive Classic export choices. Existing fixed MP4 contracts remain frozen.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Mp4,
    Webm,
}
impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
        }
    }
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Mp4 => "video/mp4",
            Self::Webm => "video/webm",
        }
    }
    pub fn video_codec(self) -> &'static str {
        match self {
            Self::Mp4 => "h264",
            Self::Webm => "vp9",
        }
    }
    pub fn audio_codec(self) -> &'static str {
        match self {
            Self::Mp4 => "aac",
            Self::Webm => "opus",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportQuality {
    Low,
    Medium,
    High,
    VeryHigh,
}

/// Raster execution size. Source remains byte-compatible with existing receipts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RasterResolution {
    #[default]
    Source,
    Preview540,
    P720,
    P1080,
    P1440,
    P2160,
}
impl RasterResolution {
    pub fn is_source(&self) -> bool {
        *self == Self::Source
    }
    pub fn is_preview(&self) -> bool {
        matches!(self, Self::Preview540)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVideoOptions {
    /// Optional native runtime fence for reusable derived pictures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "RasterResolution::is_source")]
    pub resolution: RasterResolution,
    pub format: ExportFormat,
    pub quality: ExportQuality,
    pub include_audio: bool,
}
impl Default for ExportVideoOptions {
    fn default() -> Self {
        Self {
            runtime_fingerprint: None,
            resolution: RasterResolution::Source,
            format: ExportFormat::Mp4,
            quality: ExportQuality::High,
            include_audio: true,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportVisualProfile {
    TextV2,
    CaptionV3,
    GraphicV1,
    BlendV1,
    MaskV1,
    SceneV1,
}

/// Numeric target bitrates from Classic's pinned Mediabunny 1.41.0 Quality.
/// These are encoder targets, not promises about final bytes or exact equality
/// between native codecs and the browser WebCodecs encoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVideoRates {
    pub video_bitrate: u32,
    pub audio_bitrate: Option<u32>,
}
pub fn classic_export_rates(
    options: &ExportVideoOptions,
    width: u32,
    height: u32,
) -> Option<ExportVideoRates> {
    if width == 0 || height == 0 {
        return None;
    }
    let factor = match options.quality {
        ExportQuality::Low => 0.6,
        ExportQuality::Medium => 1.0,
        ExportQuality::High => 2.0,
        ExportQuality::VeryHigh => 4.0,
    };
    let efficiency = match options.format {
        ExportFormat::Mp4 => 1.0,
        ExportFormat::Webm => 0.6,
    };
    let video = (3_000_000.0
        * ((f64::from(width) * f64::from(height)) / (1920.0 * 1080.0)).powf(0.95)
        * efficiency
        * factor
        / 1000.0)
        .ceil()
        * 1000.0;
    if !video.is_finite() || video < 1.0 || video > f64::from(u32::MAX) {
        return None;
    }
    let audio = if !options.include_audio {
        None
    } else {
        Some(match options.format {
            ExportFormat::Mp4 => {
                let target = 128_000.0 * factor;
                // Strictly nearer keeps Classic's earlier entry on an equal tie.
                [128_000u32, 160_000, 192_000]
                    .into_iter()
                    .fold(96_000u32, |best, value| {
                        if (f64::from(value) - target).abs() < (f64::from(best) - target).abs() {
                            value
                        } else {
                            best
                        }
                    })
            }
            ExportFormat::Webm => ((64_000.0 * factor / 1000.0).round() * 1000.0) as u32,
        })
    };
    Some(ExportVideoRates {
        video_bitrate: video as u32,
        audio_bitrate: audio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolution_is_explicit_and_old_option_bytes_remain_exact() {
        let old = serde_json::json!({"format":"mp4","quality":"high","include_audio":true});
        let options: ExportVideoOptions = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(options.resolution, RasterResolution::Source);
        assert_eq!(serde_json::to_value(&options).unwrap(), old);
        for (name, resolution) in [
            ("p720", RasterResolution::P720),
            ("p1080", RasterResolution::P1080),
            ("p1440", RasterResolution::P1440),
            ("p2160", RasterResolution::P2160),
        ] {
            let mut value = old.clone();
            value["resolution"] = name.into();
            let parsed: ExportVideoOptions = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(parsed.resolution, resolution);
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        }
    }
    #[test]
    fn classic_four_quality_targets_and_audio_switch_are_explicit() {
        for (quality, h264, vp9, aac, opus) in [
            (ExportQuality::Low, 1_800_000, 1_080_000, 96_000, 38_000),
            (ExportQuality::Medium, 3_000_000, 1_800_000, 128_000, 64_000),
            (ExportQuality::High, 6_000_000, 3_600_000, 192_000, 128_000),
            (
                ExportQuality::VeryHigh,
                12_000_000,
                7_200_000,
                192_000,
                256_000,
            ),
        ] {
            for (format, video, audio) in [
                (ExportFormat::Mp4, h264, aac),
                (ExportFormat::Webm, vp9, opus),
            ] {
                let mut options = ExportVideoOptions {
                    runtime_fingerprint: None,
                    resolution: RasterResolution::Source,
                    format,
                    quality,
                    include_audio: true,
                };
                assert_eq!(
                    classic_export_rates(&options, 1920, 1080),
                    Some(ExportVideoRates {
                        video_bitrate: video,
                        audio_bitrate: Some(audio)
                    })
                );
                options.include_audio = false;
                assert_eq!(
                    classic_export_rates(&options, 1920, 1080)
                        .unwrap()
                        .audio_bitrate,
                    None
                );
            }
        }
        assert!(classic_export_rates(&ExportVideoOptions::default(), 0, 1080).is_none());
        assert!(
            serde_json::from_value::<ExportVideoOptions>(
                serde_json::json!({"format":"gif","quality":"high","include_audio":true})
            )
            .is_err()
        );
    }
}
