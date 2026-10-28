use super::*;
use opencut_contracts::{AssetVersion, CreateProject, FrameRate, Time, TimeRange};
use std::cell::Cell;

fn t(ticks: i64, timebase: u32) -> Time {
    Time {
        ticks: ticks.to_string(),
        timebase,
    }
}
fn range(start: i64, duration: i64, base: u32) -> TimeRange {
    TimeRange {
        start: t(start, base),
        duration: t(duration, base),
    }
}
fn policy() -> SourceArtPolicy {
    SourceArtPolicy {
        max_encoded_bytes: 32 * 1024 * 1024,
        max_pcm_bytes: 256 * 1024 * 1024,
        max_rss_bytes: 2 * 1024 * 1024 * 1024,
        deadline_ms: 60000,
    }
}
fn identity(algorithm: &str) -> SourceArtIdentity {
    SourceArtIdentity::Browser(BrowserSourceArtIdentity {
        algorithm: algorithm.into(),
        decode_profile: SOURCE_ART_DECODE_PROFILE.into(),
        browser_version: "pinned-browser".into(),
        browser_sha256: "a".repeat(64),
        node_sha256: "b".repeat(64),
        module_sha256: "c".repeat(64),
    })
}
fn native_identity(algorithm: &str) -> SourceArtIdentity {
    use opencut_contracts::renderer_identity::{NativeLibraryIdentity, NativeSystemIdentity};
    SourceArtIdentity::Native(NativeSourceArtIdentity {
        algorithm: algorithm.into(),
        decode_profile: NATIVE_SOURCE_ART_DECODE_PROFILE.into(),
        worker_sha256: "a".repeat(64),
        module_sha256: "b".repeat(64),
        rasterizer: NATIVE_SOURCE_ART_RASTERIZER.into(),
        platform: NativeSystemIdentity {
            os: "fixture".into(),
            architecture: "fixture".into(),
            kernel_release: "fixture".into(),
            system_build: "fixture".into(),
        },
        decoder_libraries: ["avcodec", "avformat", "avutil", "swscale", "swresample"]
            .into_iter()
            .map(|name| NativeLibraryIdentity {
                name: name.into(),
                version: "fixture".into(),
                configuration_sha256: "c".repeat(64),
                binary_sha256: "d".repeat(64),
            })
            .collect(),
    })
}
#[test]
fn native_source_proofs_are_exact_and_do_not_relabel_historical_browser_facts() {
    let old = identity(SOURCE_PEAKS_ALGORITHM);
    let native = native_identity(SOURCE_PEAKS_ALGORITHM);
    validate_runtime_identity(&old).unwrap();
    validate_runtime_identity(&native).unwrap();
    let legacy = serde_json::to_value(&old).unwrap();
    let current = serde_json::to_value(&native).unwrap();
    assert_eq!(
        legacy,
        serde_json::json!({"algorithm":SOURCE_PEAKS_ALGORITHM,"decode_profile":SOURCE_ART_DECODE_PROFILE,
        "browser_version":"pinned-browser","browser_sha256":"a".repeat(64),"node_sha256":"b".repeat(64),"module_sha256":"c".repeat(64)})
    );
    assert!(current.get("browser_version").is_none());
    assert!(legacy.get("worker_sha256").is_none());
    for value in [legacy.clone(), current.clone()] {
        assert_eq!(
            serde_json::to_value(
                serde_json::from_value::<SourceArtIdentity>(value.clone()).unwrap()
            )
            .unwrap(),
            value
        );
    }
    let mut mixed = current.clone();
    mixed["browser_version"] = "153.0.0.0".into();
    assert!(serde_json::from_value::<SourceArtIdentity>(mixed).is_err());
    let mut unknown = current;
    unknown["platform"]["font_backend"] = "invented".into();
    assert!(serde_json::from_value::<SourceArtIdentity>(unknown).is_err());
    let mut duplicate = native.clone();
    let SourceArtIdentity::Native(value) = &mut duplicate else {
        panic!()
    };
    value.decoder_libraries[1] = value.decoder_libraries[0].clone();
    assert!(validate_runtime_identity(&duplicate).is_err());
    let fixture = Fixture::new();
    let prepared = prepare_source_art(
        &fixture,
        &fixture.request(WaveformProjection::Source {
            range: None,
            bins: 3,
        }),
    )
    .unwrap();
    assert_ne!(
        source_art_decode_key(&prepared, &old).unwrap(),
        source_art_decode_key(&prepared, &native).unwrap()
    );
}
struct Fixture {
    authorized: AuthorizedSourceArt,
    checks: Cell<usize>,
    revoked: bool,
}
impl Fixture {
    fn new() -> Self {
        let mut project = crate::scene::new_document(&CreateProject {
            id: "project".into(),
            name: "Source art".into(),
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
            name: "Source".into(),
            kind: AssetKind::Audio,
            version: AssetVersion {
                id: format!("av_{}", "d".repeat(64)),
                content_hash: "d".repeat(64),
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
        track.elements.push(TimelineElement {
            id: "voice".into(),
            name: "Source voice".into(),
            timeline_range: range(1, 4, 3),
            source_span: ElementSourceSpan::ClassicTrimmed {
                trim_start: t(1, 1),
                trim_end: t(1, 1),
                source_duration: Some(t(4, 1)),
            },
            enabled: true,
            content: ElementContent::Audio {
                asset_id: "asset".into(),
                audio: AudioStyle {
                    stream: None,
                    gain: Gain::Decibels { value: -6.0 },
                    muted: true,
                },
                retime: Retime {
                    rate: RationalRate {
                        numerator: 3,
                        denominator: 2,
                    },
                    maintain_pitch: true,
                    mapping: RetimeMapping::ClassicRoundedTicks { timebase: 120000 },
                },
            },
            visual: None,
            channels: vec![],
        });
        project.sequences[0].tracks.push(track);
        opencut_editor_core::scene::validate_project_scene(&project).unwrap();
        Self {
            authorized: AuthorizedSourceArt {
                snapshot: EditorSnapshot {
                    project,
                    revision: "fixed".into(),
                },
                authority: TrustedJobAuthority::LocalOwner,
                scope: "owner/project".into(),
                policy: policy(),
            },
            checks: Cell::new(0),
            revoked: false,
        }
    }
    fn request(&self, projection: WaveformProjection) -> SourceArtRequest {
        SourceArtRequest {
            project_id: "project".into(),
            revision: "fixed".into(),
            asset_id: "asset".into(),
            request_id: "waiter".into(),
            query: SourceArtQuery::Waveform {
                sample_rate: 48000,
                projection,
            },
        }
    }
}
impl SourceArtRead for Fixture {
    fn fixed_source_art_snapshot(
        &self,
        _: &str,
        _: &str,
    ) -> Result<AuthorizedSourceArt, EditorError> {
        Ok(self.authorized.clone())
    }
    fn revalidate_source_art(
        &self,
        _: &str,
        _: &str,
        _: &TrustedJobAuthority,
    ) -> Result<(), EditorError> {
        self.checks.set(self.checks.get() + 1);
        if self.revoked {
            Err(EditorError::new(
                "NOT_AUTHORIZED",
                "Revoked original caller",
            ))
        } else {
            Ok(())
        }
    }
}
fn source(bins: u32) -> WaveformProjection {
    WaveformProjection::Source { range: None, bins }
}
fn waveform(total: u64, amplitudes: Vec<f32>) -> DecodedSourceArt {
    DecodedSourceArt::Waveform {
        identity: identity(SOURCE_PEAKS_ALGORITHM),
        version: AssetVersion {
            id: format!("av_{}", "d".repeat(64)),
            content_hash: "d".repeat(64),
            size: 1,
        },
        sample_rate: 48000,
        channels: 3,
        total_samples: total,
        bucket_size: 128,
        amplitudes,
    }
}
fn projected(prepared: &PreparedSourceArt, decoded: &DecodedSourceArt) -> Vec<f32> {
    let response = project_source_art(prepared, decoded).unwrap();
    assert_eq!(
        response.identity.decode_profile(),
        SOURCE_ART_DECODE_PROFILE
    );
    let SourceArtData::Waveform { amplitudes, .. } = response.data else {
        panic!("waveform expected")
    };
    amplitudes
}

#[test]
fn whole_source_projection_preserves_all_channel_peaks_and_eof() {
    let fixture = Fixture::new();
    let prepared = prepare_source_art(&fixture, &fixture.request(source(4))).unwrap();
    let decoded = waveform(512, vec![0.25, 0.75, 1.25, 0.125]);
    assert_eq!(
        projected(&prepared, &decoded),
        vec![0.25, 0.75, 1.25, 0.125]
    );
    // Source peaks are unaffected by this element's mute, -6dB or keepPitch.
    let request = fixture.request(WaveformProjection::Source {
        range: Some(range(511, 3, 48000)),
        bins: 3,
    });
    let prepared = prepare_source_art(&fixture, &request).unwrap();
    assert_eq!(projected(&prepared, &decoded), vec![0.125, 0.0, 0.0]);
    assert_eq!(projected(&prepared, &waveform(0, vec![0.0])), vec![0.0; 3]);
}

#[test]
fn element_nonzero_trim_fractional_rate_and_final_partial_bin_have_independent_sample_oracle() {
    let fixture = Fixture::new();
    let request = fixture.request(WaveformProjection::Element {
        sequence_id: "scene".into(),
        track_id: "audio".into(),
        element_id: "voice".into(),
        local_range: range(1, 257, 48000),
        bins: 3,
        bin_duration: t(128, 48000),
    });
    let prepared = prepare_source_art(&fixture, &request).unwrap();
    let mut peaks = vec![0.0; 1500];
    peaks[375] = 0.25;
    peaks[376] = 0.5;
    peaks[377] = 0.75;
    peaks[378] = 1.0;
    peaks[379] = 9.0;
    // 48000 + localSamples*3/2 gives sample windows [48001,48194),
    // [48193,48386), [48385,48387). Intersecting source128 buckets are
    // [375,377), [376,379), [378,379); the 9.0 peak is outside the clipped end.
    assert_eq!(
        projected(&prepared, &waveform(192000, peaks)),
        vec![0.5, 1.0, 1.0]
    );
    let mut invalid = request.clone();
    if let SourceArtQuery::Waveform {
        projection: WaveformProjection::Element { bins, .. },
        ..
    } = &mut invalid.query
    {
        *bins = 2;
    }
    assert_eq!(
        validate_request(&invalid).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
    let mut beyond = request;
    if let SourceArtQuery::Waveform {
        projection:
            WaveformProjection::Element {
                local_range,
                bins,
                bin_duration,
                ..
            },
        ..
    } = &mut beyond.query
    {
        *local_range = range(1, 1, 1);
        *bins = 1;
        *bin_duration = t(1, 1);
    }
    assert_eq!(
        prepare_source_art(&fixture, &beyond).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
}

#[test]
fn element_source_projection_uses_classic_effective_rate_without_mutating_authored_clock() {
    let mut fixture = Fixture::new();
    let element = &mut fixture.authorized.snapshot.project.sequences[0].tracks[1].elements[0];
    element.timeline_range.duration = t(1, 4);
    if let ElementContent::Audio { retime, .. } = &mut element.content {
        retime.rate = RationalRate {
            numerator: 8,
            denominator: 1,
        };
    }
    let original = fixture.authorized.snapshot.clone();
    let request = fixture.request(WaveformProjection::Element {
        sequence_id: "scene".into(),
        track_id: "audio".into(),
        element_id: "voice".into(),
        local_range: range(0, 128, 48000),
        bins: 1,
        bin_duration: t(128, 48000),
    });
    let prepared = prepare_source_art(&fixture, &request).unwrap();
    let mut peaks = vec![0.0; 1500];
    peaks[379] = 0.75;
    peaks[380] = 8.0;
    // Effective rate5: source [48000,48640), exactly five128 buckets.
    assert_eq!(projected(&prepared, &waveform(192000, peaks)), vec![0.75]);
    assert_eq!(fixture.authorized.snapshot, original);
}

#[test]
fn preparation_fences_fixed_scope_and_revoked_original_authority() {
    let mut fixture = Fixture::new();
    let request = fixture.request(source(1));
    fixture.revoked = true;
    assert_eq!(
        prepare_source_art(&fixture, &request).unwrap_err().code,
        "NOT_AUTHORIZED"
    );
    assert_eq!(fixture.checks.get(), 1);
    fixture.revoked = false;
    fixture.authorized.snapshot.revision = "different".into();
    assert_eq!(
        prepare_source_art(&fixture, &request).unwrap_err().code,
        "STORAGE_ERROR"
    );
    fixture.authorized.snapshot.revision = "fixed".into();
    fixture.authorized.scope = "x".repeat(4096);
    assert!(prepare_source_art(&fixture, &request).is_ok());
    fixture.authorized.scope.push('x');
    assert_eq!(
        prepare_source_art(&fixture, &request).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
    assert!(
        validate_cancel(&SourceArtCancel {
            project_id: "project".into(),
            revision: "fixed".into(),
            request_id: "waiter".into()
        })
        .is_ok()
    );
}

#[test]
fn raw_cache_identity_includes_source_facts_policy_and_authority_but_not_viewport_or_waiter() {
    let fixture = Fixture::new();
    let prepared = prepare_source_art(&fixture, &fixture.request(source(4))).unwrap();
    let runtime = identity(SOURCE_PEAKS_ALGORITHM);
    let key = source_art_decode_key(&prepared, &runtime).unwrap();
    let mut same = prepared.clone();
    same.request.request_id = "other-waiter".into();
    same.request.revision = "historical".into();
    same.request.query = SourceArtQuery::Waveform {
        sample_rate: 48000,
        projection: source(1),
    };
    assert_eq!(source_art_decode_key(&same, &runtime).unwrap(), key);
    for changed in 0..6 {
        let mut other = prepared.clone();
        let mut runtime = runtime.clone();
        match changed {
            0 => other.scope = "credential2/project".into(),
            1 => other.asset.kind = AssetKind::Video,
            2 => other.asset.metadata.audio = AudioPresence::Unknown {},
            3 => other.asset.version.size += 1,
            4 => other.policy.deadline_ms -= 1,
            _ => {
                let SourceArtIdentity::Browser(value) = &mut runtime else {
                    panic!()
                };
                value.module_sha256 = "e".repeat(64);
            }
        }
        assert_ne!(source_art_decode_key(&other, &runtime).unwrap(), key);
    }
}

#[test]
fn malformed_or_over_budget_waveform_receipts_are_rejected_before_projection() {
    let fixture = Fixture::new();
    let prepared = prepare_source_art(&fixture, &fixture.request(source(1))).unwrap();
    for decoded in [
        waveform(129, vec![0.5]),
        waveform(128, vec![f32::NAN]),
        waveform(128, vec![-0.5]),
    ] {
        assert_eq!(
            project_source_art(&prepared, &decoded).unwrap_err().code,
            "ARTIFACT_VERIFICATION_FAILED"
        );
    }
    let mut decoded = waveform(128, vec![0.5]);
    if let DecodedSourceArt::Waveform { sample_rate, .. } = &mut decoded {
        *sample_rate = 44100;
    }
    assert_eq!(
        project_source_art(&prepared, &decoded).unwrap_err().code,
        "ARTIFACT_VERIFICATION_FAILED"
    );
    let mut wrong_version = waveform(128, vec![0.5]);
    if let DecodedSourceArt::Waveform { version, .. } = &mut wrong_version {
        version.size += 1;
    }
    assert_eq!(
        project_source_art(&prepared, &wrong_version)
            .unwrap_err()
            .code,
        "ARTIFACT_VERIFICATION_FAILED"
    );
    let mut bounded = prepared.clone();
    bounded.policy.max_pcm_bytes = 1538; // No bounded range/PCM working set fits.
    assert_eq!(
        project_source_art(&bounded, &waveform(128, vec![0.5]))
            .unwrap_err()
            .code,
        "SOURCE_ART_BUDGET_EXCEEDED"
    );
    let mut request = fixture.request(source(0));
    assert_eq!(
        validate_request(&request).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
    request.query = SourceArtQuery::Waveform {
        sample_rate: 16000,
        projection: source(1),
    };
    assert_eq!(
        validate_request(&request).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
}

// A frame header is sufficient for the pure receipt dimension check. This
// fixture asserts validation only, not successful JPEG decoding or rendering.
fn jpeg_header(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8, 0xff, 0xc0, 0, 17, 8];
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0, 0xff, 0xd9]);
    bytes
}

#[test]
fn thumbnail_receipt_checks_classic_fit_actual_bytes_and_explicit_missing_sample() {
    let mut fixture = Fixture::new();
    fixture.authorized.snapshot.project.assets[0].kind = AssetKind::Video;
    let mut request = fixture.request(source(1));
    request.query = SourceArtQuery::Thumbnail {};
    let prepared = prepare_source_art(&fixture, &request).unwrap();
    let observation = SourceArtThumbnailObservation::Video {
        track_id: 7,
        source_width: 1500,
        source_height: 3000,
        requested_seconds: 1.0,
        actual_timestamp_seconds: Some(0.96),
        frame_duration_seconds: Some(0.04),
    };
    assert_eq!(validation::thumbnail_size(3000, 6000).unwrap(), (360, 720));
    assert_eq!(validation::thumbnail_size(8, 4).unwrap(), (8, 4));
    let decoded = DecodedSourceArt::ThumbnailReady {
        identity: identity(THUMBNAIL_ALGORITHM),
        version: prepared.asset().version.clone(),
        width: 360,
        height: 720,
        jpeg_bytes: jpeg_header(360, 720),
        observation: observation.clone(),
    };
    let response = project_source_art(&prepared, &decoded).unwrap();
    let SourceArtData::ThumbnailReady {
        jpeg_bytes_hex,
        content_hash,
        ..
    } = response.data
    else {
        panic!("JPEG expected")
    };
    assert!(jpeg_bytes_hex.starts_with("ffd8"));
    assert!(jpeg_bytes_hex.ends_with("ffd9"));
    assert_eq!(
        content_hash,
        format!("{:x}", Sha256::digest(jpeg_header(360, 720)))
    );
    let mut wrong = decoded.clone();
    if let DecodedSourceArt::ThumbnailReady { jpeg_bytes, .. } = &mut wrong {
        *jpeg_bytes = jpeg_header(359, 720);
    }
    assert_eq!(
        project_source_art(&prepared, &wrong).unwrap_err().code,
        "ARTIFACT_VERIFICATION_FAILED"
    );
    let missing = DecodedSourceArt::ThumbnailNoFrame {
        identity: identity(THUMBNAIL_ALGORITHM),
        version: prepared.asset().version.clone(),
        observation: observation.clone(),
        reason: SourceArtNoFrameReason::NoSampleAtOneSecond,
    };
    assert_eq!(
        project_source_art(&prepared, &missing).unwrap_err().code,
        "ARTIFACT_VERIFICATION_FAILED"
    );
    let mut missing = missing;
    if let DecodedSourceArt::ThumbnailNoFrame {
        observation:
            SourceArtThumbnailObservation::Video {
                actual_timestamp_seconds,
                frame_duration_seconds,
                ..
            },
        ..
    } = &mut missing
    {
        *actual_timestamp_seconds = None;
        *frame_duration_seconds = None;
    }
    assert!(matches!(
        project_source_art(&prepared, &missing).unwrap().data,
        SourceArtData::ThumbnailNoFrame { .. }
    ));
    let mut oversized = decoded;
    if let DecodedSourceArt::ThumbnailReady { jpeg_bytes, .. } = &mut oversized {
        *jpeg_bytes = vec![0; SOURCE_ART_MAX_JPEG_BYTES + 1];
    }
    assert_eq!(
        project_source_art(&prepared, &oversized).unwrap_err().code,
        "SOURCE_ART_BUDGET_EXCEEDED"
    );
}
