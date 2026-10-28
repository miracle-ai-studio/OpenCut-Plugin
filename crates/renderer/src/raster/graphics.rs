use super::*;
use opencut_contracts::{
    graphic_frame::{GraphicGeometryPlan, GraphicRasterPlan, GraphicRasterReceipt},
    scene::StrokeAlign,
};
use tiny_skia::{BlendMode, FillRule, PixmapPaint, Stroke, Transform};

/// Sampled values only; all parameter defaults, source identity, animation and
/// the logical placement quad remain owned by the composition compiler.
pub fn graphic(
    element_id: &str,
    plan: &GraphicRasterPlan,
    limits: RasterLimits,
) -> Result<(RasterImage, GraphicRasterReceipt)> {
    let stroke = &plan.stroke;
    if !stroke.width_px.is_finite() || stroke.width_px < 0.0 {
        return Err(invalid("Invalid native graphic stroke"));
    }
    let padding = (stroke.width_px
        * match stroke.align {
            StrokeAlign::Outside => 10.0,
            StrokeAlign::Center => 5.0,
            StrokeAlign::Inside => 0.0,
        })
    .ceil();
    if !padding.is_finite() || padding > 16384.0 {
        return Err(budget("Native graphic stroke padding exceeds admission"));
    }
    let padding = padding as u32;
    let width = plan
        .source_width
        .checked_add(padding * 2)
        .ok_or_else(|| budget("Graphic width overflow"))?;
    let height = plan
        .source_height
        .checked_add(padding * 2)
        .ok_or_else(|| budget("Graphic height overflow"))?;
    let bytes_per_pixel = if stroke.width_px == 0.0 {
        4
    } else {
        match stroke.align {
            StrokeAlign::Inside => 5,
            StrokeAlign::Outside => 8,
            StrokeAlign::Center => 4,
        }
    };
    let peak = admit(width, height, bytes_per_pixel, limits)?;
    let path = path(plan)?;
    let fill = paint(plan.fill)?;
    let stroke_paint = paint(stroke.color)?;
    let transform = Transform::from_translate(padding as f32, padding as f32);
    let mut image = pixmap(width, height)?;
    image.fill_path(&path, &fill, FillRule::Winding, transform, None);
    if stroke.width_px > 0.0 {
        let style = Stroke {
            width: (stroke.width_px
                * if stroke.align == StrokeAlign::Center {
                    1.0
                } else {
                    2.0
                }) as f32,
            miter_limit: 10.0,
            ..Default::default()
        };
        match stroke.align {
            StrokeAlign::Center => image.stroke_path(&path, &stroke_paint, &style, transform, None),
            StrokeAlign::Inside => {
                let mut clip = tiny_skia::Mask::new(width, height)
                    .ok_or_else(|| budget("Graphic clip allocation failed"))?;
                clip.fill_path(&path, FillRule::Winding, true, transform);
                image.stroke_path(&path, &stroke_paint, &style, transform, Some(&clip));
            }
            StrokeAlign::Outside => {
                let mut scratch = pixmap(width, height)?;
                scratch.stroke_path(&path, &stroke_paint, &style, transform, None);
                let mut erase = paint([0.0, 0.0, 0.0, 1.0])?;
                erase.blend_mode = BlendMode::DestinationOut;
                scratch.fill_path(&path, &erase, FillRule::Winding, transform, None);
                image.draw_pixmap(
                    0,
                    0,
                    scratch.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }
        }
    }
    Ok((
        finish(image, peak),
        GraphicRasterReceipt {
            element_id: element_id.into(),
            width,
            height,
            origin_x: padding,
            origin_y: padding,
        },
    ))
}

fn path(plan: &GraphicRasterPlan) -> Result<Path> {
    let w = f64::from(plan.source_width);
    let h = f64::from(plan.source_height);
    if w == 0.0 || h == 0.0 {
        return Err(invalid("Empty native graphic extent"));
    }
    let inset = if plan.stroke.align == StrokeAlign::Center {
        plan.stroke.width_px / 2.0
    } else {
        0.0
    };
    let radius = (w.min(h) / 2.0 - inset).max(1.0);
    let mut path = PathBuilder::new();
    match plan.geometry {
        GraphicGeometryPlan::Rectangle {
            corner_radius_percent,
        } => {
            if !corner_radius_percent.is_finite() {
                return Err(invalid("Invalid graphic corner radius"));
            }
            let (width, height) = ((w - inset * 2.0).max(1.0), (h - inset * 2.0).max(1.0));
            return rounded_rect(
                inset,
                inset,
                width,
                height,
                width.min(height) / 2.0 * corner_radius_percent.clamp(0.0, 50.0) / 50.0,
            );
        }
        GraphicGeometryPlan::Ellipse => {
            let rx = (w / 2.0 - inset).max(1.0);
            let ry = (h / 2.0 - inset).max(1.0);
            let rect = tiny_skia::Rect::from_xywh(
                (w / 2.0 - rx) as f32,
                (h / 2.0 - ry) as f32,
                (rx * 2.0) as f32,
                (ry * 2.0) as f32,
            )
            .ok_or_else(|| invalid("Invalid graphic ellipse"))?;
            return PathBuilder::from_oval(rect)
                .ok_or_else(|| invalid("Degenerate graphic ellipse"));
        }
        GraphicGeometryPlan::Star {
            points,
            depth_percent,
        } => {
            if !points.is_finite() || !depth_percent.is_finite() {
                return Err(invalid("Invalid graphic star"));
            }
            let count = points.round().clamp(3.0, 12.0) as usize;
            let depth = depth_percent.clamp(1.0, 99.0) / 100.0;
            for n in 0..count * 2 {
                let r = if n % 2 == 0 { radius } else { radius * depth };
                let a =
                    -std::f64::consts::FRAC_PI_2 + n as f64 * std::f64::consts::PI / count as f64;
                point(
                    &mut path,
                    [w / 2.0 + a.cos() * r, h / 2.0 + a.sin() * r],
                    n == 0,
                );
            }
        }
        GraphicGeometryPlan::Polygon {
            sides,
            corner_radius_percent,
        } => {
            if !sides.is_finite() || !corner_radius_percent.is_finite() {
                return Err(invalid("Invalid graphic polygon"));
            }
            let count = sides.round().clamp(3.0, 12.0) as usize;
            let corner = radius
                * (std::f64::consts::PI / count as f64).sin()
                * corner_radius_percent.clamp(0.0, 50.0)
                / 50.0;
            let vertices: Vec<[f64; 2]> = (0..count)
                .map(|n| {
                    let a = -std::f64::consts::FRAC_PI_2
                        + n as f64 * std::f64::consts::TAU / count as f64;
                    [w / 2.0 + a.cos() * radius, h / 2.0 + a.sin() * radius]
                })
                .collect();
            for (n, &current) in vertices.iter().enumerate() {
                if corner <= 0.0 {
                    point(&mut path, current, n == 0);
                    continue;
                }
                let previous = vertices[(n + count - 1) % count];
                let next = vertices[(n + 1) % count];
                let (a, la) = unit(sub(previous, current));
                let (b, lb) = unit(sub(next, current));
                let angle = (a[0] * b[0] + a[1] * b[1]).clamp(-1.0, 1.0).acos();
                let maximum = la.min(lb) / 2.0;
                let offset = (corner / (angle / 2.0).tan()).min(maximum);
                let start = add(current, mul(a, offset));
                let end = add(current, mul(b, offset));
                point(&mut path, start, n == 0);
                arc_to(&mut path, start, current, end, corner.min(maximum));
            }
        }
    }
    path.close();
    path.finish()
        .ok_or_else(|| invalid("Degenerate native graphic path"))
}
fn point(path: &mut PathBuilder, p: [f64; 2], first: bool) {
    if first {
        path.move_to(p[0] as f32, p[1] as f32)
    } else {
        path.line_to(p[0] as f32, p[1] as f32)
    }
}
fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn add(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}
fn mul(a: [f64; 2], b: f64) -> [f64; 2] {
    [a[0] * b, a[1] * b]
}
fn unit(a: [f64; 2]) -> ([f64; 2], f64) {
    let len = a[0].hypot(a[1]);
    (mul(a, 1.0 / len.max(f64::MIN_POSITIVE)), len)
}
fn arc_to(
    path: &mut PathBuilder,
    previous: [f64; 2],
    corner: [f64; 2],
    next: [f64; 2],
    radius: f64,
) {
    let (a, _) = unit(sub(previous, corner));
    let (b, _) = unit(sub(next, corner));
    let angle = (a[0] * b[0] + a[1] * b[1]).clamp(-1.0, 1.0).acos();
    let distance = radius / (angle / 2.0).tan();
    let start = add(corner, mul(a, distance));
    let end = add(corner, mul(b, distance));
    let (bisector, _) = unit(add(a, b));
    let center = add(corner, mul(bisector, radius / (angle / 2.0).sin()));
    point(path, start, false);
    let start_angle = (start[1] - center[1]).atan2(start[0] - center[0]);
    let mut delta = (end[1] - center[1]).atan2(end[0] - center[0]) - start_angle;
    while delta > std::f64::consts::PI {
        delta -= std::f64::consts::TAU;
    }
    while delta < -std::f64::consts::PI {
        delta += std::f64::consts::TAU;
    }
    let steps = (delta.abs() / std::f64::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    for n in 0..steps {
        let first = start_angle + delta * n as f64 / steps as f64;
        let last = start_angle + delta * (n + 1) as f64 / steps as f64;
        let k = 4.0 / 3.0 * ((last - first) / 4.0).tan();
        let c1 = add(
            center,
            [
                radius * (first.cos() - k * first.sin()),
                radius * (first.sin() + k * first.cos()),
            ],
        );
        let c2 = add(
            center,
            [
                radius * (last.cos() + k * last.sin()),
                radius * (last.sin() - k * last.cos()),
            ],
        );
        let end = add(center, [radius * last.cos(), radius * last.sin()]);
        path.cubic_to(
            c1[0] as f32,
            c1[1] as f32,
            c2[0] as f32,
            c2[1] as f32,
            end[0] as f32,
            end[1] as f32,
        );
    }
}
