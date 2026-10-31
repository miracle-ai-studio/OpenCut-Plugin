import { afterEach, expect, it, vi } from "vitest";
vi.mock("./visual-preview-renderer", () => ({
  VisualPreviewRenderer: class {},
}));
import { VisualPreview } from "./visual-preview";
import type { VisualPreviewRenderer } from "./visual-preview-renderer";
import type { SessionHost } from "./session";
import type { AudioPreviewSession, AudioPreviewState } from "./types";
import { setupAudioPreview } from "./audio-preview-fixture";
const flush = async () => {
  for (let i = 0; i < 30; i++) await Promise.resolve();
};
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});
async function fixture(bitmaps = false, manualScreen = false) {
  const f = await setupAudioPreview();
  let state = f.state();
  let activeApi = f.api;
  const s = f.sessionFor({
    project_id: state.head.project.id,
    revision: state.head.revision,
    sequence_id: state.sequenceId,
    sample_rate: 48000,
    start_time: state.playhead,
    idempotency_key: "picture",
  });
  s.buffered_frames = "32768";
  const calls: {
    session: unknown;
    signal: AbortSignal;
    point?: { x: number; y: number };
    resolve: (time: number) => void;
    reject: (error: Error) => void;
  }[] = [];
  const close = vi.fn();
  const released: ReturnType<typeof vi.fn>[] = [];
  const renderer = {
    close,
    render: vi.fn(
      (
        _api,
        session,
        signal,
        point?: { x: number; y: number },
        _draft?: Parameters<VisualPreviewRenderer["render"]>[4],
        preparation?: Parameters<VisualPreviewRenderer["render"]>[5],
      ) =>
        new Promise((resolve, reject) =>
          calls.push({
            session,
            signal,
            point,
            reject,
            resolve: (time) => {
              const release = vi.fn();
              released.push(release);
              resolve({
                frame: {
                  prepare_ahead: !!preparation,
                  next_time: {
                    ticks: String(Math.round(time * 48000) + 1600),
                    timebase: 48000,
                  },
                  plan: {
                    identity: {
                      time: {
                        ticks: String(Math.round(time * 48000)),
                        timebase: 48000,
                      },
                    },
                  },
                  time_seconds: time,
                },
                pixels: {
                  width: 1,
                  height: 1,
                  data: new Uint8ClampedArray([255, 0, 0, 255]),
                  ...(bitmaps ? { close: release } : {}),
                },
                runtime: { gpuBackend: "mock" },
                ...(point
                  ? {
                      hitTest: {
                        point,
                        hits: [{ elementId: "opaque", alpha: 255 }],
                      },
                    }
                  : {}),
              });
            },
          }),
        ),
    ),
  };
  const failure = vi.fn();
  const host = { api: () => activeApi, state: () => state, generation:()=>1, viewGeneration:()=>1 } as SessionHost;
  let frameId = 0;
  const frames = new Map<number, FrameRequestCallback>();
  const paint = () => {
    const pending = [...frames]; frames.clear();
    for (const [, callback] of pending) callback(0);
  };
  const visual = new VisualPreview(
    host,
    failure,
    () => renderer as unknown as VisualPreviewRenderer,
    { request: callback => {
      const id = ++frameId; frames.set(id, callback);
      if (!manualScreen) queueMicrotask(() => {
        const next = frames.get(id); frames.delete(id); next?.(0);
      });
      return id;
    }, cancel: id => { frames.delete(id); } },
  );
  const observe = (
    port: ReturnType<typeof visual.start>,
    session: AudioPreviewSession,
    phase: AudioPreviewState["phase"] = "playing",
  ) => {
    state = {
      ...state,
      audioPreview: { ...state.audioPreview, session, phase },
    };
    port.observe(state.audioPreview);
  };
  return {
    visual,
    calls,
    close,
    failure,
    s,
    observe,
    released,
    renderer,
    paint,
    frames,
    api: f.api,
    audio: f.preview,
    cleanup: f.cleanup,
    replaceApi: (next: typeof f.api) => {
      activeApi = next;
    },
    state: () => state,
  };
}

it("plays an already displayed static session without recreating its native owner", async () => {
  const f = await fixture();
  let playing: Promise<void> | undefined;
  try {
    const shown = f.visual.showStill();
    await flush();
    f.calls[0].resolve(0);
    await shown;
    const original = f.calls[0].session as AudioPreviewSession;
    playing = f.audio.play(48000, f.visual.start());
    void playing.catch(() => {});
    await flush();
    expect(f.api.scene.audioPreviewCreate).toHaveBeenCalledOnce();
    expect(vi.mocked(f.api.scene.audioPreviewControl).mock.calls.filter(([r]) => r.command.kind === "stop")).toHaveLength(0);
    expect((f.calls[1].session as AudioPreviewSession).session_id).toBe(original.session_id);
    f.calls[1].resolve(0);
    await flush();
    f.calls[2].resolve(1 / 30);
    await flush();
    f.calls[3].resolve(2 / 30);
    await playing;
  } finally {
    for (const call of f.calls) call.resolve(0);
    f.cleanup();
    f.visual.dispose();
    await playing?.catch(() => {});
  }
});

