//! Owner-local exchange. All media move through streaming verified descriptors;
//! plugins use the existing package verifiers and remain reviewed proposals.
use crate::{
    package_io::{self, PackageDirectory},
    project_package as common,
};
use opencut_api_contracts::project_exchange::*;
use opencut_asset_store::AssetStore;
use opencut_contracts::{Asset, AssetKind, AssetVersion, EditorError};
use opencut_editor_application::{
    project_archive as history,
    project_exchange::{self as app, PackageSource, PreparedPackage},
};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

mod historical;
pub use historical::{PreparedImport, inspect_package, read_package};

fn asset(version: &AssetVersion, uri: String) -> Asset {
    Asset {
        id: version.content_hash.clone(),
        name: version.content_hash.clone(),
        kind: AssetKind::Image,
        uri,
        duration: None,
        version: Some(version.clone()),
        metadata: Default::default(),
    }
}
fn tools(probe: &crate::media::ProbeFactory) -> Result<Vec<PackageToolLock>, EditorError> {
    let mut locks = probe()
        .map_err(|e| EditorError::new("PACKAGE_TOOL_UNAVAILABLE", e.to_string()))?
        .tool_fingerprints()
        .map_err(|e| {
            EditorError::new(
                "PACKAGE_TOOL_UNAVAILABLE",
                format!("Cannot fingerprint native tools: {e}"),
            )
        })?
        .into_iter()
        .map(|t| PackageToolLock {
            name: t.name,
            version: t.version,
            binary_sha256: t.binary_sha256,
        })
        .collect::<Vec<_>>();
    locks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(locks)
}
fn mismatches(expected: &[PackageToolLock], probe: &crate::media::ProbeFactory) -> Vec<String> {
    let actual = tools(probe).unwrap_or_default();
    ["ffmpeg", "ffprobe"]
        .into_iter()
        .filter(|name| {
            !expected.iter().any(|e| {
                e.name == *name
                    && actual.iter().any(|a| {
                        (
                            a.name.as_str(),
                            a.version.as_str(),
                            a.binary_sha256.as_str(),
                        ) == (
                            e.name.as_str(),
                            e.version.as_str(),
                            e.binary_sha256.as_str(),
                        )
                    })
            })
        })
        .map(str::to_owned)
        .collect()
}
fn destination(text: &str) -> Result<(PathBuf, PathBuf), EditorError> {
    let path = Path::new(text);
    if !path.is_absolute()
        || path.components().any(|c| matches!(c, Component::ParentDir))
        || fs::symlink_metadata(path).is_ok()
    {
        return Err(app::invalid(
            "Package destination must be a new absolute directory",
        ));
    }
    let parent = fs::canonicalize(
        path.parent()
            .ok_or_else(|| app::invalid("Package destination has no parent"))?,
    )
    .map_err(common::io)?;
    let target = parent.join(
        path.file_name()
            .ok_or_else(|| app::invalid("Invalid package destination"))?,
    );
    Ok((parent, target))
}

