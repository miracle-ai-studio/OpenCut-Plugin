//! Shared trusted worker installation; no media policy or GPU initialization.
use opencut_contracts::EditorError;
use std::{
    env,
    io::Read,
    path::{Path, PathBuf},
};
fn unavailable() -> EditorError {
    EditorError::new("CAPABILITY_UNAVAILABLE", "NATIVE_RENDERER_UNAVAILABLE")
}
pub(crate) fn worker_at(path: &Path) -> Result<PathBuf, EditorError> {
    if !path.is_absolute() {
        return Err(unavailable());
    }
    let path = opencut_native_services::classic_video::resolve_tool(path.into())
        .map_err(|_| unavailable())?;
    let mut header = [0u8; 4];
    std::fs::File::open(&path)
        .and_then(|mut f| f.read_exact(&mut header))
        .map_err(|_| unavailable())?;
    if !matches!(
        header,
        [0x7f, b'E', b'L', b'F']
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
    ) {
        return Err(unavailable());
    }
    Ok(path)
}
pub(crate) fn worker() -> Result<PathBuf, EditorError> {
    let path = match env::var_os("OPENCUT_RENDER_WORKER") {
        Some(path) => PathBuf::from(path),
        None => env::current_exe()
            .ok()
            .as_deref()
            .and_then(Path::parent)
            .map(|p| p.join("opencut-render-worker"))
            .ok_or_else(unavailable)?,
    };
    worker_at(&path)
}