it("retires an in-flight static owner at Play intent before sound initialization", async () => {
  const f = await fixture(), create = vi.mocked(f.api.scene.audioPreviewCreate);
  const original = create.getMockImplementation()!;
  let release!: () => void, signal: AbortSignal | undefined;
  create.mockImplementationOnce(async (request, pendingSignal) => {
    signal = pendingSignal;
    await new Promise<void>(resolve => { release = resolve; });
    return original(request, pendingSignal);
  });
  const shown = f.visual.showStill().catch(error => error);
  try {
    await flush();
    expect(create).toHaveBeenCalledOnce();
    expect(signal?.aborted).toBe(false);
    const port = f.visual.start();
    expect(signal?.aborted).toBe(true);
    let admitted = false;
    const ready = port.beforeCreate!().then(() => { admitted = true; });
    await flush();
    expect(admitted).toBe(false);
    release();
    await shown;
    await ready;
    expect(f.calls).toHaveLength(0);
    expect(vi.mocked(f.api.scene.audioPreviewControl).mock.calls.filter(([request]) => request.command.kind === "stop")).toHaveLength(1);
  } finally {
    release?.();
    await shown;
    await f.visual.retireStill();
    f.visual.dispose();
  }
});

it("retires a paused owner whose first picture is still rendering instead of transferring it",async()=>{
  const f=await fixture();const shown=f.visual.showStill();
  try {
    await flush();expect(f.calls).toHaveLength(1);
    const port=f.visual.start();
    expect(f.calls[0].signal.aborted).toBe(true);
    const request=vi.mocked(f.api.scene.audioPreviewCreate).mock.calls[0][0];
    expect(port.takePaused?.(f.api,request)).toBeNull();
    f.calls[0].resolve(0);await shown;await port.beforeCreate!();
    expect(vi.mocked(f.api.scene.audioPreviewControl).mock.calls.filter(([r])=>r.command.kind==="stop")).toHaveLength(1);
  } finally {f.calls[0]?.resolve(0);await shown;f.cleanup();f.visual.dispose();}
});

it("keeps early static retirement failure on the sound admission fence", async () => {
  const f = await fixture();
  try {
    const shown = f.visual.showStill();
    await flush();
    f.calls[0].resolve(0);
    await shown;
    vi.mocked(f.api.scene.audioPreviewControl).mockRejectedValueOnce(Error("Static retirement failed"));
    const port = f.visual.start();
    await expect(port.beforeCreate!()).rejects.toThrow("Static retirement failed");
    expect(f.api.scene.audioPreviewCreate).toHaveBeenCalledOnce();
  } finally { await f.visual.retireStill(); f.visual.dispose(); }
});

it("refreshes the current paused sound owner without creating a second preview session", async () => {
  const f = await fixture(true);
  const session = { ...f.s, state: "paused" as const };
  f.state().audioPreview = { ...f.state().audioPreview, phase: "paused", session };
  try {
    const shown = f.visual.showStill();
    await flush();
    expect(f.calls).toHaveLength(1);
    f.calls[0].resolve(0);
    await shown;
    expect(f.api.scene.audioPreviewCreate).not.toHaveBeenCalled();
    expect(f.api.scene.audioPreviewControl).not.toHaveBeenCalled();
    expect(f.calls[0].session).toMatchObject({ session_id: session.session_id, lease_id: session.lease_id });
    expect(f.visual.pausedEditSession()).toBe(session);
  } finally { await f.visual.retireStill(); f.visual.dispose(); }
});

it.each(["loading","playing","stalled","unconfirmed"] as const)("does not interrupt %s playback with a static refresh", async phase => {
  const f=await fixture();
  f.state().audioPreview.phase=phase;
  try {
    await f.visual.showStill();
    expect(f.calls).toHaveLength(0);
    expect(f.api.scene.audioPreviewCreate).not.toHaveBeenCalled();
  } finally {f.visual.dispose();}
});

it("rejects a paused owner from another revision instead of opening a replacement", async () => {
  const f=await fixture();
  f.state().audioPreview={...f.state().audioPreview,phase:"paused",session:{...f.s,state:"paused",identity:{...f.s.identity,revision:"retired"}}};
  try {
    await expect(f.visual.showStill()).rejects.toThrow("paused preview no longer matches");
    expect(f.calls).toHaveLength(0);
    expect(f.api.scene.audioPreviewCreate).not.toHaveBeenCalled();
  } finally {f.visual.dispose();}
});

it("previews and restores static opacity on the existing stopped-picture owner without starting sound", async () => {
  const f=await fixture(true);
  try {
    const shown=f.visual.showStill(); await flush();
    expect(f.calls).toHaveLength(1); f.calls[0].resolve(0); await shown;
    expect(f.state().audioPreview.session).toBeNull();
    const owner=f.visual.pausedEditSession(); expect(owner?.state).toBe("paused");
    // Stop/pause without an active sound owner can leave this legitimate state.
    f.state().audioPreview.phase="paused";
    expect(f.visual.pausedEditSession()?.session_id).toBe(owner?.session_id);
    const draft=f.visual.previewEditDraft({...transformInput(f),intent:{kind:"property",property:"opacity"}},new AbortController().signal);
    await flush(); f.calls[1].resolve(0); await draft;
    expect(f.visual.snapshot().editDraftId).toBe("draft");
    const restored=f.visual.clearEditDraft("draft"); await flush();
    f.calls[2].resolve(0); expect(await restored).toBe(true);
    expect(f.visual.pausedEditSession()?.session_id).toBe(owner?.session_id);
    expect(f.state().audioPreview.session).toBeNull(); // No forged audio state.
    expect(f.visual.snapshot().editDraftId).toBeNull();
  } finally {await f.visual.retireStill(); f.visual.dispose();}
});

