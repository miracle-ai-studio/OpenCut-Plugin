//! Actual isolated probe against retained, independently observed containers.
//! Run with OPENCUT_RENDER_WORKER and OPENCUT_PACKET_RATE_REFERENCE, serially.
#![cfg(unix)]
use opencut_contracts::EditorError;
use opencut_native_services::packet_rate;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
fn worker() -> PathBuf {
    PathBuf::from(
        std::env::var_os("OPENCUT_RENDER_WORKER").expect("Set exact integrated native worker"),
    )
}
fn reference() -> Value {
    serde_json::from_slice(
        &std::fs::read(
            std::env::var_os("OPENCUT_PACKET_RATE_REFERENCE")
                .expect("Set retained independent reference manifest"),
        )
        .unwrap(),
    )
    .unwrap()
}
fn owned(worker: &Path) -> Vec<i32> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,pgid=,comm="])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let f: Vec<_> = line.split_whitespace().collect();
            if f.len() != 4 {
                return None;
            }
            let pid = f[0].parse::<i32>().ok()?;
            let parent = f[1].parse::<u32>().ok()?;
            let group = f[2].parse::<i32>().ok()?;
            (parent == std::process::id() && group == pid && Path::new(f[3]) == worker)
                .then_some(pid)
        })
        .collect()
}
#[test]
#[ignore = "Requires the exact integrated native worker and retained independent packet reference"]
fn isolated_native_packet_probe_matches_independent_container_statistics() {
    let worker = worker();
    let refs = reference();
    for case in refs["cases"].as_array().unwrap() {
        let source = Path::new(case["source"].as_str().unwrap());
        let bytes = std::fs::read(source).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            case["source_sha256"].as_str().unwrap()
        );
        let result =
            packet_rate::probe(&worker, source, bytes.len() as u64, &mut || Ok(())).unwrap();
        let expected = &case["expected"];
        assert_eq!(result.schema_version, 1);
        assert_eq!(
            result.packet_count,
            expected["packet_count"].as_u64().unwrap(),
            "{}",
            case["label"]
        );
        match (
            result.average_packet_rate,
            expected["average_packet_rate"].as_f64(),
        ) {
            (Some(actual), Some(expected)) => assert!(
                (actual - expected).abs() <= 1e-12,
                "{}: actual={actual:.17}, expected={expected:.17}",
                case["label"]
            ),
            (None, None) => {}
            other => panic!("{}: {other:?}", case["label"]),
        }
        println!(
            "{}: {}",
            case["label"],
            serde_json::to_string(&result).unwrap()
        );
        assert!(owned(&worker).is_empty());
    }
}
#[test]
#[ignore = "Actual SIGSTOP cancellation against one verified child of this test process"]
fn isolated_packet_probe_cancellation_reaps_a_blocked_child_before_returning() {
    let worker = worker();
    assert!(owned(&worker).is_empty());
    let refs = reference();
    let source = PathBuf::from(refs["cases"][0]["source"].as_str().unwrap());
    let size = std::fs::metadata(&source).unwrap().len();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelled = cancel.clone();
    let executable = worker.clone();
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let result = packet_rate::probe(&executable, &source, size, &mut || {
            if cancelled.load(Ordering::Acquire) {
                Err(EditorError::new("JOB_CANCELLED", "test cancellation"))
            } else {
                Ok(())
            }
        });
        tx.send(result).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        let rows = owned(&worker);
        if let Some(&pid) = rows.first() {
            assert_eq!(rows.len(), 1);
            break pid;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    cancel.store(true, Ordering::Release);
    let result = rx.recv_timeout(Duration::from_secs(5));
    if result.is_err() && owned(&worker).contains(&pid) {
        unsafe { libc::kill(pid, libc::SIGCONT) };
    }
    assert_eq!(result.unwrap().unwrap_err().code, "JOB_CANCELLED");
    thread.join().unwrap();
    assert!(owned(&worker).is_empty());
}
#[test]
#[ignore = "Requires the exact integrated native worker for a live authority fence"]
fn packet_probe_rejects_revocation_without_spawning_or_relabeling_it() {
    let worker = worker();
    let refs = reference();
    let source = Path::new(refs["cases"][0]["source"].as_str().unwrap());
    assert!(owned(&worker).is_empty());
    let result = packet_rate::probe(
        &worker,
        source,
        std::fs::metadata(source).unwrap().len(),
        &mut || Err(EditorError::new("NOT_AUTHORIZED", "revoked")),
    );
    assert_eq!(result.unwrap_err().code, "NOT_AUTHORIZED");
    assert!(owned(&worker).is_empty());
}
