// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { EditorApi } from "../../api";
import type { Asset } from "../types";
import { SourceArtView, ArtDisplay, useSourceArt } from "./view";
import { OpenCutApiError } from "../../../../lib/editor-sdk";
import { sourceArtFailure } from "./failure";
import { acquireSourceArt, allocateArt, clearSourceArt } from "./service";
import { SourceArtCache } from "./cache";
import * as sourceArtCacheModule from "./cache";
import type { DisplayArt } from "./service";
import type { ArtSelection, SourceArtResponse, SourceArtRequest } from "./service";
import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";

const asset = { id: "source", version: { id: "pinned", content_hash: "a".repeat(64), size: 4 } } as Asset;
const selection: ArtSelection = { projectId: "project", revision: "fixed", asset,
  query: { kind: "waveform", sample_rate: 48000, projection: { kind: "source", range: null, bins: 3 } } };
const identity = { algorithm: "source-peaks128-v1", decode_profile: "fixed-cas-native-libav-concat-linear-skia-jpeg",
  worker_sha256: "a".repeat(64), module_sha256: "c".repeat(64), rasterizer: "skia-safe-0.153.3-srgb-jpeg80",
  platform: { os: "test", architecture: "test", kernel_release: "test", system_build: "test" },
  decoder_libraries: ["avcodec", "avformat", "avutil", "swscale", "swresample"].map(name => ({name, version: "test", configuration_sha256: "d".repeat(64), binary_sha256: "e".repeat(64)})) };
const response = (request: SourceArtRequest): SourceArtResponse => ({ 
  project_id: request.project_id, revision: request.revision, asset_id: request.asset_id, version: asset.version, identity,
  data: { kind: "waveform", sample_rate: 48000, channels: 3, total_samples: "384", bucket_size: 128,
    projection: request.query.kind === "waveform" ? request.query.projection : selection.query.kind === "waveform" ? selection.query.projection : never(), amplitudes: [0, .5, 2] } });
