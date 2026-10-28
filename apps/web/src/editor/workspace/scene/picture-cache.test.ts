import {afterEach,expect,it,vi} from "vitest";
const calls=vi.hoisted(()=>({frame:vi.fn(),warm:vi.fn(),close:vi.fn(),retain:vi.fn()}));
vi.mock("./visual-preview-renderer",()=>({PreviewSegmentDecoder:class{frame=calls.frame;warm=calls.warm;close=calls.close;retain=calls.retain;}}));
import {PictureCache} from "./picture-cache";
import type {PictureCachePlan} from "./picture-cache";
import type {createEditorClient} from "../../../lib/editor-sdk";
afterEach(()=>{vi.clearAllMocks();vi.useRealTimers();});
const time=(n:number)=>({ticks:String(n),timebase:25});
const plan=():PictureCachePlan=>({project_id:"p",revision:"r",sequence_id:"s",cache_key:"a".repeat(64),width:960,height:540,
 duration:time(4),duration_seconds:.16,frame_step:time(1),frame_step_seconds:.04,max_encoded_bytes:64,
 segments:[0,2].map(start=>({start:time(start),end:time(start+2),start_seconds:start/25,end_seconds:(start+2)/25,job_id:`job_${start}`,
 frames:[start,start+1].map(n=>({time:time(n),next_time:time(n+1),time_seconds:n/25,segment_time_seconds:(n-start)/25}))}))});
