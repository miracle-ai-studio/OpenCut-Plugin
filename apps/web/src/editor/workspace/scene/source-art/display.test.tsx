// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { EditorApi } from "../../api";
import { initialSnapshot } from "../fixture";
import type { GainDraft } from "../types";
import { TimelineArt, waveformTemporaryGain, waveformViewport } from "../timeline-art";
import { allocateWaveformDisplay } from "./display";
import type { WaveformDisplayRequest, WaveformDisplayResponse, WaveformDisplaySelection } from "./display";
import { clearSourceArt } from "./service";
import type { SourceArtRequest, SourceArtResponse } from "./service";
import { ArtDisplay, useSourceArt } from "./view";

const asset = initialSnapshot().project.assets[0];
const identity = { algorithm: "source-peaks128-v1", decode_profile: "fixed-cas-native-libav-concat-linear-skia-jpeg",
  worker_sha256: "a".repeat(64), module_sha256: "c".repeat(64), rasterizer: "skia-safe-0.153.3-srgb-jpeg80",
  platform: { os: "test", architecture: "test", kernel_release: "test", system_build: "test" },
  decoder_libraries: ["avcodec", "avformat", "avutil", "swscale", "swresample"].map(name => ({name, version: "test", configuration_sha256: "d".repeat(64), binary_sha256: "e".repeat(64)})) };
const selection: WaveformDisplaySelection = { projectId: "project", revision: "fixed", asset, display: {
  sequence_id: "scene", track_id: "audio", element_id: "clip", sample_rate: 48000, temporary_gain: null,
  viewport: { clip_left_px: .25, clip_right_px: 4.25, pixels_per_second: 80, css_height: 3.5, device_pixel_ratio: 2 } } };
function receipt(request: WaveformDisplayRequest): WaveformDisplayResponse {
  return {  project_id: request.project_id, revision: request.revision, asset_id: request.asset_id,
    version: asset.version, identity, sequence_id: request.sequence_id, track_id: request.track_id, element_id: request.element_id,
    display_profile: "classic-waveform-display-v1", sample_rate: 48000, channels: 1, total_samples: "48000", bucket_size: 128,
    viewport: request.viewport, temporary_gain: request.temporary_gain,
    canvas: { width: 8, height: 7, css_left: .25, css_width: 4, css_height: 3.5 }, bars: [
      { source_start_sample: "100", source_end_sample: "200", peak: .25, gain: 1, output_amplitude: .25,
        height_fraction: .5, burn: false, device_left: 0, device_right: 2, device_top: 4, device_height: 3, burn_height: 0 },
      { source_start_sample: "200", source_end_sample: "300", peak: 2, gain: 1, output_amplitude: 2,
        height_fraction: 1, burn: true, device_left: 4, device_right: 6, device_top: 0, device_height: 7, burn_height: 2 } ] };
}
const request = (selected = selection): WaveformDisplayRequest => ({ project_id: selected.projectId, revision: selected.revision,
  asset_id: selected.asset.id, request_id: "fixture", ...selected.display });
const flush = async () => { for (let i = 0; i < 24; i++) await Promise.resolve(); };
const context = { setTransform: vi.fn(), clearRect: vi.fn(), fillRect: vi.fn(), fillStyle: "" };
beforeEach(() => { vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(context as unknown as CanvasRenderingContext2D);
  context.fillRect.mockClear(); context.clearRect.mockClear(); });
