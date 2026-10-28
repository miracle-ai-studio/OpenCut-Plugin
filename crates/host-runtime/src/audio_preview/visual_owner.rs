use super::*;
use opencut_editor_application::media_frame_jobs::LocalVisualExecutionPolicy;
use opencut_native_services::{
    classic_visual::VideoResourceCache, native_visual::NativeScenePreview,
};
use std::{sync::Weak, time::Duration};

pub(super) struct VisualOwner {
    pub scope: AudioPreviewStream,
    pub principal: String,
    pub policy: LocalVisualExecutionPolicy,
    pub renderer: NativeScenePreview,
    pub resources: VideoResourceCache,
}

/// Maintenance owns a weak session reference. Retiring a stopped, superseded,
/// revoked or idle worker never waits while holding the sound-state mutex.
pub(super) fn maintain(entry: &Arc<Entry>) -> Result<(), EditorError> {
    let weak: Weak<Entry> = Arc::downgrade(entry);
    let database = entry.database.clone();
    let (started, ready) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("opencut-picture-owner".into())
        .spawn(move || {
            let application = match opencut_sqlite_store::EditorApplication::open_managed(database)
            {
                Ok(application) => {
                    let _ = started.send(Ok(()));
                    application
                }
                Err(error) => {
                    let _ = started.send(Err(error));
                    return;
                }
            };
            loop {
                let Some(entry) = weak.upgrade() else {
                    break;
                };
                let view = entry.view();
                let terminal = entry.shutdown.load(Ordering::Acquire)
                    || view.as_ref().map_or(true, |v| {
                        matches!(
                            v.state,
                            AudioPreviewState::Stopped | AudioPreviewState::Failed
                        )
                    });
                let authority = entry.checkpoint(&application).is_ok();
                if let Ok(mut owner) = entry.visual.try_lock() {
                    let retire = terminal
                        || !authority
                        || owner.as_ref().is_some_and(|owner| {
                            owner.renderer.idle_expired()
                                || owner.renderer.check().is_err()
                                || lock(&entry.state).map_or(true, |state| {
                                    state
                                        .machine
                                        .verify_visual_frame(&owner.scope, &owner.principal, now())
                                        .is_err()
                                })
                        });
                    if retire {
                        owner.take();
                        entry.pixels.clear();
                    }
                    if terminal {
                        break;
                    }
                }
                drop(entry);
                std::thread::sleep(Duration::from_millis(250));
            }
        })
        .map_err(|_| error("IO_ERROR"))?;
    ready
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| error("IO_ERROR"))??;
    Ok(())
}