const session={session_id:"s",generation:"1",overlay_epoch:"0",owner_id:"o",lease_id:"l"};
async function fixture(){
 const native=plan(),bytes=new Uint8Array([1,2,3]);
 const hash=[...new Uint8Array(await crypto.subtle.digest("SHA-256",bytes))].map(v=>v.toString(16).padStart(2,"0")).join("");
 const call=vi.fn(async(name:string,input:{job_id?:string})=>name==="opencut_visual_preview_cache_prepare"?native:
 {state:"succeeded",job_id:input.job_id,artifacts:[{artifact_id:"a",mime_type:"video/mp4",size_bytes:"3",content_hash:hash}]});
 const downloadArtifact=vi.fn(async()=>new Response(bytes));
 const sdk={call,downloadArtifact} as unknown as ReturnType<typeof createEditorClient>,cache=new PictureCache(),progress=vi.fn();
 calls.frame.mockImplementation(async()=>({width:960,height:540,close:vi.fn()}));calls.warm.mockResolvedValue(undefined);
 return {cache,sdk,call,downloadArtifact,native,progress};
}
it("prepares the requested segment first, without waiting for earlier queued jobs",async()=>{
 const f=await fixture(),original=f.call.getMockImplementation()!;
 f.call.mockImplementation(async(name,input)=>name==="opencut_job_view"&&input.job_id==="job_0"?{state:"queued",artifacts:[]} as never:original(name,input));
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress,time(3));
 expect(f.call.mock.calls.find(([name])=>name==="opencut_job_view")?.[1]).toEqual({job_id:"job_2"});
 expect(f.downloadArtifact).toHaveBeenCalledOnce();expect(f.progress).toHaveBeenLastCalledWith(1,1);
 const frame=await f.cache.frame(time(3),new AbortController().signal);expect(frame.frame.plan.identity.time).toEqual(time(3));frame.pixels.close();await f.cache.close();
});
it("uses exact native frame mappings and never recompiles the project",async()=>{
 const f=await fixture(),signal=new AbortController().signal;
 await f.cache.prepare(f.sdk,session,signal,f.progress);
 const frame=await f.cache.frame({ticks:"3",timebase:50},signal);
 expect(frame.frame.plan.identity.time).toEqual(time(1));expect(frame.frame.next_time).toEqual(time(2));expect(calls.frame.mock.calls[0][4]).toBe(.04);frame.pixels.close();
 const after=await f.cache.frame(time(2),signal);expect(calls.frame.mock.calls[1][4]).toBe(0);after.pixels.close();await f.cache.close();
});
it("refuses corrupt bytes before the requested segment is playable",async()=>{
 const f=await fixture();f.downloadArtifact.mockResolvedValue(new Response(new Uint8Array([4,5,6])));
 await expect(f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress)).rejects.toThrow("checksum");
 await expect(f.cache.frame(time(0),new AbortController().signal)).rejects.toThrow("not ready");await f.cache.close();
});
it("reauthorizes every generation and retains only unchanged native window receipts across revisions",async()=>{
 const f=await fixture(),signal=new AbortController().signal;
 await f.cache.prepare(f.sdk,session,signal,f.progress);await f.cache.frame(time(2),signal);await f.cache.suspend();
 await f.cache.prepare(f.sdk,{...session,generation:"2"},signal,f.progress,time(2));expect(f.downloadArtifact).toHaveBeenCalledTimes(2);
 const next=structuredClone(f.native),original=f.call.getMockImplementation()!;next.cache_key="c".repeat(64);next.revision="r2";next.segments[1].job_id="edited_job";
 f.call.mockImplementation(async(name,input)=>name==="opencut_visual_preview_cache_prepare"?next:original(name,input));
 await f.cache.prepare(f.sdk,{...session,generation:"3"},signal,f.progress);await f.cache.frame(time(2),signal);
 expect(f.downloadArtifact).toHaveBeenCalledTimes(3);await f.cache.close();
});
it("probes a cold still frame without waiting for the render queue",async()=>{
 const f=await fixture(),original=f.call.getMockImplementation()!;
 f.call.mockImplementation(async(name,input)=>name==="opencut_job_view"?{state:"queued",artifacts:[]} as never:original(name,input));
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress,time(2),false);
 expect(f.cache.hasFrame(time(2))).toBe(false);expect(f.downloadArtifact).not.toHaveBeenCalled();await f.cache.close();
});
it("keeps the essential window bounded and cancels a pending native wait",async()=>{
 const limited=await fixture();limited.native.max_encoded_bytes=4;
 await expect(limited.cache.prepare(limited.sdk,session,new AbortController().signal,limited.progress)).rejects.toMatchObject({code:"PREVIEW_CACHE_LIMIT"});await limited.cache.close();
 const f=await fixture(),abort=new AbortController();
 f.call.mockImplementation(async(name)=>name==="opencut_visual_preview_cache_prepare"?f.native:{state:"queued",artifacts:[]} as never);
 const pending=f.cache.prepare(f.sdk,session,abort.signal,f.progress);await vi.waitFor(()=>expect(f.call.mock.calls.length).toBeGreaterThanOrEqual(2));abort.abort();
 await expect(pending).rejects.toMatchObject({name:"AbortError"});await f.cache.close();
});
it("closes an already decoded bitmap when its generation is cancelled during warming",async()=>{
 const f=await fixture(),signal=new AbortController().signal;await f.cache.prepare(f.sdk,session,signal,f.progress);await f.cache.frame(time(2),signal);
 const bitmap={width:960,height:540,close:vi.fn()},abort=new AbortController();calls.frame.mockResolvedValue(bitmap);calls.warm.mockImplementation(async()=>{abort.abort();});
 await expect(f.cache.frame(time(0),abort.signal)).rejects.toMatchObject({name:"AbortError"});expect(bitmap.close).toHaveBeenCalledOnce();await f.cache.close();
});
it("reads only the nearby window and advances it with the playhead instead of downloading the whole scene",async()=>{
 const f=await fixture();f.native.segments=Array.from({length:8},(_,i)=>({...f.native.segments[0],start:time(i*2),end:time(i*2+2),job_id:`job_${i*2}`,frames:[i*2,i*2+1].map(n=>({time:time(n),next_time:time(n+1),time_seconds:n/25,segment_time_seconds:(n%2)/25}))}));
 f.native.duration=time(16);f.native.duration_seconds=16/25;
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress,time(0),false);
 await vi.waitFor(()=>expect(f.downloadArtifact).toHaveBeenCalledTimes(3));
 expect(f.call.mock.calls.filter(([name])=>name==="opencut_job_view").map(([,input])=>input.job_id)).toEqual(["job_0","job_2","job_4"]);
 const moved=await f.cache.frame(time(7),new AbortController().signal);moved.pixels.close();
 await vi.waitFor(()=>expect(f.downloadArtifact).toHaveBeenCalledTimes(6));
 expect(f.call.mock.calls.some(([,input])=>input.job_id==="job_12"||input.job_id==="job_14")).toBe(false);await f.cache.close();
});


