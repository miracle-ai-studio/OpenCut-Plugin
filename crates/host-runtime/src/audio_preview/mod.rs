//! Process-local host assembly for fixed-revision audio. The application owns
//! clocks and control semantics; this host owns authorization, CAS and lifetime.
pub(crate) mod cache;
mod fonts;
mod producer;
pub(crate) mod recovery;
pub(crate) mod resources;
mod scene;
mod stream;
use crate::{Automation, Caller, ExecutionContext};
pub(crate) use fonts::{authorize_font, authorize_font_batch, open_font};
use opencut_composition::scene::{PcmFormat, PreparedPreviewAudio};
use opencut_contracts::EditorError;
use opencut_editor_application::audio_preview::*;
use opencut_editor_application::scene_text_frame_jobs::TextFrameJobSubmissionStore;
use opencut_job_execution::scene_audio::TrustedJobAuthority;
use opencut_native_services::classic_audio_preview::new_identity;
pub(crate) use scene::{edit_draft_picture, scene_picture};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Instant,
};
mod scheduling;
pub(crate) use scheduling::yield_to_realtime_audio;
pub use stream::AudioPreviewReader;
mod pixels;
mod visual_owner;
pub use pixels::VisualPictureReader;
pub(crate) use pixels::open as open_picture;

pub(super) fn error(code: &str) -> EditorError {
    EditorError::new(
        code,
        "Fixed audio-preview session is unavailable or no longer current",
    )
}
pub(super) fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>, EditorError> {
    value.lock().map_err(|_| error("INTERNAL_ERROR"))
}
pub(super) fn now() -> u64 {
    static CLOCK: OnceLock<Instant> = OnceLock::new();
    CLOCK
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn database_key(path: &Path) -> Result<PathBuf, EditorError> {
    std::fs::canonicalize(path).map_err(|_| error("STORAGE_ERROR"))
}
struct SessionState {
    machine: AudioPreviewMachine,
    cache_plan: Option<opencut_api_contracts::visual_preview::VisualPreviewCache>,
    authority: TrustedJobAuthority,
    pictures: Vec<CachedPicture>,
    text_coverage: Option<fonts::CachedTextCoverage>,
    resources: HashMap<String, resources::SessionResource>,
}
struct CachedPicture {
    resolution: opencut_contracts::export_video::RasterResolution,
    retained_picture_bytes: u32,
    session: AudioPreviewStream,
    prepared: Arc<opencut_editor_application::visual_preview::PreparedPicture>,
}

impl SessionState {
    fn font_version(
        &self,
        session: &AudioPreviewStream,
        id: &str,
    ) -> Option<opencut_contracts::AssetVersion> {
        self.pictures
            .iter()
            .rev()
            .filter(|picture| &picture.session == session)
            .find_map(|picture| picture.prepared.font_version(id))
            // Explicit edit drafts already publish complete native coverage;
            // retain that existing authority while their overlay is current.
            .or_else(|| {
                self.text_coverage
                    .as_ref()
                    .filter(|proof| &proof.session == session)
                    .and_then(|proof| {
                        proof
                            .coverage
                            .iter()
                            .find(|p| p.font.id == id)
                            .map(|p| p.font.content.clone())
                    })
            })
    }
    fn remember_picture(&mut self, picture: CachedPicture) {
        // Live bindings and one speculative cut can interleave. Retain exact
        // admitted contexts rather than letting the last request revoke an
        // in-flight resource open. All ownership/lease fences remain external.
        self.pictures.push(picture);
        if self.pictures.len() > 8 {
            self.pictures.remove(0);
        }
    }
    fn expire(&mut self, at: u64) -> Result<bool, EditorError> {
        let changed = self.machine.expire(at)?;
        // Another endpoint may already have made this session terminal. Cache
        // disposal must not depend on being the first expiry observer.
        if matches!(
            self.machine.snapshot(at).state,
            AudioPreviewState::Stopped | AudioPreviewState::Failed
        ) {
            self.cache_plan = None;
            self.pictures.clear();
            self.text_coverage = None;
            self.resources.clear();
        }
        Ok(changed)
    }
}
struct Actor {
    sender: mpsc::SyncSender<producer::BlockRequest>,
    thread: JoinHandle<()>,
}
struct Entry {
    visual: Mutex<Option<visual_owner::VisualOwner>>,
    visual_waiter: AtomicBool,
    pixels: Arc<pixels::FramePixels>,
    transform_draft_gate: Mutex<()>,
    resource_open_gate: Mutex<()>,
    database: PathBuf,
    request: AudioPreviewCreate,
    creator: String,
    prepared: Arc<PreparedPreviewAudio>,
    policy: AudioPreviewPolicy,
    state: Mutex<SessionState>,
    credit: Condvar,
    actor: Mutex<Option<Actor>>,
    stream_claim: Mutex<Option<(u64, String)>>,
    shutdown: AtomicBool,
}
impl Entry {
    fn view(&self) -> Result<AudioPreviewSession, EditorError> {
        let mut state = lock(&self.state)?;
        if state.expire(now())? {
            state.pictures.clear();
            state.text_coverage = None;
            state.resources.clear();
            self.credit.notify_all();
            self.pixels.clear();
        }
        Ok(state.machine.snapshot(now()))
    }
    fn occupies_slot(&self) -> bool {
        let active = self.view().is_ok_and(|v| {
            !matches!(
                v.state,
                AudioPreviewState::Stopped | AudioPreviewState::Failed
            )
        });
        active
            || lock(&self.actor).is_ok_and(|a| a.as_ref().is_some_and(|a| !a.thread.is_finished()))
    }
    fn checkpoint(&self, app: &opencut_sqlite_store::EditorApplication) -> Result<(), EditorError> {
        let authority = {
            let mut state = lock(&self.state)?;
            if state.expire(now())? {
                state.pictures.clear();
                state.text_coverage = None;
                state.resources.clear();
                self.credit.notify_all();
                self.pixels.clear();
            }
            if self.shutdown.load(Ordering::Acquire)
                || matches!(
                    state.machine.snapshot(now()).state,
                    AudioPreviewState::Stopped | AudioPreviewState::Failed
                )
            {
                return Err(error("PREVIEW_STOPPED"));
            }
            state.authority.clone()
        };
        app.revalidate_audio_preview(&self.request.project_id, &self.request.revision, &authority)
    }
    fn ensure_actor(self: &Arc<Self>) -> Result<(), EditorError> {
        let mut actor = lock(&self.actor)?;
        if actor.as_ref().is_some_and(|a| !a.thread.is_finished()) {
            return Ok(());
        }
        if let Some(old) = actor.take() {
            old.thread.join().map_err(|_| error("INTERNAL_ERROR"))?;
        }
        if self.shutdown.load(Ordering::Acquire) {
            return Err(error("PREVIEW_STOPPED"));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let entry = self.clone();
        let thread = std::thread::Builder::new()
            .name("opencut-audio-preview".into())
            .spawn(move || producer::run(entry, receiver))
            .map_err(|_| error("IO_ERROR"))?;
        *actor = Some(Actor { sender, thread });
        Ok(())
    }
    fn signal_shutdown(&self) -> Result<(), EditorError> {
        let mut state = lock(&self.state)?;
        state.pictures.clear();
        state.text_coverage = None;
        state.resources.clear();
        self.shutdown.store(true, Ordering::Release);
        self.pixels.clear();
        self.credit.notify_all();
        Ok(())
    }
    fn stop_actor(&self) -> Result<(), EditorError> {
        self.signal_shutdown()?;
        let actor = lock(&self.actor)?.take();
        if let Some(actor) = actor {
            actor.thread.join().map_err(|_| error("INTERNAL_ERROR"))?;
        }
        Ok(())
    }
}
#[derive(Default)]
struct Registry {
    entries: HashMap<String, Arc<Entry>>,
}
fn registries() -> &'static Mutex<HashMap<PathBuf, Registry>> {
    static REGISTRIES: OnceLock<Mutex<HashMap<PathBuf, Registry>>> = OnceLock::new();
    REGISTRIES.get_or_init(Mutex::default)
}
fn entry(database: &Path, id: &str) -> Result<Arc<Entry>, EditorError> {
    if id.len() > 1024 {
        return Err(error("INVALID_ARGUMENT"));
    }
    lock(registries())?
        .get(&database_key(database)?)
        .and_then(|r| r.entries.get(id))
        .cloned()
        .ok_or_else(|| error("NOT_FOUND"))
}
fn trusted(caller: &Caller) -> TrustedJobAuthority {
    match caller {
        Caller::LocalOwner => TrustedJobAuthority::LocalOwner,
        Caller::Credential(c) => TrustedJobAuthority::Credential {
            principal_id: c.principal_id().into(),
            credential_id: c.credential_id().into(),
        },
    }
}
fn authorize(host: &mut Automation, entry: &Entry, original: bool) -> Result<(), EditorError> {
    host.app.set_execution_context(ExecutionContext::for_caller(
        host.source,
        host.caller.clone(),
    ));
    host.app
        .revalidate_scene_audio_preview(&entry.request.project_id, &entry.request.revision)?;
    if original {
        entry.checkpoint(&host.app)?;
    }
    Ok(())
}
pub(crate) fn create(
    host: &mut Automation,
    input: AudioPreviewCreate,
) -> Result<AudioPreviewSession, EditorError> {
    validate_audio_preview_create(&input)?;
    crate::classic_audio_runtime::preview_config()?;
    host.app.set_execution_context(ExecutionContext::for_caller(
        host.source,
        host.caller.clone(),
    ));
    let selected = host
        .app
        .fixed_audio_preview_snapshot(&input.project_id, &input.revision)?;
    validate_audio_preview_policy(&selected.policy)?;
    let database = database_key(&host.database)?;
    let prepared = Arc::new(
        PreparedPreviewAudio::new(
            &selected.snapshot,
            &input.sequence_id,
            PcmFormat {
                sample_rate: input.sample_rate,
                channels: 2,
            },
        )
        .map_err(|e| EditorError::new(e.code, e.message))?,
    );
    let mut all = lock(registries())?;
    let registry = all.entry(database.clone()).or_default();
    if let Some(existing) = registry.entries.values().find(|e| {
        e.creator == host.caller.principal_id()
            && e.request.idempotency_key == input.idempotency_key
    }) {
        if existing.request != input {
            return Err(error("IDEMPOTENCY_CONFLICT"));
        }
        let original = lock(&existing.state)?.authority.clone();
        host.app
            .revalidate_audio_preview(&input.project_id, &input.revision, &original)?;
        return existing.view();
    }
    // Retain a bounded terminal receipt set; active sessions and native actors
    // must never be evicted to make room for a new create.
    if registry.entries.len() >= 32 {
        let victim = registry.entries.iter().find_map(|(id, e)| {
            let finished = lock(&e.actor)
                .ok()?
                .as_ref()
                .is_none_or(|a| a.thread.is_finished());
            let terminal = matches!(
                e.view().ok()?.state,
                AudioPreviewState::Stopped | AudioPreviewState::Failed
            );
            (finished && terminal).then(|| id.clone())
        });
        if let Some(id) = victim {
            registry.entries.remove(&id);
        } else {
            return Err(error("PREVIEW_LIMIT"));
        }
    }
    let active = registry
        .entries
        .values()
        .filter(|e| e.occupies_slot())
        .count();
    if active >= AUDIO_PREVIEW_MAX_SESSIONS {
        return Err(error("PREVIEW_LIMIT"));
    }
    let id = new_identity();
    let machine = AudioPreviewMachine::new(
        &input,
        id.clone(),
        host.caller.principal_id().into(),
        new_identity(),
        prepared.extent(),
        now(),
    )?;
    let entry = Arc::new(Entry {
        visual: Mutex::new(None),
        visual_waiter: AtomicBool::new(false),
        pixels: Arc::new(pixels::FramePixels::default()),
        transform_draft_gate: Mutex::new(()),
        resource_open_gate: Mutex::new(()),
        database,
        request: input,
        creator: host.caller.principal_id().into(),
        prepared,
        policy: selected.policy,
        state: Mutex::new(SessionState {
            machine,
            cache_plan: None,
            authority: selected.authority,
            pictures: Vec::new(),
            text_coverage: None,
            resources: HashMap::new(),
        }),
        credit: Condvar::new(),
        actor: Mutex::new(None),
        stream_claim: Mutex::new(None),
        shutdown: AtomicBool::new(false),
    });
    visual_owner::maintain(&entry)?;
    entry.ensure_actor()?;
    let view = entry.view()?;
    registry.entries.insert(id, entry);
    Ok(view)
}
pub(crate) fn inspect(
    host: &mut Automation,
    input: AudioPreviewInspect,
) -> Result<AudioPreviewSession, EditorError> {
    let e = entry(&host.database, &input.session_id)?;
    authorize(host, &e, false)?;
    e.view()
}
pub(crate) fn events(
    host: &mut Automation,
    input: AudioPreviewEventsInput,
) -> Result<AudioPreviewEvents, EditorError> {
    let e = entry(&host.database, &input.session_id)?;
    authorize(host, &e, false)?;
    let mut state = lock(&e.state)?;
    if state.expire(now())? {
        e.credit.notify_all();
    }
    state.machine.events(&input)
}
pub(crate) fn control(
    host: &mut Automation,
    input: AudioPreviewControl,
) -> Result<AudioPreviewSession, EditorError> {
    let e = entry(&host.database, &input.session_id)?;
    // Stop remains idempotently inspectable after its actor has exited.
    authorize(
        host,
        &e,
        !matches!(input.command, AudioPreviewCommand::Stop),
    )?;
    if host.caller.principal_id() != input.owner_id {
        return Err(error("NOT_AUTHORIZED"));
    }
    let gain = match &input.command {
        AudioPreviewCommand::SetTemporaryGain { gain } => Some(
            e.prepared
                .validate_temporary_gain(&gain.track_id, &gain.element_id, gain.decibels_tenths)
                .map_err(|x| EditorError::new(x.code, x.message))?,
        ),
        _ => None,
    };
    let transition = {
        let mut state = lock(&e.state)?;
        let transition = state
            .machine
            .control_validated(&input, gain.as_ref(), now())?;
        if transition.effect != AudioPreviewEffect::None {
            state.pictures.clear();
            state.text_coverage = None;
            state.resources.clear();
        }
        // Predicate changes and notification share the stream wait mutex.
        e.credit.notify_all();
        transition
    };
    if transition.session.state == AudioPreviewState::Stopped {
        e.stop_actor()?;
    }
    Ok(transition.session)
}
pub(crate) fn acquire(
    host: &mut Automation,
    input: AudioPreviewAcquire,
) -> Result<AudioPreviewSession, EditorError> {
    let e = entry(&host.database, &input.session_id)?;
    authorize(host, &e, false)?;
    // Serialize admission against creates/other acquisitions. A lease-expired
    // producer is fully torn down before a new owner's source preparation.
    let all = lock(registries())?;
    let current = e.view()?;
    if current.lease_remaining_ms == "0" {
        let registry = all.get(&e.database).ok_or_else(|| error("NOT_FOUND"))?;
        let active = registry
            .entries
            .values()
            .filter(|other| !Arc::ptr_eq(other, &e) && other.occupies_slot())
            .count();
        if active >= AUDIO_PREVIEW_MAX_SESSIONS {
            return Err(error("PREVIEW_LIMIT"));
        }
        // Expire before joining so both idle and preparing actors observe the
        // cancellation through the original-authority CAS checkpoint.
        {
            let mut state = lock(&e.state)?;
            if state.expire(now())? {
                e.credit.notify_all();
            }
        }
        if let Some(actor) = lock(&e.actor)?.take() {
            actor.thread.join().map_err(|_| error("INTERNAL_ERROR"))?;
        }
    }
    let view = {
        let mut state = lock(&e.state)?;
        let next = state.machine.acquire(
            &input,
            host.caller.principal_id().into(),
            new_identity(),
            now(),
        )?;
        if next.effect != AudioPreviewEffect::None {
            state.pictures.clear();
            state.text_coverage = None;
            state.resources.clear();
            state.authority = trusted(&host.caller);
        } else {
            host.app.revalidate_audio_preview(
                &e.request.project_id,
                &e.request.revision,
                &state.authority,
            )?;
        }
        e.credit.notify_all();
        next.session
    };
    e.ensure_actor()?;
    Ok(view)
}
pub(crate) fn authorize_stream(host: &mut Automation, session_id: &str) -> Result<(), EditorError> {
    let e = entry(&host.database, session_id)?;
    authorize(host, &e, true)
}
pub(crate) fn open_stream(
    host: &mut Automation,
    input: AudioPreviewStream,
) -> Result<AudioPreviewReader, EditorError> {
    let e = entry(&host.database, &input.session_id)?;
    authorize(host, &e, true)?;
    lock(&e.state)?
        .machine
        .verify_stream(&input, host.caller.principal_id(), now())?;
    AudioPreviewReader::new(e, input)
}
pub fn shutdown(database: &Path) -> Result<(), EditorError> {
    let registry = lock(registries())?.remove(&database_key(database)?);
    if let Some(registry) = registry {
        for e in registry.entries.values() {
            e.signal_shutdown()?;
        }
        let mut failure = None;
        for e in registry.entries.values() {
            if let Err(err) = e.stop_actor() {
                failure = Some(err);
            }
        }
        if let Some(err) = failure {
            return Err(err);
        }
    }
    Ok(())
}
