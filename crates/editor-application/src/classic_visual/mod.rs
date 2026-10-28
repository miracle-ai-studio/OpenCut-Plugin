//! Internal fixed-frame execution boundary. Resources and private output are
//! supplied by the host; no path, process or public authorization lives here.
mod identity;
mod protocol;
mod session;
pub use identity::*;
use opencut_contracts::EditorError;
pub use protocol::*;
pub use session::*;

pub trait ClassicVisualResources {
    fn resolve(
        &mut self,
        resource: &VisualResource,
        handle: &str,
    ) -> Result<(String, u64), EditorError>;
    fn read(
        &mut self,
        transfer: &str,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), EditorError>;
    fn release(&mut self, transfer: &str) -> Result<(), EditorError>;
    fn checkpoint(&mut self) -> Result<(), EditorError>;
}
/// The sink receives sequential row-major RGBA bytes, not published artifacts.
pub trait ClassicVisualRgbaSink {
    fn chunk(&mut self, offset_bytes: u64, bytes: &[u8]) -> Result<(), EditorError>;
}
pub trait ClassicVisualExecutor {
    fn execute(
        &self,
        session: &mut ClassicVisualSession<'_>,
        resources: &mut dyn ClassicVisualResources,
        handles: &[ClassicVisualResourceHandle],
        sink: &mut dyn ClassicVisualRgbaSink,
    ) -> Result<ClassicVisualExecutionReceipt, EditorError>;
}
fn invalid(message: impl Into<String>) -> EditorError {
    EditorError::new("INVALID_VISUAL_PLAN", message)
}
fn decimal(value: &str) -> Result<u64, EditorError> {
    crate::classic_audio::decimal(value)
        .map_err(|_| invalid("Expected canonical u64 decimal string"))
}