it("refills cached frames to the bounded lookahead without another device observation", async () => {
  const f = await fixture(true, true);
  const cachedFrame = vi.fn(async (_api:unknown, time:{ticks:string;timebase:number}) => {
    const tick = Number(time.ticks) * 48000 / time.timebase;
    const frame = Math.floor(tick / 1600);
    return { pixels: { width: 1, height: 1, close: vi.fn() }, runtime: {}, frame: {
      prepare_ahead: true, plan: {identity: {time: {ticks: String(frame * 1600), timebase: 48000}}},
      next_time: {ticks: String((frame + 1) * 1600), timebase: 48000}, time_seconds: frame / 30,
    }};
  });
  Object.assign(f.renderer, {
    prepareCache: async () => ({project_id:f.s.identity.project_id,revision:f.s.identity.revision,
      sequence_id:f.s.identity.sequence_id,frame_step_seconds:1/30}), cachedFrame,
  });
  const port = f.visual.start();
  const preparing=port.prepare(f.s, new AbortController().signal, vi.fn());
  await flush();f.calls[0].resolve(0);await preparing;
  expect(cachedFrame).toHaveBeenCalledTimes(9); // shown first frame plus eight queued
  // Device PCM is already queued: a low native producer FIFO must not stop
  // cheap cached decoding while that actual output cursor continues to advance.
  f.s.buffered_frames="0";f.s.stalled=true;f.observe(port,f.s);
  port.consume!(f.s, "6400");
  f.paint();
  port.consume!(f.s, "6400");
  for(let i=0;i<5;i++)await flush();
  expect(cachedFrame).toHaveBeenCalledTimes(13); // all four consumed slots refilled
  expect(f.visual.snapshot().time).toBe(4/30);
  port.close();
  expect(JSON.parse(f.visual.snapshot().lastRun!)).toMatchObject({lastFrameTime:4/30,presentedFrames:2});
});

it("uses the current refresh's output position without nesting a second screen callback", async()=>{
  const f=await fixture(true,true),port=f.visual.start();
  const prepared=port.prepare(f.s,new AbortController().signal,vi.fn());
  await flush();f.calls[0].resolve(0);await flush();f.calls[1].resolve(1/30);await flush();f.calls[2].resolve(2/30);await prepared;
  port.consume!(f.s,"1600",true);
  expect(f.visual.snapshot().time).toBe(1/30);
  expect(f.frames.size).toBe(0);
  port.close();
});

it("prepares future frames without presenting them early and retires queued/late pixels on replacement", async () => {
  const f = await fixture(true),
    port = f.visual.start(),
    credit = vi.fn();
  const prepared = port.prepare(f.s, new AbortController().signal, credit);
  await flush();
  f.calls[0].resolve(0);
  await flush();
  f.calls[1].resolve(1 / 30);
  await flush();
  f.calls[2].resolve(2 / 30);
  await prepared;
  expect(f.visual.snapshot().time).toBe(0);
  expect(credit).toHaveBeenLastCalledWith({ticks:"4800",timebase:48000});
  port.consume!(f.s, "1600");
  await flush();
  expect(f.visual.snapshot().time).toBe(1 / 30);
  expect(f.released[0]).toHaveBeenCalledOnce();
  await flush();
  const creditsBeforeRetirement=credit.mock.calls.length;
  port.suspend();
  expect(f.released[1]).toHaveBeenCalledOnce();
  expect(f.released[2]).toHaveBeenCalledOnce();
  f.calls[3].resolve(0.1);
  await flush();
  expect(f.released[3]).toHaveBeenCalledOnce();
  expect(f.visual.snapshot().pixels).toBeNull();
  expect(credit).toHaveBeenCalledTimes(creditsBeforeRetirement);
});

it("retains an explicit pause picture but releases it at final retirement",async()=>{
 const f=await fixture(true),port=f.visual.start();
 const prepared=port.prepare(f.s,new AbortController().signal);
 await flush();f.calls[0].resolve(0);await prepared;
 const pixels=f.visual.snapshot().pixels;
 port.suspend(true);
 expect(f.visual.snapshot().pixels).toBe(pixels);
 expect(f.released[0]).not.toHaveBeenCalled();
 f.observe(port,{...f.s,generation:"2",state:"paused"},"paused");
 expect(f.visual.snapshot().pixels).toBe(pixels);
 port.close();
 expect(f.released[0]).toHaveBeenCalledOnce();
});

