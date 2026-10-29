pub use opencut_contracts::source_preview::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewPreparation {
    pub jobs: Vec<crate::job_views::JobView>,
}
