use super::*;
use opencut_contracts::{
    AssetVersion,
    renderer_identity::{NativeLibraryIdentity, NativeSystemIdentity},
};

fn compact_hd_asset() -> opencut_contracts::scene::ProjectAsset {
    use opencut_contracts::scene::*;
    ProjectAsset {
        id: "video".into(),
        name: "video".into(),
        kind: AssetKind::Video,
        version: AssetVersion {
            id: "bytes".into(),
            content_hash: "a".repeat(64),
            size: 8_000_000,
        },
        duration: Some(Time {
            ticks: "8".into(),
            timebase: 1,
        }),
        origin: ResourceOrigin::LocalImport {
            original_name: "video.mp4".into(),
        },
        metadata: ProjectMediaMetadata {
            mime_type: Some("video/mp4".into()),
            intrinsic_size: Some(CanvasSize {
                width: 1920,
                height: 1080,
            }),
            frame_rate: Some(opencut_contracts::FrameRate {
                numerator: 30_000,
                denominator: 1001,
            }),
            rotation_degrees: None,
            audio: AudioPresence::Absent {},
        },
        license: None,
    }
}

#[test]
fn automatic_preparation_retains_compact_hd_and_prepares_unknown_or_heavy_sources() {
    use opencut_contracts::scene::*;
    let asset = compact_hd_asset();
    assert!(!needs_automatic_preparation(&asset));
    let mut portrait = asset.clone();
    portrait.metadata.intrinsic_size = Some(CanvasSize {
        width: 1080,
        height: 1920,
    });
    assert!(!needs_automatic_preparation(&portrait));
    let mut cases = Vec::new();
    let mut value = asset.clone();
    value.metadata.intrinsic_size = Some(CanvasSize {
        width: 3840,
        height: 2160,
    });
    cases.push(value);
    let mut value = asset.clone();
    value.metadata.frame_rate = Some(opencut_contracts::FrameRate {
        numerator: 60,
        denominator: 1,
    });
    cases.push(value);
    let mut value = asset.clone();
    value.version.size = 20_000_001;
    cases.push(value);
    let mut value = asset.clone();
    value.metadata.frame_rate = None;
    cases.push(value);
    let mut value = asset.clone();
    value.metadata.intrinsic_size = None;
    cases.push(value);
    let mut value = asset.clone();
    value.duration = None;
    cases.push(value);
    for ticks in ["0", "-1", "invalid"] {
        let mut value = asset.clone();
        value.duration.as_mut().unwrap().ticks = ticks.into();
        cases.push(value);
    }
    let mut value = asset.clone();
    value.duration.as_mut().unwrap().timebase = 0;
    cases.push(value);
    let mut value = asset.clone();
    value.version.size = u64::MAX;
    cases.push(value);
    for value in cases {
        assert!(needs_automatic_preparation(&value), "{value:?}");
    }
}