it("starts playback from its current window while distant render jobs remain queued",async()=>{
 const f=await fixture(),original=f.call.getMockImplementation()!;
 f.native.segments=Array.from({length:8},(_,i)=>({...f.native.segments[0],start:time(i*2),end:time(i*2+2),job_id:`job_${i*2}`,frames:[i*2,i*2+1].map(n=>({time:time(n),next_time:time(n+1),time_seconds:n/25,segment_time_seconds:(n%2)/25}))}));
 f.native.duration=time(16);f.native.duration_seconds=16/25;
 f.call.mockImplementation(async(name,input)=>name==="opencut_job_view"&&!["job_6","job_8","job_10"].includes(input.job_id??"")?{state:"queued",artifacts:[]} as never:original(name,input));
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress,time(6),true);
 expect(f.cache.hasFrame(time(6))).toBe(true);
 expect(f.downloadArtifact).toHaveBeenCalledTimes(3);
 expect(f.progress).toHaveBeenLastCalledWith(3,3);
 expect(calls.warm.mock.calls.slice(0,2).map(call=>call[0])).toEqual(["6/25:8/25:job_6","8/25:10/25:job_8"]);
 expect(f.call.mock.calls.some(([,input])=>input.job_id==="job_14")).toBe(false);
 const picture=await f.cache.frame(time(6),new AbortController().signal);picture.pixels.close();
 await f.cache.close();
});

it("retires a frame consumer immediately while its shared next-window render remains queued",async()=>{
 const f=await fixture(),original=f.call.getMockImplementation()!;
 f.call.mockImplementation(async(name,input)=>name==="opencut_job_view"&&input.job_id==="job_2"?{state:"queued",artifacts:[]} as never:original(name,input));
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress,time(0),false);
 const abort=new AbortController(),frame=f.cache.frame(time(2),abort.signal);
 abort.abort(new DOMException("Paused","AbortError"));
 await expect(frame).rejects.toMatchObject({name:"AbortError"});
 expect(f.cache.hasFrame(time(0))).toBe(true);
 await f.cache.close();
});

