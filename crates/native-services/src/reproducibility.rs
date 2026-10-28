//! Read-only package observation behind the application verification port.
use crate::project_package as common;
use opencut_api_contracts::{
    historical_visual_package::{HistoricalVisualManifest, PackageToolLock},
    reproducibility::{ExecutionIdentity, InspectPackageReproducibility},
};
use opencut_contracts::{EditorError, historical_visual::HistoricalVisualDocument};
use opencut_editor_application::reproducibility::{PackageObservation, ReproducibilityProbe};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

pub struct NativeReproducibility(pub fn() -> Result<opencut_media::FfmpegBackend, EditorError>);
impl ReproducibilityProbe for NativeReproducibility {
    fn observe(
        &self,
        input: &InspectPackageReproducibility,
    ) -> Result<PackageObservation, EditorError> {
        let root = Path::new(&input.package_path);
        if !root.is_absolute() || fs::canonicalize(root).map_err(common::io)? != root {
            return Err(common::invalid(
                "Package path must be a canonical absolute directory",
            ));
        }
        let bytes = common::bounded_bytes(
            &common::relative(root, "manifest.json")?,
            common::MAX_MANIFEST_BYTES,
        )?;
        let manifest_sha256 = common::digest(&bytes);
        if manifest_sha256 != input.expected_manifest_sha256 {
            return Err(common::invalid("Package manifest SHA-256 mismatch"));
        }
        let manifest: HistoricalVisualManifest = serde_json::from_slice(&bytes)
            .map_err(|_| common::invalid("Invalid legacy package manifest"))?;
        opencut_editor_application::historical_visual_package::validate_manifest(&manifest)?;
        let project = common::bounded_bytes(
            &common::relative(root, &manifest.project_path)?,
            common::MAX_DOCUMENT_BYTES,
        )?;
        if common::digest(&project) != manifest.project_sha256 {
            return Err(common::invalid("Project document SHA-256 mismatch"));
        }
        let document: HistoricalVisualDocument = serde_json::from_slice(&project)
            .map_err(|_| common::invalid("Invalid project document"))?;
        opencut_editor_application::historical_visual_package::validate_export_document(&document)?;
        if document.base.id != manifest.source_project_id {
            return Err(common::invalid("Project identity differs from manifest"));
        }
        let backend =
            (self.0)().map_err(|e| EditorError::new("PACKAGE_TOOL_UNAVAILABLE", e.to_string()))?;
        let native_tools = backend
            .tool_fingerprints()
            .map_err(|e| EditorError::new("PACKAGE_TOOL_UNAVAILABLE", e.to_string()))?
            .into_iter()
            .map(|tool| PackageToolLock {
                name: tool.name,
                version: tool.version,
                binary_sha256: tool.binary_sha256,
            })
            .collect::<Vec<_>>();
        let native_tools_match =
            manifest.schema_version >= 3 && manifest.tool_locks == native_tools;
        Ok(PackageObservation {
            manifest_sha256,
            source_revision: manifest.source_revision,
            package_schema_version: manifest.schema_version,
            legacy_claim: manifest.fully_reproducible,
            media_self_contained_declared: manifest.self_contained_media,
            native_tools_match,
            current_execution: execution_identity(native_tools)?,
        })
    }
}

/// Capture the host actually executing an attempt; never backfill old packages.
pub fn execution_identity(
    native_tools: Vec<PackageToolLock>,
) -> Result<ExecutionIdentity, EditorError> {
    let mut executable =
        fs::File::open(std::env::current_exe().map_err(common::io)?).map_err(common::io)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let read = executable.read(&mut buffer).map_err(common::io)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(ExecutionIdentity {
        process_binary_sha256: format!("{:x}", digest.finalize()),
        platform: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        native_tools,
    })
}
