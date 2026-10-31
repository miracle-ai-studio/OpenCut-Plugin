import { afterEach, expect, it, vi } from "vitest";
import { PreviewConsumptionPump } from "./pump";
import { setupAudioPreview } from "../audio-preview-fixture";
import type { AudioPreviewControl } from "../types";
afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});
const flush = async () => {
  for (let i = 0; i < 20; i++) await Promise.resolve();
};
async function session() {
  const f = await setupAudioPreview();
  await f.preview.play();
  const s = f.state().audioPreview.session!;
  f.cleanup();
  return s;
}
const open = (_s: unknown, signal: AbortSignal) =>
  new Promise<Response>((_resolve, reject) =>
    signal.addEventListener("abort", () => reject(signal.reason), {
      once: true,
    }),
  );
const output = () => ({
  waitForCredit: vi.fn(async () => {}),
  push: vi.fn(async () => {}),
});
it("joins independent output and device ports without reporting unconfirmed consumption",async()=>{
 const s=await session();vi.useFakeTimers();
 const failure=vi.fn(),control=vi.fn(async(input:AudioPreviewControl)=>{
  if(input.command.kind!=="report_consumed")return s;
  const c=input.command;
  if(c.output&&BigInt(c.output.frames)>BigInt(c.consumed_frames))throw Error("Native rejected output ahead of consumption");
  return {...s,consumed_frames:c.consumed_frames,output:c.output};
 });
 const pump=new PreviewConsumptionPump({open,control},output(),()=>true,vi.fn(),failure);pump.start(s);
 pump.observe({consumedFrames:"4096",stalled:false,contextTime:1});
 pump.observeOutput({frames:"2000",source:"device_timestamp",uncertainty_frames:null},false);
 pump.completed();await flush();
 pump.observeOutput({frames:"5000",source:"device_timestamp",uncertainty_frames:null},false);
 pump.completed();await flush();
 expect(control.mock.calls[1][0].command).toMatchObject({consumed_frames:"4096",output:{frames:"2000"}});
 pump.observeOutput({frames:"1000",source:"device_timestamp",uncertainty_frames:null},true);
 pump.observe({consumedFrames:"8192",stalled:false,contextTime:2});pump.completed();await flush();
 expect(control.mock.calls[2][0].command).toMatchObject({consumed_frames:"8192",output:{frames:"5000"}});
 expect(control.mock.calls[2][0].command).toMatchObject({stalled:false});
 expect(failure).not.toHaveBeenCalled();pump.abort();
});
it("keeps raw FIFO starvation separate from sound still reaching the device", async () => {
  const s = await session();
  vi.useFakeTimers();
  const control = vi.fn(async (_input: AudioPreviewControl) => s);
  const pump = new PreviewConsumptionPump({ open, control }, output(), () => true, vi.fn(), vi.fn());
  pump.start(s);
  pump.observe({ consumedFrames: "4096", stalled: true, contextTime: 1 });
  pump.observeOutput({ frames: "2000", source: "device_timestamp", uncertainty_frames: null }, false);
  pump.completed(); await flush();
  expect(control.mock.calls[0][0].command).toMatchObject({ consumed_frames: "4096", stalled: false,
    output: { frames: "2000" } });
  pump.freeze();
  pump.observeOutput({ frames: "4096", source: "device_timestamp", uncertainty_frames: null }, true);
  await pump.reportStopped("4096");
  expect(control.mock.calls.at(-1)![0].command).toMatchObject({ consumed_frames: "4096", playing: false,
    output: { frames: "4096" } });
  pump.abort();
});
it("clears report ownership after a synchronous transport exception", async () => {
  const s = await session();
  vi.useFakeTimers();
  const failure = vi.fn(),
    control = vi.fn(() => {
      throw new Error("transport");
    });
  const pump = new PreviewConsumptionPump(
    { open, control },
    output(),
    () => true,
    vi.fn(),
    failure,
  );
  pump.start(s);
  pump.completed();
  await flush();
  pump.completed();
  await flush();
  expect(control).toHaveBeenCalledTimes(2);
  expect(failure).toHaveBeenCalledTimes(2);
  pump.abort();
});
it("serializes the final cursor after an in-flight report and keeps the old stream until cancellation", async () => {
  const s = await session();
  vi.useFakeTimers();
  let release!: () => void, streamSignal!: AbortSignal;
  const control = vi.fn(async (input) => {
    if (input.command.sequence === "1")
      await new Promise<void>((resolve) => {
        release = resolve;
      });
    return { ...s, consumed_frames: input.command.consumed_frames };
  });
  const updated = vi.fn(),
    pump = new PreviewConsumptionPump(
      {
        control,
        open: (s, signal) => {
          streamSignal = signal;
          return open(s, signal);
        },
      },
      output(),
      () => true,
      updated,
      vi.fn(),
    );
  pump.start(s);
  pump.observe({ consumedFrames: "4096", stalled: false, contextTime: 1 });
  pump.completed();
  pump.freeze();
  const final = pump.reportStopped("4128");
  expect(control).toHaveBeenCalledOnce();
  expect(streamSignal.aborted).toBe(false);
  release();
  await final;
  expect(control.mock.calls.map(([i]) => i.command)).toEqual([
    {
      kind: "report_consumed",
      output: null,
      sequence: "1",
      consumed_frames: "4096",
      playing: true,
      stalled: false,
    },
    {
      kind: "report_consumed",
      output: null,
      sequence: "2",
      consumed_frames: "4128",
      playing: false,
      stalled: false,
    },
  ]);
  expect(streamSignal.aborted).toBe(false);
  pump.abort();
  expect(streamSignal.aborted).toBe(true);
  await vi.advanceTimersByTimeAsync(1000);
  expect(control).toHaveBeenCalledTimes(2);
});
it("does not publish a report completed after cancellation", async () => {
  const s = await session();
  vi.useFakeTimers();
  let release!: () => void;
  const updated = vi.fn(),
    failure = vi.fn(),
    control = vi.fn(async () => {
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      return s;
    });
  const pump = new PreviewConsumptionPump(
    { open, control },
    output(),
    () => true,
    updated,
    failure,
  );
  pump.start(s);
  pump.completed();
  pump.abort();
  release();
  await flush();
  expect(updated).not.toHaveBeenCalled();
  expect(failure).not.toHaveBeenCalled();
});
function clock() {
  vi.useFakeTimers();
  vi.setSystemTime(0);
  let now = 0;
  vi.spyOn(performance, "now").mockImplementation(() => now);
  return (value: number) => {
    now = value;
    vi.setSystemTime(value);
  };
}
it("reports the newest pending completion before a nonurgent heartbeat after a delayed report", async () => {
  const s = await session(),
    time = clock();
  let release!: () => void, heartbeat!: () => void;
  const control = vi.fn(async (input) => {
    if (input.command.sequence === "1")
      await new Promise<void>((r) => {
        release = r;
      });
    if (input.command.kind === "heartbeat")
      await new Promise<void>((r) => {
        heartbeat = r;
      });
    return { ...s, consumed_frames: input.command.consumed_frames ?? "8192" };
  });
  const failure = vi.fn(),
    pump = new PreviewConsumptionPump(
      { open, control },
      output(),
      () => true,
      vi.fn(),
      failure,
    );
  pump.start(s);
  pump.completed();
  time(600);
  pump.observe({ consumedFrames: "4096", stalled: false, contextTime: 0 });
  pump.completed();
  pump.observe({ consumedFrames: "8192", stalled: false, contextTime: 0 });
  pump.completed();
  release();
  await flush();
  expect(control.mock.calls.map(([i]) => i.command.kind)).toEqual([
    "report_consumed",
    "report_consumed",
    "heartbeat",
  ]);
  expect(control.mock.calls[1][0].command.consumed_frames).toBe("8192");
  expect(control.mock.calls[1][0].command.sequence).toBe("2");
  expect(new Set(control.mock.calls.map(([i]) => i.control_id)).size).toBe(3);
  pump.abort();
  heartbeat();
  await flush();
  expect(failure).not.toHaveBeenCalled();
});
it("renews before pending completion when the aged server lease cannot safely defer another report", async () => {
  const s = await session(),
    time = clock();
  let release!: () => void;
  const control = vi.fn(async (input) => {
    if (input.command.sequence === "1") {
      await new Promise<void>((r) => {
        release = r;
      });
      return { ...s, lease_remaining_ms: "5600" };
    }
    return { ...s, consumed_frames: input.command.consumed_frames ?? "4096" };
  });
  const pump = new PreviewConsumptionPump(
    { open, control },
    output(),
    () => true,
    vi.fn(),
    vi.fn(),
  );
  pump.start(s);
  pump.completed();
  time(1000);
  pump.observe({ consumedFrames: "4096", stalled: false, contextTime: 0 });
  pump.completed();
  release();
  await flush();
  expect(control.mock.calls.map(([i]) => i.command.kind)).toEqual([
    "report_consumed",
    "heartbeat",
    "report_consumed",
  ]);
  pump.abort();
});
it("renews an urgent lease even before the normal heartbeat interval", async () => {
  const s = await session();
  clock();
  const control = vi.fn(async (input) => ({
    ...s,
    lease_remaining_ms:
      input.command.kind === "report_consumed" ? "4000" : "30000",
  }));
  const pump = new PreviewConsumptionPump(
    { open, control },
    output(),
    () => true,
    vi.fn(),
    vi.fn(),
  );
  pump.start(s);
  pump.completed();
  await flush();
  expect(control.mock.calls.map(([i]) => i.command.kind)).toEqual([
    "report_consumed",
    "heartbeat",
  ]);
  pump.abort();
});
it("continuous pending completions cannot starve lease renewal and controls stay serialized", async () => {
  const s = await session(),
    time = clock();
  let now = 0,
    until = 30000,
    reports = 0,
    active = 0,
    peak = 0;
  const renewals: number[] = [],
    failure = vi.fn();
  const control = vi.fn(async (input) => {
    active++;
    peak = Math.max(peak, active);
    expect(now).toBeLessThan(until);
    now += input.command.kind === "heartbeat" ? 100 : 1000;
    time(now);
    await Promise.resolve();
    if (input.command.kind === "heartbeat") {
      renewals.push(now);
      until = now + 30000;
    }
    active--;
    return { ...s, lease_remaining_ms: String(until - now) };
  });
  let pump: PreviewConsumptionPump;
  pump = new PreviewConsumptionPump(
    { open, control },
    output(),
    () => true,
    () => {
      if (++reports < 40) pump.completed();
      else pump.freeze();
    },
    failure,
  );
  pump.start(s);
  pump.completed();
  for (let k = 0; k < 50; k++) await flush();
  expect(reports).toBe(40);
  expect(peak).toBe(1);
  expect(renewals).toHaveLength(1);
  expect(renewals[0]).toBeLessThan(30000);
  expect(failure).not.toHaveBeenCalled();
  pump.abort();
});
it("final cursor waits for the active heartbeat, with no queued completion or old stream cancellation", async () => {
  const s = await session(),
    time = clock();
  let release!: () => void, stream!: AbortSignal;
  const control = vi.fn(async (input) => {
    if (input.command.kind === "heartbeat")
      await new Promise<void>((r) => {
        release = r;
      });
    return { ...s, consumed_frames: input.command.consumed_frames ?? "0" };
  });
  const pump = new PreviewConsumptionPump(
    {
      open: (s, signal) => {
        stream = signal;
        return open(s, signal);
      },
      control,
    },
    output(),
    () => true,
    vi.fn(),
    vi.fn(),
  );
  pump.start(s);
  time(600);
  pump.completed();
  await flush();
  pump.observe({ consumedFrames: "4096", stalled: false, contextTime: 0 });
  pump.completed();
  const final = pump.reportStopped("4128");
  expect(control).toHaveBeenCalledTimes(2);
  expect(stream.aborted).toBe(false);
  release();
  await final;
  expect(control.mock.calls.map(([i]) => i.command.kind)).toEqual([
    "report_consumed",
    "heartbeat",
    "report_consumed",
  ]);
  expect(control.mock.calls[2][0].command).toMatchObject({
    consumed_frames: "4128",
    playing: false,
    sequence: "2",
  });
  expect(stream.aborted).toBe(false);
  pump.abort();
  expect(stream.aborted).toBe(true);
});
it("cancellation fences a late heartbeat and pending completion from publishing or queuing more controls", async () => {
  const s = await session(),
    time = clock();
  let release!: () => void;
  const updated = vi.fn(),
    failure = vi.fn(),
    control = vi.fn(async (input) => {
      if (input.command.kind === "heartbeat") {
        await new Promise<void>((r) => {
          release = r;
        });
        return { ...s, generation: "2", overlay_epoch: "2" };
      }
      return s;
    });
  const pump = new PreviewConsumptionPump(
    { open, control },
    output(),
    () => true,
    updated,
    failure,
  );
  pump.start(s);
  time(600);
  pump.completed();
  await flush();
  pump.completed();
  pump.abort();
  release();
  await flush();
  expect(control).toHaveBeenCalledTimes(2);
  expect(updated).toHaveBeenCalledOnce();
  expect(failure).not.toHaveBeenCalled();
});
it("wall-clock rollback cannot suppress a due heartbeat", async () => {
  const s = await session(),
    time = clock();
  vi.setSystemTime(1000000);
  const control = vi.fn(async (_input: AudioPreviewControl) => s),
    pump = new PreviewConsumptionPump(
      { open, control },
      output(),
      () => true,
      vi.fn(),
      vi.fn(),
    );
  pump.start(s);
  time(600);
  vi.setSystemTime(0);
  pump.completed();
  await flush();
  expect(control.mock.calls.map(([i]) => i.command.kind)).toEqual([
    "report_consumed",
    "heartbeat",
  ]);
  pump.abort();
});