afterEach(() => { cleanup(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

it("paints server device bars and burn, redraws same-size gain changes and releases the canvas", () => {
  const response = receipt(request()), allocation = allocateWaveformDisplay(response, selection);
  const view = render(<ArtDisplay art={allocation.materialize().value} />);
  const canvas = screen.getByRole("img", { name: "Classic element waveform" });
  expect(context.fillRect.mock.calls).toEqual([[0, 4, 2, 3], [4, 0, 2, 7], [4, 0, 2, 2]]);
  expect(canvas).toHaveProperty("width", 8); expect(canvas).toHaveProperty("height", 7);
  const changed = structuredClone(response); changed.temporary_gain = -120;
  changed.bars[0].device_top = 6; changed.bars[0].device_height = 1;
  view.rerender(<ArtDisplay art={allocateWaveformDisplay(changed, { ...selection,
    display: { ...selection.display, temporary_gain: -120 } }).materialize().value} />);
  // React need not reset unchanged width/height props after effect cleanup.
  expect(canvas).toHaveProperty("width", 8); expect(canvas).toHaveProperty("height", 7);
  expect(context.fillRect.mock.calls.slice(3)).toEqual([[0, 6, 2, 1], [4, 0, 2, 7], [4, 0, 2, 2]]);
  expect(canvas.getAttribute("data-temporary-gain")).toBe("-120");
  view.unmount(); expect(canvas).toHaveProperty("width", 0); expect(canvas).toHaveProperty("height", 0);
});

it("rebuilds the same mounted canvas after an unavailable context and a refreshed artwork receipt", () => {
  vi.mocked(HTMLCanvasElement.prototype.getContext).mockReturnValueOnce(null);
  const allocation = allocateWaveformDisplay(receipt(request()), selection);
  const view = render(<ArtDisplay art={allocation.materialize().value} />);
  const canvas = view.container.querySelector("canvas")!;
  expect(screen.getByLabelText("Source artwork unavailable")).toBeTruthy();
  expect(canvas.hidden).toBe(true); expect(canvas.width).toBe(0); expect(canvas.height).toBe(0);
  view.rerender(<ArtDisplay art={allocation.materialize().value} />);
  expect(screen.queryByLabelText("Source artwork unavailable")).toBeNull();
  expect(screen.getByRole("img", { name: "Classic element waveform" })).toBe(canvas);
  expect(canvas.width).toBe(8); expect(canvas.height).toBe(7);
  expect(context.fillRect.mock.calls).toEqual([[0, 4, 2, 3], [4, 0, 2, 7], [4, 0, 2, 2]]);
  view.unmount(); expect(canvas.width).toBe(0); expect(canvas.height).toBe(0);
});

it("rejects a mismatched scope/profile and excessive or impossible canvas geometry before allocation", () => {
  const valid = receipt(request());
  expect(() => allocateWaveformDisplay({ ...valid, display_profile: "other" }, selection)).toThrow("invalid");
  expect(() => allocateWaveformDisplay({ ...valid, element_id: "other" }, selection)).toThrow("invalid");
  expect(() => allocateWaveformDisplay({ ...valid, temporary_gain: -120 }, selection)).toThrow("invalid");
  expect(() => allocateWaveformDisplay({ ...valid, canvas: { ...valid.canvas, width: 8388609, height: 1 } }, selection)).toThrow("invalid");
  for (const change of [{ css_left: 0 }, { css_width: 3 }, { css_height: 4 }])
    expect(() => allocateWaveformDisplay({ ...valid, canvas: { ...valid.canvas, ...change } }, selection)).toThrow("invalid");
  const bad = structuredClone(valid); bad.bars[0].device_top = Infinity;
  expect(() => allocateWaveformDisplay(bad, selection)).toThrow("invalid");
  // Positive low DPR can round a last bar wholly outside the canvas, and
  // a minimum one-device-pixel bar may start at its bottom clipping edge.
  const clipped = structuredClone(valid); clipped.canvas.width = 2;
  clipped.bars[0].device_top = 7; clipped.bars[0].device_height = 1;
  clipped.bars[1].device_left = 2; clipped.bars[1].device_right = 3;
  expect(() => allocateWaveformDisplay(clipped, selection)).not.toThrow();
  // EOF can have a source-start beyond a clamped end; the service owns this zero-peak fact.
  const eof = structuredClone(valid); eof.bars[0].source_start_sample = "50000";
  eof.bars[0].source_end_sample = "48000";
  expect(() => allocateWaveformDisplay(eof, selection)).not.toThrow();
});

it("publishes only the last partially clipped viewport after rapid zoom and temporary-gain changes", async () => {
  const pending: { input: WaveformDisplayRequest; signal: AbortSignal; finish(value: WaveformDisplayResponse): void }[] = [];
  const waveformDisplay = vi.fn((input: WaveformDisplayRequest, signal?: AbortSignal) =>
    new Promise<WaveformDisplayResponse>(finish => { pending.push({ input, signal: signal!, finish }); }));
  const cancelSourceArt = vi.fn(async () => ({ cancel_requested: true }));
  const api = { scene: { waveformDisplay, cancelSourceArt } } as unknown as EditorApi;
  function Display({ selected }: { selected: WaveformDisplaySelection }) {
    const state = useSourceArt(api, selected); return <ArtDisplay art={state?.art} error={state?.error} />;
  }
  const selected = (left: number, scale: number, gain: number | null): WaveformDisplaySelection => ({ ...selection,
    display: { ...selection.display, temporary_gain: gain, viewport: { ...selection.display.viewport,
      clip_left_px: left, clip_right_px: left + 4, pixels_per_second: scale } } });
  const complete = (index: number) => {
    const value = receipt(pending[index].input);
    value.canvas.css_left = pending[index].input.viewport.clip_left_px;
    pending[index].finish(value);
  };
  try {
    const view = render(<Display selected={selection} />); await act(flush);
    view.rerender(<Display selected={selected(10.25, 120, -120)} />); await act(flush);
    view.rerender(<Display selected={selected(40.25, 160, null)} />); await act(flush);
    expect(pending).toHaveLength(1); expect(pending[0].signal.aborted).toBe(true);
    await act(async () => { complete(0); await flush(); });
    expect(pending).toHaveLength(2); expect(screen.queryByRole("img")).toBeNull();
    await act(async () => { complete(1); await flush(); });
    const canvas = screen.getByRole("img", { name: "Classic element waveform" });
    expect(canvas.style.left).toBe("40.25px"); expect(canvas.getAttribute("data-temporary-gain")).toBe("saved");
    expect(pending[1].input.viewport).toEqual(selected(40.25, 160, null).display.viewport);
    // The superseded middle viewport stayed queued and never reached native.
    expect(screen.getByRole("img", { name: "Classic element waveform" })).toBe(canvas);
    expect(canvas.style.left).toBe("40.25px"); expect(cancelSourceArt).toHaveBeenCalledTimes(1);
  } finally { clearSourceArt(api); }
});

it("cancels a temporary display on Escape, fences late success and shares raw authorization identity", async () => {
  vi.useFakeTimers();
  let finish!: (value: WaveformDisplayResponse) => void, pending!: WaveformDisplayRequest, aborted!: AbortSignal;
  const waveformDisplay = vi.fn((input: WaveformDisplayRequest, signal?: AbortSignal) => {
    if (input.temporary_gain !== null) { pending = input; aborted = signal!;
      return new Promise<WaveformDisplayResponse>(resolve => { finish = resolve; }); }
    return Promise.resolve(receipt(input));
  });
  const sourceArt = vi.fn(async (input: SourceArtRequest): Promise<SourceArtResponse> => {
    if (input.query.kind !== "waveform") throw new Error("Expected source probe");
    return {  project_id: input.project_id, revision: input.revision, asset_id: input.asset_id, version: asset.version,
      identity, data: { kind: "waveform", sample_rate: 48000, channels: 1, total_samples: "48000", bucket_size: 128,
        projection: input.query.projection, amplitudes: [.25] } };
  });
  const cancelSourceArt = vi.fn(async () => ({ cancel_requested: true }));
  const api = { scene: { waveformDisplay, sourceArt, cancelSourceArt } } as unknown as EditorApi;
  function Display({ selected }: { selected: WaveformDisplaySelection }) {
    const state = useSourceArt(api, selected); return <ArtDisplay art={state?.art} error={state?.error} />;
  }
  try {
    const view = render(<Display selected={selection} />); await act(flush);
    view.rerender(<Display selected={{ ...selection, display: { ...selection.display, temporary_gain: -120 } }} />);
    await act(flush); expect(pending.temporary_gain).toBe(-120);
    view.rerender(<Display selected={selection} />); await act(flush);
    expect(aborted.aborted).toBe(true);
    expect(cancelSourceArt).toHaveBeenCalledWith({ project_id: "project", revision: "fixed", request_id: pending.request_id });
    await act(async () => { finish(receipt(pending)); await flush(); });
    expect(screen.getByRole("img", { name: "Classic element waveform" }).getAttribute("data-temporary-gain")).toBe("saved");
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(sourceArt).toHaveBeenCalledOnce();
    expect(sourceArt.mock.calls[0][0].query).toEqual({ kind: "waveform", sample_rate: 48000,
      projection: { kind: "source", range: null, bins: 1 } });
    expect(screen.queryByLabelText("Source artwork unavailable")).toBeNull();
    sourceArt.mockRejectedValueOnce(new Error("NOT_AUTHORIZED"));
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(screen.queryByRole("img")).toBeNull(); expect(screen.getByLabelText("Source artwork unavailable")).toBeTruthy();
    await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
    expect(sourceArt).toHaveBeenCalledTimes(2);
  } finally { clearSourceArt(api); }
});

it("sends the measured viewport and exact scoped integer draft without source-clock or gain projection", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("devicePixelRatio", 1.25);
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => setTimeout(() => callback(0), 16));
  vi.stubGlobal("cancelAnimationFrame", clearTimeout);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("v4-timeline-art") ? new DOMRect(-20, 0, 320, 32) : new DOMRect(0, 0, 200, 120);
  });
  const scope = { projectId: "project", revision: "fixed", sequenceId: "scene" };
  const draft: GainDraft = { ...scope, id: "draft", trackId: "audio", elementId: "clip", originalDb: 0, tenths: -120, changed: true };
  const element = initialSnapshot().project.sequences[0].tracks[0].elements[0];
  element.id = "clip"; element.visual = null; element.content = { kind: "audio", asset_id: asset.id,
    audio: { gain: { unit: "decibels", value: 0 }, muted: false }, retime: { maintain_pitch: false,
      rate: { numerator: 2, denominator: 1 }, mapping: { kind: "classic_rounded_ticks", timebase: 120000 } } };
  const waveformDisplay = vi.fn(async (_input: WaveformDisplayRequest) => { throw new Error("CAPABILITY_UNAVAILABLE"); });
  const api = { scene: { waveformDisplay, cancelSourceArt: vi.fn(async () => ({ cancel_requested: true })) } } as unknown as EditorApi;
  try {
    render(<div className="editor-timeline-viewport"><TimelineArt api={api} scope={scope} trackId="audio" element={element}
      asset={asset} scale={80} gainDraft={draft} /></div>);
    await act(async () => { await vi.advanceTimersByTimeAsync(16); await flush(); });
    expect(waveformDisplay).toHaveBeenCalledOnce();
    expect(waveformDisplay.mock.calls[0][0]).toMatchObject({ project_id: "project", revision: "fixed", asset_id: asset.id,
      sequence_id: "scene", track_id: "audio", element_id: "clip", sample_rate: 48000, temporary_gain: -120,
      viewport: { clip_left_px: 0, clip_right_px: 320, css_height: 32, pixels_per_second: 80, device_pixel_ratio: 1.25 } });
    for (const change of [{ projectId: "other" }, { revision: "new" }, { sequenceId: "other" }, { trackId: "other" }, { elementId: "other" }, { changed: false }])
      expect(waveformTemporaryGain(scope, "audio", "clip", { ...draft, ...change })).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(4000); });
    expect(waveformDisplay).toHaveBeenCalledOnce();
  } finally { clearSourceArt(api); }
});

