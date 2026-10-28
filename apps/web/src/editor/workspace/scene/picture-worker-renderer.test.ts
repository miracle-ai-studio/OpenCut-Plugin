import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { OpenCutApiError } from "../../../lib/editor-sdk";
import type { EditorApi } from "../api";
import { PictureWorkerRenderer } from "./picture-worker-renderer";
import type { PictureCommand, PictureEvent, PictureResult } from "./picture-worker-protocol";
type EventBody=PictureEvent extends infer E ? E extends PictureEvent ? Omit<E,"token"> : never : never;

class WorkerPort {
  static current: WorkerPort;
  messages: PictureCommand[] = [];
  transfers: Transferable[][] = [];
  onmessage: ((event: { data: PictureEvent }) => void) | null = null;
  onerror: (() => void) | null = null;
  terminate = vi.fn();
  constructor() { WorkerPort.current = this; }
  postMessage(message: PictureCommand, transfers: Transferable[] = []) { this.messages.push(message); this.transfers.push(transfers); }
  emit(event: EventBody) {
    this.onmessage?.({ data: { token: this.messages[0].token, ...event } });
  }
}
const api = { audioPreviewConnection: { baseUrl: "http://127.0.0.1:8788", token: "" } } as EditorApi;
const session = { session_id: "s", generation: "2", overlay_epoch: "1", owner_id: "o", lease_id: "l" };
const result = () => ({ pixels: { close: vi.fn() }, frame: {}, runtime: {} }) as unknown as PictureResult;
beforeEach(() => { vi.stubGlobal("Worker", WorkerPort); vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

it("uses one bounded worker request and captures the exact authenticated connection and session", async () => {
  const renderer = new PictureWorkerRenderer();
  const first = renderer.render(api, session, new AbortController().signal);
  const worker = WorkerPort.current;
  expect(worker.messages[0]).toMatchObject({ kind: "init", connection: api.audioPreviewConnection });
  expect(worker.messages[1]).toMatchObject({ kind: "render", id: 1, session });
  await expect(renderer.render(api, session, new AbortController().signal)).rejects.toThrow("busy");
  const picture = result(); worker.emit({ kind: "picture", id: 1, result: picture });
  expect(await first).toBe(picture); expect(picture.pixels.close).not.toHaveBeenCalled();
  await expect(renderer.render({ ...api } as EditorApi, session, new AbortController().signal)).rejects.toThrow("connection");
  renderer.close();
});

it("keeps cache preparation serial, reports progress without finishing, and rejects late cached pixels after cancellation",async()=>{
 const renderer=new PictureWorkerRenderer(),progress=vi.fn(),abort=new AbortController();
 const pending=renderer.prepareCache(api,session,abort.signal,progress).catch(error=>error);
 const worker=WorkerPort.current;
 expect(worker.messages[1]).toMatchObject({kind:"prepareCache",session});
 worker.emit({kind:"cacheProgress",id:1,completed:2,total:21});expect(progress).toHaveBeenCalledWith(2,21);
 await expect(renderer.cachedFrame(api,{ticks:"0",timebase:25},new AbortController().signal)).rejects.toThrow("busy");
 abort.abort();expect(worker.messages.at(-1)).toMatchObject({kind:"cancel",id:1});renderer.close();
 expect((await pending).name).toBe("AbortError");
 const late={pixels:{close:vi.fn()},frame:{},runtime:{}} as unknown as import("./picture-cache").CachedPicture;
 worker.emit({kind:"cachedPicture",id:1,result:late});expect(late.pixels.close).toHaveBeenCalledOnce();
});

it("forwards frame cancellation and releases any late transferred bitmap after closing", async () => {
  const renderer = new PictureWorkerRenderer(), abort = new AbortController();
  const pending = renderer.render(api, session, abort.signal).catch(error => error);
  const worker = WorkerPort.current;
  abort.abort(); expect(worker.messages.at(-1)).toMatchObject({ kind: "cancel", id: 1, intent: false });
  renderer.close(); expect((await pending).name).toBe("AbortError");
  const late = result(); worker.emit({ kind: "picture", id: 1, result: late });
  expect(late.pixels.close).toHaveBeenCalledOnce();
  await vi.advanceTimersByTimeAsync(1000); expect(worker.terminate).toHaveBeenCalledOnce();
});

it("preserves native rejection codes and diagnostics instead of turning authorization loss into a late picture", async () => {
  const renderer = new PictureWorkerRenderer();
  const pending = renderer.render(api, session, new AbortController().signal).catch(error => error);
  WorkerPort.current.emit({ kind: "error", id: 1, error: { name: "OpenCutApiError", message: "Owner changed", code: "NOT_AUTHORIZED",
    api: { details: null, status: 403, requestId: "request", warnings: [] }, metrics: { totalMs: 12 } } });
  const error = await pending;
  expect(error).toBeInstanceOf(OpenCutApiError); expect(error.code).toBe("NOT_AUTHORIZED");
  expect(error.previewMetrics).toEqual({ totalMs: 12 }); renderer.close();
});

it("transfers the surface once, fences live reports, and accepts only final statistics after close", async () => {
  const renderer=new PictureWorkerRenderer(),updated=vi.fn(),failed=vi.fn();
  const canvas={} as OffscreenCanvas,bind=vi.fn((_port:MessagePort)=>({token:"clock",sampleRate:48000,graphLatencySeconds:.006}));
  const report={time:0,readyThrough:{ticks:"8",timebase:25},frameTime:{ticks:"0",timebase:25},generation:"2",runtime:"{}",presentedFrames:1,discardedFrames:0,statistics:"{}"};
  const pending=renderer.startSurface(api,session as import("./types").AudioPreviewSession,canvas,{bind},new AbortController().signal,updated,failed);
  const worker=WorkerPort.current,command=worker.messages[1];
  expect(command).toMatchObject({kind:"startSurface",id:1,canvas,binding:{token:"clock"}});
  expect(worker.transfers[1]).toEqual([canvas,expect.anything()]);expect(bind).toHaveBeenCalledOnce();
  worker.emit({kind:"surfaceReport",runId:0,result:report,retired:false});expect(updated).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(11000);
  expect(worker.messages.some(message=>message.kind==="close")).toBe(false);
  worker.emit({kind:"surfacePrepared",id:1,result:report});expect(await pending).toEqual(report);
  worker.emit({kind:"surfaceReport",runId:1,result:report,retired:false});expect(updated).toHaveBeenCalledTimes(1);
  worker.onerror?.();expect(failed).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({message:"Picture preparation worker failed."}));
  renderer.close();
  worker.emit({kind:"surfaceReport",runId:1,result:report,retired:false});expect(updated).toHaveBeenCalledTimes(1);
  worker.emit({kind:"surfaceReport",runId:1,result:report,retired:true});expect(updated).toHaveBeenLastCalledWith(report,true);
  worker.emit({kind:"surfaceFailed",runId:1,error:{name:"Error",message:"late failure"}});expect(failed).toHaveBeenCalledTimes(1);
  bind.mock.calls[0][0].close();
});

it("projects a paused owner into the strict stream contract and retains the worker between still and playback preparation",async()=>{
 const renderer=new PictureWorkerRenderer(),owner={...session,presentation_time:{ticks:"5",timebase:2},actual_start_sample:"120000",state:"paused"} as import("./types").AudioPreviewSession;
 const pending=renderer.stillFrame(api,owner,new AbortController().signal),worker=WorkerPort.current;
 expect(worker.messages[1]).toEqual({token:worker.messages[0].token,kind:"stillFrame",id:1,session,time:owner.presentation_time});
 const frame=result();worker.emit({kind:"stillPicture",id:1,result:frame});expect(await pending).toBe(frame);
 const suspended=renderer.suspend();expect(worker.messages.at(-1)).toMatchObject({kind:"suspend",id:2});worker.emit({kind:"suspended",id:2});await suspended;
 const next=renderer.cachedFrame(api,owner.presentation_time,new AbortController().signal);expect(WorkerPort.current).toBe(worker);worker.emit({kind:"cachedPicture",id:3,result:frame as never});await next;
 renderer.close();
});
