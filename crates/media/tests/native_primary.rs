//! Retained multi-track fixtures are generated once outside the repository.
//! Primary expectations come from the pinned retained demuxer, with independent
//! authored colors and tones. Tests never generate media or invoke a process.
use opencut_media::{MediaError, MediaResult, MediaTime, native_decode::*};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    path: PathBuf,
    sha256: String,
    video_index: u32,
    audio_index: u32,
}
struct Source {
    bytes: Vec<u8>,
    revoked: bool,
}
impl ByteSource for Source {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn checkpoint(&mut self) -> MediaResult<()> {
        if self.revoked {
            Err(MediaError::Admission {
                code: "PERMISSION_DENIED".into(),
                message: "Fixture access revoked".into(),
            })
        } else {
            Ok(())
        }
    }
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> MediaResult<usize> {
        self.checkpoint()?;
        assert!(output.len() <= 65536);
        let start = usize::try_from(offset).unwrap();
        let count = output.len().min(self.bytes.len().saturating_sub(start));
        output[..count].copy_from_slice(&self.bytes[start..start + count]);
        Ok(count)
    }
}
fn fixture() -> Fixture {
    let path =
        std::env::var_os("OPENCUT_NATIVE_PRIMARY_FIXTURE").expect("Retained fixture manifest");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn source(case: &Case) -> Source {
    let bytes = std::fs::read(&case.path).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), case.sha256);
    Source {
        bytes,
        revoked: false,
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn video_limits() -> DecodeLimits {
    DecodeLimits {
        max_pixels: 65536,
        max_resident_bytes: 32 * 1024 * 1024,
        max_io_bytes_per_operation: 8 * 1024 * 1024,
        max_packets_per_operation: 1000,
    }
}
fn audio_limits() -> AudioDecodeLimits {
    AudioDecodeLimits {
        max_channels: 2,
        max_frame_samples: 65536,
        max_resident_bytes: 8 * 1024 * 1024,
        max_io_bytes_per_operation: 8 * 1024 * 1024,
        max_packets_per_operation: 1000,
    }
}
fn video_frame(source: &mut Source, stream: Option<u32>) -> ArcFrame {
    let mut decoder = NativeVideoDecoder::open(source, stream, video_limits(), deadline()).unwrap();
    let frame = decoder
        .sample(
            source,
            MediaTime {
                ticks: 1,
                timebase: 2,
            },
            deadline(),
        )
        .unwrap()
        .unwrap();
    (decoder, frame)
}
type ArcFrame = (NativeVideoDecoder, std::sync::Arc<DecodedVideoFrame>);
fn assert_color(frame: &DecodedVideoFrame, stream: u32) {
    let (width, height, channel) = if stream == 0 {
        (32, 18, 0)
    } else {
        (64, 36, 2)
    };
    assert_eq!((frame.source_width, frame.source_height), (width, height));
    assert_eq!((frame.width, frame.height), (width, height));
    for pixel in frame.rgba.chunks_exact(4) {
        assert!(
            pixel[channel] >= 250,
            "Expected authored primary color: {pixel:?}"
        );
        assert!(pixel[(channel + 1) % 3] <= 5 && pixel[(channel + 2) % 3] <= 5);
        assert_eq!(pixel[3], 255);
    }
}

#[test]
#[ignore = "requires retained OPENCUT_NATIVE_PRIMARY_FIXTURE; no media generation"]
fn primary_video_matches_retained_default_order_and_preserves_original_raster() {
    for case in fixture().cases {
        let mut source = source(&case);
        let (decoder, frame) = video_frame(&mut source, None);
        assert_eq!(
            decoder.stream_index(),
            case.video_index as usize,
            "{}",
            case.path.display()
        );
        assert_color(&frame, case.video_index);
        assert_eq!(frame.timestamp_seconds(), 0.5);
        source.revoked = true;
        assert!(matches!(
            { let mut decoder = decoder; decoder.sample(&mut source, MediaTime { ticks: 1, timebase: 2 }, deadline()) },
            Err(MediaError::Admission { code, .. }) if code == "PERMISSION_DENIED"
        ));
    }
}

fn tone_power(samples: &[f32], rate: u32, frequency: f64) -> f64 {
    // The source is an authored sine. Phase-independent energy at its known
    // frequencies also tolerates AAC priming without inventing a sample offset.
    let mut sin = 0.;
    let mut cos = 0.;
    for (index, value) in samples.iter().enumerate() {
        let angle = std::f64::consts::TAU * frequency * index as f64 / f64::from(rate);
        sin += f64::from(*value) * angle.sin();
        cos += f64::from(*value) * angle.cos();
    }
    sin * sin + cos * cos
}
fn assert_audio(source: &mut Source, selected: Option<u32>, expected: u32) {
    let mut decoder =
        NativeAudioDecoder::open(source, selected, audio_limits(), deadline()).unwrap();
    let track = decoder.track().clone();
    assert_eq!(track.stream_index, expected);
    let second = expected == 3;
    assert_eq!(track.sample_rate, if second { 44100 } else { 48000 });
    assert_eq!(track.channels, if second { 2 } else { 1 });
    let mut samples = Vec::new();
    while samples.len() < 12000 {
        let Some(frame) = decoder.read(source, deadline()).unwrap() else {
            break;
        };
        assert_eq!(frame.sample_rate, track.sample_rate);
        assert_eq!(frame.channels.len(), track.channels as usize);
        samples.extend_from_slice(&frame.channels[0]);
    }
    assert!(samples.len() >= 10000);
    let samples = &samples[2000..10000];
    let expected_tone = tone_power(samples, track.sample_rate, if second { 880. } else { 440. });
    let other_tone = tone_power(samples, track.sample_rate, if second { 440. } else { 880. });
    assert!(
        expected_tone > other_tone * 1000. && expected_tone > 1.,
        "Authored source tone changed"
    );
    source.revoked = true;
    assert!(
        matches!(decoder.read(source, deadline()), Err(MediaError::Admission { code, .. }) if code == "PERMISSION_DENIED")
    );
}
#[test]
#[ignore = "requires retained OPENCUT_NATIVE_PRIMARY_FIXTURE; no media generation"]
fn primary_audio_matches_retained_default_order_channels_rate_and_authored_tone() {
    for case in fixture().cases {
        assert_audio(&mut source(&case), None, case.audio_index);
    }
}

#[test]
#[ignore = "requires retained OPENCUT_NATIVE_PRIMARY_FIXTURE; no media generation"]
fn explicit_streams_remain_physical_indices_and_wrong_media_is_rejected() {
    for case in fixture().cases {
        let mut input = source(&case);
        let (decoder, frame) = video_frame(&mut input, Some(0));
        assert_eq!(decoder.stream_index(), 0);
        assert_color(&frame, 0);
        assert_audio(&mut source(&case), Some(2), 2);
        assert!(
            NativeVideoDecoder::open(&mut source(&case), Some(2), video_limits(), deadline())
                .is_err()
        );
        assert!(
            NativeAudioDecoder::open(&mut source(&case), Some(0), audio_limits(), deadline())
                .is_err()
        );
        assert!(
            NativeVideoDecoder::open(&mut source(&case), Some(99), video_limits(), deadline())
                .is_err()
        );
        assert!(
            NativeAudioDecoder::open(&mut source(&case), Some(99), audio_limits(), deadline())
                .is_err()
        );
    }
}