pub fn export_package(
    input: &ExportPackage,
    source: &PackageSource,
    asset_root: &Path,
    plugin_root: &Path,
    probe: &crate::media::ProbeFactory,
) -> Result<ExportReceipt, EditorError> {
    if source.content.archive.source_project_id != input.project_id
        || source.content.archive.main_head_revision != input.revision
    {
        return Err(app::invalid(
            "Package source differs from the selected revision",
        ));
    }
    app::validate_source(source, input.scope, &input.mode)?;
    let (parent, target) = destination(&input.destination)?;
    let staging = tempfile::Builder::new()
        .prefix(".opencut-package-")
        .tempdir_in(&parent)
        .map_err(common::io)?;
    let store = AssetStore::read_only(asset_root)?;
    let versions = app::blob_versions(&source.content.archive)?;
    let mut blobs = Vec::with_capacity(versions.len());
    if !versions.is_empty() && input.mode == PackageMode::SelfContained {
        fs::create_dir(staging.path().join("blobs")).map_err(common::io)?;
    }
    for version in versions.values() {
        let location = match input.mode {
            PackageMode::SelfContained => {
                let (mut file, size) = store.open_version_verified(version)?;
                let path = format!("blobs/{}", version.content_hash);
                common::copy_verified(
                    &mut file,
                    &staging.path().join(&path),
                    &version.content_hash,
                    size,
                )?;
                BlobLocation::Bundled(path)
            }
            PackageMode::Linked => {
                BlobLocation::Linked(store.resolve_asset(&asset(version, String::new()))?.uri)
            }
        };
        blobs.push(PackageBlob {
            content_hash: version.content_hash.clone(),
            size_bytes: version.size,
            location,
        });
    }
    let workspace = asset_root
        .parent()
        .ok_or_else(|| app::invalid("Asset store has no workspace"))?;
    let mut bundles = Vec::with_capacity(source.plugin_policies.len());
    let mut plugin_bytes = 0u64;
    for policy in &source.plugin_policies {
        let (kind, id, version, hash, _) = app::policy_identity(policy);
        let path = format!(
            "plugins/{}/{hash}",
            match kind {
                PluginKind::Wasm => "wasm",
                PluginKind::Presets => "presets",
            }
        );
        let output = staging.path().join(&path);
        fs::create_dir_all(
            output
                .parent()
                .ok_or_else(|| app::invalid("Invalid plugin path"))?,
        )
        .map_err(common::io)?;
        let (size, permissions) = match kind {
            PluginKind::Wasm => opencut_plugin_host::PluginHost::read_only(plugin_root)?
                .copy_installed_package(
                    id,
                    version,
                    hash,
                    &output,
                    app::MAX_PLUGIN_BYTES - plugin_bytes,
                )?,
            PluginKind::Presets => (
                crate::presets::PresetPackages::new(workspace)
                    .copy_installed_package(id, version, hash, &output)?,
                BTreeSet::new(),
            ),
        };
        plugin_bytes = plugin_bytes
            .checked_add(size)
            .filter(|n| *n <= app::MAX_PLUGIN_BYTES)
            .ok_or_else(|| app::invalid("Plugin bundles exceed package budget"))?;
        bundles.push(PluginBundle {
            kind,
            id: id.into(),
            version: version.into(),
            package_sha256: hash.into(),
            path,
            proposed_global_permissions: permissions,
        });
    }
    let manifest = app::make_manifest(
        source,
        input.scope,
        input.mode.clone(),
        blobs,
        bundles,
        tools(probe).unwrap_or_default(),
    )?;
    let archive = app::archive_bytes(&source.content.archive)?;
    let bytes = app::canonical_bytes(&manifest, history::MAX_MANIFEST_BYTES)?;
    common::write_file(&staging.path().join("archive.json"), &archive)?;
    common::write_file(&staging.path().join("manifest.json"), &bytes)?;
    sync_tree(staging.path())?;
    package_io::publish(staging.path(), &target)?;
    common::sync_dir(&parent)?;
    let prepared = PreparedPackage {
        manifest_sha256: app::digest(&bytes),
        fonts_missing: opencut_media::missing_system_font_families(
            &manifest.external_font_families,
        ),
        manifest: manifest.clone(),
        archive: source.content.archive.clone(),
        tool_lock_mismatches: if manifest.tool_locks.len() == 2 {
            vec![]
        } else {
            vec!["ffmpeg".into(), "ffprobe".into()]
        },
        install_plan: vec![],
    };
    Ok(ExportReceipt {
        package_path: target.to_string_lossy().into_owned(),
        package: app::inspection(&prepared)?,
    })
}
fn sync_tree(path: &Path) -> Result<(), EditorError> {
    for entry in fs::read_dir(path).map_err(common::io)? {
        let entry = entry.map_err(common::io)?;
        if entry.file_type().map_err(common::io)?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            File::open(entry.path())
                .map_err(common::io)?
                .sync_all()
                .map_err(common::io)?;
        }
    }
    common::sync_dir(path)
}

fn directory(root: &PackageDirectory, path: &str) -> Result<PackageDirectory, EditorError> {
    let mut current = PackageDirectory {
        file: root.file.try_clone().map_err(common::io)?,
    };
    for part in path.split('/') {
        current = PackageDirectory {
            file: current.child(part, true)?,
        };
    }
    Ok(current)
}
fn blob(root: &PackageDirectory, b: &PackageBlob) -> Result<File, EditorError> {
    match &b.location {
        BlobLocation::Bundled(path) => root.resource(path),
        BlobLocation::Linked(path) => {
            let path = Path::new(path);
            // Parent aliases are canonicalized by the exporter; links are never
            // followed inside a linked path supplied by an untrusted package.
            if fs::canonicalize(path).map_err(common::io)? != path {
                return Err(app::invalid("Linked source must be a canonical local path"));
            }
            let parent = PackageDirectory::open(
                path.parent()
                    .ok_or_else(|| app::invalid("Linked source has no parent"))?,
            )?;
            parent.child(
                path.file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| app::invalid("Invalid linked source name"))?,
                false,
            )
        }
    }
}

