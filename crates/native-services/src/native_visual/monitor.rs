//! Parent-owned limits cover codec/driver allocations and stalled native calls.
use super::*;
use std::{
    process::{Child, ExitStatus},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

struct State {
    child: Child,
    done: bool,
    fault: Option<EditorError>,
    peak: u64,
    frame_deadline: Option<Instant>,
}
pub(super) struct Monitor {
    state: Arc<Mutex<State>>,
    thread: Option<JoinHandle<()>>,
    started: Instant,
}
impl Monitor {
    pub fn new(
        child: Child,
        max_rss: u64,
        deadline: Duration,
        progress: Option<Arc<Mutex<Instant>>>,
    ) -> Self {
        let started = Instant::now();
        let state = Arc::new(Mutex::new(State {
            child,
            done: false,
            fault: None,
            peak: 0,
            frame_deadline: None,
        }));
        let shared = state.clone();
        let thread = std::thread::spawn(move || {
            loop {
                {
                    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                    if state.done {
                        break;
                    }
                    // Reaping and signalling share this mutex. A cached completed
                    // status is never signalled, including during failed cleanup.
                    if state.child.try_wait().ok().flatten().is_some() {
                        state.done = true;
                        break;
                    }
                    let elapsed = progress.as_ref().map_or_else(
                        || started.elapsed(),
                        |p| p.lock().unwrap_or_else(|e| e.into_inner()).elapsed(),
                    );
                    let error = if elapsed >= deadline
                        || state.frame_deadline.is_some_and(|at| Instant::now() >= at)
                    {
                        Some(EditorError::new(
                            "VISUAL_TIME_LIMIT",
                            "Native rendering exceeded its captured deadline",
                        ))
                    } else {
                        match resident(state.child.id()) {
                            Ok(bytes) => {
                                state.peak = state.peak.max(bytes);
                                (bytes > max_rss).then(|| {
                                    EditorError::new(
                                        "VISUAL_BUDGET_EXCEEDED",
                                        "Native worker exceeded its RSS admission",
                                    )
                                })
                            }
                            Err(error) => {
                                // The child may exit between try_wait and the
                                // OS memory query. Zombies still answer kill(0)
                                // on macOS but no longer have task counters.
                                // Reap under the same ownership lock before
                                // treating an unavailable counter as a fault.
                                if state.child.try_wait().ok().flatten().is_some() {
                                    state.done = true;
                                    break;
                                }
                                Some(error)
                            }
                        }
                    };
                    if let Some(error) = error {
                        state.fault = Some(error);
                        terminate(&mut state.child);
                        state.done = true;
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
        Self {
            state,
            thread: Some(thread),
            started,
        }
    }
    pub fn pid(&self) -> u32 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .child
            .id()
    }
    pub fn check(&self) -> Result<(), EditorError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(error) = &state.fault {
            Err(EditorError::new(error.code.clone(), error.message.clone()))
        } else {
            Ok(())
        }
    }
    pub fn frame(&self, deadline: Option<Instant>) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frame_deadline = deadline;
    }
    pub fn quota(
        &self,
        maximum: u64,
        strict: bool,
    ) -> opencut_editor_application::classic_visual::VisualQuotaReceipt {
        opencut_editor_application::classic_visual::VisualQuotaReceipt {
            profile: if strict {
                "strict-hard"
            } else {
                "monitored-terminate"
            }
            .into(),
            max_rss_bytes: maximum.to_string(),
            observed_peak_rss_bytes: self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .peak
                .to_string(),
            monitor_interval_ms: 25,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            hard_memory_enforced: strict,
        }
    }
    pub fn reap(&mut self) -> Result<ExitStatus, EditorError> {
        // Keep a bounded wait after the worker acknowledged teardown.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.check()?;
            {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                // Any completed status is reaped while the monitor is excluded.
                match state.child.try_wait() {
                    Ok(Some(status)) => {
                        state.done = true;
                        return Ok(status);
                    }
                    Ok(None) => {}
                    Err(_) => return Err(invalid("Cannot observe native worker exit")),
                }
                if Instant::now() >= deadline {
                    state.done = true;
                    terminate(&mut state.child);
                    return Err(invalid("Native worker did not exit after completion"));
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn stop(&mut self) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.done = true;
            if state.child.try_wait().ok().flatten().is_none() {
                terminate(&mut state.child);
                let _ = state.child.wait();
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop();
    }
}
fn terminate(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}
#[cfg(target_os = "macos")]
fn resident(pid: u32) -> Result<u64, EditorError> {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    // SAFETY: valid output storage with the exact libproc record length.
    let observed = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if observed != size {
        let error = std::io::Error::last_os_error();
        // Exited but unreaped children have no resident memory. A still-live
        // process with unreadable counters is rejected rather than guessed.
        // libproc reports ESRCH as soon as the task disappears, before waitpid
        // and kill(0) necessarily observe process teardown. Do not treat that
        // kernel observation as a failure of an otherwise completed frame.
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(0);
        }
        return Err(invalid(format!(
            "Cannot measure native worker RSS: {error}"
        )));
    }
    Ok(unsafe { info.assume_init() }.pti_resident_size)
}
#[cfg(target_os = "linux")]
fn resident(pid: u32) -> Result<u64, EditorError> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/statm"))
        .map_err(|_| invalid("Cannot measure native worker RSS"))?;
    let pages = text
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| invalid("Invalid native RSS counter"))?;
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size <= 0 {
        return Err(invalid("Cannot identify native page size"));
    }
    pages
        .checked_mul(size as u64)
        .ok_or_else(|| invalid("Native RSS overflow"))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident(_: u32) -> Result<u64, EditorError> {
    Err(invalid("Native RSS monitoring unavailable"))
}
