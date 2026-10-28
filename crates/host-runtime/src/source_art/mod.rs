//! Bounded ephemeral source-art assembly. Only application ports select source
//! clocks; this host owns shared decoder lifetime and authorization checkpoints.
mod cache;
#[cfg(test)]
mod tests;
use crate::{Automation, ExecutionContext};
use cache::*;
use opencut_contracts::{EditorError, source_art::*, source_art_waveform_display::*};
use opencut_editor_application::{
    source_art::*,
    source_art_waveform_display::{prepare_waveform_display, project_waveform_display},
};
use opencut_native_services::source_art::SourceArtProcessExecutor;
use opencut_sqlite_store::EditorApplication;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};

fn error(code: &str) -> EditorError {
    EditorError::new(
        code,
        "Source art is unavailable, cancelled or outside its authorized budget",
    )
}
fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>, EditorError> {
    value.lock().map_err(|_| error("INTERNAL_ERROR"))
}
fn registries() -> &'static Mutex<std::collections::HashMap<PathBuf, Arc<Registry>>> {
    static REGISTRIES: OnceLock<Mutex<std::collections::HashMap<PathBuf, Arc<Registry>>>> =
        OnceLock::new();
    REGISTRIES.get_or_init(Mutex::default)
}
fn registry(database: &Path) -> Result<(PathBuf, Arc<Registry>), EditorError> {
    let database = std::fs::canonicalize(database).map_err(|_| error("STORAGE_ERROR"))?;
    let mut registries = lock(registries())?;
    if let Some(registry) = registries.get(&database) {
        return Ok((database, registry.clone()));
    }
    if registries.len() >= 32 {
        return Err(error("SOURCE_ART_BUDGET_EXCEEDED"));
    }
    let registry = Arc::new(Registry::default());
    registries.insert(database.clone(), registry.clone());
    Ok((database, registry))
}
fn authorize(app: &EditorApplication, prepared: &PreparedSourceArt) -> Result<(), EditorError> {
    app.revalidate_source_art(
        &prepared.request().project_id,
        &prepared.request().revision,
        prepared.authority(),
    )
}
pub(crate) fn get(
    host: &mut Automation,
    input: SourceArtRequest,
) -> Result<SourceArtResponse, EditorError> {
    host.app.set_execution_context(ExecutionContext::for_caller(
        host.source,
        host.caller.clone(),
    ));
    let prepared = Arc::new(prepare_source_art(&host.app, &input)?);
    execute_projection(host, prepared.clone(), |decoded| {
        project_source_art(&prepared, decoded)
    })
}

pub(crate) fn waveform_display(
    host: &mut Automation,
    input: SourceArtWaveformDisplayRequest,
) -> Result<SourceArtWaveformDisplayResponse, EditorError> {
    host.app.set_execution_context(ExecutionContext::for_caller(
        host.source,
        host.caller.clone(),
    ));
    let prepared = prepare_waveform_display(&host.app, &input)?;
    execute_projection(host, Arc::new(prepared.source_art().clone()), |decoded| {
        project_waveform_display(&prepared, decoded)
    })
}

