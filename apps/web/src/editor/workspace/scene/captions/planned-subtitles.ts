import type { ToolInput, ToolOutput } from "../../../../lib/editor-sdk";
import type { EditorApi } from "../../api";
import { verifyNativeSubtitlePlan } from "./native-plan";
export type SubtitleTextRequest = ToolInput<"opencut_subtitle_text_plan">;
export type SubtitleTextPlan = ToolOutput<"opencut_subtitle_text_plan">;
/** Rust owns native shaping, wrapping, cue timing and the ordinary edit. */
export async function planEditableSubtitles(api: EditorApi, input: SubtitleTextRequest, signal: AbortSignal): Promise<SubtitleTextPlan> {
  signal.throwIfAborted();
  const request = structuredClone(input);
  const planned = await api.sdk.call("opencut_subtitle_text_plan", request, { signal });
  signal.throwIfAborted();
  verifyNativeSubtitlePlan(planned, request);
  return planned;
}
