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
#[test]
fn empty_primary_packet_origin_is_an_observed_eof_without_a_synthetic_sample() {
    let mut source = wave(1, 16, 1, 48000, &[]);
    let limit = AudioDecodeLimits {
        max_channels: 2,
        max_frame_samples: 4096,
        max_resident_bytes: 1024 * 1024,
        max_io_bytes_per_operation: 1024 * 1024,
        max_packets_per_operation: 1024,
    };
    let mut decoder = NativeAudioDecoder::open(&mut source, None, limit, deadline()).unwrap();
    decoder
        .select_keyframe_at_pts(&mut source, 0, deadline())
        .unwrap();
    assert!(
        decoder
            .select_keyframe_at_pts(&mut source, 0, deadline())
            .is_err()
    );
    assert!(decoder.read(&mut source, deadline()).unwrap().is_none());
    assert!(decoder.reached_eof());
    assert!(decoder.read(&mut source, deadline()).unwrap().is_none());
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
fn initial_wave_seek_keeps_bounded_payload_probe_and_exact_samples() {
    // Long enough to expose seek resetting WAV's probe budget. The first
    // decoded samples are checked independently of the adapter's packet sizes.
    let sample = |frame: usize, channel: usize| ((frame % 997) as i16 - 498) * (channel as i16 + 1);
    let bytes: Vec<u8> = (0..48000 * 24)
        .flat_map(|frame| (0..2).flat_map(move |channel| sample(frame, channel).to_le_bytes()))
        .collect();
    for start in [0, 48000, 48000 * 20] {
        let mut source = wave(1, 16, 2, 48000, &bytes);
        let mut decoder =
            NativeAudioDecoder::open(&mut source, None, limits(), deadline()).unwrap();
        for target in [start, 24000, 0] {
            let before = decoder.encoded_bytes_read();
            decoder.seek_pts(&mut source, target, deadline()).unwrap();
            decoder
                .select_keyframe_at_pts(&mut source, target, deadline())
                .unwrap();
            let frame = decoder.read(&mut source, deadline()).unwrap().unwrap();
            assert_eq!(frame.pts, Some(target));
            assert_eq!(frame.sample_rate, 48000);
            assert_eq!(frame.channels.len(), 2);
            for (channel, samples) in frame.channels.iter().enumerate() {
                for (offset, actual) in samples.iter().enumerate() {
                    assert_eq!(
                        *actual,
                        f32::from(sample(target as usize + offset, channel)) / 32768.0
                    );
                }
            }
            assert!(
                decoder.encoded_bytes_read() - before < 1024 * 1024,
                "A seek must not expand WAV's initial payload probe to megabytes"
            );
        }
    }
}

#[test]
fn initial_wave_seek_probe_preserves_revocation_deadline_and_io_admission() {
    for failure in ["revoked", "deadline", "budget"] {
        let mut source = wave(1, 16, 2, 48000, &vec![0; 4 * 48000 * 24]);
        let mut limits = limits();
        if failure == "budget" {
            // Admit the header/SPDIF check, but not the full payload probe.
            limits.max_io_bytes_per_operation = 256 * 1024;
        }
        let mut decoder = NativeAudioDecoder::open(&mut source, None, limits, deadline()).unwrap();
        source.revoked = failure == "revoked";
        let until = if failure == "deadline" {
            Instant::now()
        } else {
            deadline()
        };
        assert!(
            decoder.seek_pts(&mut source, 48000, until).is_err(),
            "{failure}"
        );
        source.revoked = false;
        assert!(
            decoder.read(&mut source, deadline()).is_err(),
            "failed decoder stays retired"
        );
    }
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

#[test]
fn packet_origin_keeps_first_and_following_frames_and_rechecks_authority() {
    let expected: Vec<i16> = (0..8193).map(|i| (i % 1024) as i16 - 512).collect();
    let samples: Vec<u8> = expected.iter().flat_map(|v| v.to_le_bytes()).collect();
    for origin in [-1, 0] {
        let mut source = wave(1, 16, 1, 48000, &samples);
        let mut decoder =
            NativeAudioDecoder::open(&mut source, None, limits(), deadline()).unwrap();
        decoder
            .select_keyframe_at_pts(&mut source, origin, deadline())
            .unwrap();
        assert!(
            decoder
                .select_keyframe_at_pts(&mut source, origin, deadline())
                .is_err()
        );
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
        assert!(decoder.reached_eof());
    }
    let mut source = wave(1, 16, 1, 48000, &samples);
    let mut decoder = NativeAudioDecoder::open(&mut source, None, limits(), deadline()).unwrap();
    source.revoked = true;
    let error = decoder
        .select_keyframe_at_pts(&mut source, 0, deadline())
        .unwrap_err();
    assert!(matches!(error, MediaError::Admission { ref code, .. } if code == "PERMISSION_DENIED"));
    source.revoked = false;
    assert!(decoder.read(&mut source, deadline()).is_err());
}

#[test]
#[ignore = "requires explicit retained AAC source and independently captured browser PCM"]
fn aac_container_origin_matches_independent_browser_samples_without_preroll() {
    use sha2::{Digest, Sha256};
    let dir = std::path::PathBuf::from(
        std::env::var_os("OPENCUT_AUDIO_AAC_REFERENCE").expect("reference directory"),
    );
    let source_path = std::env::var_os("OPENCUT_AUDIO_AAC_SOURCE").expect("retained source");
    let oracle: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("aac-cut-oracle.json")).unwrap()).unwrap();
    let bytes = std::fs::read(source_path).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        oracle["source_sha256"]
    );
    assert_eq!(
        oracle["browser_sha256"],
        "b5626938028f718a6cdb0c8eff9d0346b186d68b4e04b43df4f9f579c81ecf28"
    );
    let expected = std::fs::read(dir.join("aac-cut-oracle.f32le")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&expected)),
        oracle["expected_pcm_sha256"]
    );
    let mut source = Source {
        bytes,
        revoked: false,
        reads: 0,
    };
    let mut decoder = NativeAudioDecoder::open(&mut source, None, limits(), deadline()).unwrap();
    assert_eq!(decoder.track().codec, "aac");
    decoder
        .select_keyframe_at_pts(&mut source, 0, deadline())
        .unwrap();
    let mut actual = Vec::new();
    while let Some(frame) = decoder.read(&mut source, deadline()).unwrap() {
        assert_eq!(frame.pts, Some(actual.len() as i64));
        assert_eq!(frame.channels.len(), 1);
        assert_eq!(frame.sample_rate, 48000);
        actual.extend(frame.channels[0].iter().copied());
    }
    assert_eq!(
        actual.len() as u64,
        oracle["native_frames"].as_u64().unwrap()
    );
    let mut selected = Vec::new();
    for range in oracle["manual_source_sample_ranges"].as_array().unwrap() {
        let start = range[0].as_u64().unwrap() as usize;
        let end = range[1].as_u64().unwrap() as usize;
        for sample in &actual[start..end] {
            selected.extend(sample.to_le_bytes());
            selected.extend(sample.to_le_bytes());
        }
    }
    assert_eq!(
        selected, expected,
        "AAC origin must match without gain or time fitting"
    );
    // A retained demuxer is reused after EOF, but a new iterator must start
    // with fresh codec overlap/PNS state rather than the previous decoder tail.
    for _ in 0..2 {
        decoder.seek_pts(&mut source, 0, deadline()).unwrap();
        decoder
            .select_keyframe_before_seconds(&mut source, 0.0, deadline())
            .unwrap();
        let mut replay = Vec::new();
        while let Some(frame) = decoder.read(&mut source, deadline()).unwrap() {
            replay.extend(frame.channels[0].iter().copied());
        }
        assert_eq!(
            replay, actual,
            "Rewound AAC must retain independent exact PCM"
        );
    }
}
