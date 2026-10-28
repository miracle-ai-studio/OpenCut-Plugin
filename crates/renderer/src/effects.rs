//! Native GPU uniforms for the existing prepared effect contract.
use crate::{ColorMatrix, Result, invalid};
use opencut_contracts::scene::{EffectKind, SceneEffect};

pub struct BlurPass {
    pub direction: [f32; 2],
    pub sigma: f32,
    pub step: f32,
}

pub fn blur(effects: &[SceneEffect], width: u32, height: u32) -> Result<Vec<BlurPass>> {
    if effects.len() > 64 {
        return Err(invalid("Too many prepared effects"));
    }
    let mut result = Vec::new();
    for entry in effects.iter().filter(|e| e.enabled) {
        let intensity = match entry.effect {
            EffectKind::ClassicBlur { intensity } => intensity,
            EffectKind::LegacyColorAdjust { .. } => continue,
            _ => {
                return Err(invalid(
                    "Effect was not lowered to the native scene contract",
                ));
            }
        };
        if !intensity.is_finite() || intensity < 0.0 {
            return Err(invalid("Invalid prepared blur intensity"));
        }
        let sx = intensity / 5.0 * f64::from(width) / 1920.0;
        let sy = intensity / 5.0 * f64::from(height) / 1080.0;
        let maximum = sx.max(sy);
        if maximum < 0.001 {
            continue;
        }
        let count = (maximum * maximum / 1600.0).ceil().clamp(1.0, 8.0) as usize;
        let x = sx / (count as f64).sqrt();
        let y = sy / (count as f64).sqrt();
        for _ in 0..count {
            for (sigma, direction) in [(x, [1.0, 0.0]), (y, [0.0, 1.0])] {
                if sigma > 0.0 {
                    if !(sigma as f32).is_finite() {
                        return Err(invalid("Blur uniform overflow"));
                    }
                    result.push(BlurPass {
                        direction,
                        sigma: sigma as f32,
                        step: (sigma / 10.0).max(1.0) as f32,
                    });
                }
            }
        }
    }
    Ok(result)
}

fn nine(value: f64) -> f64 {
    (value * 1_000_000_000.0).round() / 1_000_000_000.0
}
fn plane(contrast: f64, brightness: f64) -> [i32; 2] {
    if contrast == 1.0 && brightness == 0.0 {
        return [4096, 0];
    }
    let gain = (contrast * 4096.0).trunc() as i32;
    let offset = ((brightness * 100.0 + 100.0).floor() * 511.0 / 200.0).floor() as i32
        - 128
        - (f64::from(gain) / 32.0).floor() as i32;
    [gain, offset]
}

pub fn color(effects: &[SceneEffect], matrix: ColorMatrix, rotation: u16) -> Result<Vec<[i32; 8]>> {
    if effects.len() > 64 || ![0, 90, 180, 270].contains(&rotation) {
        return Err(invalid("Invalid prepared color/orientation passes"));
    }
    let mut result = Vec::new();
    for entry in effects.iter().filter(|e| e.enabled) {
        let EffectKind::LegacyColorAdjust {
            exposure_ev,
            contrast,
            saturation,
        } = entry.effect
        else {
            continue;
        };
        if !exposure_ev.is_finite()
            || !(-8.0..=8.0).contains(&exposure_ev)
            || !contrast.is_finite()
            || !(0.0..=4.0).contains(&contrast)
            || !saturation.is_finite()
            || !(0.0..=4.0).contains(&saturation)
        {
            return Err(invalid("Invalid prepared color adjustment"));
        }
        let y = plane(
            nine(contrast),
            nine((exposure_ev.exp2() - 1.0).clamp(-1.0, 1.0)),
        );
        let uv = plane(nine(saturation).min(3.0), 0.0);
        result.push([
            y[0],
            y[1],
            uv[0],
            uv[1],
            i32::from(matches!(matrix, ColorMatrix::Bt709)),
            0,
            0,
            0,
        ]);
    }
    if rotation != 0 && result.is_empty() {
        result.push([4096, 0, 4096, 0, 0, 0, 0, 1]);
    }
    if let Some(first) = result.first_mut() {
        first[6] = i32::from(rotation);
    }
    // Native decode provides encoded RGB directly. The browser-only inverse
    // transfer flag remains zero; applying it here would double-transform it.
    Ok(result)
}
