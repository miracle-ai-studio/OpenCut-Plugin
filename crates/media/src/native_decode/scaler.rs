//! Bounded, synchronous libswscale conversion with explicit legacy color facts.
use super::{failed, invalid};
use crate::MediaResult;
use ffmpeg_next::{self as av, ffi};
use std::{ffi::c_void, ptr::NonNull};

pub(super) const EXTRA_THREAD_BYTES: u64 = 2 * 1024 * 1024;

/// Reserve an entire extra RGBA surface and 2 MiB for every extra worker's
/// filters/stack. Small frames retain one worker; no unbounded auto threading.
pub(super) fn conversion_threads(pixels: u64, resident: u64, limit: u64) -> u32 {
    let desired = (pixels / (512 * 512)).clamp(1, 4);
    let extra = pixels * 4 + EXTRA_THREAD_BYTES;
    desired.min(1 + limit.saturating_sub(resident) / extra) as u32
}

pub(super) struct FrameScaler {
    context: NonNull<ffi::SwsContext>,
    pub input: av::software::scaling::context::Definition,
    pub threads: u32,
}

impl FrameScaler {
    pub fn new(
        input: av::software::scaling::context::Definition,
        threads: u32,
    ) -> MediaResult<Self> {
        if !(1..=4).contains(&threads) {
            return Err(invalid("Invalid native conversion parallelism"));
        }
        let context = NonNull::new(unsafe { ffi::sws_alloc_context() })
            .ok_or_else(|| invalid("Cannot allocate native scaler"))?;
        let scaler = Self {
            context,
            input,
            threads,
        };
        // This is the initialized compatibility API, retaining the exact
        // BILINEAR/ACCURATE_RND, matrix/range and unspecified-chroma behavior.
        // Dynamic frame metadata would change the established source pixels.
        let result = unsafe {
            let c = scaler.context.as_ptr();
            (*c).src_w = input.width as i32;
            (*c).src_h = input.height as i32;
            (*c).src_format = ffi::AVPixelFormat::from(input.format) as i32;
            (*c).dst_w = input.width as i32;
            (*c).dst_h = input.height as i32;
            (*c).dst_format = ffi::AVPixelFormat::AV_PIX_FMT_RGBA as i32;
            (*c).flags = (av::software::scaling::Flags::BILINEAR
                | av::software::scaling::Flags::ACCURATE_RND)
                .bits() as u32;
            (*c).threads = threads as i32;
            ffi::sws_init_context(c, std::ptr::null_mut(), std::ptr::null_mut())
        };
        if result < 0 {
            return Err(failed(av::Error::from(result)));
        }
        Ok(scaler)
    }

    pub fn convert(
        &mut self,
        frame: &av::frame::Video,
        output: &mut [u8],
        matrix: i32,
        full_range: bool,
    ) -> MediaResult<()> {
        if frame.format() != self.input.format
            || frame.width() != self.input.width
            || frame.height() != self.input.height
            || output.len() as u64 != u64::from(frame.width()) * u64::from(frame.height()) * 4
        {
            return Err(invalid("Native conversion changed its admitted extent"));
        }
        let c = self.context.as_ptr();
        let configured = unsafe {
            let coefficients = ffi::sws_getCoefficients(matrix);
            ffi::sws_setColorspaceDetails(
                c,
                coefficients,
                i32::from(full_range),
                coefficients,
                1,
                0,
                1 << 16,
                1 << 16,
            )
        };
        if configured < 0 {
            return Err(failed(av::Error::from(configured)));
        }
        let mut destination = av::frame::Video::empty();
        destination.set_format(av::format::Pixel::RGBA);
        destination.set_width(frame.width());
        destination.set_height(frame.height());
        // SAFETY: output is exclusively borrowed for this synchronous call.
        // The refcounted wrapper never owns/deallocates the Rust allocation.
        // The pinned initialized sws_scale_frame joins its workers and calls
        // sws_frame_end before returning; an early start error is ended below.
        // Drop destination (and its final buffer reference) before output's
        // borrow ends. No temporary AVFrame-sized raster or copy is needed.
        let result = unsafe {
            let buffer = ffi::av_buffer_create(
                output.as_mut_ptr(),
                output.len(),
                Some(borrowed_output),
                std::ptr::null_mut(),
                0,
            );
            if buffer.is_null() {
                return Err(invalid("Cannot wrap native conversion output"));
            }
            let dst = destination.as_mut_ptr();
            (*dst).buf[0] = buffer;
            (*dst).data[0] = output.as_mut_ptr();
            (*dst).linesize[0] = (frame.width() * 4) as i32;
            let result = ffi::sws_scale_frame(c, dst, frame.as_ptr());
            if result < 0 {
                ffi::sws_frame_end(c);
            }
            result
        };
        if result < 0 {
            return Err(failed(av::Error::from(result)));
        }
        Ok(())
    }
}

