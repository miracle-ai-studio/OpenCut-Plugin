//! Read-only editable subtitle planning over the complete fixed V4 aggregate.
pub use crate::subtitle_measurement::MeasuredSubtitleTextRequest as SubtitleTextPlanRequest;
use opencut_composition::{CompositionScope, scene::*};
use opencut_contracts::{
    EditorError, Time, TimeRange, caption_frame::CaptionRasterPlan, project_public::*, scene::*,
    scene_ops::*, subtitle_text::*, subtitles::SubtitleFormat, text_authoring::TextTrackTarget,
    text_frame::TextLayerPlan, timeline::TimelineTarget,
};
use opencut_job_contracts::scene_transcription::SceneTranscriptionResult;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct PreparedSubtitleText {
    snapshot: EditorSnapshot,
    request: SubtitleTextPlanRequest,
    sequence: Scene,
    track_id: String,
    ids: Vec<String>,
    frames: Vec<PreparedTextFrame>,
    warnings: Vec<String>,
    layouts: Vec<SubtitleCueLayout>,
}
/// Private per-cue input shared by the Classic-compatible ASS operation.
/// These are prepared domain facts, never a new public bypass around parsing.
#[derive(Clone, Debug)]
pub(crate) struct SubtitleCueLayout {
    pub max_width_px: Option<f64>,
    pub left_margin_ratio: f64,
    pub right_margin_ratio: f64,
    pub vertical_margin_ratio: f64,
    /// 0 bottom, 1 middle, 2 top, as Classic's numpad alignment rows.
    pub vertical_align: u8,
    pub allow_zero_width: bool,
}
pub(crate) struct SubtitleCueInput {
    pub range: TimeRange,
    pub text: Text,
    pub layout: SubtitleCueLayout,
}
impl PreparedSubtitleText {
    pub fn measurement_directive(&self, index: usize) -> Result<CaptionRasterPlan, EditorError> {
        let frame = self
            .frames
            .get(index)
            .ok_or_else(|| invalid("Subtitle measurement index is outside its prepared cues"))?;
        let canvas = frame.identity();
        let placement = &self.layouts[index];
        let max_width_px = placement
            .max_width_px
            .unwrap_or(f64::from(canvas.width) * 0.8);
        if !max_width_px.is_finite()
            || max_width_px < 0.
            || (!placement.allow_zero_width && max_width_px == 0.)
            || (!placement.allow_zero_width && max_width_px > f64::from(canvas.width))
        {
            return Err(invalid("Subtitle wrap width must fit canvas"));
        }
        let directive = CaptionRasterPlan {
            element_id: self.ids[index].clone(),
            max_width_px,
            bottom_margin_px: f64::from(canvas.height) * 0.05,
            wrap_profile: CLASSIC_CAPTION_WRAP_PROFILE.into(),
        };
        Ok(directive)
    }
    pub fn frames(&self) -> &[PreparedTextFrame] {
        &self.frames
    }
}
fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_SUBTITLE_TEXT_PLAN", message)
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn zero() -> Time {
    Time {
        ticks: "0".into(),
        timebase: 1,
    }
}
fn source(
    request: &SubtitleTextPlanRequest,
    result: Option<&SceneTranscriptionResult>,
) -> Result<(Vec<(TimeRange, String)>, Vec<String>), EditorError> {
    match &request.source {
        SubtitleTextSource::File { format, content } => {
            if content.len() > 2 * 1024 * 1024 {
                return Err(invalid("Subtitle file exceeds 2 MiB"));
            }
            let fallback = opencut_contracts::CaptionStyle {
                font_family: "Explicit text style".into(),
                font_size: 24.0,
                color: "#ffffff".into(),
                background_color: None,
                font_policy: opencut_contracts::FontResolutionPolicy::Strict {},
                max_width: None,
                line_height: None,
                text_align: opencut_contracts::TextAlign::Center,
            };
            let cues = match format {
                SubtitleFormat::Srt => crate::subtitle_exchange::parse_srt(content, &fallback)?,
                SubtitleFormat::Ass => crate::subtitle_exchange::parse_ass(content, &fallback)?,
            };
            Ok((
                cues.into_iter()
                    .map(|c| (c.timeline_range, c.text))
                    .collect(),
                vec![
                    "Interchange styles are replaced by the explicitly selected TextV4 style"
                        .into(),
                ],
            ))
        }
        SubtitleTextSource::Transcript {
            words_per_caption, ..
        } => {
            let result =
                result.ok_or_else(|| invalid("Fixed successful transcription is required"))?;
            if result.project_id != request.project_id
                || result.revision != request.expected_revision
                || result.sequence_id != request.sequence_id
            {
                return Err(invalid("Transcription fixed scope differs"));
            }
            let cues = crate::captions::transcript_cues(*words_per_caption, result)?
                .into_iter()
                .map(|cue| (cue.timeline_range, cue.text))
                .collect();
            Ok((cues,vec!["Word timings are estimated proportionally within ASR segments; they are not measured word timestamps".into()]))
        }
    }
}

