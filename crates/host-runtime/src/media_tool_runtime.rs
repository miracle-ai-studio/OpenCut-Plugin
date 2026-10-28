//! Trusted media-tool assembly shared by capability, import, jobs and inspection.
//! Explicit configuration wins. A declared damaged bundle never falls back to
//! PATH; absence permits the existing external-tool policy. No global env writes.
use opencut_contracts::EditorError;
use opencut_media::FfmpegBackend;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
enum Tool {
    Ffmpeg,
    Ffprobe,
}
impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
        }
    }
    fn variable(self) -> &'static str {
        match self {
            Self::Ffmpeg => "OPENCUT_FFMPEG",
            Self::Ffprobe => "OPENCUT_FFPROBE",
        }
    }
}
fn resolve_at(exe: &Path, tool: Tool, explicit: Option<OsString>) -> Result<PathBuf, EditorError> {
    use opencut_native_services::classic_video::resolve_tool;
    if let Some(value) = explicit {
        return resolve_tool(value.into());
    }
    if let Some(path) = crate::bundled_runtime::executable_at(exe, tool.name())? {
        return resolve_tool(path);
    }
    resolve_tool(tool.name().into())
}
fn configured(tool: Tool) -> Result<PathBuf, EditorError> {
    let exe = std::env::current_exe()
        .map_err(|_| EditorError::new("CAPABILITY_UNAVAILABLE", "Cannot identify native host"))?;
    resolve_at(&exe, tool, std::env::var_os(tool.variable()))
}
pub(crate) fn ffmpeg() -> Result<PathBuf, EditorError> {
    configured(Tool::Ffmpeg)
}
pub(crate) fn ffprobe() -> Result<PathBuf, EditorError> {
    configured(Tool::Ffprobe)
}
pub(crate) fn backend() -> Result<FfmpegBackend, EditorError> {
    FfmpegBackend::new(ffmpeg()?, ffprobe()?).map_err(|e| EditorError::new(e.code(), e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    fn executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, b"synthetic executable; resolution never executes it").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn explicit_configuration_wins_and_invalid_explicit_is_never_replaced() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(bin.join("libexec")).unwrap();
        fs::write(bin.join("libexec/runtime-bundle.json"), b"damaged").unwrap();
        let configured = root.path().join("configured");
        executable(&configured);
        assert_eq!(
            resolve_at(
                &bin.join("opencut"),
                Tool::Ffmpeg,
                Some(configured.as_os_str().to_owned())
            )
            .unwrap(),
            fs::canonicalize(&configured).unwrap()
        );
        assert!(
            resolve_at(
                &bin.join("opencut"),
                Tool::Ffmpeg,
                Some(root.path().join("missing").into_os_string())
            )
            .is_err()
        );
        assert!(resolve_at(&bin.join("opencut"), Tool::Ffprobe, Some(OsString::new())).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn relocated_media_bundle_is_selected_and_source_tampering_is_rejected() {
        use serde_json::json;
        use sha2::{Digest, Sha256};
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("relocated path/bin/libexec");
        let mut files = serde_json::Map::new();
        let mut components = serde_json::Map::new();
        let mut scripts = serde_json::Map::new();
        let mut put = |name: &str, execute: bool| {
            let path = directory.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if execute {
                executable(&path);
            } else {
                fs::write(&path, b"fixed fixture").unwrap();
            }
            let bytes = fs::read(&path).unwrap();
            files.insert(name.to_owned(), json!({"kind":"file","size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"executable":execute}));
        };
        for name in [
            "run_derived.py",
            "run_derived_v2.py",
            "run_color_static_v1.py",
        ] {
            let path = format!("font-catalog/{name}");
            put(&path, false);
            scripts.insert(name.to_owned(), json!(path));
        }
        for tool in ["ffmpeg", "ffprobe"] {
            let prefix = format!("runtimes/{tool}");
            for (suffix, exec) in [
                ("bin/tool", true),
                ("COPYING", false),
                ("source/archive.tar.xz", false),
                ("source/build.json", false),
            ] {
                put(&format!("{prefix}/{suffix}"), exec);
            }
            components.insert(tool.into(),json!({"entry":format!("{prefix}/bin/tool"),"version":"8.1.3","license_spdx":"GPL-2.0-or-later","licenses":[format!("{prefix}/COPYING")],"corresponding_sources":[format!("{prefix}/source/archive.tar.xz"),format!("{prefix}/source/build.json")]}));
        }
        let manifest = json!({"format":"opencut.native-runtime-bundle","schema_version":1,"target":"fixture","delivery":"bundled_unverified","components":components,"scripts":scripts,"external_dependencies":["node","python"],"files":files});
        fs::write(
            directory.join("runtime-bundle.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let exe = directory.parent().unwrap().join("opencut");
        for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
            assert_eq!(
                resolve_at(&exe, tool, None).unwrap(),
                fs::canonicalize(directory.join(format!("runtimes/{}/bin/tool", tool.name())))
                    .unwrap()
            );
        }
        fs::write(
            directory.join("runtimes/ffprobe/source/archive.tar.xz"),
            b"changed source",
        )
        .unwrap();
        let error = resolve_at(&exe, Tool::Ffmpeg, None).unwrap_err();
        assert_eq!(error.message, "BUNDLED_RUNTIME_INVALID");
    }
    #[test]
    fn damaged_manifest_propagates_the_bundle_error_instead_of_trying_path() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(bin.join("libexec")).unwrap();
        fs::write(bin.join("libexec/runtime-bundle.json"), b"{}").unwrap();
        for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
            let error = resolve_at(&bin.join("opencut"), tool, None).unwrap_err();
            assert_eq!(error.code, "CAPABILITY_UNAVAILABLE");
            assert_eq!(error.message, "BUNDLED_RUNTIME_INVALID");
        }
    }
}
