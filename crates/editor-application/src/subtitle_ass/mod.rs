//! Static ASS authoring is an ordinary atomic V4 text edit. The parser never
//! discovers fonts; exact project resources and explicit aliases own binding.
pub use crate::subtitle_measurement::MeasuredSubtitleAssRequest as SubtitleAssPlanRequest;
mod parse;
use opencut_composition::{CompositionScope, scene::*};
use opencut_contracts::{
    EditorError, Time, project_public::*, scene::*, scene_ops::*, subtitle_ass::*,
    text_authoring::TextTrackTarget, text_frame::TextLayerPlan, timeline::TimelineTarget,
};
use parse::*;
use sha2::{Digest, Sha256};
#[derive(Clone, Debug)]
pub struct PreparedSubtitleAss {
    snapshot: EditorSnapshot,
    request: SubtitleAssPlanRequest,
    sequence: Scene,
    track_id: String,
    frames: Vec<PreparedTextFrame>,
    ids: Vec<String>,
    anchors: Vec<u8>,
    width: u32,
    height: u32,
    requirements: Vec<SubtitleAssFontRequirement>,
    warnings: Vec<String>,
}
impl PreparedSubtitleAss {
    pub fn frames(&self) -> &[PreparedTextFrame] {
        &self.frames
    }
}
fn invalid(s: &str) -> EditorError {
    EditorError::new("INVALID_SUBTITLE_ASS_PLAN", s)
}
fn unsupported(s: &str) -> EditorError {
    EditorError::new("SUBTITLE_ASS_UNSUPPORTED", s)
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn identity(s: &str) -> Result<(), EditorError> {
    if s.trim().is_empty() || s.len() > 128 || s.chars().any(char::is_control) {
        return Err(invalid("ASS identities must contain 1..128 bounded bytes"));
    }
    Ok(())
}
fn zero() -> Time {
    Time {
        ticks: "0".into(),
        timebase: 1,
    }
}
fn font_weight(weight: FontWeight) -> u16 {
    if weight == FontWeight::Bold { 700 } else { 400 }
}
fn same_face(requirement: &SubtitleAssFontRequirement, font: &FontResource) -> bool {
    font.weight == font_weight(requirement.weight)
        && font.style == requirement.style
        && font.face_index == 0
}
fn size(project: &EditorDocument, sequence: &str) -> Result<CanvasSize, EditorError> {
    match &project.settings {
        SettingsProfile::ClassicGlobal { settings } => Ok(settings.canvas_size),
        SettingsProfile::LegacyPerScene { formats } => formats
            .iter()
            .find(|f| f.sequence_id == sequence)
            .map(|f| f.canvas_size)
            .ok_or_else(|| invalid("ASS destination canvas is missing")),
    }
}

pub fn prepare_with_checkpoint(
    snapshot: &EditorSnapshot,
    request: &SubtitleAssPlanRequest,
    checkpoint: &mut impl FnMut() -> Result<(), EditorError>,
) -> Result<PreparedSubtitleAss, EditorError> {
    checkpoint()?;
    crate::require_idempotency_key(&request.idempotency_key)?;
    identity(&request.project_id)?;
    identity(&request.sequence_id)?;
    if snapshot.project.id != request.project_id || snapshot.revision != request.expected_revision {
        return Err(crate::revision_conflict(
            &request.expected_revision,
            &snapshot.revision,
        ));
    }
    opencut_editor_core::scene::validate_project_scene(&snapshot.project)?;
    let parsed = parse(&request.content)?;
    let mut sequence = snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == request.sequence_id)
        .cloned()
        .ok_or_else(|| invalid("ASS destination scene is missing"))?;
    let track_id = match &request.track {
        TextTrackTarget::Existing { track_id } | TextTrackTarget::New { track_id } => {
            track_id.clone()
        }
    };
    identity(&track_id)?;
    let index =
        match &request.track {
            TextTrackTarget::Existing { .. } => sequence
                .tracks
                .iter()
                .position(|t| t.id == track_id)
                .ok_or_else(|| invalid("ASS destination text track is missing"))?,
            TextTrackTarget::New { .. } => {
                sequence.tracks.push(SceneTrack {
                    id: track_id.clone(),
                    name: "ASS subtitles".into(),
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
    let track = &sequence.tracks[index];
    if track.locked {
        return Err(EditorError::new(
            "TRACK_LOCKED",
            "ASS destination track is locked",
        ));
    }
    if track.kind != SceneTrackKind::Text
        || !track.enabled
        || track.opacity != 1.0
        || !track.channels.is_empty()
    {
        return Err(invalid(
            "ASS destination must be an enabled ordinary text track",
        ));
    }
    // Even unresolved imports must not promise a supported edit onto a colliding
    // track ID or unsupported source geometry.
    let mut check = snapshot.project.clone();
    *check
        .sequences
        .iter_mut()
        .find(|s| s.id == sequence.id)
        .unwrap() = sequence.clone();
    opencut_editor_core::scene::validate_project_scene(&check)?;
    let canvas = size(&snapshot.project, &request.sequence_id)?;
    let sx = f64::from(canvas.width) / f64::from(parsed.width);
    let sy = f64::from(canvas.height) / f64::from(parsed.height);
    let mut requirements = Vec::<SubtitleAssFontRequirement>::new();
    for cue in &parsed.cues {
        checkpoint()?;
        if cue.wrap_style != 2 {
            return Err(unsupported(
                "ASS smart/end-of-line wrapping (q0/q1/q3) has no faithful layout implementation",
            ));
        }
        if cue.style.scale_x != cue.style.scale_y
            && cue.style.rotation_degrees.rem_euclid(360.0) != 0.0
        {
            return Err(unsupported(
                "ASS rotates after nonuniform font scale; Classic rotates before scale and has no faithful skew mapping",
            ));
        }
        if sx != sy && cue.style.rotation_degrees.rem_euclid(360.0) != 0.0 {
            return Err(unsupported(
                "ASS rotation with nonuniform PlayRes scaling requires unsupported skew",
            ));
        }
        if !requirements.iter().any(|r| {
            r.family == cue.style.font_family
                && r.weight == cue.style.font_weight
                && r.style == cue.style.font_style
        }) {
            requirements.push(SubtitleAssFontRequirement {
                family: cue.style.font_family.clone(),
                weight: cue.style.font_weight,
                style: cue.style.font_style,
                resolved_font_id: None,
            })
        }
    }
    if request.font_bindings.len() > requirements.len() {
        return Err(invalid(
            "ASS font bindings include duplicate or unrelated requests",
        ));
    }
    let mut warnings=vec!["ASS font metrics and line layout use the fixed Classic backend; interchange does not establish identical external ASS renderer pixels".into()];
    let mut used = vec![false; request.font_bindings.len()];
    for requirement in &mut requirements {
        let bindings = request
            .font_bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                b.family == requirement.family
                    && b.weight == requirement.weight
                    && b.style == requirement.style
            })
            .collect::<Vec<_>>();
        if bindings.len() > 1 {
            return Err(invalid("Duplicate ASS font binding"));
        }
        if let Some((i, binding)) = bindings.first() {
            identity(&binding.font_id)?;
            used[*i] = true;
            let font = snapshot
                .project
                .fonts
                .iter()
                .find(|f| f.id == binding.font_id)
                .ok_or_else(|| invalid("ASS bound font is not in the fixed project"))?;
            if !same_face(requirement, font) {
                return Err(invalid(
                    "ASS binding must preserve exact weight/style and static face",
                ));
            }
            requirement.resolved_font_id = Some(font.id.clone());
            if font.family != requirement.family {
                warnings.push(format!(
                    "Explicit font alias: {} -> {} ({}, {:?})",
                    requirement.family, font.family, font.weight, font.style
                ));
            }
        } else {
            let matches = snapshot
                .project
                .fonts
                .iter()
                .filter(|font| same_face(requirement, font) && font.family == requirement.family)
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Err(invalid(
                    "ASS family matches multiple pinned resources; select an explicit binding",
                ));
            }
            if let Some(font) = matches.first() {
                requirement.resolved_font_id = Some(font.id.clone())
            }
        }
    }
    if used.iter().any(|used| !*used) {
        return Err(invalid("Unrelated ASS font binding"));
    }
    let mut fixed = request.clone();
    fixed.observations = None;
    let mut prepared = PreparedSubtitleAss {
        snapshot: snapshot.clone(),
        request: fixed,
        sequence,
        track_id,
        frames: vec![],
        ids: vec![],
        anchors: vec![],
        width: canvas.width,
        height: canvas.height,
        requirements,
        warnings,
    };
    if prepared
        .requirements
        .iter()
        .any(|r| r.resolved_font_id.is_none())
    {
        if request.observations.is_some() {
            return Err(invalid(
                "ASS observations cannot resolve a missing pinned font",
            ));
        }
        return Ok(prepared);
    }
    for (i, cue) in parsed.cues.into_iter().enumerate() {
        checkpoint()?;
        let font_id = prepared
            .requirements
            .iter()
            .find(|r| {
                r.family == cue.style.font_family
                    && r.weight == cue.style.font_weight
                    && r.style == cue.style.font_style
            })
            .unwrap()
            .resolved_font_id
            .clone()
            .unwrap();
        let horizontal = (cue.style.alignment - 1) % 3;
        let vertical = (cue.style.alignment - 1) / 3;
        let anchor = cue.position.unwrap_or(AssPosition {
            x: match horizontal {
                0 => f64::from(cue.margins.left),
                1 => f64::from(parsed.width) / 2.0,
                _ => f64::from(parsed.width) - f64::from(cue.margins.right),
            },
            y: match vertical {
                0 => f64::from(parsed.height) - f64::from(cue.margins.vertical),
                1 => f64::from(parsed.height) / 2.0,
                _ => f64::from(cue.margins.vertical),
            },
        });
        let id = crate::content_id(
            "ass-text",
            format!(
                "{}:{}:{}:{}",
                request.project_id, request.sequence_id, request.idempotency_key, i
            )
            .as_bytes(),
        );
        let text = Text {
            content: cue.text,
            font: FontBinding::Locked { font_id },
            font_weight: cue.style.font_weight,
            font_style: cue.style.font_style,
            decoration: cue.style.decoration,
            align: match horizontal {
                0 => SceneTextAlign::Left,
                1 => SceneTextAlign::Center,
                _ => SceneTextAlign::Right,
            },
            font_size: FontSize::CanvasPixels {
                value: cue.style.font_size_px * sy,
            },
            color: cue.style.color,
            letter_spacing_px: cue.style.letter_spacing_px * sy,
            line_height_multiplier: 1.0,
            background: TextBackground {
                enabled: false,
                color: Color {
                    encoding: ColorEncoding::Srgb,
                    red: 0.0,
                    green: 0.0,
                    blue: 0.0,
                    alpha: 0.0,
                },
                corner_radius_percent: 0.0,
                padding_x_at_size15: 0.0,
                padding_y_at_size15: 0.0,
                offset_x_px: 0.0,
                offset_y_px: 0.0,
            },
        };
        prepared.sequence.tracks[index]
            .elements
            .push(TimelineElement {
                id: id.clone(),
                name: format!("ASS subtitle {}", i + 1),
                timeline_range: cue.timeline_range,
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
                        position_x_px: anchor.x * sx - f64::from(canvas.width) / 2.0,
                        position_y_px: anchor.y * sy - f64::from(canvas.height) / 2.0,
                        scale_x: cue.style.scale_x * sx / sy,
                        scale_y: cue.style.scale_y,
                        rotation_degrees: cue.style.rotation_degrees,
                    },
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    effects: vec![],
                    masks: vec![],
                }),
            });
        prepared.ids.push(id);
        prepared.anchors.push(cue.style.alignment);
    }
    let mut candidate = snapshot.project.clone();
    *candidate
        .sequences
        .iter_mut()
        .find(|s| s.id == request.sequence_id)
        .unwrap() = prepared.sequence.clone();
    opencut_editor_core::scene::validate_project_scene(&candidate)?;
    let candidate = crate::scene::plan(
        snapshot,
        &ApplySceneProject {
            project_id: request.project_id.clone(),
            expected_revision: request.expected_revision.clone(),
            idempotency_key: request.idempotency_key.clone(),
            label: "Import ASS text".into(),
            operations: vec![ProjectOperation::SequencePut {
                sequence: prepared.sequence.clone(),
            }],
        },
    )?;
    let candidate = EditorSnapshot {
        project: candidate,
        revision: snapshot.revision.clone(),
    };
    for id in &prepared.ids {
        checkpoint()?;
        let element = prepared.sequence.tracks[index]
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
        sampled.layers.retain(|layer| &layer.element_id == id);
        sampled.fonts.retain(|font| font.id == target.font.id);
        sampled.resources.clear();
        sampled.transitions.clear();
        prepared
            .frames
            .push(prepare_text_frame(&sampled).map_err(|e| EditorError::new(e.code, e.message))?);
    }
    Ok(prepared)
}

