//! Independent arithmetic/geometry oracles; no preview/export self-comparison.
use opencut_renderer::*;
use std::time::{Duration, Instant};
struct Output {
    bytes: Vec<u8>,
    cancel: bool,
    deadline: Instant,
    probe: Option<[u32; 2]>,
    hits: Vec<u8>,
}
impl Execution for Output {
    fn checkpoint(&mut self) -> Result<()> {
        Ok(())
    }
    fn deadline(&self) -> Instant {
        self.deadline
    }
    fn hit_probe(&self) -> Option<[u32; 2]> {
        self.probe
    }
    fn hit_alpha(&mut self, layer: usize, alpha: u8) -> Result<()> {
        assert_eq!(layer, self.hits.len());
        self.hits.push(alpha);
        Ok(())
    }
    fn rgba(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        if self.cancel {
            return Err(Error::new("JOB_CANCELLED", "Synthetic cancelled output"));
        }
        assert_eq!(offset, self.bytes.len() as u64);
        assert!(!bytes.is_empty() && bytes.len() <= 65536 && bytes.len().is_multiple_of(4));
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

#[test]
#[ignore = "Requires a physical native GPU; run in the integrated serial media suite"]
fn rgba_readback_preserves_channels_rows_and_chunk_boundaries() {
    let backend = if cfg!(target_os = "macos") {
        Backend::Metal
    } else if cfg!(target_os = "windows") {
        Backend::DirectX
    } else {
        Backend::Vulkan
    };
    let mut renderer = Renderer::new(
        backend,
        Limits {
            max_frame_pixels: 70000,
            max_gpu_bytes: 16 * 1024 * 1024,
            max_cpu_bytes: 16 * 1024 * 1024,
            max_layers: 1,
        },
    )
    .unwrap();
    assert_eq!(renderer.identity().texture_format, "rgba8unorm");
    // Both paths cross the 64 KiB output boundary. The second has GPU row
    // padding; row/column markers reveal leaked padding, swizzles or shifts.
    for (width, height) in [(64u32, 257u32), (257, 65)] {
        let expected: Vec<_> = (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    [(x % 256) as u8, (y % 256) as u8, ((x + y) % 251) as u8, 255]
                })
            })
            .collect();
        let mut source = layer(Raster {
            width,
            height,
            rgba: &expected,
        });
        source.quad.center_x = width as f32 / 2.0;
        source.quad.center_y = height as f32 / 2.0;
        source.quad.width = width as f32;
        source.quad.height = height as f32;
        source.effect_size = [width, height];
        let frame = Frame {
            width,
            height,
            clear: [0.0; 4],
            background: Background::Color([0.0; 4]),
            layers: &[source],
            steps: &[Step::Layer(0)],
        };
        let mut output = Output {
            bytes: Vec::new(),
            cancel: false,
            deadline: Instant::now() + Duration::from_secs(30),
            probe: None,
            hits: Vec::new(),
        };
        renderer.render(&frame, &mut output).unwrap();
        assert_eq!(output.bytes.len(), expected.len());
        for (actual, expected) in output.bytes.chunks_exact(4).zip(expected.chunks_exact(4)) {
            near(actual.try_into().unwrap(), expected.try_into().unwrap());
        }
    }
}
fn quad() -> VisualQuad {
    VisualQuad {
        center_x: 4.0,
        center_y: 4.0,
        width: 8.0,
        height: 8.0,
        rotation_radians: 0.0,
        opacity: 1.0,
        flip_x: false,
        flip_y: false,
    }
}
fn layer(source: Raster<'_>) -> Layer<'_> {
    Layer {
        source,
        quad: quad(),
        blend: BlendMode::Normal,
        masks: &[],
        effects: &[],
        effect_size: [8, 8],
        color_matrix: ColorMatrix::Unspecified,
        rotation_degrees: 0,
    }
}
fn pixel(bytes: &[u8], x: usize, y: usize) -> [u8; 4] {
    bytes[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4]
        .try_into()
        .unwrap()
}
fn near(actual: [u8; 4], expected: [u8; 4]) {
    for (a, e) in actual.into_iter().zip(expected) {
        assert!(
            a.abs_diff(e) <= 1,
            "actual {actual:?}, expected {expected:?}"
        );
    }
}
#[test]
#[ignore = "Requires a physical native GPU; run in the integrated serial media suite"]
fn native_blend_masks_adjustment_and_cancellation_have_independent_oracles() {
    let backend = if cfg!(target_os = "macos") {
        Backend::Metal
    } else if cfg!(target_os = "windows") {
        Backend::DirectX
    } else {
        Backend::Vulkan
    };
    let mut renderer = Renderer::new(
        backend,
        Limits {
            max_frame_pixels: 64,
            max_gpu_bytes: 1024 * 1024,
            max_cpu_bytes: 1024 * 1024,
            max_layers: 10,
        },
    )
    .unwrap();
    let red = [255, 0, 0, 255];
    let blue = [0, 0, 255, 255];
    let a = layer(Raster {
        width: 1,
        height: 1,
        rgba: &blue,
    });
    let mut b = layer(Raster {
        width: 1,
        height: 1,
        rgba: &red,
    });
    b.quad.opacity = 0.25;
    let layers = [a, b];
    let steps = [Step::Layer(0), Step::Layer(1)];
    let mut frame = Frame {
        width: 8,
        height: 8,
        clear: [0.0; 4],
        background: Background::Color([0.0; 4]),
        layers: &layers,
        steps: &steps,
    };
    let mut out = Output {
        bytes: Vec::new(),
        cancel: false,
        deadline: Instant::now() + Duration::from_secs(30),
        probe: Some([4, 4]),
        hits: Vec::new(),
    };
    renderer.render(&frame, &mut out).unwrap();
    near(pixel(&out.bytes, 4, 4), [64, 0, 191, 255]);
    // An opaque composition still has a quarter-alpha isolated foreground.
    // Reading the final frame's alpha would incorrectly select it as 255.
    assert_eq!(out.hits, [255, 64]);
    out.probe = None;
    out.hits.clear();
    let grey = [128, 128, 128, 255];
    let mut multiply = layer(Raster {
        width: 1,
        height: 1,
        rgba: &grey,
    });
    multiply.blend = BlendMode::Multiply;
    let layers = [
        layer(Raster {
            width: 1,
            height: 1,
            rgba: &red,
        }),
        multiply,
    ];
    frame.layers = &layers;
    out.bytes.clear();
    renderer.render(&frame, &mut out).unwrap();
    near(pixel(&out.bytes, 4, 4), [128, 0, 0, 255]);
    let mask: Vec<_> = (0..64)
        .flat_map(|i| if i % 8 < 4 { [255; 4] } else { [0; 4] })
        .collect();
    let masks = [Mask {
        source: Raster {
            width: 8,
            height: 8,
            rgba: &mask,
        },
        feather: 0.0,
        inverted: false,
    }];
    let mut clipped = layer(Raster {
        width: 1,
        height: 1,
        rgba: &red,
    });
    clipped.masks = &masks;
    let layers = [
        layer(Raster {
            width: 1,
            height: 1,
            rgba: &blue,
        }),
        clipped,
    ];
    frame.layers = &layers;
    out.bytes.clear();
    renderer.render(&frame, &mut out).unwrap();
    near(pixel(&out.bytes, 1, 4), [255, 0, 0, 255]);
    near(pixel(&out.bytes, 6, 4), [0, 0, 255, 255]);
    out.probe = Some([6, 4]);
    out.bytes.clear();
    renderer.render(&frame, &mut out).unwrap();
    assert_eq!(
        out.hits,
        [255, 0],
        "A masked-out foreground must not be selectable"
    );
    out.probe = Some([1, 4]);
    out.bytes.clear();
    out.hits.clear();
    renderer.render(&frame, &mut out).unwrap();
    assert_eq!(out.hits, [255, 255]);
    out.probe = Some([8, 4]);
    out.bytes.clear();
    out.hits.clear();
    assert_eq!(
        renderer.render(&frame, &mut out).unwrap_err().code,
        "INVALID_VISUAL_PLAN"
    );
    assert!(out.bytes.is_empty() && out.hits.is_empty());
    out.probe = None;
    let effect = [SceneEffect {
        id: "blur".into(),
        enabled: true,
        effect: opencut_contracts::scene::EffectKind::ClassicBlur { intensity: 30.0 },
    }];
    let steps = [Step::Layer(0), Step::Effects(&effect)];
    let layers = [layer(Raster {
        width: 1,
        height: 1,
        rgba: &red,
    })];
    frame.layers = &layers;
    frame.steps = &steps;
    out.bytes.clear();
    renderer.render(&frame, &mut out).unwrap();
    near(pixel(&out.bytes, 4, 4), [255, 0, 0, 255]);
    out.cancel = true;
    assert_eq!(
        renderer.render(&frame, &mut out).unwrap_err().code,
        "JOB_CANCELLED"
    );
    out.cancel = false;
    assert!(
        renderer.render(&frame, &mut out).is_err(),
        "failed native owners cannot be reused"
    );
}
