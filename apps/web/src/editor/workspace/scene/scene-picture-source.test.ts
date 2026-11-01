import {expect,it,vi} from "vitest";
import {ScenePictureSource,type ScenePictureExecutionApi} from "./scene-picture-source";
import type {AudioPreviewSession} from "./types";
import type {VisualPreviewRenderer} from "./visual-preview-renderer";
const session={session_id:"session",generation:"1",overlay_epoch:"0",owner_id:"owner",lease_id:"lease",identity:{project_id:"p",revision:"r",sequence_id:"s",sample_rate:48000},presentation_time:{ticks:"0",timebase:1},total_frames:"2016000"} as AudioPreviewSession;
function fixture(){
 const closed=vi.fn();let revision="r",width=960;
 const render=vi.fn(async(...args:Parameters<VisualPreviewRenderer["render"]>)=>{
  const time=args[5]!.time!,n=Math.floor(Number(time.ticks)/time.timebase*25);
  return {frame:{prepare_ahead:true,plan:{identity:{projectId:"p",revision,sequenceId:"s",time:{ticks:String(n),timebase:25}}},next_time:{ticks:String(n+1),timebase:25},time_seconds:n/25,picture:{width,height:540}},
   pixels:{width,height:540,close:closed},runtime:{gpuBackend:"webgpu"}} as unknown as Awaited<ReturnType<VisualPreviewRenderer["render"]>>;
 });
 const prepare=vi.fn(async(...args:Parameters<VisualPreviewRenderer["prepare"]>)=>{const result=await render(...args);return {frame:result.frame,claimed:Promise.resolve(),complete:Promise.resolve(result)};});
 const prepareSources=vi.fn(async()=>({jobs:[] as Array<{job_id:string;state:string}>})),waitForJob=vi.fn(async(_id:string,_options?:{signal?:AbortSignal})=>({state:"succeeded"}));
 const api={sdk:{call:prepareSources,waitForJob}} as unknown as ScenePictureExecutionApi;
 const renderer={prepare};const source=new ScenePictureSource(renderer,api,session);
 return {source,renderer,api,prepareSources,waitForJob,prepare,render,closed,changeRevision:()=>revision="other",changeWidth:()=>width=640};
}
it("waits for every Rust-selected scene source before starting native frames, with one bounded observer",async()=>{
 const f=fixture(),signal=new AbortController().signal;
 f.prepareSources.mockResolvedValue({jobs:[{job_id:"ready",state:"succeeded"},{job_id:"first",state:"running"},{job_id:"later",state:"queued"}]});
 let finish!:()=>void,finishLater!:()=>void;
 f.waitForJob.mockImplementationOnce(()=>new Promise(resolve=>{finish=()=>resolve({state:"succeeded"});}));
 f.waitForJob.mockImplementationOnce(()=>new Promise(resolve=>{finishLater=()=>resolve({state:"succeeded"});}));
 const pending=f.source.initialize(signal);
 await vi.waitFor(()=>expect(f.waitForJob).toHaveBeenCalledOnce());
 expect(f.prepareSources).toHaveBeenCalledWith("opencut_source_preview_prepare",{project_id:"p",revision:"r",sequence_id:"s",asset_id:null,selected_video_stream:null},{signal});
 expect(f.render).not.toHaveBeenCalled();
 finish();await vi.waitFor(()=>expect(f.waitForJob).toHaveBeenCalledTimes(2));expect(f.render).not.toHaveBeenCalled();finishLater();await pending;
 expect(f.waitForJob.mock.calls.map(([id])=>id)).toEqual(["first","later"]);
 expect(f.waitForJob).toHaveBeenCalledWith("first",{signal,timeoutMs:1800000,pollIntervalMs:500});
 expect(f.render).toHaveBeenCalledOnce();
});
it("stops an interrupted source wait before rendering without cancelling the shared source job",async()=>{
 const f=fixture(),cancel=new AbortController();
 f.prepareSources.mockResolvedValue({jobs:[{job_id:"shared",state:"running"}]});
 f.waitForJob.mockImplementation((_id,options)=>new Promise((_resolve,reject)=>options!.signal!.addEventListener("abort",()=>reject(options!.signal!.reason),{once:true})));
 const pending=f.source.initialize(cancel.signal);const rejected=expect(pending).rejects.toMatchObject({name:"AbortError"});
 await vi.waitFor(()=>expect(f.waitForJob).toHaveBeenCalledOnce());cancel.abort();await rejected;
 expect(f.render).not.toHaveBeenCalled();expect(f.prepareSources).toHaveBeenCalledOnce();
});
it("preserves source-job failures and rejects a late completion after the playback intent was retired",async()=>{
 for(const cancelled of [false,true]){
  const f=fixture(),cancel=new AbortController(),failure=Error("Source job failed");
  f.prepareSources.mockResolvedValue({jobs:[{job_id:"source",state:"running"}]});
  f.waitForJob.mockImplementationOnce(async()=>{if(cancelled){cancel.abort();return {state:"succeeded"};}throw failure;});
  const pending=f.source.initialize(cancel.signal);
  if(cancelled)await expect(pending).rejects.toMatchObject({name:"AbortError"});else await expect(pending).rejects.toBe(failure);
  expect(f.render).not.toHaveBeenCalled();
 }
});
it("starts compact sources without a durable wait and reuses completed source preparations",async()=>{
 for(const jobs of [[],[{job_id:"existing",state:"succeeded"}]]){
  const f=fixture();f.prepareSources.mockResolvedValue({jobs});await f.source.initialize(new AbortController().signal);
  expect(f.waitForJob).not.toHaveBeenCalled();expect(f.render).toHaveBeenCalledOnce();
 }
});
it("requests a future authored boundary without waiting for speculative decode or queued cache publication",async()=>{
 const f=fixture(),signal=new AbortController().signal,follow=vi.fn();
 const cache={rebind:async()=>true,follow,hasFrame:()=>false,state:()=>"preparing"} as unknown as import("./picture-cache").PictureCache;
 const source=new ScenePictureSource(f.renderer,f.api,session,cache,[{ticks:"6",timebase:1}]);
 await source.initialize(signal);await source.frame(session.presentation_time,signal);
 const picture=await source.frame({ticks:"4",timebase:1},signal,0);
 expect(f.render.mock.calls[1][5]?.time).toEqual({ticks:"4",timebase:1});expect(picture.runtime.cacheState).toBe("preparing");expect(follow).toHaveBeenCalledOnce();
});
it("uses a completed authorized derived window and rejects a stale revision instead of presenting old pixels",async()=>{
 const f=fixture(),signal=new AbortController().signal;
 const frame=vi.fn(async()=>({pixels:{width:960,height:540,close:vi.fn()},frame:{plan:{identity:{time:{ticks:"1",timebase:25}}},next_time:{ticks:"2",timebase:25}},runtime:{cacheSegment:"verified"}}));
 const plan={project_id:"p",revision:"r",sequence_id:"s"};
 const cache={rebind:async()=>true,follow:vi.fn(),hasFrame:()=>true,descriptor:()=>plan,nextWindow:()=>null,frame,state:()=>"ready"} as unknown as import("./picture-cache").PictureCache;
 const source=new ScenePictureSource(f.renderer,f.api,session,cache);
 await source.initialize(signal);await source.frame(session.presentation_time,signal);
 const picture=await source.frame({ticks:"1",timebase:25},signal);expect(picture.runtime.cacheHits).toBe("1");expect(f.render).toHaveBeenCalledOnce();
 plan.revision="other";await expect(source.frame({ticks:"2",timebase:25},signal)).rejects.toThrow("fixed session");expect(frame).toHaveBeenCalledOnce();
});
it("waits for a newly authorized manifest before making the first picture available and rebinds replays",async()=>{
 const f=fixture(),signal=new AbortController().signal;
 let finish!:()=>void;const gate=new Promise<void>(resolve=>{finish=resolve;});
 const prepare=vi.fn(()=>gate),rebind=vi.fn(async()=>false);
 const cache={rebind,suspend:vi.fn(),state:()=>"preparing"} as unknown as import("./picture-cache").PictureCache;
 const source=new ScenePictureSource(f.renderer,f.api,session,cache,[],prepare);
 let ready=false;const pending=source.initialize(signal).then(()=>{ready=true;});
 await vi.waitFor(()=>expect(prepare).toHaveBeenCalledOnce());expect(ready).toBe(false);
 finish();await pending;expect(ready).toBe(true);expect(cache.suspend).not.toHaveBeenCalled();
 rebind.mockResolvedValue(true);await source.initialize(signal);expect(prepare).toHaveBeenCalledOnce();
});
it("uses native frame boundaries and session duration, reuses the first bitmap, and forwards retained-pixel admission",async()=>{
 const f=fixture(),signal=new AbortController().signal;await f.source.initialize(signal);
 expect(f.source.descriptor()).toEqual({project_id:"p",revision:"r",sequence_id:"s",width:960,height:540,duration:{ticks:"2016000",timebase:48000},frame_step_seconds:.04});
 expect(f.source.preparationFrames).toBe(28);
 expect(f.source.hasFrame({ticks:"42",timebase:1})).toBe(false);
 await f.source.frame({ticks:"0",timebase:1},signal);expect(f.render).toHaveBeenCalledTimes(1);expect(f.render.mock.calls[0][1]).toEqual({session_id:"session",generation:"1",overlay_epoch:"0",owner_id:"owner",lease_id:"lease"});
 const next=await f.source.frame({ticks:"1",timebase:25},signal,123456);
 expect(next.frame.next_time).toEqual({ticks:"2",timebase:25});expect(f.render.mock.calls[1][5]).toEqual({time:{ticks:"1",timebase:25},reservedBytes:123456,end:{ticks:"2016000",timebase:48000},captureSurface:true});
});
it("rejects a changed revision or raster and closes late pixels after cancellation",async()=>{
 for(const change of ["changeRevision","changeWidth"] as const){const f=fixture(),signal=new AbortController().signal;await f.source.initialize(signal);await f.source.frame(session.presentation_time,signal);f[change]();await expect(f.source.frame({ticks:"1",timebase:25},signal)).rejects.toThrow("fixed session");expect(f.closed).toHaveBeenCalledOnce();}
 const f=fixture(),cancel=new AbortController();f.render.mockImplementationOnce(async()=>{cancel.abort();return {pixels:{close:f.closed}} as unknown as Awaited<ReturnType<VisualPreviewRenderer["render"]>>;});
 await expect(f.source.initialize(cancel.signal)).rejects.toBeDefined();expect(f.closed).toHaveBeenCalledOnce();
});
it("keeps prepared source facts separate from pixels and rejects late completion after retirement",async()=>{
 const f=fixture(),abort=new AbortController();await f.source.initialize(abort.signal);await f.source.frame(session.presentation_time,abort.signal);
 let claim!:()=>void,finish!:()=>void;
 const claimed=new Promise<void>(yes=>{claim=yes;}),body=new Promise<void>(yes=>{finish=yes;});
 f.prepare.mockImplementationOnce(async(...args)=>{
  const picture=await f.render(...args);return {frame:picture.frame,claimed,complete:body.then(()=>picture)};
 });
 const pending=await f.source.prepareFrame({ticks:"1",timebase:25},abort.signal,123456);
 expect(pending.frame.next_time).toEqual({ticks:"2",timebase:25});
 claim();await pending.claimed;abort.abort();finish();
 await expect(pending.complete).rejects.toBeDefined();expect(f.closed).toHaveBeenCalledOnce();
});