it("releases presented and late bitmap ownership at replacement/stop without publishing canceled pixels", async () => {
  const f = await fixture(true),
    port = f.visual.start();
  const prepare = port.prepare(f.s, new AbortController().signal);
  await flush();
  f.calls[0].resolve(0);
  await prepare;
  f.observe(port, {
    ...f.s,
    presentation_time: { ticks: "50", timebase: 1000 },
  });
  await flush();
  port.suspend();
  expect(f.released[0]).toHaveBeenCalledTimes(1);
  f.calls[1].resolve(0.05);
  await flush();
  expect(f.released[1]).toHaveBeenCalledTimes(1);
  expect(f.visual.snapshot().pixels).toBeNull();
  port.close();
  expect(f.released[0]).toHaveBeenCalledTimes(1);
});
it("coalesces consumer observations and holds the last authorized picture while overdue replacements are discarded", async () => {
  const f = await fixture(),
    port = f.visual.start();
  try {
    const prepare = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await prepare;
    const at = (n: number) => ({
      ...f.s,
      presentation_time: { ticks: String(n), timebase: 1000 },
    });
    f.observe(port, at(50));
    await flush();
    f.observe(port, at(100));
    f.observe(port, at(200));
    f.observe(port, at(300));
    expect(f.calls).toHaveLength(2);
    expect(f.visual.snapshot().time).toBe(0);
    expect(f.visual.snapshot().pixels).not.toBeNull();
    f.calls[1].resolve(0.05);
    await flush();
    expect(f.calls).toHaveLength(3);
    expect(f.visual.snapshot().time).toBe(0);
    expect(f.visual.snapshot().pixels).not.toBeNull();
    f.calls[2].resolve(0.3);
    await flush();
    expect(f.visual.snapshot().time).toBe(0.3);
  } finally {
    port.close();
  }
});
it("seek cancels and drains an old frame before preparing the new generation; late pixels never publish", async () => {
  const f = await fixture(),
    port = f.visual.start();
  try {
    const first = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await first;
    f.observe(port, {
      ...f.s,
      presentation_time: { ticks: "50", timebase: 1000 },
    });
    await flush();
    port.suspend();
    expect(f.calls[1].signal.aborted).toBe(true);
    expect(f.visual.snapshot().pixels).toBeNull();
    const next = port.prepare(
      {
        ...f.s,
        generation: "2",
        presentation_time: { ticks: "2", timebase: 1 },
      },
      new AbortController().signal,
    );
    await flush();
    expect(f.calls).toHaveLength(2);
    f.calls[1].resolve(0.05);
    await flush();
    expect(f.calls).toHaveLength(3);
    expect(f.visual.snapshot().pixels).toBeNull();
    f.calls[2].resolve(2);
    await next;
    expect(f.visual.snapshot()).toMatchObject({ generation: "2", time: 2 });
    expect(f.failure).not.toHaveBeenCalled();
  } finally {
    port.close();
  }
});
it("playing without fresh consumer observations removes its picture and close releases resources", async () => {
  const f = await fixture(),
    port = f.visual.start();
  const first = port.prepare(f.s, new AbortController().signal);
  await flush();
  f.calls[0].resolve(0);
  await first;
  vi.useFakeTimers();
  f.observe(port, f.s);
  await flush();
  expect(f.calls).toHaveLength(1);
  expect(f.visual.snapshot().pixels).not.toBeNull();
  vi.advanceTimersByTime(151);
  expect(f.visual.snapshot().pixels).toBeNull();
  port.close();
  expect(f.close).toHaveBeenCalled();
  expect(f.visual.snapshot().phase).toBe("idle");
});

it("rejects overdue replacement pixels but holds the last picture while server confirmation is delayed", async () => {
  const f = await fixture(),
    port = f.visual.start();
  try {
    const first = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await first;
    f.observe(port, {
      ...f.s,
      presentation_time: { ticks: "1", timebase: 1000 },
    });
    await flush();
    port.consume?.(f.s, "7424"); // 154.666ms, server still confirms zero.
    expect(f.visual.snapshot().time).toBe(0);
    expect(f.visual.snapshot().pixels).not.toBeNull();
    f.calls[1].resolve(0);
    await flush();
    expect(f.visual.snapshot().pixels).not.toBeNull();
    expect(f.visual.snapshot().time).toBe(0);
    expect(f.failure).not.toHaveBeenCalled();
  } finally {
    port.close();
  }
});

it("retains the fixed session for paused authority checks after mutable UI content changes", async () => {
  const f = await fixture();
  const sequence = f
    .state()
    .head.project.sequences.find((s) => s.id === f.s.identity.sequence_id)!;
  sequence.tracks[0].elements.push({
    enabled: true,
    content: { kind: "graphic" },
  } as never);
  sequence.captions.push({} as never);
  const port = f.visual.start();
  try {
    const first = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await first;
    vi.useFakeTimers();
    f.observe(port, f.s, "paused");
    // Mutable UI content cannot select a different authority or fixed session.
    sequence.tracks[0].elements.pop();
    sequence.captions.length = 0;
    vi.mocked(f.api.scene.audioPreviewControl).mockResolvedValue({ ...f.s, state: "paused" });
    await vi.advanceTimersByTimeAsync(1000);
    expect(f.api.scene.audioPreviewControl).toHaveBeenCalledWith(
      expect.objectContaining({
        session_id: f.s.session_id, generation: f.s.generation,
        owner_id: f.s.owner_id, lease_id: f.s.lease_id,
        command: { kind: "heartbeat" },
      }),
      expect.anything(),
    );
    expect(vi.mocked(f.api.sdk.call).mock.calls.filter(([name]) => name === "opencut_visual_preview_frame")).toHaveLength(0);
  } finally {
    port.close();
  }
});

async function pausedAuthorityFixture() {
  const f = await fixture(true), port = f.visual.start();
  const first = port.prepare(f.s, new AbortController().signal);
  await flush(); f.calls[0].resolve(0); await first;
  const paused = { ...f.s, state: "paused" as const };
  vi.mocked(f.api.scene.audioPreviewControl).mockResolvedValue(paused);
  vi.useFakeTimers(); f.observe(port, paused, "paused"); await flush();
  return { ...f, port, paused };
}