fn result() -> SourcePreviewResult {
    let runtime = SourcePreviewRuntimeIdentity {
        host_sha256: "a".repeat(64),
        decoder: SourcePreviewDecoderIdentity {
            worker_sha256: "b".repeat(64),
            decoder_libraries: ["avcodec", "avformat", "avutil", "swscale", "swresample"]
                .into_iter()
                .map(|name| NativeLibraryIdentity {
                    name: name.into(),
                    version: "test".into(),
                    configuration_sha256: "c".repeat(64),
                    binary_sha256: "d".repeat(64),
                })
                .collect(),
            platform: NativeSystemIdentity {
                os: "test".into(),
                architecture: "test".into(),
                kernel_release: "test".into(),
                system_build: "test".into(),
            },
        },
        ffmpeg_sha256: "e".repeat(64),
        ffprobe_sha256: "f".repeat(64),
    };
    SourcePreviewResult {
        identity: SourcePreviewIdentity {
            project_id: "project".into(),
            asset_id: "source".into(),
            source_version: AssetVersion {
                id: "original".into(),
                content_hash: "a".repeat(64),
                size: 100,
            },
            selected_video_stream: Some(1),
            raster_bound: SourcePreviewRaster {
                width: 16,
                height: 16,
            },
            runtime_fingerprint: runtime_fingerprint(&runtime).unwrap(),
            policy_fingerprint: "c".repeat(64),
        },
        producer_revision: "fixed-producer".into(),
        actual_stream_index: 1,
        actual_track_id: 7,
        source_time_base: SourcePreviewTimeBase {
            numerator: 1,
            denominator: 1000,
        },
        proxy_time_base: SourcePreviewTimeBase {
            numerator: 1,
            denominator: 1000,
        },
        encoding: SourcePreviewEncoding::Ffv1Rgba,
        frame_count: "3".into(),
        first_source_pts: "-80".into(),
        last_source_pts: "200".into(),
        last_source_duration: "50".into(),
        video: SourcePreviewArtifact {
            content_hash: "d".repeat(64),
            size_bytes: "100".into(),
            mime_type: "video/x-matroska".into(),
        },
        frame_index: SourcePreviewArtifact {
            content_hash: "e".repeat(64),
            size_bytes: "208".into(),
            mime_type: "application/vnd.opencut.source-preview-index".into(),
        },
        runtime,
    }
}
fn video() -> SourcePreviewFrameVideo {
    SourcePreviewFrameVideo {
        source_width: 16,
        source_height: 16,
        raster_width: 16,
        raster_height: 16,
        rotation_degrees: 0,
        pixel_aspect_numerator: 1,
        pixel_aspect_denominator: 1,
        matrix: SourcePreviewMatrix::Bt709,
        full_range: false,
        has_alpha: true,
    }
}
struct Index {
    reads: usize,
}
impl SourcePreviewFrameIndexRead for Index {
    fn frame_count(&self) -> u64 {
        3
    }
    fn frame(&mut self, ordinal: u64) -> Result<SourcePreviewIndexRecord, EditorError> {
        self.reads += 1;
        let (source_pts, source_duration, proxy_pts) =
            [(-80, 40, 0), (0, 80, 33), (200, 50, 67)][ordinal as usize];
        Ok(SourcePreviewIndexRecord {
            source_pts,
            source_duration,
            proxy_pts,
            proxy_duration: 33,
            source_video: video(),
        })
    }
}
#[test]
fn source_selection_uses_real_vfr_pts_and_keeps_gap_and_terminal_facts() {
    let source = result();
    for (ticks, ordinal, pts, last) in [
        (0, "1", "0", false),
        (79, "1", "0", false),
        (199, "1", "0", false),
        (200, "2", "200", true),
        (300, "2", "200", true),
    ] {
        let mut index = Index { reads: 0 };
        let frame = select_frame(
            &mut index,
            &source,
            &Time {
                ticks: ticks.to_string(),
                timebase: 1000,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                frame.ordinal.as_str(),
                frame.source_pts.as_str(),
                frame.last_frame
            ),
            (ordinal, pts, last)
        );
        assert_eq!(frame.proxy_duration, "33");
        assert!(index.reads <= 4);
    }
    let mut positive = source;
    positive.first_source_pts = "40".into();
    struct Positive;
    impl SourcePreviewFrameIndexRead for Positive {
        fn frame_count(&self) -> u64 {
            3
        }
        fn frame(&mut self, n: u64) -> Result<SourcePreviewIndexRecord, EditorError> {
            Ok(SourcePreviewIndexRecord {
                source_pts: 40 + n as i64 * 80,
                source_duration: 50,
                proxy_pts: n as i64 * 33,
                proxy_duration: 33,
                source_video: video(),
            })
        }
    }
    assert!(
        select_frame(
            &mut Positive,
            &positive,
            &Time {
                ticks: "39".into(),
                timebase: 1000
            }
        )
        .unwrap()
        .is_none()
    );
}
#[test]
fn runtime_facts_and_authority_are_part_of_exact_source_identity() {
    let source = result();
    validate_public_result(&source).unwrap();
    let first = request_identity(&source.identity, &TrustedJobAuthority::LocalOwner).unwrap();
    let second = request_identity(
        &source.identity,
        &TrustedJobAuthority::Credential {
            principal_id: "principal".into(),
            credential_id: "credential".into(),
        },
    )
    .unwrap();
    assert_ne!(first.idempotency_key, second.idempotency_key);
    let mut altered = source.clone();
    altered.runtime.ffmpeg_sha256 = "0".repeat(64);
    assert!(validate_public_result(&altered).is_err());
    altered = source.clone();
    altered.identity.selected_video_stream = Some(0);
    assert!(validate_public_result(&altered).is_err());
    altered = source;
    altered.frame_index.size_bytes = "207".into();
    assert!(validate_public_result(&altered).is_err());
}

