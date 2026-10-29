//! In-process libav decoding over an already authorized immutable byte port.
//! No path, URL, project document or authorization grant enters the decoder.
mod audio;
pub mod identity;
pub(crate) mod input;
mod scaler;
mod video;
use crate::{MediaError, MediaResult};
pub use audio::{AudioDecodeLimits, DecodedAudioFrame, NativeAudioDecoder, NativeAudioTrack};
use std::time::Instant;
pub use video::{
    DecodedVideoFrame, NativeVideoDecoder, NativeVideoTrack, VideoColorMatrix, VideoOutputSize,
};

pub trait ByteSource {
    fn len(&self) -> u64;
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> MediaResult<usize>;
    /// Re-check the owning execution's authority, cancellation and deadline.
    fn checkpoint(&mut self) -> MediaResult<()>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    pub max_pixels: u64,
    pub max_resident_bytes: u64,
    pub max_io_bytes_per_operation: u64,
    pub max_packets_per_operation: usize,
}
impl DecodeLimits {
    fn validate(&self) -> MediaResult<()> {
        if self.max_pixels == 0
            || self.max_pixels > 16384 * 16384
            || self.max_resident_bytes < 65536
            || self.max_io_bytes_per_operation < 65536
            || self.max_packets_per_operation == 0
            || self.max_packets_per_operation > 100_000
        {
            return Err(MediaError::InvalidRequest(
                "Invalid native decode admission".into(),
            ));
        }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> MediaError {
    MediaError::InvalidToolOutput {
        tool: "libav".into(),
        detail: message.into(),
    }
}
fn failed(error: ffmpeg_next::Error) -> MediaError {
    invalid(error.to_string())
}
fn checkpoint(source: &mut dyn ByteSource, deadline: Instant) -> MediaResult<()> {
    source.checkpoint()?;
    if Instant::now() >= deadline {
        return Err(MediaError::Backpressure(
            "Native decoder deadline exceeded".into(),
        ));
    }
    Ok(())
}