pub fn prepare(
    snapshot: &EditorSnapshot,
    request: &SubtitleTextPlanRequest,
    result: Option<&SceneTranscriptionResult>,
) -> Result<PreparedSubtitleText, EditorError> {
    prepare_with_checkpoint(snapshot, request, result, &mut || Ok(()))
}

pub fn prepare_with_checkpoint(
    snapshot: &EditorSnapshot,
    request: &SubtitleTextPlanRequest,
    result: Option<&SceneTranscriptionResult>,
    checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
) -> Result<PreparedSubtitleText, EditorError> {
    prepare_cues_with_checkpoint(
        snapshot,
        request,
        || {
            let (cues, warnings) = source(request, result)?;
            let cues = cues
                .into_iter()
                .map(|(range, content)| {
                    let mut text = request.style.clone();
                    text.content = content;
                    SubtitleCueInput {
                        range,
                        text,
                        layout: SubtitleCueLayout {
                            max_width_px: request.max_width_px.map(f64::from),
                            left_margin_ratio: 0.,
                            right_margin_ratio: 0.,
                            vertical_margin_ratio: 0.05,
                            vertical_align: 0,
                            allow_zero_width: false,
                        },
                    }
                })
                .collect();
            Ok((cues, warnings))
        },
        checkpoint,
    )
}