function never(): never { throw new Error("Fixture waveform projection required"); }
class Visibility {
  static instances: Visibility[] = [];
  target!: Element;
  constructor(private callback: IntersectionObserverCallback) { Visibility.instances.push(this); }
  observe(target: Element) { this.target = target; }
  disconnect = vi.fn();
  show() { this.callback([{ target: this.target, isIntersecting: true } as IntersectionObserverEntry], this as unknown as IntersectionObserver); }
}
beforeEach(() => { Visibility.instances = []; vi.stubGlobal("IntersectionObserver", Visibility); });
afterEach(() => { cleanup(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

it("shows only service-projected source peaks, shares visible requests and fences a late old revision", async () => {
  let finish!: (value: SourceArtResponse) => void;
  const sourceArt = vi.fn((request: SourceArtRequest) => Promise.resolve(response(request)));
  sourceArt.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const cancelSourceArt = vi.fn(async () => ({ cancel_requested: true }));
  const api = { scene: { sourceArt, cancelSourceArt } } as unknown as EditorApi;
  try {
    const view = render(<><SourceArtView api={api} selection={selection} /><SourceArtView api={api} selection={selection} /></>);
    expect(sourceArt).not.toHaveBeenCalled();
    act(() => { Visibility.instances.forEach(observer => observer.show()); });
    await waitFor(() => expect(sourceArt).toHaveBeenCalledOnce());
    const old = sourceArt.mock.calls[0][0];
    view.rerender(<SourceArtView api={api} selection={{ ...selection, revision: "new-fixed" }} />);
    await act(async () => { finish(response(old)); });
    await waitFor(() => expect(sourceArt).toHaveBeenCalledTimes(2));
    expect(cancelSourceArt).toHaveBeenCalledWith({ project_id: "project", revision: "fixed", request_id: old.request_id });
    expect(sourceArt.mock.calls[1][0].revision).toBe("new-fixed");
    const peaks = await screen.findByRole("img", { name: "Source peaks before element gain" });
    // Independent presentation coordinates: zero omitted, .5 => half height,
    // over-unity source peak clips only the display height; stored data stays2.
    expect(peaks.querySelector("path")?.getAttribute("d")).toBe("M2.5 24V12M4.5 24V0");
    expect(peaks.querySelectorAll("path")).toHaveLength(1);
  } finally { clearSourceArt(api); }
});

it("rejects changed receipts and checksum bytes, budgets decoded JPEG pixels and keeps NoFrame explicit", () => {
  const base = response({ project_id: selection.projectId, revision: selection.revision, asset_id: asset.id, request_id: "test", query: selection.query });
  expect(() => allocateArt({ ...base, revision: "other" }, selection)).toThrow("invalid");
  const changed = structuredClone(base); changed.identity.module_sha256 = "missing";
  expect(() => allocateArt(changed, selection)).toThrow("invalid");
  const mixed = structuredClone(base) as SourceArtResponse & { identity: { browser_version?: string } };
  mixed.identity.browser_version = "153.0.0.0";
  expect(() => allocateArt(mixed, selection)).toThrow("invalid");
  const duplicate = structuredClone(base);
  if (!("decoder_libraries" in duplicate.identity)) throw Error("Native fixture required");
  duplicate.identity.decoder_libraries[1] = duplicate.identity.decoder_libraries[0];
  expect(() => allocateArt(duplicate, selection)).toThrow("invalid");
  const thumbnailSelection: ArtSelection = { ...selection, query: { kind: "thumbnail" } };
  const jpeg = Uint8Array.from([255, 216, 255, 217]);
  const thumbnail: SourceArtResponse = { ...base, identity: { ...identity, algorithm: "classic-thumbnail-1s-jpeg08-v1" }, data: {
    kind: "thumbnail_ready", width: 1280, height: 720, content_hash: bytesToHex(sha256(jpeg)), jpeg_bytes_hex: bytesToHex(jpeg),
    observation: { kind: "static_image", source_width: 1280, source_height: 720 } } };
  expect(allocateArt(thumbnail, thumbnailSelection).bytes).toBeGreaterThanOrEqual(1280 * 720 * 4 + jpeg.length);
  const corrupt = structuredClone(thumbnail); if (corrupt.data.kind === "thumbnail_ready") corrupt.data.content_hash = "f".repeat(64);
  expect(() => allocateArt(corrupt, thumbnailSelection)).toThrow("invalid");
  const noFrame: SourceArtResponse = { ...thumbnail, data: { kind: "thumbnail_no_frame", reason: "no_sample_at_one_second",
    observation: { kind: "video", track_id: 1, source_width: 1, source_height: 1, requested_seconds: 1, actual_timestamp_seconds: null, frame_duration_seconds: null } } };
  render(<ArtDisplay art={allocateArt(noFrame, thumbnailSelection).materialize().value} />);
  expect(screen.getByLabelText("No source thumbnail at one second")).toBeTruthy(); expect(screen.queryByRole("img")).toBeNull();
});

it("reauthorizes100 visible element leases once per asset/profile and stops without retry after revocation", async () => {
  vi.useFakeTimers();
  const sourceArt = vi.fn(async (request: SourceArtRequest) => {
    const value = response(request);
    if (value.data.kind === "waveform" && request.query.kind === "waveform") value.data.amplitudes = Array(request.query.projection.bins).fill(.5);
    return value;
  });
  const api = { scene: { sourceArt, cancelSourceArt: vi.fn(async () => ({ cancel_requested: true })) } } as unknown as EditorApi;
  const invalidated = vi.fn();
  const leases = Array.from({ length: 100 }, (_, index) => acquireSourceArt(api, { ...selection,
    query: { kind: "waveform", sample_rate: 48000, projection: { kind: "element", sequence_id: "scene", track_id: "track",
      element_id: `clip${index}`, local_range: { start: { ticks: "0", timebase: 1 }, duration: { ticks: "1", timebase: 1 } },
      bins: 2, bin_duration: { ticks: "1", timebase: 2 } } } }, invalidated));
  try {
    await Promise.all(leases.map(lease => lease.promise));
    expect(sourceArt).toHaveBeenCalledTimes(100);
    await vi.advanceTimersByTimeAsync(1000);
    expect(sourceArt).toHaveBeenCalledTimes(101);
    expect(sourceArt.mock.calls.at(-1)?.[0].query).toEqual({ kind: "waveform", sample_rate: 48000,
      projection: { kind: "source", range: null, bins: 1 } });
    sourceArt.mockRejectedValueOnce(new Error("NOT_AUTHORIZED"));
    await vi.advanceTimersByTimeAsync(1000);
    // Each display receives its own failure even when its callback is shared.
    expect(invalidated).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(5000); expect(sourceArt).toHaveBeenCalledTimes(102);
  } finally { leases.forEach(lease => lease.release()); clearSourceArt(api); }
});

it("hides mounted artwork after failed authorization and cancels an in-flight probe when the last display unmounts", async () => {
  vi.useFakeTimers();
  const sourceArt = vi.fn(async (request: SourceArtRequest) => {
    const value = response(request);
    if (value.data.kind === "waveform" && request.query.kind === "waveform") value.data.amplitudes = Array(request.query.projection.bins).fill(.5);
    return value;
  });
  const cancelSourceArt = vi.fn(async () => ({ cancel_requested: true }));
  const api = { scene: { sourceArt, cancelSourceArt } } as unknown as EditorApi;
  const flush = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };
  try {
    const view = render(<SourceArtView api={api} selection={selection} />);
    await act(async () => { Visibility.instances[0].show(); await flush(); });
    expect(screen.getByRole("img", { name: "Source peaks before element gain" })).toBeTruthy();
    sourceArt.mockRejectedValueOnce(new Error("NOT_AUTHORIZED"));
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(screen.queryByRole("img")).toBeNull(); expect(screen.getByLabelText("Source artwork unavailable")).toBeTruthy();
    view.unmount();
    const again = render(<SourceArtView api={api} selection={selection} />);
    await act(async () => { Visibility.instances.at(-1)!.show(); await flush(); });
    sourceArt.mockImplementationOnce((_request, signal?: AbortSignal) => new Promise((_resolve, reject) => {
      signal?.addEventListener("abort", () => reject(new DOMException("abort", "AbortError")));
    }));
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    const pending = sourceArt.mock.calls.at(-1)![0];
    again.unmount(); await act(flush);
    expect(cancelSourceArt).toHaveBeenCalledWith({ project_id: "project", revision: "fixed", request_id: pending.request_id });
    const count = sourceArt.mock.calls.length; await vi.advanceTimersByTimeAsync(3000); expect(sourceArt).toHaveBeenCalledTimes(count);
  } finally { clearSourceArt(api); }
});

it("keeps authorization failure visible when a second artwork success was already queued", async () => {
  vi.useFakeTimers();
  // Retain the wider-cache completion-order regression. Interactive default
  // scheduling is now one credit; the generic cache still supports two.
  const OriginalCache = sourceArtCacheModule.SourceArtCache;
  const cacheConstructor = vi.spyOn(sourceArtCacheModule, "SourceArtCache").mockImplementation(
    function <T>(limits: { bytes: number; entries: number; concurrent: number } = sourceArtCacheModule.SOURCE_ART_LIMITS) {
      return new OriginalCache<T>({ ...limits, concurrent: 2 });
    },
  );
  const create = vi.fn(() => "blob:already-resolved-thumbnail"), revoke = vi.fn();
  vi.stubGlobal("URL", { createObjectURL: create, revokeObjectURL: revoke });
  let rejectProbe!: (error: Error) => void, finishThumbnail!: (value: SourceArtResponse) => void;
  let thumbnailRequest!: SourceArtRequest;
  const sourceArt = vi.fn((request: SourceArtRequest) => {
    if (request.query.kind === "thumbnail") {
      thumbnailRequest = request;
      return new Promise<SourceArtResponse>(resolve => { finishThumbnail = resolve; });
    }
    if (request.query.projection.bins === 1) return new Promise<SourceArtResponse>((_resolve, reject) => { rejectProbe = reject; });
    return Promise.resolve(response(request));
  });
  const api = { scene: { sourceArt, cancelSourceArt: vi.fn(async () => ({ cancel_requested: true })) } } as unknown as EditorApi;
  const thumbnailSelection: ArtSelection = { ...selection, asset: { ...asset, id: "second" }, query: { kind: "thumbnail" } };
  const flush = async () => { for (let i = 0; i < 24; i++) await Promise.resolve(); };
  try {
    const view = render(<SourceArtView api={api} selection={selection} />);
    await act(async () => { Visibility.instances[0].show(); await flush(); });
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    view.rerender(<><SourceArtView api={api} selection={selection} /><SourceArtView api={api} selection={thumbnailSelection} /></>);
    await act(async () => { Visibility.instances.at(-1)!.show(); await flush(); });
    const jpeg = Uint8Array.from([255, 216, 255, 217]);
    const thumbnail: SourceArtResponse = { ...response(thumbnailRequest), identity: { ...identity, algorithm: "classic-thumbnail-1s-jpeg08-v1" }, data: {
      kind: "thumbnail_ready", width: 1, height: 1, content_hash: bytesToHex(sha256(jpeg)), jpeg_bytes_hex: bytesToHex(jpeg),
      observation: { kind: "static_image", source_width: 1, source_height: 1 } } };
    await act(async () => {
      // The native responses settle in one turn: failed authorization is
      // queued first, while the other load can still materialize its receipt.
      rejectProbe(new Error("NOT_AUTHORIZED")); finishThumbnail(thumbnail); await flush();
    });
    expect(create).toHaveBeenCalledOnce(); expect(revoke).toHaveBeenCalledWith("blob:already-resolved-thumbnail");
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.getAllByLabelText("Source artwork unavailable")).toHaveLength(2);
  } finally { try { clearSourceArt(api); } finally { cacheConstructor.mockRestore(); } }
});

it("keeps the decoder credit occupied across refresh until prior cancellation acknowledgement settles", async () => {
  const acknowledgements: (() => void)[] = [];
  let calls = 0;
  const sourceArt = vi.fn((input: SourceArtRequest, signal?: AbortSignal) => {
    if (++calls > 1) return Promise.resolve(response(input));
    return new Promise<SourceArtResponse>((_resolve, reject) => {
      signal!.addEventListener("abort", () => reject(new DOMException("abort", "AbortError")));
    });
  });
  const cancelSourceArt = vi.fn(() => new Promise<{ cancel_requested: boolean }>(resolve => {
    acknowledgements.push(() => resolve({ cancel_requested: true }));
  }));
  const api = { scene: { sourceArt, cancelSourceArt } } as unknown as EditorApi;
  const old = [acquireSourceArt(api, selection), acquireSourceArt(api, { ...selection, asset: { ...asset, id: "second" } })];
  const flush = async () => { for (let i = 0; i < 24; i++) await Promise.resolve(); };
  try {
    expect(sourceArt).toHaveBeenCalledTimes(1);
    clearSourceArt(api); await flush();
    const fresh = acquireSourceArt(api, selection);
    expect(sourceArt).toHaveBeenCalledTimes(1); expect(cancelSourceArt).toHaveBeenCalledTimes(1);
    acknowledgements[0](); await flush();
    expect(await fresh.promise).toMatchObject({ kind: "waveform" }); expect(sourceArt).toHaveBeenCalledTimes(2);
    await expect(old[0].promise).rejects.toHaveProperty("name", "AbortError");
    await expect(old[1].promise).rejects.toHaveProperty("name", "AbortError");
    fresh.release();
  } finally { clearSourceArt(api); acknowledgements.forEach(finish => finish()); old.forEach(lease => lease.release()); await flush(); }
});

it("revokes evicted JPEG URLs exactly once and rebuilds a released entry after reauthorization", async () => {
  const create = vi.fn().mockReturnValueOnce("blob:first").mockReturnValueOnce("blob:second").mockReturnValueOnce("blob:rebuilt");
  const revoke = vi.fn(); vi.stubGlobal("URL", { createObjectURL: create, revokeObjectURL: revoke });
  const cache = new SourceArtCache<DisplayArt>({ bytes: 4096, entries: 1, concurrent: 2 });
  const jpeg = Uint8Array.from([255, 216, 255, 217]);
  const selected: ArtSelection = { ...selection, query: { kind: "thumbnail" } };
  const value: SourceArtResponse = { ...response({ project_id: "project", revision: "fixed", asset_id: asset.id, request_id: "test", query: selected.query }),
    identity: { ...identity, algorithm: "classic-thumbnail-1s-jpeg08-v1" }, data: { kind: "thumbnail_ready", width: 1, height: 1,
      content_hash: bytesToHex(sha256(jpeg)), jpeg_bytes_hex: bytesToHex(jpeg), observation: { kind: "static_image", source_width: 1, source_height: 1 } } };
  const load = vi.fn(async () => allocateArt(value, selected));
  try {
    const first = cache.acquire("first", load, vi.fn()); expect(await first.promise).toMatchObject({ url: "blob:first" }); first.release();
    const second = cache.acquire("second", load, vi.fn()); expect(await second.promise).toMatchObject({ url: "blob:second" }); second.release();
    expect(revoke.mock.calls).toEqual([["blob:first"]]);
    const rebuilt = cache.acquire("first", load, vi.fn()); expect(await rebuilt.promise).toMatchObject({ url: "blob:rebuilt" });
    expect(load).toHaveBeenCalledTimes(3); expect(revoke.mock.calls).toEqual([["blob:first"], ["blob:second"]]);
    rebuilt.release(); cache.clear(); cache.clear();
    expect(revoke.mock.calls).toEqual([["blob:first"], ["blob:second"], ["blob:rebuilt"]]);
  } finally { cache.clear(); }
});

it("keeps a budget receipt, waits for user intent and deduplicates retry without changing the fixed source", async () => {
  vi.useFakeTimers();
  let finish!: (value: SourceArtResponse) => void;
  const sourceArt = vi.fn(async (request: SourceArtRequest) => response(request));
  sourceArt.mockRejectedValueOnce(new OpenCutApiError("SOURCE_ART_BUDGET_EXCEEDED", "native limit", null, 422, "budget-request", []));
  sourceArt.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const api = { scene: { sourceArt, cancelSourceArt: vi.fn(async () => ({})) } } as unknown as EditorApi;
  const flush = async () => { for (let i = 0; i < 24; i++) await Promise.resolve(); };
  try {
    render(<SourceArtView api={api} selection={selection} />);
    await act(async () => { Visibility.instances[0].show(); await flush(); });
    expect(screen.getByLabelText("Source artwork unavailable").textContent).toContain("resource limit");
    expect(screen.getByLabelText(/technical details/).getAttribute("title")).toContain("budget-request");
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(sourceArt).toHaveBeenCalledOnce();
    const retry = screen.getByRole("button", { name: /Retry source artwork/ });
    await act(async () => { fireEvent.click(retry); fireEvent.click(retry); await flush(); });
    expect(sourceArt).toHaveBeenCalledTimes(2);
    expect(screen.getByLabelText("Loading source artwork")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Retry source artwork/ })).toBeNull();
    const first = sourceArt.mock.calls[0][0], retried = sourceArt.mock.calls[1][0];
    expect({ ...retried, request_id: first.request_id }).toEqual(first);
    expect(retried.request_id).not.toBe(first.request_id);
    await act(async () => { finish(response(retried)); await flush(); });
    expect(screen.getByRole("img", { name: "Source peaks before element gain" })).toBeTruthy();
    expect(screen.queryByLabelText("Source artwork unavailable")).toBeNull();
  } finally { clearSourceArt(api); }
});