pub fn finish(
    prepared: &PreparedSubtitleAss,
    request: &SubtitleAssPlanRequest,
    layers: &[TextLayerPlan],
) -> Result<SubtitleAssPlanResult, EditorError> {
    let mut fixed = request.clone();
    fixed.observations = None;
    if fixed != prepared.request || layers.len() != prepared.frames.len() {
        return Err(invalid("ASS fixed request or layers changed"));
    }
    if request
        .observations
        .as_ref()
        .is_some_and(|o| o.len() != layers.len() || layers.is_empty())
    {
        return Err(invalid(
            "ASS observations must cover each resolved cue exactly once",
        ));
    }
    let mut sequence = prepared.sequence.clone();
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
                "ASS native text/font/transform differs from fixed candidate",
            ));
        }
        let element = sequence
            .tracks
            .iter_mut()
            .find(|t| t.id == prepared.track_id)
            .unwrap()
            .elements
            .iter_mut()
            .find(|e| e.id == layer.element_id)
            .unwrap();
        let mut final_layer = layer.clone();
        if let Some(observations) = &request.observations {
            let raster = &observations[i];
            if raster.element_id != layer.element_id
                || raster.width != prepared.width
                || raster.height != prepared.height
            {
                return Err(invalid("ASS raster identity/canvas differs"));
            }
            crate::classic_text_frame::validate_layout(original, raster)
                .map_err(|e| invalid(&e.message))?;
            // TextV4's local box is vertically centered. Convert ASS's numpad
            // anchor to the same center using Classic T*S*R. Nonuniform ASS
            // scale plus rotation was rejected because ASS uses T*R*S.
            let delta = match (prepared.anchors[i] - 1) / 3 {
                0 => -raster.layout.block.height / 2.0,
                1 => 0.0,
                _ => raster.layout.block.height / 2.0,
            };
            let transform = &mut element.visual.as_mut().unwrap().transform;
            let angle = transform.rotation_degrees.to_radians();
            transform.position_x_px -= angle.sin() * delta * transform.scale_x;
            transform.position_y_px += angle.cos() * delta * transform.scale_y;
            final_layer.text.transform.center_x =
                transform.position_x_px + f64::from(prepared.width) / 2.0;
            final_layer.text.transform.center_y =
                transform.position_y_px + f64::from(prepared.height) / 2.0;
        }
        cues.push(SubtitleAssCuePlan {
            timeline_range: element.timeline_range.clone(),
            layer: final_layer,
        });
    }
    let transaction = if request.observations.is_some() {
        let apply = ApplySceneProject {
            project_id: request.project_id.clone(),
            expected_revision: request.expected_revision.clone(),
            idempotency_key: request.idempotency_key.clone(),
            label: "Import ASS text".into(),
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
    Ok(SubtitleAssPlanResult {
        measurement: None,
        revision: prepared.snapshot.revision.clone(),
        width: prepared.width,
        height: prepared.height,
        font_requirements: prepared.requirements.clone(),
        cues,
        transaction,
        selection: prepared.ids.first().map(|id| TimelineTarget {
            track_id: prepared.track_id.clone(),
            element_id: id.clone(),
        }),
        warnings: prepared.warnings.clone(),
    })
}
#[cfg(test)]
mod tests;
