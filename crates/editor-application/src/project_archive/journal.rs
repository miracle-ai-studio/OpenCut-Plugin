use super::*;
use opencut_contracts::{
    TransactionRequest,
    branching::BranchJournal,
    brand_resources::ImportBrandResource,
    catalog_resources::{GenerateCatalogSound, ImportCatalogResource},
    font_import::ImportFont,
    historical_visual_ops::ApplyHistoricalVisualProject,
    project_history::CausalJournal,
    project_public::{ApplyProject, AssetSelector, FontSelector, ProjectEditOperation},
    scene_ops::{ApplySceneProject, PromoteProject, RestoreProject},
    subtitles::{GenerateCaptions, SubtitleImport},
};
mod historical;
pub use historical::{ArchivedHistoricalCausal, ArchivedVisualApply, ArchivedVisualPromotion};
mod branches;
pub use branches::{ArchivedBranchApply, ArchivedBranchMerge};

/// The archive envelope identifies a known original request shape. Storage is
/// restored to that original shape; no arbitrary JSON is rewritten or executed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ArchivedJournal {
    HistoricalBranchApply(ArchivedBranchApply),
    HistoricalBranchMerge(ArchivedBranchMerge),
    HistoricalCausal(ArchivedHistoricalCausal),
    HistoricalVisualPromote(ArchivedVisualPromotion),
    LegacyApply(TransactionRequest),
    LegacyRestore(opencut_api_contracts::RestoreRevisionRequest),
    HistoricalVisualApply(ArchivedVisualApply),
    Promote(PromoteProject),
    Apply(ApplySceneProject),
    ApplyPublic(ApplyProject),
    BranchMerge(BranchJournal),
    Restore(RestoreProject),
    Causal(CausalJournal),
    /// Static and derived imports have exactly the same frozen request shape.
    FontImport(ImportFont),
    CaptionGenerate(GenerateCaptions),
    CaptionImport(SubtitleImport),
    CatalogImport(ImportCatalogResource),
    CatalogGenerate(GenerateCatalogSound),
    BrandImport(ArchivedBrandImport),
}

