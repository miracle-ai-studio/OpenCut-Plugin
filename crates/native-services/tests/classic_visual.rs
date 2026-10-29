//! Actual isolated native GPU pixel checks. All PNG inputs are synthetic,
//! tiny and owned by a TempDir; no user media or queue-publication claim.
use opencut_asset_store::AssetStore;
use opencut_composition::{CompositionScope, scene::*};
use opencut_contracts::{
    Asset, AssetKind, CreateProject, EditorError, FrameRate, Time, TimeRange, scene::*,
};
use opencut_editor_application::{
    classic_visual::*, media_frame_jobs::LocalVisualExecutionPolicy, scene::new_document,
};
use opencut_native_services::classic_visual::*;
use std::{collections::BTreeMap, path::Path, time::Duration};

fn time(n: i32, d: u32) -> Time {
    Time {
        ticks: n.to_string(),
        timebase: d,
    }
}
fn policy() -> LocalVisualExecutionPolicy {
    LocalVisualExecutionPolicy {
        max_frame_pixels: 16_777_216,
        max_decoded_pixels: 16_777_216,
        max_gpu_bytes: 512 * 1024 * 1024,
        max_rgba_bytes: 64 * 1024 * 1024,
        max_encoded_bytes: 32 * 1024 * 1024,
        max_output_bytes: 129 * 1024 * 1024,
        max_rss_bytes: 2 * 1024 * 1024 * 1024,
        deadline_ms: 45000,
    }
}
fn config() -> ClassicVisualProcessConfig {
    ClassicVisualProcessConfig {
        worker: std::env::var_os("OPENCUT_RENDER_WORKER")
            .expect("Set OPENCUT_RENDER_WORKER to the integrated native worker")
            .into(),
        deadline: Duration::from_secs(45),
        policy: policy(),
        strict_cgroup: None,
    }
}
fn snapshot() -> EditorSnapshot {
    let mut project = new_document(&CreateProject {
        id: "project".into(),
        name: "Synthetic GPU".into(),
        sequence_id: "scene".into(),
        width: 16,
        height: 16,
        fps: FrameRate {
            numerator: 30,
            denominator: 1,
        },
    })
    .unwrap();
    project.sequences[0].main_track_id = "main".into();
    project.sequences[0].tracks[0].id = "main".into();
    EditorSnapshot {
        project,
        revision: "fixed-native-visual".into(),
    }
}
fn add_png(
    directory: &Path,
    snapshot: &mut EditorSnapshot,
    id: &str,
    width: u32,
    height: u32,
    pixel: impl Fn(u32, u32) -> [u8; 4],
) {
    let path = directory.join(format!("synthetic-{id}.png"));
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend(pixel(x, y));
            }
        }
        writer.write_image_data(&pixels).unwrap();
    }
    std::fs::write(&path, &bytes).unwrap();
    let original = Asset {
        id: id.into(),
        name: id.into(),
        kind: AssetKind::Image,
        uri: path.to_string_lossy().into(),
        duration: None,
        version: None,
        metadata: BTreeMap::new(),
    };
    let pinned = AssetStore::open(directory.join("cas"))
        .unwrap()
        .pin_asset(&original)
        .unwrap();
    snapshot.project.assets.push(ProjectAsset {
        id: id.into(),
        name: id.into(),
        kind: AssetKind::Image,
        version: pinned.version.unwrap(),
        duration: None,
        origin: ResourceOrigin::LegacyLocator {
            uri: "unused-origin.png".into(),
        },
        // Deliberately wrong nominal size: only actual decode dimensions may
        // decide contain geometry; the CAS identity remains exact.
        metadata: ProjectMediaMetadata {
            mime_type: Some("image/png".into()),
            intrinsic_size: Some(CanvasSize {
                width: 1,
                height: 1,
            }),
            frame_rate: None,
            rotation_degrees: None,
            audio: AudioPresence::Absent {},
        },
        license: None,
    });
}
fn element(id: &str, asset_id: &str, opacity: f64, scale_x: f64) -> TimelineElement {
    TimelineElement {
        id: id.into(),
        name: id.into(),
        timeline_range: TimeRange {
            start: time(0, 1),
            duration: time(1, 1),
        },
        source_span: ElementSourceSpan::ClassicTrimmed {
            trim_start: time(0, 1),
            trim_end: time(0, 1),
            source_duration: None,
        },
        enabled: true,
        content: ElementContent::Image {
            asset_id: asset_id.into(),
        },
        channels: vec![],
        visual: Some(VisualStyle {
            profile: VisualProfile::ClassicCanvasV31,
            transform: Transform {
                position_x_px: 0.0,
                position_y_px: 0.0,
                scale_x,
                scale_y: 1.0,
                rotation_degrees: 0.0,
            },
            opacity,
            blend_mode: BlendMode::Normal,
            effects: vec![],
            masks: vec![],
        }),
    }
}
#[derive(Default)]
struct Rgba {
    bytes: Vec<u8>,
}
impl ClassicVisualRgbaSink for Rgba {
    fn chunk(&mut self, offset: u64, bytes: &[u8]) -> Result<(), EditorError> {
        assert_eq!(offset, self.bytes.len() as u64);
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}
fn render(root: &Path, snapshot: &EditorSnapshot) -> (Rgba, ClassicVisualExecutionReceipt) {
    let graph =
        compile_frame_scene(snapshot, "scene", &time(1, 7), CompositionScope::Visual).unwrap();
    let prepared = prepare_visual_frame_scene(&graph).unwrap();
    let mut resources = VerifiedClassicVisualResources::new(root, &prepared, || Ok(())).unwrap();
    let handles = resources.handles();
    let mut session = ClassicVisualSession::new(&prepared, &policy()).unwrap();
    let mut rgba = Rgba::default();
    let execution = ClassicVisualProcess { config: config() }
        .execute(&mut session, &mut resources, &handles, &mut rgba)
        .unwrap();
    assert_eq!(rgba.bytes.len(), 16 * 16 * 4);
    assert_eq!(execution.receipt.identity, *prepared.identity());
    assert_eq!(execution.receipt.identity.time, time(1, 7));
    assert_eq!(
        execution.runtime_identity.identity.texture_format,
        "rgba8unorm"
    );
    opencut_editor_application::classic_visual::validate_native_identity(
        &execution.runtime_identity.identity,
    )
    .unwrap();
    for hash in [
        &execution.runtime_identity.identity.renderer_sha256,
        &execution.runtime_identity.identity.shader_sha256,
    ] {
        assert_eq!(hash.len(), 64);
        assert!(
            hash.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }
    #[cfg(unix)]
    unsafe {
        assert_eq!(
            libc::kill(execution.runtime_identity.worker_pid as i32, 0),
            -1,
            "Native worker remains alive after completion"
        );
    }
    (rgba, execution)
}
fn pixel(rgba: &Rgba, x: usize, y: usize) -> [u8; 4] {
    rgba.bytes[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4]
        .try_into()
        .unwrap()
}

#[test]
#[ignore = "Requires the integrated native worker; run explicitly with OPENCUT_RENDER_WORKER"]
fn actual_gpu_contain_layer_order_and_quarter_opacity_have_independent_rgb_oracles() {
    let directory = tempfile::tempdir().unwrap();
    let mut snapshot = snapshot();
    add_png(directory.path(), &mut snapshot, "blue", 16, 16, |_, _| {
        [0, 0, 255, 255]
    });
    add_png(directory.path(), &mut snapshot, "red", 32, 16, |_, _| {
        [255, 0, 0, 255]
    });
    snapshot.project.sequences[0].tracks[0]
        .elements
        .push(element("base", "blue", 1.0, 1.0));
    let mut upper = snapshot.project.sequences[0].tracks[0].clone();
    upper.id = "upper".into();
    upper.name = "Upper".into();
    upper.elements = vec![element("overlay", "red", 0.25, 1.0)];
    snapshot.project.sequences[0].tracks.push(upper);
    let (rgba, receipt) = render(&directory.path().join("cas"), &snapshot);
    // The wide source contains to 16x8 centered at (8,8), leaving blue above
    // and below. Quarter red over opaque blue independently gives 64/0/191.
    assert_eq!(pixel(&rgba, 8, 2), [0, 0, 255, 255]);
    assert_eq!(pixel(&rgba, 8, 8), [64, 0, 191, 255]);
    assert_eq!(pixel(&rgba, 8, 14), [0, 0, 255, 255]);
    assert_eq!(
        receipt
            .receipt
            .bound
            .layers
            .iter()
            .map(|l| l.element_id.as_str())
            .collect::<Vec<_>>(),
        ["base", "overlay"]
    );
    assert!(
        matches!(&receipt.receipt.bound.decoded[1], DecodedVisualSource::ReadyImage { element_id, width: 32, height: 16 } if element_id == "overlay")
    );
    let quad = receipt.receipt.bound.layers[1].quad.as_ref().unwrap();
    assert_eq!((quad.width, quad.height), (16.0, 8.0));
}
#[test]
#[ignore = "Requires the integrated native worker; run explicitly with OPENCUT_RENDER_WORKER"]
fn actual_gpu_negative_scale_flips_asymmetric_source_without_replacing_identity() {
    let directory = tempfile::tempdir().unwrap();
    let mut snapshot = snapshot();
    add_png(
        directory.path(),
        &mut snapshot,
        "asymmetric",
        16,
        16,
        |x, _| {
            if x < 8 {
                [0, 255, 0, 255]
            } else {
                [255, 0, 0, 255]
            }
        },
    );
    snapshot.project.sequences[0].tracks[0]
        .elements
        .push(element("flipped", "asymmetric", 1.0, -1.0));
    let (rgba, receipt) = render(&directory.path().join("cas"), &snapshot);
    assert_eq!(pixel(&rgba, 2, 8), [255, 0, 0, 255]);
    assert_eq!(pixel(&rgba, 13, 8), [0, 255, 0, 255]);
    let quad = receipt.receipt.bound.layers[0].quad.as_ref().unwrap();
    assert!(quad.flip_x);
    assert!(!quad.flip_y);
    assert_eq!(receipt.receipt.bound.layers[0].asset_id, "asymmetric");
    assert_eq!(
        receipt.runtime_identity.identity.decoder_profile,
        opencut_editor_application::classic_visual::NATIVE_DECODER_PROFILE
    );
}

#[test]
#[ignore = "Requires the integrated native worker and FFmpeg; run explicitly with OPENCUT_RENDER_WORKER"]
fn actual_video_trim_requests_green_second_frame_and_records_observed_pts() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("two-colors.rgba");
    let path = directory.path().join("two-colors.webm");
    let mut bytes = Vec::new();
    for color in [[255u8, 0, 0, 255], [0, 255, 0, 255]] {
        for _ in 0..16 * 16 {
            bytes.extend(color);
        }
    }
    std::fs::write(&raw, bytes).unwrap();
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgba",
            "-video_size",
            "16x16",
            "-framerate",
            "2",
            "-i",
        ])
        .arg(&raw)
        .args([
            "-an",
            "-c:v",
            "libvpx-vp9",
            "-lossless",
            "1",
            "-pix_fmt",
            "yuv444p",
            "-color_range",
            "pc",
            "-y",
        ])
        .arg(&path)
        .output()
        .expect("Installed FFmpeg must encode the synthetic local fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pinned = AssetStore::open(directory.path().join("cas"))
        .unwrap()
        .pin_asset(&Asset {
            id: "video".into(),
            name: "Two colors".into(),
            kind: AssetKind::Video,
            uri: path.to_string_lossy().into(),
            duration: Some(time(1, 1)),
            version: None,
            metadata: BTreeMap::new(),
        })
        .unwrap();
    let mut snapshot = snapshot();
    snapshot.project.assets.push(ProjectAsset {
        id: "video".into(),
        name: "Two colors".into(),
        kind: AssetKind::Video,
        version: pinned.version.unwrap(),
        duration: Some(time(1, 1)),
        origin: ResourceOrigin::LegacyLocator {
            uri: "unused-source.webm".into(),
        },
        metadata: ProjectMediaMetadata {
            mime_type: Some("video/webm".into()),
            intrinsic_size: None,
            frame_rate: Some(FrameRate {
                numerator: 2,
                denominator: 1,
            }),
            rotation_degrees: None,
            audio: AudioPresence::Absent {},
        },
        license: None,
    });
    let mut clip = element("trimmed", "video", 1.0, 1.0);
    clip.timeline_range.duration = time(1, 2);
    clip.source_span = ElementSourceSpan::ClassicTrimmed {
        trim_start: time(1, 2),
        trim_end: time(0, 1),
        source_duration: Some(time(1, 1)),
    };
    clip.content = ElementContent::Video {
        asset_id: "video".into(),
        video_stream: None,
        source_audio_enabled: false,
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
    };
    snapshot.project.sequences[0].tracks[0].elements.push(clip);
    let (rgba, receipt) = render(&directory.path().join("cas"), &snapshot);
    // The two frames are authored red then green. A .5-second trim selects
    // green at scene 1/7; no identity/hash equality can establish this oracle.
    let actual = pixel(&rgba, 8, 8);
    assert!(
        actual[0] <= 3 && actual[1] >= 252 && actual[2] <= 3 && actual[3] == 255,
        "Expected green, observed {actual:?}"
    );
    let DecodedVisualSource::ReadyVideo {
        requested_time,
        actual_timestamp_seconds,
        frame_duration_seconds,
        width,
        height,
        ..
    } = &receipt.receipt.bound.decoded[0]
    else {
        panic!("Expected an actual decoded video frame")
    };
    assert_eq!(*requested_time, time(77143, 120000));
    assert_eq!((*width, *height), (16, 16));
    assert!((*actual_timestamp_seconds - 0.5).abs() < 1e-9);
    assert!((*frame_duration_seconds - 0.5).abs() < 1e-9);
}

