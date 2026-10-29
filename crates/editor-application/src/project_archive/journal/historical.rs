//! Private exact historical journal readers. No historical write API or grants.
use super::*;
use opencut_contracts::project_history::Direction;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum OriginalApplyKind {
    #[serde(rename = "project_v3.apply")]
    Apply,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalVisualApply {
    kind: OriginalApplyKind,
    request: ApplyHistoricalVisualProject,
}
/// Preserve the original bare request or the exact original writer envelope.
/// The marker describes stored data, never a second edit or execution API.
#[derive(Clone, Debug)]
pub struct ArchivedVisualApply {
    request: ApplyHistoricalVisualProject,
    wrapped: bool,
}
impl From<ApplyHistoricalVisualProject> for ArchivedVisualApply {
    fn from(request: ApplyHistoricalVisualProject) -> Self {
        Self {
            request,
            wrapped: false,
        }
    }
}
impl Serialize for ArchivedVisualApply {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.wrapped {
            serde_json::json!({"kind":"project_v3.apply","request":&self.request})
                .serialize(serializer)
        } else {
            self.request.serialize(serializer)
        }
    }
}
impl<'de> Deserialize<'de> for ArchivedVisualApply {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if value.get("kind").is_some() {
            let OriginalVisualApply {
                kind: OriginalApplyKind::Apply,
                request,
            } = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self {
                request,
                wrapped: true,
            })
        } else {
            serde_json::from_value(value)
                .map(Self::from)
                .map_err(serde::de::Error::custom)
        }
    }
}
impl std::ops::Deref for ArchivedVisualApply {
    type Target = ApplyHistoricalVisualProject;
    fn deref(&self) -> &Self::Target {
        &self.request
    }
}
impl std::ops::DerefMut for ArchivedVisualApply {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.request
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum OriginalDirection {
    #[serde(rename = "selective_undo")]
    Undo,
    #[serde(rename = "selective_redo")]
    Redo,
}

// Option<Value> would collapse a serialized JSON null into None on read. The
// original writer already lost that distinction; compare its exact serialized
// changes against the original kernel, not a fabricated interpretation of null.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OriginalChange {
    path: String,
    before: Value,
    after: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaseCausal {
    kind: OriginalDirection,
    project_id: String,
    expected_revision: String,
    target_transaction_id: String,
    undo_transaction_id: Option<String>,
    idempotency_key: String,
    changes: Vec<OriginalChange>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VisualCausal {
    kind: OriginalDirection,
    schema_version: u32,
    target_transaction_id: String,
    undo_transaction_id: Option<String>,
    idempotency_key: String,
    changes: Vec<OriginalChange>,
}
#[derive(Clone, Debug)]
enum OriginalCausal {
    Base(BaseCausal),
    Visual(VisualCausal),
}
#[derive(Clone, Debug)]
pub struct ArchivedHistoricalCausal(OriginalCausal);
impl Serialize for ArchivedHistoricalCausal {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            OriginalCausal::Base(r) => r.serialize(serializer),
            OriginalCausal::Visual(r) => r.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for ArchivedHistoricalCausal {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if value.get("undo_transaction_id").is_none() {
            return Err(serde::de::Error::custom(
                "Original causal journals include their explicit nullable undo reference",
            ));
        }
        let record = match value.get("schema_version") {
            None => serde_json::from_value(value).map(OriginalCausal::Base),
            Some(n) if n.as_u64() == Some(3) => {
                serde_json::from_value(value).map(OriginalCausal::Visual)
            }
            _ => {
                return Err(serde::de::Error::custom(
                    "Unknown original causal document format",
                ));
            }
        }
        .map_err(serde::de::Error::custom)?;
        let journal = Self(record);
        journal.validate().map_err(serde::de::Error::custom)?;
        Ok(journal)
    }
}
impl ArchivedHistoricalCausal {
    pub(super) fn original(&self) -> &str {
        match &self.0 {
            OriginalCausal::Base(r) => &r.target_transaction_id,
            OriginalCausal::Visual(r) => &r.target_transaction_id,
        }
    }
    pub(super) fn undo(&self) -> Option<&str> {
        match &self.0 {
            OriginalCausal::Base(r) => r.undo_transaction_id.as_deref(),
            OriginalCausal::Visual(r) => r.undo_transaction_id.as_deref(),
        }
    }
    fn direction(&self) -> Direction {
        match match &self.0 {
            OriginalCausal::Base(r) => r.kind,
            OriginalCausal::Visual(r) => r.kind,
        } {
            OriginalDirection::Undo => Direction::Undo,
            OriginalDirection::Redo => Direction::Redo,
        }
    }
    pub(super) fn scope<'a>(
        &'a self,
        project: &'a str,
        base: &'a str,
    ) -> (&'a str, &'a str, &'a str) {
        match &self.0 {
            OriginalCausal::Base(r) => (&r.project_id, &r.expected_revision, &r.idempotency_key),
            OriginalCausal::Visual(r) => (project, base, &r.idempotency_key),
        }
    }
    pub(super) fn validate(&self) -> Result<(), EditorError> {
        if !valid_id(self.original())
            || self.undo().is_some_and(|id| !valid_id(id))
            || (self.direction() == Direction::Redo) != self.undo().is_some()
        {
            return Err(invalid("Invalid original causal references"));
        }
        Ok(())
    }
    pub(super) fn validate_effect(
        &self,
        base: &str,
        after: &ArchivedDocument,
        revisions: &BTreeMap<&str, &ArchivedRevision>,
        transactions: &BTreeMap<&str, &ArchivedTransaction>,
    ) -> Result<(), EditorError> {
        let original = transactions
            .get(self.original())
            .ok_or_else(|| invalid("Missing original historical edit"))?;
        if !matches!(
            &original.journal,
            ArchivedJournal::LegacyApply(_)
                | ArchivedJournal::LegacyRestore(_)
                | ArchivedJournal::HistoricalVisualApply(_)
                | ArchivedJournal::HistoricalBranchApply(_)
                | ArchivedJournal::HistoricalBranchMerge(_)
        ) {
            return Err(invalid(
                "Historical causal target is not an original base/visual edit",
            ));
        }
        let before = &revisions[original.base_revision.as_str()].document;
        let changed = &revisions[original.revision.as_str()].document;
        let (from, to) = match self.direction() {
            Direction::Undo => (changed, before),
            Direction::Redo => (before, changed),
        };
        let head = &revisions[base].document;
        let (expected, actual) = match &self.0 {
            OriginalCausal::Base(record) => {
                let (
                    ArchivedDocument::Legacy(from),
                    ArchivedDocument::Legacy(to),
                    ArchivedDocument::Legacy(head),
                    ArchivedDocument::Legacy(after),
                ) = (from, to, head, after)
                else {
                    return Err(invalid(
                        "Base causal journal changed its original document shape",
                    ));
                };
                let plan = crate::collaboration::plan(
                    self.direction(),
                    base,
                    self.original(),
                    self.undo(),
                    from,
                    to,
                    head,
                );
                if !plan.conflicts.is_empty() || plan.proposed_project.as_ref() != Some(after) {
                    return Err(invalid(
                        "Base causal result differs from the original selective merge",
                    ));
                }
                (
                    serde_json::to_value(&plan.changes),
                    serde_json::to_value(&record.changes),
                )
            }
            OriginalCausal::Visual(record) => {
                let (
                    ArchivedDocument::HistoricalVisual(head),
                    ArchivedDocument::HistoricalVisual(after),
                ) = (head, after)
                else {
                    return Err(invalid(
                        "Visual causal journal changed its original document shape",
                    ));
                };
                let lift = |document: &ArchivedDocument| -> Result<
                    opencut_contracts::historical_visual::HistoricalVisualDocument,
                    EditorError,
                > {
                    match document {
                        ArchivedDocument::HistoricalVisual(p) => Ok(p.clone()),
                        ArchivedDocument::Legacy(p) => {
                            let mut p = p.clone();
                            p.schema_version = 2;
                            crate::historical_visual::promote(&p)
                        }
                        _ => Err(invalid(
                            "Visual causal source cannot be a current or preserved document",
                        )),
                    }
                };
                let from = lift(from)?;
                let to = lift(to)?;
                let plan = crate::historical_visual_collaboration::plan_history(
                    self.direction(),
                    base,
                    self.original(),
                    self.undo(),
                    &from,
                    &to,
                    head,
                );
                if !plan.conflicts.is_empty() || plan.proposed_project.as_ref() != Some(after) {
                    return Err(invalid(
                        "Visual causal result differs from the original selective merge",
                    ));
                }
                (
                    serde_json::to_value(&plan.changes),
                    serde_json::to_value(&record.changes),
                )
            }
        };
        if expected.map_err(|_| invalid("Cannot encode original merge changes"))?
            != actual.map_err(|_| invalid("Cannot encode historical changes"))?
        {
            return Err(invalid(
                "Historical causal changes differ from their original kernel",
            ));
        }
        Ok(())
    }
    pub(super) fn remap(
        &mut self,
        project: &str,
        revisions: &BTreeMap<String, String>,
        transactions: &BTreeMap<String, String>,
    ) -> Result<(), EditorError> {
        let reference =
            |id: &mut String, ids: &BTreeMap<String, String>| -> Result<(), EditorError> {
                *id = ids
                    .get(id)
                    .ok_or_else(|| invalid("Cannot map original causal identity"))?
                    .clone();
                Ok(())
            };
        let (original, undo) = match &mut self.0 {
            OriginalCausal::Base(r) => {
                r.project_id = project.into();
                reference(&mut r.expected_revision, revisions)?;
                (&mut r.target_transaction_id, &mut r.undo_transaction_id)
            }
            OriginalCausal::Visual(r) => (&mut r.target_transaction_id, &mut r.undo_transaction_id),
        };
        reference(original, transactions)?;
        if let Some(id) = undo {
            reference(id, transactions)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum OriginalPromotionKind {
    #[serde(rename = "project_v3.promote")]
    Promote,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalPromotionRequest {
    project_id: String,
    expected_revision: String,
    idempotency_key: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivedVisualPromotion {
    kind: OriginalPromotionKind,
    request: OriginalPromotionRequest,
}
impl ArchivedVisualPromotion {
    pub(super) fn scope(&self) -> (&str, &str, &str) {
        (
            &self.request.project_id,
            &self.request.expected_revision,
            &self.request.idempotency_key,
        )
    }
    pub(super) fn validate_effect(
        &self,
        before: &ArchivedDocument,
        after: &ArchivedDocument,
    ) -> Result<(), EditorError> {
        let (ArchivedDocument::Legacy(before), ArchivedDocument::HistoricalVisual(after)) =
            (before, after)
        else {
            return Err(invalid(
                "Original visual promotion requires its original document shapes",
            ));
        };
        if crate::historical_visual::promote(before)? != *after {
            return Err(invalid(
                "Original visual promotion changed preserved content",
            ));
        }
        Ok(())
    }
    pub(super) fn remap(
        &mut self,
        project: &str,
        revisions: &BTreeMap<String, String>,
    ) -> Result<(), EditorError> {
        self.request.project_id = project.into();
        self.request.expected_revision = revisions
            .get(&self.request.expected_revision)
            .ok_or_else(|| invalid("Cannot map original visual promotion revision"))?
            .clone();
        Ok(())
    }
}