#[test]
fn deep_trimmed_compact_hd_uses_original_unless_explicit_or_retimed() {
    use opencut_contracts::{CreateProject, FrameRate, scene::*};
    let mut project = crate::scene::new_document(&CreateProject {
        id: "project".into(),
        name: "Source selection".into(),
        sequence_id: "scene".into(),
        width: 1920,
        height: 1080,
        fps: FrameRate {
            numerator: 25,
            denominator: 1,
        },
    })
    .unwrap();
    project.assets.push(compact_hd_asset());
    let t = |ticks: &str| Time {
        ticks: ticks.into(),
        timebase: 1,
    };
    let element: TimelineElement = serde_json::from_value(serde_json::json!({
        "id":"clip", "name":"Deep source start", "enabled":true,
        "timeline_range":{"start":t("0"),"duration":t("1")},
        "source_span":{"profile":"classic_trimmed","trim_start":t("7"),"trim_end":t("0"),"source_duration":t("8")},
        "content":{"kind":"video","asset_id":"video","video_stream":null,"source_audio_enabled":false,
            "audio":{"stream":null,"gain":{"unit":"decibels","value":0},"muted":false},
            "retime":{"rate":{"numerator":1,"denominator":1},"maintain_pitch":true,"mapping":{"kind":"classic_rounded_ticks","timebase":120000}}},
        "visual":{"profile":"classic_canvas_v31","transform":{"position_x_px":0,"position_y_px":0,"scale_x":1,"scale_y":1,"rotation_degrees":0},"opacity":1,"blend_mode":"normal","effects":[],"masks":[]},
        "channels":[]
    })).unwrap();
    project.sequences[0].tracks[0].elements.push(element);
    let mut snapshot = EditorSnapshot {
        project,
        revision: "fixed".into(),
    };
    let input = SourcePreviewPrepare {
        project_id: "project".into(),
        revision: "fixed".into(),
        sequence_id: "scene".into(),
        asset_id: None,
        selected_video_stream: None,
    };
    assert!(source_targets(&snapshot, &input).unwrap().is_empty());
    snapshot.project.sequences[0].tracks[0].elements[0].source_span =
        ElementSourceSpan::LegacySourceRange {
            range: opencut_contracts::TimeRange {
                start: t("7"),
                duration: t("1"),
            },
        };
    assert!(source_targets(&snapshot, &input).unwrap().is_empty());
    let mut explicit = input.clone();
    explicit.asset_id = Some("video".into());
    assert_eq!(
        source_targets(&snapshot, &explicit).unwrap(),
        BTreeSet::from([("video".into(), None)])
    );
    if let ElementContent::Video { retime, .. } =
        &mut snapshot.project.sequences[0].tracks[0].elements[0].content
    {
        retime.rate.numerator = 2;
    }
    assert_eq!(
        source_targets(&snapshot, &input).unwrap(),
        BTreeSet::from([("video".into(), None)])
    );
    if let ElementContent::Video {
        retime,
        video_stream,
        ..
    } = &mut snapshot.project.sequences[0].tracks[0].elements[0].content
    {
        retime.rate.numerator = 1;
        *video_stream = Some(1);
    }
    assert_eq!(
        source_targets(&snapshot, &input).unwrap(),
        BTreeSet::from([("video".into(), Some(1))])
    );
}
