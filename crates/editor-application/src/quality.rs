//! Report generation owns scope/identity/evidence checks; native analysis is a port.
use crate::{
    export_video as export,
    queries::JobRead,
    source_art::{AuthorizedSourceArt, SourceArtRead},
};
use opencut_api_contracts::{job_views::ArtifactReference, quality::*};
use opencut_contracts::{EditorError, scene::*};
use opencut_time::RationalTime as R;
pub struct PreparedQuality {
    pub job_id: String,
    pub attempt: u32,
    pub artifact: opencut_job_contracts::ArtifactDescriptor,
    pub source: AuthorizedSourceArt,
}
pub struct MeasuredQuality {
    pub measurements: OutputMeasurements,
    pub ffmpeg_sha256: String,
    pub ffprobe_sha256: String,
}
pub trait QualityNative {
    fn measure(&self, input: &PreparedQuality) -> Result<MeasuredQuality, EditorError>;
}
fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_ARGUMENT", message)
}
fn seconds(t: &opencut_contracts::Time) -> Result<f64, EditorError> {
    let t = R::try_from(t)?;
    Ok(t.ticks() as f64 / f64::from(t.timebase()))
}
fn caption_bounds(raster: &opencut_contracts::text_frame::TextRasterReceipt) -> [f64; 4] {
    let layout = &raster.layout;
    let transform = &layout.transform;
    let mut local = vec![];
    for line in &layout.lines {
        let left = match layout.align.as_str() {
            "left" | "start" => 0.0,
            "right" | "end" => -line.width,
            _ => -line.width / 2.0,
        };
        local.push([
            left,
            line.baseline_y - line.ascent,
            left + line.width,
            line.baseline_y + line.descent,
        ]);
        if let Some(rect) = &line.decoration {
            local.push([
                rect.left,
                rect.top,
                rect.left + rect.width,
                rect.top + rect.height,
            ]);
        }
    }
    if let Some(rect) = &layout.background {
        local.push([
            rect.left,
            rect.top,
            rect.left + rect.width,
            rect.top + rect.height,
        ]);
    }
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let (c, s) = (
        transform.rotation_radians.cos(),
        transform.rotation_radians.sin(),
    );
    for rect in local {
        for (x, y) in [
            (rect[0], rect[1]),
            (rect[2], rect[1]),
            (rect[2], rect[3]),
            (rect[0], rect[3]),
        ] {
            let (x, y) = (
                transform.center_x + transform.scale_x * (x * c - y * s),
                transform.center_y + transform.scale_y * (x * s + y * c),
            );
            bounds[0] = bounds[0].min(x);
            bounds[1] = bounds[1].min(y);
            bounds[2] = bounds[2].max(x);
            bounds[3] = bounds[3].max(y);
        }
    }
    bounds
}
pub fn inspect(
    store: &(impl JobRead + SourceArtRead),
    native: &impl QualityNative,
    input: QualityInspect,
) -> Result<QualityReport, EditorError> {
    if input.caption_frame_job_ids.len() > 32 {
        return Err(invalid(
            "Quality accepts at most32 existing scene-frame evidence jobs",
        ));
    }
    let (job_id, _) = crate::job_views::parse_artifact_id(&input.artifact_id)?;
    let record = store
        .read_job(job_id)
        .map_err(crate::job_views::public_error)?;
    let view = crate::job_views::project(record.clone());
    // Preparation belongs to the exact stored kind; public task names are projected.
    if view.state != crate::JobState::Succeeded
        || !matches!(
            record.kind.as_str(),
            export::EXPORT_VIDEO_KIND | export::EXPORT_VIDEO_RANGE_KIND
        )
    {
        return Err(invalid(
            "Quality requires a successful configurable whole/range video export",
        ));
    }
    let reference: ArtifactReference = view
        .artifacts
        .into_iter()
        .find(|a| a.artifact_id == input.artifact_id)
        .ok_or_else(|| invalid("Artifact is not a published output of this job"))?;
    let prepared: export::PreparedExportVideo = serde_json::from_value(record.input)
        .map_err(|_| invalid("Export preparation is unavailable"))?;
    let stored: export::StoredExportVideo = serde_json::from_value(
        record
            .result
            .ok_or_else(|| invalid("Export result missing"))?,
    )
    .map_err(|_| invalid("Export result unavailable"))?;
    let public = export::validate_stored_result(&prepared, &stored)?;
    let identity = &public.identity;
    let source = store.fixed_source_art_snapshot(&identity.project_id, &identity.revision)?;
    let sequence = source
        .snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == identity.sequence_id)
        .ok_or_else(|| invalid("Export scene missing"))?;
    let mut caption_samples = vec![];
    let mut unique = std::collections::BTreeSet::new();
    let mapping = export::range_mapping(&prepared, &export::prepare(&prepared)?)?;
    let start = mapping
        .as_ref()
        .map(|m| &m.source_start)
        .map(seconds)
        .transpose()?
        .unwrap_or(0.0);
    let end = start + seconds(&identity.video_duration)?;
    for id in &input.caption_frame_job_ids {
        if !unique.insert(id) {
            return Err(invalid("Caption evidence jobs must be unique"));
        }
        let frame = crate::queries::job_view(store, id)?;
        if frame.state != crate::JobState::Succeeded
            || frame.kind != "render.frame"
            || frame.result_unavailable.is_some()
        {
            return Err(invalid(
                "Caption evidence requires published scene-frame results",
            ));
        }
        let Some(opencut_api_contracts::job_results::JobResultView::SceneFrame(frame)) =
            frame.result
        else {
            return Err(invalid("Frame evidence missing"));
        };
        if frame.identity.project_id != identity.project_id
            || frame.identity.revision != identity.revision
            || frame.identity.sequence_id != identity.sequence_id
        {
            return Err(invalid(
                "Caption evidence does not match the exported revision/scene",
            ));
        }
        let time = seconds(&frame.identity.time)?;
        if time < start || time >= end {
            return Err(invalid("Caption evidence is outside this export interval"));
        }
        for raster in &frame.texts {
            if sequence.captions.iter().any(|c| c.id == raster.element_id) {
                let bounds = caption_bounds(raster);
                if bounds.iter().any(|n| !n.is_finite()) {
                    return Err(invalid("Caption layout bounds unavailable"));
                }
                caption_samples.push(CaptionBoundsSample {
                    job_id: id.clone(),
                    time: frame.identity.time.clone(),
                    caption_id: raster.element_id.clone(),
                    same_runtime_as_export: frame.execution == public.execution.visual,
                    visual: frame.execution.clone(),
                    outside_canvas: bounds[0] < 0.0
                        || bounds[1] < 0.0
                        || bounds[2] > f64::from(identity.width)
                        || bounds[3] > f64::from(identity.height),
                    bounds,
                });
            }
        }
    }
    let mut temporal = vec![];
    for caption in &sequence.captions {
        let a = seconds(&caption.timeline_range.start)?;
        let b = seconds(&opencut_time::range_end(&caption.timeline_range)?)?;
        if b > start && a < end && (a < start || b > end) {
            temporal.push(caption.id.clone());
        }
    }
    let measured = native.measure(&PreparedQuality {
        job_id: job_id.into(),
        attempt: record.attempt,
        artifact: stored.artifact,
        source: source.clone(),
    })?;
    store.revalidate_source_art(&identity.project_id, &identity.revision, &source.authority)?;
    // The artifact may have been collected or unpublished during analysis; recheck
    // the authorized public result as well as the exact bytes held by the adapter.
    let current = crate::queries::job_view(store, job_id)?;
    if !current
        .artifacts
        .iter()
        .any(|a| a.artifact_id == reference.artifact_id)
    {
        return Err(invalid("Export artifact is no longer published"));
    }
    let m = &measured.measurements;
    let mut findings = vec![];
    let tolerance: f64 = if identity.options.format == export::ExportFormat::Webm {
        0.05
    } else {
        0.002
    };
    if (m.duration_seconds - seconds(&identity.video_duration)?).abs() > 0.1 {
        findings.push("output_duration_differs".into());
    }
    if m.video.codec != identity.options.format.video_codec()
        || m.video.width != Some(identity.width)
        || m.video.height != Some(identity.height)
    {
        findings.push("video_format_or_dimensions_differ".into());
    }
    if m.video.average_rate.is_none_or(|(a, b)| {
        u64::from(a) * u64::from(identity.fps.denominator)
            != u64::from(b) * u64::from(identity.fps.numerator)
    }) {
        findings.push("frame_rate_differs".into());
    }
    if m.video_frames_decoded != identity.frame_count {
        findings.push("decoded_frame_count_differs".into());
    }
    if m.audio.as_ref().map(|a| a.codec.as_str())
        != identity
            .options
            .include_audio
            .then(|| identity.options.format.audio_codec())
    {
        findings.push("audio_stream_or_codec_differs".into());
    }
    if let Some(audio) = &m.audio {
        match (
            audio.duration_seconds,
            identity
                .audio_frame_count
                .as_ref()
                .and_then(|n| n.parse::<u64>().ok()),
        ) {
            (Some(actual), Some(frames)) if (actual - frames as f64 / 48000.0).abs() > 0.05 => {
                findings.push("audio_duration_differs".into())
            }
            (None, _) => findings.push("audio_stream_duration_unavailable".into()),
            _ => {}
        }
    }
    if caption_samples.iter().any(|sample| sample.outside_canvas) {
        findings.push("caption_sample_layout_outside_canvas".into());
    }
    if caption_samples
        .iter()
        .any(|sample| !sample.same_runtime_as_export)
    {
        findings.push("caption_evidence_runtime_differs".into());
    }
    if !input.caption_frame_job_ids.is_empty()
        && !sequence.captions.is_empty()
        && caption_samples.is_empty()
    {
        findings.push("no_caption_spatial_samples".into());
    }
    let offset = m
        .video
        .start_seconds
        .zip(m.audio.as_ref().and_then(|a| a.start_seconds))
        .map(|(v, a)| a - v);
    if offset.is_some_and(|offset| offset.abs() > tolerance.max(0.05)) {
        findings.push("stream_timestamp_offset".into());
    }
    if !m.black_intervals.is_empty() {
        findings.push("black_intervals_detected".into());
    }
    if !m.frozen_intervals.is_empty() {
        findings.push("frozen_intervals_detected".into());
    }
    if m.true_peak_dbfs.is_some_and(|p| p > 0.0) {
        findings.push("true_peak_above_zero_dbfs".into());
    }
    Ok(QualityReport{artifact:reference,project_id:identity.project_id.clone(),revision:identity.revision.clone(),sequence_id:identity.sequence_id.clone(),ffmpeg_sha256:measured.ffmpeg_sha256,ffprobe_sha256:measured.ffprobe_sha256,measurements:measured.measurements,technical_findings:findings,timestamp_av_offset_seconds:offset,caption_samples,temporal_caption_findings:temporal,caption_coverage:if input.caption_frame_job_ids.is_empty(){"not_measured"}else{"provided_frame_samples_only; transformed_font_metrics_not_pixel_alpha"}.into(),limitations:vec!["Timestamp alignment is not semantic lip-sync verification.".into(),"Black/frozen intervals may be intentional editorial choices; no aesthetic pass/fail is inferred.".into(),"Caption samples do not prove unsampled frames or the full output.".into()]})
}

