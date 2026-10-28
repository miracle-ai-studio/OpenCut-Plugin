//! Trusted host installation discovery; no child processes or client paths.
#[cfg(test)]
use crate::browser_runtime::METADATA_BYTES;
use crate::browser_runtime::{
    Unavailable, absolute, dependency, executable, metadata, package_file,
};
use opencut_contracts::EditorError;
use opencut_job_contracts::scene_audio::AudioWavCapability;
use opencut_job_execution::scene_audio::LocalAudioExecutionPolicy;
use opencut_native_services::native_audio::NativeAudioProcessConfig;
#[cfg(test)]
use std::fs;
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

const HOST_MODULES: [&str; 6] = ["runner", "protocol", "browser", "page", "modules", "limits"];
const RENDERER_MODULES: [&str; 8] = [
    "index",
    "decoder",
    "iso-edit-list",
    "preparation",
    "mixing",
    "mastering",
    "runtime",
    "validation",
];

struct HostConfiguration {
    package: Option<OsString>,
    chromium: Option<OsString>,
    node: Option<OsString>,
    search_path: Option<OsString>,
    current_exe: Option<PathBuf>,
}
impl HostConfiguration {
    fn capture() -> Self {
        Self {
            package: env::var_os("OPENCUT_CLASSIC_AUDIO_PACKAGE"),
            chromium: env::var_os("OPENCUT_CLASSIC_AUDIO_CHROMIUM"),
            node: env::var_os("OPENCUT_CLASSIC_AUDIO_NODE"),
            search_path: env::var_os("PATH"),
            current_exe: env::current_exe().ok(),
        }
    }
}
struct RuntimePaths {
    node: PathBuf,
    chromium: PathBuf,
    runner: PathBuf,
}
impl Unavailable {
    fn reason(&self) -> &str {
        match self {
            Self::NotConfigured => "AUDIO_RUNTIME_NOT_CONFIGURED",
            Self::Unavailable => "AUDIO_RUNTIME_UNAVAILABLE",
        }
    }
}

pub(crate) fn configured(
    policy: &LocalAudioExecutionPolicy,
) -> Result<NativeAudioProcessConfig, EditorError> {
    opencut_editor_application::scene_audio_jobs::validate_policy(policy)?;
    Ok(NativeAudioProcessConfig {
        worker: crate::native_worker_runtime::worker()?,
        policy: policy.clone(),
        strict_cgroup: None,
    })
}

pub(crate) fn capability() -> AudioWavCapability {
    let runtime = crate::native_worker_runtime::worker();
    AudioWavCapability {
        schema_version: 1,
        available: runtime.is_ok(),
        submit_tool: "opencut_job_audio_wav".into(),
        sample_rates: vec![44100, 48000],
        unavailable_reason: runtime
            .err()
            .map(|_| "NATIVE_AUDIO_RUNTIME_UNAVAILABLE".into()),
    }
}

pub(crate) fn preview_config()
-> Result<opencut_native_services::native_audio::NativeAudioPreviewProcessConfig, EditorError> {
    Ok(
        opencut_native_services::native_audio::NativeAudioPreviewProcessConfig {
            worker: crate::native_worker_runtime::worker()?,
            preparation_deadline: Duration::from_secs(120),
            operation_deadline: Duration::from_secs(30),
            max_pcm_bytes: 256 * 1024 * 1024,
            max_encoded_bytes: 32 * 1024 * 1024,
            max_rss_bytes: 2 * 1024 * 1024 * 1024,
            strict_cgroup: None,
        },
    )
}

pub(crate) fn preview_capability() -> opencut_api_contracts::audio_preview::AudioPreviewCapability {
    use opencut_api_contracts::audio_preview::*;
    let resolved = preview_config();
    AudioPreviewCapability {
        available: resolved.is_ok(),
        unavailable_reason: resolved.err().map(|e| e.code),
        profile: AudioPreviewProfile::ClassicRealtimeV31,
        pcm_stage: AudioPreviewPcmStage::Mastered,
        sample_rates: vec![44100, 48000],
        stream_path: "/api/preview/audio/stream".into(),
        max_sessions_per_database: 4,
        max_block_frames: 4096,
        max_buffered_frames:
            opencut_editor_application::audio_preview::AUDIO_PREVIEW_MAX_BUFFERED_FRAMES as u32,
        owner_lease_ms: 30_000,
        authorization_heartbeat_ms: 1000,
        session_ttl_ms: 3_600_000,
    }
}
pub(crate) fn range_capability() -> opencut_job_contracts::scene_audio::AudioRangeWavCapability {
    let whole = capability();
    opencut_job_contracts::scene_audio::AudioRangeWavCapability {
        schema_version: 1,
        available: whole.available,
        submit_tool: "opencut_job_audio_range_wav".into(),
        sample_rates: whole.sample_rates,
        unavailable_reason: whole.unavailable_reason,
    }
}
#[cfg(test)]
fn capability_for(configuration: &HostConfiguration) -> AudioWavCapability {
    let result = resolve(configuration);
    AudioWavCapability {
        schema_version: 1,
        available: result.is_ok(),
        submit_tool: "opencut_job_audio_wav".into(),
        sample_rates: vec![44_100, 48_000],
        unavailable_reason: result.err().map(|reason| reason.reason().into()),
    }
}