it('preserves the native consumer catch-up binding while rejecting arbitrary future frames',async()=>{
 for(const valid of [true,false]){
  const f=fixture(),signal=new AbortController().signal;
  await f.source.initialize(signal);await f.source.frame(session.presentation_time,signal);
  const original=f.render.getMockImplementation()!;
  f.render.mockImplementationOnce(async(...args)=>{
   const p=await original(...args);p.frame.plan.identity.time={ticks:'3',timebase:25};p.frame.next_time={ticks:'4',timebase:25};p.frame.time_seconds=.12;p.frame.consumer_time_seconds=valid?.13:.10;p.frame.max_lag_ms=150;return p;
  });
  const pending=f.source.frame({ticks:'1',timebase:25},signal);
  if(valid){const p=await pending;expect(p.frame.catchUp).toEqual({requestedTime:{ticks:'1',timebase:25},consumerSeconds:.13});p.pixels.close();}
  else await expect(pending).rejects.toThrow('fixed session');
  expect(f.closed).toHaveBeenCalledOnce();
 }
});

it("keeps cold-seek reserve inside the native byte budget including four in-flight slots",async()=>{
 for(const capacity of [32*1024*1024,64*1024*1024,128*1024*1024]){
  const f=fixture(),original=f.render.getMockImplementation()!;
  f.render.mockImplementationOnce(async(...args)=>{const picture=await original(...args);picture.runtime.maxRetainedBytes=String(capacity);return picture;});
  await f.source.initialize(new AbortController().signal);
  const count=f.source.preparationFrames;
  expect((count+4)*960*540*4).toBeLessThanOrEqual(capacity);
  expect(count*.04).toBeLessThanOrEqual(2);
  expect(count).toBe(Math.min(50,Math.floor(capacity/(960*540*4))-4));
  if(capacity===128*1024*1024)expect(count).toBe(50);
 }
});
