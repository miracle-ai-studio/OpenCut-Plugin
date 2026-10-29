import { PreviewSegmentDecoder } from "../../../../../../packages/classic-visual-renderer/src/preview-segment.mjs";
import type { EditorApi } from "../api";
import { OpenCutApiError, type ToolInput, type ToolOutput } from "../../../lib/editor-sdk";

export { PreviewSegmentDecoder };
type Input = ToolInput<"opencut_visual_preview_frame">;
type Frame = ToolOutput<"opencut_visual_preview_frame">;
export type VisualPreviewExecutionApi = {
  scene: Pick<EditorApi["scene"], "resource">;
  sdk: Pick<EditorApi["sdk"], "prepareVisualPreviewPicture">;
};
export type NativePictureResult = {
  frame: Frame;
  pixels: ImageBitmap;
  ready?: Promise<void>;
  hitTest?: NonNullable<Frame["hit_test"]>;
  runtime: Record<string, string>;
};
export type NativePicturePreparation = {
  frame: Frame;
  claimed: Promise<void>;
  complete: Promise<NativePictureResult>;
};

/** Present completed native frames. All source decoding, shaping, effects,
 * masks, layer alpha and binding observations are owned by Rust. */
export class VisualPreviewRenderer {
  private abort = new AbortController();
  async render(...args: Parameters<VisualPreviewRenderer["prepare"]>): Promise<NativePictureResult> {
    return (await this.prepare(...args)).complete;
  }
  async prepare(
    api: VisualPreviewExecutionApi,
    session: Input["session"],
    signal: AbortSignal,
    hitQuery?: { x: number; y: number },
    transformDraft?: {
      draftId: string;
      intent: ToolInput<"opencut_visual_preview_edit_draft_frame">["intent"];
      transaction: ToolInput<"opencut_edit_apply">;
      time: Input["time"];
      intentSignal?: AbortSignal;
    },
    preparation?: { time: Input["time"]; reservedBytes: number; end?: NonNullable<Input["time"]>; captureSurface?: boolean },
  ): Promise<NativePicturePreparation> {
    const intent = AbortSignal.any([signal, this.abort.signal, ...(transformDraft?.intentSignal ? [transformDraft.intentSignal] : [])]);
    intent.throwIfAborted();
    const began = performance.now();
    const request: Input = {
      session, resolution: preparation ? "preview540" : "source",
      time: transformDraft?.time ?? preparation?.time ?? null,
      prepare_ahead: !!preparation,
      retained_picture_bytes: preparation?.reservedBytes ?? 0,
      hit_query: hitQuery ? { x: hitQuery.x, y: hitQuery.y } : null,
    };
    let prepared: Awaited<ReturnType<VisualPreviewExecutionApi["sdk"]["prepareVisualPreviewPicture"]>>;
    for (let attempt = 0; ; attempt++) {
      try {
        prepared = await api.sdk.prepareVisualPreviewPicture(transformDraft ? { kind: "edit_draft", draft: {
            draft_id: transformDraft.draftId, intent: transformDraft.intent,
            transaction: transformDraft.transaction, frame: request,
        } } : { kind: "scene", frame: request }, { signal: intent });
        break;
      } catch (error) {
        if (!(error instanceof OpenCutApiError && ["PREVIEW_PICTURE_BUSY", "PREVIEW_DRAFT_BUSY"].includes(error.code)) || attempt >= 6) throw error;
        await new Promise<void>((resolve, reject) => {
          const abort = () => { clearTimeout(timer); intent.removeEventListener("abort", abort); reject(intent.reason); };
          const timer = setTimeout(() => { intent.removeEventListener("abort", abort); resolve(); }, 32);
          intent.addEventListener("abort", abort, { once: true });
          if (intent.aborted) abort();
        });
      }
    }
    let frame: Frame;
    try {
      intent.throwIfAborted();
      const header = prepared.header;
      if (transformDraft) {
        if (header.kind !== "edit_draft" || header.draft.draft_id !== transformDraft.draftId || !header.draft.transaction_digest)
          throw Error("Native draft changed its identity");
        frame = header.draft.frame;
      } else {
        if (header.kind !== "scene") throw Error("Native picture changed preparation kind");
        frame = header.frame;
      }
      if (!frame.bound || !frame.bound_media || !frame.bound_execution || !frame.bound_graphic || !frame.bound_blend)
        throw Error("Native picture omitted its complete scene binding");
      const picture = frame.picture, identity = frame.plan.identity;
      if (!picture || picture.width !== identity.width || picture.height !== identity.height ||
          frame.prepare_ahead !== request.prepare_ahead ||
          (hitQuery && (!frame.hit_test || frame.hit_test.point.x !== hitQuery.x || frame.hit_test.point.y !== hitQuery.y ||
            JSON.stringify(frame.hit_test.identity) !== JSON.stringify(identity))))
        throw Error("Native picture changed its prepared identity");
    } catch (error) { prepared.cancel(error); throw error; }
    const picture = frame.picture;
    const preparedAt = performance.now();
    const completedFrame = frame;
    const complete = (async (): Promise<NativePictureResult> => {
      const bytes = await prepared.pixels;
      intent.throwIfAborted();
      const pixels = await createImageBitmap(new ImageData(bytes, picture.width, picture.height), { colorSpaceConversion: "none", premultiplyAlpha: "none" });
      try {
        intent.throwIfAborted();
        if (pixels.width !== picture.width || pixels.height !== picture.height) throw Error("Native picture changed raster dimensions");
        return {
          frame: completedFrame, pixels,
          ...(completedFrame.hit_test ? { hitTest: completedFrame.hit_test } : {}),
          runtime: {
            renderer: "native", rendererSha256: picture.renderer.renderer_sha256,
            gpuBackend: picture.renderer.gpu_backend, gpuDevice: picture.renderer.gpu_device,
            peakWorkerRssBytes: picture.peak_worker_rss_bytes,
            maxRetainedBytes: String(picture.max_retained_bytes),
            nativeFrameMs: String(preparedAt - began), pictureReadMs: String(performance.now() - preparedAt),
            totalMs: String(performance.now() - began),
          } as Record<string, string>,
        };
      } catch (error) { pixels.close(); throw error; }
    })();
    void complete.catch(() => {});
    // The host claims before emitting the validated metadata returned by the SDK.
    return { frame: completedFrame, claimed: Promise.resolve(), complete };
  }
  surfacePresenter(canvas: OffscreenCanvas) {
    const context = canvas.getContext("2d", { desynchronized: true });
    if (!context) throw Error("Native picture presentation is unavailable");
    return { draw: (bitmap: ImageBitmap) => context.drawImage(bitmap, 0, 0), clear: () => context.clearRect(0, 0, canvas.width, canvas.height) };
  }
  async surfaceBitmap(pixels: ImageBitmap) { return pixels; }
  close() { this.abort.abort(new DOMException("Native picture presentation closed", "AbortError")); }
}