it("reanchors periodic reports to immediate block completions without delaying the latest cursor", async () => {
  const s = await session();
  vi.useFakeTimers();
  const control = vi.fn(async (input: AudioPreviewControl) => ({
    ...s,
    consumed_frames: input.command.kind === "report_consumed"
      ? input.command.consumed_frames : s.consumed_frames,
  }));
  const failure = vi.fn();
  const pump = new PreviewConsumptionPump({ open, control }, output(), () => true, vi.fn(), failure);
  pump.start(s);
  await vi.advanceTimersByTimeAsync(90);
  pump.observe({ consumedFrames: "4096", stalled: false, contextTime: 0 });
  pump.completed();
  await flush();
  expect(control).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(10);
  // The old timer phase must not duplicate the just-reported block.
  expect(control).toHaveBeenCalledTimes(1);
  pump.observe({ consumedFrames: "4608", stalled: false, contextTime: 0 });
  await vi.advanceTimersByTimeAsync(90);
  expect(control).toHaveBeenCalledTimes(2);
  expect(control.mock.calls[1][0].command).toMatchObject({ consumed_frames: "4608" });
  pump.observe({ consumedFrames: "8192", stalled: false, contextTime: 0 });
  pump.completed();
  await flush();
  expect(control).toHaveBeenCalledTimes(3);
  pump.freeze();
  await pump.reportStopped("8256");
  expect(control.mock.calls.at(-1)![0].command).toMatchObject({ consumed_frames: "8256", playing: false });
  await vi.advanceTimersByTimeAsync(200);
  expect(control).toHaveBeenCalledTimes(4);
  expect(failure).not.toHaveBeenCalled();
  pump.abort();
});

