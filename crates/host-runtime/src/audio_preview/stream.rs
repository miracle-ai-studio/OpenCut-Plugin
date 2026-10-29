use super::*;
use crate::transport_io::StreamRead;
use opencut_native_services::native_audio::NativeAudioPreviewPcmBlock;
use sha2::{Digest, Sha256};
use std::time::Duration;

/// One in-flight native command survives read timeouts. Polling a slow decoder
/// never sends a second command or advances the production cursor twice.
pub struct AudioPreviewReader {
    entry: Arc<Entry>,
    application: opencut_sqlite_store::EditorApplication,
    input: AudioPreviewStream,
    token: String,
    generation: u64,
    epoch: u64,
    sequence: u64,
    pending: Option<mpsc::Receiver<Result<NativeAudioPreviewPcmBlock, EditorError>>>,
}
impl AudioPreviewReader {
    pub(super) fn new(entry: Arc<Entry>, input: AudioPreviewStream) -> Result<Self, EditorError> {
        let application = opencut_sqlite_store::EditorApplication::open_managed(&entry.database)?;
        let generation = input
            .generation
            .parse()
            .map_err(|_| error("INVALID_ARGUMENT"))?;
        let epoch = input
            .overlay_epoch
            .parse()
            .map_err(|_| error("INVALID_ARGUMENT"))?;
        let token = new_identity();
        claim_stream(&entry.state, &entry.stream_claim, &input, &token)?;
        Ok(Self {
            entry,
            application,
            input,
            token,
            generation,
            epoch,
            sequence: 0,
            pending: None,
        })
    }
    fn fence(&self) -> Result<AudioPreviewSession, EditorError> {
        self.entry.checkpoint(&self.application)?;
        if self.entry.shutdown.load(Ordering::Acquire) {
            return Err(error("PREVIEW_STOPPED"));
        }
        let state = lock(&self.entry.state)?;
        state
            .machine
            .verify_stream(&self.input, &self.input.owner_id, now())?;
        Ok(state.machine.snapshot(now()))
    }
    pub fn checkpoint(&self) -> Result<(), EditorError> {
        self.fence().map(|_| ())
    }
    pub fn read(&mut self, timeout: Duration) -> Result<StreamRead, EditorError> {
        let view = self.fence()?;
        if self.pending.is_none() {
            let next = lock(&self.entry.state)?.machine.next_block(now())?;
            let Some((start, count)) = next else {
                if view.produced_through_sample == view.total_frames {
                    return Ok(StreamRead::End);
                }
                // Recheck under the same mutex used by controls, so a report
                // between the first check and waiting cannot lose its wakeup.
                wait_for_credit(
                    lock(&self.entry.state)?,
                    &self.entry.credit,
                    &self.entry.shutdown,
                    &self.input,
                    timeout,
                )?;
                // Authorization and generation remain live after every wake.
                self.fence()?;
                return Ok(StreamRead::Timeout);
            };
            let count = count.min(self.entry.prepared.block_frame_limit(start));
            let (reply, receiver) = mpsc::channel();
            let actor = lock(&self.entry.actor)?;
            let actor = actor.as_ref().ok_or_else(|| error("PREVIEW_STOPPED"))?;
            match actor.sender.try_send(producer::BlockRequest {
                generation: self.generation,
                overlay_epoch: self.epoch,
                start,
                count,
                reply,
            }) {
                Ok(()) => self.pending = Some(receiver),
                Err(mpsc::TrySendError::Full(_)) => return Ok(StreamRead::Timeout),
                Err(mpsc::TrySendError::Disconnected(_)) => return Err(error("PREVIEW_STOPPED")),
            }
        }
        let result = self
            .pending
            .as_ref()
            .expect("pending command")
            .recv_timeout(timeout);
        let block = match result {
            Ok(result) => {
                self.pending = None;
                result?
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(StreamRead::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(error("PREVIEW_STOPPED")),
        };
        let view = self.fence()?;
        if block.generation != self.generation
            || block.overlay_epoch != self.epoch
            || !(1..=4096).contains(&block.frame_count)
            || block
                .channels
                .iter()
                .any(|c| c.len() != block.frame_count as usize || c.iter().any(|v| !v.is_finite()))
        {
            return Err(error("ARTIFACT_VERIFICATION_FAILED"));
        }
        let mut payload = Vec::with_capacity(block.frame_count as usize * 8);
        for index in 0..block.frame_count as usize {
            for channel in &block.channels {
                payload.extend_from_slice(&channel[index].to_le_bytes());
            }
        }
        let header = AudioPreviewPcmHeader {
            pcm_stage: AudioPreviewPcmStage::Mastered,
            session_id: self.input.session_id.clone(),
            identity: view.identity,
            generation: self.input.generation.clone(),
            overlay_epoch: self.input.overlay_epoch.clone(),
            sequence: self.sequence.to_string(),
            start_sample: block.start_sample.to_string(),
            frame_count: block.frame_count,
            content_hash: format!("{:x}", Sha256::digest(&payload)),
        };
        let header = serde_json::to_vec(&header).map_err(|_| error("SERIALIZATION_ERROR"))?;
        if header.len() > 4096 || payload.len() > 32768 {
            return Err(error("PREVIEW_LIMIT"));
        }
        let mut bytes = Vec::with_capacity(12 + header.len() + payload.len());
        bytes.extend_from_slice(b"OCAP");
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&payload);
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| error("PREVIEW_LIMIT"))?;
        Ok(StreamRead::Chunk(bytes))
    }
}
/// Wait only while this exact stream has no consumer credit. The condvar
/// releases the state mutex; no database or CAS authorization lock is held.
fn wait_for_credit(
    state: MutexGuard<'_, SessionState>,
    credit: &Condvar,
    shutdown: &AtomicBool,
    input: &AudioPreviewStream,
    timeout: Duration,
) -> Result<bool, EditorError> {
    let validate = |state: &SessionState| {
        if shutdown.load(Ordering::Acquire) {
            return Err(error("PREVIEW_STOPPED"));
        }
        state.machine.verify_stream(input, &input.owner_id, now())?;
        Ok(())
    };
    validate(&state)?;
    let view = state.machine.snapshot(now());
    if state.machine.next_block(now())?.is_some()
        || view.produced_through_sample == view.total_frames
    {
        return Ok(true);
    }
    let (state, waited) = credit
        .wait_timeout(state, timeout.min(Duration::from_millis(200)))
        .map_err(|_| error("INTERNAL_ERROR"))?;
    validate(&state)?;
    Ok(!waited.timed_out())
}
fn claim_stream(
    state: &Mutex<SessionState>,
    claim: &Mutex<Option<(u64, String)>>,
    input: &AudioPreviewStream,
    token: &str,
) -> Result<(), EditorError> {
    let state = lock(state)?;
    state.machine.verify_stream(input, &input.owner_id, now())?;
    let generation = state.machine.generation();
    let mut claim = lock(claim)?;
    if claim.as_ref().is_some_and(|(g, _)| *g == generation) {
        return Err(error("PREVIEW_STREAM_ACTIVE"));
    }
    *claim = Some((generation, token.into()));
    Ok(())
}
fn abandon_stream(
    state: &Mutex<SessionState>,
    claim: &Mutex<Option<(u64, String)>>,
    generation: u64,
    token: &str,
    credit: &Condvar,
) {
    // Claim replacement and disconnect are atomic with generation controls.
    if let Ok(mut state) = lock(state)
        && let Ok(mut claim) = lock(claim)
        && claim.as_ref().is_some_and(|(_, current)| current == token)
    {
        let _ = state.machine.disconnect(generation, now());
        *claim = None;
        credit.notify_all();
    }
}
impl Drop for AudioPreviewReader {
    fn drop(&mut self) {
        abandon_stream(
            &self.entry.state,
            &self.entry.stream_claim,
            self.generation,
            &self.token,
            &self.entry.credit,
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    type Fixture = (
        Mutex<SessionState>,
        Mutex<Option<(u64, String)>>,
        AudioPreviewStream,
    );
    fn fixture() -> Fixture {
        let request = AudioPreviewCreate {
            project_id: "p".into(),
            revision: "r".into(),
            sequence_id: "s".into(),
            sample_rate: 48000,
            start_time: opencut_contracts::Time {
                ticks: "0".into(),
                timebase: 1,
            },
            idempotency_key: "create".into(),
        };
        let mut machine = AudioPreviewMachine::new(
            &request,
            "session".into(),
            "owner".into(),
            "lease".into(),
            480000,
            now(),
        )
        .unwrap();
        let view = machine.snapshot(now());
        let play = AudioPreviewControl {
            session_id: view.session_id,
            generation: view.generation,
            owner_id: view.owner_id,
            lease_id: view.lease_id,
            control_id: "play".into(),
            command: AudioPreviewCommand::Play,
        };
        let view = machine.control(&play, now()).unwrap().session;
        let input = AudioPreviewStream {
            session_id: view.session_id,
            generation: view.generation,
            overlay_epoch: view.overlay_epoch,
            owner_id: view.owner_id,
            lease_id: view.lease_id,
        };
        (
            Mutex::new(SessionState {
                machine,
                cache_plan: None,
                source_plan: None,
                authority: TrustedJobAuthority::LocalOwner,
                pictures: Vec::new(),
                text_coverage: None,
                resources: HashMap::new(),
            }),
            Mutex::new(None),
            input,
        )
    }
    #[test]
    fn delayed_old_opener_cannot_replace_current_generation_claim() {
        let (state, claim, old) = fixture();
        lock(&state)
            .unwrap()
            .machine
            .verify_stream(&old, "owner", now())
            .unwrap();
        // A control and new opener intervene after the host's first validation.
        let mut current = old.clone();
        let view = lock(&state)
            .unwrap()
            .machine
            .control(
                &AudioPreviewControl {
                    session_id: old.session_id.clone(),
                    generation: old.generation.clone(),
                    owner_id: old.owner_id.clone(),
                    lease_id: old.lease_id.clone(),
                    control_id: "seek".into(),
                    command: AudioPreviewCommand::Seek {
                        sequence_time: opencut_contracts::Time {
                            ticks: "1".into(),
                            timebase: 2,
                        },
                    },
                },
                now(),
            )
            .unwrap()
            .session;
        current.generation = view.generation;
        claim_stream(&state, &claim, &current, "new-stream").unwrap();
        assert_eq!(
            claim_stream(&state, &claim, &old, "late-old")
                .unwrap_err()
                .code,
            "PREVIEW_GENERATION_CONFLICT"
        );
        assert_eq!(lock(&claim).unwrap().as_ref().unwrap().1, "new-stream");
    }
    #[test]
    fn stale_drop_never_pauses_another_token_in_the_same_generation() {
        let (state, claim, input) = fixture();
        claim_stream(&state, &claim, &input, "current-stream").unwrap();
        abandon_stream(
            &state,
            &claim,
            input.generation.parse().unwrap(),
            "stale-token",
            &Condvar::new(),
        );
        assert_eq!(
            lock(&state).unwrap().machine.snapshot(now()).state,
            AudioPreviewState::Playing
        );
        assert_eq!(lock(&claim).unwrap().as_ref().unwrap().1, "current-stream");
        abandon_stream(
            &state,
            &claim,
            input.generation.parse().unwrap(),
            "current-stream",
            &Condvar::new(),
        );
        assert_eq!(
            lock(&state).unwrap().machine.snapshot(now()).state,
            AudioPreviewState::Paused
        );
        assert!(lock(&claim).unwrap().is_none());
    }
    fn fill_credit(state: &Mutex<SessionState>, input: &AudioPreviewStream) {
        let mut state = lock(state).unwrap();
        for start in (0
            ..opencut_editor_application::audio_preview::AUDIO_PREVIEW_MAX_BUFFERED_FRAMES)
            .step_by(4096)
        {
            state
                .machine
                .produced(
                    input.generation.parse().unwrap(),
                    input.overlay_epoch.parse().unwrap(),
                    start,
                    4096,
                    now(),
                )
                .unwrap();
        }
        assert!(state.machine.next_block(now()).unwrap().is_none());
    }
    fn control_machine(
        state: &mut SessionState,
        input: &AudioPreviewStream,
        command: AudioPreviewCommand,
    ) {
        state
            .machine
            .control(
                &AudioPreviewControl {
                    session_id: input.session_id.clone(),
                    generation: input.generation.clone(),
                    owner_id: input.owner_id.clone(),
                    lease_id: input.lease_id.clone(),
                    control_id: "thread-control".into(),
                    command,
                },
                now(),
            )
            .unwrap();
    }
    fn consume() -> AudioPreviewCommand {
        AudioPreviewCommand::ReportConsumed {
            sequence: "1".into(),
            consumed_frames: "4096".into(),
            output: None,
            playing: true,
            stalled: false,
        }
    }
    #[test]
    fn consumed_before_wait_is_not_lost_and_does_not_produce_pcm() {
        let (state, _, input) = fixture();
        fill_credit(&state, &input);
        let credit = Condvar::new();
        let shutdown = AtomicBool::new(false);
        // The consumer owns the predicate mutex before the waiter can inspect
        // it. Its notification precedes waiting; the predicate must suffice.
        std::thread::scope(|scope| {
            let mut guard = lock(&state).unwrap();
            let thread = scope.spawn(|| {
                wait_for_credit(
                    lock(&state).unwrap(),
                    &credit,
                    &shutdown,
                    &input,
                    Duration::from_millis(200),
                )
            });
            control_machine(&mut guard, &input, consume());
            credit.notify_all();
            drop(guard);
            assert!(thread.join().unwrap().unwrap());
        });
        let state = lock(&state).unwrap();
        assert_eq!(
            state.machine.next_block(now()).unwrap(),
            Some((131072, 4096))
        );
        assert_eq!(
            state.machine.snapshot(now()).produced_through_sample,
            "131072"
        );
    }
    #[test]
    fn consumed_during_wait_wakes_without_timeout_or_extra_credit() {
        let (state, _, input) = fixture();
        fill_credit(&state, &input);
        let credit = Condvar::new();
        let shutdown = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (ready, waiting) = mpsc::channel();
            let (state, credit, shutdown, input) = (&state, &credit, &shutdown, &input);
            let thread = scope.spawn(move || {
                let guard = lock(state).unwrap();
                ready.send(()).unwrap();
                wait_for_credit(guard, credit, shutdown, input, Duration::from_millis(200))
            });
            waiting.recv_timeout(Duration::from_secs(1)).unwrap();
            // Acquiring this mutex proves the waiter released it atomically
            // inside wait_timeout, rather than relying on a scheduling sleep.
            let mut guard = lock(state).unwrap();
            control_machine(&mut guard, input, consume());
            credit.notify_all();
            drop(guard);
            assert!(thread.join().unwrap().unwrap());
        });
        let state = lock(&state).unwrap();
        assert_eq!(
            state.machine.next_block(now()).unwrap(),
            Some((131072, 4096))
        );
        assert_eq!(
            state.machine.snapshot(now()).produced_through_sample,
            "131072"
        );
    }
    #[test]
    fn empty_credit_wait_cannot_create_a_block() {
        let (state, _, input) = fixture();
        fill_credit(&state, &input);
        wait_for_credit(
            lock(&state).unwrap(),
            &Condvar::new(),
            &AtomicBool::new(false),
            &input,
            Duration::ZERO,
        )
        .unwrap();
        let state = lock(&state).unwrap();
        assert!(state.machine.next_block(now()).unwrap().is_none());
        assert_eq!(
            state.machine.snapshot(now()).produced_through_sample,
            "131072"
        );
        assert_eq!(state.machine.snapshot(now()).consumed_frames, "0");
    }
    #[test]
    fn seek_or_stop_during_credit_wait_rejects_the_old_stream() {
        for command in [
            AudioPreviewCommand::Seek {
                sequence_time: opencut_contracts::Time {
                    ticks: "1".into(),
                    timebase: 2,
                },
            },
            AudioPreviewCommand::Stop,
        ] {
            let (state, _, input) = fixture();
            fill_credit(&state, &input);
            let credit = Condvar::new();
            let shutdown = AtomicBool::new(false);
            std::thread::scope(|scope| {
                let (ready, waiting) = mpsc::channel();
                let (state, credit, shutdown, input) = (&state, &credit, &shutdown, &input);
                let thread = scope.spawn(move || {
                    let guard = lock(state).unwrap();
                    ready.send(()).unwrap();
                    wait_for_credit(guard, credit, shutdown, input, Duration::from_millis(200))
                });
                waiting.recv_timeout(Duration::from_secs(1)).unwrap();
                let mut guard = lock(state).unwrap();
                control_machine(&mut guard, input, command);
                credit.notify_all();
                drop(guard);
                assert_eq!(
                    thread.join().unwrap().unwrap_err().code,
                    "PREVIEW_GENERATION_CONFLICT"
                );
            });
        }
    }
    #[test]
    fn shutdown_wakes_credit_wait_and_rejects_the_stream() {
        let (state, _, input) = fixture();
        fill_credit(&state, &input);
        let credit = Condvar::new();
        let shutdown = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (ready, waiting) = mpsc::channel();
            let (state, credit, shutdown, input) = (&state, &credit, &shutdown, &input);
            let thread = scope.spawn(move || {
                let guard = lock(state).unwrap();
                ready.send(()).unwrap();
                wait_for_credit(guard, credit, shutdown, input, Duration::from_millis(200))
            });
            waiting.recv_timeout(Duration::from_secs(1)).unwrap();
            let guard = lock(state).unwrap();
            shutdown.store(true, Ordering::Release);
            credit.notify_all();
            drop(guard);
            assert_eq!(thread.join().unwrap().unwrap_err().code, "PREVIEW_STOPPED");
        });
    }
}

