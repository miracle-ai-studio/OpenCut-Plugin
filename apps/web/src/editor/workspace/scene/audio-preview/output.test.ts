import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PreviewAudioOutput } from "./output";
import { WorkerPreviewPump } from "./worker-pump";
import type { DeviceReceipt } from "./device-port";

vi.mock("./worklet.ts?worker&url", () => ({ default: "/worklet.js" }));
vi.mock("./worker-pump", () => ({
  WorkerPreviewPump: class {
    ready = Promise.resolve();
    ownsDevice = false;
    receipt: DeviceReceipt | null = null;
    constructor(_token: string, _connection: unknown, private take: () => MessagePort,
      readonly progress: (receipt: DeviceReceipt) => void) {}
    start() { this.take(); this.ownsDevice = true; }
    freeze() {}
    async stopOutput() { this.progress(this.receipt!); return this.receipt!.consumedFrames; }
  },
}));
let rate = 48000;
let outputTime = 500.02;
class Device {
  static latest: Device;
  sampleRate = rate;
  state = "running";
  currentTime = 500;
  destination = {};
  getOutputTimestamp = () => ({ contextTime: outputTime, performanceTime: performance.now() });
  onstatechange: (() => void) | null = null;
  createDynamicsCompressor = vi.fn(() => {throw Error("Device must not master native PCM again");});
  createGain = vi.fn(() => {throw Error("Device must not add an output gain to native PCM");});
  audioWorklet = { addModule: vi.fn(async () => {}) };
  resume = vi.fn(async () => {});
  close = vi.fn(async () => {
    this.state = "closed";
  });
  constructor() { Device.latest = this; }
}
class Node {
  static latest: Node;
  consumed = "0";
  clock:MessagePort|null=null;
  onprocessorerror: (() => void) | null = null;
  connect = vi.fn();
  disconnect = vi.fn();
  port = {
    onmessage: null as ((event: MessageEvent) => void) | null,
    close: vi.fn(),
    start: vi.fn(),
    postMessage: vi.fn((message: { kind: string; token?: string;port?:MessagePort }) => {
      if(message.kind==="clock-port")this.clock=message.port!;
      if (message.kind === "stop")
        queueMicrotask(() =>
          this.emit({
            kind: "stopped",
            token: "generation",
            consumedFrames: this.consumed,
          }),
        );
    }),
  };
  constructor() {
    Node.latest = this;
  }
  emit(data: unknown) {
    const value = data as Record<string, unknown>;
    if (value.kind === "progress" && value.token === "generation") this.consumed = String(value.consumedFrames);
    if (value.kind === "progress") data = { renderFrame: 24000000 + Number(value.consumedFrames) - Number(value.copied),
      quantumFrames: 128, runStartFrame: 24000000, runStartConsumed: "0", phase: "playing", ...value };
    if(value.kind==="progress")this.clock?.postMessage(data);
    this.port.onmessage?.({ data } as MessageEvent);
  }
}
beforeEach(() => {
  rate = 48000;
  outputTime = 500.02;
  vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  vi.stubGlobal("AudioContext", Device);
  vi.stubGlobal("AudioWorkletNode", Node);
  vi.stubGlobal("MessageChannel",class {
    port1={onmessage:null as ((event:{data:unknown})=>void)|null,start:vi.fn(),close:vi.fn(),postMessage:(data:unknown)=>this.port2.onmessage?.({data})};
    port2={onmessage:null as ((event:{data:unknown})=>void)|null,start:vi.fn(),close:vi.fn(),postMessage:(data:unknown)=>this.port1.onmessage?.({data})};
  });
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

it("presents native-mastered samples without a second compressor, gain or look-ahead",async()=>{
  const output=new PreviewAudioOutput("generation",48000,vi.fn(),vi.fn());
  try {
    await output.ready;
    expect(Node.latest.connect).toHaveBeenCalledExactlyOnceWith(Device.latest.destination);
    expect(Device.latest.createDynamicsCompressor).not.toHaveBeenCalled();
    expect(Device.latest.createGain).not.toHaveBeenCalled();
    const channel=new MessageChannel();
    expect(output.bindPresentationClock(channel.port1).graphLatencySeconds).toBe(0);
  }finally{output.silence();await output.stop();}
});

it("publishes the valid device mapping before the first PCM content interval",async()=>{
  const progress=vi.fn(),output=new PreviewAudioOutput("generation",48000,progress,vi.fn());
  try {
    await output.ready;
    Node.latest.emit({kind:"progress",token:"generation",consumedFrames:"0",copied:0,completedBlocks:0,
      renderFrame:24000000,runStartFrame:null,runStartConsumed:"0",phase:"preparing"});
    vi.mocked(requestAnimationFrame).mock.calls.at(-1)![0](0);
    expect(progress).toHaveBeenLastCalledWith(expect.objectContaining({output:null,consumedFrames:"0",stalled:true,
      deviceTimestamp:expect.objectContaining({contextTime:outputTime,performanceTime:expect.any(Number),timeOrigin:performance.timeOrigin})}));
  } finally {output.silence();await output.stop();}
});

it("keeps thirty-two transfer slots until the actual processor completes a block and ignores stale progress", async () => {
  const progress = vi.fn(),
    completedBlock = vi.fn(),
    output = new PreviewAudioOutput("generation", 48000, progress, vi.fn(), completedBlock);
  const abort = new AbortController();
  try {
    await output.ready;
    for(let i=0;i<32;i++) await output.push(new Float32Array(8), abort.signal);
    let released = false;
    const third = output.push(new Float32Array(8), abort.signal).then(() => {
      released = true;
    });
    Node.latest.emit({
      kind: "progress",
      token: "old",
      completedBlocks: 1,
      consumedFrames: "4",
      copied: 4,
    });
    await Promise.resolve();
    expect(released).toBe(false);
    expect(progress).not.toHaveBeenCalled();
    expect(completedBlock).not.toHaveBeenCalled();
    Node.latest.emit({ kind: "progress", token: "generation", completedBlocks: 0,
      consumedFrames: "2", copied: 2 });
    expect(completedBlock).not.toHaveBeenCalled();
    Node.latest.emit({
      kind: "progress",
      token: "generation",
      completedBlocks: 1,
      consumedFrames: "4",
      copied: 4,
    });
    await third;
    expect(completedBlock).toHaveBeenCalledOnce();
    // Receipt credit is immediate; presentation samples once at screen refresh.
    expect(progress).not.toHaveBeenCalled();
    vi.mocked(requestAnimationFrame).mock.calls.at(-1)![0](0);
    expect(progress).toHaveBeenLastCalledWith({
      consumedFrames: "4",
      stalled: false,
      contextTime: 24000000 / 48000,
      output: { frames: "4", source: "device_timestamp", uncertainty_frames: null },
      atRefresh:true,
      deviceTimestamp:expect.objectContaining({contextTime:outputTime,timeOrigin:performance.timeOrigin,performanceTime:expect.any(Number)}),
    });
    expect(
      Node.latest.port.postMessage.mock.calls.filter(
        ([m]) => m.kind === "block",
      ),
    ).toHaveLength(33);
    Node.latest.emit({ kind: "progress", token: "generation", completedBlocks: 1,
      consumedFrames: "6", copied: 2 });
    expect(completedBlock).toHaveBeenCalledOnce();
    output.silence();
    Node.latest.emit({ kind: "progress", token: "generation", completedBlocks: 2,
      consumedFrames: "8", copied: 2 });
    expect(completedBlock).toHaveBeenCalledOnce();
    expect(Node.latest.disconnect).toHaveBeenCalledOnce();
    const stop = output.stop();
    expect(await stop).toBe("8");
    expect(output.stop()).toBe(stop);
    expect(
      Node.latest.port.postMessage.mock.calls.filter(
        ([m]) => m.kind === "stop",
      ),
    ).toHaveLength(1);
  } finally {
    abort.abort();
    await output.stop();
  }
});
it("rejects a device sample-rate mismatch before installing a worklet or playing service PCM", async () => {
  rate = 44100;
  const output = new PreviewAudioOutput("generation", 48000, vi.fn(), vi.fn());
  await expect(output.ready).rejects.toThrow("sample rate");
  expect(await output.stop()).toBe("0");
});
it("keeps the output graph alive until the last native-mastered sample reaches the device", async () => {
  vi.useFakeTimers();
  vi.advanceTimersByTime(10);
  outputTime = 500;
  const output = new PreviewAudioOutput("generation", 48000, vi.fn(), vi.fn());
  await output.ready;
  await output.push(new Float32Array(256), new AbortController().signal);
  Node.latest.emit({ kind: "progress", token: "generation", completedBlocks: 1, consumedFrames: "128", copied: 128 });
  let closed = false;
  const stop = output.stop().then(value => { closed = true; return value; });
  await vi.advanceTimersByTimeAsync(1);
  expect(closed).toBe(false);
  expect(Node.latest.disconnect).not.toHaveBeenCalled();
  expect(Device.latest.close).not.toHaveBeenCalled();
  outputTime = 500.02;
  await vi.advanceTimersByTimeAsync(10);
  expect(await stop).toBe("128");
  expect(Node.latest.disconnect).toHaveBeenCalledOnce();
  expect(Device.latest.close).toHaveBeenCalledOnce();
});
it("does not substitute an independent wall clock when the browser cannot observe device output", async () => {
  vi.stubGlobal("AudioContext", class extends Device {
    constructor() { super(); Object.defineProperty(this, "getOutputTimestamp", { value: undefined }); }
  });
  const output = new PreviewAudioOutput("generation", 48000, vi.fn(), vi.fn());
  await expect(output.ready).rejects.toThrow("output clock");
  expect(await output.stop()).toBe("0");
});
it("uses only the worker's final receipt after transferring the device port", async () => {
  const failure = vi.fn();
  const output = new PreviewAudioOutput("generation", 48000, vi.fn(), failure);
  await output.ready;
  Node.latest.emit({ kind: "progress", token: "generation", completedBlocks: 0,
    consumedFrames: "0", copied: 0, runStartFrame: null, phase: "preparing" });
  const pump = await output.createPump({} as Parameters<PreviewAudioOutput["createPump"]>[0], vi.fn(), failure);
  pump.start({} as Parameters<WorkerPreviewPump["start"]>[0]);
  const transferred = pump as unknown as {
    receipt: DeviceReceipt; progress(receipt: DeviceReceipt): void;
  };
  transferred.receipt = { kind: "progress", token: "generation", completedBlocks: 1,
    consumedFrames: "128", copied: 128, renderFrame: 24000000, quantumFrames: 128,
    runStartFrame: 24000000, runStartConsumed: "0", phase: "playing" };
  transferred.progress(transferred.receipt);
  expect(await output.stop()).toBe("128");
  expect(failure).not.toHaveBeenCalled();
  expect(Device.latest.close).toHaveBeenCalledOnce();
});
