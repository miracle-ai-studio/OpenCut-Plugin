//! Native encoding of already verified RGBA/PCM. Quality targets follow Classic
//! scene-exporter.ts -> Mediabunny 1.41.0 Quality; rendering remains unchanged.
use crate::classic_video::ClassicVideoEncoderConfig;
use opencut_contracts::{EditorError, export_video::*};
use serde_json::Value;
use std::process::Command;
fn invalid(message: &str) -> EditorError {
    EditorError::new("VIDEO_ENCODER_FAILED", message)
}
#[derive(Debug, Clone, Copy, Default)]
pub struct ExportEncoderSupport {
    pub h264: bool,
    pub vp9: bool,
    pub aac: bool,
    pub opus: bool,
}
impl ExportEncoderSupport {
    pub fn formats(self) -> Vec<ExportFormat> {
        [
            self.h264.then_some(ExportFormat::Mp4),
            self.vp9.then_some(ExportFormat::Webm),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
    pub fn audio_for(self, format: ExportFormat) -> bool {
        match format {
            ExportFormat::Mp4 => self.aac,
            ExportFormat::Webm => self.opus,
        }
    }
}
pub fn probe_encoder_support(
    ffmpeg: &std::path::Path,
    cancel: &opencut_media::CancellationToken,
) -> Result<ExportEncoderSupport, EditorError> {
    let names =
        opencut_media::FfmpegBackend::export_encoder_names(ffmpeg, cancel).map_err(|_| {
            EditorError::new(
                "CAPABILITY_UNAVAILABLE",
                "Bounded export encoder inventory is unavailable",
            )
        })?;
    Ok(ExportEncoderSupport {
        h264: names.contains("libx264"),
        vp9: names.contains("libvpx-vp9"),
        aac: names.contains("aac"),
        opus: names.contains("libopus"),
    })
}
pub(crate) fn append_arguments(
    command: &mut Command,
    config: &ClassicVideoEncoderConfig,
    options: &ExportVideoOptions,
) -> Result<(), EditorError> {
    let rates = classic_export_rates(options, config.width, config.height)
        .ok_or_else(|| invalid("Invalid configurable encoder dimensions"))?;
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-n",
            "-protocol_whitelist",
            "file,pipe",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
        ])
        .arg(format!("{}x{}", config.width, config.height))
        .arg("-framerate")
        .arg(format!(
            "{}/{}",
            config.fps_numerator, config.fps_denominator
        ))
        .args(["-i", "pipe:0"]);
    if options.include_audio {
        command.arg("-i").arg(&config.audio);
    }
    command
        .args([
            "-map",
            "0:v:0",
            "-c:v",
            match options.format {
                ExportFormat::Mp4 => "libx264",
                ExportFormat::Webm => "libvpx-vp9",
            },
            "-pix_fmt",
            "yuv420p",
            "-b:v",
        ])
        .arg(rates.video_bitrate.to_string())
        .args([
            "-threads",
            "2",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
        ]);
    match options.format {
        ExportFormat::Mp4 => {
            command.args(["-preset", "medium", "-movflags", "+faststart"]);
        }
        ExportFormat::Webm => {
            command.args(["-deadline", "good", "-cpu-used", "2", "-row-mt", "1"]);
        }
    }
    if let Some(rate) = rates.audio_bitrate {
        command
            .args([
                "-map",
                "1:a:0",
                "-c:a",
                match options.format {
                    ExportFormat::Mp4 => "aac",
                    ExportFormat::Webm => "libopus",
                },
                "-b:a",
            ])
            .arg(rate.to_string())
            .args(["-ar", "48000", "-ac", "2"]);
    } else {
        command.arg("-an");
    }
    command
        .args(["-f", options.format.extension()])
        .arg(&config.output);
    Ok(())
}
pub(crate) struct ObservedExport {
    pub frames: u64,
    pub video_seconds: f64,
    pub audio_seconds: Option<f64>,
}
fn duration(stream: &Value) -> Result<f64, EditorError> {
    let direct = stream
        .get("duration")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok());
    let tagged = || {
        let value = stream.get("tags")?.get("DURATION")?.as_str()?;
        let mut parts = value.split(':');
        let h = parts.next()?.parse::<u32>().ok()?;
        let m = parts.next()?.parse::<u32>().ok()?;
        let s = parts.next()?.parse::<f64>().ok()?;
        if parts.next().is_some() || m >= 60 || !(0.0..60.0).contains(&s) {
            return None;
        }
        Some(f64::from(h) * 3600.0 + f64::from(m) * 60.0 + s)
    };
    direct
        .or_else(tagged)
        .filter(|n| n.is_finite() && *n > 0.0)
        .ok_or_else(|| invalid("Missing observed stream duration"))
}
pub(crate) fn verify_probe(
    probe: &Value,
    config: &ClassicVideoEncoderConfig,
    options: &ExportVideoOptions,
) -> Result<ObservedExport, EditorError> {
    let streams = probe
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Missing export streams"))?;
    if streams.len() != if options.include_audio { 2 } else { 1 } {
        return Err(invalid("Unexpected exported stream count"));
    }
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or_else(|| invalid("Missing exported video"))?;
    let frames = video["nb_read_frames"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| invalid("Missing decoded video frame count"))?;
    let format = probe["format"]["format_name"]
        .as_str()
        .ok_or_else(|| invalid("Missing observed container"))?;
    let rate = video["r_frame_rate"]
        .as_str()
        .and_then(|s| s.split_once('/'))
        .and_then(|(n, d)| Some((n.parse::<u64>().ok()?, d.parse::<u64>().ok()?)));
    if !format.split(',').any(|s| s == options.format.extension())
        || video["codec_name"] != options.format.video_codec()
        || video["pix_fmt"] != "yuv420p"
        || video["width"].as_u64() != Some(u64::from(config.width))
        || video["height"].as_u64() != Some(u64::from(config.height))
        || frames != config.frame_count
        || rate.is_none_or(|(n, d)| {
            n == 0
                || d == 0
                || u128::from(n) * u128::from(config.fps_denominator)
                    != u128::from(d) * u128::from(config.fps_numerator)
        })
    {
        return Err(invalid(
            "Observed container/video differs from fixed export configuration",
        ));
    }
    let audio_seconds = if options.include_audio {
        let audio = streams
            .iter()
            .find(|s| s["codec_type"] == "audio")
            .ok_or_else(|| invalid("Missing requested audio stream"))?;
        if audio["codec_name"] != options.format.audio_codec()
            || audio["sample_rate"] != "48000"
            || audio["channels"] != 2
        {
            return Err(invalid(
                "Observed export audio differs from fixed configuration",
            ));
        }
        Some(duration(audio)?)
    } else {
        None
    };
    Ok(ObservedExport {
        frames,
        video_seconds: duration(video)?,
        audio_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn video_and_audio_support_are_independent_for_both_formats() {
        let partial = ExportEncoderSupport {
            h264: true,
            vp9: true,
            aac: true,
            opus: false,
        };
        assert_eq!(
            partial.formats(),
            vec![ExportFormat::Mp4, ExportFormat::Webm]
        );
        assert!(partial.audio_for(ExportFormat::Mp4));
        assert!(!partial.audio_for(ExportFormat::Webm));
        assert!(
            ExportEncoderSupport {
                aac: true,
                opus: true,
                ..Default::default()
            }
            .formats()
            .is_empty()
        );
    }
    fn config() -> ClassicVideoEncoderConfig {
        ClassicVideoEncoderConfig {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            output: "output.webm".into(),
            audio: "private.wav".into(),
            width: 1920,
            height: 1080,
            fps_numerator: 30,
            fps_denominator: 1,
            frame_count: 90,
            max_output_bytes: 1 << 24,
            max_rss_bytes: 1 << 26,
            execution_limit:
                opencut_editor_application::media_video_jobs::VideoExecutionLimit::Total {
                    milliseconds: 30000,
                },
        }
    }
    #[test]
    fn all_quality_targets_reach_encoder_and_audio_off_has_no_audio_input_or_encoder() {
        for format in [ExportFormat::Mp4, ExportFormat::Webm] {
            let mut observed = std::collections::BTreeSet::new();
            for quality in [
                ExportQuality::Low,
                ExportQuality::Medium,
                ExportQuality::High,
                ExportQuality::VeryHigh,
            ] {
                let options = ExportVideoOptions {
                    runtime_fingerprint: None,
                    resolution: opencut_contracts::export_video::RasterResolution::Source,
                    format,
                    quality,
                    include_audio: false,
                };
                let mut command = Command::new("not-executed");
                append_arguments(&mut command, &config(), &options).unwrap();
                let args = command
                    .get_args()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                let pixels = args.iter().position(|a| a == "-pixel_format").unwrap();
                assert_eq!(args[pixels + 1], "rgb24");
                let at = args.iter().position(|a| a == "-b:v").unwrap();
                observed.insert(args[at + 1].clone());
                assert!(
                    !args
                        .iter()
                        .any(|s| s == "private.wav" || s == "-c:a" || s == "-crf")
                );
                assert!(args.iter().any(|s| s == "-an"));
            }
            assert_eq!(observed.len(), 4);
        }
    }
    #[test]
    fn webm_observation_checks_real_container_codec_count_and_absent_audio() {
        let options = ExportVideoOptions {
            runtime_fingerprint: None,
            resolution: opencut_contracts::export_video::RasterResolution::Source,
            format: ExportFormat::Webm,
            quality: ExportQuality::High,
            include_audio: false,
        };
        let mut probe = serde_json::json!({"format":{"format_name":"matroska,webm"},"streams":[{"codec_type":"video","codec_name":"vp9","pix_fmt":"yuv420p","width":1920,"height":1080,"r_frame_rate":"30/1","nb_read_frames":"90","tags":{"DURATION":"00:00:03.000000000"}}]});
        let observed = verify_probe(&probe, &config(), &options).unwrap();
        assert_eq!(observed.video_seconds, 3.0);
        assert_eq!(observed.audio_seconds, None);
        probe["streams"][0]["codec_name"] = "h264".into();
        assert!(verify_probe(&probe, &config(), &options).is_err());
        probe["streams"][0]["codec_name"] = "vp9".into();
        probe["streams"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"codec_type":"audio"}));
        assert!(verify_probe(&probe, &config(), &options).is_err());
    }
}
