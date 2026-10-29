// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
// Browser composition/font caches have moved to the native worker. Their real
// byte authorization/lifecycle tests live in native-services/classic_visual.
// This adapter must submit fixed intent and present only completed native bytes.
vi.mock("../../../../../../packages/classic-visual-renderer/src/preview-segment.mjs", () => ({ PreviewSegmentDecoder: class {} }));
import { OpenCutApiError } from "../../../lib/editor-sdk";
import { VisualPreviewRenderer, type VisualPreviewExecutionApi } from "./visual-preview-renderer";
const session = { session_id: "s", generation: "1", overlay_epoch: "0", owner_id: "o", lease_id: "l" };
let bitmap: { width: number; height: number; close: ReturnType<typeof vi.fn> };
let create: ReturnType<typeof vi.fn>;
beforeEach(() => {
  bitmap = { width: 2, height: 2, close: vi.fn() };
  create = vi.fn(async () => bitmap);
  vi.stubGlobal("createImageBitmap", create);
  vi.stubGlobal("ImageData", class { constructor(public data: Uint8ClampedArray, public width: number, public height: number) {} });
});
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.useRealTimers(); });
function fixture() {
  const identity = { projectId: "p", revision: "r", sequenceId: "scene", time: { ticks: "0", timebase: 25 }, width: 2, height: 2 };
  const frame = {
    plan: { identity }, prepare_ahead: false,
    bound: {}, bound_media: {}, bound_execution: {}, bound_graphic: {}, bound_blend: {},
    picture: { picture_id: "ticket", width: 2, height: 2, byte_length: 16, content_hash: "a".repeat(64), max_retained_bytes: 1024,
      renderer: { renderer_sha256: "b".repeat(64), gpu_backend: "metal", gpu_device: "native-device" }, peak_worker_rss_bytes: "123" },
    hit_test: null as null | { identity: typeof identity; point: { x: number; y: number }; hits: { element_id: string; alpha: number }[] },
  };
  const call = vi.fn().mockResolvedValue(frame), read = vi.fn().mockResolvedValue(new Uint8ClampedArray(16));
  const resource = vi.fn();
  const api = { scene: { resource }, sdk: { call, readVisualPreviewPicture: read } } as unknown as VisualPreviewExecutionApi;
  return { frame, call, read, resource, api, renderer: new VisualPreviewRenderer() };
}
it("submits one native intent per frame and presents verified RGBA without source decoding", async () => {
  const f = fixture(), signal = new AbortController().signal;
  const result = await f.renderer.render(f.api, session, signal);
  expect(f.call).toHaveBeenCalledOnce();
  expect(f.call.mock.calls[0].slice(0, 2)).toEqual(["opencut_visual_preview_frame", {
    session, resolution: "source", time: null, prepare_ahead: false, retained_picture_bytes: 0, hit_query: null,
  }]);
  expect(f.read).toHaveBeenCalledWith(session, f.frame.picture, { signal: expect.any(AbortSignal), onClaimed: expect.any(Function) });
  expect(create).toHaveBeenCalledWith(expect.objectContaining({ data: new Uint8ClampedArray(16), width: 2, height: 2 }), { colorSpaceConversion: "none", premultiplyAlpha: "none" });
  expect(result.pixels).toBe(bitmap);
  expect(result.runtime).toMatchObject({ renderer: "native", gpuBackend: "metal", rendererSha256: "b".repeat(64) });
  expect(f.resource).not.toHaveBeenCalled();
  await f.renderer.render(f.api, { ...session, generation: "2" }, signal);
  expect(f.call).toHaveBeenCalledTimes(2);
  expect(f.read.mock.calls[1][0].generation).toBe("2");
});
it.each(["bound", "bound_media", "bound_execution", "bound_graphic", "bound_blend", "size", "intent"])(
  "refuses incomplete or changed native %s before reading bytes", async kind => {
    const f = fixture();
    if (kind === "size") f.frame.picture.width = 3;
    else if (kind === "intent") f.frame.prepare_ahead = true;
    else (f.frame as Record<string, unknown>)[kind] = null;
    await expect(f.renderer.render(f.api, session, new AbortController().signal)).rejects.toThrow();
    expect(f.read).not.toHaveBeenCalled(); expect(create).not.toHaveBeenCalled();
  },
);
it("binds the exact draft, time, preparation memory and native hit point in one call", async () => {
  const f = fixture(), time = { ticks: "7", timebase: 25 }, transaction = { project_id: "p", expected_revision: "r", idempotency_key: "draft", label: "Move", operations: [] };
  f.frame.prepare_ahead = true;
  f.frame.hit_test = { identity: f.frame.plan.identity, point: { x: 1, y: 0 }, hits: [{ element_id: "e", alpha: 128 }] };
  f.call.mockResolvedValue({ draft_id: "draft", transaction_digest: "digest", frame: f.frame });
  const result = await f.renderer.render(f.api, session, new AbortController().signal, { x: 1, y: 0 }, { draftId: "draft", intent: "transform" as never, transaction, time }, { time: { ticks: "2", timebase: 25 }, reservedBytes: 1234 });
  expect(f.call.mock.calls[0].slice(0, 2)).toEqual(["opencut_visual_preview_edit_draft_frame", {
    draft_id: "draft", intent: "transform", transaction,
    frame: { session, time, resolution: "preview540", prepare_ahead: true, retained_picture_bytes: 1234, hit_query: { x: 1, y: 0 } },
  }]);
  expect(result.hitTest).toEqual(f.frame.hit_test);
});
it.each(["draft", "digest", "hit-point", "hit-identity"])("rejects changed %s before presentation", async kind => {
  const f = fixture();
  if (kind.startsWith("hit")) {
    f.frame.hit_test = { identity: structuredClone(f.frame.plan.identity), point: { x: 1, y: 0 }, hits: [] };
    if (kind === "hit-point") f.frame.hit_test.point.x = 0;
    else f.frame.hit_test.identity.revision = "other";
    await expect(f.renderer.render(f.api, session, new AbortController().signal, { x: 1, y: 0 })).rejects.toThrow();
  } else {
    f.call.mockResolvedValue({ draft_id: kind === "draft" ? "other" : "draft", transaction_digest: kind === "digest" ? "" : "digest", frame: f.frame });
    await expect(f.renderer.render(f.api, session, new AbortController().signal, undefined, { draftId: "draft", intent: "transform" as never, transaction: {} as never, time: { ticks: "0", timebase: 1 } })).rejects.toThrow();
  }
  expect(f.read).not.toHaveBeenCalled();
});
it("propagates revoked native byte reads and closes a bitmap delivered after cancellation", async () => {
  const f = fixture(); f.read.mockRejectedValueOnce(Error("Owner replaced"));
  await expect(f.renderer.render(f.api, session, new AbortController().signal)).rejects.toThrow("Owner replaced");
  expect(create).not.toHaveBeenCalled();
  const abort = new AbortController(); create.mockImplementationOnce(async () => { abort.abort(); return bitmap; });
  await expect(f.renderer.render(f.api, session, abort.signal)).rejects.toThrow();
  expect(bitmap.close).toHaveBeenCalledOnce();
});
it("retires presentation on close and rejects changed raster dimensions without leaking pixels", async () => {
  const f = fixture(); bitmap.width = 3;
  await expect(f.renderer.render(f.api, session, new AbortController().signal)).rejects.toThrow("dimensions");
  expect(bitmap.close).toHaveBeenCalledOnce();
  f.call.mockClear(); f.renderer.close();
  await expect(f.renderer.render(f.api, session, new AbortController().signal)).rejects.toThrow();
  expect(f.call).not.toHaveBeenCalled();
});
it("retries only native busy errors within a fixed bound and cancels the retry delay", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const busy = new OpenCutApiError("PREVIEW_PICTURE_BUSY", "Busy", null, 429, "busy", []);
  f.call.mockRejectedValue(busy);
  const pending = expect(f.renderer.render(f.api, session, new AbortController().signal)).rejects.toThrow("Busy");
  await vi.runAllTimersAsync(); await pending; expect(f.call).toHaveBeenCalledTimes(7);
  const abort = new AbortController();
  const cancelled = expect(f.renderer.render(f.api, session, abort.signal)).rejects.toThrow();
  await vi.advanceTimersByTimeAsync(0); abort.abort(); await cancelled;
  await vi.runAllTimersAsync(); expect(f.call).toHaveBeenCalledTimes(8);
});
it("uses only a display context for presentation", () => {
  const f = fixture(), drawImage = vi.fn(), clearRect = vi.fn(), getContext = vi.fn(() => ({ drawImage, clearRect }));
  const painter = f.renderer.surfacePresenter({ width: 2, height: 2, getContext } as unknown as OffscreenCanvas);
  painter.draw(bitmap as unknown as ImageBitmap); painter.clear();
  expect(getContext).toHaveBeenCalledExactlyOnceWith("2d", { desynchronized: true });
  expect(drawImage).toHaveBeenCalledWith(bitmap, 0, 0); expect(clearRect).toHaveBeenCalledWith(0, 0, 2, 2);
});
it("exposes only prepared facts after a native claim and completes pixels after validation", async () => {
  const f = fixture(); let claim!: () => void, bytes!: (value: Uint8ClampedArray) => void;
  f.read.mockImplementationOnce((_session, _picture, options) => { claim = options.onClaimed; return new Promise(resolve => { bytes = resolve; }); });
  const pending = await f.renderer.prepare(f.api, session, new AbortController().signal);
  let claimed = false, completed = false;
  void pending.claimed.then(() => { claimed = true; });
  void pending.complete.then(() => { completed = true; });
  expect(pending.frame).toBe(f.frame); expect(claimed).toBe(false); expect(create).not.toHaveBeenCalled();
  claim(); await Promise.resolve(); expect(claimed).toBe(true); expect(completed).toBe(false);
  bytes(new Uint8ClampedArray(16)); expect((await pending.complete).pixels).toBe(bitmap);
});
it("rejects both preparation stages when a scoped read fails before its claim", async () => {
  const f = fixture(); f.read.mockRejectedValueOnce(Error("Revoked reader"));
  const pending = await f.renderer.prepare(f.api, session, new AbortController().signal);
  await expect(pending.claimed).rejects.toThrow("Revoked reader");
  await expect(pending.complete).rejects.toThrow("Revoked reader");
  expect(create).not.toHaveBeenCalled();
});
