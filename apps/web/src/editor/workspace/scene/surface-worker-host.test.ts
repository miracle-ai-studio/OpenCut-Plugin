import {afterEach,expect,it,vi} from 'vitest';
import {SurfaceWorkerHost,SurfaceWorkerRun} from './surface-worker-host';
import type {AudioPreviewSession} from './types';
import type {CachedPicture} from './picture-cache';
import type {SurfaceCommand,SurfaceEvent} from './surface-worker-protocol';
import type {SurfacePictureSource} from './surface-player';
const flush=async()=>{for(let i=0;i<80;i++)await Promise.resolve();};
const deferred=()=>{let resolve!:()=>void;const promise=new Promise<void>(yes=>{resolve=yes;});return {promise,resolve};};
afterEach(()=>vi.unstubAllGlobals());
function fixture(maxRetainedBytes=64*1024*1024){
 class FakeWorker{onmessage:((event:{data:SurfaceEvent})=>void)|null=null;onerror:null|(()=>void)=null;messages:SurfaceCommand[]=[];postMessage=(data:SurfaceCommand)=>this.messages.push(data);terminate=vi.fn();}
 vi.stubGlobal('Worker',FakeWorker);
 const host=new SurfaceWorkerHost(),worker=host.worker as unknown as FakeWorker;
 const session={generation:'1',presentation_time:{ticks:'0',timebase:25},identity:{project_id:'p',revision:'r',sequence_id:'s',sample_rate:48000}} as AudioPreviewSession;
 const plan={project_id:'p',revision:'r',sequence_id:'s',width:2,height:2,duration:{ticks:'100',timebase:25},frame_step_seconds:.04};
 const raw:{close:ReturnType<typeof vi.fn>}[]=[],bitmaps:{close:ReturnType<typeof vi.fn>}[]=[];
 const frame=vi.fn(async(time:{ticks:string;timebase:number},_signal:AbortSignal,_retained:number):Promise<CachedPicture>=>{
  const pixels={width:2,height:2,close:vi.fn()};raw.push(pixels);
  return {frame:{prepare_ahead:true,next_time:{ticks:String(Number(time.ticks)+1),timebase:25},plan:{identity:{time}},time_seconds:Number(time.ticks)/25},pixels,runtime:{}};
 });
 const source:SurfacePictureSource={descriptor:()=>plan,preparationFrames:24,maxRetainedBytes,hasFrame:()=>true,frame};
 const exporter=vi.fn(async(_picture:CachedPicture)=>{const pixels={width:2,height:2,close:vi.fn()};bitmaps.push(pixels);return pixels;});
 const failed=vi.fn(),run=new SurfaceWorkerRun(host,source,{} as OffscreenCanvas,session,{token:'device',sampleRate:48000,graphLatencySeconds:0},{} as MessagePort,exporter,vi.fn(),failed);
 const event=(data:SurfaceEvent extends infer E?E extends {token:string}?Omit<E,'token'>:never:never)=>worker.onmessage?.({data:{...data,token:run.token} as SurfaceEvent});
 const report=run.snapshot();
 const finish=async()=>{const stopping=run.stop(true);event({kind:'stopped',report});await stopping;host.close();};
 return {host,worker,run,source,frame,exporter,raw,bitmaps,failed,event,report,finish};
}
it('stages six native frames, reserves both copies, and sends only fully validated transferable pixels',async()=>{
 const f=fixture(),gpu=deferred(),transfer=deferred();
 const base=f.frame.getMockImplementation()!;
 f.frame.mockImplementation(async(...args)=>{const p=await base(...args);if(args[0].ticks==='0')p.ready=gpu.promise;return p;});
 const exportBase=f.exporter.getMockImplementation()!;
 f.exporter.mockImplementation(async p=>{if(p.frame.plan.identity.time.ticks==='0')await transfer.promise;return exportBase(p);});
 const starting=f.run.start(new AbortController().signal);
 f.event({kind:'frame',id:1,time:{ticks:'0',timebase:25},retainedBytes:16} as never);await flush();
 expect(f.frame).toHaveBeenCalledTimes(6);expect(f.frame.mock.calls.map(c=>c[2])).toEqual([16,48,80,112,144,176]);
 expect(f.worker.messages.some(m=>m.kind==='picture')).toBe(false);
 gpu.resolve();await flush();expect(f.worker.messages.some(m=>m.kind==='picture')).toBe(false);
 transfer.resolve();await flush();const pictures=f.worker.messages.filter(m=>m.kind==='picture');expect(pictures).toHaveLength(1);
 expect(pictures[0]).toMatchObject({token:f.run.token,id:1,picture:{frame:{time_seconds:0}}});
 expect('ready' in (pictures[0] as Extract<SurfaceCommand,{kind:'picture'}>).picture).toBe(false);
 f.event({kind:'started',report:f.report});await starting;await f.finish();
 expect(f.raw.every(p=>p.close.mock.calls.length===1)).toBe(true);expect(f.bitmaps.every(p=>p.close.mock.calls.length===1)).toBe(true);expect(f.failed).not.toHaveBeenCalled();
});
it('initial cancellation closes all staged textures and a late validation cannot export old pixels',async()=>{
 const f=fixture(),gpu=deferred(),base=f.frame.getMockImplementation()!;
 f.frame.mockImplementation(async(...args)=>({...await base(...args),ready:gpu.promise}));
 const abort=new AbortController(),starting=f.run.start(abort.signal);f.event({kind:'frame',id:1,time:{ticks:'0',timebase:25},retainedBytes:16} as never);await flush();
 abort.abort();await expect(starting).rejects.toMatchObject({name:'AbortError'});f.event({kind:'stopped',report:f.report});
 gpu.resolve();await flush();expect(f.exporter).not.toHaveBeenCalled();expect(f.worker.messages.some(m=>m.kind==='picture')).toBe(false);
 expect(f.raw.every(p=>p.close.mock.calls.length===1)).toBe(true);await f.finish();
});
it('never allocates beyond resident credit and never retries an authority denial',async()=>{
 const f=fixture(),starting=f.run.start(new AbortController().signal);f.event({kind:'frame',id:1,time:{ticks:'0',timebase:25},retainedBytes:64*1024*1024} as never);await flush();
 expect(f.frame).not.toHaveBeenCalled();expect(f.worker.messages.at(-1)).toMatchObject({kind:'pictureError',error:{code:'VISUAL_BUDGET_EXCEEDED'}});
 f.frame.mockRejectedValue(Object.assign(Error('Revoked'),{code:'NOT_AUTHORIZED'}));f.event({kind:'frame',id:2,time:{ticks:'0',timebase:25},retainedBytes:16} as never);
 await expect(starting).rejects.toMatchObject({code:'NOT_AUTHORIZED'});expect(f.frame).toHaveBeenCalledTimes(1);expect(f.failed).toHaveBeenCalledTimes(1);
 f.event({kind:'stopped',report:f.report});await f.finish();
});
it('waits for each host claim, overlaps at most two incomplete reads, and retires late pixels',async()=>{
 const f=fixture(),stages:{claim:ReturnType<typeof deferred>;pixels:ReturnType<typeof deferred>}[]=[];
 const prepare=vi.fn(async(time:Parameters<SurfacePictureSource['frame']>[0],signal:AbortSignal,retained:number)=>{
  const picture=await f.frame(time,signal,retained),claim=deferred(),pixels=deferred();stages.push({claim,pixels});
  return {frame:picture.frame,claimed:claim.promise,complete:pixels.promise.then(()=>picture)};
 });
 f.source.prepareFrame=prepare;
 const abort=new AbortController(),starting=f.run.start(abort.signal);
 f.event({kind:'frame',id:1,time:{ticks:'0',timebase:25},retainedBytes:16} as never);await flush();
 expect(prepare).toHaveBeenCalledTimes(1);expect(f.worker.messages.some(m=>m.kind==='picture')).toBe(false);
 stages[0].claim.resolve();await flush();expect(prepare).toHaveBeenCalledTimes(2);
 stages[1].claim.resolve();await flush();expect(prepare).toHaveBeenCalledTimes(2);
 stages[0].pixels.resolve();await flush();expect(prepare).toHaveBeenCalledTimes(3);
 stages[2].claim.resolve();await flush();expect(prepare).toHaveBeenCalledTimes(3);
 expect(prepare.mock.calls.map(c=>c[2])).toEqual([16,48,64]);
 expect(f.worker.messages.filter(m=>m.kind==='picture')).toMatchObject([{id:1,picture:{frame:{time_seconds:0}}}]);
 // Stop must finish even when two body reads never finish; late completions
 // must release their bitmaps without reaching the presentation worker.
 abort.abort();await expect(starting).rejects.toMatchObject({name:'AbortError'});f.event({kind:'stopped',report:f.report});
 await f.finish();stages[1].pixels.resolve();stages[2].pixels.resolve();await flush();
 expect(prepare).toHaveBeenCalledTimes(3);expect(f.raw.every(p=>p.close.mock.calls.length===1)).toBe(true);
 expect(f.bitmaps.every(p=>p.close.mock.calls.length===1)).toBe(true);
 expect(f.worker.messages.filter(m=>m.kind==='picture')).toHaveLength(1);expect(f.failed).not.toHaveBeenCalled();
});
it('stops the native intent stream when its reader claim is denied',async()=>{
 const f=fixture();let deny!:(error:Error)=>void;const claim=new Promise<void>((_yes,no)=>{deny=no;});
 let finishPixels!:()=>void;
 const pixels=new Promise<void>(yes=>{finishPixels=yes;});
 const prepare=vi.fn(async(time:Parameters<SurfacePictureSource['frame']>[0],signal:AbortSignal,retained:number)=>{
  const picture=await f.frame(time,signal,retained);return {frame:picture.frame,claimed:claim,complete:pixels.then(()=>picture)};
 });
 f.source.prepareFrame=prepare;const starting=f.run.start(new AbortController().signal);
 f.event({kind:'frame',id:1,time:{ticks:'0',timebase:25},retainedBytes:16} as never);await flush();
 deny(Object.assign(Error('Revoked claim'),{code:'NOT_AUTHORIZED'}));
 await expect(starting).rejects.toMatchObject({code:'NOT_AUTHORIZED'});
 f.event({kind:'stopped',report:f.report});await f.finish();finishPixels();await flush();
 expect(prepare).toHaveBeenCalledOnce();expect(f.failed).toHaveBeenCalledOnce();expect(f.raw[0].close).toHaveBeenCalledOnce();
});
it('delivers a verified head without waiting for a later native preparation',async()=>{
 const f=fixture(),later=deferred();
 const prepare=vi.fn(async(time:Parameters<SurfacePictureSource['frame']>[0],signal:AbortSignal,retained:number)=>{
  if(time.ticks==='1')await later.promise;
  const picture=await f.frame(time,signal,retained);
  return {frame:picture.frame,claimed:Promise.resolve(),complete:Promise.resolve(picture)};
 });
 f.source.prepareFrame=prepare;
 const starting=f.run.start(new AbortController().signal);f.event({kind:'started',report:f.report});await starting;
 try{
  f.event({kind:'frame',id:1,time:{ticks:'0',timebase:25},retainedBytes:16} as never);await flush();
  expect(prepare).toHaveBeenCalledTimes(2);
  expect(f.worker.messages.filter(m=>m.kind==='picture')).toMatchObject([{id:1,picture:{frame:{time_seconds:0}}}]);
  expect(f.failed).not.toHaveBeenCalled();
 }finally{later.resolve();await flush();await f.finish();}
 expect(f.raw.every(p=>p.close.mock.calls.length===1)).toBe(true);
 expect(f.bitmaps.every(p=>p.close.mock.calls.length===1)).toBe(true);
});
it('requires the head claim even when its pixels have completed',async()=>{
 const f=fixture(),claim=deferred(),later=deferred();
 f.source.prepareFrame=async(time,signal,retained)=>{
  if(time.ticks!=='0')await later.promise;
  const picture=await f.frame(time,signal,retained);
  return {frame:picture.frame,claimed:time.ticks==='0'?claim.promise:Promise.resolve(),complete:Promise.resolve(picture)};
 };
 const starting=f.run.start(new AbortController().signal);f.event({kind:'started',report:f.report});await starting;
 try{
  f.event({kind:'frame',id:1,time:{ticks:'0',timebase:25},retainedBytes:16} as never);await flush();
  expect(f.exporter).toHaveBeenCalledOnce();expect(f.worker.messages.some(m=>m.kind==='picture')).toBe(false);
  claim.resolve();await flush();expect(f.worker.messages.filter(m=>m.kind==='picture')).toHaveLength(1);
 }finally{claim.resolve();later.resolve();await flush();await f.finish();}
});
it('charges an early transferred bitmap until the next presentation credit report',async()=>{
 for(const capacity of [64*1024*1024,128*1024*1024]){
 const f=fixture(capacity),bytes=16;
 const starting=f.run.start(new AbortController().signal);f.event({kind:'started',report:f.report});await starting;
 expect(f.worker.messages.find(m=>m.kind==='start')).toMatchObject({maxRetainedBytes:capacity});
 try{
  f.event({kind:'frame',id:1,time:{ticks:'0',timebase:25},retainedBytes:capacity-3*bytes} as never);await flush();
  expect(f.frame.mock.calls.map(c=>c[2])).toEqual([capacity-3*bytes,capacity-2*bytes]);
  expect(f.worker.messages.filter(m=>m.kind==='picture')).toHaveLength(1);
  expect(f.failed).not.toHaveBeenCalled();
 }finally{await f.finish();}
 expect(f.raw.every(p=>p.close.mock.calls.length===1)).toBe(true);
 expect(f.bitmaps.every(p=>p.close.mock.calls.length===1)).toBe(true);
 }
});

it('transfers a completed native catch-up interval with its original request binding',async()=>{
 const f=fixture(),base=f.frame.getMockImplementation()!;
 f.frame.mockImplementation(async(time,...args)=>{
  if(time.ticks!=='1')return base(time,...args);
  const p=await base({ticks:'3',timebase:25},...args);p.frame.catchUp={requestedTime:time,consumerSeconds:.13};return p;
 });
 const starting=f.run.start(new AbortController().signal);f.event({kind:'started',report:f.report});await starting;
 try{
  f.event({kind:'frame',id:1,time:{ticks:'0',timebase:25},retainedBytes:16} as never);await flush();
  f.event({kind:'frame',id:2,time:{ticks:'1',timebase:25},retainedBytes:16} as never);await flush();
  expect(f.worker.messages.filter(m=>m.kind==='picture').at(-1)).toMatchObject({id:2,picture:{frame:{plan:{identity:{time:{ticks:'3',timebase:25}}},catchUp:{requestedTime:{ticks:'1',timebase:25},consumerSeconds:.13}}}});
  expect(f.failed).not.toHaveBeenCalled();
 }finally{await f.finish();}
});
