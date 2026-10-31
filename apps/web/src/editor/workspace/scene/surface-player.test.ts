import {expect,it,vi} from "vitest";
import {SurfacePlayer} from "./surface-player";
import type {SurfaceReport,SurfaceScheduler} from "./surface-player";
import type {AudioPreviewSession} from "./types";
import type {PictureCache,PictureCachePlan,CachedPicture} from "./picture-cache";
import type {DeviceReceipt} from "./audio-preview/device-port";
import type {PresentationClockBinding} from "./audio-preview/presentation-clock";

const flush=async()=>{for(let i=0;i<30;i++)await Promise.resolve();};
function fixture(preparationFrames=8) {
  const session={session_id:"session",generation:"1",overlay_epoch:"0",owner_id:"owner",lease_id:"lease",
    identity:{project_id:"p",revision:"r",sequence_id:"s",profile:"classic-realtime-v31",sample_rate:48000,channels:2},
    state:"playing",actual_start_sample:"0",total_frames:"192000",presentation_time:{ticks:"0",timebase:48000},
    submitted_time:{ticks:"0",timebase:48000},output:null,consumed_frames:"0",produced_through_sample:"32768",buffered_frames:"32768",
    presentation_fresh:true,stalled:false,lease_remaining_ms:"30000",expires_in_ms:"120000",temporary_gain:null,failure_code:null,next_event_cursor:"0"} as AudioPreviewSession;
  const plan={project_id:"p",revision:"r",sequence_id:"s",width:960,height:540,duration:{ticks:"100",timebase:25},
    frame_step_seconds:.04} as PictureCachePlan;
  const bitmaps:{at:number;close:ReturnType<typeof vi.fn>}[]=[];
  const picture=(time:{ticks:string;timebase:number}):CachedPicture=>{
    const n=Math.floor(Number(time.ticks)/time.timebase*25+1e-7),bitmap={at:n/25,width:960,height:540,close:vi.fn()};bitmaps.push(bitmap);
    return {pixels:bitmap as unknown as ImageBitmap,runtime:{cacheKey:"native-fixed-cache"},
      frame:{prepare_ahead:true,time_seconds:n/25,plan:{identity:{time:{ticks:String(n),timebase:25}}},next_time:{ticks:String(n+1),timebase:25}}};
  };
  const frame=vi.fn(async(time:{ticks:string;timebase:number})=>picture(time));
  const hasFrame=vi.fn(()=>true);
  const cache={descriptor:()=>plan,frame,hasFrame,preparationFrames} as unknown as PictureCache;
  const drawImage=vi.fn(),clearRect=vi.fn(),canvas={width:960,height:540,getContext:()=>({drawImage,clearRect})} as unknown as OffscreenCanvas;
  const port={onmessage:null as ((event:{data:DeviceReceipt})=>void)|null,start:vi.fn(),close:vi.fn(),postMessage:vi.fn()};
  let now=1000,id=0;const callbacks=new Map<number,FrameRequestCallback>();
  const screen:SurfaceScheduler={request:callback=>{callbacks.set(++id,callback);return id;},cancel:key=>{callbacks.delete(key);},now:()=>now,timeOrigin:100000};
  const reports:SurfaceReport[]=[],failed=vi.fn();
  const binding:PresentationClockBinding={token:"device",sampleRate:48000,graphLatencySeconds:0};
  const player=new SurfacePlayer(cache,canvas,session,binding,port as unknown as MessagePort,report=>reports.push(report),failed,screen);
  const receipt=(through:number,token="device",start=48000,first=0)=>port.onmessage?.({data:{kind:"progress",token,
    consumedFrames:String(through),copied:128,completedBlocks:0,runStartFrame:start,runStartConsumed:String(first),
    renderFrame:start+through-first-128,quantumFrames:128,phase:"playing"}});
  const tick=async(delta=20)=>{now+=delta;const batch=[...callbacks.values()];callbacks.clear();for(const callback of batch)callback(now);await flush();};
  const timestamp=()=>player.timestamp({contextTime:1,performanceTime:1000,timeOrigin:100000});
  return {player,binding,session,plan,frame,hasFrame,picture,bitmaps,drawImage,clearRect,port,callbacks,reports,failed,receipt,tick,timestamp};
}
it("presents exact native frames while the UI sends no new timestamp, bounded by actual copied audio",async()=>{
  const f=fixture();await f.player.start(new AbortController().signal);
  f.receipt(24000);f.timestamp();
  for(let n=0;n<30;n++)await f.tick();
  const result=f.player.snapshot();
  expect(result.time).toBe(.48);expect(result.presentedFrames).toBe(13);
  expect(JSON.parse(result.statistics)).toMatchObject({missedCachedFrames:0,presentationThread:"worker"});
  expect(f.drawImage.mock.calls.map(([bitmap])=>(bitmap as {at:number}).at)).toEqual(Array.from({length:13},(_,n)=>n/25));
  expect(f.failed).not.toHaveBeenCalled();await f.player.stop(true);
  expect(f.port.close).toHaveBeenCalledOnce();expect(f.callbacks.size).toBe(0);
  expect(f.bitmaps.every(bitmap=>bitmap.close.mock.calls.length===1)).toBe(true);
});
it("does not advance from a stale device timestamp or another clock token",async()=>{
  const f=fixture();await f.player.start(new AbortController().signal);
  f.receipt(96000,"other-owner");f.timestamp();await f.tick();expect(f.player.snapshot().time).toBe(0);
  f.receipt(96000);await f.tick(1200);expect(f.player.snapshot().time).toBe(0);
  await f.player.stop();expect(f.failed).not.toHaveBeenCalled();
});
it("holds content through an audio gap and resumes from the next real interval",async()=>{
  const f=fixture();await f.player.start(new AbortController().signal);
  f.receipt(480);f.timestamp();await f.tick(80);expect(f.player.snapshot().time).toBe(0);
  f.receipt(5280,"device",52800,480);await f.tick(40);expect(f.player.snapshot().time).toBe(0);
  await f.tick();expect(f.player.snapshot().time).toBe(.04);
  await f.player.stop();expect(f.failed).not.toHaveBeenCalled();
});
it("cancels preparation and closes a late bitmap without starting its display loop",async()=>{
  const f=fixture(),abort=new AbortController();let resolve!:(picture:CachedPicture)=>void;
  f.frame.mockImplementationOnce(async time=>f.picture(time));
  f.frame.mockImplementationOnce(()=>new Promise<CachedPicture>(done=>{resolve=done;}));
  const starting=f.player.start(abort.signal);await flush();abort.abort();
  const late=f.picture({ticks:"1",timebase:25});resolve(late);
  await expect(starting).rejects.toMatchObject({name:"AbortError"});
  expect(late.pixels.close).toHaveBeenCalledOnce();expect(f.clearRect).toHaveBeenCalledOnce();
  expect(f.callbacks.size).toBe(0);expect(f.port.close).toHaveBeenCalledOnce();
});

