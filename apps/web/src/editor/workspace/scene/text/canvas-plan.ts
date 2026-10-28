import type { EditorApi } from "../../api";
import type { CanvasTextPlanning } from "./canvas-types";
import {
  checkSynthesisResource,
  synthesisFont,
  synthesisBindingsEqual,
} from "./font-synthesis";
import type { FontSynthesisBinding } from "./font-synthesis";
import type { ToolOutput } from "../../../../lib/editor-sdk";

/** Application owns sampling/geometry and native font coverage. The existing
 * native text worker supplies metrics; DOM never invents a layout. */
export function canvasTextPlanner(api: EditorApi): CanvasTextPlanning {
  let cached: {
    key: string;
    snapshot: Extract<ToolOutput<"opencut_project_inspect">,{kind:"editor"}>["snapshot"];
  } | null = null;
  return async (scope, time, signal) => {
    if (!scope.elementId) throw new Error("Canvas text element is missing.");
    const connection = api.connectionIdentity;
    const checkConnection = () => {
      signal.throwIfAborted();
      if (api.connectionIdentity !== connection)
        throw new Error("The edit target changed.");
    };
    const key = JSON.stringify([connection, scope.projectId, scope.revision]);
    const snapshot =
      cached?.key === key
        ? cached.snapshot
        : await api.sdk.inspectEditorProject(
            { project_id: scope.projectId, revision: scope.revision },
            { signal },
          );
    checkConnection();
    if (
      snapshot.project.id !== scope.projectId ||
      snapshot.revision !== scope.revision
    )
      throw new Error("The edit target changed.");
    cached = { key, snapshot };
    const result = await api.sdk.call(
      "opencut_text_canvas_plan",
      {
        project_id: scope.projectId,
        revision: scope.revision,
        sequence_id: scope.sequenceId,
        element_id: scope.elementId,
        time,
      },
      { signal },
    );
    const identity = result.identity;
    checkConnection();
    if (
      identity.profile !== "classic-canvas-text" ||
      identity.projectId !== scope.projectId ||
      identity.revision !== scope.revision ||
      identity.sequenceId !== scope.sequenceId ||
      result.layer.elementId !== scope.elementId ||
      identity.time.ticks !== time.ticks ||
      identity.time.timebase !== time.timebase
    )
      throw new Error("The edit target changed.");
    const actualFont = result.layer.font;
    let binding: FontSynthesisBinding | undefined;
    const layer = structuredClone(result.layer);
    if ("font_synthesis" in result && result.font_synthesis) {
      const resource = snapshot.project.fonts.find(
        (font) => font.id === actualFont.fontId,
      );
      const proof = await api.sdk.call(
        "opencut_font_synthesis_proof",
        {
          project_id: scope.projectId,
          revision: scope.revision,
          font_id: actualFont.fontId,
        },
        { signal },
      );
      checkConnection();
      if (
        !resource ||
        proof.project_id !== scope.projectId ||
        proof.revision !== scope.revision ||
        proof.font_id !== actualFont.fontId
      )
        throw new Error("The locked font identity changed.");
      const enriched = checkSynthesisResource(resource, proof.source);
      binding = enriched.synthesis_binding;
      if (
        !binding ||
        !synthesisBindingsEqual(binding, result.font_synthesis) ||
        resource.content.content_hash !== actualFont.contentHash ||
        String(resource.content.size) !== actualFont.size ||
        resource.weight !== actualFont.weight ||
        resource.style !== actualFont.style
      )
        throw new Error("The locked font identity changed.");
      layer.font.style = binding.requested.style;
    } else if (
      snapshot.project.fonts.some(
        (font) => font.id === actualFont.fontId && synthesisFont(font),
      )
    ) {
      throw new Error(
        "The Canvas plan omitted its explicit synthesis binding.",
      );
    }
    checkConnection();
    const layout = result.layout;
    if (!layout || layout.fontId !== actualFont.fontId || layout.fontContentHash !== actualFont.contentHash ||
        layout.validatedTextHash !== actualFont.validatedTextHash || layout.fontWeight !== actualFont.weight ||
        layout.fontStyle !== actualFont.style || !/^[0-9a-f]{64}$/.test(result.measurement.worker_sha256))
      throw Error("The native text layout changed its locked font identity.");
    return { scope, width: identity.width, height: identity.height, layer,
      ...(binding ? { actualFont, fontSynthesis: binding } : {}), block: layout.block };
  };
}