it("checks paused authority every second without creating unclaimed source-resolution pictures", async () => {
  const f = await pausedAuthorityFixture();
  try {
    const rendered = f.renderer.render.mock.calls.length;
    await vi.advanceTimersByTimeAsync(3000);
    const heartbeats = vi.mocked(f.api.scene.audioPreviewControl).mock.calls;
    expect(heartbeats).toHaveLength(3);
    expect(heartbeats.every(([input]) => input.command.kind === "heartbeat")).toBe(true);
    expect(new Set(heartbeats.map(([input]) => input.control_id)).size).toBe(3);
    expect(f.renderer.render).toHaveBeenCalledTimes(rendered);
    expect(vi.mocked(f.api.sdk.call).mock.calls.filter(([name]) => name === "opencut_visual_preview_frame")).toHaveLength(0);
    expect(f.visual.snapshot().pixels).not.toBeNull();
    expect(f.port.hasPausedPicture?.(f.paused)).toBe(true);
  } finally { f.port.close(); }
});

it.each(["generation", "overlay_epoch", "owner_id", "lease_id", "revision", "sequence", "state"])(
  "retires a paused picture when heartbeat returns changed %s", async field => {
    const f = await pausedAuthorityFixture();
    try {
      const next = structuredClone(f.paused);
      if (field === "revision") next.identity.revision = "foreign";
      else if (field === "sequence") next.identity.sequence_id = "foreign";
      else if (field === "state") Object.assign(next, { state: "playing" });
      else Object.assign(next, { [field]: "foreign" });
      vi.mocked(f.api.scene.audioPreviewControl).mockResolvedValue(next);
      await vi.advanceTimersByTimeAsync(1000);
      expect(f.visual.snapshot()).toMatchObject({ phase: "failed", pixels: null });
      expect(f.port.hasPausedPicture?.(f.paused)).toBe(false);
    } finally { f.port.close(); }
  },
);

it("clears the paused picture on fresh heartbeat authorization failure", async () => {
  const f = await pausedAuthorityFixture();
  try {
    vi.mocked(f.api.scene.audioPreviewControl).mockRejectedValue(Error("permission revoked"));
    await vi.advanceTimersByTimeAsync(1000);
    expect(f.visual.snapshot()).toMatchObject({ phase: "failed", pixels: null, error: "permission revoked" });
  } finally { f.port.close(); }
});

it("ignores a late old-authority failure after the next picture generation is ready", async () => {
  const f = await pausedAuthorityFixture();
  let reject!: (error: Error) => void;
  try {
    vi.mocked(f.api.scene.audioPreviewControl).mockImplementationOnce(() => new Promise((_yes, no) => { reject = no; }));
    await vi.advanceTimersByTimeAsync(1000);
    const next = { ...f.paused, generation: "2" };
    f.observe(f.port, next, "paused"); await flush();
    f.calls.at(-1)!.resolve(0); await flush();
    reject(Error("old generation revoked")); await flush();
    expect(f.visual.snapshot().phase).not.toBe("failed");
    expect(f.visual.snapshot().generation).toBe("2");
    expect(f.visual.snapshot().pixels).not.toBeNull();
  } finally { f.port.close(); }
});

it("rejects a preview session for another revision before rendering", async () => {
  const f = await fixture(),
    port = f.visual.start();
  try {
    await expect(
      port.prepare(
        { ...f.s, identity: { ...f.s.identity, revision: "other" } },
        new AbortController().signal,
      ),
    ).rejects.toThrow("fixed project view");
    expect(f.renderer.render).not.toHaveBeenCalled();
  } finally {
    port.close();
  }
});

it("reserves simultaneous paused alpha queries before either awaits the renderer", async () => {
  const f = await fixture(true),
    port = f.visual.start();
  try {
    const prepared = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await prepared;
    f.observe(port, f.s, "paused");
    await flush();
    const first = f.visual.hitTest({ x: 0, y: 0 }),
      second = f.visual.hitTest({ x: 0, y: 0 });
    await flush();
    expect(f.calls).toHaveLength(2);
    expect(f.calls[1].point).toEqual({ x: 0, y: 0 });
    f.calls[1].resolve(0);
    await first;
    await flush();
    expect(f.calls).toHaveLength(3);
    f.calls[2].resolve(0);
    expect((await second).hits).toEqual([{ elementId: "opaque", alpha: 255 }]);
    expect(f.released[1]).toHaveBeenCalledTimes(1);
    expect(f.released[2]).toHaveBeenCalledTimes(1);
    expect(f.visual.snapshot().phase).toBe("ready");
  } finally {
    port.close();
  }
});
it("a finishing presentation cannot erase queued query ownership or enter it concurrently", async () => {
  const f = await fixture(),
    port = f.visual.start();
  try {
    const prepared = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await prepared;
    f.observe(
      port,
      { ...f.s, presentation_time: { ticks: "50", timebase: 1000 } },
      "paused",
    );
    await flush();
    expect(f.calls).toHaveLength(2);
    const first = f.visual.hitTest({ x: 0, y: 0 }),
      second = f.visual.hitTest({ x: 0, y: 0 });
    f.calls[1].resolve(0.05);
    await flush();
    expect(f.calls).toHaveLength(3);
    f.observe(
      port,
      { ...f.s, presentation_time: { ticks: "100", timebase: 1000 } },
      "paused",
    );
    await flush();
    expect(f.calls).toHaveLength(3);
    f.calls[2].resolve(0.05);
    await first;
    await flush();
    expect(f.calls).toHaveLength(4);
    f.calls[3].resolve(0.05);
    await second;
    await flush();
    expect(f.calls).toHaveLength(5);
    f.calls[4].resolve(0.1);
    await flush();
    expect(f.visual.snapshot().time).toBe(0.1);
  } finally {
    port.close();
  }
});