// One decoder/cache/cancellation lifecycle; only the pure typed projection
// differs. The raw key intentionally excludes both viewport and element gain.
fn execute_projection<T>(
    host: &Automation,
    prepared: Arc<PreparedSourceArt>,
    project: impl FnOnce(&DecodedSourceArt) -> Result<T, EditorError>,
) -> Result<T, EditorError> {
    let mut config =
        crate::source_art_runtime::configured(prepared.policy(), &prepared.request().query)?;
    if prepared.thumbnail_time().is_some() {
        config.identity.set_algorithm(SOURCE_FRAME_ALGORITHM);
    }
    let key = source_art_decode_key(&prepared, &config.identity)?;
    let (database, registry) = registry(&host.database)?;
    let ticket_key = TicketKey::new(&prepared);
    let ticket = Arc::new(Ticket::new(prepared.clone()));
    let admitted = await_admission(&host.app, &registry, &key, &ticket)?;
    match admitted {
        Admission::Draining(_) | Admission::Queued => {
            unreachable!("waiting admission was consumed above")
        }
        Admission::Cached(decoded) => {
            publish_cached(&host.app, &registry, &ticket, decoded, project)
        }
        Admission::Flight { flight, start } => {
            if start {
                let selected = flight.clone();
                let owner = registry.clone();
                let root = host.app.asset_store_root().to_path_buf();
                let worker_key = key.clone();
                let spawned = std::thread::Builder::new()
                    .name("opencut-source-art".into())
                    .spawn(move || {
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                let app = EditorApplication::open_managed(&database)?;
                                let mut checkpoint = FlightCheckpoint {
                                    flight: selected.clone(),
                                    app,
                                };
                                checkpoint.checkpoint()?;
                                let executor = SourceArtProcessExecutor::new(config.clone(), root)?;
                                let decoded =
                                    executor.execute(&selected.prepared, &mut checkpoint)?;
                                validate_decoded_source_art(&selected.prepared, &decoded)?;
                                if decoded.identity() != &config.identity {
                                    return Err(error("ARTIFACT_VERIFICATION_FAILED"));
                                }
                                checkpoint.checkpoint()?;
                                Ok(Arc::new(decoded))
                            }))
                            .unwrap_or_else(|_| Err(error("INTERNAL_ERROR")));
                        let _ = owner.complete(&worker_key, &selected, outcome);
                    });
                if spawned.is_err() {
                    registry.complete(&key, &flight, Err(error("IO_ERROR")))?;
                }
            }
            let result = wait(&host.app, &flight, &ticket).and_then(|decoded| {
                authorize(&host.app, &prepared)?;
                project(&decoded)
            });
            registry.release(&ticket_key, &ticket, &flight)?;
            result
        }
    }
}

// Each waiting request retains its original authority, deadline and cancellation
// identity. Waiting holds no decoder credit and never retries accepted work.
fn await_admission(
    app: &EditorApplication,
    registry: &Registry,
    key: &str,
    ticket: &Arc<Ticket>,
) -> Result<Admission, EditorError> {
    let ticket_key = TicketKey::new(&ticket.prepared);
    loop {
        let attempt = (|| {
            authorize(app, &ticket.prepared)?;
            if let Some(err) = ticket.failure()? {
                return Err(err);
            }
            if ticket.started.elapsed()
                >= Duration::from_millis(ticket.prepared.policy().deadline_ms)
            {
                return Err(error("SOURCE_ART_DEADLINE_EXCEEDED"));
            }
            registry.admit(key.into(), ticket_key.clone(), ticket.clone())
        })();
        let outcome = match attempt {
            Ok(Admission::Queued) => registry.wait_for_change(),
            Ok(Admission::Draining(flight)) => {
                let result = wait_cleanup(app, &flight, ticket);
                registry.release_cached(&ticket_key, ticket)?;
                result
            }
            Ok(admitted) => return Ok(admitted),
            Err(err) => Err(err),
        };
        if let Err(err) = outcome {
            let _ = registry.release_cached(&ticket_key, ticket);
            return Err(err);
        }
    }
}

fn publish_cached<T>(
    app: &EditorApplication,
    registry: &Registry,
    ticket: &Arc<Ticket>,
    decoded: Arc<DecodedSourceArt>,
    project: impl FnOnce(&DecodedSourceArt) -> Result<T, EditorError>,
) -> Result<T, EditorError> {
    let result = authorize(app, &ticket.prepared).and_then(|_| project(&decoded));
    drop(decoded);
    registry.release_cached(&TicketKey::new(&ticket.prepared), ticket)?;
    result
}

fn wait_cleanup(
    app: &EditorApplication,
    flight: &Flight,
    ticket: &Ticket,
) -> Result<(), EditorError> {
    loop {
        authorize(app, &ticket.prepared)?;
        if let Some(error) = ticket.failure()? {
            return Err(error);
        }
        if ticket.started.elapsed() >= Duration::from_millis(ticket.prepared.policy().deadline_ms) {
            return Err(error("SOURCE_ART_DEADLINE_EXCEEDED"));
        }
        let result = lock(&flight.result)?;
        if result.is_some() {
            return Ok(());
        }
        let _ = flight
            .changed
            .wait_timeout(result, Duration::from_millis(50))
            .map_err(|_| error("INTERNAL_ERROR"))?;
    }
}

