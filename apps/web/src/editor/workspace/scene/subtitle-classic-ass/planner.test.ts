import { it, expect, vi } from "vitest";
import { planClassicAssSubtitles, type ClassicAssRequest } from "./planner";
import type { EditorApi } from "../../api";
it("returns unresolved source fonts and warnings without acquiring fallback font bytes", async () => {
  const plan = {
    
    revision: "r",
    width: 320,
    height: 180,
    cues: [],
    transaction: null,
    selection: null,
    font_requirements: [
      {
        family: "Arial",
        weight: "bold",
        style: "normal",
        resolved_font_id: null,
      },
    ],
    warnings: ["Ignored ASS event effects in 1 subtitle cue(s)."],
    skipped_cue_count: 1,
  };
  const call = vi.fn().mockResolvedValue(plan);
  const api = { sdk: { call } } as unknown as EditorApi;
  const input = {
    project_id: "p",
    expected_revision: "r",
    sequence_id: "s",
    idempotency_key: "same-key",
    track: { kind: "new", track_id: "subtitles" },
    content: "original ASS",
    font_bindings: [],
  } as ClassicAssRequest;
  expect(
    await planClassicAssSubtitles(api, input, new AbortController().signal),
  ).toEqual(plan);
  expect(call).toHaveBeenCalledOnce();
  expect(call).toHaveBeenCalledWith(
    "opencut_subtitle_classic_ass_plan",
    input,
    { signal: expect.any(AbortSignal) },
  );
});
