use super::*;
use crate::effects::BlurPass;
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
#[derive(Default)]
struct Uploads {
    sources: BTreeMap<(usize, u32, u32), wgpu::Texture>,
    masks: Vec<wgpu::Texture>,
}

impl Renderer {
    pub fn render(&mut self, frame: &Frame<'_>, execution: &mut dyn Execution) -> Result<Receipt> {
        execution.checkpoint()?;
        if self.failed {
            return Err(gpu_error("Native GPU session was retired"));
        }
        let mut admission = self.admit(frame)?;
        let probe = execution.hit_probe();
        if probe.is_some_and(|[x, y]| x >= frame.width || y >= frame.height) {
            return Err(invalid("Hit probe is outside the prepared frame"));
        }
        if probe.is_some() {
            let bytes = frame.layers.len() as u64 * 256;
            admission.receipt.accounted_gpu_bytes = admission
                .receipt
                .accounted_gpu_bytes
                .checked_add(bytes)
                .ok_or_else(|| budget("Hit probe accounting overflow"))?;
            admission.receipt.accounted_cpu_bytes = admission
                .receipt
                .accounted_cpu_bytes
                .checked_add(bytes + frame.layers.len() as u64)
                .ok_or_else(|| budget("Hit probe accounting overflow"))?;
            if admission.receipt.accounted_gpu_bytes > self.limits.max_gpu_bytes
                || admission.receipt.accounted_cpu_bytes > self.limits.max_cpu_bytes
            {
                return Err(budget("Hit probe exceeds frame admission"));
            }
        }
        // Release obsolete storage before allocating replacement scratch. A
        // warm render receives exactly the same budget admission as a cold one.
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.layout != admission.layout)
        {
            self.targets = None;
        }
        let memory_scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation_scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.render_inner(frame, &admission, execution);
        let validation = pollster::block_on(validation_scope.pop());
        let memory = pollster::block_on(memory_scope.pop());
        if result.is_err() || validation.is_some() || memory.is_some() {
            self.targets = None;
            self.failed = true;
        }
        result?;
        if let Some(error) = validation.or(memory) {
            return Err(gpu_error(error));
        }
        execution.checkpoint()?;
        Ok(admission.receipt)
    }

    fn allocate(&self, layout: &Layout) -> Targets {
        let width = layout.width;
        let height = layout.height;
        let aligned = (width * 4).div_ceil(256) * 256;
        Targets {
            layout: layout.clone(),
            aligned,
            textures: std::array::from_fn(|_| texture(&self.device, width, height, FORMAT)),
            masks: (0..layout.masks)
                .map(|_| texture(&self.device, width, height, FORMAT))
                .collect(),
            effects: (0..if layout.effects { 2 } else { 0 })
                .map(|_| texture(&self.device, width, height, FORMAT))
                .collect(),
            colors: layout
                .colors
                .iter()
                .map(|&size| {
                    (
                        size,
                        std::array::from_fn(|_| texture(&self.device, size[0], size[1], FORMAT)),
                    )
                })
                .collect(),
            readback: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bounded frame readback"),
                size: u64::from(aligned) * u64::from(height),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        }
    }

    fn render_inner(
        &mut self,
        frame: &Frame<'_>,
        admission: &Admission,
        execution: &mut dyn Execution,
    ) -> Result<()> {
        if self.targets.is_none() {
            self.targets = Some(self.allocate(&admission.layout));
        }
        let targets = self.targets.as_ref().expect("admitted scratch");
        let probe = execution.hit_probe();
        let hit_buffer = probe.filter(|_| !frame.layers.is_empty()).map(|_| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bounded per-layer hit readback"),
                size: frame.layers.len() as u64 * 256,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fixed frame"),
            });
        let mut current = 0;
        let mut uploads = Uploads::default();
        self.draw(
            &mut encoder,
            &targets.textures[current],
            frame.clear,
            None,
            &[],
        );
        match &frame.background {
            Background::Layer(layer) => self.layer(
                &mut encoder,
                targets,
                layer,
                &admission.background,
                &mut current,
                &mut uploads,
                execution,
                None,
            )?,
            Background::Color(color) => {
                let rgba = background_bytes(*color);
                let layer = Layer {
                    source: Raster {
                        width: 1,
                        height: 1,
                        rgba: &rgba,
                    },
                    quad: VisualQuad {
                        center_x: frame.width as f32 / 2.0,
                        center_y: frame.height as f32 / 2.0,
                        width: frame.width as f32,
                        height: frame.height as f32,
                        rotation_radians: 0.0,
                        opacity: 1.0,
                        flip_x: false,
                        flip_y: false,
                    },
                    blend: BlendMode::Normal,
                    effects: &[],
                    masks: &[],
                    effect_size: [frame.width, frame.height],
                    color_matrix: ColorMatrix::Unspecified,
                    rotation_degrees: 0,
                };
                self.layer(
                    &mut encoder,
                    targets,
                    &layer,
                    &admission.background,
                    &mut current,
                    &mut uploads,
                    execution,
                    None,
                )?;
            }
        }
        for (index, step) in frame.steps.iter().enumerate() {
            execution.checkpoint()?;
            match step {
                Step::Layer(layer) => self.layer(
                    &mut encoder,
                    targets,
                    &frame.layers[*layer],
                    &admission.layers[*layer],
                    &mut current,
                    &mut uploads,
                    execution,
                    hit_buffer
                        .as_ref()
                        .zip(probe)
                        .map(|(buffer, point)| (buffer, *layer as u64 * 256, point)),
                )?,
                Step::Effects(_) => {
                    let passes = &admission.adjustments[&index];
                    if passes.color.is_empty() && passes.blur.is_empty() {
                        continue;
                    }
                    let graded = self.color(
                        &mut encoder,
                        targets,
                        &targets.textures[current],
                        &passes.color,
                        execution,
                    )?;
                    let filtered =
                        self.blur(&mut encoder, targets, graded, &passes.blur, execution)?;
                    let next = 1 - current;
                    encoder.copy_texture_to_texture(
                        copy_texture(filtered),
                        copy_texture(&targets.textures[next]),
                        extent(frame.width, frame.height),
                    );
                    current = next;
                }
            }
        }
        encoder.copy_texture_to_buffer(
            copy_texture(&targets.textures[current]),
            wgpu::TexelCopyBufferInfo {
                buffer: &targets.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(targets.aligned),
                    rows_per_image: Some(frame.height),
                },
            },
            extent(frame.width, frame.height),
        );
        execution.checkpoint()?;
        self.queue.submit([encoder.finish()]);
        let result = self.readback(targets, execution).and_then(|_| {
            if let Some(buffer) = &hit_buffer {
                self.read_hits(buffer, frame.layers.len(), execution)?;
            }
            Ok(())
        });
        // All source uploads are retained through queue completion and never
        // survive this execution. Only bounded scratch survives between frames.
        drop(uploads);
        result
    }

    fn upload_shared(&self, source: Raster<'_>, uploads: &mut Uploads) -> wgpu::Texture {
        let key = (source.rgba.as_ptr() as usize, source.width, source.height);
        uploads
            .sources
            .entry(key)
            .or_insert_with(|| self.upload(source))
            .clone()
    }

    fn upload(&self, source: Raster<'_>) -> wgpu::Texture {
        let result = texture(
            &self.device,
            source.width,
            source.height,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        self.queue.write_texture(
            copy_texture(&result),
            source.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(source.width * 4),
                rows_per_image: Some(source.height),
            },
            extent(source.width, source.height),
        );
        result
    }

    fn layer(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        targets: &Targets,
        layer: &Layer<'_>,
        passes: &Passes,
        current: &mut usize,
        uploads: &mut Uploads,
        execution: &mut dyn Execution,
        hit: Option<(&wgpu::Buffer, u64, [u32; 2])>,
    ) -> Result<()> {
        execution.checkpoint()?;
        // Finish every distance field before placing a foreground into shared
        // mask scratch. A later feather operation must not overwrite it.
        let mut masks = Vec::with_capacity(layer.masks.len());
        for mask in layer.masks {
            execution.checkpoint()?;
            let raw = self.upload_shared(mask.source, uploads);
            let resolved = if mask.feather > 0.0 {
                let result = texture(
                    &self.device,
                    targets.layout.width,
                    targets.layout.height,
                    FORMAT,
                );
                self.feather(encoder, targets, &raw, &result, mask.feather, execution)?;
                result
            } else {
                raw
            };
            masks.push((resolved, mask.inverted));
        }
        let uploaded = self.upload_shared(layer.source, uploads);
        let graded = self.color(encoder, targets, &uploaded, &passes.color, execution)?;
        let pipeline = &self.pipelines.layer;
        let input = self.texture_group(pipeline, 0, graded);
        let q = &layer.quad;
        let placement = self.uniform(
            pipeline,
            1,
            &floats(&[
                targets.layout.width as f32,
                targets.layout.height as f32,
                q.center_x,
                q.center_y,
                q.width,
                q.height,
                q.rotation_radians,
                q.opacity,
                if q.flip_x { 1.0 } else { 0.0 },
                if q.flip_y { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ]),
        );
        self.draw(
            encoder,
            &targets.textures[2],
            [0.0; 4],
            Some(pipeline),
            &[&input, &placement],
        );
        let mut foreground = self.blur(
            encoder,
            targets,
            &targets.textures[2],
            &passes.blur,
            execution,
        )?;
        for (mask, inverted) in &masks {
            execution.checkpoint()?;
            let output = if std::ptr::eq(foreground, &targets.masks[0]) {
                &targets.textures[2]
            } else {
                &targets.masks[0]
            };
            let pipeline = &self.pipelines.mask;
            self.draw(
                encoder,
                output,
                [0.0; 4],
                Some(pipeline),
                &[
                    &self.texture_group(pipeline, 0, foreground),
                    &self.texture_group(pipeline, 1, mask),
                    &self.uniform(
                        pipeline,
                        2,
                        &floats(&[if *inverted { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0]),
                    ),
                ],
            );
            foreground = output;
        }
        if let Some((buffer, offset, [x, y])) = hit {
            let mut source = copy_texture(foreground);
            source.origin = wgpu::Origin3d { x, y, z: 0 };
            encoder.copy_texture_to_buffer(
                source,
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(1),
                    },
                },
                extent(1, 1),
            );
        }
        let pipeline = &self.pipelines.blend;
        let mode: Vec<u8> = [blend_index(layer.blend), 0, 0, 0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let next = 1 - *current;
        self.draw(
            encoder,
            &targets.textures[next],
            [0.0; 4],
            Some(pipeline),
            &[
                &self.texture_group(pipeline, 0, &targets.textures[*current]),
                &self.texture_group(pipeline, 1, foreground),
                &self.uniform(pipeline, 2, &mode),
            ],
        );
        *current = next;
        uploads
            .masks
            .extend(masks.into_iter().map(|(texture, _)| texture));
        Ok(())
    }

    fn color<'t>(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        targets: &'t Targets,
        input: &'t wgpu::Texture,
        passes: &[[i32; 8]],
        execution: &mut dyn Execution,
    ) -> Result<&'t wgpu::Texture> {
        let Some(first) = passes.first() else {
            return Ok(input);
        };
        let size = if first[6] % 180 == 0 {
            [input.width(), input.height()]
        } else {
            [input.height(), input.width()]
        };
        let scratch = &targets.colors[&size];
        let mut current = input;
        let pipeline = &self.pipelines.color;
        for values in passes {
            execution.checkpoint()?;
            let output = if std::ptr::eq(current, &scratch[0]) {
                &scratch[1]
            } else {
                &scratch[0]
            };
            let view = current.create_view(&Default::default());
            let source = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("source color texture"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            self.draw(
                encoder,
                output,
                [0.0; 4],
                Some(pipeline),
                &[&source, &self.uniform(pipeline, 1, &bytes)],
            );
            current = output;
        }
        Ok(current)
    }

    fn blur<'t>(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        targets: &'t Targets,
        input: &'t wgpu::Texture,
        passes: &[BlurPass],
        execution: &mut dyn Execution,
    ) -> Result<&'t wgpu::Texture> {
        let mut current = input;
        let pipeline = &self.pipelines.blur;
        for pass in passes {
            execution.checkpoint()?;
            let output = if std::ptr::eq(current, &targets.effects[0]) {
                &targets.effects[1]
            } else {
                &targets.effects[0]
            };
            self.draw(
                encoder,
                output,
                [0.0; 4],
                Some(pipeline),
                &[
                    &self.texture_group(pipeline, 0, current),
                    &self.uniform(
                        pipeline,
                        1,
                        &floats(&[
                            targets.layout.width as f32,
                            targets.layout.height as f32,
                            pass.direction[0],
                            pass.direction[1],
                            pass.sigma,
                            pass.step,
                            0.0,
                            0.0,
                        ]),
                    ),
                ],
            );
            current = output;
        }
        Ok(current)
    }

    fn feather(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        targets: &Targets,
        raw: &wgpu::Texture,
        output: &wgpu::Texture,
        feather: f32,
        execution: &mut dyn Execution,
    ) -> Result<()> {
        let width = targets.layout.width;
        let height = targets.layout.height;
        let steps = u32::BITS - (width.max(height) - 1).leading_zeros();
        let p = &self.pipelines;
        let mut run = |invert: f32, first: usize, second: usize| -> Result<(usize, usize)> {
            let (mut ping, mut pong) = (first, second);
            execution.checkpoint()?;
            self.draw(
                encoder,
                &targets.masks[ping],
                [1.0; 4],
                Some(&p.init),
                &[
                    &self.sampled_group(&p.init, 0, raw, &self.nearest),
                    &self.uniform(
                        &p.init,
                        1,
                        &floats(&[width as f32, height as f32, invert, 0.0]),
                    ),
                ],
            );
            for step in (0..steps).rev() {
                execution.checkpoint()?;
                self.draw(
                    encoder,
                    &targets.masks[pong],
                    [1.0; 4],
                    Some(&p.step),
                    &[
                        &self.sampled_group(&p.step, 0, &targets.masks[ping], &self.nearest),
                        &self.uniform(
                            &p.step,
                            1,
                            &floats(&[width as f32, height as f32, (1u32 << step) as f32, 0.0]),
                        ),
                    ],
                );
                std::mem::swap(&mut ping, &mut pong);
            }
            Ok((ping, pong))
        };
        let (inside, spare) = run(0.0, 0, 1)?;
        let (outside, _) = run(1.0, spare, 2)?;
        self.draw(
            encoder,
            output,
            [0.0; 4],
            Some(&p.distance),
            &[
                &self.sampled_group(&p.distance, 0, &targets.masks[inside], &self.nearest),
                &self.sampled_group(&p.distance, 1, &targets.masks[outside], &self.nearest),
                &self.uniform(
                    &p.distance,
                    2,
                    &floats(&[width as f32, height as f32, feather / 2.0, 0.0]),
                ),
            ],
        );
        Ok(())
    }

    fn map_read(&self, buffer: &wgpu::Buffer, execution: &mut dyn Execution) -> Result<()> {
        let (send, receive) = mpsc::sync_channel(1);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |value| {
                let _ = send.send(value);
            });
        loop {
            execution.checkpoint()?;
            if Instant::now() >= execution.deadline() {
                return Err(Error::new(
                    "VISUAL_TIME_LIMIT",
                    "Native GPU readback exceeded deadline",
                ));
            }
            self.device.poll(wgpu::PollType::Poll).map_err(gpu_error)?;
            match receive.try_recv() {
                Ok(value) => return value.map_err(gpu_error),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(gpu_error("GPU readback channel closed"));
                }
                Err(mpsc::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    fn read_hits(
        &self,
        buffer: &wgpu::Buffer,
        layers: usize,
        execution: &mut dyn Execution,
    ) -> Result<()> {
        let result = (|| {
            self.map_read(buffer, execution)?;
            let mapped = buffer.slice(..).get_mapped_range().map_err(gpu_error)?;
            for index in 0..layers {
                execution.checkpoint()?;
                execution.hit_alpha(index, mapped[index * 256 + 3])?;
            }
            Ok(())
        })();
        buffer.unmap();
        result
    }

    fn readback(&self, targets: &Targets, execution: &mut dyn Execution) -> Result<()> {
        let result = (|| {
            self.map_read(&targets.readback, execution)?;
            let mapped = targets
                .readback
                .slice(..)
                .get_mapped_range()
                .map_err(gpu_error)?;
            let mut chunk = [0u8; CHUNK];
            let (mut used, mut offset) = (0, 0u64);
            for row in mapped.chunks_exact(targets.aligned as usize) {
                for pixel in row[..targets.layout.width as usize * 4].chunks_exact(4) {
                    chunk[used..used + 4]
                        .copy_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                    used += 4;
                    if used == chunk.len() {
                        execution.checkpoint()?;
                        execution.rgba(offset, &chunk)?;
                        offset += used as u64;
                        used = 0;
                    }
                }
            }
            if used != 0 {
                execution.checkpoint()?;
                execution.rgba(offset, &chunk[..used])?;
            }
            Ok(())
        })();
        targets.readback.unmap();
        result
    }
}

fn copy_texture(texture: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    }
}
fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}