fn wait(
    app: &EditorApplication,
    flight: &Flight,
    ticket: &Ticket,
) -> Result<Arc<DecodedSourceArt>, EditorError> {
    loop {
        if let Err(err) = authorize(app, &ticket.prepared) {
            ticket.fail(err)?;
        }
        if ticket.started.elapsed() > Duration::from_millis(ticket.prepared.policy().deadline_ms) {
            ticket.fail(error("SOURCE_ART_DEADLINE_EXCEEDED"))?;
        }
        let result = lock(&flight.result)?;
        if let Some(result) = result.as_ref() {
            if let Some(err) = ticket.failure()? {
                return Err(err);
            }
            return result.clone();
        }
        if let Some(err) = ticket.failure()? {
            // A cancelled participant may leave a shared decode. The final
            // participant observes completion only after native process reap.
            if flight.has_other_active(ticket)? {
                return Err(err);
            }
        }
        let _ = flight
            .changed
            .wait_timeout(result, Duration::from_millis(50))
            .map_err(|_| error("INTERNAL_ERROR"))?;
    }
}
struct FlightCheckpoint {
    flight: Arc<Flight>,
    app: EditorApplication,
}
impl SourceArtCheckpoint for FlightCheckpoint {
    fn checkpoint(&mut self) -> Result<(), EditorError> {
        let tickets: Vec<_> = lock(&self.flight.tickets)?.values().cloned().collect();
        let mut active = false;
        for ticket in tickets {
            if ticket.failure()?.is_some() {
                continue;
            }
            if ticket.started.elapsed()
                > Duration::from_millis(ticket.prepared.policy().deadline_ms)
            {
                ticket.fail(error("SOURCE_ART_DEADLINE_EXCEEDED"))?;
            } else if let Err(err) = authorize(&self.app, &ticket.prepared) {
                ticket.fail(err)?;
            } else {
                active = true;
            }
        }
        if active {
            Ok(())
        } else {
            Err(error("SOURCE_ART_CANCELLED"))
        }
    }
}
pub(crate) fn cancel(
    host: &mut Automation,
    input: SourceArtCancel,
) -> Result<SourceArtCancelResponse, EditorError> {
    validate_cancel(&input)?;
    host.app.set_execution_context(ExecutionContext::for_caller(
        host.source,
        host.caller.clone(),
    ));
    let selected = host
        .app
        .fixed_source_art_snapshot(&input.project_id, &input.revision)?;
    host.app
        .revalidate_source_art(&input.project_id, &input.revision, &selected.authority)?;
    let (_, registry) = registry(&host.database)?;
    if let Some(flight) = registry.cancel(TicketKey {
        scope: selected.scope,
        revision: input.revision,
        request_id: input.request_id,
    })? {
        let mut result = lock(&flight.result)?;
        while result.is_none() {
            result = flight
                .changed
                .wait(result)
                .map_err(|_| error("INTERNAL_ERROR"))?;
        }
    }
    Ok(SourceArtCancelResponse {
        cancel_requested: true,
    })
}
pub(crate) fn shutdown(database: &Path) -> Result<(), EditorError> {
    let database = std::fs::canonicalize(database).map_err(|_| error("STORAGE_ERROR"))?;
    let registry = lock(registries())?.get(&database).cloned();
    let Some(registry) = registry else {
        return Ok(());
    };
    let flights = registry.shutdown()?;
    for flight in flights {
        let mut result = lock(&flight.result)?;
        while result.is_none() {
            result = flight
                .changed
                .wait(result)
                .map_err(|_| error("INTERNAL_ERROR"))?;
        }
    }
    registry.drain()?;
    let mut registries = lock(registries())?;
    if registries
        .get(&database)
        .is_some_and(|current| Arc::ptr_eq(current, &registry))
    {
        registries.remove(&database);
    }
    Ok(())
}

pub(crate) fn source_frame(
    host: &mut Automation,
    input: opencut_contracts::source_evidence::SourceFrameRequest,
) -> Result<opencut_contracts::source_evidence::SourceFrameResponse, EditorError> {
    host.app.set_execution_context(ExecutionContext::for_caller(
        host.source,
        host.caller.clone(),
    ));
    let prepared = Arc::new(prepare_source_frame(&host.app, &input)?);
    let response = execute_projection(host, prepared.clone(), |decoded| {
        project_source_art(&prepared, decoded)
    })?;
    Ok(opencut_contracts::source_evidence::SourceFrameResponse {
        project_id: response.project_id,
        revision: response.revision,
        asset_id: response.asset_id,
        version: response.version,
        identity: response.identity,
        requested_time: input.time,
        data: response.data,
    })
}
