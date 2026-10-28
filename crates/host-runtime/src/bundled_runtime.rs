//! Trusted installation discovery, never a project/request-provided path.
//! A missing manifest permits the caller's existing external-runtime policy.
//! A present but damaged installation fails closed; callers must not hide it by
//! falling back to PATH. Explicit environment configuration is handled upstream.
use opencut_contracts::EditorError;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

const MAX_MANIFEST: u64 = 16 * 1024 * 1024;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_TOTAL: u64 = 8 * 1024 * 1024 * 1024;
const MAX_FILES: usize = 100_000;

fn invalid() -> EditorError {
    EditorError::new("CAPABILITY_UNAVAILABLE", "BUNDLED_RUNTIME_INVALID")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    schema_version: u32,
    target: String,
    delivery: String,
    #[serde(deserialize_with = "unique_map")]
    components: BTreeMap<String, Runtime>,
    #[serde(deserialize_with = "unique_map")]
    scripts: BTreeMap<String, String>,
    external_dependencies: Vec<String>,
    #[serde(deserialize_with = "unique_map")]
    files: BTreeMap<String, Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Runtime {
    entry: String,
    version: String,
    license_spdx: String,
    licenses: Vec<String>,
    fonttools: Option<FontTools>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontTools {
    fonttools_version: String,
    implementation_sha256: String,
    licenses: Vec<String>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Entry {
    File {
        sha256: String,
        size: u64,
        executable: bool,
    },
    Symlink {
        target: String,
    },
}

fn unique_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct Visitor<V>(std::marker::PhantomData<V>);
    impl<'de, V: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<V> {
        type Value = BTreeMap<String, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a map with unique keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut access: A,
        ) -> Result<Self::Value, A::Error> {
            let mut map = BTreeMap::new();
            while let Some((key, value)) = access.next_entry::<String, V>()? {
                if map.len() >= MAX_FILES || map.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate or excessive runtime entries",
                    ));
                }
            }
            Ok(map)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}

