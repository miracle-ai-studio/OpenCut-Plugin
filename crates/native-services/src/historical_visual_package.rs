//! Owner-local schema 3 project packages and schema 5 plugin bundles, built on the v1 media verifier.
use crate::project_package as common;
use opencut_api_contracts::{
    historical_visual_package::{
        ExportReceipt, HistoricalVisualExport, HistoricalVisualImport, HistoricalVisualManifest,
        PackagePluginKind, PackageToolLock, PluginGrantRestoreAction, PreparedHistoricalVisual,
    },
    project_package::{PackageManifest, PackageMode},
};
use opencut_asset_store::AssetStore;
use opencut_contracts::EditorError;
use opencut_editor_application::historical_visual_package::{self, HistoricalVisualSource};
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_PLUGIN_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;

fn common_manifest(manifest: &HistoricalVisualManifest) -> PackageManifest {
    PackageManifest {
        schema_version: 1,
        mode: manifest.mode.clone(),
        source_project_id: manifest.source_project_id.clone(),
        source_revision: manifest.source_revision.clone(),
        project_path: manifest.project_path.clone(),
        project_sha256: manifest.project_sha256.clone(),
        assets: manifest.assets.clone(),
        dependencies: manifest.dependencies.clone(),
        self_contained_media: manifest.self_contained_media,
        fully_reproducible: manifest.fully_reproducible,
    }
}