// Node-only and browser executions share installation discovery, while the
// packet observation deliberately does not require Chromium or audio DSP files.
fn resolve_package_node(
    configuration: &HostConfiguration,
) -> Result<(PathBuf, PathBuf), Unavailable> {
    if !cfg!(unix) {
        return Err(Unavailable::Unavailable);
    }
    let package = match &configuration.package {
        Some(path) => PathBuf::from(path),
        None => configuration
            .current_exe
            .as_deref()
            .and_then(Path::parent)
            .map(|parent| parent.join("libexec/classic-audio-renderer"))
            .filter(|path| path.is_dir())
            .ok_or(Unavailable::NotConfigured)?,
    };
    let package = absolute(&package)?;
    if !package.is_dir() {
        return Err(Unavailable::Unavailable);
    }
    let node = match &configuration.node {
        Some(path) => executable(Path::new(path))?,
        None => match crate::bundled_runtime::executable("node")
            .map_err(|_| Unavailable::Unavailable)?
        {
            Some(path) => executable(&path)?,
            None => configuration
                .search_path
                .as_deref()
                .into_iter()
                .flat_map(env::split_paths)
                .filter(|directory| directory.is_absolute())
                .find_map(|directory| executable(&directory.join("node")).ok())
                .ok_or(Unavailable::Unavailable)?,
        },
    };
    let manifest = metadata(&package.join("package.json"))?;
    if manifest["name"] != "@opencut/classic-audio-renderer"
        || manifest["version"] != "0.1.0"
        || manifest["type"] != "module"
        || manifest["dependencies"]["playwright-core"] != "1.63.0"
        || manifest["dependencies"]["mediabunny"] != "1.41.0"
    {
        return Err(Unavailable::Unavailable);
    }
    Ok((package, node))
}