it("retains an authorization failure receipt, hides stale art and offers reconnect instead of an artwork retry", async () => {
  vi.useFakeTimers();
  const sourceArt = vi.fn(async (request: SourceArtRequest) => {
    const value = response(request);
    if (value.data.kind === "waveform" && request.query.kind === "waveform") value.data.amplitudes = Array(request.query.projection.bins).fill(.5);
    return value;
  });
  const api = { scene: { sourceArt, cancelSourceArt: vi.fn(async () => ({})) } } as unknown as EditorApi;
  const flush = async () => { for (let i = 0; i < 24; i++) await Promise.resolve(); };
  try {
    render(<SourceArtView api={api} selection={selection} />);
    await act(async () => { Visibility.instances[0].show(); await flush(); });
    sourceArt.mockRejectedValueOnce(new OpenCutApiError("NOT_AUTHORIZED", "A valid bearer token is required", null, 401, "auth-request", []));
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(screen.getByLabelText("Source artwork unavailable").textContent).toContain("Reconnect");
    expect(screen.queryByRole("img", { name: "Source peaks before element gain" })).toBeNull();
    expect(screen.getByLabelText(/technical details/).getAttribute("title")).toContain("auth-request");
    expect(screen.queryByRole("button", { name: /Retry source artwork/ })).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(sourceArt).toHaveBeenCalledTimes(2);
  } finally { clearSourceArt(api); }
});

