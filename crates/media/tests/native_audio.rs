//! Independent tiny PCM bytes over the actual authorized libav AVIO port.
//! No user media, external process, host path or browser decoder is consulted.
use opencut_media::{MediaError, MediaResult, native_decode::*};
use std::time::{Duration, Instant};
struct Source {
    bytes: Vec<u8>,
    revoked: bool,
    reads: usize,
}
impl ByteSource for Source {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn checkpoint(&mut self) -> MediaResult<()> {
        if self.revoked {
            Err(MediaError::Admission {
                code: "PERMISSION_DENIED".into(),
                message: "Owned source revoked".into(),
            })
        } else {
            Ok(())
        }
    }
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> MediaResult<usize> {
        self.checkpoint()?;
        assert!(output.len() <= 65536);
        self.reads += 1;
        let start = usize::try_from(offset).unwrap();
        let n = output.len().min(self.bytes.len().saturating_sub(start));
        output[..n].copy_from_slice(&self.bytes[start..start + n]);
        Ok(n)
    }
}
fn wave(format: u16, bits: u16, channels: u16, rate: u32, pcm: &[u8]) -> Source {
    let mut b = Vec::new();
    b.extend(b"RIFF");
    b.extend((36 + pcm.len() as u32).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(format.to_le_bytes());
    b.extend(channels.to_le_bytes());
    b.extend(rate.to_le_bytes());
    let align = channels * bits / 8;
    b.extend((rate * u32::from(align)).to_le_bytes());
    b.extend(align.to_le_bytes());
    b.extend(bits.to_le_bytes());
    b.extend(b"data");
    b.extend((pcm.len() as u32).to_le_bytes());
    b.extend(pcm);
    Source {
        bytes: b,
        revoked: false,
        reads: 0,
    }
}
fn limits() -> AudioDecodeLimits {
    AudioDecodeLimits {
        max_channels: 4,
        max_frame_samples: 65536,
        max_resident_bytes: 8 * 1024 * 1024,
        max_io_bytes_per_operation: 8 * 1024 * 1024,
        max_packets_per_operation: 1000,
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
#[test]
fn native_pcm_formats_preserve_channels_samples_pts_and_actual_eof() {
    for (format, bits, samples, expected) in [
        (
            1,
            8,
            vec![0, 128, 255, 64],
            vec![-1.0, 0.0, 127.0 / 128.0, -0.5],
        ),
        (
            1,
            16,
            [-32768i16, 0, 32767, -16384]
                .into_iter()
                .flat_map(i16::to_le_bytes)
                .collect(),
            vec![-1.0, 0.0, 32767.0 / 32768.0, -0.5],
        ),
        (
            1,
            24,
            vec![0, 0, 128, 0, 0, 0, 255, 255, 127, 0, 0, 192],
            vec![-1.0, 0.0, 8388607.0 / 8388608.0, -0.5],
        ),
        (
            1,
            32,
            [-2147483648i32, 0, 2147483647, -1073741824]
                .into_iter()
                .flat_map(i32::to_le_bytes)
                .collect(),
            vec![-1.0, 0.0, 1.0, -0.5],
        ),
        (
            3,
            32,
            [-1.0f32, 0.0, 0.25, -0.5]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
            vec![-1.0, 0.0, 0.25, -0.5],
        ),
        (
            3,
            64,
            [-1.0f64, 0.0, 0.25, -0.5]
                .into_iter()
                .flat_map(f64::to_le_bytes)
                .collect(),
            vec![-1.0, 0.0, 0.25, -0.5],
        ),
    ] {
        let mut source = wave(format, bits, 2, 48000, &samples);
        let mut decoder =
            NativeAudioDecoder::open(&mut source, None, limits(), deadline()).unwrap();
        let frame = decoder.read(&mut source, deadline()).unwrap().unwrap();
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.frames(), 2);
        assert_eq!(
            frame.channels,
            vec![
                vec![expected[0], expected[2]],
                vec![expected[1], expected[3]]
            ]
        );
        assert_eq!(frame.pts, Some(0));
        assert_eq!(
            (frame.time_base_numerator, frame.time_base_denominator),
            (1, 48000)
        );
        assert!(decoder.read(&mut source, deadline()).unwrap().is_none());
        assert!(decoder.reached_eof());
        assert!(decoder.read(&mut source, deadline()).unwrap().is_none());
        assert!(source.reads > 0);
    }
}
#[test]
fn continuous_audio_frames_and_exact_stream_seek_do_not_reopen_or_invent_time() {
    let expected: Vec<i16> = (0..8193).map(|i| ((i % 1024) as i16 - 512) * 32).collect();
    let samples: Vec<u8> = expected.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut source = wave(1, 16, 1, 44100, &samples);
    let mut decoder = NativeAudioDecoder::open(&mut source, None, limits(), deadline()).unwrap();
    let mut actual = Vec::new();
    while let Some(frame) = decoder.read(&mut source, deadline()).unwrap() {
        assert_eq!(frame.pts, Some(actual.len() as i64));
        actual.extend(frame.channels[0].iter().copied());
    }
    assert_eq!(
        actual,
        expected
            .iter()
            .map(|v| f32::from(*v) / 32768.0)
            .collect::<Vec<_>>()
    );
    decoder.seek_pts(&mut source, 512, deadline()).unwrap();
    let frame = decoder.read(&mut source, deadline()).unwrap().unwrap();
    assert_eq!(frame.pts, Some(512));
    assert_eq!(frame.channels[0][0], actual[512]);
}
#[test]
fn revocation_invalid_pcm_deadline_and_admission_retire_instead_of_falling_back() {
    let mut source = wave(1, 16, 1, 48000, &[0; 16]);
    let mut decoder = NativeAudioDecoder::open(&mut source, None, limits(), deadline()).unwrap();
    source.revoked = true;
    assert!(decoder.read(&mut source, deadline()).is_err());
    source.revoked = false;
    assert!(decoder.read(&mut source, deadline()).is_err());
    let mut source = wave(3, 32, 1, 48000, &f32::NAN.to_le_bytes());
    let mut decoder = NativeAudioDecoder::open(&mut source, None, limits(), deadline()).unwrap();
    assert!(decoder.read(&mut source, deadline()).is_err());
    let mut source = wave(1, 16, 1, 48000, &[0; 16]);
    assert!(NativeAudioDecoder::open(&mut source, None, limits(), Instant::now()).is_err());
    let mut bounded = limits();
    bounded.max_channels = 0;
    assert!(NativeAudioDecoder::open(&mut source, None, bounded, deadline()).is_err());
    assert!(NativeAudioDecoder::open(&mut source, Some(63), limits(), deadline()).is_err());
}
