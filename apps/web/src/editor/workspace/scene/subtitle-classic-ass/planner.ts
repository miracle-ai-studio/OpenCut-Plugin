import type { ToolInput, ToolOutput } from "../../../../lib/editor-sdk";
import type { EditorApi } from "../../api";
import { verifyNativeSubtitlePlan } from "../captions/native-plan";
export type ClassicAssRequest = ToolInput<"opencut_subtitle_classic_ass_plan">;
export type ClassicAssPlan = ToolOutput<"opencut_subtitle_classic_ass_plan">;
export async function planClassicAssSubtitles(api: EditorApi, input: ClassicAssRequest, signal: AbortSignal): Promise<ClassicAssPlan> {
  signal.throwIfAborted();
  const request = structuredClone(input);
  const planned = await api.sdk.call("opencut_subtitle_classic_ass_plan", request, { signal });
  signal.throwIfAborted();
  verifyNativeSubtitlePlan(planned, request, !planned.cues.length);
  return planned;
}
