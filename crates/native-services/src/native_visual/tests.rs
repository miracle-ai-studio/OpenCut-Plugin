//! One serial lifecycle check using only owned native worker processes.
use super::{monitor::Monitor, process::NativeProcess};
use crate::classic_visual::ClassicVisualProcessConfig;
use opencut_contracts::EditorError;
use opencut_editor_application::media_frame_jobs::LocalVisualExecutionPolicy;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(unix)]
fn gone(pid: u32) {
    assert_eq!(
        unsafe { libc::kill(pid as i32, 0) },
        -1,
        "Owned native worker {pid} remains alive"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
#[cfg(unix)]
#[ignore = "Requires OPENCUT_RENDER_WORKER; run serially with native pixel acceptance"]
fn cancellation_revocation_deadline_and_rss_reap_the_owned_worker() {
    use std::os::unix::process::CommandExt;
    let worker = std::env::var_os("OPENCUT_RENDER_WORKER").expect("Integrated native worker");
    let policy = LocalVisualExecutionPolicy {
        max_frame_pixels: 1024,
        max_decoded_pixels: 1024,
        max_gpu_bytes: 32 * 1024 * 1024,
        max_rgba_bytes: 32 * 1024 * 1024,
        max_encoded_bytes: 1024 * 1024,
        max_output_bytes: 1024 * 1024,
        max_rss_bytes: 2 * 1024 * 1024 * 1024,
        deadline_ms: 30_000,
    };
    for code in ["JOB_CANCELLED", "NOT_AUTHORIZED"] {
        let process = NativeProcess::start(
            ClassicVisualProcessConfig {
                worker: worker.clone().into(),
                deadline: Duration::from_secs(30),
                policy: policy.clone(),
                strict_cgroup: None,
            },
            Duration::from_secs(30),
            None,
            &mut || Ok(()),
        )
        .unwrap();
        let pid = process.runtime().worker_pid;
        let error = process
            .finish(&mut || Err(EditorError::new(code, "Revoked owned execution")))
            .unwrap_err();
        assert_eq!(error.code, code);
        gone(pid);
    }
    // Leave the worker waiting on its private initialization pipe. Both limits
    // must terminate a stalled native call without cooperation from the child.
    for (rss, duration, code) in [
        (u64::MAX, Duration::from_millis(100), "VISUAL_TIME_LIMIT"),
        (1, Duration::from_secs(30), "VISUAL_BUDGET_EXCEEDED"),
    ] {
        let child = Command::new(&worker)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut monitor = Monitor::new(child, rss, duration, None);
        let pid = monitor.pid();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Err(error) = monitor.check() {
                assert_eq!(error.code, code);
                break;
            }
            assert!(Instant::now() < deadline, "Parent failed to enforce {code}");
            std::thread::sleep(Duration::from_millis(10));
        }
        monitor.stop();
        gone(pid);
    }
}