// Copy a plugin tree through anchored descriptors before passing it to the
// mature format-specific verifier. Only bounded plugin bytes enter private
// staging; multi-GB media are independently streamed, never read_to_end.
fn copy_plugin(
    root: &PackageDirectory,
    target: &Path,
    total: &mut u64,
    entries: &mut usize,
    depth: usize,
) -> Result<(), EditorError> {
    if depth > 16 {
        return Err(app::invalid("Plugin tree is too deep"));
    }
    fs::create_dir(target).map_err(common::io)?;
    for name in root.names()? {
        *entries = entries
            .checked_add(1)
            .filter(|n| *n <= history::MAX_HISTORY_ROWS)
            .ok_or_else(|| app::invalid("Plugin tree has too many entries"))?;
        // O_NOFOLLOW and fstat in child reject links, devices and pipes.
        #[cfg(unix)]
        let is_directory = {
            use rustix::fs::{AtFlags, FileType, statat};
            let stat = statat(&root.file, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| common::io(e.into()))?;
            FileType::from_raw_mode(stat.st_mode) == FileType::Directory
        };
        #[cfg(not(unix))]
        let is_directory = false;
        if is_directory {
            copy_plugin(
                &directory(root, &name)?,
                &target.join(&name),
                total,
                entries,
                depth + 1,
            )?;
        } else {
            let mut source = root.child(&name, false)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target.join(&name))
                .map_err(common::io)?;
            let mut block = [0u8; 64 * 1024];
            loop {
                let n = source.read(&mut block).map_err(common::io)?;
                if n == 0 {
                    break;
                }
                *total = total
                    .checked_add(n as u64)
                    .filter(|n| *n <= app::MAX_PLUGIN_BYTES)
                    .ok_or_else(|| app::invalid("Plugin bundles exceed package budget"))?;
                output.write_all(&block[..n]).map_err(common::io)?;
            }
        }
    }
    Ok(())
}
fn verify_plugins(
    root: &PackageDirectory,
    path: &Path,
    bundles: &[PluginBundle],
) -> Result<Vec<PluginGrantRestoreAction>, EditorError> {
    let staging = tempfile::Builder::new()
        .prefix(".opencut-package-review-")
        .tempdir()
        .map_err(common::io)?;
    let (mut total, mut entries) = (0, 0);
    let mut plan = Vec::with_capacity(bundles.len());
    for (index, bundle) in bundles.iter().enumerate() {
        let owned = staging.path().join(index.to_string());
        copy_plugin(
            &directory(root, &bundle.path)?,
            &owned,
            &mut total,
            &mut entries,
            0,
        )?;
        match bundle.kind {
            PluginKind::Wasm => {
                let (m, hash) = opencut_plugin_host::PluginHost::verify_package_root(&owned)?;
                if m.id != bundle.id
                    || m.version != bundle.version
                    || hash != bundle.package_sha256
                    || !bundle
                        .proposed_global_permissions
                        .iter()
                        .all(|permission| m.permissions.contains(permission))
                {
                    return Err(app::invalid(
                        "WASM bundle identity or permission proposal differs",
                    ));
                }
            }
            PluginKind::Presets => {
                let (m, hash) = crate::presets::PresetPackages::verify_package_root(&owned)?;
                if m.id != bundle.id || m.version != bundle.version || hash != bundle.package_sha256
                {
                    return Err(app::invalid("Preset bundle identity differs"));
                }
            }
        }
        plan.push(PluginGrantRestoreAction::Install(
            opencut_api_contracts::plugin_packages::PinnedPluginPackage {
                package_root: path
                    .join(&bundle.path)
                    .to_str()
                    .ok_or_else(|| app::invalid("Plugin path is not UTF-8"))?
                    .into(),
                expected_package_sha256: bundle.package_sha256.clone(),
                granted_permissions: bundle.proposed_global_permissions.iter().cloned().collect(),
            },
        ));
    }
    Ok(plan)
}

