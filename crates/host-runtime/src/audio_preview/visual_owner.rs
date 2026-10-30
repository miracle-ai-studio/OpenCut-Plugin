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

/// The Entry fixes project/revision. Only scratch from this exact live owner
/// and lease may cross a generation; prepared pixels retain their full scope.
pub(super) fn same_lease(
    previous: &AudioPreviewStream,
    principal: &str,
    next: &AudioPreviewStream,
    caller: &str,
) -> bool {
    principal == caller
        && previous.owner_id == principal
        && next.owner_id == caller
        && previous.session_id == next.session_id
        && previous.lease_id == next.lease_id
}
fn scope(view: &AudioPreviewSession) -> AudioPreviewStream {
    AudioPreviewStream {
        session_id: view.session_id.clone(),
        generation: view.generation.clone(),
        overlay_epoch: view.overlay_epoch.clone(),
        owner_id: view.owner_id.clone(),
        lease_id: view.lease_id.clone(),
    }
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

/// Maintenance owns a weak session reference. A generation change retires old
/// pixels/readers; worker scratch stays bounded by the same live owner/lease.
/// No process retirement waits while holding the sound-state mutex.
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
                    let current = owner.as_ref().and_then(|owner| {
                        let state = lock(&entry.state).ok()?;
                        let current = scope(&state.machine.snapshot(now()));
                        (same_lease(&owner.scope, &owner.principal, &current, &owner.principal)
                            && state
                                .machine
                                .verify_visual_frame(&current, &owner.principal, now())
                                .is_ok())
                        .then_some(current)
                    });
                    let retire = terminal
                        || !authority
                        || owner.as_ref().is_some_and(|owner| {
                            owner.renderer.idle_expired()
                                || owner.renderer.check().is_err()
                                || current.is_none()
                        });
                    if retire {
                        owner.take();
                        entry.pixels.clear();
                        entry.readers.clear();
                    } else if owner
                        .as_ref()
                        .zip(current.as_ref())
                        .is_some_and(|(owner, current)| owner.scope != *current)
                    {
                        // These artifacts grant presentation, unlike source
                        // descriptors/decoders which reauthorize every use.
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
    fn scratch_reuse_is_limited_to_one_principal_session_and_lease() {
        let previous = AudioPreviewStream {
            session_id: "session".into(),
            generation: "2".into(),
            overlay_epoch: "0".into(),
            owner_id: "owner".into(),
            lease_id: "lease".into(),
        };
        let mut next = previous.clone();
        next.generation = "3".into();
        next.overlay_epoch = "1".into();
        assert!(same_lease(&previous, "owner", &next, "owner"));
        for field in ["session", "lease", "owner"] {
            let mut other = next.clone();
            match field {
                "session" => other.session_id = "other".into(),
                "lease" => other.lease_id = "other".into(),
                _ => other.owner_id = "other".into(),
            }
            assert!(!same_lease(&previous, "owner", &other, "owner"));
        }
        assert!(!same_lease(&previous, "owner", &next, "other"));
        assert!(!same_lease(&previous, "other", &next, "owner"));
    }

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
