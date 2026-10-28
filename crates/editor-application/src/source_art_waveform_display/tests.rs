use super::*;
use crate::source_art::{SOURCE_ART_DECODE_PROFILE, SOURCE_PEAKS_ALGORITHM, SourceArtPolicy};
use opencut_contracts::{AssetKind, AssetVersion, CreateProject, FrameRate, Time, TimeRange};
use std::cell::Cell;

#[test]
fn legacy_linear_waveform_preserves_physical_amplitudes_and_midpoint_grid() {
    let mut fixture = Fixture::new();
    fixture.audio().gain = Gain::LinearLegacy { value: 0.25 };
    let input = fixture.request();
    let prepared = prepare_waveform_display(&fixture, &input).unwrap();
    assert!(
        projection::gain_samples(&prepared)
            .unwrap()
            .iter()
            .all(|gain| *gain == 0.25)
    );
    fixture.audio().gain = Gain::LinearLegacy { value: 0.0 };
    fixture.element().channels.push(AnimationChannel::Scalar {
        id: "linear-fade".into(),
        target: ChannelTarget::Element {
            element_id: "element".into(),
        },
        binding: ScalarBinding::Property {
            parameter: ScalarParameter::GainLinearLegacy,
        },
        keys: [0.0, 1.0]
            .into_iter()
            .enumerate()
            .map(|(index, value)| ScalarKey {
                id: format!("fade-{index}"),
                segment_id: None,
                time: t(index as i64, 2),
                value,
                left_handle: None,
                right_handle: None,
                segment_to_next: ScalarSegment::Linear,
                tangent_mode: TangentMode::Auto,
            })
            .collect(),
        extrapolation: Extrapolation {
            before: ExtrapolationMode::Hold,
            after: ExtrapolationMode::Hold,
        },
    });
    let original = fixture.captured.snapshot.clone();
    let prepared = prepare_waveform_display(&fixture, &input).unwrap();
    let samples = projection::gain_samples(&prepared).unwrap();
    for (index, actual) in samples.iter().enumerate() {
        let seconds = (index as f64 + 0.5) / 200.0;
        close(*actual, (seconds * 2.0).min(1.0));
    }
    assert_eq!(fixture.captured.snapshot, original);
}