it("extends a partial native manifest across two windows without replacing its decoder or mapping",async()=>{
 const f=await fixture(),full=plan();full.segments=Array.from({length:6},(_,i)=>({...full.segments[0],start:time(i*2),end:time(i*2+2),job_id:`job_${i*2}`,frames:[i*2,i*2+1].map(n=>({time:time(n),next_time:time(n+1),time_seconds:n/25,segment_time_seconds:(n%2)/25}))}));full.duration=time(12);full.duration_seconds=.48;
 const first={...full,segments:full.segments.slice(0,3)},second={...full,segments:full.segments.slice(2,5)};
 const original=f.call.getMockImplementation()!;let prepares=0;
 f.call.mockImplementation(async(name,input)=>name==="opencut_visual_preview_cache_prepare"?(++prepares===1?structuredClone(first):structuredClone(second)):original(name,input));
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress,time(0));
 const closes=calls.close.mock.calls.length;
 for(const n of [3,6,8]){const picture=await f.cache.frame(time(n),new AbortController().signal);expect(picture.frame.plan.identity.time).toEqual(time(n));picture.pixels.close();}
 expect(f.cache.descriptor().duration).toEqual(time(12));expect(calls.close).toHaveBeenCalledTimes(closes);
 expect(calls.frame.mock.calls.map(call=>call[0])).toEqual(["2/25:4/25:job_2","6/25:8/25:job_6","8/25:10/25:job_8"]);
 await f.cache.close();
});
it("rejects an old prepare arriving after a newer generation has become ready",async()=>{
 const f=await fixture(),original=f.call.getMockImplementation()!;let resolve!:(value:PictureCachePlan)=>void,first=true;
 f.call.mockImplementation(async(name,input)=>name==="opencut_visual_preview_cache_prepare"&&first?(first=false,new Promise<PictureCachePlan>(done=>{resolve=done;})):original(name,input));
 const old=f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress);await vi.waitFor(()=>expect(resolve).toBeTypeOf("function"));
 await f.cache.prepare(f.sdk,{...session,generation:"2"},new AbortController().signal,f.progress);
 resolve({...plan(),cache_key:"f".repeat(64)});await expect(old).rejects.toMatchObject({name:"AbortError"});
 expect(f.cache.descriptor().cache_key).toBe("a".repeat(64));await f.cache.close();
});
it("accepts a final CFR frame extending past a non-frame-aligned timeline but refuses EOF",async()=>{
 const f=await fixture();f.native.duration={ticks:"101",timebase:1000};f.native.duration_seconds=.101;
 f.native.segments=[{...f.native.segments[0],end:time(3),frames:[0,1,2].map(n=>({time:time(n),next_time:time(n+1),time_seconds:n/25,segment_time_seconds:n/25}))}];
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress);
 const picture=await f.cache.frame({ticks:"100",timebase:1000},new AbortController().signal);expect(picture.frame.plan.identity.time).toEqual(time(2));picture.pixels.close();
 await expect(f.cache.frame(f.native.duration,new AbortController().signal)).rejects.toThrow("outside its timeline");await f.cache.close();
});
it("bounds stale-consumer refreshes rather than looping or treating a missing window as EOF",async()=>{
 const f=await fixture();f.native.duration=time(8);f.native.duration_seconds=.32;
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress);
 const count=f.call.mock.calls.filter(([name])=>name==="opencut_visual_preview_cache_prepare").length;
 await expect(f.cache.frame(time(6),new AbortController().signal)).rejects.toMatchObject({code:"PREVIEW_CACHE_POSITION"});
 expect(f.call.mock.calls.filter(([name])=>name==="opencut_visual_preview_cache_prepare")).toHaveLength(count+2);await f.cache.close();
});

it("never decodes an old window after its job is replaced while decoder retention is pending",async()=>{
 const f=await fixture(),original=f.call.getMockImplementation()!,next=plan();f.native.duration=time(8);f.native.duration_seconds=.32;
 next.duration=time(8);next.duration_seconds=.32;next.segments=[{...next.segments[0],job_id:"replacement"},{...next.segments[1],start:time(6),end:time(8),job_id:"job_6",frames:[6,7].map(n=>({time:time(n),next_time:time(n+1),time_seconds:n/25,segment_time_seconds:(n-6)/25}))}];
 let prepares=0,refresh!:(plan:PictureCachePlan)=>void,release!:()=>void;
 f.call.mockImplementation(async(name,input)=>name==="opencut_visual_preview_cache_prepare"?(++prepares===1?structuredClone(f.native):new Promise<PictureCachePlan>(done=>{refresh=done;})):original(name,input));
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress);
 calls.retain.mockImplementationOnce(()=>new Promise<void>(done=>{release=done;}));
 const old=f.cache.frame(time(0),new AbortController().signal);await vi.waitFor(()=>expect(release).toBeTypeOf("function"));
 const moved=f.cache.frame(time(6),new AbortController().signal);refresh(next);const current=await moved;current.pixels.close();release();
 await expect(old).rejects.toMatchObject({name:"AbortError"});expect(calls.frame.mock.calls.every(([key])=>key!=="0/25:2/25:job_0")).toBe(true);await f.cache.close();
});
it("closes a decoded old bitmap if its window is replaced while warming its neighbour",async()=>{
 const f=await fixture(),original=f.call.getMockImplementation()!;f.native.duration=time(8);f.native.duration_seconds=.32;
 const next=structuredClone(f.native);next.segments[0].job_id="replacement";
 next.segments.push({...next.segments[1],start:time(6),end:time(8),job_id:"job_6",frames:[6,7].map(n=>({time:time(n),next_time:time(n+1),time_seconds:n/25,segment_time_seconds:(n-6)/25}))});
 let prepares=0,refresh!:(plan:PictureCachePlan)=>void,release!:()=>void;
 f.call.mockImplementation(async(name,input)=>name==="opencut_visual_preview_cache_prepare"?(++prepares===1?structuredClone(f.native):new Promise<PictureCachePlan>(done=>{refresh=done;})):original(name,input));
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress);await vi.waitFor(()=>expect(f.cache.hasFrame(time(2))).toBe(true));
 const bitmap={width:960,height:540,close:vi.fn()};calls.frame.mockResolvedValueOnce(bitmap);calls.warm.mockImplementationOnce(()=>new Promise<void>(done=>{release=done;}));
 const old=f.cache.frame(time(0),new AbortController().signal);await vi.waitFor(()=>expect(release).toBeTypeOf("function"));
 const moved=f.cache.frame(time(6),new AbortController().signal);refresh(next);const current=await moved;current.pixels.close();release();
 await expect(old).rejects.toMatchObject({name:"AbortError"});expect(bitmap.close).toHaveBeenCalledOnce();await f.cache.close();
});

