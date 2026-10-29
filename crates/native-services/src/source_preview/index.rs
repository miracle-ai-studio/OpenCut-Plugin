use super::{INDEX_MAGIC, invalid};
use opencut_contracts::{EditorError, source_preview::*};
use opencut_editor_application::source_preview::{
    INDEX_HEADER_BYTES, INDEX_RECORD_BYTES, MAX_SOURCE_FRAMES, SourcePreviewFrameIndexRead,
    SourcePreviewIndexRecord, validate_frame_video, validate_index_record,
};
use std::io::{Read, Seek, SeekFrom};

fn count_limit(count: u64) -> Result<u64, EditorError> {
    if count == 0 || count > MAX_SOURCE_FRAMES {
        return Err(invalid("Invalid source frame index admission"));
    }
    Ok(INDEX_HEADER_BYTES + count * INDEX_RECORD_BYTES)
}
fn encode_record(record: &SourcePreviewIndexRecord) -> [u8; 64] {
    let mut bytes = [0; 64];
    for (slot, value) in [
        record.source_pts,
        record.source_duration,
        record.proxy_pts,
        record.proxy_duration,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[slot * 8..slot * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }
    let video = &record.source_video;
    for (slot, value) in [
        video.source_width,
        video.source_height,
        video.raster_width,
        video.raster_height,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[32 + slot * 4..36 + slot * 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[48..50].copy_from_slice(&video.rotation_degrees.to_le_bytes());
    bytes[50] = match video.matrix {
        SourcePreviewMatrix::Bt601 => 0,
        SourcePreviewMatrix::Bt709 => 1,
        SourcePreviewMatrix::Bt2020 => 2,
        SourcePreviewMatrix::Rgb => 3,
        SourcePreviewMatrix::Unspecified => 4,
    };
    bytes[51] = u8::from(video.full_range) | (u8::from(video.has_alpha) << 1);
    bytes[52..56].copy_from_slice(&video.pixel_aspect_numerator.to_le_bytes());
    bytes[56..60].copy_from_slice(&video.pixel_aspect_denominator.to_le_bytes());
    bytes
}
fn decode_record(bytes: [u8; 64]) -> Result<SourcePreviewIndexRecord, EditorError> {
    if bytes[60..64] != [0; 4] || bytes[51] & !3 != 0 {
        return Err(invalid("Unknown source frame index fields"));
    }
    let i64_at = |at| i64::from_le_bytes(bytes[at..at + 8].try_into().expect("fixed record"));
    let u32_at = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("fixed record"));
    let matrix = match bytes[50] {
        0 => SourcePreviewMatrix::Bt601,
        1 => SourcePreviewMatrix::Bt709,
        2 => SourcePreviewMatrix::Bt2020,
        3 => SourcePreviewMatrix::Rgb,
        4 => SourcePreviewMatrix::Unspecified,
        _ => return Err(invalid("Unknown original source color matrix")),
    };
    Ok(SourcePreviewIndexRecord {
        source_pts: i64_at(0),
        source_duration: i64_at(8),
        proxy_pts: i64_at(16),
        proxy_duration: i64_at(24),
        source_video: SourcePreviewFrameVideo {
            source_width: u32_at(32),
            source_height: u32_at(36),
            raster_width: u32_at(40),
            raster_height: u32_at(44),
            rotation_degrees: u16::from_le_bytes(bytes[48..50].try_into().expect("fixed record")),
            pixel_aspect_numerator: i32::from_le_bytes(
                bytes[52..56].try_into().expect("fixed record"),
            ),
            pixel_aspect_denominator: i32::from_le_bytes(
                bytes[56..60].try_into().expect("fixed record"),
            ),
            matrix,
            full_range: bytes[51] & 1 != 0,
            has_alpha: bytes[51] & 2 != 0,
        },
    })
}

/// The owning host verifies the artifact hash/lease before constructing this
/// reader and fences the bounded metadata lookup before exposing proxy bytes.
/// Frame lookup retains only one fixed record, independent of source duration.
pub struct SourcePreviewIndex<R> {
    input: R,
    count: u64,
    raster: SourcePreviewRaster,
}
impl<R: Read + Seek> SourcePreviewIndex<R> {
    pub fn new(mut input: R, count: u64, raster: SourcePreviewRaster) -> Result<Self, EditorError> {
        let expected = count_limit(count)?;
        if input
            .seek(SeekFrom::End(0))
            .map_err(|_| invalid("Cannot size source frame index"))?
            != expected
        {
            return Err(invalid("Source frame index byte length changed"));
        }
        input
            .seek(SeekFrom::Start(0))
            .map_err(|_| invalid("Cannot seek source frame index"))?;
        let mut header = [0; 16];
        input
            .read_exact(&mut header)
            .map_err(|_| invalid("Truncated source frame index"))?;
        if header[..8] != INDEX_MAGIC
            || u64::from_le_bytes(header[8..16].try_into().expect("fixed header")) != count
        {
            return Err(invalid("Source frame index format/count changed"));
        }
        Ok(Self {
            input,
            count,
            raster,
        })
    }
}
impl<R: Read + Seek> SourcePreviewFrameIndexRead for SourcePreviewIndex<R> {
    fn frame_count(&self) -> u64 {
        self.count
    }
    fn frame(&mut self, ordinal: u64) -> Result<SourcePreviewIndexRecord, EditorError> {
        if ordinal >= self.count {
            return Err(invalid("Source frame ordinal exceeds index"));
        }
        self.input
            .seek(SeekFrom::Start(
                INDEX_HEADER_BYTES + ordinal * INDEX_RECORD_BYTES,
            ))
            .map_err(|_| invalid("Cannot seek source frame record"))?;
        let mut bytes = [0; 64];
        self.input
            .read_exact(&mut bytes)
            .map_err(|_| invalid("Truncated source frame record"))?;
        let record = decode_record(bytes)?;
        validate_index_record(&record, self.raster)?;
        Ok(record)
    }
}

/// One bounded metadata allocation. Native source facts are appended in actual
/// presentation order; independently observed encoder timestamps fill only the
/// proxy slots. Publication is impossible before both streams agree in count.
pub struct SourcePreviewIndexBuilder {
    bytes: Vec<u8>,
    max_frames: u64,
    source_count: u64,
    proxy_count: u64,
    last_source: Option<i64>,
    last_proxy: Option<i64>,
    raster: SourcePreviewRaster,
}
impl SourcePreviewIndexBuilder {
    pub fn new(max_frames: u64, raster: SourcePreviewRaster) -> Result<Self, EditorError> {
        let size = usize::try_from(count_limit(max_frames)?)
            .map_err(|_| invalid("Source frame index allocation overflow"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| invalid("Cannot allocate admitted source index"))?;
        bytes.extend_from_slice(&INDEX_MAGIC);
        bytes.extend_from_slice(&0u64.to_le_bytes());
        Ok(Self {
            bytes,
            max_frames,
            source_count: 0,
            proxy_count: 0,
            last_source: None,
            last_proxy: None,
            raster,
        })
    }
    pub fn source_frame(
        &mut self,
        pts: i64,
        duration: i64,
        video: SourcePreviewFrameVideo,
    ) -> Result<(), EditorError> {
        if self.source_count >= self.max_frames
            || self.proxy_count != 0
            || duration <= 0
            || self.last_source.is_some_and(|last| pts <= last)
        {
            return Err(invalid(
                "Source frame index changed its admitted presentation order",
            ));
        }
        validate_frame_video(&video, self.raster)?;
        self.bytes
            .extend_from_slice(&encode_record(&SourcePreviewIndexRecord {
                source_pts: pts,
                source_duration: duration,
                proxy_pts: 0,
                proxy_duration: 0,
                source_video: video,
            }));
        self.last_source = Some(pts);
        self.source_count += 1;
        Ok(())
    }
    pub fn proxy_frame(
        &mut self,
        ordinal: u64,
        pts: i64,
        duration: i64,
    ) -> Result<(), EditorError> {
        if ordinal != self.proxy_count
            || ordinal >= self.source_count
            || pts < 0
            || duration <= 0
            || self.last_proxy.is_some_and(|last| pts <= last)
        {
            return Err(invalid(
                "Encoded frame index changed actual frame order/count",
            ));
        }
        let start = INDEX_HEADER_BYTES as usize + ordinal as usize * INDEX_RECORD_BYTES as usize;
        self.bytes[start + 16..start + 24].copy_from_slice(&pts.to_le_bytes());
        self.bytes[start + 24..start + 32].copy_from_slice(&duration.to_le_bytes());
        self.last_proxy = Some(pts);
        self.proxy_count += 1;
        Ok(())
    }
    pub fn frame_count(&self) -> u64 {
        self.source_count
    }
    pub fn finish(mut self) -> Result<Vec<u8>, EditorError> {
        if self.source_count == 0 || self.source_count != self.proxy_count {
            return Err(invalid("Source/proxy frame index is incomplete"));
        }
        self.bytes[8..16].copy_from_slice(&self.source_count.to_le_bytes());
        Ok(self.bytes)
    }
}
