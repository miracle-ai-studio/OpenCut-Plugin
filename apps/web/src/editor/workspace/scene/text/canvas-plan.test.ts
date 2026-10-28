// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { OpenCutClient } from "../../../../lib/editor-sdk";
import type { EditorApi } from "../../api";
import { canvasTextPlanner } from "./canvas-plan";
import { synthesisFixture } from "./font-synthesis-fixture";
afterEach(() => { vi.restoreAllMocks(); vi.clearAllMocks(); });
function fixture() {
  const f = synthesisFixture(),
    scope = { projectId: "p", revision: "r", sequenceId: "s", elementId: "e" },
    time = { ticks: "0", timebase: 1 };
  const result = {
    identity: {
      projectId: "p",
      revision: "r",
      sequenceId: "s",
      time,
      width: 320,
      height: 180,
      profile: "classic-canvas-text",
    },
    layer: {
      elementId: "e",
      font: {
        fontId: f.font.id,
        contentHash: f.hash,
        size: "3",
        weight: 700,
        style: "normal",
        faceIndex: 0,
        glyphCoverageValidated: true,
        validatedTextHash: "a".repeat(64),
      },
      text: { content: "AV" },
    },
    font_synthesis: f.source.binding,
    layout: {
      fontId: f.font.id, fontContentHash: f.hash, validatedTextHash: "a".repeat(64),
      fontWeight: 700, fontStyle: "normal", block: { maxWidth: 32, height: 20 },
    },
    measurement: { worker_sha256: "c".repeat(64), rasterizer: "native" },
  };
  const call = vi.fn(async (name: string) => {
    if (name === "opencut_project_inspect")
      return { kind: "editor", snapshot: { revision: "r", project: { id: "p", fonts: [f.font] } } };
    if (name === "opencut_text_canvas_plan") return result;
    if (name === "opencut_font_synthesis_proof")
      return {
        
        project_id: "p",
        revision: "r",
        font_id: f.font.id,
        source: f.source,
      };
    if (name === "opencut_project_resource_inspect")
      return { version: { content_hash: f.hash, size: 3 } };
    throw new Error(`Unexpected ${name}`);
  });
  const api = {
    connectionIdentity: "connection",
    sdk: {
      call,
      inspectEditorProject: OpenCutClient.prototype.inspectEditorProject,
      hostCapabilities: vi.fn(async () => ({
        tools: ["opencut_text_canvas_plan"],
      })),
      downloadProjectResource: vi.fn(async () => new Response(f.bytes)),
    },
  } as unknown as EditorApi;
  return { f, scope, time, result, api, call };
}
it("uses native metrics with actual face metadata and requested italic for input", async () => {
  const f = fixture(),
    plan = await canvasTextPlanner(f.api)(
      f.scope,
      f.time,
      new AbortController().signal,
    );
  expect(plan.block).toEqual({ maxWidth: 32, height: 20 });
  expect(plan.actualFont?.style).toBe("normal");
  expect(plan.layer.font.style).toBe("italic");
  expect(f.result.layer.font.style).toBe("normal");
  expect(f.api.sdk.downloadProjectResource).not.toHaveBeenCalled();
  expect(f.call.mock.calls.map(([name]) => name)).toContain("opencut_text_canvas_plan");
});
it("refuses an omitted synthesis binding from the sole Canvas interface", async () => {
  const g = fixture();
  g.result.font_synthesis = null as never;
  await expect(
    canvasTextPlanner(g.api)(g.scope, g.time, new AbortController().signal),
  ).rejects.toThrow("omitted");
  expect(g.api.sdk.downloadProjectResource).not.toHaveBeenCalled();
});

it.each(["font", "proof", "revision"])("rejects changed native %s facts", async kind => {
  const f = fixture();
  if (kind === "font") f.result.layout.fontContentHash = "b".repeat(64);
  if (kind === "proof") f.result.measurement.worker_sha256 = "bad";
  if (kind === "revision") f.result.identity.revision = "other";
  await expect(canvasTextPlanner(f.api)(f.scope, f.time, new AbortController().signal)).rejects.toThrow();
});