#[test]
#[ignore = "Requires the integrated native worker; run explicitly with OPENCUT_RENDER_WORKER"]
fn actual_gpu_clockwise_rotation_position_background_and_track_source_alpha_multiply() {
    let directory = tempfile::tempdir().unwrap();
    let mut snapshot = snapshot();
    add_png(
        directory.path(),
        &mut snapshot,
        "quadrants",
        8,
        4,
        |x, y| match (x < 4, y < 2) {
            (true, true) => [255, 0, 0, 128],
            (false, true) => [0, 255, 0, 128],
            (true, false) => [0, 0, 255, 128],
            (false, false) => [255, 255, 0, 128],
        },
    );
    let SettingsProfile::ClassicGlobal { settings } = &mut snapshot.project.settings else {
        panic!("Synthetic fixture requires Classic global settings")
    };
    settings.background = Background::Color {
        color: Color {
            encoding: ColorEncoding::Srgb,
            red: 0.0,
            green: 0.0,
            blue: 1.0,
            alpha: 1.0,
        },
    };
    let track = &mut snapshot.project.sequences[0].tracks[0];
    track.opacity = 0.5;
    let mut quad = element("rotated", "quadrants", 1.0, 1.0);
    let visual = quad.visual.as_mut().unwrap();
    visual.transform.position_x_px = 2.0;
    visual.transform.rotation_degrees = 90.0;
    track.elements.push(quad);
    let (rgba, receipt) = render(&directory.path().join("cas"), &snapshot);
    // With screen Y down, +90 degrees sends TL red to upper right, TR green
    // to lower right, BL blue to upper left and BR yellow to lower left.
    // Source alpha128 times track .5 yields 64/255 over opaque blue. All
    // chosen pixels are inside their quadrants, away from filtering edges.
    assert_eq!(pixel(&rgba, 7, 2), [0, 0, 255, 255]);
    assert_eq!(pixel(&rgba, 12, 2), [64, 0, 191, 255]);
    assert_eq!(pixel(&rgba, 7, 13), [64, 64, 191, 255]);
    assert_eq!(pixel(&rgba, 12, 13), [0, 64, 191, 255]);
    assert_eq!(pixel(&rgba, 2, 8), [0, 0, 255, 255]);
    let bound = &receipt.receipt.bound;
    assert_eq!(bound.background, [0.0, 0.0, 1.0, 1.0]);
    let quad = bound.layers[0].quad.as_ref().unwrap();
    assert_eq!(
        (quad.center_x, quad.center_y, quad.width, quad.height),
        (10.0, 8.0, 16.0, 8.0)
    );
    assert_eq!(quad.rotation_radians, std::f32::consts::FRAC_PI_2);
    assert_eq!(quad.opacity, 0.5);
    assert!(
        matches!(&bound.decoded[0], DecodedVisualSource::ReadyImage { element_id, width: 8, height: 4 } if element_id == "rotated")
    );
}

