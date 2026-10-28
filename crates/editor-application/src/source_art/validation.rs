use super::*;

pub(super) fn id(value: &str) -> Result<(), EditorError> {
    if value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        return Err(invalid(
            "Source-art identifiers must be nonempty bounded text",
        ));
    }
    Ok(())
}
pub(super) fn scope(value: &str) -> Result<(), EditorError> {
    if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(invalid("Source-art trusted scope must be bounded text"));
    }
    Ok(())
}
pub fn validate_cancel(request: &SourceArtCancel) -> Result<(), EditorError> {
    for value in [&request.project_id, &request.revision, &request.request_id] {
        id(value)?;
    }
    Ok(())
}
pub fn validate_request(request: &SourceArtRequest) -> Result<(), EditorError> {
    for value in [
        &request.project_id,
        &request.revision,
        &request.asset_id,
        &request.request_id,
    ] {
        id(value)?;
    }
    if let SourceArtQuery::Waveform {
        sample_rate,
        projection,
    } = &request.query
    {
        if !matches!(sample_rate, 44100 | 48000) {
            return Err(invalid(
                "Source waveform sample rate must be 44100 or 48000",
            ));
        }
        let bins = match projection {
            WaveformProjection::Source { range, bins } => {
                if let Some(range) = range {
                    positive_range(range)?;
                }
                *bins
            }
            WaveformProjection::Element {
                sequence_id,
                track_id,
                element_id,
                local_range,
                bins,
                bin_duration,
            } => {
                for value in [sequence_id, track_id, element_id] {
                    id(value)?;
                }
                positive_range(local_range)?;
                let step = RationalTime::try_from(bin_duration)?;
                if step.ticks() <= 0 {
                    return Err(invalid("Source-art bin duration must be positive"));
                }
                let duration = RationalTime::try_from(&local_range.duration)?;
                let numerator =
                    u128::from(duration.ticks().unsigned_abs()) * u128::from(step.timebase());
                let denominator =
                    u128::from(duration.timebase()) * u128::from(step.ticks().unsigned_abs());
                let expected = numerator.div_ceil(denominator);
                if expected != u128::from(*bins) {
                    return Err(invalid(
                        "Source-art bin count must cover the exact local window and step",
                    ));
                }
                *bins
            }
        };
        if !(1..=4096).contains(&bins) {
            return Err(invalid("Source-art bin count must be 1 through 4096"));
        }
    }
    Ok(())
}
fn positive_range(range: &opencut_contracts::TimeRange) -> Result<(), EditorError> {
    validate_range(range, true)?;
    if RationalTime::try_from(&range.start)?.ticks() < 0 {
        return Err(invalid(
            "Source-art window must begin at a nonnegative time",
        ));
    }
    Ok(())
}
pub fn validate_policy(policy: &SourceArtPolicy) -> Result<(), EditorError> {
    if policy.max_encoded_bytes == 0
        || policy.max_encoded_bytes > 32 * 1024 * 1024
        || policy.max_pcm_bytes == 0
        || policy.max_pcm_bytes > 256 * 1024 * 1024
        || policy.max_rss_bytes == 0
        || policy.max_rss_bytes > 2 * 1024 * 1024 * 1024
        || policy.deadline_ms == 0
        || policy.deadline_ms > 60000
    {
        return Err(invalid("Invalid bounded local source-art execution policy"));
    }
    Ok(())
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
pub(super) fn identity(
    prepared: &PreparedSourceArt,
    value: &SourceArtIdentity,
) -> Result<(), EditorError> {
    let expected = match prepared.request.query {
        SourceArtQuery::Waveform { .. } => SOURCE_PEAKS_ALGORITHM,
        SourceArtQuery::Thumbnail {} => {
            if prepared.thumbnail_time().is_some() {
                SOURCE_FRAME_ALGORITHM
            } else {
                THUMBNAIL_ALGORITHM
            }
        }
    };
    if value.algorithm() != expected {
        return Err(receipt(
            "Source-art renderer identity is incomplete or differs from its algorithm",
        ));
    }
    validate_runtime_identity(value)
}
/// Pure execution-proof validation, shared by live preparation and adapters.
/// Historical proofs keep their exact shape; live host assembly chooses only
/// the native variant and binds it to the observed configured worker.
pub fn validate_runtime_identity(value: &SourceArtIdentity) -> Result<(), EditorError> {
    if ![
        SOURCE_PEAKS_ALGORITHM,
        THUMBNAIL_ALGORITHM,
        SOURCE_FRAME_ALGORITHM,
    ]
    .contains(&value.algorithm())
    {
        return Err(receipt("Unknown source-art algorithm"));
    }
    match value {
        SourceArtIdentity::Browser(value) => {
            if value.decode_profile != SOURCE_ART_DECODE_PROFILE
                || id(&value.browser_version).is_err()
                || [
                    &value.browser_sha256,
                    &value.node_sha256,
                    &value.module_sha256,
                ]
                .iter()
                .any(|s| !hash(s))
            {
                return Err(receipt("Invalid historical source-art identity"));
            }
        }
        SourceArtIdentity::Native(value) => {
            if value.decode_profile != NATIVE_SOURCE_ART_DECODE_PROFILE
                || value.rasterizer != NATIVE_SOURCE_ART_RASTERIZER
                || !hash(&value.worker_sha256)
                || !hash(&value.module_sha256)
                || value.decoder_libraries.len() != 5
                || [
                    &value.platform.os,
                    &value.platform.architecture,
                    &value.platform.kernel_release,
                    &value.platform.system_build,
                ]
                .iter()
                .any(|s| id(s).is_err())
            {
                return Err(receipt("Invalid native source-art identity"));
            }
            let mut names = std::collections::BTreeSet::new();
            for library in &value.decoder_libraries {
                if !names.insert(library.name.as_str())
                    || !matches!(
                        library.name.as_str(),
                        "avcodec" | "avformat" | "avutil" | "swscale" | "swresample"
                    )
                    || id(&library.version).is_err()
                    || !hash(&library.configuration_sha256)
                    || !hash(&library.binary_sha256)
                {
                    return Err(receipt("Invalid native source-art decoder libraries"));
                }
            }
        }
    }
    Ok(())
}
pub(super) fn decoded(
    prepared: &PreparedSourceArt,
    decoded: &DecodedSourceArt,
) -> Result<(), EditorError> {
    identity(prepared, decoded.identity())?;
    if decoded.version() != &prepared.asset.version {
        return Err(receipt(
            "Source-art decoded resource version differs from its verified binding",
        ));
    }
    match (&prepared.request.query, decoded) {
        (
            SourceArtQuery::Waveform {
                sample_rate: requested,
                ..
            },
            DecodedSourceArt::Waveform {
                sample_rate,
                channels,
                total_samples,
                bucket_size,
                amplitudes,
                ..
            },
        ) => {
            if sample_rate != requested
                || !(1..=64).contains(channels)
                || *bucket_size != SOURCE_ART_BUCKET_SIZE
                || amplitudes.len() as u128 != u128::from(*total_samples).div_ceil(128).max(1)
                || amplitudes.iter().any(|v| !v.is_finite() || *v < 0.0)
                || (*total_samples == 0 && amplitudes[0] != 0.0)
            {
                return Err(receipt(
                    "Source waveform receipt differs from its complete decoded extent",
                ));
            }
            let summary = u64::try_from(amplitudes.len())
                .ok()
                .and_then(|n| n.checked_mul(4));
            if summary.is_none_or(|bytes| bytes > prepared.waveform_output_bytes().unwrap_or(0)) {
                return Err(budget(
                    "Streamed waveform summary exceeds its retained memory admission",
                ));
            }
        }
        (
            SourceArtQuery::Thumbnail {},
            DecodedSourceArt::ThumbnailReady {
                width,
                height,
                jpeg_bytes,
                observation,
                ..
            },
        ) => {
            let (source_width, source_height) = observation_check(prepared, observation, true)?;
            if (*width, *height) != thumbnail_size(source_width, source_height)? {
                return Err(receipt(
                    "Thumbnail dimensions differ from Classic's sequential fit",
                ));
            }
            if jpeg_bytes.len() > SOURCE_ART_MAX_JPEG_BYTES {
                return Err(budget("Source thumbnail exceeds its 1 MiB byte limit"));
            }
            if jpeg_dimensions(jpeg_bytes)? != (*width, *height) {
                return Err(receipt("Thumbnail JPEG bytes and dimension receipt differ"));
            }
            let total = u64::from(source_width)
                .checked_mul(u64::from(source_height))
                .and_then(|n| n.checked_mul(16))
                .and_then(|n| {
                    u64::from(*width)
                        .checked_mul(u64::from(*height))
                        .and_then(|p| p.checked_mul(4))
                        .and_then(|p| n.checked_add(p))
                })
                .and_then(|n| {
                    prepared
                        .encoded_resident_bytes()
                        .ok()
                        .and_then(|p| n.checked_add(p))
                })
                .and_then(|n| {
                    u64::try_from(jpeg_bytes.len())
                        .ok()
                        .and_then(|p| n.checked_add(p))
                });
            if total.is_none_or(|n| n > prepared.policy.max_pcm_bytes) {
                return Err(budget("Decoded thumbnail exceeds its memory admission"));
            }
        }
        (SourceArtQuery::Thumbnail {}, DecodedSourceArt::ThumbnailNoFrame { observation, .. }) => {
            observation_check(prepared, observation, false)?;
            if prepared.thumbnail_time().is_some() {
                return Err(EditorError::new(
                    "SOURCE_FRAME_NOT_FOUND",
                    "No decoded sample at the requested source point",
                ));
            }
        }
        _ => {
            return Err(receipt(
                "Source-art decoded kind differs from the fixed request",
            ));
        }
    }
    Ok(())
}
fn observation_check(
    prepared: &PreparedSourceArt,
    observation: &SourceArtThumbnailObservation,
    ready: bool,
) -> Result<(u32, u32), EditorError> {
    let dimensions = match (&prepared.asset.kind, observation) {
        (
            AssetKind::Image,
            SourceArtThumbnailObservation::StaticImage {
                source_width,
                source_height,
            },
        ) if ready => (*source_width, *source_height),
        (
            AssetKind::Video,
            SourceArtThumbnailObservation::Video {
                source_width,
                source_height,
                requested_seconds,
                actual_timestamp_seconds,
                frame_duration_seconds,
                ..
            },
        ) => {
            let expected = prepared
                .thumbnail_time()
                .map(RationalTime::try_from)
                .transpose()?
                .map_or(1.0, |time| time.ticks() as f64 / f64::from(time.timebase()));
            if *requested_seconds != expected
                || (ready
                    && (actual_timestamp_seconds.is_none_or(|n| !n.is_finite())
                        || frame_duration_seconds.is_none_or(|n| !n.is_finite() || n <= 0.0)))
                || (!ready
                    && (actual_timestamp_seconds.is_some() || frame_duration_seconds.is_some()))
            {
                return Err(receipt(
                    "Video thumbnail observation differs from getSample(1)",
                ));
            }
            (*source_width, *source_height)
        }
        _ => {
            return Err(receipt(
                "Thumbnail observation differs from the fixed asset or missing-frame status",
            ));
        }
    };
    if dimensions.0 == 0 || dimensions.1 == 0 {
        return Err(receipt(
            "Thumbnail actual display dimensions must be positive",
        ));
    }
    Ok(dimensions)
}
/// Classic thumbnailSize uses two sequential caps and positive Math.round.
pub fn thumbnail_size(width: u32, height: u32) -> Result<(u32, u32), EditorError> {
    if width == 0 || height == 0 {
        return Err(receipt("Invalid source thumbnail dimensions"));
    }
    let (mut w, mut h) = (u64::from(width), u64::from(height));
    let round = |n: u64, d: u64| (n + d / 2) / d;
    if w > 1280 {
        w = 1280;
        h = round(w * u64::from(height), u64::from(width));
    }
    if h > 720 {
        h = 720;
        w = round(h * u64::from(width), u64::from(height));
    }
    if w == 0 || h == 0 {
        return Err(receipt("Classic thumbnail fit has a zero dimension"));
    }
    Ok((w as u32, h as u32))
}
// Structural byte-envelope/dimension validation. Actual JPEG decoding and
// encoding are the trusted native renderer's responsibility, not a pure port.
fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32), EditorError> {
    if !bytes.starts_with(&[0xff, 0xd8]) || !bytes.ends_with(&[0xff, 0xd9]) {
        return Err(receipt("Thumbnail is not a complete JPEG byte stream"));
    }
    let mut cursor = 2usize;
    while cursor < bytes.len() {
        if bytes[cursor] != 0xff {
            return Err(receipt("Malformed thumbnail JPEG marker"));
        }
        while bytes.get(cursor) == Some(&0xff) {
            cursor += 1;
        }
        let marker = *bytes
            .get(cursor)
            .ok_or_else(|| receipt("Truncated thumbnail JPEG"))?;
        cursor += 1;
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        let size = bytes
            .get(cursor..cursor + 2)
            .ok_or_else(|| receipt("Truncated thumbnail JPEG segment"))?;
        let size = usize::from(u16::from_be_bytes([size[0], size[1]]));
        let end = cursor
            .checked_add(size)
            .filter(|n| *n <= bytes.len())
            .ok_or_else(|| receipt("Truncated thumbnail JPEG segment"))?;
        if size < 2 {
            return Err(receipt("Invalid thumbnail JPEG segment size"));
        }
        if matches!(marker, 0xc0..=0xc2) {
            let header = bytes
                .get(cursor + 2..end)
                .ok_or_else(|| receipt("Invalid thumbnail JPEG frame"))?;
            if header.len() < 6 || header[0] != 8 {
                return Err(receipt("Invalid thumbnail JPEG precision"));
            }
            let h = u32::from(u16::from_be_bytes([header[1], header[2]]));
            let w = u32::from(u16::from_be_bytes([header[3], header[4]]));
            if w == 0 || h == 0 {
                return Err(receipt("Invalid thumbnail JPEG dimensions"));
            }
            return Ok((w, h));
        }
        cursor = end;
    }
    Err(receipt("Thumbnail JPEG has no supported frame dimensions"))
}
