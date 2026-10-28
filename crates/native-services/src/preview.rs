//! Process-local preview registry. Immutable private inputs outlive every generation reader.
use opencut_api_contracts::preview::*;
use opencut_contracts::{EditorError, ProjectSnapshot, Time};
use opencut_editor_application::{
    access::SourceAccess,
    preview::{Effect, Machine},
};
use opencut_media::{
    CancellationToken, ContinuousPreviewProducer, PreviewStreamLimits, PreviewStreamRead,
};
use opencut_project_render::{
    ProjectRenderAdapter, ResolvedProjectRenderRequest, ResolvedProjectSnapshot,
};
use opencut_time::RationalTime;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use opencut_contracts::historical_visual::HistoricalVisualSnapshot;
use opencut_project_render::historical_visual::{
    HistoricalVisualProjectRenderAdapter, HistoricalVisualResolvedProjectRenderRequest,
    HistoricalVisualResolvedProjectSnapshot,
};
const MAX_SESSIONS: usize = 32;
#[derive(Clone)]
enum ResolvedPreview {
    V2(ResolvedProjectSnapshot),
    V3(HistoricalVisualResolvedProjectSnapshot),
}
pub enum PreviewSource {
    Basic(ProjectSnapshot),
    Visual(HistoricalVisualSnapshot),
}
impl PreviewSource {
    fn base(&self) -> ProjectSnapshot {
        match self {
            Self::Basic(snapshot) => snapshot.clone(),
            Self::Visual(snapshot) => ProjectSnapshot {
                project: snapshot.project.base.clone(),
                revision: snapshot.revision.clone(),
            },
        }
    }
}

