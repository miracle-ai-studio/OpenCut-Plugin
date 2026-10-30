//! Bounded audio replies. Preview headers use the shared Rust declaration;
//! dense sample facts keep their exact integer/Float64 bits in binary records.
use crate::native_process::{Domain, wire};
use opencut_composition::scene::{PreviewAudioBlock, PreviewSourceSample};
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

// Private host/worker framing, never persisted or exposed through a public DTO.
// A sample is: present:u8, segment:u32, lower:u64, upper:u64, fraction:f64,
// amplitude:f64, all little-endian. Missing positions have 28 zero bytes.
const SAMPLE_BYTES: usize = 37;
fn preview_work(block: &PreviewAudioBlock) -> Result<usize, EditorError> {
    if block.frame_count == 0
        || block.frame_count > 4096
        || block.voices.len() > 32
        || block.receipts.len() > 256
    {
        return Err(error("Native preview facts exceed their work envelope"));
    }
    block.voices.iter().try_fold(0usize, |work, voice| {
        if voice.frame_count == 0
            || voice
                .block_offset
                .checked_add(voice.frame_count)
                .is_none_or(|n| n > block.frame_count)
        {
            return Err(error("Native preview voice exceeds its block"));
        }
        work.checked_add(voice.frame_count as usize)
            .filter(|n| *n <= 65536)
            .ok_or_else(|| error("Native preview sample work exceeds admission"))
    })
}
pub(super) fn encode_preview(mut block: PreviewAudioBlock) -> Result<Vec<u8>, EditorError> {
    let work = preview_work(&block)?;
    let mut tables = Vec::with_capacity(block.voices.len());
    for voice in &mut block.voices {
        if voice.positions.len() != voice.frame_count as usize
            || voice.amplitudes.len() != voice.frame_count as usize
        {
            return Err(error(
                "Native preview sample lengths differ from their voice",
            ));
        }
        tables.push((
            std::mem::take(&mut voice.positions),
            std::mem::take(&mut voice.amplitudes),
        ));
    }
    // Dense arrays are moved out, not cloned or converted to serde_json::Value.
    let header = encode(&block)?;
    let length = 8usize
        .checked_add(header.len())
        .and_then(|n| n.checked_add(work * SAMPLE_BYTES))
        .filter(|n| *n <= JSON_BYTES)
        .ok_or_else(|| error("Native preview facts exceed transport admission"))?;
    let mut out = Vec::new();
    out.try_reserve_exact(length)
        .map_err(|_| error("Cannot allocate admitted preview facts"))?;
    out.extend_from_slice(b"OCAP");
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    for (positions, amplitudes) in tables {
        for (position, amplitude) in positions.into_iter().zip(amplitudes) {
            if !amplitude.is_finite() || position.as_ref().is_some_and(|p| !p.fraction.is_finite())
            {
                return Err(error("Non-finite compiled preview fact"));
            }
            if let Some(p) = position {
                out.push(1);
                out.extend_from_slice(&p.segment_id.to_le_bytes());
                out.extend_from_slice(&p.lower.to_le_bytes());
                out.extend_from_slice(&p.upper.to_le_bytes());
                out.extend_from_slice(&p.fraction.to_le_bytes());
            } else {
                out.extend_from_slice(&[0; 29]);
            }
            out.extend_from_slice(&amplitude.to_le_bytes());
        }
    }
    Ok(out)
}
pub(super) fn decode_preview(data: &[u8]) -> Result<PreviewAudioBlock, EditorError> {
    if data.len() < 8 || data.len() > JSON_BYTES || &data[..4] != b"OCAP" {
        return Err(error("Invalid native preview fact envelope"));
    }
    let header_length = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let header_end = 8usize
        .checked_add(header_length)
        .filter(|n| *n <= data.len())
        .ok_or_else(|| error("Invalid native preview header length"))?;
    let mut block: PreviewAudioBlock = serde_json::from_slice(&data[8..header_end])
        .map_err(|_| error("Native preview header differs from its shared Rust declaration"))?;
    let work = preview_work(&block)?;
    if data.len() - header_end != work * SAMPLE_BYTES
        || block
            .voices
            .iter()
            .any(|v| !v.positions.is_empty() || !v.amplitudes.is_empty())
    {
        return Err(error(
            "Native preview sample extent differs from its header",
        ));
    }
    let mut samples = data[header_end..].chunks_exact(SAMPLE_BYTES);
    for voice in &mut block.voices {
        voice
            .positions
            .try_reserve_exact(voice.frame_count as usize)
            .map_err(|_| error("Cannot allocate admitted preview positions"))?;
        voice
            .amplitudes
            .try_reserve_exact(voice.frame_count as usize)
            .map_err(|_| error("Cannot allocate admitted preview amplitudes"))?;
        for _ in 0..voice.frame_count {
            let bytes = samples.next().expect("validated preview sample count");
            let position = match bytes[0] {
                0 if bytes[1..29].iter().all(|b| *b == 0) => None,
                1 => Some(PreviewSourceSample {
                    segment_id: u32::from_le_bytes(bytes[1..5].try_into().unwrap()),
                    lower: u64::from_le_bytes(bytes[5..13].try_into().unwrap()),
                    upper: u64::from_le_bytes(bytes[13..21].try_into().unwrap()),
                    fraction: f64::from_le_bytes(bytes[21..29].try_into().unwrap()),
                }),
                _ => return Err(error("Invalid native preview position tag")),
            };
            let amplitude = f64::from_le_bytes(bytes[29..37].try_into().unwrap());
            if !amplitude.is_finite() || position.as_ref().is_some_and(|p| !p.fraction.is_finite())
            {
                return Err(error("Non-finite compiled preview fact"));
            }
            voice.positions.push(position);
            voice.amplitudes.push(amplitude);
        }
    }
    // Fixed identity, voice order, interpolation bounds and PCM read authority
    // are still validated by PreviewMaster before any sample is mixed.
    Ok(block)
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

#[cfg(test)]
mod tests {
    use super::*;
    use opencut_composition::scene::{
        CLASSIC_REALTIME_AUDIO_PROFILE, PreviewAudioIdentity, PreviewAudioVoiceBlock,
    };
    fn block(frames: u32) -> PreviewAudioBlock {
        PreviewAudioBlock {
            identity: PreviewAudioIdentity {
                project_id: "project".into(),
                revision: "fixed".into(),
                sequence_id: "scene".into(),
                profile: CLASSIC_REALTIME_AUDIO_PROFILE,
                sample_rate: 48000,
                channels: 2,
            },
            start_sample: u64::MAX - u64::from(frames),
            frame_count: frames,
            receipts: vec![],
            voices: vec![PreviewAudioVoiceBlock {
                element_id: "voice".into(),
                block_offset: 0,
                frame_count: frames,
                positions: (0..frames)
                    .map(|i| {
                        (i % 3 != 0).then_some(PreviewSourceSample {
                            segment_id: i,
                            lower: u64::MAX - 1,
                            upper: u64::MAX,
                            fraction: if i % 2 == 0 {
                                -0.0
                            } else {
                                f64::from_bits(0x3fd5555555555555)
                            },
                        })
                    })
                    .collect(),
                amplitudes: (0..frames)
                    .map(|i| match i % 3 {
                        0 => -0.0,
                        1 => f64::from_bits(1),
                        _ => f64::from_bits(0x3fefffffffffffff),
                    })
                    .collect(),
            }],
        }
    }
    #[test]
    fn preview_binary_preserves_integer_clocks_float_bits_and_missing_samples() {
        let original = block(4096);
        let bytes = encode_preview(original.clone()).unwrap();
        let actual = decode_preview(&bytes).unwrap();
        assert_eq!(original, actual);
        for (a, b) in original.voices[0]
            .amplitudes
            .iter()
            .zip(&actual.voices[0].amplitudes)
        {
            assert_eq!(a.to_bits(), b.to_bits());
        }
        for (a, b) in original.voices[0]
            .positions
            .iter()
            .zip(&actual.voices[0].positions)
        {
            if let (Some(a), Some(b)) = (a, b) {
                assert_eq!(a.fraction.to_bits(), b.fraction.to_bits());
            }
        }
        // Hand-authored bytes independently fix endian, tag and field layout.
        let offset = 8 + u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let record = &bytes[offset + SAMPLE_BYTES..offset + 2 * SAMPLE_BYTES];
        assert_eq!(&record[..5], &[1, 1, 0, 0, 0]);
        assert_eq!(&record[5..13], &[254, 255, 255, 255, 255, 255, 255, 255]);
        assert_eq!(&record[13..21], &[255; 8]);
        assert_eq!(&record[21..29], &[85, 85, 85, 85, 85, 85, 213, 63]);
        assert_eq!(&record[29..37], &[1, 0, 0, 0, 0, 0, 0, 0]);
        let mut empty = block(1);
        empty.voices.clear();
        assert_eq!(
            decode_preview(&encode_preview(empty.clone()).unwrap()).unwrap(),
            empty
        );
    }
    #[test]
    fn preview_binary_rejects_truncation_tags_nonfinite_and_work_overflow() {
        let bytes = encode_preview(block(3)).unwrap();
        for length in 0..bytes.len() {
            assert!(decode_preview(&bytes[..length]).is_err());
        }
        let mut bad = bytes.clone();
        bad.push(0);
        assert!(decode_preview(&bad).is_err());
        let offset = 8 + u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        for (at, value) in [(0, 2), (1, 1)] {
            let mut bad = bytes.clone();
            bad[offset + at] = value;
            assert!(decode_preview(&bad).is_err());
        }
        for at in [offset + 29, offset + SAMPLE_BYTES + 21] {
            let mut bad = bytes.clone();
            bad[at..at + 8].copy_from_slice(&f64::NAN.to_le_bytes());
            assert!(decode_preview(&bad).is_err());
        }
        let mut bad = bytes.clone();
        bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_preview(&bad).is_err());
        let mut oversized = block(4096);
        oversized.voices = vec![oversized.voices[0].clone(); 17];
        assert!(encode_preview(oversized).is_err());
        let mut invalid = block(3);
        invalid.voices[0].amplitudes[0] = f64::INFINITY;
        assert!(encode_preview(invalid).is_err());
    }
    #[test]
    #[ignore = "component timing only; run serially with --nocapture"]
    fn preview_fact_transport_component() {
        use std::{hint::black_box, time::Instant};
        let original = block(4096);
        for binary in [false, true, true, false] {
            let start = Instant::now();
            let mut length = 0;
            for _ in 0..500 {
                let actual = if binary {
                    let bytes = encode_preview(black_box(original.clone())).unwrap();
                    length = bytes.len();
                    decode_preview(black_box(&bytes)).unwrap()
                } else {
                    let bytes = encode(black_box(&original)).unwrap();
                    length = bytes.len();
                    serde_json::from_slice::<PreviewAudioBlock>(black_box(&bytes)).unwrap()
                };
                assert_eq!(black_box(actual), original);
            }
            println!(
                "preview_fact_component binary={binary} blocks=500 bytes={length} elapsed_ms={:.3}",
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}