it("retires cached playback when rolling reserve authorization fails",async()=>{
 const f=await fixture(),original=f.call.getMockImplementation()!;f.native.duration=time(8);f.native.duration_seconds=.32;
 let prepares=0;
 const denied=Object.assign(Error("Preview grant revoked"),{code:"NOT_AUTHORIZED"});
 f.call.mockImplementation(async(name,input)=>name==="opencut_visual_preview_cache_prepare"?(++prepares===1?structuredClone(f.native):Promise.reject(denied)):original(name,input));
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress);
 await expect(f.cache.frame(time(0),new AbortController().signal)).rejects.toMatchObject({code:"NOT_AUTHORIZED"});
 await expect(f.cache.frame(time(0),new AbortController().signal)).rejects.toMatchObject({code:"NOT_AUTHORIZED"});
 expect(prepares).toBe(2);await f.cache.close();
});

it("does not extend a native manifest for an already cancelled frame consumer",async()=>{
 const f=await fixture();f.native.duration=time(8);f.native.duration_seconds=.32;
 await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress);
 const before=f.call.mock.calls.filter(([name])=>name==="opencut_visual_preview_cache_prepare").length,abort=new AbortController();abort.abort();
 await expect(f.cache.frame(time(6),abort.signal)).rejects.toMatchObject({name:"AbortError"});
 expect(f.call.mock.calls.filter(([name])=>name==="opencut_visual_preview_cache_prepare")).toHaveLength(before);await f.cache.close();
});
it("reauthorizes an unchanged replay without discarding warm decoders or downloading the same blobs",async()=>{
 const f=await fixture(),signal=new AbortController().signal;
 await f.cache.prepare(f.sdk,session,signal,f.progress);
 const downloads=f.downloadArtifact.mock.calls.length,closes=calls.close.mock.calls.length;
 await f.cache.park();
 await f.cache.prepare(f.sdk,{...session,generation:session.generation+1},signal,f.progress);
 expect(calls.close).toHaveBeenCalledTimes(closes);
 expect(f.downloadArtifact).toHaveBeenCalledTimes(downloads);
 expect(f.cache.hasFrame(time(0))).toBe(true);
 await f.cache.close();
});
it("releases parked decoders after the bounded idle grace period",async()=>{
 const f=await fixture();await f.cache.prepare(f.sdk,session,new AbortController().signal,f.progress);
 vi.useFakeTimers();const closes=calls.close.mock.calls.length;
 await f.cache.park();await vi.advanceTimersByTimeAsync(29999);expect(calls.close).toHaveBeenCalledTimes(closes);
 await vi.advanceTimersByTimeAsync(1);expect(calls.close).toHaveBeenCalledTimes(closes+1);await f.cache.close();
});
