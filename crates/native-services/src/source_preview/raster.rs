//! Reversible placement of admitted RGBA into one fixed lossless codec raster.
use opencut_contracts::{EditorError, source_preview::*};
use opencut_editor_application::source_preview::validate_frame_video;

fn position(x: u32, y: u32, video: &SourcePreviewFrameVideo) -> (u32, u32) {
    match video.rotation_degrees {
        90 => (video.raster_height - 1 - y, x),
        180 => (video.raster_width - 1 - x, video.raster_height - 1 - y),
        270 => (y, video.raster_width - 1 - x),
        _ => (x, y),
    }
}
pub(super) fn normalize(
    input: &[u8],
    video: &SourcePreviewFrameVideo,
    raster: SourcePreviewRaster,
    output: &mut Vec<u8>,
    checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
) -> Result<(), EditorError> {
    validate_frame_video(video, raster)?;
    if input.len() != video.raster_width as usize * video.raster_height as usize * 4 {
        return Err(super::protocol("Source frame RGBA extent changed"));
    }
    let length = raster.width as usize * raster.height as usize * 4;
    output.resize(length, 0);
    for chunk in output.chunks_mut(65536) {
        checkpoint()?;
        chunk.fill(0);
    }
    if video.rotation_degrees == 0 {
        copy_rows(
            input,
            video.raster_width,
            output,
            raster.width,
            video,
            checkpoint,
        )?;
        return checkpoint();
    }
    for y in 0..video.raster_height {
        if y % 8 == 0 {
            checkpoint()?;
        }
        for x in 0..video.raster_width {
            let (dx, dy) = position(x, y, video);
            let source = (y as usize * video.raster_width as usize + x as usize) * 4;
            let target = (dy as usize * raster.width as usize + dx as usize) * 4;
            output[target..target + 4].copy_from_slice(&input[source..source + 4]);
        }
    }
    checkpoint()
}
pub(crate) fn restore(
    input: &[u8],
    video: &SourcePreviewFrameVideo,
    raster: SourcePreviewRaster,
    checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
) -> Result<Vec<u8>, EditorError> {
    validate_frame_video(video, raster)?;
    if input.len() != raster.width as usize * raster.height as usize * 4 {
        return Err(super::protocol("Prepared source RGBA extent changed"));
    }
    let mut output = vec![0; video.raster_width as usize * video.raster_height as usize * 4];
    if video.rotation_degrees == 0 {
        copy_rows(
            input,
            raster.width,
            &mut output,
            video.raster_width,
            video,
            checkpoint,
        )?;
        checkpoint()?;
        return Ok(output);
    }
    for y in 0..video.raster_height {
        if y % 8 == 0 {
            checkpoint()?;
        }
        for x in 0..video.raster_width {
            let (sx, sy) = position(x, y, video);
            let source = (sy as usize * raster.width as usize + sx as usize) * 4;
            let target = (y as usize * video.raster_width as usize + x as usize) * 4;
            output[target..target + 4].copy_from_slice(&input[source..source + 4]);
        }
    }
    checkpoint()?;
    Ok(output)
}

fn copy_rows(
    input: &[u8],
    input_width: u32,
    output: &mut [u8],
    output_width: u32,
    video: &SourcePreviewFrameVideo,
    checkpoint: &mut dyn FnMut() -> Result<(), EditorError>,
) -> Result<(), EditorError> {
    let length = video.raster_width as usize * 4;
    for y in 0..video.raster_height as usize {
        if y % 8 == 0 {
            checkpoint()?;
        }
        let source = y * input_width as usize * 4;
        let target = y * output_width as usize * 4;
        output[target..target + length].copy_from_slice(&input[source..source + length]);
    }
    Ok(())
}