type Producer = Arc<Generation>;
struct Generation {
    // Drop/reap the producer before removing its private caption directory.
    producer: Mutex<ContinuousPreviewProducer>,
    _directory: tempfile::TempDir,
    _inputs: Arc<crate::preview_inputs::PreviewInputs>,
}
impl std::ops::Deref for Generation {
    type Target = Mutex<ContinuousPreviewProducer>;
    fn deref(&self) -> &Self::Target {
        &self.producer
    }
}
impl Generation {
    fn cancel(&self) {
        if let Ok(producer) = self.producer.lock() {
            producer.cancel();
        }
    }
}
struct Entry {
    machine: Machine,
    principal: String,
    identity: String,
    key: String,
    resolved: ResolvedPreview,
    inputs: Arc<crate::preview_inputs::PreviewInputs>,
    producer: Option<Producer>,
    stream_claimed: bool,
    audio: bool,
    retired_at: Option<u64>,
}
struct Registry {
    entries: BTreeMap<(PathBuf, String), Entry>,
}
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static CLOCK: OnceLock<Instant> = OnceLock::new();
static PREPARING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct Preparing;
impl Preparing {
    fn claim() -> Result<Self, EditorError> {
        PREPARING
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |active| (active < 2).then_some(active + 1),
            )
            .map_err(|_| err("PREVIEW_LIMIT"))?;
        Ok(Self)
    }
}
impl Drop for Preparing {
    fn drop(&mut self) {
        PREPARING.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
fn now() -> u64 {
    CLOCK
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn err(code: &str) -> EditorError {
    EditorError::new(code, "Preview session request could not be completed")
}
fn media(error: opencut_media::MediaError) -> EditorError {
    err(error.code())
}
fn root(path: &Path) -> Result<PathBuf, EditorError> {
    std::fs::canonicalize(path).map_err(|_| err("STORAGE_ERROR"))
}
fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let Some(registry) = REGISTRY.get() else {
                    continue;
                };
                if let Ok(mut registry) = registry.lock() {
                    let at = now();
                    for entry in registry.entries.values_mut() {
                        refresh(entry, at);
                    }
                    registry.entries.retain(|_, e| {
                        e.retired_at
                            .is_none_or(|at| now().saturating_sub(at) < 60_000)
                    });
                }
            }
        });
        Mutex::new(Registry {
            entries: BTreeMap::new(),
        })
    })
}
fn cancel(entry: &mut Entry) {
    if let Some(producer) = entry.producer.take()
        && let Ok(producer) = producer.lock()
    {
        producer.cancel();
    }
    entry.stream_claimed = false;
}
fn refresh(entry: &mut Entry, at: u64) {
    if entry.machine.expire(at) {
        cancel(entry);
        entry.retired_at = Some(at);
    }
    if let Some(producer) = &entry.producer
        && let Ok(producer) = producer.try_lock()
    {
        let status = producer.status();
        let view = entry.machine.snapshot(at);
        let through = status.produced_through.and_then(|t| {
            RationalTime::try_from(&view.actual_start)
                .ok()?
                .checked_add(RationalTime::new(t.ticks, t.timebase).ok()?)
                .ok()
                .map(Time::from)
        });
        entry.machine.producer(
            &view.generation,
            through,
            status.buffered_bytes,
            status.done,
            status.error_code.as_deref(),
            at,
        );
    }
}
pub fn create(
    database: &Path,
    asset_root: &Path,
    principal: &str,
    input: &Create,
    source: PreviewSource,
    sources: SourceAccess,
    reauthorize: impl FnOnce() -> Result<(), EditorError>,
) -> Result<Session, EditorError> {
    let snapshot = source.base();
    if input.idempotency_key.is_empty()
        || input.idempotency_key.len() > 256
        || snapshot.revision != input.revision
        || snapshot.project.id != input.project_id
    {
        return Err(err("INVALID_ARGUMENT"));
    }
    let database = root(database)?;
    let identity =
        opencut_editor_application::identity::request_identity("preview_create", principal, input)?
            .current_digest()
            .to_owned();
    {
        let registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
        if let Some(entry) = registry.entries.iter().find_map(|((db, _), entry)| {
            (db == &database && entry.principal == principal && entry.key == input.idempotency_key)
                .then_some(entry)
        }) {
            if entry.identity != identity {
                return Err(err("IDEMPOTENCY_KEY_REUSED"));
            }
            if entry.machine.expired(now()) {
                return Err(err("PREVIEW_EXPIRED"));
            }
            return Ok(entry.machine.snapshot(now()));
        }
        if registry.entries.len() >= MAX_SESSIONS {
            return Err(err("PREVIEW_LIMIT"));
        }
    }
    let composition = match &source {
        PreviewSource::Basic(snapshot) => opencut_composition::compile(
            snapshot,
            &input.sequence_id,
            opencut_composition::CompositionScope::AudioVisual,
        ),
        PreviewSource::Visual(snapshot) => {
            opencut_composition::historical_visual::compile_historical_visual(
                snapshot,
                &input.sequence_id,
                opencut_composition::CompositionScope::AudioVisual,
            )
            .map(|v| v.base)
        }
    }
    .map_err(|e| err(e.code))?;
    let sequence = snapshot
        .project
        .sequences
        .iter()
        .find(|s| s.id == input.sequence_id)
        .ok_or_else(|| err("NOT_FOUND"))?;
    if sequence.width > 1920 || sequence.height > 1080 {
        return Err(err("PREVIEW_VIEWPORT_LIMIT"));
    }
    let duration: Time = RationalTime::new(
        composition.timeline.duration.ticks,
        composition.timeline.duration.timebase,
    )?
    .into();
    let id = format!("preview_{}", uuid::Uuid::new_v4().simple());
    let fps = sequence.fps.clone();
    opencut_editor_application::preview::align(
        &input.start_time,
        &fps,
        input.alignment,
        &duration,
    )?;
    let _preparing = Preparing::claim()?;
    let ids = match &source {
        PreviewSource::Basic(snapshot) => {
            ProjectRenderAdapter::required_asset_ids(snapshot, &input.sequence_id)
        }
        PreviewSource::Visual(snapshot) => {
            HistoricalVisualProjectRenderAdapter::required_asset_ids(snapshot, &input.sequence_id)
        }
    }
    .map_err(|e| err(e.code))?;
    if sources == SourceAccess::ProjectContent
        && snapshot
            .project
            .assets
            .iter()
            .any(|a| ids.contains(&a.id) && a.version.is_none())
    {
        return Err(err("ASSET_LOCK_REQUIRED"));
    }
    let workspace = database.parent().ok_or_else(|| err("STORAGE_ERROR"))?;
    crate::preview_inputs::recover(workspace)?;
    let inputs = Arc::new(crate::preview_inputs::PreviewInputs::create(workspace)?);
    let store = opencut_asset_store::AssetStore::open_managed(asset_root)?;
    let lock = store.capture(&snapshot, &input.sequence_id, &ids)?;
    let fixed = store.materialize(
        &snapshot,
        &input.sequence_id,
        &lock,
        &ids,
        inputs.path(),
        &|| false,
    )?;
    let resolved = match source {
        PreviewSource::Basic(_) => ResolvedPreview::V2(
            ProjectRenderAdapter::resolve_assets(fixed, &input.sequence_id, |asset| {
                Ok(PathBuf::from(&asset.uri))
            })
            .map_err(|e| err(e.code))?,
        ),
        PreviewSource::Visual(mut snapshot) => {
            snapshot.project.base = fixed.project;
            ResolvedPreview::V3(
                HistoricalVisualProjectRenderAdapter::resolve_assets(
                    snapshot,
                    &input.sequence_id,
                    |asset| Ok(PathBuf::from(&asset.uri)),
                )
                .map_err(|e| err(e.code))?,
            )
        }
    };
    reauthorize()?;
    let machine = Machine::new(
        input,
        id.clone(),
        principal.into(),
        uuid::Uuid::new_v4().to_string(),
        fps,
        duration,
        now(),
    )?;
    let mut registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
    // A concurrent identical create may have completed during native capture.
    if let Some(entry) = registry.entries.iter().find_map(|((db, _), entry)| {
        (db == &database && entry.principal == principal && entry.key == input.idempotency_key)
            .then_some(entry)
    }) {
        return if entry.identity == identity {
            Ok(entry.machine.snapshot(now()))
        } else {
            Err(err("IDEMPOTENCY_KEY_REUSED"))
        };
    }
    if registry.entries.len() >= MAX_SESSIONS {
        return Err(err("PREVIEW_LIMIT"));
    }
    let view = machine.snapshot(now());
    registry.entries.insert(
        (database, id),
        Entry {
            machine,
            principal: principal.into(),
            identity,
            key: input.idempotency_key.clone(),
            resolved,
            inputs,
            producer: None,
            stream_claimed: false,
            audio: input.audio_enabled,
            retired_at: None,
        },
    );
    Ok(view)
}
/// Used only by a host to reauthorize this fixed project on every request.
pub fn target(database: &Path, id: &str) -> Result<(String, String), EditorError> {
    let database = root(database)?;
    let registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
    let entry = registry
        .entries
        .get(&(database, id.into()))
        .ok_or_else(|| err("NOT_FOUND"))?;
    let view = entry.machine.snapshot(now());
    Ok((view.project_id, view.revision))
}
/// Internal discriminator for authorization against the correct document version.
pub fn is_historical_visual(database: &Path, id: &str) -> Result<bool, EditorError> {
    let database = root(database)?;
    let registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
    let entry = registry
        .entries
        .get(&(database, id.into()))
        .ok_or_else(|| err("NOT_FOUND"))?;
    Ok(matches!(&entry.resolved, ResolvedPreview::V3(_)))
}
/// Host startup reclaims abandoned inputs without touching a live decoder.
pub fn recover_workspace(database: &Path) -> Result<usize, EditorError> {
    let database = root(database)?;
    crate::preview_inputs::recover(database.parent().ok_or_else(|| err("STORAGE_ERROR"))?)
}
/// Host shutdown cancels every generation before releasing private session inputs.
pub fn shutdown_workspace(database: &Path) -> Result<(), EditorError> {
    let database = root(database)?;
    let removed = {
        let mut registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
        let keys: Vec<_> = registry
            .entries
            .keys()
            .filter(|(db, _)| db == &database)
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| registry.entries.remove(&key))
            .collect::<Vec<_>>()
    };
    for mut entry in removed {
        cancel(&mut entry);
    }
    Ok(())
}
pub fn inspect(database: &Path, id: &str) -> Result<Session, EditorError> {
    let database = root(database)?;
    let mut registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
    let entry = registry
        .entries
        .get_mut(&(database, id.into()))
        .ok_or_else(|| err("NOT_FOUND"))?;
    refresh(entry, now());
    Ok(entry.machine.snapshot(now()))
}
pub fn acquire(database: &Path, principal: &str, input: &Acquire) -> Result<Session, EditorError> {
    let database = root(database)?;
    let mut registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
    let entry = registry
        .entries
        .get_mut(&(database, input.session_id.clone()))
        .ok_or_else(|| err("NOT_FOUND"))?;
    entry.machine.acquire(
        &input.expected_generation,
        principal.into(),
        uuid::Uuid::new_v4().to_string(),
        now(),
    )?;
    cancel(entry);
    entry.principal = principal.into();
    entry.retired_at = None;
    Ok(entry.machine.snapshot(now()))
}
pub fn events(database: &Path, input: &EventsInput) -> Result<Events, EditorError> {
    let database = root(database)?;
    let mut registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
    let entry = registry
        .entries
        .get_mut(&(database, input.session_id.clone()))
        .ok_or_else(|| err("NOT_FOUND"))?;
    refresh(entry, now());
    entry.machine.events(input.cursor.as_deref())
}
pub fn control(
    database: &Path,
    principal: &str,
    input: &Control,
    probe: &crate::media::ProbeFactory,
) -> Result<Session, EditorError> {
    let database = root(database)?;
    let key = (database.clone(), input.session_id.clone());
    let prepared = {
        let mut registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
        let entry = registry
            .entries
            .get_mut(&key)
            .ok_or_else(|| err("NOT_FOUND"))?;
        if entry.principal != principal {
            return Err(err("NOT_AUTHORIZED"));
        }
        refresh(entry, now());
        let effect = entry.machine.control(input, now())?;
        if effect == Effect::None {
            return Ok(entry.machine.snapshot(now()));
        }
        cancel(entry);
        let view = entry.machine.snapshot(now());
        if effect == Effect::Cancel {
            if view.state == State::Stopped {
                entry.retired_at = Some(now());
            }
            return Ok(view);
        }
        (
            view,
            entry.resolved.clone(),
            entry.inputs.clone(),
            entry.audio,
        )
    };
    let (view, resolved, inputs, audio) = prepared;
    let result = (|| {
        let start = RationalTime::try_from(&view.actual_start)?;
        let duration = RationalTime::try_from(&view.duration)?.checked_sub(start)?;
        let sequence_start = opencut_media::MediaTime {
            ticks: start.ticks(),
            timebase: start.timebase(),
        };
        let sequence_duration = opencut_media::MediaTime {
            ticks: duration.ticks(),
            timebase: duration.timebase(),
        };
        let directory = tempfile::Builder::new()
            .prefix(&format!("generation-{}-", view.generation))
            .tempdir_in(inputs.path())
            .map_err(|_| err("STORAGE_ERROR"))?;
        let output = directory.path().to_path_buf();
        let backend = probe()?;
        #[cfg(unix)]
        let backend = backend.with_output_admission(Arc::new(PreviewAdmission {
            budget: opencut_storage_budget::Budget::new(
                database.parent().ok_or_else(|| err("STORAGE_ERROR"))?,
            ),
            attempt: inputs.lease.clone(),
            directory: output,
            reservations: Mutex::new(Vec::new()),
        }));
        let producer = match resolved {
            ResolvedPreview::V2(resolved) => {
                let mut plan = ProjectRenderAdapter::plan_resolved(&ResolvedProjectRenderRequest {
                    resolved: &resolved,
                    container: opencut_media::Container::Mp4,
                    video_codec: opencut_media::VideoCodec::H264,
                    output: directory.path().join("stream.mp4"),
                    overwrite: false,
                })
                .map_err(|e| err(e.code))?;
                if !audio {
                    plan.media_request.audio_clips.clear();
                }
                let request = opencut_media::TimelineRangeRenderRequest {
                    timeline: plan.media_request,
                    sequence_start,
                    sequence_duration,
                };
                backend.start_timeline_stream(
                    &request,
                    CancellationToken::new(),
                    PreviewStreamLimits::default(),
                )
            }
            ResolvedPreview::V3(resolved) => {
                let mut plan = HistoricalVisualProjectRenderAdapter::plan_resolved(
                    &HistoricalVisualResolvedProjectRenderRequest {
                        resolved: &resolved,
                        container: opencut_media::Container::Mp4,
                        video_codec: opencut_media::VideoCodec::H264,
                        output: directory.path().join("stream.mp4"),
                        overwrite: false,
                    },
                )
                .map_err(|e| err(e.code))?;
                if !audio {
                    plan.media_request.base.audio_clips.clear();
                }
                let request = opencut_media::HistoricalVisualTimelineRangeRenderRequest {
                    timeline: plan.media_request,
                    sequence_start,
                    sequence_duration,
                };
                backend.start_timeline_stream_historical_visual(
                    &request,
                    CancellationToken::new(),
                    PreviewStreamLimits::default(),
                )
            }
        }
        .map_err(media)?;
        Ok::<_, EditorError>(Arc::new(Generation {
            producer: Mutex::new(producer),
            _directory: directory,
            _inputs: inputs.clone(),
        }))
    })();
    let mut registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
    let entry = registry
        .entries
        .get_mut(&key)
        .ok_or_else(|| err("NOT_FOUND"))?;
    if entry.machine.snapshot(now()).generation != view.generation
        || entry.machine.snapshot(now()).state != State::Producing
    {
        if let Ok(producer) = result {
            producer.cancel();
        }
        return Err(err("PREVIEW_GENERATION_CONFLICT"));
    }
    match result {
        Ok(producer) => {
            entry.producer = Some(producer);
        }
        Err(error) => {
            entry
                .machine
                .producer(&view.generation, None, 0, false, Some(&error.code), now());
        }
    }
    Ok(entry.machine.snapshot(now()))
}
pub struct Stream {
    producer: Producer,
    database: PathBuf,
    session: String,
    generation: String,
    _inputs: Arc<crate::preview_inputs::PreviewInputs>,
}
impl Stream {
    pub fn read(&self, timeout: Duration) -> Result<PreviewStreamRead, EditorError> {
        {
            let registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
            let entry = registry
                .entries
                .get(&(self.database.clone(), self.session.clone()))
                .ok_or_else(|| err("PREVIEW_EXPIRED"))?;
            let view = entry.machine.snapshot(now());
            if view.generation != self.generation
                || entry.machine.expired(now())
                || !matches!(view.state, State::Producing | State::Ended)
            {
                return Err(err("PREVIEW_GENERATION_CONFLICT"));
            }
        }
        self.producer
            .lock()
            .map_err(|_| err("INTERNAL_ERROR"))?
            .recv_timeout(timeout)
            .map_err(media)
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        if let Ok(producer) = self.producer.lock() {
            producer.cancel();
        }
        if let Ok(mut registry) = registry().lock()
            && let Some(entry) = registry
                .entries
                .get_mut(&(self.database.clone(), self.session.clone()))
            && entry.machine.snapshot(now()).generation == self.generation
        {
            let completed = self.producer.lock().ok().is_some_and(|producer| {
                let status = producer.status();
                status.done && status.error_code.is_none()
            });
            entry.machine.producer(
                &self.generation,
                None,
                0,
                completed,
                if completed {
                    None
                } else {
                    Some("PREVIEW_DISCONNECTED")
                },
                now(),
            );
        }
    }
}
pub fn stream(
    database: &Path,
    principal: &str,
    id: &str,
    generation: &str,
    owner: &str,
    lease: &str,
) -> Result<Stream, EditorError> {
    let database = root(database)?;
    let mut registry = registry().lock().map_err(|_| err("INTERNAL_ERROR"))?;
    let entry = registry
        .entries
        .get_mut(&(database.clone(), id.into()))
        .ok_or_else(|| err("NOT_FOUND"))?;
    if entry.principal != principal {
        return Err(err("NOT_AUTHORIZED"));
    }
    entry.machine.verify(generation, owner, lease, now())?;
    if entry.stream_claimed {
        return Err(err("PREVIEW_STREAM_CLAIMED"));
    }
    let producer = entry
        .producer
        .clone()
        .ok_or_else(|| err("PREVIEW_NOT_PLAYING"))?;
    entry.stream_claimed = true;
    Ok(Stream {
        producer,
        database,
        session: id.into(),
        generation: generation.into(),
        _inputs: entry.inputs.clone(),
    })
}
pub use opencut_media::PreviewStreamRead as StreamRead;

