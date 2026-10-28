import { expect, it, vi } from "vitest";
import type { EditorApi } from "../../api";
import { planAssSubtitles, type AssRequest } from "./planned-ass";
function fixture() {
  const input: AssRequest = { project_id: "p", expected_revision: "r", sequence_id: "s", idempotency_key: "key", track: { kind: "new", track_id: "subtitles" }, content: "original ASS", font_bindings: [] };
  const result = {
    revision: "r", width: 320, height: 180, font_requirements: [], warnings: [],
    cues: [{ layer: { elementId: "cue", text: { content: "Exact\ncue" } } }],
    transaction: { project_id: "p", expected_revision: "r", idempotency_key: "key" },
    selection: { track_id: "subtitles", element_id: "cue" },
    measurement: { worker_sha256: "a".repeat(64), rasterizer: "native" },
  };
  const call = vi.fn().mockResolvedValue(result);
  return { input, result, call, api: { sdk: { call } } as unknown as EditorApi };
}
it("receives exact native ASS content and a fixed ordinary edit in one call", async () => {
  const f = fixture(), signal = new AbortController().signal;
  const result = await planAssSubtitles(f.api, f.input, signal);
  expect(result.cues[0].layer.text.content).toBe("Exact\ncue");
  expect(result.transaction?.idempotency_key).toBe("key");
  expect(f.call).toHaveBeenCalledExactlyOnceWith("opencut_subtitle_ass_plan", f.input, { signal });
});
it("returns unresolved fonts without acquiring fallback bytes or returning an edit", async () => {
  const f = fixture();
  const unresolved = { ...f.result, cues: [], transaction: null, selection: null, measurement: null, font_requirements: [{ family: "Missing", weight: "bold", style: "italic", resolved_font_id: null }] };
  f.call.mockResolvedValue(unresolved);
  expect(await planAssSubtitles(f.api, f.input, new AbortController().signal)).toEqual(unresolved);
  expect(f.call).toHaveBeenCalledOnce();
  f.call.mockResolvedValue({ ...unresolved, transaction: f.result.transaction });
  await expect(planAssSubtitles(f.api, f.input, new AbortController().signal)).rejects.toThrow("Unresolved");
  f.call.mockResolvedValue({ ...unresolved, cues: f.result.cues });
  await expect(planAssSubtitles(f.api, f.input, new AbortController().signal)).rejects.toThrow("Unresolved");
});
it("rejects changed edit scope or missing native measurement", async () => {
  const f = fixture(); f.result.transaction.expected_revision = "changed";
  await expect(planAssSubtitles(f.api, f.input, new AbortController().signal)).rejects.toThrow();
  f.result.transaction.expected_revision = "r"; f.result.measurement.worker_sha256 = "bad";
  await expect(planAssSubtitles(f.api, f.input, new AbortController().signal)).rejects.toThrow();
});
it("propagates parser refusal and abort without applying anything", async () => {
  const f = fixture(); f.call.mockRejectedValueOnce(Error("Unsupported ASS tag"));
  await expect(planAssSubtitles(f.api, f.input, new AbortController().signal)).rejects.toThrow("Unsupported ASS tag");
  f.call.mockClear(); const abort = new AbortController(); abort.abort();
  await expect(planAssSubtitles(f.api, f.input, abort.signal)).rejects.toThrow();
  expect(f.call).not.toHaveBeenCalled();
});
