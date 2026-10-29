//! Inode-held private output helpers shared by V4 media workers.
use opencut_contracts::EditorError;
use opencut_media::CancellationToken;
use opencut_sqlite_store::JobRecord;
use opencut_storage_budget::AttemptLease;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

pub(super) fn attempt_directory(root: &Path, job: &JobRecord) -> Result<PathBuf, EditorError> {
    let mut components = Path::new(&job.job_id).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(error(
            "INVALID_ARGUMENT",
            "Invalid private media job identity",
        ));
    }
    real_directory(root)?;
    let root = fs::canonicalize(root).map_err(io_error)?;
    let job_directory = root.join(&job.job_id);
    real_directory(&job_directory)?;
    let attempt_directory = job_directory.join(format!("attempt-{}", job.attempt));
    real_directory(&attempt_directory)?;
    if fs::canonicalize(&attempt_directory).map_err(io_error)? != attempt_directory {
        return Err(error(
            "STORAGE_BUDGET_ERROR",
            "Private output roots changed during creation",
        ));
    }
    Ok(attempt_directory)
}
fn real_directory(path: &Path) -> Result<(), EditorError> {
    match fs::create_dir(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(io_error(error)),
    }
    if !fs::symlink_metadata(path).map_err(io_error)?.is_dir() {
        return Err(error(
            "STORAGE_BUDGET_ERROR",
            "Private output roots must be real directories",
        ));
    }
    Ok(())
}
pub(super) fn check_attempt(attempt: &AttemptLease, directory: &Path) -> Result<(), EditorError> {
    if !attempt.still_names_canonical(directory).map_err(io_error)? {
        return Err(error(
            "JOB_LEASE_LOST",
            "Private output attempt was replaced",
        ));
    }
    Ok(())
}
pub(super) fn check_output_name(
    attempt: &AttemptLease,
    name: &OsStr,
    file: &File,
) -> Result<(), EditorError> {
    let named = attempt.open_readonly_child(name).map_err(io_error)?;
    let named = named.metadata().map_err(io_error)?;
    let held = file.metadata().map_err(io_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if named.is_file() && named.dev() == held.dev() && named.ino() == held.ino() {
            return Ok(());
        }
    }
    #[cfg(not(unix))]
    let _ = (named, held);
    Err(error(
        "ARTIFACT_VERIFICATION_FAILED",
        "Private media output inode changed",
    ))
}
pub(super) fn file_hash(
    file: &mut File,
    expected: u64,
    cancel: &CancellationToken,
    mut progress: impl FnMut() -> Result<(), EditorError>,
) -> Result<String, EditorError> {
    if file.metadata().map_err(io_error)?.len() != expected {
        return Err(error(
            "ARTIFACT_VERIFICATION_FAILED",
            "Media output length differs from admission",
        ));
    }
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65_536];
    let mut observed = 0u64;
    while observed < expected {
        cancelled(cancel)?;
        let count = file
            .read(&mut buffer[..(expected - observed).min(65_536) as usize])
            .map_err(io_error)?;
        if count == 0 {
            return Err(error(
                "ARTIFACT_VERIFICATION_FAILED",
                "Media output was truncated during verification",
            ));
        }
        observed += count as u64;
        digest.update(&buffer[..count]);
        progress()?;
    }
    if file.metadata().map_err(io_error)?.len() != expected {
        return Err(error(
            "ARTIFACT_VERIFICATION_FAILED",
            "Media output length changed during verification",
        ));
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub(super) fn cancelled(cancel: &CancellationToken) -> Result<(), EditorError> {
    if cancel.is_cancelled() {
        return Err(error("JOB_CANCELLED", "Media task was cancelled"));
    }
    Ok(())
}
pub(super) fn error(code: &str, message: &str) -> EditorError {
    EditorError::new(code, message)
}
pub(super) fn io_error(_: std::io::Error) -> EditorError {
    error("MEDIA_IO_ERROR", "Private output output I/O failed")
}
