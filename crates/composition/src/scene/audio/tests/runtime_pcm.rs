use super::*;

#[test]
fn compiled_audio_wire_keeps_wide_indices_rationals_and_every_float64_gain_bit() {
    let amplitudes: Vec<f64> = (0..4096u64)
        .map(|n| {
            if n == 0 {
                -0.0
            } else {
                f64::from_bits(
                    0x3f00_0000_0000_0000
                        | ((n.wrapping_mul(6364136223846793005)) & 0x000f_ffff_ffff_ffff),
                )
            }
        })
        .collect();
    let block = RuntimeAudioBlock {
        identity: AudioIdentity {
            project_id: "p".into(),
            revision: "fixed".into(),
            sequence_id: "s".into(),
            profile: AudioExecutionProfile::ClassicV31,
            format: PcmFormat {
                sample_rate: 48000,
                channels: 2,
            },
        },
        start_sample: u64::MAX - 4096,
        frame_count: 4096,
        voices: vec![RuntimeAudioVoiceBlock {
            element_id: "voice".into(),
            block_offset: 0,
            frame_count: 4096,
            positions: vec![
                ExactSourceSample {
                    lower: u64::MAX,
                    fraction_numerator: u128::MAX - 1,
                    fraction_denominator: u128::MAX
                };
                4096
            ],
            amplitudes,
        }],
        receipts: vec![AudioReceipt {
            track_id: "track".into(),
            element_id: "voice".into(),
            decision: AudioParticipation::Ready,
        }],
    };
    let encoded = serde_json::to_vec(&block).unwrap();
    let decoded: RuntimeAudioBlock = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, block);
    assert!(
        decoded.voices[0]
            .amplitudes
            .iter()
            .zip(&block.voices[0].amplitudes)
            .all(|(a, b)| a.to_bits() == b.to_bits())
    );
    for (field, value) in [
        ("lower", serde_json::json!(17)),
        ("fractionNumerator", serde_json::json!("01")),
        (
            "fractionDenominator",
            serde_json::json!("340282366920938463463374607431768211456"),
        ),
    ] {
        let mut point = serde_json::to_value(block.voices[0].positions[0]).unwrap();
        point[field] = value;
        assert!(serde_json::from_value::<ExactSourceSample>(point).is_err());
    }
}

fn retime(s: &mut EditorSnapshot, pitch: bool, numerator: u32, denominator: u32) {
    let ElementContent::Audio { retime, .. } =
        &mut s.project.sequences[0].tracks[1].elements[0].content
    else {
        panic!()
    };
    retime.maintain_pitch = pitch;
    retime.rate = RationalRate {
        numerator,
        denominator,
    };
}
fn format(mode: ClassicAudioPreparationMode, rate: u32, frames: u64) -> RuntimeSourceFormat {
    RuntimeSourceFormat {
        element_id: "voice".into(),
        sample_rate: rate,
        frame_count: frames,
        channels: 1,
        mode,
    }
}

