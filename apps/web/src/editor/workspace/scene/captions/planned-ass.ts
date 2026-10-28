import type { ToolInput, ToolOutput } from "../../../../lib/editor-sdk";
import type { EditorApi } from "../../api";
import { verifyNativeSubtitlePlan } from "./native-plan";
export type AssRequest = ToolInput<"opencut_subtitle_ass_plan">;
export type AssPlan = ToolOutput<"opencut_subtitle_ass_plan">;
export const assFontKey = (font: { family: string; weight: string; style: string }) => JSON.stringify([font.family, font.weight, font.style]);
export async function planAssSubtitles(api: EditorApi, input: AssRequest, signal: AbortSignal): Promise<AssPlan> {
  signal.throwIfAborted();
  const request = structuredClone(input);
  const planned = await api.sdk.call("opencut_subtitle_ass_plan", request, { signal });
  signal.throwIfAborted();
  const unresolved = planned.font_requirements.some(font => !font.resolved_font_id);
  if (unresolved && planned.cues.length) throw Error("Unresolved ASS fonts cannot produce editable cues.");
  verifyNativeSubtitlePlan(planned, request, unresolved);
  return planned;
}