it("fences a captured retry after a revision change and ignores its late source result", async () => {
  let finish!: (value: SourceArtResponse) => void;
  const sourceArt = vi.fn(async (request: SourceArtRequest) => response(request));
  sourceArt.mockRejectedValueOnce(new Error("read failed"));
  sourceArt.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const cancelSourceArt = vi.fn(async () => ({}));
  const api = { scene: { sourceArt, cancelSourceArt } } as unknown as EditorApi;
  let retry: (() => void) | undefined;
  function Harness({ selected }: { selected: ArtSelection }) {
    const state = useSourceArt(api, selected); retry = state?.retry;
    return <ArtDisplay art={state?.art} error={state?.error} />;
  }
  try {
    const view = render(<Harness selected={selection} />);
    await screen.findByLabelText("Source artwork unavailable");
    const captured = retry!;
    act(captured);
    await waitFor(() => expect(sourceArt).toHaveBeenCalledTimes(2));
    const original = sourceArt.mock.calls[1][0];
    view.rerender(<Harness selected={{ ...selection, revision: "new-fixed" }} />);
    act(captured);
    await act(async () => { finish(response(original)); });
    await screen.findByRole("img", { name: "Source peaks before element gain" });
    expect(sourceArt).toHaveBeenCalledTimes(3);
    expect(sourceArt.mock.calls[2][0].revision).toBe("new-fixed");
    expect(cancelSourceArt).toHaveBeenCalledWith({ project_id: "project", revision: "fixed", request_id: original.request_id });
  } finally { clearSourceArt(api); }
});

