import type { ToolInput } from "../../../../lib/editor-sdk";
type Scope = { project_id: string; expected_revision: string; idempotency_key: string; track: { track_id: string } };
type Plan = {
  revision: string;
  cues: { layer: { elementId: string } }[];
  transaction?: ToolInput<"opencut_edit_apply"> | null;
  selection?: { element_id: string; track_id: string } | null;
  measurement?: { worker_sha256: string; rasterizer: string } | null;
};
export function verifyNativeSubtitlePlan(plan: Plan, request: Scope, empty = false) {
  if (plan.revision !== request.expected_revision || new Set(plan.cues.map(c => c.layer.elementId)).size !== plan.cues.length)
    throw Error("The subtitle plan differs from the fixed revision.");
  if (empty && !plan.cues.length) {
    if (plan.transaction || plan.selection || plan.measurement) throw Error("Unresolved subtitles cannot contain an edit or measurement proof.");
    return;
  }
  if (!plan.cues.length || !plan.transaction || !plan.selection || !plan.measurement ||
      !/^[0-9a-f]{64}$/.test(plan.measurement.worker_sha256) || !plan.measurement.rasterizer ||
      plan.transaction.project_id !== request.project_id || plan.transaction.expected_revision !== request.expected_revision ||
      plan.transaction.idempotency_key !== request.idempotency_key || plan.selection.track_id !== request.track.track_id ||
      plan.selection.element_id !== plan.cues[0].layer.elementId)
    throw Error("The native subtitle plan omitted its fixed edit or measurement proof.");
}