it("starts at a warm window tail without granting audio credit into a cold next window",async()=>{
 const f=fixture();f.hasFrame.mockReturnValue(false);
 await f.player.start(new AbortController().signal);
 expect(f.frame).toHaveBeenCalledTimes(1);expect(f.player.snapshot().readyThrough).toEqual({ticks:"1",timebase:25});
 let resolve!:(picture:CachedPicture)=>void;
 f.frame.mockImplementationOnce(()=>new Promise<CachedPicture>(done=>{resolve=done;}));
 await f.tick();expect(f.frame).toHaveBeenCalledTimes(2);
 expect(f.player.snapshot().readyThrough).toEqual({ticks:"1",timebase:25});
 const late=f.picture({ticks:"1",timebase:25}),stopped=f.player.stop();resolve(late);await stopped;
 expect(late.pixels.close).toHaveBeenCalledOnce();expect(f.failed).not.toHaveBeenCalled();
});

it("bounds live scene preparation to twenty-four native frames and accounts every queued raster before another render",async()=>{
 const f=fixture(24);await f.player.start(new AbortController().signal);
 await f.tick();
 expect(f.frame).toHaveBeenCalledTimes(25);
 expect(f.frame.mock.calls.at(-1)?.[0]).toEqual({ticks:"24",timebase:25});
 const calls=f.frame.mock.calls as unknown as [unknown,AbortSignal,number][];
 expect(calls.at(-1)?.[2]).toBe(24*960*540*4);
 expect(calls.at(-1)![2]).toBeLessThan(64*1024*1024);
 expect(f.failed).not.toHaveBeenCalled();await f.player.stop(true);
 expect(f.bitmaps.every(b=>b.close.mock.calls.length===1)).toBe(true);
});