pub(crate) fn prepare_cues_with_checkpoint(
    snapshot: &EditorSnapshot,
    request: &SubtitleTextPlanRequest,
    source: impl FnOnce() -> Result<(Vec<SubtitleCueInput>, Vec<String>), EditorError>,
    checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
) -> Result<PreparedSubtitleText, EditorError> {
    checkpoint()?;
    crate::require_idempotency_key(&request.idempotency_key)?;
    if snapshot.project.id != request.project_id || snapshot.revision != request.expected_revision {
        return Err(crate::revision_conflict(
            &request.expected_revision,
            &snapshot.revision,
        ));
    }
    if !request.style.content.is_empty() {
        return Err(invalid("Subtitle style content must be empty"));
    }
    if !matches!(request.style.font, FontBinding::Locked { .. }) {
        return Err(invalid("Subtitle text requires an imported locked font"));
    }
    let mut sequence = snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == request.sequence_id)
        .cloned()
        .ok_or_else(|| invalid("Subtitle scene does not exist"))?;
    let track_id = match &request.track {
        TextTrackTarget::Existing { track_id } | TextTrackTarget::New { track_id } => {
            track_id.clone()
        }
    };
    if track_id.trim().is_empty() || track_id.len() > 128 || track_id.chars().any(char::is_control)
    {
        return Err(invalid("Invalid subtitle track identity"));
    }
    let index =
        match &request.track {
            TextTrackTarget::Existing { .. } => sequence
                .tracks
                .iter()
                .position(|t| t.id == track_id)
                .ok_or_else(|| invalid("Subtitle text track is missing"))?,
            TextTrackTarget::New { .. } => {
                sequence.tracks.push(SceneTrack {
                    id: track_id.clone(),
                    name: "Subtitles".into(),
                    kind: SceneTrackKind::Text,
                    enabled: true,
                    locked: false,
                    muted: false,
                    opacity: 1.0,
                    gain: Gain::Decibels { value: 0.0 },
                    elements: vec![],
                    channels: vec![],
                });
                sequence.tracks.len() - 1
            }
        };
    let track = &mut sequence.tracks[index];
    if track.locked {
        return Err(EditorError::new("TRACK_LOCKED", "Subtitle track is locked"));
    }
    if track.kind != SceneTrackKind::Text
        || !track.enabled
        || track.opacity != 1.0
        || !track.channels.is_empty()
    {
        return Err(invalid(
            "Subtitle target must be an enabled ordinary text track",
        ));
    }
    let (cues, warnings) = source()?;
    if cues.is_empty() || cues.len() > 5000 {
        return Err(invalid("Subtitle source requires 1..5000 cues"));
    }
    let mut ids = Vec::new();
    let mut layouts = Vec::new();
    for (i, cue) in cues.into_iter().enumerate() {
        checkpoint()?;
        let SubtitleCueInput {
            range,
            text,
            layout,
        } = cue;
        if !matches!(text.font, FontBinding::Locked { .. })
            || layout.vertical_align > 2
            || [
                layout.left_margin_ratio,
                layout.right_margin_ratio,
                layout.vertical_margin_ratio,
            ]
            .iter()
            .any(|v| !v.is_finite())
        {
            return Err(invalid("Invalid fixed subtitle cue style/placement"));
        }
        layouts.push(layout);
        let id = crate::content_id(
            "subtitle-text",
            format!(
                "{}:{}:{}:{}",
                request.project_id, request.sequence_id, request.idempotency_key, i
            )
            .as_bytes(),
        );
        track.elements.push(TimelineElement {
            id: id.clone(),
            name: format!("Subtitle {}", i + 1),
            timeline_range: range,
            enabled: true,
            source_span: ElementSourceSpan::ClassicTrimmed {
                trim_start: zero(),
                trim_end: zero(),
                source_duration: None,
            },
            content: ElementContent::Text { text },
            channels: vec![],
            visual: Some(VisualStyle {
                profile: VisualProfile::ClassicCanvasV31,
                transform: Transform {
                    position_x_px: 0.0,
                    position_y_px: 0.0,
                    scale_x: 1.0,
                    scale_y: 1.0,
                    rotation_degrees: 0.0,
                },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                effects: vec![],
                masks: vec![],
            }),
        });
        ids.push(id);
    }
    // Validate the aggregate before Put can accidentally replace a global ID.
    let mut candidate = snapshot.project.clone();
    *candidate
        .sequences
        .iter_mut()
        .find(|s| s.id == request.sequence_id)
        .unwrap() = sequence.clone();
    opencut_editor_core::scene::validate_project_scene(&candidate)?;
    candidate = crate::scene::plan(
        snapshot,
        &ApplySceneProject {
            project_id: request.project_id.clone(),
            expected_revision: request.expected_revision.clone(),
            idempotency_key: request.idempotency_key.clone(),
            label: "Create subtitle text".into(),
            operations: vec![ProjectOperation::SequencePut {
                sequence: sequence.clone(),
            }],
        },
    )?;
    let candidate = EditorSnapshot {
        project: candidate,
        revision: snapshot.revision.clone(),
    };
    let mut frames = Vec::new();
    for id in &ids {
        checkpoint()?;
        let element = sequence.tracks[index]
            .elements
            .iter()
            .find(|e| &e.id == id)
            .unwrap();
        let mut sampled = compile_frame_scene(
            &candidate,
            &request.sequence_id,
            &element.timeline_range.start,
            CompositionScope::Visual,
        )
        .map_err(|e| EditorError::new(e.code, e.message))?;
        let target =
            prepare_text_raster(&sampled, id).map_err(|e| EditorError::new(e.code, e.message))?;
        sampled.layers.retain(|l| &l.element_id == id);
        sampled.fonts.retain(|f| f.id == target.font.id);
        sampled.resources.clear();
        sampled.transitions.clear();
        let frame =
            prepare_text_frame(&sampled).map_err(|e| EditorError::new(e.code, e.message))?;
        let width = frame.identity().width;
        if request.max_width_px.is_some_and(|w| w == 0 || w > width) {
            return Err(invalid("Subtitle wrap width must fit canvas"));
        }
        frames.push(frame);
    }
    let mut fixed = request.clone();
    fixed.observations = None;
    Ok(PreparedSubtitleText {
        snapshot: snapshot.clone(),
        request: fixed,
        sequence,
        track_id,
        ids,
        frames,
        warnings,
        layouts,
    })
}

