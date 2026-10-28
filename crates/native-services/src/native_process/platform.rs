use super::*;
use opencut_contracts::renderer_identity::NativeSystemIdentity;
#[cfg(unix)]
pub(crate) fn system_identity() -> Result<NativeSystemIdentity, EditorError> {
    let mut info = std::mem::MaybeUninit::<libc::utsname>::zeroed();
    // SAFETY: uname writes exactly one initialized utsname record.
    if unsafe { libc::uname(info.as_mut_ptr()) } != 0 {
        return Err(invalid("Cannot identify native operating system"));
    }
    let info = unsafe { info.assume_init() };
    fn field(bytes: &[libc::c_char]) -> Result<String, EditorError> {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("Unterminated native platform identity"))?;
        String::from_utf8(bytes[..end].iter().map(|v| *v as u8).collect())
            .map_err(|_| invalid("Invalid native platform identity"))
    }
    #[cfg(target_os = "macos")]
    let system_build = {
        let mut bytes = [0u8; 256];
        let mut size = bytes.len();
        // The macOS build pins the CoreText and Metal system components,
        // including dylibs supplied by the shared system cache.
        let status = unsafe {
            libc::sysctlbyname(
                c"kern.osversion".as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 || size == 0 || size > bytes.len() || bytes[size - 1] != 0 {
            return Err(invalid("Cannot identify native system build"));
        }
        String::from_utf8(bytes[..size - 1].to_vec())
            .map_err(|_| invalid("Invalid native system build"))?
    };
    #[cfg(not(target_os = "macos"))]
    let system_build = field(&info.version)?;
    Ok(NativeSystemIdentity {
        os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        kernel_release: field(&info.release)?,
        system_build,
    })
}
#[cfg(not(unix))]
pub(crate) fn system_identity() -> Result<NativeSystemIdentity, EditorError> {
    Err(invalid("Native platform identity is unavailable"))
}