it("starts with a validated prefix and refills its steady reserve without crediting a blocked tail",async()=>{
 const f=fixture(24);f.binding.directPictureCredit=true;
 const base=f.frame.getMockImplementation()!;let release!:(picture:CachedPicture)=>void;
 f.frame.mockImplementation((...args)=>args[0].ticks==="5"
  ?new Promise<CachedPicture>(resolve=>{release=resolve;}) :base(...args));
 await f.player.start(new AbortController().signal);
 expect(f.frame).toHaveBeenCalledTimes(5);
 expect(f.player.snapshot().readyThrough).toEqual({ticks:"5",timebase:25});
 expect(f.port.postMessage).toHaveBeenLastCalledWith({kind:"picture-limit",token:"device",frames:"14400"});
 await f.tick();expect(f.frame).toHaveBeenCalledTimes(6);
 expect(f.player.snapshot().readyThrough).toEqual({ticks:"5",timebase:25});
 const stopping=f.player.stop(true),late=f.picture({ticks:"5",timebase:25});release(late);await stopping;
 expect(late.pixels.close).toHaveBeenCalledOnce();expect(f.failed).not.toHaveBeenCalled();
 expect(f.callbacks.size).toBe(0);expect(f.bitmaps.every(b=>b.close.mock.calls.length===1)).toBe(true);
});

it("reports the actual first picture before lookahead completes without starting the device clock",async()=>{
 const f=fixture(24);f.binding.directPictureCredit=true;
 const base=f.frame.getMockImplementation()!;let release!:(picture:CachedPicture)=>void;
 f.frame.mockImplementation((...args)=>args[0].ticks==="1"
  ?new Promise<CachedPicture>(resolve=>{release=resolve;}) :base(...args));
 let started=false;const starting=f.player.start(new AbortController().signal).then(()=>{started=true;});await flush();
 expect(started).toBe(false);expect(f.drawImage).toHaveBeenCalledOnce();
 expect(f.reports[0]).toMatchObject({presentedFrames:1,time:0,readyThrough:{ticks:"1",timebase:25}});
 expect(f.port.postMessage).toHaveBeenLastCalledWith({kind:"picture-limit",token:"device",frames:"6720"});
 expect(f.callbacks.size).toBe(0);
 release(f.picture({ticks:"1",timebase:25}));await starting;
 expect(f.player.snapshot().readyThrough).toEqual({ticks:"5",timebase:25});
 await f.player.stop(true);expect(f.failed).not.toHaveBeenCalled();
 expect(f.bitmaps.every(b=>b.close.mock.calls.length===1)).toBe(true);
});

it("pipelines at most four GPU validations and credits only their contiguous successful prefix",async()=>{
 const f=fixture(16),gates:{resolve():void;reject(error:Error):void}[]=[];
 f.frame.mockImplementation(async time=>{
  const picture=f.picture(time);
  if(time.ticks!=="0")picture.ready=new Promise<void>((resolve,reject)=>{gates.push({resolve,reject});});
  return picture;
 });
 f.binding.directPictureCredit=true;
 const start=f.player.start(new AbortController().signal);await flush();
 expect(f.frame).toHaveBeenCalledTimes(5);expect(f.player.snapshot().readyThrough).toEqual({ticks:"1",timebase:25});
 gates[3].resolve();await flush();expect(f.player.snapshot().readyThrough).toEqual({ticks:"1",timebase:25});
 expect(f.port.postMessage).toHaveBeenLastCalledWith({kind:"picture-limit",token:"device",frames:"6720"});
 expect(f.drawImage).toHaveBeenCalledTimes(1);
 gates[0].resolve();gates[1].resolve();gates[2].resolve();await start;expect(f.player.snapshot().readyThrough).toEqual({ticks:"5",timebase:25});
 expect(f.port.postMessage).toHaveBeenLastCalledWith({kind:"picture-limit",token:"device",frames:"14400"});
 await f.tick();expect(f.frame).toHaveBeenCalledTimes(9);
 gates[4].reject(Error("GPU validation failed"));await flush();
 expect(f.failed).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({message:"GPU validation failed"}));
 expect(f.drawImage).toHaveBeenCalledTimes(1);expect(f.callbacks.size).toBe(0);
 for(const gate of gates.slice(5))gate.resolve();await flush();expect(f.bitmaps.every(b=>b.close.mock.calls.length===1)).toBe(true);
});
