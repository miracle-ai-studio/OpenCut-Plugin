use super::*;
use opencut_contracts::{
    mask_frame::MaskQuad,
    scene::{LayerMaskShape, Mask, MaskFeather, StrokeAlign},
};
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, PixmapPaint, Point, SpreadMode,
    Stroke, Transform,
};

pub struct SceneMaskRaster {
    pub body: RasterImage,
    pub stroke: Option<RasterImage>,
    pub feather: f32,
    pub inverted: bool,
}

/// Text masks supply a font-verified local body/stroke from the same native
/// text rasterizer. Geometry masks need no external source or resource grant.
pub fn scene_mask(
    mask: &Mask,
    quad: &MaskQuad,
    width: u32,
    height: u32,
    text: Option<(&RasterImage, Option<&RasterImage>)>,
    limits: RasterLimits,
) -> Result<Option<SceneMaskRaster>> {
    if !mask.enabled {
        return Ok(None);
    }
    if matches!(&mask.shape, LayerMaskShape::Freeform { path } if !path.closed)
        || matches!(&mask.shape, LayerMaskShape::Text { mask } if mask.content.trim().is_empty())
    {
        return Ok(None);
    }
    if [
        quad.center_x,
        quad.center_y,
        quad.width,
        quad.height,
        quad.rotation_radians,
        quad.opacity,
    ]
    .iter()
    .any(|v| !v.is_finite())
        || quad.width < 0.0
        || quad.height < 0.0
        || quad.width > 16384.0
        || quad.height > 16384.0
        || !mask.stroke.width_px.is_finite()
        || mask.stroke.width_px < 0.0
    {
        return Err(invalid("Invalid prepared native mask geometry"));
    }
    let MaskFeather::ClassicPixels { value } = mask.feather else {
        return Err(invalid("Mask feather was not lowered to scene pixels"));
    };
    if value < 0.0
        || !value.is_finite()
        || !(value as f32).is_finite()
        || (value > 0.0 && (value as f32) / 2.0 == 0.0)
    {
        return Err(invalid("Invalid prepared native mask feather"));
    }
    let local_width = quad.width.round() as u32;
    let local_height = quad.height.round() as u32;
    let copies = if mask.stroke.width_px > 0.0 { 2 } else { 1 };
    let global = admit(width, height, 4, limits)?;
    let local = if local_width == 0 || local_height == 0 {
        0
    } else {
        admit(local_width, local_height, 4, limits)?
    };
    let peak = (global + local) * copies;
    if peak > limits.max_bytes || (global + local) / 4 * copies > limits.max_pixels {
        return Err(budget("Native mask surfaces exceed admission"));
    }
    let mut body = pixmap(width, height)?;
    let mut stroke = if copies == 2 {
        Some(pixmap(width, height)?)
    } else {
        None
    };
    if local_width != 0 && local_height != 0 {
        let mut local_body = pixmap(local_width, local_height)?;
        match &mask.shape {
            LayerMaskShape::Text { .. } => copy_text(&mut local_body, text.map(|(body, _)| body))?,
            LayerMaskShape::Split {
                center_x,
                center_y,
                rotation_degrees,
            } if value > 0.0 => {
                let angle = rotation_degrees.to_radians();
                let nx = snap(angle.cos());
                let ny = snap(angle.sin());
                let x = quad.width / 2.0 + center_x * quad.width;
                let y = quad.height / 2.0 + center_y * quad.height;
                let half = value / 2.0;
                let shader = LinearGradient::new(
                    Point::from_xy((x - nx * half) as f32, (y - ny * half) as f32),
                    Point::from_xy((x + nx * half) as f32, (y + ny * half) as f32),
                    vec![
                        GradientStop::new(0.0, Color::from_rgba8(255, 255, 255, 0)),
                        GradientStop::new(1.0, Color::WHITE),
                    ],
                    SpreadMode::Pad,
                    Transform::identity(),
                )
                .ok_or_else(|| invalid("Invalid split-mask gradient"))?;
                let fill = Paint {
                    shader,
                    ..Default::default()
                };
                let rectangle =
                    tiny_skia::Rect::from_xywh(0.0, 0.0, quad.width as f32, quad.height as f32)
                        .ok_or_else(|| invalid("Invalid split mask extent"))?;
                local_body.fill_rect(rectangle, &fill, Transform::identity(), None);
            }
            _ => {
                if let Some(path) = mask_path(mask, quad.width, quad.height, false)? {
                    local_body.fill_path(
                        &path,
                        &paint([1.0; 4])?,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }
        }
        place(&mut body, &local_body, quad);
        if let Some(output) = &mut stroke {
            let mut local_stroke = pixmap(local_width, local_height)?;
            if matches!(&mask.shape, LayerMaskShape::Text { .. }) {
                copy_text(&mut local_stroke, text.and_then(|(_, stroke)| stroke))?;
            } else if let Some(path) = mask_path(mask, quad.width, quad.height, true)? {
                let c = &mask.stroke.color;
                local_stroke.stroke_path(
                    &path,
                    &paint([c.red, c.green, c.blue, c.alpha])?,
                    &Stroke {
                        width: mask.stroke.width_px as f32,
                        miter_limit: 10.0,
                        ..Default::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
            place(output, &local_stroke, quad);
        }
    }
    Ok(Some(SceneMaskRaster {
        body: finish(body, peak),
        stroke: stroke.map(|stroke| finish(stroke, peak)),
        feather: if matches!(&mask.shape, LayerMaskShape::Split { .. }) {
            0.0
        } else {
            value as f32
        },
        inverted: mask.inverted,
    }))
}
fn copy_text(target: &mut Pixmap, source: Option<&RasterImage>) -> Result<()> {
    let source = source.ok_or_else(|| invalid("Missing verified native text mask"))?;
    if source.width != target.width()
        || source.height != target.height()
        || source.rgba.len() != target.data().len()
    {
        return Err(invalid(
            "Native text mask extent differs from its local canvas",
        ));
    }
    for (input, output) in source
        .rgba
        .chunks_exact(4)
        .zip(target.data_mut().chunks_exact_mut(4))
    {
        output[3] = input[3];
        for n in 0..3 {
            output[n] = ((u16::from(input[n]) * u16::from(input[3]) + 127) / 255) as u8;
        }
    }
    Ok(())
}
fn place(output: &mut Pixmap, local: &Pixmap, quad: &MaskQuad) {
    let sx = quad.width / f64::from(local.width()) * if quad.flip_x { -1.0 } else { 1.0 };
    let sy = quad.height / f64::from(local.height()) * if quad.flip_y { -1.0 } else { 1.0 };
    let (sin, cos) = quad.rotation_radians.sin_cos();
    let a = cos * sx;
    let b = sin * sx;
    let c = -sin * sy;
    let d = cos * sy;
    let x =
        quad.center_x - a * f64::from(local.width()) / 2.0 - c * f64::from(local.height()) / 2.0;
    let y =
        quad.center_y - b * f64::from(local.width()) / 2.0 - d * f64::from(local.height()) / 2.0;
    output.draw_pixmap(
        0,
        0,
        local.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..Default::default()
        },
        Transform::from_row(a as f32, b as f32, c as f32, d as f32, x as f32, y as f32),
        None,
    );
}
fn snap(value: f64) -> f64 {
    if value.abs() < 1e-10 { 0.0 } else { value }
}
fn mask_path(mask: &Mask, width: f64, height: f64, stroke: bool) -> Result<Option<Path>> {
    use LayerMaskShape::*;
    let mut builder = PathBuilder::new();
    let line = |builder: &mut PathBuilder, p: [f64; 2], first: bool| {
        if first {
            builder.move_to(p[0] as f32, p[1] as f32)
        } else {
            builder.line_to(p[0] as f32, p[1] as f32)
        }
    };
    match &mask.shape {
        Split {
            center_x,
            center_y,
            rotation_degrees,
        } => {
            if [*center_x, *center_y, *rotation_degrees]
                .iter()
                .any(|v| !v.is_finite())
            {
                return Err(invalid("Invalid split mask"));
            }
            let cx = width / 2.0 + center_x * width;
            let cy = height / 2.0 + center_y * height;
            let a = rotation_degrees.to_radians();
            let nx = snap(a.cos());
            let ny = snap(a.sin());
            if stroke {
                let mut hits = Vec::new();
                for (x, y, dx, dy) in [
                    (0.0, 0.0, width, 0.0),
                    (width, 0.0, 0.0, height),
                    (width, height, -width, 0.0),
                    (0.0, height, 0.0, -height),
                ] {
                    let denominator = dx * nx + dy * ny;
                    if denominator.abs() < 1e-10 {
                        continue;
                    }
                    let t = -((x - cx) * nx + (y - cy) * ny) / denominator;
                    if (0.0..=1.0).contains(&t) {
                        let p = [x + t * dx, y + t * dy];
                        if !hits
                            .iter()
                            .any(|q: &[f64; 2]| (q[0] - p[0]).hypot(q[1] - p[1]) < 1e-7)
                        {
                            hits.push(p);
                        }
                    }
                }
                if hits.len() >= 2 {
                    line(&mut builder, hits[0], true);
                    line(&mut builder, hits[1], false);
                }
                return Ok(builder.finish());
            }
            let corners = [[0.0, 0.0], [width, 0.0], [width, height], [0.0, height]];
            let mut output = Vec::new();
            let sign = |p: [f64; 2]| (p[0] - cx) * nx + (p[1] - cy) * ny;
            for n in 0..4 {
                let p = corners[n];
                let q = corners[(n + 1) % 4];
                let s = sign(p);
                let t = sign(q);
                if s >= 0.0 {
                    output.push(p);
                }
                if (s < 0.0) != (t < 0.0) {
                    let f = s / (s - t);
                    output.push([p[0] + f * (q[0] - p[0]), p[1] + f * (q[1] - p[1])]);
                }
            }
            let area = (0..output.len())
                .map(|n| {
                    let p = output[n];
                    let q = output[(n + 1) % output.len()];
                    p[0] * q[1] - q[0] * p[1]
                })
                .sum::<f64>()
                .abs()
                / 2.0;
            if output.len() >= 3 && area >= 0.5 {
                for (n, p) in output.into_iter().enumerate() {
                    line(&mut builder, p, n == 0);
                }
                builder.close();
            }
            return Ok(builder.finish());
        }
        Freeform { path } => {
            if path.points.len() > 4096
                || [
                    path.center_x,
                    path.center_y,
                    path.rotation_degrees,
                    path.scale,
                ]
                .iter()
                .any(|v| !v.is_finite())
                || path.scale < 0.0
            {
                return Err(invalid("Invalid native freeform mask"));
            }
            let Some(first) = path.points.first() else {
                return Ok(None);
            };
            let a = path.rotation_degrees.to_radians();
            let (cx, cy) = (
                width / 2.0 + path.center_x * width,
                height / 2.0 + path.center_y * height,
            );
            let point = |x: f64, y: f64| {
                [
                    cx + x * width * path.scale * a.cos() - y * height * path.scale * a.sin(),
                    cy + x * width * path.scale * a.sin() + y * height * path.scale * a.cos(),
                ]
            };
            line(&mut builder, point(first.x, first.y), true);
            for (p, q) in path
                .points
                .iter()
                .zip(path.points.iter().skip(1).chain(std::iter::once(first)))
                .take(if path.closed {
                    path.points.len()
                } else {
                    path.points.len() - 1
                })
            {
                if [p.out_x, p.out_y, q.in_x, q.in_y, q.x, q.y]
                    .iter()
                    .any(|v| !v.is_finite())
                {
                    return Err(invalid("Invalid native mask control point"));
                }
                let p = point(p.out_x, p.out_y);
                let q1 = point(q.in_x, q.in_y);
                let q = point(q.x, q.y);
                builder.cubic_to(
                    p[0] as f32,
                    p[1] as f32,
                    q1[0] as f32,
                    q1[1] as f32,
                    q[0] as f32,
                    q[1] as f32,
                );
            }
            if path.closed {
                builder.close();
            }
            return Ok(builder.finish());
        }
        Text { .. } => return Err(invalid("Text mask requires its verified raster")),
        LegacyRectangle { .. } | LegacyEllipse { .. } => {
            return Err(invalid(
                "Mask geometry was not lowered to the scene contract",
            ));
        }
        _ => {}
    }
    let geometry = match &mask.shape {
        CinematicBars { geometry }
        | Rectangle { geometry }
        | Ellipse { geometry }
        | Heart { geometry }
        | Diamond { geometry }
        | Star { geometry } => geometry,
        _ => unreachable!(),
    };
    if [
        geometry.center_x,
        geometry.center_y,
        geometry.width,
        geometry.height,
        geometry.rotation_degrees,
        geometry.scale,
    ]
    .iter()
    .any(|v| !v.is_finite())
        || geometry.width <= 0.0
        || geometry.height <= 0.0
        || geometry.scale < 0.0
    {
        return Err(invalid("Invalid native box mask"));
    }
    let offset = if stroke {
        match mask.stroke.align {
            StrokeAlign::Inside => -mask.stroke.width_px / 2.0,
            StrokeAlign::Outside => mask.stroke.width_px / 2.0,
            StrokeAlign::Center => 0.0,
        }
    } else {
        0.0
    };
    let mut hw = if matches!(&mask.shape, CinematicBars { .. }) {
        (geometry.width * width).max(width) / 2.0
    } else {
        geometry.width.max(0.01) * width / 2.0
    };
    let mut hh = geometry.height.max(0.01) * height / 2.0;
    if stroke {
        hw = (hw + offset).max(1.0);
        hh = (hh + offset).max(1.0);
    }
    let angle = geometry.rotation_degrees.to_radians();
    let cx = width / 2.0 + geometry.center_x * width;
    let cy = height / 2.0 + geometry.center_y * height;
    let p = |x: f64, y: f64| {
        [
            cx + x * angle.cos() - y * angle.sin(),
            cy + x * angle.sin() + y * angle.cos(),
        ]
    };
    if matches!(&mask.shape, Ellipse { .. }) {
        let rect = tiny_skia::Rect::from_xywh(
            -hw as f32,
            -hh as f32,
            (hw * 2.0) as f32,
            (hh * 2.0) as f32,
        )
        .ok_or_else(|| invalid("Invalid native mask ellipse"))?;
        return Ok(PathBuilder::from_oval(rect).and_then(|path| {
            path.transform(Transform::from_row(
                angle.cos() as f32,
                angle.sin() as f32,
                -angle.sin() as f32,
                angle.cos() as f32,
                cx as f32,
                cy as f32,
            ))
        }));
    }
    if matches!(&mask.shape, Heart { .. }) {
        let start = p(0.0, -hh * 0.475);
        line(&mut builder, start, true);
        for (a, b, end) in [
            (p(hw, -hh * 1.225), p(hw, -hh * 0.125), p(0.0, hh * 0.725)),
            (p(-hw, -hh * 0.125), p(-hw, -hh * 1.225), start),
        ] {
            builder.cubic_to(
                a[0] as f32,
                a[1] as f32,
                b[0] as f32,
                b[1] as f32,
                end[0] as f32,
                end[1] as f32,
            );
        }
    } else {
        let points: Vec<[f64; 2]> = if matches!(&mask.shape, Star { .. }) {
            (0..10)
                .map(|n| {
                    let r = if n % 2 == 0 { 1.0 } else { 0.45 };
                    let a = n as f64 * std::f64::consts::PI / 5.0 - std::f64::consts::FRAC_PI_2;
                    p(a.cos() * hw * r, a.sin() * hh * r)
                })
                .collect()
        } else if matches!(&mask.shape, Diamond { .. }) {
            vec![p(0.0, -hh), p(hw, 0.0), p(0.0, hh), p(-hw, 0.0)]
        } else {
            vec![p(-hw, -hh), p(hw, -hh), p(hw, hh), p(-hw, hh)]
        };
        for (n, p) in points.into_iter().enumerate() {
            line(&mut builder, p, n == 0);
        }
    }
    builder.close();
    Ok(builder.finish())
}