fn t(ticks: i64, timebase: u32) -> Time {
    Time {
        ticks: ticks.to_string(),
        timebase,
    }
}
struct Fixture {
    captured: AuthorizedSourceArt,
    revoked: bool,
    reads: Cell<usize>,
}
impl Fixture {
    fn new() -> Self {
        let mut project = crate::scene::new_document(&CreateProject {
            id: "project".into(),
            name: "Waveform".into(),
            sequence_id: "scene".into(),
            width: 16,
            height: 16,
            fps: FrameRate {
                numerator: 30,
                denominator: 1,
            },
        })
        .unwrap();
        project.assets.push(ProjectAsset {
            id: "asset".into(),
            name: "Audio".into(),
            kind: AssetKind::Audio,
            version: AssetVersion {
                id: format!("av_{}", "a".repeat(64)),
                content_hash: "a".repeat(64),
                size: 1,
            },
            duration: Some(t(4, 1)),
            origin: ResourceOrigin::LocalImport {
                original_name: "source.wav".into(),
            },
            metadata: ProjectMediaMetadata {
                mime_type: Some("audio/wav".into()),
                intrinsic_size: None,
                frame_rate: None,
                rotation_degrees: None,
                audio: AudioPresence::Present { metadata: None },
            },
            license: None,
        });
        let mut track = project.sequences[0].tracks[0].clone();
        track.id = "audio".into();
        track.kind = SceneTrackKind::Audio;
        // These track facts must not multiply/suppress the Classic clip display.
        track.muted = true;
        track.enabled = false;
        track.gain = Gain::Decibels { value: -20.0 };
        track.elements.push(TimelineElement {
            id: "element".into(),
            name: "Audio".into(),
            timeline_range: TimeRange {
                start: t(5, 1),
                duration: t(1, 1),
            },
            source_span: ElementSourceSpan::ClassicTrimmed {
                trim_start: t(1, 1),
                trim_end: t(2, 1),
                source_duration: Some(t(4, 1)),
            },
            enabled: false,
            content: ElementContent::Audio {
                asset_id: "asset".into(),
                audio: AudioStyle {
                    stream: None,
                    gain: Gain::Decibels { value: 0.0 },
                    muted: false,
                },
                retime: Retime {
                    rate: RationalRate {
                        numerator: 1,
                        denominator: 1,
                    },
                    maintain_pitch: false,
                    mapping: RetimeMapping::ClassicRoundedTicks { timebase: 120000 },
                },
            },
            visual: None,
            channels: vec![],
        });
        project.sequences[0].tracks.push(track);
        Self {
            captured: AuthorizedSourceArt {
                snapshot: EditorSnapshot {
                    project,
                    revision: "fixed".into(),
                },
                authority: TrustedJobAuthority::LocalOwner,
                scope: "owner/project".into(),
                policy: SourceArtPolicy {
                    max_encoded_bytes: 32 * 1024 * 1024,
                    max_pcm_bytes: 256 * 1024 * 1024,
                    max_rss_bytes: 2 * 1024 * 1024 * 1024,
                    deadline_ms: 60000,
                },
            },
            revoked: false,
            reads: Cell::new(0),
        }
    }
    fn element(&mut self) -> &mut TimelineElement {
        &mut self.captured.snapshot.project.sequences[0].tracks[1].elements[0]
    }
    fn audio(&mut self) -> &mut AudioStyle {
        let ElementContent::Audio { audio, .. } = &mut self.element().content else {
            unreachable!()
        };
        audio
    }
    fn request(&self) -> SourceArtWaveformDisplayRequest {
        SourceArtWaveformDisplayRequest {
            project_id: "project".into(),
            revision: "fixed".into(),
            asset_id: "asset".into(),
            request_id: "waiter".into(),
            sample_rate: 48000,
            sequence_id: "scene".into(),
            track_id: "audio".into(),
            element_id: "element".into(),
            viewport: SourceArtWaveformViewport {
                clip_left_px: 0.0,
                clip_right_px: 2.0,
                pixels_per_second: 100.0,
                css_height: 20.0,
                device_pixel_ratio: 1.0,
            },
            temporary_gain: None,
        }
    }
    fn decoded(&self, peak: f32) -> DecodedSourceArt {
        DecodedSourceArt::Waveform {
            identity: SourceArtIdentity::Browser(BrowserSourceArtIdentity {
                algorithm: SOURCE_PEAKS_ALGORITHM.into(),
                decode_profile: SOURCE_ART_DECODE_PROFILE.into(),
                browser_version: "pinned".into(),
                browser_sha256: "b".repeat(64),
                node_sha256: "c".repeat(64),
                module_sha256: "d".repeat(64),
            }),
            version: self.captured.snapshot.project.assets[0].version.clone(),
            sample_rate: 48000,
            channels: 3,
            total_samples: 192000,
            bucket_size: 128,
            amplitudes: vec![peak; 1500],
        }
    }
    fn display(
        &self,
        input: &SourceArtWaveformDisplayRequest,
        peak: f32,
    ) -> SourceArtWaveformDisplayResponse {
        project_waveform_display(
            &prepare_waveform_display(self, input).unwrap(),
            &self.decoded(peak),
        )
        .unwrap()
    }
}
impl SourceArtRead for Fixture {
    fn fixed_source_art_snapshot(
        &self,
        _: &str,
        _: &str,
    ) -> Result<AuthorizedSourceArt, EditorError> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.captured.clone())
    }
    fn revalidate_source_art(
        &self,
        _: &str,
        _: &str,
        _: &TrustedJobAuthority,
    ) -> Result<(), EditorError> {
        if self.revoked {
            Err(EditorError::new(
                "NOT_AUTHORIZED",
                "Original authority revoked",
            ))
        } else {
            Ok(())
        }
    }
}
fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
}

