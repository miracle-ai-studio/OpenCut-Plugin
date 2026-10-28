import {SurfacePlayer} from './surface-player';
import type {SurfacePictureSource} from './surface-player';
import type {CachedPicture} from './picture-cache';
import {surfaceError,PRODUCER_AHEAD} from './surface-worker-protocol';
import type {SurfaceCommand,SurfaceEvent} from './surface-worker-protocol';
const scope=self as unknown as {onmessage:((event:MessageEvent<SurfaceCommand>)=>void)|null;postMessage(event:SurfaceEvent):void;close():void};
let canvas:OffscreenCanvas|null=null;
type Run={token:string;abort:AbortController;player:SurfacePlayer|null;pending:Map<number,{time:{ticks:string;timebase:number};resolve:(p:CachedPicture)=>void;reject:(e:Error)=>void}>;serial:number;empty:import('./surface-player').SurfaceReport};
let current:Run|null=null;
const errorFrom=(value:{name:string;message:string;code?:string})=>Object.assign(new Error(value.message),value);
const stop=async(clear:boolean)=>{
 const run=current;if(!run)return;
 run.abort.abort();for(const pending of run.pending.values())pending.reject(new DOMException('Presentation retired','AbortError'));run.pending.clear();
 const report=await run.player?.stop(clear)??run.empty;scope.postMessage({token:run.token,kind:'stopped',report});
 if(current===run)current=null;
};
scope.onmessage=({data})=>{
 if(data.kind==='close'){void stop(true).finally(()=>{scope.close();});return;}
 if(data.kind==='start'){
  if(current){scope.postMessage({token:data.token,kind:'failed',error:surfaceError(Error('Presentation worker already owns a run'))});return;}
  const run:Run={token:data.token,abort:new AbortController(),player:null,pending:new Map(),serial:0,empty:{time:Number(data.session.presentation_time.ticks)/data.session.presentation_time.timebase,frameTime:data.session.presentation_time,readyThrough:data.session.presentation_time,generation:data.session.generation,runtime:'{}',presentedFrames:0,discardedFrames:0,statistics:'{}'}};current=run;
  void(async()=>{
   if(data.canvas){if(canvas)throw Error('Presentation canvas already captured');canvas=data.canvas;}
   if(!canvas)throw Error('Presentation canvas missing');run.abort.signal.throwIfAborted();
   const source:SurfacePictureSource={descriptor:()=>data.plan,preparationFrames:Math.max(1,data.capacity-PRODUCER_AHEAD),hasFrame:time=>BigInt(time.ticks)*BigInt(data.plan.duration.timebase)<BigInt(data.plan.duration.ticks)*BigInt(time.timebase),
    frame:(time,signal,retainedBytes=0)=>new Promise<CachedPicture>((resolve,reject)=>{
     signal.throwIfAborted();const id=++run.serial;
     run.pending.set(id,{time,resolve,reject});scope.postMessage({token:run.token,kind:'frame',id,time,retainedBytes});
    })};
   const player=new SurfacePlayer(source,canvas,data.session,data.binding,data.clockPort,report=>scope.postMessage({token:run.token,kind:'report',report}),error=>{
    scope.postMessage({token:run.token,kind:'failed',error:surfaceError(error)});void stop(true);
   });run.player=player;
   const report=await player.start(run.abort.signal);if(current===run)scope.postMessage({token:run.token,kind:'started',report});
  })().catch(error=>{if(current===run&&!run.abort.signal.aborted){scope.postMessage({token:run.token,kind:'failed',error:surfaceError(error)});void stop(true);}});return;
 }
 const run=current;if(!run||data.token!==run.token){if(data.kind==='picture')data.picture.pixels.close();return;}
 if(data.kind==='timestamp'){run.player?.timestamp(data.timestamp);return;}
 if(data.kind==='stop'){void stop(data.clear);return;}
 if(data.kind==='picture'||data.kind==='pictureError'){
  const pending=run.pending.get(data.id);if(!pending){if(data.kind==='picture')data.picture.pixels.close();return;}run.pending.delete(data.id);
  if(data.kind==='pictureError'){pending.reject(errorFrom(data.error));return;}
  const p=data.picture,t=p.frame.plan.identity.time,next=p.frame.next_time,at=pending.time;
  if(!(p.pixels instanceof ImageBitmap)||p.pixels.width!==canvas!.width||p.pixels.height!==canvas!.height||!p.frame.prepare_ahead||BigInt(t.ticks)*BigInt(at.timebase)>BigInt(at.ticks)*BigInt(t.timebase)||BigInt(next.ticks)*BigInt(at.timebase)<=BigInt(at.ticks)*BigInt(next.timebase)){
   p.pixels.close();pending.reject(Error('Transferred picture changed its requested interval or raster'));return;
  }pending.resolve(p);
 }
};