#[test]
fn final_prepared_pcm_has_zero_unit_clock_but_original_sampling_keeps_trim_and_true_duration() {
    let mut s = snapshot();
    // 1/7 second at 10 Hz is two output samples. The actual authored span
    // remains 1/7, not the 1/5 seconds obtained from that quantized length.
    let e = &mut s.project.sequences[0].tracks[1].elements[0];
    e.timeline_range.duration = t(1, 7);
    e.source_span = ElementSourceSpan::ClassicTrimmed {
        trim_start: t(1, 4),
        trim_end: t(0, 1),
        source_duration: None,
    };
    retime(&mut s, true, 3, 2);
    let prepared = prepare(&s, 10);
    let original = compile_original_audio_positions_scene(&prepared, "voice", 0, 2, 8).unwrap();
    assert_eq!(
        original,
        vec![
            ExactSourceSample {
                lower: 2,
                fraction_numerator: 0,
                fraction_denominator: 1
            },
            ExactSourceSample {
                lower: 3,
                fraction_numerator: 1,
                fraction_denominator: 5
            },
        ]
    );
    let formats = [format(
        ClassicAudioPreparationMode::PreparedSoundtouch,
        10,
        2,
    )];
    let bound = bind_classic_audio_pcm_scene(&prepared, &formats).unwrap();
    assert_eq!(bound.identity(), prepared.identity());
    assert_eq!(bound.duration_samples(), prepared.duration_samples());
    assert_eq!(bound.receipts(), prepared.receipts());
    let block = compile_bound_audio_block_scene(&bound, 3, 2).unwrap();
    assert_eq!(block.identity, *prepared.identity());
    assert_eq!(
        block.voices[0].positions,
        vec![
            ExactSourceSample {
                lower: 0,
                fraction_numerator: 0,
                fraction_denominator: 1
            },
            ExactSourceSample {
                lower: 1,
                fraction_numerator: 0,
                fraction_denominator: 1
            },
        ]
    );
    let voice = prepared.voices().next().unwrap();
    assert_eq!(*voice.timeline_duration, t(1, 7));
    assert_eq!(voice.source.clock.nominal_request.start, t(1, 4));
    // The stored request still uses Classic's 120000-tick forward rounding;
    // preparation length uses the independent authored 1/7-second duration.
    assert_eq!(
        voice.source.clock.nominal_request.duration,
        t(25714, 120000)
    );
    let json = serde_json::to_value(block).unwrap();
    assert_eq!(json["identity"]["profile"], "classic-export-v31-web-audio");
    assert_eq!(json["startSample"], "3");
    assert_eq!(json["voices"][0]["positions"][1]["lower"], "1");
    assert_eq!(json["voices"][0]["positions"][1]["fractionNumerator"], "0");
    assert!(!json.to_string().contains("/private/"));

    // Unit-rate keepPitch is a different prepared mode, but still advances
    // from the prepared Float32 buffer origin rather than reapplying trim.
    retime(&mut s, true, 1, 1);
    let linear = prepare(&s, 10);
    let bound = bind_classic_audio_pcm_scene(
        &linear,
        &[format(ClassicAudioPreparationMode::PreparedLinear, 10, 2)],
    )
    .unwrap();
    assert_eq!(
        compile_bound_audio_block_scene(&bound, 3, 2)
            .unwrap()
            .voices[0]
            .positions[0]
            .lower,
        0
    );
    assert_eq!(
        compile_original_audio_positions_scene(&linear, "voice", 0, 2, 8).unwrap()[0].lower,
        2
    );
}