fn relative(value: &str) -> Result<&Path, EditorError> {
    if value.is_empty()
        || value.len() > 2048
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || value
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || Path::new(value)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid());
    }
    Ok(Path::new(value))
}
fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, EditorError> {
    let file = File::open(path).map_err(|_| invalid())?;
    let meta = file.metadata().map_err(|_| invalid())?;
    if !meta.is_file() || meta.len() > limit {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > limit {
        return Err(invalid());
    }
    Ok(bytes)
}
fn hash(path: &Path, size: u64) -> Result<String, EditorError> {
    let mut file = File::open(path).map_err(|_| invalid())?.take(size + 1);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(|_| invalid())?;
        if count == 0 {
            break;
        }
        total += count as u64;
        hasher.update(&buffer[..count]);
    }
    if total != size {
        return Err(invalid());
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn executable_mode(metadata: &fs::Metadata) -> Result<bool, EditorError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o6000 != 0 {
            return Err(invalid());
        }
        Ok(mode & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(invalid())
    }
}
fn collect(
    root: &Path,
    directory: &Path,
    found: &mut BTreeSet<String>,
    count: &mut usize,
) -> Result<(), EditorError> {
    for child in fs::read_dir(directory).map_err(|_| invalid())? {
        let child = child.map_err(|_| invalid())?;
        let path = child.path();
        *count += 1;
        let name = path
            .strip_prefix(root)
            .map_err(|_| invalid())?
            .to_str()
            .ok_or_else(invalid)?
            .to_owned();
        relative(&name)?;
        if *count > MAX_FILES {
            return Err(invalid());
        }
        let metadata = fs::symlink_metadata(&path).map_err(|_| invalid())?;
        if metadata.is_dir() {
            collect(root, &path, found, count)?;
        } else {
            if name != "runtime-bundle.json" && !found.insert(name) {
                return Err(invalid());
            }
            if found.len() > MAX_FILES {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
fn inside(root: &Path, value: &str) -> Result<PathBuf, EditorError> {
    let path = fs::canonicalize(root.join(relative(value)?)).map_err(|_| invalid())?;
    if !path.starts_with(root) {
        return Err(invalid());
    }
    Ok(path)
}
fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn load_uncached(root: &Path) -> Result<Option<(PathBuf, Manifest)>, EditorError> {
    let manifest_path = root.join("runtime-bundle.json");
    match fs::symlink_metadata(&manifest_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => (),
        _ => return Err(invalid()),
    }
    if fs::symlink_metadata(root)
        .map_err(|_| invalid())?
        .file_type()
        .is_symlink()
    {
        return Err(invalid());
    }
    let root = fs::canonicalize(root).map_err(|_| invalid())?;
    let manifest: Manifest =
        serde_json::from_slice(&bounded(&manifest_path, MAX_MANIFEST)?).map_err(|_| invalid())?;
    let complete = manifest
        .components
        .keys()
        .map(String::as_str)
        .eq(["chromium", "node", "python"]);
    let native = manifest.format == "opencut.native-runtime-bundle" && manifest.schema_version == 1;
    let dependencies: Vec<&str> = if native {
        let mut dependencies = vec!["ffmpeg", "ffprobe"];
        for component in ["node", "python"] {
            if !manifest.components.contains_key(component) {
                dependencies.push(component);
            }
        }
        dependencies
    } else if complete {
        vec!["ffmpeg", "ffprobe"]
    } else {
        vec!["ffmpeg", "ffprobe", "chromium", "node", "python"]
    };
    let legacy = manifest.format == "opencut.runtime-bundle.v1" && manifest.schema_version == 1;
    let current = manifest.format == "opencut.runtime-bundle.v2" && manifest.schema_version == 2;
    let mut scripts = BTreeMap::from([
        (
            "run_derived.py".to_owned(),
            "font-catalog/run_derived.py".to_owned(),
        ),
        (
            "run_derived_v2.py".to_owned(),
            "font-catalog/run_derived_v2.py".to_owned(),
        ),
    ]);
    if current || native {
        scripts.insert(
            "run_color_static_v1.py".to_owned(),
            "font-catalog/run_color_static_v1.py".to_owned(),
        );
    }
    if !(legacy || current || native)
        || !text(&manifest.target)
        || if native {
            manifest
                .components
                .keys()
                .any(|name| !matches!(name.as_str(), "node" | "python"))
        } else {
            !complete && !manifest.components.is_empty()
        }
        || manifest.delivery
            != if !manifest.components.is_empty() {
                "bundled_unverified"
            } else {
                "external_executables"
            }
        || manifest
            .external_dependencies
            .iter()
            .map(String::as_str)
            .ne(dependencies)
        || manifest.scripts != scripts
        || manifest.files.is_empty()
        || manifest.files.len() > MAX_FILES
    {
        return Err(invalid());
    }
    let mut total = 0_u64;
    for (name, entry) in &manifest.files {
        if native && !native_payload(name, &manifest.components) {
            return Err(invalid());
        }
        let path = root.join(relative(name)?);
        let metadata = fs::symlink_metadata(&path).map_err(|_| invalid())?;
        inside(&root, name)?;
        match entry {
            Entry::File {
                sha256,
                size,
                executable,
            } => {
                total = total.checked_add(*size).ok_or_else(invalid)?;
                if !metadata.is_file()
                    || *size > MAX_FILE
                    || total > MAX_TOTAL
                    || metadata.len() != *size
                    || executable_mode(&metadata)? != *executable
                    || hash(&path, *size)? != *sha256
                {
                    return Err(invalid());
                }
            }
            Entry::Symlink { target } => {
                if !metadata.file_type().is_symlink()
                    || target.is_empty()
                    || target.len() > 2048
                    || Path::new(target).is_absolute()
                    || target.contains('\\')
                    || target.chars().any(char::is_control)
                    || fs::read_link(&path).map_err(|_| invalid())? != Path::new(target)
                {
                    return Err(invalid());
                }
            }
        }
    }
    let mut found = BTreeSet::new();
    let mut entry_count = 0;
    collect(&root, &root, &mut found, &mut entry_count)?;
    if found != manifest.files.keys().cloned().collect() {
        return Err(invalid());
    }
    for name in manifest.scripts.values() {
        if !matches!(manifest.files.get(name), Some(Entry::File { .. })) {
            return Err(invalid());
        }
    }
    for (name, component) in &manifest.components {
        let prefix = root.join("runtimes").join(name);
        let entry = inside(&root, &component.entry)?;
        let component_path = format!("runtimes/{name}/");
        for relative in manifest
            .files
            .keys()
            .filter(|path| path.starts_with(&component_path))
        {
            if !inside(&root, relative)?.starts_with(&prefix) {
                return Err(invalid());
            }
        }
        if !entry.starts_with(&prefix)
            || !entry.is_file()
            || !executable_mode(&fs::metadata(&entry).map_err(|_| invalid())?)?
            || !text(&component.version)
            || !text(&component.license_spdx)
            || component.licenses.is_empty()
            || component.licenses.len() > 128
        {
            return Err(invalid());
        }
        for license in &component.licenses {
            let path = inside(&root, license)?;
            if !path.starts_with(&prefix) || !path.is_file() {
                return Err(invalid());
            }
        }
        if name == "python" {
            let fonttools = component.fonttools.as_ref().ok_or_else(invalid)?;
            if !text(&fonttools.fonttools_version)
                || fonttools.implementation_sha256.len() != 64
                || !fonttools
                    .implementation_sha256
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit())
                || fonttools.licenses.is_empty()
            {
                return Err(invalid());
            }
            for license in &fonttools.licenses {
                if !inside(&prefix, license)?.is_file() {
                    return Err(invalid());
                }
            }
        } else if component.fonttools.is_some() {
            return Err(invalid());
        }
    }
    Ok(Some((root, manifest)))
}

fn native_payload(name: &str, components: &BTreeMap<String, Runtime>) -> bool {
    matches!(
        name,
        "LICENSE"
            | "classic-visual-renderer/package.json"
            | "classic-visual-renderer/host/catalog-archive.mjs"
            | "font-catalog/LICENSE"
            | "font-catalog/run_derived.py"
            | "font-catalog/run_derived_v2.py"
            | "font-catalog/derive_static.py"
            | "font-catalog/derive_renamed_v2.py"
            | "font-catalog/generate.py"
            | "font-catalog/fonttools-lock.json"
            | "font-catalog/derived-runtime-lock.json"
            | "font-catalog/derived-v2-runtime-lock.json"
            | "font-catalog/run_color_static_v1.py"
            | "font-catalog/derive_color_static_v1.py"
            | "font-catalog/color-static-runtime-lock.json"
    ) || components
        .keys()
        .any(|component| name.starts_with(&format!("runtimes/{component}/")))
}

// Integrity belongs to the installation, shared by the host's adapters. Each
// lookup used to hash the complete installation again (including both lookups
// for Node and Chromium in one configuration). Cache only a verified snapshot,
// with a fresh filesystem identity fence on every use. Never cache absence or
// an invalid installation, or fall back to PATH after a failed fence.
#[cfg(unix)]
mod verified_snapshot {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::sync::{Mutex, OnceLock};

    #[derive(PartialEq, Eq)]
    struct Stamp {
        device: u64,
        inode: u64,
        mode: u32,
        length: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }
    fn stamp(path: &Path) -> Result<Stamp, EditorError> {
        let metadata = fs::symlink_metadata(path).map_err(|_| invalid())?;
        Ok(Stamp {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
    // Include every directory, even an empty one: its change time fences new,
    // removed or renamed entries without rewalking the tree on a cache hit.
    fn capture(root: &Path) -> Result<BTreeMap<PathBuf, Stamp>, EditorError> {
        fn visit(
            root: &Path,
            path: &Path,
            out: &mut BTreeMap<PathBuf, Stamp>,
        ) -> Result<(), EditorError> {
            if out.len() > MAX_FILES {
                return Err(invalid());
            }
            if path != root {
                relative(
                    path.strip_prefix(root)
                        .map_err(|_| invalid())?
                        .to_str()
                        .ok_or_else(invalid)?,
                )?;
            }
            let metadata = fs::symlink_metadata(path).map_err(|_| invalid())?;
            out.insert(path.to_owned(), stamp(path)?);
            if metadata.is_dir() {
                for child in fs::read_dir(path).map_err(|_| invalid())? {
                    visit(root, &child.map_err(|_| invalid())?.path(), out)?;
                }
            }
            Ok(())
        }
        let mut out = BTreeMap::new();
        visit(root, root, &mut out)?;
        Ok(out)
    }
    struct Verified {
        root: PathBuf,
        manifest: Arc<Manifest>,
        stamps: BTreeMap<PathBuf, Stamp>,
    }
    impl Verified {
        fn unchanged(&self) -> bool {
            self.stamps
                .iter()
                .all(|(path, expected)| stamp(path).is_ok_and(|actual| actual == *expected))
        }
    }
    // Host discovery normally uses one installation. Bound the retained roots
    // as well as each inventory; a mutex makes concurrent first lookups share
    // one full verification instead of launching competing whole-tree reads.
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Verified>>> = OnceLock::new();
    pub(super) fn load(root: &Path) -> Result<Option<(PathBuf, Arc<Manifest>)>, EditorError> {
        let mut cache = CACHE
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .map_err(|_| invalid())?;
        if let Some(verified) = cache.get(root)
            && verified.unchanged()
        {
            return Ok(Some((verified.root.clone(), verified.manifest.clone())));
        }
        cache.remove(root);
        match fs::symlink_metadata(root.join("runtime-bundle.json")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => (),
            _ => return Err(invalid()),
        }
        if fs::symlink_metadata(root)
            .map_err(|_| invalid())?
            .file_type()
            .is_symlink()
        {
            return Err(invalid());
        }
        let before = capture(root)?;
        let Some((canonical, manifest)) = load_uncached(root)? else {
            return Err(invalid());
        };
        if capture(root)? != before {
            return Err(invalid());
        }
        let manifest = Arc::new(manifest);
        if cache.len() == 4 {
            let oldest_key = cache
                .keys()
                .next()
                .cloned()
                .expect("bounded nonempty cache");
            cache.remove(&oldest_key);
        }
        cache.insert(
            root.to_owned(),
            Verified {
                root: canonical.clone(),
                manifest: manifest.clone(),
                stamps: before,
            },
        );
        Ok(Some((canonical, manifest)))
    }
}
fn load(root: &Path) -> Result<Option<(PathBuf, Arc<Manifest>)>, EditorError> {
    #[cfg(unix)]
    {
        verified_snapshot::load(root)
    }
    #[cfg(not(unix))]
    {
        load_uncached(root).map(|result| result.map(|(path, manifest)| (path, Arc::new(manifest))))
    }
}

pub(super) fn executable(component: &str) -> Result<Option<PathBuf>, EditorError> {
    if !matches!(component, "node" | "chromium" | "python") {
        return Err(invalid());
    }
    let exe = std::env::current_exe().map_err(|_| invalid())?;
    resolve(&exe, component, false)
}
pub(super) fn script(name: &str) -> Result<Option<PathBuf>, EditorError> {
    if !matches!(
        name,
        "run_derived.py" | "run_derived_v2.py" | "run_color_static_v1.py"
    ) {
        return Err(invalid());
    }
    let exe = std::env::current_exe().map_err(|_| invalid())?;
    resolve(&exe, name, true)
}
fn resolve(exe: &Path, name: &str, is_script: bool) -> Result<Option<PathBuf>, EditorError> {
    let directory = exe.parent().ok_or_else(invalid)?.join("libexec");
    let Some((root, manifest)) = load(&directory)? else {
        return Ok(None);
    };
    let path = if is_script {
        manifest.scripts.get(name)
    } else {
        manifest.components.get(name).map(|c| &c.entry)
    };
    path.map(|path| inside(&root, path)).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(root: &Path) -> PathBuf {
        let directory = root.join("bin/libexec/font-catalog");
        fs::create_dir_all(&directory).unwrap();
        let mut files = serde_json::Map::new();
        for name in ["run_derived.py", "run_derived_v2.py"] {
            let path = directory.join(name);
            fs::write(&path, b"fixed fixture").unwrap();
            files.insert(format!("font-catalog/{name}"), json!({"kind":"file","size":13,"sha256":hash(&path,13).unwrap(),"executable":false}));
        }
        let manifest = json!({"format":"opencut.runtime-bundle.v1","schema_version":1,"target":"fixture", "delivery":"external_executables", "components":{},
            "scripts":{"run_derived.py":"font-catalog/run_derived.py","run_derived_v2.py":"font-catalog/run_derived_v2.py"},
            "external_dependencies":["ffmpeg","ffprobe","chromium","node","python"],"files":files});
        fs::write(
            root.join("bin/libexec/runtime-bundle.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        root.join("bin/opencut")
    }
    #[test]
    fn relocated_scripts_resolve_without_claiming_external_executables() {
        let root = tempfile::tempdir().unwrap();
        let exe = fixture(root.path());
        assert_eq!(resolve(&exe, "node", false).unwrap(), None);
        assert_eq!(
            resolve(&exe, "run_derived.py", true).unwrap(),
            Some(
                fs::canonicalize(root.path().join("bin/libexec/font-catalog/run_derived.py"))
                    .unwrap()
            )
        );
        fs::write(
            root.path().join("bin/libexec/font-catalog/run_derived.py"),
            b"changed bytes",
        )
        .unwrap();
        assert!(resolve(&exe, "run_derived.py", true).is_err());
    }
    #[test]
    fn native_auxiliary_manifest_resolves_scripts_and_rejects_browser_payload() {
        let root = tempfile::tempdir().unwrap();
        let exe = fixture(root.path());
        let directory = root.path().join("bin/libexec");
        let path = directory.join("runtime-bundle.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["format"] = json!("opencut.native-runtime-bundle");
        manifest["external_dependencies"] = json!(["ffmpeg", "ffprobe", "node", "python"]);
        let name = "font-catalog/run_color_static_v1.py";
        let script = directory.join(name);
        fs::write(&script, b"fixed fixture").unwrap();
        manifest["scripts"]["run_color_static_v1.py"] = json!(name);
        manifest["files"][name] =
            json!({"kind":"file","size":13,"sha256":hash(&script,13).unwrap(),"executable":false});
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(
            resolve(&exe, "run_color_static_v1.py", true)
                .unwrap()
                .is_some()
        );
        assert_eq!(resolve(&exe, "chromium", false).unwrap(), None);
        // Even a correctly hashed and inventoried old renderer is forbidden in
        // a new native installation; historical manifests retain exact reads.
        fs::create_dir_all(directory.join("classic-audio-renderer/src")).unwrap();
        let browser = directory.join("classic-audio-renderer/src/index.mjs");
        fs::write(&browser, b"fixed fixture").unwrap();
        manifest["files"]["classic-audio-renderer/src/index.mjs"] =
            json!({"kind":"file","size":13,"sha256":hash(&browser,13).unwrap(),"executable":false});
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(resolve(&exe, "run_derived.py", true).is_err());
    }
    #[test]
    fn missing_is_optional_but_extra_files_and_damaged_manifest_are_not() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve(&root.path().join("bin/opencut"), "node", false).unwrap(),
            None
        );
        let exe = fixture(root.path());
        // Prime a healthy snapshot before the inventory changes.
        assert_eq!(resolve(&exe, "node", false).unwrap(), None);
        fs::write(root.path().join("bin/libexec/unregistered"), b"extra").unwrap();
        assert!(resolve(&exe, "node", false).is_err());
        fs::remove_file(root.path().join("bin/libexec/unregistered")).unwrap();
        fs::write(root.path().join("bin/libexec/runtime-bundle.json"), b"{}").unwrap();
        assert!(resolve(&exe, "node", false).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_file_and_executable_mode_tampering_fail_closed() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let root = tempfile::tempdir().unwrap();
        let exe = fixture(root.path());
        assert_eq!(resolve(&exe, "node", false).unwrap(), None);
        let file = root.path().join("bin/libexec/font-catalog/run_derived.py");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(resolve(&exe, "node", false).is_err());
        fs::remove_file(&file).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, b"fixed fixture").unwrap();
        symlink(&outside, &file).unwrap();
        assert!(resolve(&exe, "node", false).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn declared_internal_link_is_valid_but_cannot_be_retargeted() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let exe = fixture(root.path());
        let directory = root.path().join("bin/libexec");
        let manifest_path = directory.join("runtime-bundle.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["files"]["font-catalog/alias.py"] =
            json!({"kind":"symlink","target":"run_derived.py"});
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let alias = directory.join("font-catalog/alias.py");
        symlink("run_derived.py", &alias).unwrap();
        assert!(resolve(&exe, "run_derived.py", true).unwrap().is_some());
        fs::remove_file(&alias).unwrap();
        symlink("run_derived_v2.py", &alias).unwrap();
        assert!(resolve(&exe, "run_derived.py", true).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn verified_snapshot_detects_same_size_rewrite_with_restored_mtime() {
        use std::fs::FileTimes;
        let root = tempfile::tempdir().unwrap();
        let exe = fixture(root.path());
        let directory = root.path().join("bin/libexec");
        fs::create_dir(directory.join("empty")).unwrap();
        let file = directory.join("font-catalog/run_derived.py");
        assert!(resolve(&exe, "run_derived.py", true).unwrap().is_some());
        let before = fs::metadata(&file).unwrap();
        fs::write(&file, [b'x'; 13]).unwrap();
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
        assert_eq!(fs::metadata(&file).unwrap().len(), before.len());
        assert_eq!(
            fs::metadata(&file).unwrap().modified().unwrap(),
            before.modified().unwrap()
        );
        assert!(resolve(&exe, "run_derived.py", true).is_err());
        fs::write(&file, b"fixed fixture").unwrap();
        assert!(resolve(&exe, "run_derived.py", true).unwrap().is_some());
        fs::write(directory.join("empty/undeclared"), b"new entry").unwrap();
        assert!(resolve(&exe, "run_derived.py", true).is_err());
    }
}