/// A deterministic review sample plan, with no hidden render submissions.
pub fn evidence_plan(
    store: &impl crate::scene::SceneRead,
    input: QualityEvidenceRequest,
) -> Result<QualityEvidencePlan, EditorError> {
    if input.revision.is_empty()
        || input.request_key.is_empty()
        || input.request_key.len() > 1024
        || !(1..=24).contains(&input.max_frames)
    {
        return Err(invalid(
            "Evidence planning requires a fixed revision, bounded key and 1..24 samples",
        ));
    }
    let snapshot = crate::scene::inspect(
        store,
        &opencut_contracts::project_inspection::InspectProject {
            project_id: input.project_id.clone(),
            revision: Some(input.revision.clone()),
        },
    )?;
    let sequence = snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == input.sequence_id)
        .ok_or_else(|| invalid("Evidence scene missing"))?;
    let SettingsProfile::ClassicGlobal { settings } = &snapshot.project.settings else {
        return Err(invalid("Evidence requires a global output frame grid"));
    };
    let mut extent = R::new(0, 1)?;
    let mut samples: std::collections::BTreeMap<u64, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    let frame_index = |at: R| -> Result<u64, EditorError> {
        let numerator = i128::from(at.ticks()) * i128::from(settings.fps.numerator);
        let denominator = i128::from(at.timebase()) * i128::from(settings.fps.denominator);
        u64::try_from(numerator / denominator).map_err(|_| invalid("Evidence frame time overflow"))
    };
    for range in sequence
        .tracks
        .iter()
        .flat_map(|t| t.elements.iter().map(|e| &e.timeline_range))
        .chain(sequence.captions.iter().map(|c| &c.timeline_range))
    {
        let end = R::try_from(&opencut_time::range_end(range)?)?;
        if end.cmp_exact(extent).is_gt() {
            extent = end;
        }
    }
    let mut add = |at: R, reason: &str| -> Result<(), EditorError> {
        let index = frame_index(at)?;
        for frame in [index.saturating_sub(1), index, index.saturating_add(1)] {
            let time = R::try_from(&crate::media_video_jobs::grid_time(&settings.fps, frame)?)?;
            if time.cmp_exact(extent).is_lt() {
                samples.entry(frame).or_default().insert(reason.into());
            }
        }
        Ok(())
    };
    for track in &sequence.tracks {
        for element in &track.elements {
            add(R::try_from(&element.timeline_range.start)?, "clip_boundary")?;
            add(
                R::try_from(&opencut_time::range_end(&element.timeline_range)?)?,
                "clip_boundary",
            )?;
        }
    }
    for caption in &sequence.captions {
        let start = R::try_from(&caption.timeline_range.start)?;
        let duration = R::try_from(&caption.timeline_range.duration)?;
        add(
            start.checked_add(duration.checked_scale(R::new(1, 1)?, R::new(2, 1)?)?)?,
            "caption_midpoint",
        )?;
        add(start, "caption_boundary")?;
    }
    let mut candidates: Vec<_> = samples.into_iter().collect();
    let candidate_frames = candidates.len();
    // Dense caption/clip boundaries receive priority, then cover the sorted
    // remainder evenly. The report always retains the omitted count.
    candidates.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    let limit = usize::from(input.max_frames);
    let selected = if candidates.len() <= limit {
        candidates
    } else {
        let dense = candidates
            .iter()
            .take_while(|(_, reasons)| reasons.len() > 1)
            .count()
            .min(limit / 2);
        let mut out = candidates[..dense].to_vec();
        let mut rest = candidates[dense..].to_vec();
        rest.sort_by_key(|(frame, _)| *frame);
        let remaining = limit - dense;
        for i in 0..remaining {
            out.push(rest[i * (rest.len() - 1) / remaining.saturating_sub(1).max(1)].clone());
        }
        out
    };
    let mut frames = vec![];
    for (frame, reasons) in selected {
        let key = crate::identity::request_identity(
            "quality_evidence_frame_v1",
            &input.project_id,
            &(&input, frame),
        )?
        .current_digest()
        .to_owned();
        frames.push(QualityEvidenceFrame {
            reasons: reasons.into_iter().collect(),
            request: opencut_job_contracts::scene_frame::FrameRenderInput {
                idempotency_key: key,
                project_id: input.project_id.clone(),
                revision: input.revision.clone(),
                sequence_id: input.sequence_id.clone(),
                time: crate::media_video_jobs::grid_time(&settings.fps, frame)?,
            },
        });
    }
    frames.sort_by(|a, b| {
        R::try_from(&a.request.time)
            .expect("frame grid")
            .cmp_exact(R::try_from(&b.request.time).expect("frame grid"))
    });
    Ok(QualityEvidencePlan{project_id:input.project_id,revision:input.revision,sequence_id:input.sequence_id,omitted_frames:candidate_frames.saturating_sub(frames.len()),candidate_frames,frames,coverage:"deterministic clip-boundary and caption samples only; submit returned requests unchanged through render.v4.submit".into()})
}

#[cfg(test)]
mod tests;
