//! Bounded binary audio replies. Large typed sample blocks are serialized once,
//! split into 64KiB packets and parsed directly into the shared Rust facts.
use crate::native_process::{Domain, wire};
use opencut_contracts::EditorError;
use std::io::Write;
pub(crate) const JSON_BYTES: usize = 16 * 1024 * 1024;
pub(crate) fn error(s: impl Into<String>) -> EditorError {
    Domain::Audio.protocol(wire::error(s))
}
struct BoundedJson(Vec<u8>);
impl Write for BoundedJson {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let next = self
            .0
            .len()
            .checked_add(data.len())
            .filter(|n| *n <= JSON_BYTES)
            .ok_or_else(|| std::io::Error::other("Native audio facts exceed admission"))?;
        if next > self.0.capacity() {
            let capacity = next.next_multiple_of(wire::DATA_BYTES).min(JSON_BYTES);
            self.0
                .try_reserve_exact(capacity - self.0.len())
                .map_err(std::io::Error::other)?;
        }
        self.0.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, EditorError> {
    let mut out = BoundedJson(Vec::new());
    serde_json::to_writer(&mut out, value).map_err(|_| {
        EditorError::new(
            "AUDIO_BUDGET_EXCEEDED",
            "Native audio JSON facts exceed the bounded transport",
        )
    })?;
    Ok(out.0)
}
pub(crate) fn reject_failure(packet: &wire::Packet) -> Result<(), EditorError> {
    if packet.meta["type"] == "failed" {
        if !packet.data.is_empty() {
            return Err(error("Failure packet carries unexpected bytes"));
        }
        return Err(EditorError::new(
            wire::text(&packet.meta, "code")?,
            wire::text(&packet.meta, "message")?,
        ));
    }
    Ok(())
}