it("remeasures zoom while reusing a full short-clip waveform during scrolling and placement", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("devicePixelRatio", 1);
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => setTimeout(() => callback(0), 16));
  vi.stubGlobal("cancelAnimationFrame", clearTimeout);
  let clip = new DOMRect(100, 0, 400, 32);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("v4-timeline-art") ? clip : new DOMRect(200, 0, 400, 120);
  });
  const scope = { projectId: "project", revision: "fixed", sequenceId: "scene" };
  const element = initialSnapshot().project.sequences[0].tracks[0].elements[0];
  element.id = "clip"; element.visual = null; element.content = { kind: "audio", asset_id: asset.id,
    audio: { gain: { unit: "decibels", value: 0 }, muted: false }, retime: { maintain_pitch: false,
      rate: { numerator: 2, denominator: 1 }, mapping: { kind: "classic_rounded_ticks", timebase: 120000 } } };
  const waveformDisplay = vi.fn(async (input: WaveformDisplayRequest): Promise<WaveformDisplayResponse> => ({
    ...receipt(input), canvas: { width: input.viewport.clip_right_px - input.viewport.clip_left_px,
      height: input.viewport.css_height, css_left: input.viewport.clip_left_px,
      css_width: input.viewport.clip_right_px - input.viewport.clip_left_px, css_height: input.viewport.css_height } }));
  const api = { scene: { waveformDisplay, cancelSourceArt: vi.fn(async () => ({ cancel_requested: true })) } } as unknown as EditorApi;
  const component = (scale: number, selected = element) => <div className="editor-timeline-viewport">
    <TimelineArt api={api} scope={scope} trackId="audio" element={selected} asset={asset} scale={scale} /></div>;
  try {
    const view = render(component(80)); await act(flush);
    expect(waveformDisplay.mock.calls.map(([input]) => input.viewport)).toEqual([
      { clip_left_px: 0, clip_right_px: 400, pixels_per_second: 80, css_height: 32, device_pixel_ratio: 1 } ]);
    clip = new DOMRect(100, 0, 800, 32);
    view.rerender(component(160)); await act(flush);
    expect(waveformDisplay.mock.calls.map(([input]) => input.viewport)).toEqual([
      { clip_left_px: 0, clip_right_px: 400, pixels_per_second: 80, css_height: 32, device_pixel_ratio: 1 },
      { clip_left_px: 0, clip_right_px: 800, pixels_per_second: 160, css_height: 32, device_pixel_ratio: 1 } ]);
    // The snapshot revision and scale stay fixed; authored placement changes its layout identity.
    clip = new DOMRect(300, 0, 800, 32);
    const moved = structuredClone(element);
    moved.timeline_range.start = { ticks: "1", timebase: 1 };
    view.rerender(component(160, moved)); await act(flush);
    expect(waveformDisplay).toHaveBeenCalledTimes(3);
    for (let i = 0; i < 20; i++) {
      clip = new DOMRect(300 - i * 10, 0, 800, 32);
      view.container.querySelector(".editor-timeline-viewport")!.dispatchEvent(new Event("scroll"));
      await act(async () => { await vi.advanceTimersByTimeAsync(16); await flush(); });
    }
    expect(waveformDisplay).toHaveBeenCalledTimes(3);
    const canvas = screen.getByRole("img", { name: "Classic element waveform" });
    expect(canvas.style.left).toBe("0px"); expect(canvas.style.width).toBe("800px");
    expect(canvas).toHaveProperty("width", 800); expect(canvas).toHaveProperty("height", 32);
    await act(async () => { await vi.advanceTimersByTimeAsync(16); await flush(); });
    expect(waveformDisplay).toHaveBeenCalledTimes(3);
  } finally { clearSourceArt(api); }
});