fn resolve(configuration: &HostConfiguration) -> Result<RuntimePaths, Unavailable> {
    let chromium = match &configuration.chromium {
        Some(path) => PathBuf::from(path),
        None => crate::bundled_runtime::executable("chromium")
            .map_err(|_| Unavailable::Unavailable)?
            .ok_or(Unavailable::NotConfigured)?,
    };
    let (package, node) = resolve_package_node(configuration)?;
    let chromium = executable(&chromium)?;
    for module in HOST_MODULES {
        package_file(&package, &format!("host/{module}.mjs"))?;
    }
    for module in RENDERER_MODULES {
        package_file(&package, &format!("src/{module}.mjs"))?;
    }
    package_file(&package, "vendor/soundtouchjs-0.3.0/soundtouch.js")?;
    dependency(&package, "playwright-core", "1.63.0", "index.mjs")?;
    dependency(
        &package,
        "mediabunny",
        "1.41.0",
        "dist/bundles/mediabunny.mjs",
    )?;
    Ok(RuntimePaths {
        node,
        chromium,
        runner: package_file(&package, "host/runner.mjs")?,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn put(root: &Path, relative: &str, contents: &[u8]) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }
    fn installation(root: &Path) -> HostConfiguration {
        let package = root.join("package");
        put(&package, "package.json", br#"{"name":"@opencut/classic-audio-renderer","version":"0.1.0","type":"module","dependencies":{"playwright-core":"1.63.0","mediabunny":"1.41.0"}}"#);
        for module in HOST_MODULES {
            put(&package, &format!("host/{module}.mjs"), b"export {};");
        }
        for module in RENDERER_MODULES {
            put(&package, &format!("src/{module}.mjs"), b"export {};");
        }
        put(
            &package,
            "vendor/soundtouchjs-0.3.0/soundtouch.js",
            b"export {};",
        );
        for (name, version, entry) in [
            ("playwright-core", "1.63.0", "index.mjs"),
            ("mediabunny", "1.41.0", "dist/bundles/mediabunny.mjs"),
        ] {
            put(
                &package,
                &format!("node_modules/{name}/package.json"),
                format!(r#"{{"name":"{name}","version":"{version}"}}"#).as_bytes(),
            );
            put(
                &package,
                &format!("node_modules/{name}/{entry}"),
                b"export {};",
            );
        }
        let node = put(root, "bin/node", b"not executed");
        let chromium = put(root, "bin/chromium", b"not executed");
        for file in [&node, &chromium] {
            fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
        }
        HostConfiguration {
            package: Some(package.into()),
            chromium: Some(chromium.into()),
            node: Some(node.into()),
            search_path: None,
            current_exe: None,
        }
    }

    #[test]
    fn capability_requires_actual_pinned_entries_and_regular_executables() {
        let directory = tempfile::tempdir().unwrap();
        let configuration = installation(directory.path());
        let available = capability_for(&configuration);
        assert!(available.available);
        assert_eq!(available.sample_rates, vec![44_100, 48_000]);
        let package = PathBuf::from(configuration.package.as_ref().unwrap());
        let manifest = package.join("node_modules/mediabunny/package.json");
        let old = fs::read(&manifest).unwrap();
        fs::write(&manifest, br#"{"name":"mediabunny","version":"1.42.0"}"#).unwrap();
        assert_eq!(
            resolve(&configuration).err(),
            Some(Unavailable::Unavailable)
        );
        fs::write(&manifest, old).unwrap();
        fs::remove_file(package.join("host/page.mjs")).unwrap();
        assert!(!capability_for(&configuration).available);
        put(&package, "host/page.mjs", b"export {};");
        fs::remove_file(package.join("src/iso-edit-list.mjs")).unwrap();
        assert!(!capability_for(&configuration).available);
        put(&package, "src/iso-edit-list.mjs", b"export {};");
        fs::set_permissions(
            PathBuf::from(configuration.chromium.as_ref().unwrap()),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(!capability_for(&configuration).available);
    }

    #[test]
    fn package_fallback_and_trusted_path_resolution_need_no_global_environment() {
        let directory = tempfile::tempdir().unwrap();
        let mut configuration = installation(directory.path());
        configuration.node = None;
        configuration.search_path = Some(
            env::join_paths([Path::new("."), directory.path().join("bin").as_path()]).unwrap(),
        );
        assert!(resolve(&configuration).is_ok());
        let exe = directory.path().join("host/opencut");
        let adjacent = exe.parent().unwrap().join("libexec/classic-audio-renderer");
        fs::create_dir_all(adjacent.parent().unwrap()).unwrap();
        fs::rename(
            PathBuf::from(configuration.package.take().unwrap()),
            &adjacent,
        )
        .unwrap();
        configuration.current_exe = Some(exe);
        assert!(resolve(&configuration).is_ok());
        configuration.chromium = None;
        assert_eq!(
            capability_for(&configuration).unavailable_reason.as_deref(),
            Some("AUDIO_RUNTIME_NOT_CONFIGURED")
        );
        configuration.package = Some("relative/package".into());
        configuration.chromium = Some(directory.path().join("bin/chromium").into());
        assert_eq!(
            resolve(&configuration).err(),
            Some(Unavailable::Unavailable)
        );
    }

    #[test]
    fn metadata_is_bounded_and_missing_configuration_does_not_leak_paths() {
        let directory = tempfile::tempdir().unwrap();
        let configuration = installation(directory.path());
        let package = PathBuf::from(configuration.package.as_ref().unwrap());
        fs::write(
            package.join("package.json"),
            vec![b' '; METADATA_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(
            capability_for(&configuration).unavailable_reason.as_deref(),
            Some("AUDIO_RUNTIME_UNAVAILABLE")
        );
        let absent = HostConfiguration {
            package: None,
            chromium: None,
            node: None,
            search_path: None,
            current_exe: None,
        };
        let reply = capability_for(&absent);
        assert!(!reply.available);
        assert_eq!(
            reply.unavailable_reason.as_deref(),
            Some("AUDIO_RUNTIME_NOT_CONFIGURED")
        );
        assert_eq!(reply.submit_tool, "opencut_job_audio_wav");
        assert_eq!(reply.schema_version, 1);
    }
}
