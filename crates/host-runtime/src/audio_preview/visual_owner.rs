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
    pub source_resources: crate::source_preview_runtime::SourceCache,
}

/// Allow one request to wait for an in-flight picture or retiring generation.
/// The waiter owns no pixels, CAS handles or extra renderer. Do not hold the
/// sound-state mutex while waiting; the supplied checkpoint briefly reacquires
/// it to preserve live authority, lease, generation and shutdown fences.
pub(super) fn acquire<'a, T>(
    owner: &'a Mutex<T>,
    waiting: &AtomicBool,
    timeout: Duration,
    checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
) -> Result<MutexGuard<'a, T>, EditorError> {
    fn busy() -> EditorError {
        EditorError::new(
            "PREVIEW_PICTURE_BUSY",
            "The previous native picture is still finishing",
        )
    }
    struct Waiter<'a>(&'a AtomicBool);
    impl Drop for Waiter<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    checkpoint()?;
    waiting
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| busy())?;
    let _waiter = Waiter(waiting);
    let began = Instant::now();
    loop {
        checkpoint()?;
        match owner.try_lock() {
            Ok(owner) => {
                // The prior renderer may have retired across a control change.
                checkpoint()?;
                return Ok(owner);
            }
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(error("INTERNAL_ERROR")),
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        let remaining = timeout.saturating_sub(began.elapsed());
        if remaining.is_zero() {
            return Err(busy());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
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
                        entry.readers.clear();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retiring_picture_hands_off_to_one_waiter_without_an_extra_owner() {
        let owner = Mutex::new(7);
        let waiting = AtomicBool::new(false);
        let held = owner.lock().unwrap();
        std::thread::scope(|threads| {
            let (ready, observed) = std::sync::mpsc::sync_channel(1);
            let owner = &owner;
            let waiting = &waiting;
            let waiter = threads.spawn(move || {
                let mut announced = false;
                let mut next = acquire(&owner, &waiting, Duration::from_secs(2), &mut || {
                    if waiting.load(Ordering::Acquire) && !announced {
                        ready.send(()).unwrap();
                        announced = true;
                    }
                    Ok(())
                })
                .unwrap();
                assert_eq!(*next, 7);
                *next = 8;
            });
            observed.recv_timeout(Duration::from_secs(2)).unwrap();
            let excess = acquire(&owner, &waiting, Duration::from_secs(2), &mut || Ok(()));
            assert_eq!(excess.unwrap_err().code, "PREVIEW_PICTURE_BUSY");
            drop(held);
            waiter.join().unwrap();
        });
        assert!(!waiting.load(Ordering::Acquire));
        assert_eq!(*owner.lock().unwrap(), 8);
    }

    #[test]
    fn queued_picture_preserves_live_fence_errors_and_releases_its_slot() {
        let owner = Mutex::new(());
        let waiting = AtomicBool::new(false);
        let _held = owner.lock().unwrap();
        for code in [
            "PREVIEW_GENERATION_CONFLICT",
            "PREVIEW_STOPPED",
            "FORBIDDEN",
        ] {
            let mut checks = 0;
            let result = acquire(&owner, &waiting, Duration::from_secs(2), &mut || {
                checks += 1;
                if checks >= 3 {
                    Err(EditorError::new(code, "live fence"))
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.unwrap_err().code, code);
            assert!(!waiting.load(Ordering::Acquire));
        }
    }

    #[test]
    fn timed_out_waiter_releases_capacity_and_acquisition_rechecks_authority() {
        let owner = Mutex::new(());
        let waiting = AtomicBool::new(false);
        let held = owner.lock().unwrap();
        let result = acquire(&owner, &waiting, Duration::from_millis(10), &mut || Ok(()));
        assert_eq!(result.unwrap_err().code, "PREVIEW_PICTURE_BUSY");
        assert!(!waiting.load(Ordering::Acquire));
        drop(held);
        let mut checks = 0;
        let result = acquire(&owner, &waiting, Duration::from_secs(2), &mut || {
            checks += 1;
            if checks >= 3 {
                Err(error("PREVIEW_GENERATION_CONFLICT"))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err().code, "PREVIEW_GENERATION_CONFLICT");
        assert!(!waiting.load(Ordering::Acquire));
        assert!(owner.try_lock().is_ok());
        assert!(acquire(&owner, &waiting, Duration::from_secs(2), &mut || Ok(())).is_ok());
    }
}
