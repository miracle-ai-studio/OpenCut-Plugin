use super::*;
use opencut_editor_application::classic_audio::{ClassicAudioResources, ClassicAudioSession};
use opencut_native_services::classic_audio::VerifiedClassicAudioResources;
use opencut_native_services::native_audio::{
    NativeAudioPreviewPcmBlock, NativeAudioPreviewProducer,
};
use opencut_sqlite_store::EditorApplication;
use std::time::Duration;

pub(super) struct BlockRequest {
    pub generation: u64,
    pub overlay_epoch: u64,
    pub start: u64,
    pub count: u32,
    pub reply: mpsc::Sender<Result<NativeAudioPreviewPcmBlock, EditorError>>,
}
pub(super) fn run(entry: Arc<Entry>, receiver: mpsc::Receiver<BlockRequest>) {
    let result = execute(&entry, receiver);
    if let Err(err) = result
        && let Ok(mut state) = lock(&entry.state)
        && !entry.shutdown.load(Ordering::Acquire)
        && !matches!(
            state.machine.snapshot(now()).state,
            AudioPreviewState::Stopped | AudioPreviewState::Failed
        )
    {
        let _ = state.machine.fail(&err.code, now());
        state.pictures.clear();
        state.text_coverage = None;
        state.resources.clear();
        entry.credit.notify_all();
    }
}
fn execute(entry: &Entry, receiver: mpsc::Receiver<BlockRequest>) -> Result<(), EditorError> {
    let app = EditorApplication::open_managed(&entry.database)?;
    entry.checkpoint(&app)?;
    let mut config = crate::classic_audio_runtime::preview_config()?;
    config.max_pcm_bytes = entry.policy.max_pcm_bytes;
    config.max_encoded_bytes = entry.policy.max_encoded_bytes;
    config.max_rss_bytes = entry.policy.max_rss_bytes;
    let mut session = ClassicAudioSession::new_preview(
        &entry.prepared,
        entry.policy.max_pcm_bytes,
        entry.policy.max_encoded_bytes,
    )?;
    let mut resources =
        VerifiedClassicAudioResources::new(app.asset_store_root(), entry.prepared.audio(), || {
            entry.checkpoint(&app)
        })?;
    let handles = resources.handles();
    let mut producer =
        NativeAudioPreviewProducer::start(&config, &mut session, &mut resources, &handles)?;
    let result = (|| {
        loop {
            producer.checkpoint(&mut session, &mut resources)?;
            let request = match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(request) => request,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            };
            let execution = block(
                entry,
                &app,
                &mut producer,
                &mut session,
                &mut resources,
                &request,
            );
            let fatal = execution
                .as_ref()
                .err()
                .filter(|e| {
                    !matches!(
                        e.code.as_str(),
                        "PREVIEW_GENERATION_CONFLICT" | "PREVIEW_NOT_PLAYING"
                    )
                })
                .cloned();
            let _ = request.reply.send(execution);
            if let Some(error) = fatal {
                return Err(error);
            }
        }
    })();
    // Stop waits for the managed process to really finish. Even a revoked
    // resource callback cannot release the CAS gate before process teardown.
    let stopped = producer.stop(&mut session, &mut resources);
    result.and(stopped)
}
fn block(
    entry: &Entry,
    app: &EditorApplication,
    producer: &mut NativeAudioPreviewProducer,
    session: &mut ClassicAudioSession<'_>,
    resources: &mut dyn ClassicAudioResources,
    request: &BlockRequest,
) -> Result<NativeAudioPreviewPcmBlock, EditorError> {
    entry.checkpoint(app)?;
    let (anchor, gain) = {
        let state = lock(&entry.state)?;
        if state.machine.generation() != request.generation
            || state.machine.overlay_epoch() != request.overlay_epoch
        {
            return Err(error("PREVIEW_GENERATION_CONFLICT"));
        }
        let Some((expected_start, max_count)) = state.machine.next_block(now())? else {
            return Err(error("PREVIEW_NOT_PLAYING"));
        };
        if request.start != expected_start || request.count == 0 || request.count > max_count {
            return Err(error("PREVIEW_NOT_PLAYING"));
        }
        (
            state.machine.actual_start_sample(),
            state.machine.temporary_gain(),
        )
    };
    session.set_preview_state(anchor, gain)?;
    let block = producer.next(
        session,
        resources,
        request.generation,
        request.overlay_epoch,
        request.start,
        request.count,
    )?;
    entry.checkpoint(app)?;
    lock(&entry.state)?.machine.produced(
        request.generation,
        request.overlay_epoch,
        request.start,
        request.count,
        now(),
    )?;
    Ok(block)
}