#[test]
fn independent_db_floor_height_burn_and_element_mute_oracle() {
    let mut fixture = Fixture::new();
    let input = fixture.request();
    let silent = fixture.display(&input, 0.01);
    assert_eq!(silent.bars[0].device_height, 0);
    assert_eq!(silent.bars[0].height_fraction, 0.0);
    let quiet = fixture.display(&input, 0.1);
    close(quiet.bars[0].height_fraction, 0.3535533905932738);
    assert_eq!(
        (quiet.bars[0].device_top, quiet.bars[0].device_height),
        (13, 7)
    );
    // Float32 1 is exact, avoiding a decimal .1*10 threshold near 1.
    let unity = fixture.display(&input, 1.0);
    assert_eq!(unity.bars[0].output_amplitude, 1.0);
    assert!(!unity.bars[0].burn);
    let burned = fixture.display(&input, 1.25);
    assert!(burned.bars[0].burn);
    assert_eq!(
        (burned.bars[0].device_height, burned.bars[0].burn_height),
        (20, 1)
    );
    fixture.audio().muted = true;
    let muted = fixture.display(&input, 1.25);
    assert_eq!(
        (
            muted.bars[0].gain,
            muted.bars[0].device_height,
            muted.bars[0].burn
        ),
        (0.0, 0, false)
    );
    fixture.audio().muted = false;
    fixture.audio().gain = Gain::Decibels { value: -60.0 };
    let low_gain = fixture.display(&input, 1.25);
    close(low_gain.bars[0].gain, 0.001);
    assert_eq!(low_gain.bars[0].device_height, 0);
    assert_eq!(
        fixture.reads.get(),
        6,
        "one fixed snapshot read per preparation"
    );
}

#[test]
fn whole_clip_midpoint_amplitude_interpolation_preserves_trim_rate_and_rejects_invalid_overlays() {
    let mut fixture = Fixture::new();
    fixture.element().timeline_range.duration = t(4, 3);
    fixture.element().source_span = ElementSourceSpan::ClassicTrimmed {
        trim_start: t(1, 1),
        trim_end: t(1, 1),
        source_duration: Some(t(4, 1)),
    };
    if let ElementContent::Audio { retime, .. } = &mut fixture.element().content {
        retime.rate = RationalRate {
            numerator: 3,
            denominator: 2,
        };
    }
    fixture.element().channels.push(AnimationChannel::Scalar {
        id: "gain".into(),
        target: ChannelTarget::Element {
            element_id: "element".into(),
        },
        binding: ScalarBinding::Property {
            parameter: ScalarParameter::GainDecibels,
        },
        keys: vec![
            ScalarKey {
                id: "low".into(),
                segment_id: None,
                time: t(0, 120000),
                value: -20.0,
                left_handle: None,
                right_handle: None,
                segment_to_next: ScalarSegment::Step,
                tangent_mode: TangentMode::Auto,
            },
            ScalarKey {
                id: "high".into(),
                segment_id: None,
                time: t(80000, 120000),
                value: 0.0,
                left_handle: None,
                right_handle: None,
                segment_to_next: ScalarSegment::Step,
                tangent_mode: TangentMode::Auto,
            },
        ],
        extrapolation: Extrapolation {
            before: ExtrapolationMode::Hold,
            after: ExtrapolationMode::Hold,
        },
    });
    let original = fixture.captured.snapshot.clone();
    let mut input = fixture.request();
    // The first bar center is exactly D/2: left 39.5px, pps60, D4/3.
    input.viewport.clip_left_px = 39.5;
    input.viewport.clip_right_px = 41.5;
    input.viewport.pixels_per_second = 60.0;
    let prepared = prepare_waveform_display(&fixture, &input).unwrap();
    let displayed = project_waveform_display(&prepared, &fixture.decoded(1.0)).unwrap();
    close(displayed.bars[0].gain, 0.55); // .1 + (1-.1)*.5, not dB interpolation.
    assert_eq!(displayed.bars[0].source_start_sample, "95400");
    assert_eq!(displayed.bars[0].source_end_sample, "97800");
    assert_eq!(fixture.captured.snapshot, original);
    let samples = projection::gain_samples(&prepared).unwrap();
    close(samples[99], 0.1);
    close(samples[100], 1.0);
    assert_eq!(samples.len(), 200);
    input.temporary_gain = Some(0);
    assert!(
        prepare_waveform_display(&fixture, &input).is_err(),
        "animated static override must fail"
    );
    fixture.element().channels.clear();
    let changed = fixture.display(&input, 1.0);
    assert_eq!(changed.bars[0].gain, 1.0);
    fixture.captured.snapshot.project.sequences[0].tracks[1].locked = true;
    assert_eq!(
        prepare_waveform_display(&fixture, &input).unwrap_err().code,
        "LOCKED_TRACK"
    );
    fixture.captured.snapshot.project.sequences[0].tracks[1].locked = false;
    fixture.audio().gain = Gain::LinearLegacy { value: 1.0 };
    assert_eq!(
        prepare_waveform_display(&fixture, &input).unwrap_err().code,
        "UNSUPPORTED_WAVEFORM_DISPLAY_PROFILE"
    );
    fixture.audio().gain = Gain::Decibels { value: 0.0 };
    input.revision = "wrong".into();
    assert_eq!(
        prepare_waveform_display(&fixture, &input).unwrap_err().code,
        "STORAGE_ERROR"
    );
    input.revision = "fixed".into();
    fixture.revoked = true;
    assert_eq!(
        prepare_waveform_display(&fixture, &input).unwrap_err().code,
        "NOT_AUTHORIZED"
    );
}