#[cfg(test)]
pub fn export_historical_visual(
    input: &HistoricalVisualExport,
    source: &HistoricalVisualSource,
    asset_store_root: &Path,
    plugin_root: &Path,
) -> Result<ExportReceipt, EditorError> {
    if source.snapshot.project.base.id != input.project_id
        || source.snapshot.revision != input.revision
    {
        return Err(common::invalid(
            "Export source is not the requested fixed revision",
        ));
    }
    let requested = Path::new(&input.destination);
    if !requested.is_absolute() || requested.exists() {
        return Err(common::invalid(
            "Package destination must be a new absolute directory",
        ));
    }
    let parent = fs::canonicalize(
        requested
            .parent()
            .ok_or_else(|| common::invalid("Invalid package destination"))?,
    )
    .map_err(common::io)?;
    let destination = parent.join(
        requested
            .file_name()
            .ok_or_else(|| common::invalid("Invalid package destination name"))?,
    );
    if destination.exists() {
        return Err(common::invalid("Package destination already exists"));
    }
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| common::invalid("System clock is invalid"))?
        .as_nanos();
    let staging = parent.join(format!(
        ".opencut-package-v3-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir(&staging).map_err(common::io)?;
    let result = (|| {
        let store = AssetStore::open_managed(asset_store_root)?;
        let mut document = source.snapshot.project.clone();
        historical_visual_package::validate_export_document(&document)?;
        let inventory = common::export_media(&mut document.base, &input.mode, &staging, &store)?;
        historical_visual_package::validate_export_document(&document)?;
        let document_bytes = serde_json::to_vec(&document)
            .map_err(|_| common::invalid("Could not serialize schema 3 project document"))?;
        if document_bytes.len() as u64 > common::MAX_DOCUMENT_BYTES {
            return Err(common::invalid(
                "Project document exceeds package size limit",
            ));
        }
        common::write_file(&staging.join("project.json"), &document_bytes)?;
        let mut dependencies = historical_visual_package::dependencies(source);
        let mut plugin_bundles = historical_visual_package::plugin_bundles(&source.plugin_grants);
        let workspace_root = asset_store_root
            .parent()
            .ok_or_else(|| common::invalid("Invalid workspace asset-store path"))?;
        let mut plugin_bytes = 0_u64;
        if !plugin_bundles.is_empty() {
            for bundle in &mut plugin_bundles {
                let target = staging.join(&bundle.path);
                fs::create_dir_all(
                    target
                        .parent()
                        .ok_or_else(|| common::invalid("Invalid plugin bundle path"))?,
                )
                .map_err(common::io)?;
                let copied = match bundle.kind {
                    PackagePluginKind::PlannerV1 => {
                        let (size, permissions) =
                            opencut_plugin_host::PluginHost::read_only(plugin_root)?
                                .copy_installed_package(
                                    &bundle.id,
                                    &bundle.version,
                                    &bundle.package_sha256,
                                    &target,
                                    MAX_PLUGIN_BUNDLE_BYTES - plugin_bytes,
                                )?;
                        bundle.proposed_global_permissions = permissions;
                        size
                    }
                    PackagePluginKind::PresetV2 => {
                        crate::presets::PresetPackages::new(workspace_root).copy_installed_package(
                            &bundle.id,
                            &bundle.version,
                            &bundle.package_sha256,
                            &target,
                        )?
                    }
                };
                plugin_bytes = plugin_bytes
                    .checked_add(copied)
                    .ok_or_else(|| common::invalid("Plugin bundle size overflow"))?;
                if plugin_bytes > MAX_PLUGIN_BUNDLE_BYTES {
                    return Err(common::invalid("Plugin bundles exceed package size limit"));
                }
            }
        }
        dependencies.plugins_not_bundled.clear();
        let tool_locks = opencut_media::FfmpegBackend::discover()
            .and_then(|backend| backend.tool_fingerprints())
            .map_err(|error| {
                EditorError::new(
                    "PACKAGE_TOOL_UNAVAILABLE",
                    format!("Could not fingerprint package render tools: {error}"),
                )
            })?
            .into_iter()
            .map(|tool| PackageToolLock {
                name: tool.name,
                version: tool.version,
                binary_sha256: tool.binary_sha256,
            })
            .collect::<Vec<_>>();
        dependencies.tools_not_locked.clear();
        let manifest = HistoricalVisualManifest {
            schema_version: 5,
            project_schema_version: 3,
            mode: input.mode.clone(),
            source_project_id: input.project_id.clone(),
            source_revision: input.revision.clone(),
            project_path: "project.json".into(),
            project_sha256: common::digest(&document_bytes),
            assets: inventory,
            dependencies: dependencies.clone(),
            tool_locks: tool_locks.clone(),
            plugin_grants: source.plugin_grants.clone(),
            plugin_bundles,
            self_contained_media: input.mode == PackageMode::SelfContained,
            fully_reproducible: input.mode == PackageMode::SelfContained
                && dependencies.fonts_not_bundled.is_empty()
                && dependencies.plugins_not_bundled.is_empty()
                && dependencies.tools_not_locked.is_empty()
                && dependencies.unsupported.is_empty(),
        };
        historical_visual_package::validate_manifest(&manifest)?;
        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|_| common::invalid("Could not serialize schema 3 manifest"))?;
        if manifest_bytes.len() as u64 > common::MAX_MANIFEST_BYTES {
            return Err(common::invalid("Package manifest exceeds size limit"));
        }
        common::write_file(&staging.join("manifest.json"), &manifest_bytes)?;
        if input.mode == PackageMode::SelfContained {
            common::sync_dir(&staging.join("media"))?;
        }
        sync_package_directories(&staging)?;
        common::sync_dir(&staging)?;
        if destination.exists() {
            return Err(common::invalid(
                "Package destination appeared during export",
            ));
        }
        fs::rename(&staging, &destination).map_err(common::io)?;
        common::sync_dir(&parent)?;
        Ok(ExportReceipt {
            package_path: destination.to_string_lossy().into_owned(),
            manifest_sha256: common::digest(&manifest_bytes),
            source_revision: input.revision.clone(),
            asset_count: document.base.assets.len(),
            dependencies,
            tool_locks,
            plugin_grants: source.plugin_grants.clone(),
            self_contained_media: manifest.self_contained_media,
            fully_reproducible: manifest.fully_reproducible,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn sync_package_directories(root: &Path) -> Result<(), EditorError> {
    for entry in fs::read_dir(root).map_err(common::io)? {
        let entry = entry.map_err(common::io)?;
        if entry.file_type().map_err(common::io)?.is_dir() {
            sync_package_directories(&entry.path())?;
        }
    }
    common::sync_dir(root)
}

pub fn read_historical_visual(
    input: &HistoricalVisualImport,
    asset_store_root: &Path,
    probe: &crate::media::ProbeFactory,
) -> Result<PreparedHistoricalVisual, EditorError> {
    read_package_mode(input, asset_store_root, true, probe)
}
pub(crate) fn read_package_mode(
    input: &HistoricalVisualImport,
    asset_store_root: &Path,
    capture: bool,
    probe: &crate::media::ProbeFactory,
) -> Result<PreparedHistoricalVisual, EditorError> {
    let root = Path::new(&input.package_path);
    if !root.is_absolute()
        || fs::canonicalize(root).map_err(common::io)? != root
        || !fs::symlink_metadata(root)
            .map_err(common::io)?
            .file_type()
            .is_dir()
    {
        return Err(common::invalid(
            "Package root must be a canonical directory without symlinks",
        ));
    }
    let manifest_bytes = common::bounded_bytes(
        &common::relative(root, "manifest.json")?,
        common::MAX_MANIFEST_BYTES,
    )?;
    let manifest_sha256 = common::digest(&manifest_bytes);
    if !common::valid_hash(&input.expected_manifest_sha256)
        || manifest_sha256 != input.expected_manifest_sha256
    {
        return Err(common::invalid("Package manifest SHA-256 mismatch"));
    }
    let manifest: HistoricalVisualManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| common::invalid("Schema 3 package manifest is invalid"))?;
    historical_visual_package::validate_manifest(&manifest)?;
    let plugin_bundle_paths = verify_plugin_bundles(root, &manifest)?;
    let plugin_package_install_plan = plugin_package_install_plan(root, &manifest)?;
    let document_bytes = common::bounded_bytes(
        &common::relative(root, &manifest.project_path)?,
        common::MAX_DOCUMENT_BYTES,
    )?;
    if common::digest(&document_bytes) != manifest.project_sha256 {
        return Err(common::invalid("Project document SHA-256 mismatch"));
    }
    let document = serde_json::from_slice(&document_bytes)
        .map_err(|_| common::invalid("Schema 3 project document is invalid"))?;
    historical_visual_package::validate_export_document(&document)?;
    if document.base.id != manifest.source_project_id {
        return Err(common::invalid("Project identity differs from manifest"));
    }
    let fonts_missing =
        opencut_media::missing_system_font_families(&manifest.dependencies.fonts_not_bundled);
    let font_environment_checked = true;
    let tool_lock_mismatches = match probe().and_then(|backend| {
        backend
            .tool_fingerprints()
            .map_err(|e| EditorError::new(e.code(), e.to_string()))
    }) {
        Ok(_) if manifest.schema_version == 2 => manifest.dependencies.tools_not_locked.clone(),
        Ok(actual) => manifest
            .tool_locks
            .iter()
            .filter(|expected| {
                !actual.iter().any(|found| {
                    found.name == expected.name
                        && found.version == expected.version
                        && found.binary_sha256 == expected.binary_sha256
                })
            })
            .map(|expected| expected.name.clone())
            .collect::<Vec<_>>(),
        Err(_) => manifest
            .tool_locks
            .iter()
            .map(|expected| expected.name.clone())
            .chain(manifest.dependencies.tools_not_locked.iter().cloned())
            .collect(),
    };
    let tool_environment_matches =
        matches!(manifest.schema_version, 3..=5) && tool_lock_mismatches.is_empty();
    let prepared = PreparedHistoricalVisual {
        manifest_sha256,
        manifest,
        document,
        font_environment_checked,
        fonts_missing,
        tool_environment_matches,
        tool_lock_mismatches,
        plugin_package_install_plan,
    };
    historical_visual_package::prepare_import(&prepared, &input.target_project_id)?;
    common::capture_media_mode(
        root,
        &common_manifest(&prepared.manifest),
        &prepared.document.base,
        asset_store_root,
        &plugin_bundle_paths,
        capture,
    )?;
    Ok(prepared)
}

fn plugin_package_install_plan(
    root: &Path,
    manifest: &HistoricalVisualManifest,
) -> Result<Vec<PluginGrantRestoreAction>, EditorError> {
    manifest
        .plugin_bundles
        .iter()
        .map(|bundle| {
            let path = common::relative(root, &bundle.path)?;
            let package_root = path
                .into_os_string()
                .into_string()
                .map_err(|_| common::invalid("Plugin bundle path is not valid UTF-8"))?;
            Ok(PluginGrantRestoreAction::Install(
                opencut_api_contracts::plugin_packages::PinnedPluginPackage {
                    package_root,
                    granted_permissions: match bundle.kind {
                        PackagePluginKind::PlannerV1 => {
                            bundle.proposed_global_permissions.iter().cloned().collect()
                        }
                        PackagePluginKind::PresetV2 => Default::default(),
                    },
                    expected_package_sha256: bundle.package_sha256.clone(),
                },
            ))
        })
        .collect()
}

fn verify_plugin_bundles(
    root: &Path,
    manifest: &HistoricalVisualManifest,
) -> Result<Vec<String>, EditorError> {
    let mut total = 0_u64;
    let mut paths = Vec::with_capacity(manifest.plugin_bundles.len());
    for bundle in &manifest.plugin_bundles {
        let package_root = common::relative(root, &bundle.path)?;
        if !fs::symlink_metadata(&package_root)
            .map_err(common::io)?
            .file_type()
            .is_dir()
        {
            return Err(common::invalid("Plugin bundle root is not a directory"));
        }
        let (id, version, package_sha256, bytes) = match bundle.kind {
            PackagePluginKind::PlannerV1 => {
                let (plugin_manifest, hash) =
                    opencut_plugin_host::PluginHost::verify_package_root(&package_root).map_err(
                        |_| common::invalid("Bundled planner plugin package failed verification"),
                    )?;
                if plugin_manifest.id != bundle.id
                    || plugin_manifest.version != bundle.version
                    || hash != bundle.package_sha256
                    || bundle
                        .proposed_global_permissions
                        .iter()
                        .any(|permission| !plugin_manifest.permissions.contains(permission))
                {
                    return Err(common::invalid(
                        "Planner plugin bundle identity or permission snapshot differs",
                    ));
                }
                (
                    plugin_manifest.id,
                    plugin_manifest.version,
                    hash,
                    bounded_tree_size(&package_root)?,
                )
            }
            PackagePluginKind::PresetV2 => {
                let (plugin_manifest, hash) =
                    crate::presets::PresetPackages::verify_package_root(&package_root).map_err(
                        |_| common::invalid("Bundled preset plugin package failed verification"),
                    )?;
                if plugin_manifest.id != bundle.id
                    || plugin_manifest.version != bundle.version
                    || hash != bundle.package_sha256
                {
                    return Err(common::invalid("Preset plugin bundle identity differs"));
                }
                (
                    plugin_manifest.id,
                    plugin_manifest.version,
                    hash,
                    bounded_tree_size(&package_root)?,
                )
            }
        };
        if id != bundle.id || version != bundle.version || package_sha256 != bundle.package_sha256 {
            return Err(common::invalid("Plugin bundle identity differs"));
        }
        total = total
            .checked_add(bytes)
            .ok_or_else(|| common::invalid("Plugin bundle size overflow"))?;
        if total > MAX_PLUGIN_BUNDLE_BYTES {
            return Err(common::invalid("Plugin bundles exceed package size limit"));
        }
        paths.push(bundle.path.clone());
    }
    Ok(paths)
}

fn bounded_tree_size(root: &Path) -> Result<u64, EditorError> {
    let mut total = 0_u64;
    for entry in fs::read_dir(root).map_err(common::io)? {
        let entry = entry.map_err(common::io)?;
        let ty = entry.file_type().map_err(common::io)?;
        if ty.is_dir() {
            total = total
                .checked_add(bounded_tree_size(&entry.path())?)
                .ok_or_else(|| common::invalid("Plugin bundle size overflow"))?;
        } else if ty.is_file() {
            total = total
                .checked_add(entry.metadata().map_err(common::io)?.len())
                .ok_or_else(|| common::invalid("Plugin bundle size overflow"))?;
        } else {
            return Err(common::invalid(
                "Plugin bundle entries must be regular files or directories",
            ));
        }
        if total > MAX_PLUGIN_BUNDLE_BYTES {
            return Err(common::invalid("Plugin bundles exceed package size limit"));
        }
    }
    Ok(total)
}
