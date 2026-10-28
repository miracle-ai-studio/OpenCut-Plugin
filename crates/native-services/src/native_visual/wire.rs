//! Bounded binary transport for the dedicated renderer worker, never CLI RPC.
use super::*;
use serde_json::Value;
use std::io::{Read, Write};
pub const DATA_BYTES: usize = 65536;
pub const META_BYTES: usize = 2 * 1024 * 1024;
pub struct Packet {
    pub meta: Value,
    pub data: Vec<u8>,
}
pub fn error(message: impl Into<String>) -> EditorError {
    EditorError::new("VISUAL_PROTOCOL_ERROR", message)
}
pub fn send(output: &mut dyn Write, meta: Value, data: &[u8]) -> Result<(), EditorError> {
    let header = serde_json::to_vec(&meta).map_err(|_| error("Invalid native packet"))?;
    if header.is_empty() || header.len() > META_BYTES || data.len() > DATA_BYTES {
        return Err(error("Native packet exceeds admission"));
    }
    output
        .write_all(&(header.len() as u32).to_le_bytes())
        .and_then(|_| output.write_all(&(data.len() as u32).to_le_bytes()))
        .and_then(|_| output.write_all(&header))
        .and_then(|_| output.write_all(data))
        .and_then(|_| output.flush())
        .map_err(|_| error("Native worker pipe closed"))
}
pub fn read(input: &mut dyn Read) -> Result<Packet, EditorError> {
    let mut lengths = [0u8; 8];
    input
        .read_exact(&mut lengths)
        .map_err(|_| error("Native worker pipe closed"))?;
    let meta = u32::from_le_bytes(lengths[..4].try_into().unwrap()) as usize;
    let data = u32::from_le_bytes(lengths[4..].try_into().unwrap()) as usize;
    if meta == 0 || meta > META_BYTES || data > DATA_BYTES {
        return Err(error("Native packet exceeds admission"));
    }
    let mut header = vec![0; meta];
    let mut bytes = vec![0; data];
    input
        .read_exact(&mut header)
        .and_then(|_| input.read_exact(&mut bytes))
        .map_err(|_| error("Truncated native packet"))?;
    let meta = serde_json::from_slice(&header).map_err(|_| error("Invalid native packet JSON"))?;
    Ok(Packet { meta, data: bytes })
}
pub fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, EditorError> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| error("Missing native text field"))
}
pub fn integer(v: &Value, key: &str) -> Result<u64, EditorError> {
    v.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| error("Missing native integer field"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_pixels_round_trip_without_text_expansion_and_oversize_is_rejected() {
        let bytes: Vec<u8> = (0..DATA_BYTES).map(|n| n as u8).collect();
        let meta = serde_json::json!({"type":"rgba","frame":7,"offset":0});
        let mut wire = Vec::new();
        send(&mut wire, meta.clone(), &bytes).unwrap();
        assert!(wire.len() < DATA_BYTES + 128);
        let packet = read(&mut wire.as_slice()).unwrap();
        assert_eq!(packet.meta, meta);
        assert_eq!(packet.data, bytes);
        let mut oversized = Vec::new();
        oversized.extend_from_slice(&((META_BYTES + 1) as u32).to_le_bytes());
        oversized.extend_from_slice(&0u32.to_le_bytes());
        assert!(read(&mut oversized.as_slice()).is_err());
        assert!(read(&mut &wire[..wire.len() - 1]).is_err());
    }
}