pub fn finish(
    prepared: &PreparedSubtitleText,
    request: &SubtitleTextPlanRequest,
    layers: &[TextLayerPlan],
) -> Result<SubtitleTextPlanResult, EditorError> {
    let mut fixed = request.clone();
    fixed.observations = None;
    if fixed != prepared.request || layers.len() != prepared.frames.len() {
        return Err(invalid(
            "Subtitle fixed request or layer collection changed",
        ));
    }
    if request
        .observations
        .as_ref()
        .is_some_and(|o| o.len() != layers.len())
    {
        return Err(invalid(
            "Subtitle observations must cover every cue exactly once",
        ));
    }
    let mut sequence = prepared.sequence.clone();
    let track = sequence
        .tracks
        .iter_mut()
        .find(|t| t.id == prepared.track_id)
        .unwrap();
    let mut cues = Vec::new();
    for (i, (frame, layer)) in prepared.frames.iter().zip(layers).enumerate() {
        let original = &frame.texts()[0];
        let font = &original.font;
        if layer.element_id != prepared.ids[i]
            || layer.text != crate::classic_text_frame::text_input(&original.text)
            || layer.font.font_id != font.id
            || layer.font.content_hash != font.content.content_hash
            || layer.font.size != font.content.size.to_string()
            || layer.font.weight != font.weight
            || layer.font.style != font.style
            || layer.font.face_index != font.face_index
            || !layer.font.glyph_coverage_validated
            || layer.font.validated_text_hash != hash(&original.text.content)
            || layer.opacity != original.opacity
        {
            return Err(invalid(
                "Subtitle native layer differs from its fixed text/font/transform",
            ));
        }
        let canvas = frame.identity();
        let placement = &prepared.layouts[i];
        let directive = prepared.measurement_directive(i)?;
        let element = track
            .elements
            .iter_mut()
            .find(|e| e.id == layer.element_id)
            .unwrap();
        if let Some(observations) = &request.observations {
            let observation = &observations[i];
            let receipt = &observation.caption;
            let raster = &observation.raster;
            if receipt.element_id != layer.element_id
                || receipt.original_text_hash != hash(&original.text.content)
                || raster.element_id != layer.element_id
                || raster.width != canvas.width
                || raster.height != canvas.height
            {
                return Err(invalid("Subtitle observation identity/hash/canvas differs"));
            }
            let verify_wrap = if placement.allow_zero_width {
                crate::classic_caption_frame::verify_classic_wrap
            } else {
                crate::classic_caption_frame::verify_wrap
            };
            let wrapped = verify_wrap(
                &original.text.content,
                directive.max_width_px,
                &receipt.measurements,
            )
            .map_err(|e| invalid(&e.message))?;
            if wrapped != receipt.wrapped_text {
                return Err(invalid("Subtitle wrap differs from greedy observations"));
            }
            let layout = &raster.layout;
            crate::classic_caption_frame::verify_line_widths(
                &wrapped,
                &receipt.measurements,
                &layout.lines,
            )
            .map_err(|e| invalid(&e.message))?;
            let (left, top, width, height) = match &layout.background {
                Some(bg) => (bg.left, bg.top, bg.width, bg.height),
                None => {
                    let left = match original.text.align {
                        SceneTextAlign::Left => 0.0,
                        SceneTextAlign::Right => -layout.block.max_width,
                        SceneTextAlign::Center => -layout.block.max_width / 2.0,
                    };
                    (
                        left,
                        -layout.block.height / 2.0,
                        layout.block.max_width,
                        layout.block.height,
                    )
                }
            };
            let left_margin = f64::from(canvas.width) * placement.left_margin_ratio;
            let right_margin = f64::from(canvas.width) * placement.right_margin_ratio;
            let margin = f64::from(canvas.height) * placement.vertical_margin_ratio;
            let x = match original.text.align {
                SceneTextAlign::Left => left_margin - left,
                SceneTextAlign::Right => f64::from(canvas.width) - right_margin - (left + width),
                SceneTextAlign::Center => {
                    left_margin + (f64::from(canvas.width) - left_margin - right_margin) / 2.
                        - (left + width / 2.)
                }
            };
            let y = match placement.vertical_align {
                2 => margin - top,
                1 => f64::from(canvas.height) / 2. - (top + height / 2.),
                _ => f64::from(canvas.height) - margin - (top + height),
            };
            // Reprepare only the sampled target after applying application-owned
            // wrapping/placement. V2 checks every font, style and layout fact.
            let mut graph = frame.graph().clone();
            let target = &mut graph.layers[0];
            let SampledContent::Text { text } = &mut target.content else {
                return Err(invalid("Subtitle target partition changed"));
            };
            text.content = wrapped.clone();
            let visual = target
                .visual
                .as_mut()
                .ok_or_else(|| invalid("Subtitle visual missing"))?;
            visual.transform.position_x_px = x - f64::from(canvas.width) / 2.0;
            visual.transform.position_y_px = y - f64::from(canvas.height) / 2.0;
            let next =
                prepare_text_frame(&graph).map_err(|e| EditorError::new(e.code, e.message))?;
            crate::classic_text_frame::validate_layout(&next.texts()[0], raster)
                .map_err(|e| invalid(&e.message))?;
            let ElementContent::Text { text } = &mut element.content else {
                unreachable!()
            };
            text.content = wrapped;
            let transform = &mut element.visual.as_mut().unwrap().transform;
            transform.position_x_px = x - f64::from(canvas.width) / 2.0;
            transform.position_y_px = y - f64::from(canvas.height) / 2.0;
        }
        cues.push(SubtitleTextCuePlan {
            timeline_range: element.timeline_range.clone(),
            layer: layer.clone(),
            directive,
        });
    }
    let transaction = if request.observations.is_some() {
        let apply = ApplySceneProject {
            project_id: request.project_id.clone(),
            expected_revision: request.expected_revision.clone(),
            idempotency_key: request.idempotency_key.clone(),
            label: "Create subtitle text".into(),
            operations: vec![ProjectOperation::SequencePut {
                sequence: sequence.clone(),
            }],
        };
        crate::scene::plan(&prepared.snapshot, &apply)?;
        Some(ApplyProject {
            branch_name: None,
            project_id: apply.project_id,
            expected_revision: apply.expected_revision,
            idempotency_key: apply.idempotency_key,
            label: apply.label,
            operations: vec![ProjectEditOperation::SequencePut { sequence }],
        })
    } else {
        None
    };
    let canvas = prepared.frames[0].identity();
    Ok(SubtitleTextPlanResult {
        measurement: None,
        revision: prepared.snapshot.revision.clone(),
        width: canvas.width,
        height: canvas.height,
        cues,
        transaction,
        selection: Some(TimelineTarget {
            track_id: prepared.track_id.clone(),
            element_id: prepared.ids[0].clone(),
        }),
        warnings: prepared.warnings.clone(),
    })
}
#[cfg(test)]
mod tests;
