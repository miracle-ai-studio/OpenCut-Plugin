//! Visual error vocabulary over the single bounded native binary transport.
use crate::native_process::{Domain, wire};
use opencut_contracts::EditorError;
pub use wire::{DATA_BYTES, Packet};
pub fn error(message: impl Into<String>) -> EditorError {
    Domain::Visual.protocol(wire::error(message))
}
pub fn send(
    output: &mut dyn std::io::Write,
    meta: serde_json::Value,
    data: &[u8],
) -> Result<(), EditorError> {
    wire::send(output, meta, data).map_err(|e| Domain::Visual.protocol(e))
}
pub fn read(input: &mut dyn std::io::Read) -> Result<Packet, EditorError> {
    wire::read(input).map_err(|e| Domain::Visual.protocol(e))
}
pub fn text<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str, EditorError> {
    wire::text(value, key).map_err(|e| Domain::Visual.protocol(e))
}
pub fn integer(value: &serde_json::Value, key: &str) -> Result<u64, EditorError> {
    wire::integer(value, key).map_err(|e| Domain::Visual.protocol(e))
}
