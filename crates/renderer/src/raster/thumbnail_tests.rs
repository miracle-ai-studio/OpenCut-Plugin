use super::*;
fn limits() -> SourceThumbnailLimits {
    SourceThumbnailLimits {
        max_pixels: 1024 * 1024,
        max_bytes: 8 * 1024 * 1024,
        retained_bytes: 65536,
        max_jpeg_bytes: 1024 * 1024,
    }
}
fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut codec = skia::Codec::from_data(skia::Data::new_copy(bytes)).unwrap();
    let info = skia::ImageInfo::new(
        codec.dimensions(),
        skia::ColorType::RGBA8888,
        skia::AlphaType::Unpremul,
        skia::ColorSpace::new_srgb(),
    );
    let mut pixels = vec![0; info.width() as usize * info.height() as usize * 4];
    assert_eq!(
        codec.get_pixels_with_options(&info, &mut pixels, info.width() as usize * 4, None),
        skia::codec::Result::Success
    );
    (info.width() as u32, info.height() as u32, pixels)
}
#[test]
fn cpu_jpeg_keeps_observed_orientation_and_blends_alpha_on_black() {
    let mut rgba = Vec::new();
    for _y in 0..16 {
        for x in 0..32 {
            rgba.extend_from_slice(if x < 16 {
                &[255, 0, 0, 128]
            } else {
                &[0, 0, 255, 255]
            });
        }
    }
    // Independent expected centers: left/right halves become top/bottom
    // under clockwise rotation. JPEG tolerance is unchanged per channel.
    for (rotation, first, second) in [
        (0, [128, 0, 0], [0, 0, 255]),
        (90, [128, 0, 0], [0, 0, 255]),
        (180, [0, 0, 255], [128, 0, 0]),
        (270, [0, 0, 255], [128, 0, 0]),
    ] {
        let image =
            SourceThumbnailImage::rgba(32, 16, &rgba, rotation, limits(), &mut || Ok(())).unwrap();
        let (width, height) = image.source_size();
        let encoded = image.jpeg(width, height, limits(), &mut || Ok(())).unwrap();
        let (actual_width, actual_height, pixels) = decode(&encoded);
        assert_eq!((actual_width, actual_height), (width, height));
        let centers = if rotation % 180 == 0 {
            [(8, 8), (24, 8)]
        } else {
            [(8, 8), (8, 24)]
        };
        for ((x, y), expected) in centers.into_iter().zip([first, second]) {
            let offset = (y * width as usize + x) * 4;
            for c in 0..3 {
                assert!(
                    (i32::from(pixels[offset + c]) - expected[c]).abs() <= 5,
                    "rotation {rotation}, pixel ({x},{y}), channel {c}: {}",
                    pixels[offset + c]
                );
            }
        }
    }
}
#[test]
fn source_thumbnail_limits_reject_before_large_allocation_and_during_jpeg_write() {
    let mut small = limits();
    small.max_bytes = 1024;
    assert!(SourceThumbnailImage::rgba(16384, 16384, &[], 0, small, &mut || Ok(())).is_err());
    assert!(SourceThumbnailImage::rgba(32, 16, &[], 0, limits(), &mut || Ok(())).is_err());
    let rgba = vec![0; 32 * 16 * 4];
    let mut tiny_jpeg = limits();
    tiny_jpeg.max_jpeg_bytes = 16;
    let image = SourceThumbnailImage::rgba(32, 16, &rgba, 0, tiny_jpeg, &mut || Ok(())).unwrap();
    assert!(image.jpeg(32, 16, tiny_jpeg, &mut || Ok(())).is_err());
    let image = SourceThumbnailImage::rgba(32, 16, &rgba, 0, limits(), &mut || Ok(())).unwrap();
    assert!(
        image
            .jpeg(32, 16, limits(), &mut || Err(crate::Error::new(
                "CANCELLED",
                "Source waiter cancelled"
            )))
            .is_err()
    );
    let mut bounded = BoundedJpeg {
        bytes: Vec::new(),
        limit: 4,
        exceeded: false,
    };
    assert_eq!(bounded.write(&[1, 2, 3]).unwrap(), 3);
    assert!(bounded.write(&[4, 5]).is_err());
    assert_eq!(bounded.bytes, vec![1, 2, 3]);
    assert!(bounded.exceeded);
}