it("recovers manually after a failed authorization probe without automatically resuming probes", async () => {
  vi.useFakeTimers();
  const sourceArt = vi.fn(async (request: SourceArtRequest) => {
    const value = response(request);
    if (value.data.kind === "waveform" && request.query.kind === "waveform") value.data.amplitudes = Array(request.query.projection.bins).fill(.5);
    return value;
  });
  const api = { scene: { sourceArt, cancelSourceArt: vi.fn(async () => ({})) } } as unknown as EditorApi;
  const flush = async () => { for (let i = 0; i < 24; i++) await Promise.resolve(); };
  try {
    render(<SourceArtView api={api} selection={selection} />);
    await act(async () => { Visibility.instances[0].show(); await flush(); });
    sourceArt.mockRejectedValueOnce(new OpenCutApiError("SOURCE_ART_BUDGET_EXCEEDED", "probe budget", null, 422, "probe-request", []));
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(screen.queryByRole("img", { name: "Source peaks before element gain" })).toBeNull();
    expect(screen.getByLabelText(/technical details/).getAttribute("title")).toContain("probe-request");
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(sourceArt).toHaveBeenCalledTimes(2);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /Retry source artwork/ })); await flush(); });
    expect(sourceArt).toHaveBeenCalledTimes(3);
    expect(screen.getByRole("img", { name: "Source peaks before element gain" })).toBeTruthy();
  } finally { clearSourceArt(api); }
});

