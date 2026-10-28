//! Scene frame binding on the original fixed consumer session.
use super::fonts::CachedTextCoverage;
use super::*;
use opencut_api_contracts::visual_preview::*;
use opencut_contracts::Time;
use opencut_editor_application::scene::SceneRead;
use opencut_editor_application::{
    classic_scene_frame::admit_scene_frame, font_import::FontImportPolicy,
    visual_preview::PreparedPicture, visual_preview::validate_preparation_time,
};
use opencut_native_services::classic_visual::VerifiedClassicTextResources;
pub(crate) fn scene_picture(
    host: &mut Automation,
    input: VisualPreviewFrame,
) -> Result<VisualPreviewFrameResult, EditorError> {
    scene_picture_inner(host, input, None, None).map(|result| result.0)
}
pub(crate) fn edit_draft_picture(
    host: &mut Automation,
    input: opencut_api_contracts::visual_edit_draft::VisualEditDraftFrame,
) -> Result<opencut_api_contracts::visual_edit_draft::VisualEditDraftFrameResult, EditorError> {
    if input.draft_id.is_empty()
        || input.draft_id.len() > 128
        || input.draft_id.chars().any(char::is_control)
    {
        return Err(EditorError::new(
            "INVALID_ARGUMENT",
            "Visual edit draft identity is invalid",
        ));
    }
    let (frame, digest) = scene_picture_inner(
        host,
        input.frame,
        Some(&input.transaction),
        Some(input.intent),
    )?;
    Ok(
        opencut_api_contracts::visual_edit_draft::VisualEditDraftFrameResult {
            draft_id: input.draft_id,
            transaction_digest: digest.ok_or_else(|| error("INTERNAL_ERROR"))?,
            frame,
        },
    )
}
fn scene_picture_inner(
    host: &mut Automation,
    mut input: VisualPreviewFrame,
    draft: Option<&opencut_contracts::project_public::ApplyProject>,
    intent: Option<opencut_contracts::visual_edit_draft::VisualEditDraftIntent>,
) -> Result<(VisualPreviewFrameResult, Option<String>), EditorError> {
    use opencut_editor_application::classic_visual::{
        ClassicVisualResources, ClassicVisualRgbaSink,
    };
    use opencut_native_services::{
        classic_visual::VideoResourceCache, native_visual::NativeScenePreview,
    };
    use std::time::Duration;
    let e = entry(&host.database, &input.session.session_id)?;
    authorize(host, &e, true)?;
    let view = {
        let state = lock(&e.state)?;
        state
            .machine
            .verify_visual_frame(&input.session, host.caller.principal_id(), now())?;
        state.machine.snapshot(now())
    };
    let _draft_guard = if draft.is_some() {
        Some(
            e.transform_draft_gate
                .try_lock()
                .map_err(|lock_error| match lock_error {
                    std::sync::TryLockError::WouldBlock => EditorError::new(
                        "PREVIEW_DRAFT_BUSY",
                        "The previous transform draft is still finishing",
                    ),
                    std::sync::TryLockError::Poisoned(_) => error("INTERNAL_ERROR"),
                })?,
        )
    } else {
        None
    };
    if let Some(transaction) = draft {
        if input.prepare_ahead
            || input.resolution != opencut_contracts::export_video::RasterResolution::Source
            || view.state != opencut_api_contracts::audio_preview::AudioPreviewState::Paused
            || transaction.project_id != e.request.project_id
            || transaction.expected_revision != e.request.revision
            || input
                .time
                .as_ref()
                .is_none_or(|time| !same_consumer_time(time, &view.presentation_time))
        {
            return Err(EditorError::new(
                "INVALID_ARGUMENT",
                "Visual edit draft requires the original paused scene and exact consumer time",
            ));
        }
    }
    if input.prepare_ahead
        && input.time.as_ref().is_some_and(|time| {
            opencut_time::RationalTime::try_from(time)
                .ok()
                .zip(opencut_time::RationalTime::try_from(&view.presentation_time).ok())
                .is_some_and(|(time, consumer)| time.cmp_exact(consumer).is_lt())
        })
    {
        input.time = Some(view.presentation_time.clone());
    }
    let time = input.time.as_ref().unwrap_or(&view.presentation_time);
    validate_preparation_time(time, &view.presentation_time, input.prepare_ahead)?;
    let mut selected = host
        .app
        .fixed_scene_text_frame_snapshot(&e.request.project_id, &e.request.revision)?;
    let mut digest = None;
    if let Some(transaction) = draft {
        let (snapshot, identity) = opencut_editor_application::visual_edit_draft::prepare(
            &host.app,
            transaction,
            &e.request.sequence_id,
            time,
            intent.ok_or_else(|| {
                EditorError::new(
                    "INVALID_ARGUMENT",
                    "Visual edit draft requires its explicit edit intent",
                )
            })?,
        )?;
        selected.snapshot = snapshot;
        digest = Some(identity);
    }
    let frame = if input.resolution == opencut_contracts::export_video::RasterResolution::Preview540
    {
        opencut_editor_application::scene_frame_jobs::prepare_preview(
            &selected.snapshot,
            &e.request.sequence_id,
            time,
        )?
    } else {
        opencut_editor_application::scene_frame_jobs::prepare(
            &selected.snapshot,
            &e.request.sequence_id,
            time,
        )?
    };
    let (policy, picture_capacity) =
        opencut_editor_application::visual_preview::reserve_native_picture_memory(
            selected.processing_policy,
            input.retained_picture_bytes,
        )?;
    admit_scene_frame(&frame, &policy)?;
    let identity = frame.execution().execution().execution().identity();
    let (width, height) = (identity.width, identity.height);
    if input
        .hit_query
        .is_some_and(|point| point.x >= width || point.y >= height)
    {
        return Err(EditorError::new(
            "INVALID_ARGUMENT",
            "Hit point is outside the prepared picture",
        ));
    }
    let length = u32::try_from(u64::from(width) * u64::from(height) * 4)
        .map_err(|_| error("VISUAL_BUDGET_EXCEEDED"))?;
    let mut owner = e.visual.try_lock().map_err(|failure| match failure {
        std::sync::TryLockError::WouldBlock => EditorError::new(
            "PREVIEW_PICTURE_BUSY",
            "The previous native picture is still finishing",
        ),
        std::sync::TryLockError::Poisoned(_) => error("INTERNAL_ERROR"),
    })?;
    // A newer frame intent supersedes any unclaimed payload. Claimed readers
    // remain charged until drop and keep their exact scope fence.
    e.pixels.clear();
    let reservation = e.pixels.reserve(length, picture_capacity)?;
    let mut checkpoint = || {
        e.checkpoint(&host.app)?;
        lock(&e.state)?.machine.verify_visual_frame(
            &input.session,
            host.caller.principal_id(),
            now(),
        )
    };
    checkpoint()?;
    if owner.as_ref().is_some_and(|owner| {
        owner.scope != input.session
            || owner.policy != policy
            || owner.renderer.idle_expired()
            || owner.renderer.check().is_err()
    }) {
        owner.take();
        e.pixels.clear();
    }
    if owner.is_none() {
        *owner = Some(super::visual_owner::VisualOwner {
            scope: input.session.clone(),
            principal: host.caller.principal_id().into(),
            policy: policy.clone(),
            renderer: NativeScenePreview::new(
                crate::classic_visual_runtime::configured_scene(&policy)?,
                Duration::from_secs(30),
                &mut checkpoint,
            )?,
            resources: VideoResourceCache::default(),
        });
    }
    let current = owner.as_mut().expect("admitted native picture owner");
    let mut resources = VerifiedClassicTextResources::new_scene_frame_cached(
        host.app.asset_store_root(),
        &frame,
        &FontImportPolicy::default(),
        checkpoint,
        &mut current.resources,
    )?;
    let coverage = resources.coverage().to_vec();
    let prepared = Arc::new(PreparedPicture::new(frame, policy, coverage.clone())?);
    let handles = resources.handles();
    let fonts = resources.font_handles();
    let mut session = prepared.session()?;
    struct Pixels {
        bytes: Vec<u8>,
        length: usize,
    }
    impl ClassicVisualRgbaSink for Pixels {
        fn chunk(&mut self, offset: u64, bytes: &[u8]) -> Result<(), EditorError> {
            if offset != self.bytes.len() as u64 || bytes.len() > self.length - self.bytes.len() {
                return Err(error("INVALID_VISUAL_PLAN"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
    }
    let mut pixels = Pixels {
        bytes: Vec::new(),
        length: length as usize,
    };
    pixels
        .bytes
        .try_reserve_exact(length as usize)
        .map_err(|_| error("VISUAL_BUDGET_EXCEEDED"))?;
    let execution = current.renderer.render(
        &mut session,
        &mut resources,
        &handles,
        &fonts,
        &mut pixels,
        input.hit_query.map(|p| [p.x, p.y]),
    )?;
    resources.checkpoint()?;
    drop(session);
    drop(resources);
    if pixels.bytes.len() != length as usize {
        return Err(error("INVALID_VISUAL_PLAN"));
    }
    authorize(host, &e, true)?;
    if let Some(transaction) = draft {
        host.app.authorize_project(
            opencut_editor_application::scene::SceneCommitIntent::PublicApply(transaction),
        )?;
        if host
            .app
            .head_scene_source(&transaction.project_id)?
            .revision
            != transaction.expected_revision
        {
            return Err(EditorError::new(
                "REVISION_CONFLICT",
                "The transform draft head changed",
            ));
        }
    }
    let mut state = lock(&e.state)?;
    state
        .machine
        .verify_visual_frame(&input.session, host.caller.principal_id(), now())?;
    let consumer = state.machine.snapshot(now());
    if draft.is_some()
        && (consumer.state != AudioPreviewState::Paused
            || !same_consumer_time(prepared.time(), &consumer.presentation_time))
    {
        return Err(error("PREVIEW_GENERATION_CONFLICT"));
    }
    validate_preparation_time(
        prepared.time(),
        &consumer.presentation_time,
        input.prepare_ahead,
    )?;
    let ticket = reservation.publish(input.session.clone(), pixels.bytes)?;
    let descriptor = VisualPreviewPicture {
        picture_id: ticket.id.clone(),
        width,
        height,
        byte_length: ticket.length,
        content_hash: ticket.hash,
        max_retained_bytes: picture_capacity,
        renderer: execution.runtime.identity,
        peak_worker_rss_bytes: execution.quota.observed_peak_rss_bytes,
    };
    let hits = execution
        .hits
        .into_iter()
        .map(|hit| VisualPreviewHit {
            element_id: hit.element_id,
            alpha: hit.alpha,
        })
        .collect();
    let result = match prepared.rendered_result(
        &input,
        &consumer.presentation_time,
        &execution.receipt,
        descriptor,
        hits,
    ) {
        Ok(result) => result,
        Err(error) => {
            e.pixels.discard(&ticket.id);
            return Err(error);
        }
    };
    state.text_coverage = Some(CachedTextCoverage {
        session: input.session.clone(),
        coverage,
    });
    if draft.is_none() {
        state.remember_picture(CachedPicture {
            resolution: input.resolution,
            retained_picture_bytes: input.retained_picture_bytes,
            session: input.session,
            prepared,
        });
    }
    Ok((result, digest))
}

// A stopped timeline may name 0/1 while its paused audio consumer names 0/48000.
// Compare their exact instant at both admission and publication, never their
// serialization or a rounded floating-point timestamp. Invalid times fail closed.
fn same_consumer_time(time: &Time, consumer: &Time) -> bool {
    opencut_time::RationalTime::try_from(time)
        .ok()
        .zip(opencut_time::RationalTime::try_from(consumer).ok())
        .is_some_and(|(time, consumer)| time.cmp_exact(consumer).is_eq())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_consumer_fences_accept_equivalent_bases_but_reject_even_one_sample_drift() {
        let time = |ticks: &str, timebase| Time {
            ticks: ticks.into(),
            timebase,
        };
        for (frame, consumer) in [
            (time("0", 1), time("0", 48000)),
            (time("1", 2), time("24000", 48000)),
            (
                time("9223372036854775807", u32::MAX),
                time("9223372036854775807", u32::MAX),
            ),
        ] {
            assert!(same_consumer_time(&frame, &consumer));
            assert!(same_consumer_time(&consumer, &frame));
        }
        for (frame, consumer) in [
            (time("0", 1), time("1", 48000)),
            (time("1", 2), time("24001", 48000)),
            (time("1", 2), time("23999", 48000)),
            (time("invalid", 1), time("invalid", 1)),
            (time("0", 0), time("0", 0)),
        ] {
            assert!(!same_consumer_time(&frame, &consumer));
            assert!(!same_consumer_time(&consumer, &frame));
        }
    }
}