#[cfg(test)]
mod picture_expiry_tests {
    use super::*;
    #[test]
    fn an_already_expired_session_releases_its_prepared_frame() {
        use opencut_contracts::{CreateProject, FrameRate, Time, scene::EditorSnapshot};
        use opencut_editor_application::scene::new_document;
        use opencut_job_execution::media_frame::LocalVisualExecutionPolicy;
        let start = Time {
            ticks: "0".into(),
            timebase: 48000,
        };
        let request = AudioPreviewCreate {
            project_id: "p".into(),
            revision: "r".into(),
            sequence_id: "s".into(),
            start_time: start.clone(),
            sample_rate: 48000,
            idempotency_key: "create".into(),
        };
        let machine = AudioPreviewMachine::new(
            &request,
            "session".into(),
            "owner".into(),
            "lease".into(),
            96000,
            0,
        )
        .unwrap();
        let view = machine.snapshot(0);
        let session = AudioPreviewStream {
            session_id: view.session_id,
            generation: view.generation,
            overlay_epoch: view.overlay_epoch,
            owner_id: view.owner_id,
            lease_id: view.lease_id,
        };
        let snapshot = EditorSnapshot {
            revision: "r".into(),
            project: new_document(&CreateProject {
                id: "p".into(),
                name: "Synthetic expiry".into(),
                sequence_id: "s".into(),
                width: 1,
                height: 1,
                fps: FrameRate {
                    numerator: 24,
                    denominator: 1,
                },
            })
            .unwrap(),
        };
        let policy = LocalVisualExecutionPolicy {
            max_frame_pixels: 1,
            max_decoded_pixels: 16,
            max_gpu_bytes: 1024,
            max_rgba_bytes: 1024,
            max_encoded_bytes: 1024,
            max_output_bytes: 1024,
            max_rss_bytes: 1024,
            deadline_ms: 1000,
        };
        let prepared = Arc::new(
            opencut_editor_application::visual_preview::PreparedPicture::new(
                opencut_editor_application::scene_frame_jobs::prepare(&snapshot, "s", &start)
                    .unwrap(),
                policy.clone(),
                vec![],
            )
            .unwrap(),
        );
        let weak = Arc::downgrade(&prepared);
        let mut state = SessionState {
            machine,
            cache_plan: None,
            source_plan: None,
            authority: TrustedJobAuthority::LocalOwner,
            pictures: vec![CachedPicture {
                resolution: opencut_contracts::export_video::RasterResolution::Source,
                retained_picture_bytes: 0,
                session: session.clone(),
                prepared,
            }],
            resources: HashMap::new(),
            text_coverage: Some(fonts::CachedTextCoverage {
                session: session.clone(),
                coverage: vec![],
            }),
        };
        assert!(weak.upgrade().is_some());
        for index in 1..=10 {
            let at = Time {
                ticks: index.to_string(),
                timebase: 24,
            };
            let prepared = Arc::new(
                opencut_editor_application::visual_preview::PreparedPicture::new(
                    opencut_editor_application::scene_frame_jobs::prepare(&snapshot, "s", &at)
                        .unwrap(),
                    policy.clone(),
                    vec![],
                )
                .unwrap(),
            );
            state.remember_picture(CachedPicture {
                resolution: opencut_contracts::export_video::RasterResolution::Source,
                retained_picture_bytes: 0,
                session: session.clone(),
                prepared,
            });
            assert!(state.pictures.len() <= 8);
        }
        assert!(weak.upgrade().is_none());
        assert!(state.pictures.iter().any(|p| p.prepared.time()
            == &Time {
                ticks: "6".into(),
                timebase: 24
            }));
        let weak = Arc::downgrade(&state.pictures.last().unwrap().prepared);
        assert!(state.machine.expire(31000).unwrap());
        assert!(weak.upgrade().is_some());
        assert!(!state.expire(31000).unwrap());
        assert!(weak.upgrade().is_none());
        assert!(state.pictures.is_empty());
        assert!(state.text_coverage.is_none());
    }
}
