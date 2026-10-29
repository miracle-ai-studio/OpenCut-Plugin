use super::*;
use opencut_editor_application::source_preview::{INDEX_HEADER_BYTES, SourcePreviewFrameIndexRead};
use std::io::Cursor;
fn video(rotation: u16) -> SourcePreviewFrameVideo {
    SourcePreviewFrameVideo {
        source_width: 2,
        source_height: 3,
        raster_width: 2,
        raster_height: 3,
        rotation_degrees: rotation,
        pixel_aspect_numerator: 4,
        pixel_aspect_denominator: 3,
        matrix: SourcePreviewMatrix::Bt709,
        full_range: false,
        has_alpha: true,
    }
}
#[test]
fn normalized_rotation_and_padding_preserve_all_rgba_including_transparent_colour() {
    let input: Vec<u8> = (0..6u8)
        .flat_map(|n| [n, 40 + n, 80 + n, if n % 2 == 0 { 0 } else { 255 }])
        .collect();
    let raster = SourcePreviewRaster {
        width: 4,
        height: 4,
    };
    let expected = [
        (0, vec![0, 1, 2, 3, 4, 5]),
        (90, vec![4, 2, 0, 5, 3, 1]),
        (180, vec![5, 4, 3, 2, 1, 0]),
        (270, vec![1, 3, 5, 0, 2, 4]),
    ];
    for (rotation, order) in expected {
        let metadata = video(rotation);
        let mut normalized = Vec::new();
        raster::normalize(&input, &metadata, raster, &mut normalized, &mut || Ok(())).unwrap();
        let (width, height) = if rotation % 180 == 0 {
            (2usize, 3usize)
        } else {
            (3, 2)
        };
        for y in 0..4 {
            for x in 0..4 {
                let pixel = &normalized[(y * 4 + x) * 4..(y * 4 + x + 1) * 4];
                if x < width && y < height {
                    let n = order[y * width + x] as usize;
                    assert_eq!(pixel, &input[n * 4..n * 4 + 4]);
                } else {
                    assert_eq!(pixel, &[0, 0, 0, 0]);
                }
            }
        }
        assert_eq!(
            raster::restore(&normalized, &metadata, raster, &mut || Ok(())).unwrap(),
            input
        );
    }
    let mut calls = 0;
    assert!(
        raster::normalize(&input, &video(0), raster, &mut Vec::new(), &mut || {
            calls += 1;
            Err(EditorError::new("NOT_AUTHORIZED", "Revoked"))
        })
        .is_err()
    );
    assert_eq!(calls, 1);
}
#[test]
fn exact_binary_index_rejects_incomplete_or_reordered_encoder_observations() {
    let raster = SourcePreviewRaster {
        width: 4,
        height: 4,
    };
    let mut builder = SourcePreviewIndexBuilder::new(3, raster).unwrap();
    builder.source_frame(-80, 40, video(0)).unwrap();
    builder.source_frame(0, 80, video(90)).unwrap();
    builder.source_frame(200, 50, video(180)).unwrap();
    assert!(builder.proxy_frame(1, 33, 33).is_err());
    builder.proxy_frame(0, 0, 33).unwrap();
    builder.proxy_frame(1, 33, 34).unwrap();
    builder.proxy_frame(2, 67, 33).unwrap();
    let bytes = builder.finish().unwrap();
    assert_eq!(bytes.len(), 208);
    assert_eq!(&bytes[..8], b"OCSPIDX\0");
    assert_eq!(&bytes[8..16], &3u64.to_le_bytes());
    assert_eq!(&bytes[16..24], &(-80i64).to_le_bytes());
    assert_eq!(&bytes[32..40], &0i64.to_le_bytes());
    let mut index = SourcePreviewIndex::new(Cursor::new(bytes.clone()), 3, raster).unwrap();
    let record = index.frame(1).unwrap();
    assert_eq!(
        (
            record.source_pts,
            record.source_duration,
            record.proxy_pts,
            record.proxy_duration
        ),
        (0, 80, 33, 34)
    );
    assert_eq!(record.source_video, video(90));
    assert!(index.frame(3).is_err());
    let mut corrupt = bytes.clone();
    corrupt[INDEX_HEADER_BYTES as usize + 60] = 1;
    assert!(
        SourcePreviewIndex::new(Cursor::new(corrupt), 3, raster)
            .unwrap()
            .frame(0)
            .is_err()
    );
    assert!(SourcePreviewIndex::new(Cursor::new(&bytes[..207]), 3, raster).is_err());
    let mut unfinished = SourcePreviewIndexBuilder::new(3, raster).unwrap();
    unfinished.source_frame(0, 1, video(0)).unwrap();
    assert!(unfinished.finish().is_err());
}