function transformInput(
  f: Awaited<ReturnType<typeof fixture>>,
  draftId = "draft",
) {
  const state = f.state();
  return {
    draftId,
    intent: {kind:"gesture" as const,gesture:"translate" as const},
    sequenceId: state.sequenceId,
    time: structuredClone(state.playhead),
    transaction: {
      project_id: state.head.project.id,
      expected_revision: state.head.revision,
      idempotency_key: "original-transform",
      label: "Transform selection",
      operations: [],
    },
  };
}
it("coalesces transform drafts into the existing render queue and clears only the owning draft", async () => {
  const f = await fixture(true),
    port = f.visual.start();
  try {
    const prepared = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await prepared;
    f.observe(port, f.s, "paused");
    await flush();
    const first = f.visual
      .previewEditDraft(transformInput(f), new AbortController().signal)
      .catch((error) => error);
    await flush();
    expect(f.calls).toHaveLength(2);
    const skipped = f.visual
      .previewEditDraft(transformInput(f), new AbortController().signal)
      .catch((error) => error);
    const latest = f.visual.previewEditDraft(
      transformInput(f),
      new AbortController().signal,
    );
    await flush();
    expect(f.calls).toHaveLength(2);
    f.calls[1].resolve(0);
    await flush();
    expect((await first).name).toBe("AbortError");
    expect((await skipped).name).toBe("AbortError");
    expect(f.released[1]).toHaveBeenCalledOnce();
    expect(f.calls).toHaveLength(3);
    expect(f.renderer.render.mock.calls[2][4]).toMatchObject({
      draftId: "draft",
      transaction: { idempotency_key: "original-transform" },
    });
    f.calls[2].resolve(0);
    await latest;
    const pixels = f.visual.snapshot().pixels;
    f.visual.clearEditDraft("unrelated-draft");
    expect(f.visual.snapshot().pixels).toBe(pixels);
    const restored = f.visual.clearEditDraft("draft");
    expect(f.visual.snapshot().phase).toBe("loading");
    expect(f.visual.snapshot().pixels).toBe(pixels);
    expect(f.visual.snapshot().restoringDraftId).toBe("draft");
    await flush();
    expect(f.calls).toHaveLength(4);
    f.calls[3].resolve(0);
    expect(await restored).toBe(true);
    expect(f.released[2]).toHaveBeenCalledOnce();
    expect(f.visual.snapshot().phase).toBe("ready");
    expect(f.visual.snapshot().editDraftId).toBeNull();
    expect(f.visual.snapshot().restoringDraftId).toBeNull();
  } finally {
    port.close();
  }
});
it("retires an in-flight baseline restoration when the same opacity gesture moves again", async () => {
  const f=await fixture(true),port=f.visual.start();
  try {
    const original=port.prepare(f.s,new AbortController().signal); await flush();
    f.calls[0].resolve(0); await original; f.observe(port,f.s,"paused"); await flush();
    const input={...transformInput(f),intent:{kind:"property" as const,property:"opacity" as const}};
    const first=f.visual.previewEditDraft(input,new AbortController().signal); await flush();
    f.calls[1].resolve(0); await first; const pixels=f.visual.snapshot().pixels;
    const restoration=f.visual.clearEditDraft("draft"); await flush();
    expect(f.calls).toHaveLength(3);
    f.visual.supersedeEditDraft("draft");
    const latest=f.visual.previewEditDraft(input,new AbortController().signal); await flush();
    expect(f.calls).toHaveLength(3); // Original blocking phase must finish first.
    f.calls[2].resolve(0); await flush();
    expect(await restoration).toBe(false);
    expect(f.released[2]).toHaveBeenCalledOnce();
    expect(f.visual.snapshot().pixels).toBe(pixels); // No obsolete baseline flash.
    expect(f.calls).toHaveLength(4);
    f.calls[3].resolve(0); await latest;
    expect(f.visual.snapshot().phase).toBe("ready");
    expect(f.visual.snapshot().editDraftId).toBe("draft");
  } finally {port.close();}
});

it("drops a late transform picture after cancellation and restores the original paused frame", async () => {
  const f = await fixture(true),
    port = f.visual.start();
  try {
    const prepared = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await prepared;
    f.observe(port, f.s, "paused");
    await flush();
    const abort = new AbortController();
    const draft = f.visual
      .previewEditDraft(transformInput(f), abort.signal)
      .catch((error) => error);
    await flush();
    abort.abort();
    f.calls[1].resolve(0);
    await flush();
    expect((await draft).name).toBe("AbortError");
    expect(f.released[1]).toHaveBeenCalledOnce();
    expect(f.calls).toHaveLength(3);
    f.calls[2].resolve(0);
    await flush();
    expect(f.visual.snapshot().pixels).not.toBeNull();
  } finally {
    port.close();
  }
});