it("does not present an asset permission rejection as a session expiration or a retryable decode failure", () => {
  render(<ArtDisplay error={sourceArtFailure(new OpenCutApiError("NOT_AUTHORIZED", "source access denied", null, 403, "permission-request", []))}
    retryHint="Zoom in to retry source artwork."
    onRetry={() => { throw new Error("must not retry unauthorized art"); }} />);
  expect(screen.getByLabelText("Source artwork unavailable").textContent).toContain("permissions");
  expect(screen.getByLabelText("Source artwork unavailable").textContent).not.toContain("Reconnect");
  expect(screen.getByLabelText("Source artwork unavailable").textContent).not.toContain("Zoom in");
  expect(screen.queryByRole("button")).toBeNull();
  expect(screen.getByLabelText(/technical details/).title).toContain("permission-request");
});

it("keeps source preview available as a separate failure action without retrying the artwork", () => {
  const retry = vi.fn(), preview = vi.fn();
  render(<ArtDisplay error={sourceArtFailure(new OpenCutApiError("SOURCE_ART_BUDGET_EXCEEDED", "limit", null, 422, "receipt", []))}
    onRetry={retry} onPreview={preview} retryName="source.mp4" />);
  const button = screen.getByRole("button", { name: "Preview source source.mp4" });
  fireEvent.click(button);
  expect(preview).toHaveBeenCalledOnce(); expect(retry).not.toHaveBeenCalled();
  expect(button.closest("button button")).toBeNull();
});

it("announces queued or running artwork generation separately from a resource-limit failure", () => {
  const view = render(<ArtDisplay loading />);
  expect(screen.getByRole("status").textContent).toBe("Waiting for or generating thumbnails/waveforms…");
  expect(screen.queryByLabelText("Source artwork unavailable")).toBeNull();
  view.rerender(<ArtDisplay error={sourceArtFailure(new OpenCutApiError("SOURCE_ART_BUDGET_EXCEEDED", "Source artwork request queue is full (256 active or waiting requests)", null, 422, "queue-full", []))} />);
  expect(screen.queryByRole("status")).toBeNull();
  expect(screen.getByLabelText("Source artwork unavailable").textContent).toContain("See technical details");
});