unsafe extern "C" fn borrowed_output(_: *mut c_void, _: *mut u8) {}

impl Drop for FrameScaler {
    fn drop(&mut self) {
        let mut context = self.context.as_ptr();
        unsafe { ffi::sws_free_context(&mut context) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallelism_is_capped_by_extent_and_resident_headroom() {
        assert_eq!(conversion_threads(64 * 64, 0, u64::MAX), 1);
        let pixels = 3840 * 2160;
        let resident = pixels * 12;
        let extra = pixels * 4 + EXTRA_THREAD_BYTES;
        assert_eq!(conversion_threads(pixels, resident, resident), 1);
        assert_eq!(
            conversion_threads(pixels, resident, resident + extra - 1),
            1
        );
        assert_eq!(conversion_threads(pixels, resident, resident + extra), 2);
        assert_eq!(
            conversion_threads(pixels, resident, resident + 3 * extra),
            4
        );
        assert_eq!(conversion_threads(pixels, resident, u64::MAX), 4);
    }

    fn legacy(frame: &av::frame::Video, matrix: i32, full: bool) -> Vec<u8> {
        let flags =
            av::software::scaling::Flags::BILINEAR | av::software::scaling::Flags::ACCURATE_RND;
        let mut scaler = av::software::scaling::Context::get(
            frame.format(),
            frame.width(),
            frame.height(),
            av::format::Pixel::RGBA,
            frame.width(),
            frame.height(),
            flags,
        )
        .unwrap();
        let mut output = vec![0; frame.width() as usize * frame.height() as usize * 4];
        unsafe {
            let c = scaler.as_mut_ptr();
            let coefficients = ffi::sws_getCoefficients(matrix);
            assert!(
                ffi::sws_setColorspaceDetails(
                    c,
                    coefficients,
                    i32::from(full),
                    coefficients,
                    1,
                    0,
                    1 << 16,
                    1 << 16
                ) >= 0
            );
            let dst = [
                output.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ];
            let pitch = [(frame.width() * 4) as i32, 0, 0, 0];
            assert_eq!(
                ffi::sws_scale(
                    c,
                    (*frame.as_ptr()).data.as_ptr().cast(),
                    (*frame.as_ptr()).linesize.as_ptr(),
                    0,
                    frame.height() as i32,
                    dst.as_ptr(),
                    pitch.as_ptr()
                ),
                frame.height() as i32
            );
        }
        output
    }

    #[test]
    fn frame_api_preserves_legacy_pixels_and_borrowed_output_canaries() {
        use av::format::Pixel;
        for format in [
            Pixel::YUV420P,
            Pixel::YUV422P,
            Pixel::YUV444P,
            Pixel::NV12,
            Pixel::RGBA,
            Pixel::BGRA,
            Pixel::GRAY8,
            Pixel::YUV420P10LE,
        ] {
            for (width, height) in [(37u32, 19u32), (64, 48)] {
                let mut frame = av::frame::Video::new(format, width, height);
                for plane in 0..frame.planes() {
                    let data = frame.data_mut(plane);
                    for (index, value) in data.iter_mut().enumerate() {
                        *value = ((index * 37 + plane * 53) % 251) as u8;
                    }
                    if format == Pixel::YUV420P10LE {
                        for (index, word) in data.chunks_exact_mut(2).enumerate() {
                            word.copy_from_slice(&((index * 71 % 1024) as u16).to_le_bytes());
                        }
                    }
                }
                let definition = av::software::scaling::context::Definition {
                    format,
                    width,
                    height,
                };
                for matrix in [ffi::SWS_CS_ITU601, ffi::SWS_CS_ITU709, ffi::SWS_CS_BT2020] {
                    for full in [false, true] {
                        let expected = legacy(&frame, matrix as i32, full);
                        for threads in [1, 4] {
                            let mut scaler = FrameScaler::new(definition, threads).unwrap();
                            let size = expected.len();
                            // Repeated calls must not retain the prior borrowed buffer;
                            // guard bytes also cover odd extents and packed formats.
                            for _ in 0..3 {
                                let mut guarded = vec![0xa5; size + 128];
                                scaler
                                    .convert(
                                        &frame,
                                        &mut guarded[64..64 + size],
                                        matrix as i32,
                                        full,
                                    )
                                    .unwrap();
                                assert_eq!(
                                    &guarded[64..64 + size],
                                    expected,
                                    "{format:?} {threads} {full}"
                                );
                                assert!(
                                    guarded[..64]
                                        .iter()
                                        .chain(&guarded[64 + size..])
                                        .all(|v| *v == 0xa5)
                                );
                            }
                            assert!(
                                scaler
                                    .convert(&frame, &mut vec![0; size - 1], matrix as i32, full)
                                    .is_err()
                            );
                        }
                    }
                }
            }
        }
    }
}
