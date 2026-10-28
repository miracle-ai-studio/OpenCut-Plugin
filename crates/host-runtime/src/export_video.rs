//! Public capability assembly; no project state or local paths cross this DTO.
use opencut_job_contracts::export_video::*;
use opencut_native_services::export_video::ExportEncoderSupport;
pub(crate) fn capability() -> ExportVideoCapability {
    let result = encoder_support();
    let audio_runtime = result.is_ok() && crate::classic_audio_runtime::capability().available;
    report(result, audio_runtime)
}
pub(crate) fn encoder_support() -> Result<ExportEncoderSupport, opencut_contracts::EditorError> {
    crate::classic_visual_runtime::check_export_video_runtime()?;
    let ffmpeg = crate::media_tool_runtime::ffmpeg()?;
    crate::media_tool_runtime::ffprobe()?;
    opencut_native_services::export_video::probe_encoder_support(
        &ffmpeg,
        &opencut_media::CancellationToken::new(),
    )
}
fn report(
    result: Result<ExportEncoderSupport, opencut_contracts::EditorError>,
    audio_runtime: bool,
) -> ExportVideoCapability {
    let (formats, audio_available, unavailable_reason) = match result {
        Ok(encoders) => {
            let formats = encoders.formats();
            // This DTO has one audio flag. Be conservative for every advertised
            // format; video-only remains usable when any audio encoder is absent.
            let audio = audio_runtime
                && !formats.is_empty()
                && formats.iter().all(|f| encoders.audio_for(*f));
            let reason = formats
                .is_empty()
                .then(|| "No supported H.264 or VP9 export encoder is installed.".into());
            (formats, audio, reason)
        }
        Err(error) => (vec![], false, Some(error.message)),
    };
    ExportVideoCapability {
        schema_version: 2,
        available: !formats.is_empty(),
        audio_available,
        submit_tool: "opencut_job_export_video".into(),
        formats,
        qualities: vec![
            ExportQuality::Low,
            ExportQuality::Medium,
            ExportQuality::High,
            ExportQuality::VeryHigh,
        ],
        unavailable_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_audio_does_not_hide_video_but_single_audio_flag_covers_all_formats() {
        let encoders = ExportEncoderSupport {
            h264: true,
            vp9: true,
            aac: true,
            opus: true,
        };
        let silent = report(Ok(encoders), false);
        assert!(silent.available);
        assert!(!silent.audio_available);
        assert_eq!(silent.formats, vec![ExportFormat::Mp4, ExportFormat::Webm]);
        assert!(
            !report(
                Ok(ExportEncoderSupport {
                    opus: false,
                    ..encoders
                }),
                true
            )
            .audio_available
        );
        let mp4 = report(
            Ok(ExportEncoderSupport {
                vp9: false,
                opus: false,
                ..encoders
            }),
            true,
        );
        assert_eq!(mp4.formats, vec![ExportFormat::Mp4]);
        assert!(mp4.audio_available);
        let missing = report(Ok(ExportEncoderSupport::default()), true);
        assert!(!missing.available);
        assert!(!missing.audio_available);
        assert!(missing.unavailable_reason.is_some());
    }
    #[test]
    fn visual_or_probe_failure_never_advertises_an_export_format() {
        let report = report(
            Err(opencut_contracts::EditorError::new(
                "CAPABILITY_UNAVAILABLE",
                "missing persistent runner",
            )),
            true,
        );
        assert!(!report.available);
        assert!(!report.audio_available);
        assert!(report.formats.is_empty());
        assert_eq!(
            report.unavailable_reason.as_deref(),
            Some("missing persistent runner")
        );
    }
}