it.each([86, 93])("does not add partial reports between %i ms block completions", async (blockMs) => {
  const s = await session();
  vi.useFakeTimers();
  const failure = vi.fn();
  const control = vi.fn(async (input: AudioPreviewControl) => ({
    ...s,
    consumed_frames: input.command.kind === "report_consumed"
      ? input.command.consumed_frames : s.consumed_frames,
  }));
  const pump = new PreviewConsumptionPump({ open, control }, output(), () => true, vi.fn(), failure);
  pump.start(s);
  for (let block = 1; block <= 3; block++) {
    await vi.advanceTimersByTimeAsync(50);
    pump.observe({ consumedFrames: String((block - 1) * 4096 + 2048), stalled: false, contextTime: 0 });
    await vi.advanceTimersByTimeAsync(blockMs - 50);
    expect(control).toHaveBeenCalledTimes(block - 1);
    pump.observe({ consumedFrames: String(block * 4096), stalled: false, contextTime: 0 });
    pump.completed();
    await flush();
    expect(control).toHaveBeenCalledTimes(block);
    expect(control.mock.calls.at(-1)![0].command).toMatchObject({ consumed_frames: String(block * 4096) });
  }
  // A stalled or partial final block still gets a periodic receipt.
  pump.observe({ consumedFrames: "12352", stalled: true, contextTime: 0 });
  await vi.advanceTimersByTimeAsync(100);
  expect(control).toHaveBeenCalledTimes(4);
  expect(control.mock.calls.at(-1)![0].command).toMatchObject({ consumed_frames: "12352", stalled: true });
  expect(failure).not.toHaveBeenCalled();
  pump.abort();
});
