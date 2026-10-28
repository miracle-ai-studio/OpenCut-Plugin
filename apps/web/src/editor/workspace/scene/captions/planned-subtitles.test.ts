import { expect, it, vi } from "vitest";
import type { EditorApi } from "../../api";
import { planEditableSubtitles, type SubtitleTextRequest } from "./planned-subtitles";
import { textInitial } from "../text/fixture";
function fixture() {
  const request: SubtitleTextRequest = {
    project_id: "project", expected_revision: "fixed", sequence_id: "scene", idempotency_key: "original-key",
    track: { kind: "new", track_id: "new-track" },
    source: { kind: "transcript", analysis_job_id: "original-asr", words_per_caption: 3 },
    style: { ...textInitial().text, content: "" }, max_width_px: null,
  };
  const result = {
    revision: "fixed", width: 320, height: 180, warnings: [],
    cues: ["cue-one", "cue-two"].map(elementId => ({ layer: { elementId } })),
    transaction: { project_id: "project", expected_revision: "fixed", idempotency_key: "original-key", label: "Editable subtitles", operations: [] },
    selection: { track_id: "new-track", element_id: "cue-one" },
    measurement: { worker_sha256: "a".repeat(64), rasterizer: "native" },
  };
  const call = vi.fn().mockResolvedValue(result);
  return { request, result, call, api: { sdk: { call } } as unknown as EditorApi };
}
it("requests one native plan with the unchanged transcript and transaction identity", async () => {
  const f = fixture(), original = structuredClone(f.request), signal = new AbortController().signal;
  expect(await planEditableSubtitles(f.api, f.request, signal)).toEqual(f.result);
  expect(f.request).toEqual(original);
  expect(f.call).toHaveBeenCalledExactlyOnceWith("opencut_subtitle_text_plan", original, { signal });
  expect(f.call.mock.calls[0][1]).not.toHaveProperty("observations");
});
it.each(["revision", "project", "key", "order", "selection", "duplicate", "proof"])(
  "rejects changed native %s before returning an edit", async kind => {
    const f = fixture();
    if (kind === "revision") f.result.revision = "other";
    if (kind === "project") f.result.transaction.project_id = "other";
    if (kind === "key") f.result.transaction.idempotency_key = "other";
    if (kind === "order") f.result.cues.reverse();
    if (kind === "selection") f.result.selection.element_id = "outside-plan";
    if (kind === "duplicate") f.result.cues.push(f.result.cues[0]);
    if (kind === "proof") f.result.measurement.worker_sha256 = "bad";
    await expect(planEditableSubtitles(f.api, f.request, new AbortController().signal)).rejects.toThrow();
    expect(f.call).toHaveBeenCalledOnce();
  },
);
it("preserves native refusal and aborts before calling or returning a late result", async () => {
  const f = fixture(); f.call.mockRejectedValueOnce(Error("Font authorization revoked"));
  await expect(planEditableSubtitles(f.api, f.request, new AbortController().signal)).rejects.toThrow("revoked");
  f.call.mockClear(); const abort = new AbortController(); abort.abort();
  await expect(planEditableSubtitles(f.api, f.request, abort.signal)).rejects.toThrow();
  expect(f.call).not.toHaveBeenCalled();
  const late = new AbortController();
  f.call.mockImplementationOnce(async () => { late.abort(); return f.result; });
  await expect(planEditableSubtitles(f.api, f.request, late.signal)).rejects.toThrow();
});
