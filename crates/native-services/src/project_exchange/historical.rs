//! Exact old file readers. Actual manifest content selects once; a parse error
//! is never retried as another format. No retired producer is exposed.
use super::*;
pub use opencut_editor_application::project_exchange::PreparedImport;

fn classify(
    path: &str,
    expected: Option<&str>,
) -> Result<(String, String, Option<u64>), EditorError> {
    let canonical = package_io::root(path)?;
    let root = PackageDirectory::open(&canonical)?;
    let bytes = package_io::bounded(
        root.child("manifest.json", false)?,
        history::MAX_MANIFEST_BYTES,
    )?;
    let hash = app::digest(&bytes);
    if expected.is_some_and(|h| h != hash) {
        return Err(app::invalid("Package manifest SHA-256 mismatch"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| app::invalid("Invalid package manifest JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| app::invalid("Invalid package manifest"))?;
    let tag = object
        .get("schema_version")
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| app::invalid("Invalid historical package identity"))
        })
        .transpose()?;
    Ok((
        canonical
            .to_str()
            .ok_or_else(|| app::invalid("Package path is not UTF-8"))?
            .into(),
        hash,
        tag,
    ))
}
pub fn inspect_package(
    input: &InspectPackage,
    probe: &crate::media::ProbeFactory,
) -> Result<PackageInspection, EditorError> {
    let (path, hash, tag) = classify(&input.package_path, None)?;
    let unused = Path::new("");
    let prepared = match tag {
        None => PreparedImport::Current(verified(&path, Some(&hash), probe)?.1),
        Some(1) => PreparedImport::Media(common::read_package_mode(
            &opencut_api_contracts::project_package::ImportPackage {
                package_path: path,
                target_project_id: "package-review".into(),
                expected_manifest_sha256: hash,
                idempotency_key: "package-review".into(),
            },
            unused,
            false,
        )?),
        Some(2..=5) => PreparedImport::Visual(crate::historical_visual_package::read_package_mode(
            &opencut_api_contracts::historical_visual_package::HistoricalVisualImport {
                package_path: path,
                target_project_id: "package-review".into(),
                expected_manifest_sha256: hash,
                idempotency_key: "package-review".into(),
            },
            unused,
            false,
            probe,
        )?),
        Some(6) => PreparedImport::Snapshot(
            crate::historical_snapshot_package::verified_package(&path, Some(&hash))?.1,
        ),
        Some(7) => PreparedImport::History(crate::project_archive::verified(&path, Some(&hash))?.1),
        _ => return Err(app::invalid("Unknown historical package format")),
    };
    let mut value = app::inspect_prepared(&prepared)?;
    value.fonts_missing =
        opencut_media::missing_system_font_families(&value.external_font_families);
    Ok(value)
}

pub fn read_package(
    input: &ImportPackage,
    asset_root: &Path,
    probe: &crate::media::ProbeFactory,
) -> Result<PreparedImport, EditorError> {
    let (path, hash, tag) = classify(&input.package_path, Some(&input.expected_manifest_sha256))?;
    match tag {
        None => Ok(PreparedImport::Current(read_current(
            &ImportPackage {
                package_path: path,
                ..input.clone()
            },
            asset_root,
            probe,
        )?)),
        Some(1) => Ok(PreparedImport::Media(common::read_package(
            &opencut_api_contracts::project_package::ImportPackage {
                package_path: path,
                target_project_id: input.target_project_id.clone(),
                expected_manifest_sha256: hash,
                idempotency_key: input.idempotency_key.clone(),
            },
            asset_root,
        )?)),
        Some(2..=5) => Ok(PreparedImport::Visual(
            crate::historical_visual_package::read_historical_visual(
                &opencut_api_contracts::historical_visual_package::HistoricalVisualImport {
                    package_path: path,
                    target_project_id: input.target_project_id.clone(),
                    expected_manifest_sha256: hash,
                    idempotency_key: input.idempotency_key.clone(),
                },
                asset_root,
                probe,
            )?,
        )),
        Some(6) => Ok(PreparedImport::Snapshot(
            crate::historical_snapshot_package::read_historical_snapshot(
                &opencut_api_contracts::historical_snapshot_package::HistoricalSnapshotImport {
                    package_path: path,
                    target_project_id: input.target_project_id.clone(),
                    expected_manifest_sha256: hash,
                    idempotency_key: input.idempotency_key.clone(),
                },
                asset_root,
            )?,
        )),
        Some(7) => Ok(PreparedImport::History(
            crate::project_archive::read_historical_archive(
                &opencut_api_contracts::project_archive::HistoricalArchiveImport {
                    package_path: path,
                    target_project_id: input.target_project_id.clone(),
                    expected_manifest_sha256: hash,
                    idempotency_key: input.idempotency_key.clone(),
                },
                asset_root,
            )?,
        )),
        _ => Err(app::invalid("Unknown historical package format")),
    }
}