it("retires a transform immediately when the consumer starts playing and discards its late pixels", async () => {
  const f = await fixture(true),
    port = f.visual.start();
  try {
    const prepared = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await prepared;
    f.observe(port, f.s, "paused");
    await flush();
    const draft = f.visual
      .previewEditDraft(transformInput(f), new AbortController().signal)
      .catch((error) => error);
    await flush();
    f.observe(port, f.s, "playing");
    expect(f.visual.snapshot().pixels).toBeNull();
    f.calls[1].resolve(0);
    await flush();
    expect((await draft).name).toBe("AbortError");
    expect(f.released[1]).toHaveBeenCalledOnce();
    f.calls[2].resolve(0);
    await flush();
    expect(f.visual.snapshot().phase).toBe("ready");
  } finally {
    port.close();
  }
});

it("rejects a transform result captured before the API connection is replaced", async () => {
  const f = await fixture(true),
    port = f.visual.start();
  try {
    const prepared = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await prepared;
    f.observe(port, f.s, "paused");
    await flush();
    const draft = f.visual
      .previewEditDraft(transformInput(f), new AbortController().signal)
      .catch((error) => error);
    await flush();
    const original = f.visual.snapshot().pixels;
    f.replaceApi(Object.create(f.api));
    f.calls[1].resolve(0);
    await flush();
    expect((await draft).name).toBe("AbortError");
    expect(f.released[1]).toHaveBeenCalledOnce();
    expect(f.visual.snapshot().pixels).toBe(original);
  } finally {
    port.close();
  }
});

it("does not publish transform pixels after the fixed preview generation changes", async () => {
  const f = await fixture(true),
    port = f.visual.start();
  try {
    const prepared = port.prepare(f.s, new AbortController().signal);
    await flush();
    f.calls[0].resolve(0);
    await prepared;
    f.observe(port, f.s, "paused");
    await flush();
    const draft = f.visual
      .previewEditDraft(transformInput(f), new AbortController().signal)
      .catch((error) => error);
    await flush();
    const next = { ...f.s, generation: "2" };
    f.observe(port, next, "paused");
    f.calls[1].resolve(0);
    await flush();
    expect((await draft).name).toBe("AbortError");
    expect(f.released[1]).toHaveBeenCalledOnce();
    expect(f.visual.snapshot().pixels).toBeNull();
  } finally {
    port.close();
  }
});

it("continues from the device cursor without jumping over missing picture credit", async () => {
  const f = await fixture(true),
    port = f.visual.start();
  const prepared = port.prepare(f.s, new AbortController().signal, vi.fn());
  await flush();
  f.calls[0].resolve(0);
  await flush();
  f.calls[1].resolve(1 / 30);
  await flush();
  f.calls[2].resolve(2 / 30);
  await prepared;
  port.consume!(f.s, "6000");
  await flush();
  expect(f.renderer.render.mock.calls[3][5]?.time).toEqual({
    ticks: "6000",
    timebase: 48000,
  });
  port.close();
  f.calls[3].resolve(0.125);
  await flush();
  expect(f.released[3]).toHaveBeenCalledOnce();
});

it("a slow frame cannot create a future hole in the contiguous picture credit", async () => {
  const f = await fixture(true), port = f.visual.start();
  let now = 0;
  vi.spyOn(performance, "now").mockImplementation(() => now);
  const prepared = port.prepare(f.s, new AbortController().signal, vi.fn());
  await flush(); now = 600; f.calls[0].resolve(0); await flush();
  now = 610; f.calls[1].resolve(1 / 30); await flush();
  now = 620; f.calls[2].resolve(2 / 30); await prepared;
  port.consume!(f.s, "6000"); await flush();
  const requested = f.renderer.render.mock.calls[3][5]!.time!;
  const ahead = Number(requested.ticks) / requested.timebase - .125;
  expect(ahead).toBe(0);
  port.close(); f.calls[3].resolve(.125); await flush();
});

it("coalesces output observations at the next screen refresh and shows only the newest due picture", async () => {
  const f = await fixture(true, true), port = f.visual.start();
  const prepared = port.prepare(f.s, new AbortController().signal, vi.fn());
  await flush(); f.calls[0].resolve(0); await flush();
  f.calls[1].resolve(1 / 30); await flush(); f.calls[2].resolve(2 / 30); await prepared;
  port.consume!(f.s, "1600"); port.consume!(f.s, "3200");
  expect(f.frames.size).toBe(1);
  expect(f.visual.snapshot().time).toBe(0);
  f.paint();
  expect(f.visual.snapshot()).toMatchObject({ time: 2 / 30, presentedFrames: 2, discardedFrames: 1 });
  expect(f.released[0]).toHaveBeenCalledOnce();
  expect(f.released[1]).toHaveBeenCalledOnce();
  port.consume!(f.s, "4000");
  expect(f.frames.size).toBe(1);
  port.close(); expect(f.frames.size).toBe(0);
  f.paint(); expect(f.visual.snapshot().pixels).toBeNull();
  await flush();
});

it("holds one bitmap through missed deadlines, counts skipped pictures honestly and clears on authority failure", async () => {
  const f = await fixture(true, true), port = f.visual.start();
  const prepared = port.prepare(f.s, new AbortController().signal, vi.fn());
  await flush(); f.calls[0].resolve(0); await flush();
  f.calls[1].resolve(1 / 30); await flush(); f.calls[2].resolve(2 / 30); await prepared;
  const held = f.visual.snapshot().pixels;
  port.consume!(f.s, "16000"); f.paint();
  expect(f.visual.snapshot()).toMatchObject({ phase: "ready", time: 0, presentedFrames: 1, discardedFrames: 2 });
  expect(f.visual.snapshot().pixels).toBe(held);
  expect(f.released[0]).not.toHaveBeenCalled();
  port.consume!(f.s, "20000");
  f.observe(port, f.s, "failed");
  expect(f.frames.size).toBe(0); expect(f.visual.snapshot().pixels).toBeNull();
  expect(f.released[0]).toHaveBeenCalledOnce();
  f.paint(); expect(f.visual.snapshot().phase).toBe("idle");
  port.close(); await flush();
});

