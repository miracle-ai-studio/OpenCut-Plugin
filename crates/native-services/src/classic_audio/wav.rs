use opencut_contracts::EditorError;
use opencut_editor_application::classic_audio::{ClassicAudioPcmSink, ClassicAudioWavLayout};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
};

const MAX_HEADER_BYTES: usize = 94;
const FRAME_BYTES: u64 = 8;
const BLOCK_FRAMES: usize = opencut_editor_application::classic_audio::AUDIO_PCM_CHUNK_FRAMES;

/// Private attempt WAV output. The host creates/admit-leases the read-write
/// file before constructing this sink and owns cleanup on every failure.
/// Completion returns a synced file; it does not publish an artifact.
pub struct ClassicAudioWavSink {
    file: File,
    frames: u64,
    total_bytes: u64,
    data_offset: u64,
    next: [u64; 2],
    failed: bool,
}

impl ClassicAudioWavSink {
    /// Exact storage admission size, including non-PCM fmt and fact chunks.
    pub fn checked_bytes(sample_rate: u32, frames: u64) -> Result<u64, EditorError> {
        Ok(ClassicAudioWavLayout::new(sample_rate, frames)?.total_bytes)
    }

    pub fn new(mut file: File, sample_rate: u32, frames: u64) -> Result<Self, EditorError> {
        let layout = ClassicAudioWavLayout::new(sample_rate, frames)?;
        let total_bytes = layout.total_bytes;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err(invalid(
                "WAV output must be a newly created empty regular file",
            ));
        }
        let data_bytes = frames * FRAME_BYTES;
        let mut header = [0u8; MAX_HEADER_BYTES];
        header[8..12].copy_from_slice(b"WAVE");
        let fmt = if layout.rf64 {
            // EBU Tech3306: ds64 is the first RF64 chunk. All three legacy
            // size/count fields use the sentinel; exact values remain u64.
            header[0..4].copy_from_slice(b"RF64");
            header[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
            header[12..16].copy_from_slice(b"ds64");
            header[16..20].copy_from_slice(&28u32.to_le_bytes());
            header[20..28].copy_from_slice(&(total_bytes - 8).to_le_bytes());
            header[28..36].copy_from_slice(&data_bytes.to_le_bytes());
            header[36..44].copy_from_slice(&frames.to_le_bytes());
            // tableLength is zero: the only large payload is the data chunk.
            &mut header[48..94]
        } else {
            header[0..4].copy_from_slice(b"RIFF");
            header[4..8].copy_from_slice(&((total_bytes - 8) as u32).to_le_bytes());
            &mut header[12..58]
        };
        fmt[0..4].copy_from_slice(b"fmt ");
        fmt[4..8].copy_from_slice(&18u32.to_le_bytes());
        fmt[8..10].copy_from_slice(&3u16.to_le_bytes());
        fmt[10..12].copy_from_slice(&2u16.to_le_bytes());
        fmt[12..16].copy_from_slice(&sample_rate.to_le_bytes());
        fmt[16..20].copy_from_slice(&(sample_rate * 8).to_le_bytes());
        fmt[20..22].copy_from_slice(&8u16.to_le_bytes());
        fmt[22..24].copy_from_slice(&32u16.to_le_bytes());
        // cbSize is zero: IEEE float has no extra format extension.
        fmt[26..30].copy_from_slice(b"fact");
        fmt[30..34].copy_from_slice(&4u32.to_le_bytes());
        fmt[34..38]
            .copy_from_slice(&(if layout.rf64 { u32::MAX } else { frames as u32 }).to_le_bytes());
        fmt[38..42].copy_from_slice(b"data");
        fmt[42..46].copy_from_slice(
            &(if layout.rf64 {
                u32::MAX
            } else {
                data_bytes as u32
            })
            .to_le_bytes(),
        );
        // Extension of an empty regular file supplies zeroes for unwritten
        // channels, without retaining a full-scene Rust PCM allocation.
        file.set_len(total_bytes).map_err(io_error)?;
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        file.write_all(&header[..layout.header_bytes as usize])
            .map_err(io_error)?;
        Ok(Self {
            file,
            frames,
            total_bytes,
            data_offset: layout.header_bytes,
            next: [0, 0],
            failed: false,
        })
    }

    fn write_chunk(
        &mut self,
        channel: u32,
        offset: u64,
        samples: &[f32],
    ) -> Result<(), EditorError> {
        if self.failed || channel > 1 || samples.is_empty() || samples.len() > BLOCK_FRAMES {
            return Err(invalid("Invalid or failed WAV PCM chunk"));
        }
        let channel = channel as usize;
        let end = offset
            .checked_add(samples.len() as u64)
            .filter(|&end| end <= self.frames)
            .ok_or_else(|| invalid("WAV PCM chunk exceeds the admitted frame range"))?;
        if offset != self.next[channel] || samples.iter().any(|sample| !sample.is_finite()) {
            return Err(invalid(
                "WAV PCM must be finite and strictly ordered per channel",
            ));
        }
        let position = self.data_offset + offset * FRAME_BYTES;
        let mut interleaved = [0u8; BLOCK_FRAMES * FRAME_BYTES as usize];
        let bytes = &mut interleaved[..samples.len() * FRAME_BYTES as usize];
        self.file
            .seek(SeekFrom::Start(position))
            .map_err(io_error)?;
        self.file.read_exact(bytes).map_err(io_error)?;
        for (frame, sample) in samples.iter().enumerate() {
            let at = frame * 8 + channel * 4;
            bytes[at..at + 4].copy_from_slice(&sample.to_le_bytes());
        }
        self.file
            .seek(SeekFrom::Start(position))
            .map_err(io_error)?;
        self.file.write_all(bytes).map_err(io_error)?;
        self.next[channel] = end;
        Ok(())
    }

    pub fn finalize(self) -> Result<File, EditorError> {
        if self.failed {
            return Err(invalid("WAV execution previously rejected PCM"));
        }
        if self.next != [self.frames; 2] {
            return Err(EditorError::new(
                "INCOMPLETE_OUTPUT_PCM",
                "WAV is missing channel frames",
            ));
        }
        if self.file.metadata().map_err(io_error)?.len() != self.total_bytes {
            return Err(invalid("WAV output length changed during execution"));
        }
        self.file.sync_all().map_err(io_error)?;
        Ok(self.file)
    }
}

impl ClassicAudioPcmSink for ClassicAudioWavSink {
    fn chunk(&mut self, channel: u32, offset: u64, samples: &[f32]) -> Result<(), EditorError> {
        let result = self.write_chunk(channel, offset, samples);
        self.failed |= result.is_err();
        result
    }
}

fn invalid(message: &str) -> EditorError {
    EditorError::new("INVALID_OUTPUT_PCM", message)
}
fn io_error(_: std::io::Error) -> EditorError {
    EditorError::new("AUDIO_WAV_IO_FAILED", "Private WAV output I/O failed")
}
