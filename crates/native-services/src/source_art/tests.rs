use super::*;
use serde_json::json;
fn identity() -> SourceArtIdentity {
    SourceArtIdentity::Browser(BrowserSourceArtIdentity {
        algorithm: SOURCE_PEAKS_ALGORITHM.into(),
        decode_profile: SOURCE_ART_DECODE_PROFILE.into(),
        browser_version: "142.0.0.0".into(),
        browser_sha256: "a".repeat(64),
        node_sha256: "b".repeat(64),
        module_sha256: "c".repeat(64),
    })
}
#[test]
fn typed_peaks_keep_actual_extent_and_float_bits() {
    let mut bytes = Vec::new();
    for value in [0.0f32, 1.25, 0.125] {
        bytes.extend(value.to_le_bytes());
    }
    let value = json!({"kind":"waveform","identity":identity(),"sample_rate":48000,"channels":4,"total_samples":"257","bucket_size":128,"output_bytes":12});
    let DecodedSourceArt::Waveform {
        total_samples,
        channels,
        amplitudes,
        ..
    } = process::decode_for_test(value, bytes).unwrap()
    else {
        panic!()
    };
    assert_eq!(total_samples, 257);
    assert_eq!(channels, 4);
    assert_eq!(amplitudes, vec![0.0, 1.25, 0.125]);
}
#[test]
fn no_sample_at_one_second_is_not_a_fallback_frame() {
    let mut id = identity();
    id.set_algorithm(THUMBNAIL_ALGORITHM);
    let value = json!({"kind":"thumbnail_no_frame","identity":id,"output_bytes":0,"reason":"no_sample_at_one_second","observation":{"kind":"video","track_id":0,"source_width":16,"source_height":9,"requested_seconds":1.0,"actual_timestamp_seconds":null,"frame_duration_seconds":null}});
    assert!(matches!(
        process::decode_for_test(value.clone(), vec![]).unwrap(),
        DecodedSourceArt::ThumbnailNoFrame { .. }
    ));
    assert!(process::decode_for_test(value, vec![0]).is_err());
}
#[test]
fn completion_requires_exact_bytes_and_canonical_clock() {
    let value = json!({"kind":"waveform","identity":identity(),"sample_rate":44100,"channels":1,"total_samples":"01","bucket_size":128,"output_bytes":4});
    assert!(process::decode_for_test(value.clone(), vec![0; 4]).is_err());
    assert!(process::decode_for_test(value, vec![0; 3]).is_err());
    let mut id = identity();
    let SourceArtIdentity::Browser(value) = &mut id else {
        panic!()
    };
    value.node_sha256 = "A".repeat(64);
    assert!(validate_identity(&id).is_err());
}
#[path = "tests/real.rs"]
mod real;