it("keeps admitted queued pictures on memory pressure and retries within confirmed picture credit with fewer resident bytes", async () => {
  const f = await fixture(true),
    port = f.visual.start();
  try {
    const ready = port.prepare(f.s, new AbortController().signal, vi.fn());
    await flush();
    f.calls[0].resolve(0);
    await flush();
    f.calls[1].resolve(1 / 30);
    await flush();
    f.calls[2].resolve(2 / 30);
    await ready;
    port.consume!(f.s, "1600");
    await flush();
    const request = f.renderer.render.mock.calls[3][5]!;
    f.calls[3].reject(
      Object.assign(new Error("Queue consumes the frame admission"), {
        code: "VISUAL_BUDGET_EXCEEDED",
      }),
    );
    await flush();
    expect(f.visual.snapshot().phase).toBe("ready");
    port.consume!(f.s, "3200");
    await flush();
    const retry = f.renderer.render.mock.calls[4][5]!;
    expect(Number(retry.time!.ticks) / retry.time!.timebase).toBeGreaterThanOrEqual(Number(request.time!.ticks) / request.time!.timebase);
    expect(Number(retry.time!.ticks) / retry.time!.timebase).toBeLessThanOrEqual(2 / 30 + .9);
    expect(retry.reservedBytes).toBeLessThan(request.reservedBytes);
    f.calls[4].resolve(0.1);
    await flush();
  } finally {
    port.close();
  }
});

it("drops a missed prepared picture without failing the independent audio clock",async()=>{
 const f=await fixture(true),port=f.visual.start();
 try{
  const ready=port.prepare(f.s,new AbortController().signal,vi.fn());await flush();f.calls[0].resolve(0);await flush();f.calls[1].resolve(1/30);await flush();f.calls[2].resolve(2/30);await ready;
  port.consume!(f.s,"1600");await flush();
  f.calls[3].reject(Object.assign(new Error("Picture passed its presentation window"),{code:"PREVIEW_FRAME_STALE"}));await flush();
  expect(f.visual.snapshot().phase).not.toBe("failed");
  port.consume!(f.s,"3200");await flush();expect(f.calls).toHaveLength(5);
  f.calls[4].resolve(.15);await flush();
 }finally{port.close();}
});


it("preserves low-PCM backpressure but refills a picture-starved stalled consumer once PCM credit is available", async () => {
  const f = await fixture(true), port = f.visual.start();
  try {
    const ready = port.prepare(f.s, new AbortController().signal, vi.fn());
    await flush(); f.calls[0].resolve(0); await flush();
    f.calls[1].resolve(1 / 30); await flush(); f.calls[2].resolve(2 / 30); await ready;
    const low = { ...f.s, stalled: true, buffered_frames: "4096" };
    f.observe(port, low); port.consume!(low, "1600"); await flush();
    expect(f.calls).toHaveLength(3);
    expect(f.failure).not.toHaveBeenCalled();
    f.observe(port, { ...low, buffered_frames: "8192" }); await flush();
    expect(f.calls).toHaveLength(4);
    f.calls[3].resolve(.2); await flush();
  } finally { port.close(); }
});

it("keeps one stable visible-picture snapshot across repeated stale device observations", async () => {
  const f = await fixture(true), port = f.visual.start();
  try {
    const ready = port.prepare(f.s, new AbortController().signal);
    await flush(); f.calls[0].resolve(0); await ready;
    const changed = vi.fn(), unsubscribe = f.visual.subscribe(changed);
    port.consume!(f.s, "8000");
    const held = f.visual.snapshot();
    expect(held.pixels).not.toBeNull();
    for (let i = 0; i < 100; i++) port.consume!(f.s, "8000");
    expect(changed).not.toHaveBeenCalled();
    expect(f.visual.snapshot()).toBe(held);
    unsubscribe();
  } finally { port.close(); }
});


it("shows the current frame while cold cache publication is pending without granting sound credit",async()=>{
 const f=await fixture(true);let release!:()=>void,entered!:()=>void;
 const gate=new Promise<void>(resolve=>{release=resolve;}),started=new Promise<void>(resolve=>{entered=resolve;});
 Object.assign(f.renderer,{prepareCache:async()=>{entered();await gate;throw new DOMException("Canceled","AbortError");},cachedFrame:vi.fn()});
 const port=f.visual.start(),ready=vi.fn(),abort=new AbortController();
 const preparing=port.prepare(f.s,abort.signal,ready).catch(()=>{});
 try{await flush();f.calls[0].resolve(0);await started;
  expect(f.visual.snapshot()).toMatchObject({phase:"loading",time:0,target:JSON.stringify([f.s.identity.project_id,f.s.identity.revision,f.s.identity.sequence_id])});
  expect(f.visual.snapshot().pixels).not.toBeNull();expect(ready).not.toHaveBeenCalled();
 }finally{abort.abort();release();await preparing;port.close();}
});
