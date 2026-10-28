use super::*;
use std::collections::{HashMap, VecDeque};
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
// Background source observations share the host with the foreground audio
// browser. Keep one decoder active; joined waiters still share that same work.
const MAX_FLIGHTS: usize = 1;
const TOMBSTONE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Hash, PartialEq, Eq)]
pub(super) struct TicketKey {
    pub scope: String,
    pub revision: String,
    pub request_id: String,
}
impl TicketKey {
    pub fn new(prepared: &PreparedSourceArt) -> Self {
        Self {
            scope: prepared.scope().into(),
            revision: prepared.request().revision.clone(),
            request_id: prepared.request().request_id.clone(),
        }
    }
}
pub(super) struct Ticket {
    pub prepared: Arc<PreparedSourceArt>,
    pub started: Instant,
    failure: Mutex<Option<EditorError>>,
}
impl Ticket {
    pub fn new(prepared: Arc<PreparedSourceArt>) -> Self {
        Self {
            prepared,
            started: Instant::now(),
            failure: Mutex::default(),
        }
    }
    pub fn fail(&self, err: EditorError) -> Result<(), EditorError> {
        let mut failure = lock(&self.failure)?;
        if failure.is_none() {
            *failure = Some(err);
        }
        Ok(())
    }
    pub fn failure(&self) -> Result<Option<EditorError>, EditorError> {
        Ok(lock(&self.failure)?.clone())
    }
}
pub(super) struct Flight {
    pub prepared: Arc<PreparedSourceArt>,
    pub tickets: Mutex<HashMap<TicketKey, Arc<Ticket>>>,
    pub result: Mutex<Option<Result<Arc<DecodedSourceArt>, EditorError>>>,
    pub changed: Condvar,
}
impl Flight {
    pub fn has_other_active(&self, selected: &Ticket) -> Result<bool, EditorError> {
        for ticket in lock(&self.tickets)?.values() {
            if !std::ptr::eq(ticket.as_ref(), selected) && ticket.failure()?.is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
struct Cached {
    value: Arc<DecodedSourceArt>,
    bytes: usize,
    touch: u64,
}
#[derive(Default)]
struct State {
    cache: HashMap<String, Cached>,
    bytes: usize,
    touch: u64,
    flights: HashMap<String, Arc<Flight>>,
    tickets: HashMap<TicketKey, Arc<Ticket>>,
    queued: VecDeque<TicketKey>,
    cancelled: HashMap<TicketKey, Instant>,
    shutdown: bool,
    #[cfg(test)]
    completed: usize,
}
#[derive(Default)]
pub(super) struct Registry {
    state: Mutex<State>,
    changed: Condvar,
}
pub(super) enum Admission {
    Cached(Arc<DecodedSourceArt>),
    Flight { flight: Arc<Flight>, start: bool },
    Draining(Arc<Flight>),
    Queued,
}
fn cost(value: &DecodedSourceArt) -> usize {
    let identity = value.identity();
    let identity_bytes = match identity {
        SourceArtIdentity::Browser(v) => {
            v.algorithm.capacity()
                + v.decode_profile.capacity()
                + v.browser_version.capacity()
                + v.browser_sha256.capacity()
                + v.node_sha256.capacity()
                + v.module_sha256.capacity()
        }
        SourceArtIdentity::Native(v) => {
            v.algorithm.capacity()
                + v.decode_profile.capacity()
                + v.worker_sha256.capacity()
                + v.module_sha256.capacity()
                + v.rasterizer.capacity()
                + v.platform.os.capacity()
                + v.platform.architecture.capacity()
                + v.platform.kernel_release.capacity()
                + v.platform.system_build.capacity()
                + v.decoder_libraries.capacity()
                    * std::mem::size_of::<opencut_contracts::renderer_identity::NativeLibraryIdentity>(
                    )
                + v.decoder_libraries
                    .iter()
                    .map(|l| {
                        l.name.capacity()
                            + l.version.capacity()
                            + l.configuration_sha256.capacity()
                            + l.binary_sha256.capacity()
                    })
                    .sum::<usize>()
        }
    };
    let version = value.version();
    identity_bytes
        + version.id.capacity()
        + version.content_hash.capacity()
        + 1024
        + match value {
            DecodedSourceArt::Waveform { amplitudes, .. } => {
                amplitudes.capacity() * std::mem::size_of::<f32>()
            }
            DecodedSourceArt::ThumbnailReady { jpeg_bytes, .. } => jpeg_bytes.capacity(),
            DecodedSourceArt::ThumbnailNoFrame { .. } => 0,
        }
}
impl Registry {
    pub fn admit(
        &self,
        key: String,
        ticket_key: TicketKey,
        ticket: Arc<Ticket>,
    ) -> Result<Admission, EditorError> {
        let mut state = lock(&self.state)?;
        state
            .cancelled
            .retain(|_, time| time.elapsed() < TOMBSTONE_TTL);
        if state.shutdown {
            return Err(error("SOURCE_ART_STOPPED"));
        }
        if state.cancelled.contains_key(&ticket_key) {
            return Err(error("SOURCE_ART_CANCELLED"));
        }
        let resuming = state.tickets.get(&ticket_key).is_some_and(|current| {
            Arc::ptr_eq(current, &ticket) && state.queued.contains(&ticket_key)
        });
        if state.tickets.contains_key(&ticket_key) && !resuming {
            return Err(error("SOURCE_ART_REQUEST_CONFLICT"));
        }
        if !resuming && state.tickets.len() >= MAX_ENTRIES {
            return Err(EditorError::new(
                "SOURCE_ART_BUDGET_EXCEEDED",
                "Source artwork request queue is full (256 active or waiting requests)",
            ));
        }
        if let Some(err) = ticket.failure()? {
            return Err(err);
        }
        state.touch = state.touch.wrapping_add(1);
        let touch = state.touch;
        if let Some(cached) = state.cache.get_mut(&key) {
            cached.touch = touch;
            let value = cached.value.clone();
            state.queued.retain(|waiting| waiting != &ticket_key);
            state.tickets.insert(ticket_key, ticket);
            return Ok(Admission::Cached(value));
        }
        if let Some(flight) = state.flights.get(&key).cloned() {
            state.queued.retain(|waiting| waiting != &ticket_key);
            // No participants join a terminating all-cancelled flight. Wait for
            // actual cleanup instead of launching a replacement over its gate.
            if !flight.has_other_active(&ticket)? {
                // This new request was never cancelled. Register its own
                // bounded, cancellable wait without reviving the old decoder.
                state.tickets.insert(ticket_key, ticket);
                return Ok(Admission::Draining(flight));
            }
            lock(&flight.tickets)?.insert(ticket_key.clone(), ticket.clone());
            state.tickets.insert(ticket_key, ticket);
            return Ok(Admission::Flight {
                flight,
                start: false,
            });
        }
        if state.flights.len() >= MAX_FLIGHTS
            || state
                .queued
                .front()
                .is_some_and(|first| first != &ticket_key)
        {
            if !resuming {
                state.queued.push_back(ticket_key.clone());
                state.tickets.insert(ticket_key, ticket);
            }
            return Ok(Admission::Queued);
        }
        state.queued.retain(|waiting| waiting != &ticket_key);
        let flight = Arc::new(Flight {
            prepared: ticket.prepared.clone(),
            tickets: Mutex::new(HashMap::from([(ticket_key.clone(), ticket.clone())])),
            result: Mutex::default(),
            changed: Condvar::new(),
        });
        state.tickets.insert(ticket_key, ticket);
        state.flights.insert(key, flight.clone());
        Ok(Admission::Flight {
            flight,
            start: true,
        })
    }
    pub fn wait_for_change(&self) -> Result<(), EditorError> {
        let state = lock(&self.state)?;
        let _ = self
            .changed
            .wait_timeout(state, Duration::from_millis(50))
            .map_err(|_| error("INTERNAL_ERROR"))?;
        Ok(())
    }
    pub fn complete(
        &self,
        key: &str,
        flight: &Arc<Flight>,
        mut result: Result<Arc<DecodedSourceArt>, EditorError>,
    ) -> Result<(), EditorError> {
        let mut state = lock(&self.state)?;
        #[cfg(test)]
        {
            state.completed += 1;
        }
        if let Ok(value) = &result {
            let bytes = cost(value);
            if state.shutdown {
                result = Err(error("SOURCE_ART_STOPPED"));
            } else if bytes > MAX_BYTES {
                result = Err(error("SOURCE_ART_BUDGET_EXCEEDED"));
            } else {
                // Published payloads remain charged while a completion receipt
                // or warm projection borrows them. Native process RSS/PCM is
                // separately bounded by the decoder slot and captured policy.
                while state.cache.len() >= MAX_ENTRIES || state.bytes + bytes > MAX_BYTES {
                    let Some(oldest) = state
                        .cache
                        .iter()
                        .filter(|(_, cached)| Arc::strong_count(&cached.value) == 1)
                        .min_by_key(|(_, value)| value.touch)
                        .map(|(key, _)| key.clone())
                    else {
                        break;
                    };
                    if let Some(removed) = state.cache.remove(&oldest) {
                        state.bytes -= removed.bytes;
                    }
                }
                if state.cache.len() >= MAX_ENTRIES || state.bytes + bytes > MAX_BYTES {
                    result = Err(error("SOURCE_ART_BUDGET_EXCEEDED"));
                } else {
                    state.touch = state.touch.wrapping_add(1);
                    let touch = state.touch;
                    state.cache.insert(
                        key.into(),
                        Cached {
                            value: value.clone(),
                            bytes,
                            touch,
                        },
                    );
                    state.bytes += bytes;
                }
            }
        }
        if state
            .flights
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, flight))
        {
            state.flights.remove(key);
        }
        *lock(&flight.result)? = Some(result);
        flight.changed.notify_all();
        self.changed.notify_all();
        Ok(())
    }
    pub fn release(
        &self,
        key: &TicketKey,
        ticket: &Arc<Ticket>,
        flight: &Flight,
    ) -> Result<(), EditorError> {
        let mut state = lock(&self.state)?;
        if state
            .tickets
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, ticket))
        {
            state.tickets.remove(key);
        }
        lock(&flight.tickets)?.remove(key);
        self.changed.notify_all();
        if let Some(err) = ticket.failure()? {
            return Err(err);
        }
        if state.shutdown {
            return Err(error("SOURCE_ART_STOPPED"));
        }
        Ok(())
    }
    pub fn release_cached(&self, key: &TicketKey, ticket: &Arc<Ticket>) -> Result<(), EditorError> {
        let mut state = lock(&self.state)?;
        if state
            .tickets
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, ticket))
        {
            state.tickets.remove(key);
            state.queued.retain(|waiting| waiting != key);
        }
        self.changed.notify_all();
        if let Some(err) = ticket.failure()? {
            return Err(err);
        }
        if state.shutdown {
            return Err(error("SOURCE_ART_STOPPED"));
        }
        Ok(())
    }
    pub fn cancel(&self, key: TicketKey) -> Result<Option<Arc<Flight>>, EditorError> {
        let mut state = lock(&self.state)?;
        state
            .cancelled
            .retain(|_, time| time.elapsed() < TOMBSTONE_TTL);
        if let Some(ticket) = state.tickets.get(&key) {
            ticket.fail(error("SOURCE_ART_CANCELLED"))?;
        }
        let final_flight = state.flights.values().find_map(|flight| {
            let tickets = flight.tickets.lock().ok()?;
            if tickets.contains_key(&key)
                && tickets
                    .values()
                    .all(|ticket| ticket.failure().is_ok_and(|failure| failure.is_some()))
            {
                Some(flight.clone())
            } else {
                None
            }
        });
        if !state.cancelled.contains_key(&key) && state.cancelled.len() >= MAX_ENTRIES {
            return Err(error("SOURCE_ART_BUDGET_EXCEEDED"));
        }
        state.cancelled.insert(key, Instant::now());
        self.changed.notify_all();
        Ok(final_flight)
    }
    pub fn shutdown(&self) -> Result<Vec<Arc<Flight>>, EditorError> {
        let mut state = lock(&self.state)?;
        state.shutdown = true;
        for ticket in state.tickets.values() {
            ticket.fail(error("SOURCE_ART_STOPPED"))?;
        }
        self.changed.notify_all();
        Ok(state.flights.values().cloned().collect())
    }
    pub fn drain(&self) -> Result<(), EditorError> {
        let mut state = lock(&self.state)?;
        // Dropping a worker's final Arc is not a registry notification. A short
        // timed wait also observes that final release, without holding locks
        // needed by completion or warm/draining ticket cleanup.
        while !state.flights.is_empty()
            || !state.tickets.is_empty()
            || state
                .cache
                .values()
                .any(|cached| Arc::strong_count(&cached.value) != 1)
        {
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(50))
                .map_err(|_| error("INTERNAL_ERROR"))?
                .0;
        }
        state.cache.clear();
        state.bytes = 0;
        state.cancelled.clear();
        Ok(())
    }
    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.state.lock().unwrap().queued.len()
    }
    #[cfg(test)]
    pub fn stats(&self) -> (usize, usize, usize, usize) {
        let state = self.state.lock().unwrap();
        (
            state.completed,
            state.cache.len(),
            state.bytes,
            state.flights.len(),
        )
    }
}