fn validate_tree(root: &PackageDirectory, m: &PackageManifest) -> Result<(), EditorError> {
    let mut expected = BTreeSet::from(["manifest.json".into(), "archive.json".into()]);
    if m.mode == PackageMode::SelfContained && !m.blobs.is_empty() {
        expected.insert("blobs".into());
    }
    if !m.plugin_bundles.is_empty() {
        expected.insert("plugins".into());
    }
    if root.names()? != expected {
        return Err(app::invalid("Unexpected or missing package entry"));
    }
    if m.mode == PackageMode::SelfContained && !m.blobs.is_empty() {
        let blobs = directory(root, "blobs")?;
        if blobs.names()? != m.blobs.iter().map(|b| b.content_hash.clone()).collect() {
            return Err(app::invalid("Unexpected or missing package resource"));
        }
    }
    if !m.plugin_bundles.is_empty() {
        let plugins = directory(root, "plugins")?;
        let kinds: BTreeSet<String> = m
            .plugin_bundles
            .iter()
            .map(|b| match b.kind {
                PluginKind::Wasm => "wasm".into(),
                PluginKind::Presets => "presets".into(),
            })
            .collect();
        if plugins.names()? != kinds {
            return Err(app::invalid("Unexpected plugin kind directory"));
        }
        for kind in kinds {
            let expected = m
                .plugin_bundles
                .iter()
                .filter(|b| b.path.starts_with(&format!("plugins/{kind}/")))
                .map(|b| b.package_sha256.clone())
                .collect();
            if directory(&plugins, &kind)?.names()? != expected {
                return Err(app::invalid("Unexpected or missing plugin bundle"));
            }
        }
    }
    Ok(())
}

fn verified(
    path: &str,
    expected: Option<&str>,
    probe: &crate::media::ProbeFactory,
) -> Result<(PackageDirectory, PreparedPackage), EditorError> {
    let canonical = package_io::root(path)?;
    let root = PackageDirectory::open(&canonical)?;
    let bytes = package_io::bounded(
        root.child("manifest.json", false)?,
        history::MAX_MANIFEST_BYTES,
    )?;
    let hash = app::digest(&bytes);
    if expected.is_some_and(|e| e != hash) {
        return Err(app::invalid("Package manifest SHA-256 mismatch"));
    }
    let manifest: PackageManifest =
        serde_json::from_slice(&bytes).map_err(|_| app::invalid("Invalid package manifest"))?;
    if app::canonical_bytes(&manifest, history::MAX_MANIFEST_BYTES)? != bytes {
        return Err(app::invalid("Package manifest is not canonical"));
    }
    let bytes = package_io::bounded(
        root.child("archive.json", false)?,
        history::MAX_HISTORY_BYTES,
    )?;
    let archive = app::decode_archive(&bytes)?;
    app::validate_manifest(&manifest, &archive)?;
    validate_tree(&root, &manifest)?;
    let install_plan = verify_plugins(&root, &canonical, &manifest.plugin_bundles)?;
    let prepared = PreparedPackage {
        manifest_sha256: hash,
        tool_lock_mismatches: mismatches(&manifest.tool_locks, probe),
        fonts_missing: opencut_media::missing_system_font_families(
            &manifest.external_font_families,
        ),
        manifest,
        archive,
        install_plan,
    };
    app::validate_prepared(&prepared)?;
    for b in &prepared.manifest.blobs {
        package_io::verify_blob(
            blob(&root, b)?,
            &opencut_api_contracts::project_archive::ArchiveBlob {
                content_hash: b.content_hash.clone(),
                size_bytes: b.size_bytes,
                path: String::new(),
            },
        )?;
    }
    Ok((root, prepared))
}
fn read_current(
    input: &ImportPackage,
    asset_root: &Path,
    probe: &crate::media::ProbeFactory,
) -> Result<PreparedPackage, EditorError> {
    let (root, prepared) = verified(
        &input.package_path,
        Some(&input.expected_manifest_sha256),
        probe,
    )?;
    app::prepare_import(&prepared, &input.target_project_id)?;
    let total = prepared
        .manifest
        .blobs
        .iter()
        .try_fold(0u64, |sum, b| sum.checked_add(b.size_bytes))
        .ok_or_else(|| app::invalid("Package size overflow"))?;
    let workspace = asset_root
        .parent()
        .ok_or_else(|| app::invalid("Asset store has no workspace"))?;
    let _reservation =
        opencut_storage_budget::Budget::new(workspace).reserve("private_asset_copy", total)?;
    let staging = tempfile::Builder::new()
        .prefix(".opencut-package-import-")
        .tempdir_in(workspace)
        .map_err(common::io)?;
    for b in &prepared.manifest.blobs {
        common::copy_verified(
            &mut blob(&root, b)?,
            &staging.path().join(&b.content_hash),
            &b.content_hash,
            b.size_bytes,
        )?;
    }
    let store = AssetStore::open_managed(asset_root)?;
    for version in app::blob_versions(&prepared.archive)?.values() {
        let mut capture = asset(
            version,
            staging
                .path()
                .join(&version.content_hash)
                .to_string_lossy()
                .into_owned(),
        );
        capture.version = None;
        let captured = store.pin_asset(&capture)?;
        if captured.version.as_ref() != Some(version) {
            return Err(app::invalid("Captured CAS version differs from package"));
        }
    }
    Ok(prepared)
}