/// An exact stored brand journal, including its original marker when present.
/// New requests have no marker. Retaining it here preserves historical archive
/// journals without exposing a second acquisition API or changing receipt hashes.
#[derive(Clone, Debug)]
pub struct ArchivedBrandImport {
    request: ImportBrandResource,
    original_marker: bool,
}
impl ArchivedBrandImport {
    pub fn request(&self) -> &ImportBrandResource {
        &self.request
    }
}
impl Serialize for ArchivedBrandImport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.original_marker {
            crate::brand_resources::original_request(&self.request).serialize(serializer)
        } else {
            self.request.serialize(serializer)
        }
    }
}
impl<'de> Deserialize<'de> for ArchivedBrandImport {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let original_marker = match value
            .as_object_mut()
            .and_then(|v| v.remove("schema_version"))
        {
            None => false,
            Some(serde_json::Value::Number(n)) if n.as_u64() == Some(1) => true,
            Some(_) => {
                return Err(serde::de::Error::custom(
                    "Invalid original brand journal marker",
                ));
            }
        };
        let request = serde_json::from_value::<ImportBrandResource>(value)
            .map_err(serde::de::Error::custom)?;
        if !request.catalog_id.starts_with("simple-icons:") {
            return Err(serde::de::Error::custom(
                "Brand journal requires its fixed catalog identity",
            ));
        }
        Ok(Self {
            request,
            original_marker,
        })
    }
}
impl ArchivedJournal {
    pub fn from_storage(raw: &str) -> Result<Self, EditorError> {
        if raw.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("Historical journal exceeds document limit"));
        }
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| invalid("Malformed historical journal"))?;
        macro_rules! parse {
            ($ty:ty,$variant:ident) => {
                if let Ok(request) = serde_json::from_value::<$ty>(value.clone()) {
                    return Ok(Self::$variant(request));
                }
            };
        }
        if value.get("kind").and_then(serde_json::Value::as_str) == Some("branch_merge") {
            if value.get("request").is_none() {
                return serde_json::from_value(value)
                    .map(Self::HistoricalBranchMerge)
                    .map_err(|_| invalid("Malformed original branch merge"));
            }
            return serde_json::from_value(value)
                .map(Self::BranchMerge)
                .map_err(|_| invalid("Malformed branch merge journal"));
        }
        match value.get("kind").and_then(serde_json::Value::as_str) {
            Some("branch_apply" | "project_v3.branch_apply") => {
                return serde_json::from_value(value)
                    .map(Self::HistoricalBranchApply)
                    .map_err(|_| invalid("Malformed original branch edit"));
            }
            Some("project_v3.branch_merge") => {
                return serde_json::from_value(value)
                    .map(Self::HistoricalBranchMerge)
                    .map_err(|_| invalid("Malformed original visual merge"));
            }
            Some("selective_undo" | "selective_redo") => {
                return serde_json::from_value(value)
                    .map(Self::HistoricalCausal)
                    .map_err(|_| invalid("Malformed original causal journal"));
            }
            Some("project_v3.promote") => {
                return serde_json::from_value(value)
                    .map(Self::HistoricalVisualPromote)
                    .map_err(|_| invalid("Malformed original visual promotion"));
            }
            Some("project_v3.apply") => {
                return serde_json::from_value(value)
                    .map(Self::HistoricalVisualApply)
                    .map_err(|_| invalid("Malformed original visual edit"));
            }
            _ => {}
        }
        parse!(CausalJournal, Causal);
        parse!(ApplySceneProject, Apply);
        parse!(ApplyProject, ApplyPublic);
        parse!(RestoreProject, Restore);
        parse!(opencut_api_contracts::RestoreRevisionRequest, LegacyRestore);
        parse!(PromoteProject, Promote);
        parse!(ArchivedVisualApply, HistoricalVisualApply);
        parse!(TransactionRequest, LegacyApply);
        parse!(ImportFont, FontImport);
        parse!(GenerateCaptions, CaptionGenerate);
        parse!(SubtitleImport, CaptionImport);
        // Brand and flag acquisitions share a current field shape. Select by
        // the fixed source identity before decoding; never try one acquisition
        // planner and fall back to another after a validation failure.
        if value
            .get("catalog_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id.starts_with("simple-icons:"))
        {
            return serde_json::from_value(value)
                .map(Self::BrandImport)
                .map_err(|_| invalid("Malformed brand acquisition journal"));
        }
        parse!(ImportCatalogResource, CatalogImport);
        parse!(GenerateCatalogSound, CatalogGenerate);
        Err(unsupported(
            "History package requires known transaction journals; no unknown journal is omitted",
        ))
    }
    pub fn storage_json(&self) -> Result<String, EditorError> {
        macro_rules! encode {
            ($r:expr) => {
                serde_json::to_string($r)
                    .map_err(|_| invalid("Cannot serialize original historical request"))
            };
        }
        match self {
            Self::HistoricalBranchApply(r) => encode!(r),
            Self::HistoricalBranchMerge(r) => encode!(r),
            Self::HistoricalCausal(r) => encode!(r),
            Self::HistoricalVisualPromote(r) => encode!(r),
            Self::LegacyApply(r) => encode!(r),
            Self::LegacyRestore(r) => encode!(r),
            Self::HistoricalVisualApply(r) => encode!(r),
            Self::Promote(r) => encode!(r),
            Self::Apply(r) => encode!(r),
            Self::ApplyPublic(r) => encode!(r),
            Self::BranchMerge(r) => encode!(r),
            Self::Restore(r) => encode!(r),
            Self::Causal(r) => encode!(r),
            Self::FontImport(r) => encode!(r),
            Self::CaptionGenerate(r) => encode!(r),
            Self::CaptionImport(r) => encode!(r),
            Self::CatalogImport(r) => encode!(r),
            Self::CatalogGenerate(r) => encode!(r),
            Self::BrandImport(r) => encode!(r),
        }
    }
    fn scope<'a>(&'a self, project: &'a str, base: &'a str) -> (&'a str, &'a str, Option<&'a str>) {
        macro_rules! scope {
            ($r:expr) => {
                (
                    &$r.project_id,
                    &$r.expected_revision,
                    Some($r.idempotency_key.as_str()),
                )
            };
        }
        match self {
            Self::HistoricalBranchApply(r) => r.scope(project),
            Self::HistoricalBranchMerge(r) => r.scope(project, base),
            Self::HistoricalCausal(r) => {
                let (p, b, key) = r.scope(project, base);
                (p, b, Some(key))
            }
            Self::HistoricalVisualPromote(r) => {
                let (p, b, key) = r.scope();
                (p, b, Some(key))
            }
            Self::LegacyApply(r) => (project, &r.expected_revision, Some(&r.idempotency_key)),
            Self::LegacyRestore(r) => (project, &r.expected_revision, Some(&r.idempotency_key)),
            Self::HistoricalVisualApply(r) => scope!(r),
            Self::Promote(r) => scope!(r),
            Self::Apply(r) => scope!(r),
            Self::ApplyPublic(r) => scope!(r),
            Self::BranchMerge(BranchJournal::BranchMerge { request, .. }) => (
                &request.project_id,
                &request.expected_target_revision,
                Some(&request.idempotency_key),
            ),
            Self::Restore(r) => scope!(r),
            Self::Causal(CausalJournal::Undo { request, .. }) => scope!(request),
            Self::Causal(CausalJournal::Redo { request, .. }) => scope!(request),
            Self::FontImport(r) => scope!(r),
            Self::CaptionGenerate(r) => scope!(r),
            Self::CaptionImport(r) => scope!(r),
            Self::CatalogImport(r) => scope!(r),
            Self::CatalogGenerate(r) => scope!(r),
            Self::BrandImport(r) => scope!(r.request),
        }
    }
    pub(super) fn validate_original_effect(
        &self,
        base: &str,
        after: &ArchivedDocument,
        revisions: &BTreeMap<&str, &ArchivedRevision>,
        transactions: &BTreeMap<&str, &ArchivedTransaction>,
        merges: &BTreeMap<&str, &str>,
        newest: &[&str],
    ) -> Result<(), EditorError> {
        let before = &revisions[base].document;
        let same = match self {
            Self::HistoricalBranchApply(j) => return j.validate_effect(before, after),
            Self::HistoricalBranchMerge(j) => {
                return j.validate_effect(
                    before.project_id(),
                    base,
                    after,
                    revisions,
                    merges,
                    newest,
                );
            }
            Self::HistoricalCausal(j) => {
                return j.validate_effect(base, after, revisions, transactions);
            }
            Self::HistoricalVisualPromote(j) => return j.validate_effect(before, after),
            Self::LegacyApply(request) => {
                let (ArchivedDocument::Legacy(before), ArchivedDocument::Legacy(after)) =
                    (before, after)
                else {
                    return Err(invalid("Historical base edit changed document shape"));
                };
                let plan = opencut_editor_core::plan_transaction(
                    &opencut_contracts::ProjectSnapshot {
                        project: before.clone(),
                        revision: request.expected_revision.clone(),
                    },
                    request,
                )?;
                plan.proposed_project == *after
            }
            Self::LegacyRestore(request) => {
                let (
                    ArchivedDocument::Legacy(_),
                    ArchivedDocument::Legacy(after),
                    ArchivedDocument::Legacy(target),
                ) = (
                    before,
                    after,
                    &revisions[request.target_revision.as_str()].document,
                )
                else {
                    return Err(invalid("Original base restore changed its document shape"));
                };
                after == target
            }
            Self::HistoricalVisualApply(request) => {
                let (
                    ArchivedDocument::HistoricalVisual(before),
                    ArchivedDocument::HistoricalVisual(after),
                ) = (before, after)
                else {
                    return Err(invalid("Historical visual edit changed document shape"));
                };
                crate::historical_visual::plan_apply(before, request)?.proposed_project == *after
            }
            Self::Promote(request) => {
                if matches!(before, ArchivedDocument::Editable(_)) {
                    return Err(invalid(
                        "Original promotion cannot start from an editable document",
                    ));
                }
                before.converted(&request.asset_versions)? == *after.editable()?
            }
            Self::Restore(request) => {
                if matches!(before, ArchivedDocument::ReadOnly(_)) {
                    return Err(invalid(
                        "Preserved plugin content cannot be implicitly restored over",
                    ));
                }
                revisions[request.target_revision.as_str()]
                    .document
                    .converted(&request.asset_versions)?
                    == *after.editable()?
            }
            Self::Causal(_) => {
                before.editable()?;
                after.editable()?;
                return Ok(());
            }
            _ => return Ok(()),
        };
        if !same {
            return Err(invalid(
                "Historical journal differs from its exact original effect",
            ));
        }
        Ok(())
    }
    pub(super) fn transaction_references(&self) -> Vec<&str> {
        match self {
            Self::HistoricalCausal(j) => match j.undo() {
                Some(undo) => vec![undo, j.original()],
                None => vec![j.original()],
            },
            Self::Causal(CausalJournal::Undo { request, .. }) => {
                vec![&request.target_transaction_id]
            }
            Self::Causal(CausalJournal::Redo {
                request,
                target_transaction_id,
                ..
            }) => vec![&request.undo_transaction_id, target_transaction_id],
            _ => vec![],
        }
    }
    fn causal_reference(&self) -> Option<(&str, Option<&str>)> {
        match self {
            Self::HistoricalCausal(j) => Some((j.original(), j.undo())),
            Self::Causal(CausalJournal::Undo { request, .. }) => {
                Some((&request.target_transaction_id, None))
            }
            Self::Causal(CausalJournal::Redo {
                request,
                target_transaction_id,
                ..
            }) => Some((target_transaction_id, Some(&request.undo_transaction_id))),
            _ => None,
        }
    }
    pub(super) fn is_causal(&self) -> bool {
        self.causal_reference().is_some()
    }
    pub(super) fn needs_source_order(&self) -> bool {
        matches!(
            self,
            Self::HistoricalCausal(_)
                | Self::HistoricalBranchApply(_)
                | Self::HistoricalBranchMerge(_)
        )
    }
    pub(super) fn merge_source(&self) -> Option<&str> {
        match self {
            Self::HistoricalBranchMerge(j) => Some(j.references().1),
            Self::BranchMerge(BranchJournal::BranchMerge { request, .. }) => {
                Some(&request.expected_source_revision)
            }
            _ => None,
        }
    }
    fn validate_causal(
        &self,
        base: &str,
        revisions: &BTreeMap<&str, &ArchivedRevision>,
        transactions: &BTreeMap<&str, &ArchivedTransaction>,
        merges: &BTreeMap<&str, &str>,
        ordered: &[&ArchivedTransaction],
        source_order: Option<u64>,
        traversal_left: &mut usize,
    ) -> Result<(), EditorError> {
        let Some((original, undo)) = self.causal_reference() else {
            return Ok(());
        };
        if transactions
            .get(original)
            .is_none_or(|t| t.journal.is_causal())
        {
            return Err(invalid(
                "Causal journal has no ordinary original transaction",
            ));
        }
        if let Some(undo) = undo {
            if transactions
                .get(undo)
                .and_then(|t| t.journal.causal_reference())
                != Some((original, None))
            {
                return Err(invalid("Redo does not reference its original undo"));
            }
        }
        if matches!(self, Self::HistoricalCausal(_)) {
            let order = source_order.ok_or_else(|| invalid("Original causal order is missing"))?;
            for reference in std::iter::once(original).chain(undo) {
                let previous = transactions[reference]
                    .source_order
                    .ok_or_else(|| invalid("Original causal reference order is missing"))?;
                if previous >= order {
                    return Err(invalid(
                        "Original causal reference was not published before the operation",
                    ));
                }
            }
            let mut ancestors = BTreeSet::new();
            let mut pending = vec![base];
            while let Some(id) = pending.pop() {
                if !ancestors.insert(id) {
                    continue;
                }
                *traversal_left = traversal_left.checked_sub(1).ok_or_else(|| {
                    EditorError::new(
                        "RESOURCE_TOO_LARGE",
                        "Causal history exceeds bounded traversal budget",
                    )
                })?;
                if let Some(parent) = revisions[id].parent_revision.as_deref() {
                    pending.push(parent);
                }
                if let Some(parent) = merges.get(id) {
                    pending.push(*parent);
                }
            }
            let mut active = None;
            for transaction in ordered {
                let observed = transaction
                    .source_order
                    .ok_or_else(|| invalid("Original transaction order is incomplete"))?;
                if observed >= order {
                    break;
                }
                if !ancestors.contains(transaction.revision.as_str()) {
                    continue;
                }
                if let Self::HistoricalCausal(record) = &transaction.journal {
                    if record.original() != original {
                        continue;
                    }
                    match record.undo() {
                        None => active = Some(transaction.transaction_id.as_str()),
                        Some(id) if active == Some(id) => active = None,
                        _ => {}
                    }
                }
            }
            if undo != active {
                return Err(invalid(
                    "Original causal state differs from observed transaction order and full ancestry",
                ));
            }
            return Ok(());
        }
        // One bounded first-parent causal walk covers the actual stored shapes.
        // Historical records are not projected into current requests.
        let mut cursor = Some(base);
        let mut active = None;
        while let Some(id) = cursor {
            *traversal_left = traversal_left.checked_sub(1).ok_or_else(|| {
                EditorError::new(
                    "RESOURCE_TOO_LARGE",
                    "Causal history exceeds bounded traversal budget",
                )
            })?;
            let revision = revisions[id];
            if let Some(id) = &revision.transaction_id {
                match transactions[id.as_str()].journal.causal_reference() {
                    Some((target, None)) if target == original => {
                        active = Some(id.as_str());
                        break;
                    }
                    Some((target, Some(_))) if target == original => break,
                    _ => {}
                }
            }
            cursor = revision.parent_revision.as_deref();
        }
        if undo != active {
            return Err(invalid(if undo.is_some() {
                "Redo does not match the active undo"
            } else {
                "Original transaction is already undone"
            }));
        }
        Ok(())
    }
    pub(super) fn revision_references(&self) -> Vec<&str> {
        match self {
            Self::HistoricalBranchMerge(j) => {
                let (base, source) = j.references();
                vec![base, source]
            }
            Self::LegacyRestore(r) => vec![r.target_revision.as_str()],
            Self::Restore(r) => vec![r.target_revision.as_str()],
            Self::BranchMerge(BranchJournal::BranchMerge {
                request,
                base_revision,
                ..
            }) => vec![
                base_revision,
                &request.expected_source_revision,
                &request.expected_target_revision,
            ],
            Self::ApplyPublic(r) => r
                .operations
                .iter()
                .filter_map(|op| match op {
                    ProjectEditOperation::AssetBind {
                        source: AssetSelector::ProjectAsset { revision, .. },
                        ..
                    } => Some(revision.as_str()),
                    ProjectEditOperation::FontBind {
                        source: FontSelector::ProjectFont { revision, .. },
                        ..
                    } => Some(revision.as_str()),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }
    pub(super) fn validate(
        &self,
        project: &str,
        base: &str,
        revisions: &BTreeMap<&str, &ArchivedRevision>,
        transactions: &BTreeMap<&str, &ArchivedTransaction>,
        merges: &BTreeMap<&str, &str>,
        ordered: &[&ArchivedTransaction],
        source_order: Option<u64>,
        traversal_left: &mut usize,
    ) -> Result<(), EditorError> {
        let (p, b, key) = self.scope(project, base);
        if p != project || b != base || key.is_some_and(|key| !valid_id(key)) {
            return Err(invalid(
                "Historical request differs from its scoped transaction",
            ));
        }
        let require_revision = |id: &str| -> Result<(), EditorError> {
            if revisions.contains_key(id) {
                Ok(())
            } else {
                Err(invalid(
                    "Historical request references a revision outside the package",
                ))
            }
        };
        match self {
            Self::HistoricalBranchApply(j) => j.validate()?,
            Self::HistoricalBranchMerge(j) => {
                j.validate()?;
                let (parent, source) = j.references();
                require_revision(parent)?;
                require_revision(source)?;
            }
            Self::HistoricalCausal(j) => j.validate()?,
            Self::BrandImport(r) => crate::brand_resources::validate_request(&r.request)?,
            Self::BranchMerge(BranchJournal::BranchMerge {
                request,
                base_revision,
                changes,
            }) => {
                crate::branching::validate_compare(&crate::branching::comparison(request))?;
                require_revision(base_revision)?;
                require_revision(&request.expected_source_revision)?;
                if changes.is_empty()
                    || changes.iter().any(|change| {
                        change
                            .before_sha256
                            .as_deref()
                            .is_some_and(|hash| !valid_hash(hash))
                            || change
                                .after_sha256
                                .as_deref()
                                .is_some_and(|hash| !valid_hash(hash))
                    })
                {
                    return Err(invalid("Branch merge change digests are invalid"));
                }
            }
            Self::Restore(r) => {
                require_revision(&r.target_revision)?;
            }
            Self::LegacyRestore(r) => require_revision(&r.target_revision)?,
            Self::ApplyPublic(r) => {
                for op in &r.operations {
                    match op {
                        ProjectEditOperation::AssetBind {
                            source: AssetSelector::ProjectAsset { revision, .. },
                            ..
                        } => require_revision(revision)?,
                        ProjectEditOperation::FontBind {
                            source: FontSelector::ProjectFont { revision, .. },
                            ..
                        } => require_revision(revision)?,
                        _ => {}
                    }
                }
            }
            Self::Causal(
                CausalJournal::Undo { schema_version, .. }
                | CausalJournal::Redo { schema_version, .. },
            ) if *schema_version != 4 => {
                return Err(invalid("Invalid current causal document format"));
            }
            _ => {}
        }
        self.validate_causal(
            base,
            revisions,
            transactions,
            merges,
            ordered,
            source_order,
            traversal_left,
        )
    }
    pub(super) fn remap(
        &mut self,
        project: &str,
        revisions: &BTreeMap<String, String>,
        transactions: &BTreeMap<String, String>,
    ) -> Result<(), EditorError> {
        let revision = |id: &mut String| -> Result<(), EditorError> {
            *id = revisions
                .get(id)
                .ok_or_else(|| invalid("Cannot map historical revision"))?
                .clone();
            Ok(())
        };
        let transaction = |id: &mut String| -> Result<(), EditorError> {
            *id = transactions
                .get(id)
                .ok_or_else(|| invalid("Cannot map historical transaction"))?
                .clone();
            Ok(())
        };
        macro_rules! scope {
            ($r:expr) => {{
                $r.project_id = project.into();
                revision(&mut $r.expected_revision)?;
            }};
        }
        match self {
            Self::HistoricalBranchApply(r) => r.remap(project, revisions)?,
            Self::HistoricalBranchMerge(r) => r.remap(revisions)?,
            Self::HistoricalCausal(r) => r.remap(project, revisions, transactions)?,
            Self::HistoricalVisualPromote(r) => r.remap(project, revisions)?,
            Self::LegacyApply(r) => revision(&mut r.expected_revision)?,
            Self::LegacyRestore(r) => {
                revision(&mut r.expected_revision)?;
                revision(&mut r.target_revision)?;
            }
            Self::HistoricalVisualApply(r) => scope!(r),
            Self::Promote(r) => scope!(r),
            Self::Apply(r) => scope!(r),
            Self::ApplyPublic(r) => {
                scope!(r);
                for op in &mut r.operations {
                    match op {
                        ProjectEditOperation::AssetBind {
                            source: AssetSelector::ProjectAsset { revision: id, .. },
                            ..
                        } => revision(id)?,
                        ProjectEditOperation::FontBind {
                            source: FontSelector::ProjectFont { revision: id, .. },
                            ..
                        } => revision(id)?,
                        _ => {}
                    }
                }
            }
            Self::Restore(r) => {
                scope!(r);
                revision(&mut r.target_revision)?;
            }
            Self::BranchMerge(BranchJournal::BranchMerge {
                request,
                base_revision,
                ..
            }) => {
                request.project_id = project.into();
                revision(&mut request.expected_source_revision)?;
                revision(&mut request.expected_target_revision)?;
                revision(base_revision)?;
            }
            Self::Causal(CausalJournal::Undo { request, .. }) => {
                scope!(request);
                transaction(&mut request.target_transaction_id)?;
            }
            Self::Causal(CausalJournal::Redo {
                request,
                target_transaction_id,
                ..
            }) => {
                scope!(request);
                transaction(&mut request.undo_transaction_id)?;
                transaction(target_transaction_id)?;
            }
            Self::FontImport(r) => scope!(r),
            Self::CaptionGenerate(r) => scope!(r),
            Self::CaptionImport(r) => scope!(r),
            Self::CatalogImport(r) => scope!(r),
            Self::CatalogGenerate(r) => scope!(r),
            Self::BrandImport(r) => scope!(r.request),
        }
        Ok(())
    }
}
