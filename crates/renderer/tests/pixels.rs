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
        // Change all source bytes while retaining extent and upload slots.
        for phase in 0..3 {
            let expected: Vec<_> = (0..height)
                .flat_map(|y| {
                    (0..width).flat_map(move |x| {
                        [
                            ((x + phase * 13) % 256) as u8,
                            ((y + phase * 29) % 256) as u8,
                            ((x + y + phase * 43) % 251) as u8,
                            255,
                        ]
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
}

#[test]
#[ignore = "Requires a physical native GPU; run in the integrated serial media suite"]
fn successive_sources_with_equal_extents_remain_independent_across_layout_changes() {
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
            max_frame_pixels: 1024,
            max_gpu_bytes: 1024 * 1024,
            max_cpu_bytes: 1024 * 1024,
            max_layers: 2,
        },
    )
    .unwrap();
    // Equal-size sources need independent slots. Change every source and the
    // layout, then return to the old extent; no old texture may leak pixels.
    for (phase, (width, height)) in [(65, 2), (65, 2), (1, 1), (1, 1), (65, 2)]
        .into_iter()
        .enumerate()
    {
        let left = [13 + phase as u8 * 35, 11, 3, 255];
        let right = [4, phase as u8 * 47, 197, 255];
        let a = left.repeat((width * height) as usize);
        let b = right.repeat((width * height) as usize);
        let mut layers = [
            layer(Raster {
                width,
                height,
                rgba: &a,
            }),
            layer(Raster {
                width,
                height,
                rgba: &b,
            }),
        ];
        for (index, input) in layers.iter_mut().enumerate() {
            input.quad.center_x = width as f32 / 2.0 + index as f32 * (width + 1) as f32;
            input.quad.center_y = height as f32 / 2.0;
            input.quad.width = width as f32;
            input.quad.height = height as f32;
            input.effect_size = [width, height];
        }
        let frame = Frame {
            width: width * 2 + 1,
            height,
            clear: [0.0; 4],
            background: Background::Color([0.2, 0.4, 0.6, 1.0]),
            layers: &layers,
            steps: &[Step::Layer(0), Step::Layer(1)],
        };
        let mut output = Output {
            bytes: Vec::new(),
            cancel: false,
            deadline: Instant::now() + Duration::from_secs(30),
            probe: None,
            hits: Vec::new(),
        };
        let receipt = renderer.render(&frame, &mut output).unwrap();
        assert!(receipt.accounted_gpu_bytes <= 1024 * 1024);
        assert_eq!(output.bytes.len(), (frame.width * height * 4) as usize);
        for row in output.bytes.chunks_exact(frame.width as usize * 4) {
            for (x, actual) in row.chunks_exact(4).enumerate() {
                let expected = if x < width as usize {
                    left
                } else if x == width as usize {
                    [51, 102, 153, 255]
                } else {
                    right
                };
                near(actual.try_into().unwrap(), expected);
            }
        }
    }
}

#[test]
#[ignore = "Requires a physical native GPU; run in the integrated serial media suite"]
fn padded_upload_admission_and_mapping_cancellation_remain_bounded() {
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
            max_frame_pixels: 1024,
            max_gpu_bytes: 1024 * 1024,
            max_cpu_bytes: 512 * 1024,
            max_layers: 1,
        },
    )
    .unwrap();
    let rgba = vec![255; 1024 * 4];
    let mut input = layer(Raster {
        width: 1,
        height: 1024,
        rgba: &rgba,
    });
    input.quad.center_x = 0.5;
    input.quad.center_y = 512.0;
    input.quad.width = 1.0;
    input.quad.height = 1024.0;
    input.effect_size = [1, 1024];
    let mut frame = Frame {
        width: 1,
        height: 1024,
        clear: [0.0; 4],
        background: Background::Color([0.0; 4]),
        layers: &[input],
        steps: &[Step::Layer(0)],
    };
    struct Stop {
        checks: usize,
        deadline: Instant,
    }
    impl Execution for Stop {
        fn checkpoint(&mut self) -> Result<()> {
            self.checks += 1;
            if self.checks >= 3 {
                Err(Error::new("JOB_CANCELLED", "Stopped during upload mapping"))
            } else {
                Ok(())
            }
        }
        fn deadline(&self) -> Instant {
            self.deadline
        }
        fn rgba(&mut self, _: u64, _: &[u8]) -> Result<()> {
            panic!("No partial frame may be emitted")
        }
    }
    let mut stop = Stop {
        checks: 0,
        deadline: Instant::now() + Duration::from_secs(30),
    };
    // Narrow rows need256-byte GPU pitches: the two padded buffers alone
    // exceed512KiB even though the source raster has only4096bytes.
    assert_eq!(
        renderer.render(&frame, &mut stop).unwrap_err().code,
        "VISUAL_BUDGET_EXCEEDED"
    );
    frame.width = 8;
    frame.height = 8;
    frame.layers = &[];
    frame.steps = &[];
    stop.checks = 0;
    assert_eq!(
        renderer.render(&frame, &mut stop).unwrap_err().code,
        "JOB_CANCELLED"
    );
    stop.checks = 0;
    assert_eq!(
        renderer.render(&frame, &mut stop).unwrap_err().code,
        "VISUAL_GPU_FAILED"
    );
    let mut renderer = Renderer::new(
        backend,
        Limits {
            max_frame_pixels: 1024,
            max_gpu_bytes: 1024 * 1024,
            max_cpu_bytes: 512 * 1024,
            max_layers: 1,
        },
    )
    .unwrap();
    let mut expired = Output {
        bytes: Vec::new(),
        cancel: false,
        deadline: Instant::now(),
        probe: None,
        hits: Vec::new(),
    };
    assert_eq!(
        renderer.render(&frame, &mut expired).unwrap_err().code,
        "VISUAL_TIME_LIMIT"
    );
    assert!(expired.bytes.is_empty());
    expired.deadline = Instant::now() + Duration::from_secs(30);
    assert_eq!(
        renderer.render(&frame, &mut expired).unwrap_err().code,
        "VISUAL_GPU_FAILED",
        "An expired mapping retires its native scratch"
    );
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