#[test]
fn independent_visible_stride_device_pixels_tail_and_budget_oracle() {
    let fixture = Fixture::new();
    let mut input = fixture.request();
    input.viewport.clip_right_px = 5.7;
    input.viewport.device_pixel_ratio = 2.0;
    let displayed = fixture.display(&input, 0.1);
    assert_eq!(displayed.bars.len(), 2); // Only [0,2] and [2,4], no remainder bucket.
    assert_eq!((displayed.canvas.width, displayed.canvas.height), (12, 40));
    assert_eq!(
        (
            displayed.bars[0].device_left,
            displayed.bars[0].device_right
        ),
        (0, 2)
    );
    assert_eq!(
        (
            displayed.bars[1].device_left,
            displayed.bars[1].device_right
        ),
        (4, 6)
    );
    assert_eq!(
        (
            displayed.bars[0].device_top,
            displayed.bars[0].device_height
        ),
        (26, 14)
    );
    assert_eq!(displayed.bars[0].source_start_sample, "48000");
    assert_eq!(displayed.bars[0].source_end_sample, "48960");
    assert_eq!(displayed.bars[1].source_end_sample, "49920");
    let burned = fixture.display(&input, 1.25);
    assert_eq!(burned.bars[0].burn_height, 2);
    input.viewport.clip_right_px = 0.75;
    assert_eq!(fixture.display(&input, 0.1).bars.len(), 1);
    // A minimum one-CSS-pixel bar may start above a fractional canvas.
    // Classic Math.round(-0.5) is zero, unlike Rust's default -1 tie.
    input.viewport.css_height = 2.0 / 3.0;
    input.viewport.device_pixel_ratio = 1.0;
    let fractional = fixture.display(&input, 0.1);
    assert_eq!(fractional.canvas.height, 1);
    assert_eq!(
        (
            fractional.bars[0].device_top,
            fractional.bars[0].device_height
        ),
        (0, 1)
    );
    input.viewport.css_height = 20.0;
    input.viewport.device_pixel_ratio = 2.0;
    input.viewport.clip_left_px = 500.0;
    input.viewport.clip_right_px = 502.0;
    assert_eq!(fixture.display(&input, 0.1).bars[0].device_height, 0);
    input.viewport.clip_left_px = 0.0;
    input.viewport.clip_right_px = 8194.0;
    assert_eq!(
        validate_request(&input).unwrap_err().code,
        "SOURCE_ART_BUDGET_EXCEEDED"
    );
    input.viewport.clip_right_px = 2.0;
    input.viewport.css_height = 5_000_000.0;
    assert_eq!(
        validate_request(&input).unwrap_err().code,
        "SOURCE_ART_BUDGET_EXCEEDED"
    );
    input.viewport.css_height = 20.0;
    input.viewport.pixels_per_second = f64::NAN;
    assert_eq!(
        validate_request(&input).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
    input.viewport.pixels_per_second = 100.0;
    let prepared = prepare_waveform_display(&fixture, &input).unwrap();
    let mut mismatched = fixture.decoded(0.1);
    if let DecodedSourceArt::Waveform { version, .. } = &mut mismatched {
        version.size += 1;
    }
    assert_eq!(
        project_waveform_display(&prepared, &mismatched)
            .unwrap_err()
            .code,
        "ARTIFACT_VERIFICATION_FAILED"
    );
}