it("bounds long-clip waveform tiles and keeps sub-tile scrolling stable", () => {
  expect(waveformViewport(3360, 700, 1400)).toEqual({ from: 0, through: 3360 });
  const tile = waveformViewport(100000, 1024, 1724);
  expect(tile).toEqual({ from: 512, through: 2048 });
  for (let offset = 1; offset <= 60; offset++)
    expect(waveformViewport(100000, 1024 + offset, 1724 + offset)).toEqual(tile);
  expect(waveformViewport(100000, 1600, 2300)).toEqual({ from: 1024, through: 2560 });
  const huge = waveformViewport(100000, 10000, 30000);
  expect(huge.through - huge.from).toBe(8192);
  expect(waveformViewport(9000, 8800, 9000)).toEqual({ from: 8192, through: 9000 });
});

it("anchors a failed long waveform to the visible interval without decoder churn and leaves narrow trims clear", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => setTimeout(() => callback(0), 16));
  vi.stubGlobal("cancelAnimationFrame", clearTimeout);
  let clip = new DOMRect(-1000, 0, 100000, 32);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("v4-timeline-art") ? clip : this.classList.contains("editor-track-header") ? new DOMRect(0, 0, 136, 56) : new DOMRect(0, 0, 400, 120);
  });
  const scope = { projectId: "project", revision: "fixed", sequenceId: "scene" };
  const element = initialSnapshot().project.sequences[0].tracks[0].elements[0];
  element.id = "clip"; element.content = { kind: "audio", asset_id: asset.id,
    audio: { gain: { unit: "decibels", value: 0 }, muted: false },
    retime: { maintain_pitch: false, rate: { numerator: 1, denominator: 1 }, mapping: { kind: "classic_rounded_ticks", timebase: 120000 } } };
  const waveformDisplay = vi.fn(async (_input: WaveformDisplayRequest) => { throw new Error("display failed"); });
  const api = { scene: { waveformDisplay, cancelSourceArt: vi.fn(async () => ({})) } } as unknown as EditorApi;
  const component = (scale: number) => <div className="editor-timeline-viewport"><div className="editor-track"><div className="editor-track-header" /><TimelineArt api={api} scope={scope}
    trackId="audio" element={element} asset={asset} scale={scale} /></div></div>;
  const position = () => {
    const box = screen.getByLabelText("Source artwork unavailable").parentElement!.parentElement!;
    return [box.style.left, box.style.width];
  };
  try {
    const view = render(component(80)); await act(flush);
    expect(position()).toEqual(["1144px", "248px"]);
    expect(screen.getByRole("button", { name: /Retry source artwork/ }).closest("button button")).toBeNull();
    clip = new DOMRect(-1200, 0, 100000, 32);
    view.container.firstElementChild!.dispatchEvent(new Event("scroll"));
    await act(async () => { await vi.advanceTimersByTimeAsync(16); await flush(); });
    expect(position()).toEqual(["1344px", "248px"]);
    expect(waveformDisplay).toHaveBeenCalledOnce();
    clip = new DOMRect(136, 0, 20, 32);
    view.rerender(component(40)); await act(flush);
    expect(screen.queryByRole("button", { name: /Retry source artwork/ })).toBeNull();
    expect(screen.getByLabelText("Source artwork unavailable").title).toContain("Zoom in");
    clip = new DOMRect(136, 0, 160, 32);
    view.rerender(component(120)); await act(flush);
    expect(position()).toEqual(["8px", "144px"]);
    expect(screen.getByRole("button", { name: /Retry source artwork/ })).toBeTruthy();
  } finally { clearSourceArt(api); }
});