#[test]
fn unchanged_gain_clock_and_zero_tail_are_independent_of_blocks_and_preparation() {
    let mut s = snapshot();
    s.project.sequences[0].tracks[1].elements[0]
        .channels
        .push(channel(
            false,
            vec![
                key("element-a", t(0, 1), 0.0),
                key("element-b", t(1, 2), 20.0),
            ],
        ));
    s.project.sequences[0].tracks[1].channels.push(channel(
        true,
        vec![key("track-a", t(0, 1), 0.0), key("track-b", t(1, 2), -20.0)],
    ));
    let prepared = prepare(&s, 4);
    // Actual PCM is only three samples long. Nominal index5 must stay index5,
    // which the renderer treats as zero rather than clamping/repeating index2.
    let bound = bind_classic_audio_pcm_scene(
        &prepared,
        &[format(ClassicAudioPreparationMode::UnpreparedLinear, 8, 3)],
    )
    .unwrap();
    let whole = compile_bound_audio_block_scene(&bound, 0, 4).unwrap();
    let left = compile_bound_audio_block_scene(&bound, 1, 1).unwrap();
    let right = compile_bound_audio_block_scene(&bound, 2, 1).unwrap();
    assert_eq!(
        whole.voices[0]
            .positions
            .iter()
            .map(|p| p.lower)
            .collect::<Vec<_>>(),
        vec![2, 5]
    );
    assert_eq!(
        [
            left.voices[0].positions.clone(),
            right.voices[0].positions.clone()
        ]
        .concat(),
        whole.voices[0].positions
    );
    assert_eq!(
        [
            left.voices[0].amplitudes.clone(),
            right.voices[0].amplitudes.clone()
        ]
        .concat(),
        whole.voices[0].amplitudes
    );
    // First sample is sequence .25s/local0, second is sequence .5s/local.25s.
    close(whole.voices[0].amplitudes[0], 1.0 / 10.0_f64.sqrt());
    close(whole.voices[0].amplitudes[1], 1.0 / 10.0_f64.sqrt());
    assert_eq!(left.identity, right.identity);
    assert_eq!(left.receipts, right.receipts);

    retime(&mut s, true, 3, 2);
    let prepared_pitch = prepare(&s, 4);
    let bound_pitch = bind_classic_audio_pcm_scene(
        &prepared_pitch,
        &[format(
            ClassicAudioPreparationMode::PreparedSoundtouch,
            4,
            2,
        )],
    )
    .unwrap();
    let pitch = compile_bound_audio_block_scene(&bound_pitch, 0, 4).unwrap();
    assert_eq!(pitch.voices[0].amplitudes, whole.voices[0].amplitudes);
    assert_eq!(
        pitch.voices[0]
            .positions
            .iter()
            .map(|p| p.lower)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn runtime_binding_requires_complete_unique_participants_and_exact_prepared_format() {
    let mut s = snapshot();
    retime(&mut s, true, 3, 2);
    let prepared = prepare(&s, 4);
    let valid = format(ClassicAudioPreparationMode::PreparedSoundtouch, 4, 2);
    assert!(bind_classic_audio_pcm_scene(&prepared, &[]).is_err());
    assert!(bind_classic_audio_pcm_scene(&prepared, &[valid.clone(), valid.clone()]).is_err());
    let wrong = RuntimeSourceFormat {
        element_id: "not-participating".into(),
        ..valid.clone()
    };
    assert!(bind_classic_audio_pcm_scene(&prepared, &[wrong]).is_err());
    for wrong in [
        RuntimeSourceFormat {
            mode: ClassicAudioPreparationMode::UnpreparedLinear,
            ..valid.clone()
        },
        RuntimeSourceFormat {
            sample_rate: 8,
            ..valid.clone()
        },
        RuntimeSourceFormat {
            frame_count: 3,
            ..valid.clone()
        },
        RuntimeSourceFormat {
            channels: 3,
            ..valid.clone()
        },
        RuntimeSourceFormat {
            frame_count: 0,
            ..valid.clone()
        },
    ] {
        assert!(bind_classic_audio_pcm_scene(&prepared, &[wrong]).is_err());
    }
    let bound = bind_classic_audio_pcm_scene(&prepared, std::slice::from_ref(&valid)).unwrap();
    assert!(compile_bound_audio_block_scene(&bound, u64::MAX, 1).is_err());
    assert!(compile_bound_audio_block_scene(&bound, 0, 0).is_err());
    assert!(compile_original_audio_positions_scene(&prepared, "voice", 1, 2, 8).is_err());
    assert!(compile_original_audio_positions_scene(&prepared, "unknown", 0, 1, 8).is_err());
    assert!(compile_original_audio_positions_scene(&prepared, "voice", 0, 1, 0).is_err());
    let json = serde_json::to_value(&valid).unwrap();
    assert_eq!(json["frameCount"], "2");
    assert_eq!(json["mode"], "prepared-soundtouch");
    let mut wrong_json = json;
    wrong_json["frameCount"] = serde_json::json!(2);
    assert!(serde_json::from_value::<RuntimeSourceFormat>(wrong_json).is_err());

    // Muted source voices are not missing bindings; they stay in receipts.
    audio_mut(&mut s).muted = true;
    let muted = prepare(&s, 4);
    let bound = bind_classic_audio_pcm_scene(&muted, &[]).unwrap();
    assert_eq!(
        compile_bound_audio_block_scene(&bound, 0, 4)
            .unwrap()
            .voices
            .len(),
        0
    );
    assert!(
        bind_classic_audio_pcm_scene(
            &muted,
            &[format(
                ClassicAudioPreparationMode::PreparedSoundtouch,
                4,
                2
            )]
        )
        .is_err()
    );

    audio_mut(&mut s).muted = false;
    let mut second = s.project.sequences[0].tracks[1].elements[0].clone();
    second.id = "voice-second".into();
    second.timeline_range.start = t(1, 1);
    s.project.sequences[0].tracks[1].elements.push(second);
    let two = prepare(&s, 4);
    assert!(bind_classic_audio_pcm_scene(&two, &[valid.clone(), valid.clone()]).is_err());
    let second_format = RuntimeSourceFormat {
        element_id: "voice-second".into(),
        ..valid.clone()
    };
    let reversed = bind_classic_audio_pcm_scene(&two, &[second_format, valid]).unwrap();
    assert_eq!(
        reversed
            .source_formats()
            .iter()
            .map(|f| f.element_id.as_str())
            .collect::<Vec<_>>(),
        vec!["voice", "voice-second"]
    );
}