#[cfg(unix)]
struct PreviewAdmission {
    budget: opencut_storage_budget::Budget,
    attempt: opencut_storage_budget::AttemptLease,
    directory: PathBuf,
    reservations: Mutex<Vec<opencut_storage_budget::Reservation>>,
}
#[cfg(unix)]
impl std::fmt::Debug for PreviewAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewAdmission")
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}
#[cfg(unix)]
struct PreviewProcess(opencut_storage_budget::ReservedChild);
#[cfg(unix)]
impl opencut_media::OutputProcess for PreviewProcess {
    fn child(&mut self) -> &mut std::process::Child {
        self.0.process()
    }
    fn output_limit(&self) -> Option<u64> {
        None
    }
}
#[cfg(unix)]
impl opencut_media::OutputAdmission for PreviewAdmission {
    fn spawn(
        &self,
        command: std::process::Command,
        output: Option<&Path>,
    ) -> opencut_media::MediaResult<Box<dyn opencut_media::OutputProcess>> {
        if output.is_some() {
            return Err(opencut_media::MediaError::InvalidRequest(
                "Preview producer writes only to its bounded stream".into(),
            ));
        }
        Ok(Box::new(PreviewProcess(
            self.attempt.clone().spawn_readonly(command)?,
        )))
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> opencut_media::MediaResult<()> {
        use std::io::Write;
        if path.parent() != Some(self.directory.as_path()) {
            return Err(opencut_media::MediaError::InvalidRequest(
                "Preview caption is outside its generation".into(),
            ));
        }
        let reservation = self
            .budget
            .reserve("preview_caption", bytes.len() as u64)
            .map_err(|e| opencut_media::MediaError::Admission {
                code: e.code,
                message: e.message,
            })?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        self.reservations
            .lock()
            .map_err(|_| {
                opencut_media::MediaError::InvalidRequest("Preview admission lock failed".into())
            })?
            .push(reservation);
        Ok(())
    }
}