#[test]
#[ignore = "Requires the integrated native worker; run serially with preview acceptance"]
fn native_scene_preview_reuses_worker_and_binds_real_foreground_alpha() {
    use opencut_editor_application::{
        classic_scene_frame::ClassicSceneFrameSession, font_import::FontImportPolicy,
    };
    use opencut_native_services::native_visual::NativeScenePreview;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let directory = tempfile::tempdir().unwrap();
    let mut snapshot = snapshot();
    add_png(directory.path(), &mut snapshot, "blue", 16, 16, |_, _| {
        [0, 0, 255, 255]
    });
    add_png(directory.path(), &mut snapshot, "hole", 16, 16, |x, _| {
        [255, 0, 0, if x < 8 { 0 } else { 255 }]
    });
    snapshot.project.sequences[0].tracks[0]
        .elements
        .push(element("base", "blue", 1.0, 1.0));
    let mut track = snapshot.project.sequences[0].tracks[0].clone();
    track.id = "upper".into();
    track.elements = vec![element("overlay", "hole", 0.25, 1.0)];
    snapshot.project.sequences[0].tracks.push(track);
    let prepared =
        opencut_editor_application::scene_frame_jobs::prepare(&snapshot, "scene", &time(1, 7))
            .unwrap();
    let mut preview =
        NativeScenePreview::new(config(), Duration::from_secs(30), &mut || Ok(())).unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let authorize = || {
        if allowed.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(EditorError::new(
                "NOT_AUTHORIZED",
                "Revoked synthetic scene",
            ))
        }
    };
    let mut cache = VideoResourceCache::default();
    let mut pid = None;
    for point in [[2, 8], [13, 8], [2, 8]] {
        let mut resources = VerifiedClassicTextResources::new_scene_frame_cached(
            &directory.path().join("cas"),
            &prepared,
            &FontImportPolicy::default(),
            authorize,
            &mut cache,
        )
        .unwrap();
        let handles = resources.handles();
        let fonts = resources.font_handles();
        let mut session =
            ClassicSceneFrameSession::new(&prepared, &policy(), resources.coverage()).unwrap();
        let mut rgba = Rgba::default();
        let result = preview
            .render(
                &mut session,
                &mut resources,
                &handles,
                &fonts,
                &mut rgba,
                Some(point),
            )
            .unwrap();
        if let Some(original) = pid {
            assert_eq!(result.runtime.worker_pid, original);
        } else {
            pid = Some(result.runtime.worker_pid);
        }
        let hits: Vec<_> = result
            .hits
            .iter()
            .map(|hit| (hit.element_id.as_str(), hit.alpha))
            .collect();
        if point[0] < 8 {
            assert_eq!(hits, [("base", 255)]);
            assert_eq!(pixel(&rgba, 2, 8), [0, 0, 255, 255]);
        } else {
            assert_eq!(hits, [("overlay", 64), ("base", 255)]);
            assert_eq!(pixel(&rgba, 13, 8), [64, 0, 191, 255]);
        }
    }
    let mut resources = VerifiedClassicTextResources::new_scene_frame_cached(
        &directory.path().join("cas"),
        &prepared,
        &FontImportPolicy::default(),
        authorize,
        &mut cache,
    )
    .unwrap();
    let handles = resources.handles();
    let fonts = resources.font_handles();
    let mut session =
        ClassicSceneFrameSession::new(&prepared, &policy(), resources.coverage()).unwrap();
    allowed.store(false, Ordering::Release);
    let failure = preview
        .render(
            &mut session,
            &mut resources,
            &handles,
            &fonts,
            &mut Rgba::default(),
            None,
        )
        .err()
        .expect("revoked preview must fail");
    assert_eq!(failure.code, "NOT_AUTHORIZED");
    #[cfg(unix)]
    unsafe {
        assert_eq!(
            libc::kill(pid.unwrap() as i32, 0),
            -1,
            "revoked native owner must be reaped"
        );
    }
}
