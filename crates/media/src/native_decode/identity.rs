use super::*;
use ffmpeg_next::{self as av, ffi};
use std::path::PathBuf;

/// Private native paths never enter a public renderer receipt.
pub struct DecoderLibrary {
    pub name: &'static str,
    pub version: String,
    pub configuration: &'static str,
    pub path: PathBuf,
}

#[cfg(unix)]
pub fn libraries() -> MediaResult<Vec<DecoderLibrary>> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};
    let definitions = [
        (
            "avcodec",
            av::codec::version(),
            av::codec::configuration(),
            ffi::avcodec_version as *const libc::c_void,
        ),
        (
            "avformat",
            av::format::version(),
            av::format::configuration(),
            ffi::avformat_version as *const libc::c_void,
        ),
        (
            "avutil",
            av::util::version(),
            av::util::configuration(),
            ffi::avutil_version as *const libc::c_void,
        ),
        (
            "swresample",
            av::software::resampling::version(),
            av::software::resampling::configuration(),
            ffi::swresample_version as *const libc::c_void,
        ),
        (
            "swscale",
            av::software::scaling::version(),
            av::software::scaling::configuration(),
            ffi::swscale_version as *const libc::c_void,
        ),
    ];
    definitions
        .into_iter()
        .map(|(name, version, configuration, address)| {
            // SAFETY: each address is a linked library function. dladdr writes an
            // initialized record; its filename belongs to the loaded image.
            let path = unsafe {
                let mut info = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
                if libc::dladdr(address, info.as_mut_ptr()) == 0 {
                    return Err(invalid("Cannot identify loaded decoder library"));
                }
                let info = info.assume_init();
                if info.dli_fname.is_null() {
                    return Err(invalid("Decoder library has no observed filename"));
                }
                PathBuf::from(std::ffi::OsStr::from_bytes(
                    CStr::from_ptr(info.dli_fname).to_bytes(),
                ))
            }
            .canonicalize()
            .map_err(|_| invalid("Cannot resolve loaded decoder library"))?;
            Ok(DecoderLibrary {
                name,
                version: format!(
                    "{}.{}.{}",
                    version >> 16,
                    (version >> 8) & 255,
                    version & 255
                ),
                configuration,
                path,
            })
        })
        .collect()
}

#[cfg(not(unix))]
pub fn libraries() -> MediaResult<Vec<DecoderLibrary>> {
    Err(invalid(
        "Loaded decoder identity is unavailable on this platform",
    ))
}
