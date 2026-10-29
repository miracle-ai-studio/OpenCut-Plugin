import type {PreparedSurfacePicture,SurfacePictureSource,SurfaceReport} from './surface-player';
import type {CachedPicture} from './picture-cache';
import type {AudioPreviewSession} from './types';
import type {DeviceTimestamp,PresentationClockBinding} from './audio-preview/presentation-clock';
import {surfaceError,PRODUCER_AHEAD} from './surface-worker-protocol';
import type {SurfaceCommand,SurfaceEvent} from './surface-worker-protocol';
const isPressure=(error:unknown)=>error instanceof Error&&'code' in error&&['VISUAL_BUDGET_EXCEEDED','PREVIEW_FRAME_STALE'].includes(String(error.code));
const compare=(a:{ticks:string;timebase:number},b:{ticks:string;timebase:number})=>BigInt(a.ticks)*BigInt(b.timebase)-BigInt(b.ticks)*BigInt(a.timebase);
type StagedPicture={frame:CachedPicture['frame'];picture?:CachedPicture;claimed:Promise<void>;ready:Promise<void>;released:boolean};
async function waitFor<T>(pending:Promise<T>,signal:AbortSignal):Promise<T>{
 signal.throwIfAborted();let abort!:()=>void;
 try{return await Promise.race([pending,new Promise<never>((_yes,no)=>{abort=()=>no(signal.reason);signal.addEventListener('abort',abort,{once:true});if(signal.aborted)abort();})]);}
 finally{signal.removeEventListener('abort',abort);}
}
/** One execution-only display worker, retained with its transferred canvas. */
export class SurfaceWorkerHost {
 readonly worker=new Worker(new URL('./surface-worker.ts',import.meta.url),{type:'module',name:'opencut-device-surface'});
 private captured=false;
 closed=false;
 current:SurfaceWorkerRun|null=null;
 constructor(){
  this.worker.onmessage=({data}:MessageEvent<SurfaceEvent>)=>{if(data.token===this.current?.token)this.current.event(data);};
  this.worker.onerror=()=>this.current?.fail(Error('Device presentation worker failed'));
 }
 post(command:SurfaceCommand,transfer:Transferable[]=[]){if(this.closed)throw Error('Presentation host is closed');this.worker.postMessage(command,transfer);}
 takeCanvas(canvas:OffscreenCanvas){if(this.captured)return undefined;this.captured=true;return canvas;}
 retire(){this.closed=true;this.worker.terminate();this.current=null;}
 close(){this.current?.fail(new DOMException('Presentation host closed','AbortError'));this.retire();}
}
/** Producer-side bounded staging; no source-time or composition evaluation. */
export class SurfaceWorkerRun {
 readonly token=crypto.randomUUID();
 private abort=new AbortController();
 private queue:StagedPicture[]=[];
 private reading=new Set<Promise<void>>();
 private next:AudioPreviewSession['presentation_time'];
 private filling:Promise<void>|null=null;
 private headAvailable:(()=>void)|null=null;
 private retained=0;
 private closed=false;
 private serving=false;
 private report:SurfaceReport;
 private startResolve!:(report:SurfaceReport)=>void;
 private startReject!:(error:Error)=>void;
 private stopResolve:((report:SurfaceReport)=>void)|null=null;
 private stopping:Promise<SurfaceReport>|null=null;
 constructor(private host:SurfaceWorkerHost,private source:SurfacePictureSource,private canvas:OffscreenCanvas,private session:AudioPreviewSession,private binding:PresentationClockBinding,private port:MessagePort,
  private exportBitmap:(picture:CachedPicture)=>ImageBitmap|Promise<ImageBitmap>,private updated:(report:SurfaceReport)=>void,private failed:(error:Error)=>void){
  if(host.closed||host.current)throw Error('Device surface still owns an earlier run or is closed');host.current=this;this.next=session.presentation_time;
  this.report={time:Number(this.next.ticks)/this.next.timebase,frameTime:this.next,readyThrough:this.next,generation:session.generation,runtime:'{}',presentedFrames:0,discardedFrames:0,statistics:'{}'};
 }
 start(signal:AbortSignal){
  signal.throwIfAborted();
  const promise=new Promise<SurfaceReport>((yes,no)=>{this.startResolve=yes;this.startReject=no;});
  const abort=()=>{void this.stop(true).catch(()=>{});this.startReject(new DOMException('Presentation replaced','AbortError'));};signal.addEventListener('abort',abort,{once:true});
  const timer=setTimeout(()=>this.fail(Error('Device presentation did not initialize')),10000);
  void promise.finally(()=>{clearTimeout(timer);signal.removeEventListener('abort',abort);}).catch(()=>{});
  const canvas=this.host.takeCanvas(this.canvas);
  this.host.post({token:this.token,kind:'start',...(canvas?{canvas}:{}),session:this.session,plan:this.source.descriptor(),capacity:this.source.preparationFrames??8,clockPort:this.port,binding:this.binding},[...(canvas?[canvas]:[]),this.port,...(this.binding.outputPort?[this.binding.outputPort]:[])]);return promise;
 }
 timestamp(timestamp:DeviceTimestamp){if(!this.closed)this.host.post({token:this.token,kind:'timestamp',timestamp});}
 snapshot(){return this.report;}
 private decorate(report:SurfaceReport){return {...report,statistics:JSON.stringify({...JSON.parse(report.statistics),presentationThread:'dedicated-worker',producerPreparedFrames:this.queue.length,
  producerNext:this.next,producerFilling:!!this.filling,producerServing:this.serving,producerReadingFrames:this.reading.size,producerRetainedBytes:this.retained})};}
 event(data:SurfaceEvent){
  if(data.kind==='stopped'){this.report=this.decorate(data.report);this.stopResolve?.(this.report);if(this.host.current===this)this.host.current=null;return;}
  if(this.closed)return;
  if(data.kind==='failed'){this.fail(Object.assign(new Error(data.error.message),data.error));return;}
  if(data.kind==='report'||data.kind==='started'){
   const report=this.decorate(data.report);
   this.report=report;this.updated(report);if(data.kind==='started')this.startResolve(report);return;
  }
  if(data.kind==='frame')void this.serve(data).catch(error=>this.fail(error instanceof Error?error:Error(String(error))));
 }
 private release(stage:StagedPicture){if(stage.picture&&!stage.released){stage.released=true;stage.picture.pixels.close();}}
 private stage(pending:PreparedSurfacePicture):StagedPicture{
  const stage:StagedPicture={frame:pending.frame,claimed:pending.claimed,ready:Promise.resolve(),released:false};
  stage.ready=(async()=>{
   const picture=await pending.complete;stage.picture=picture;
   if(this.closed){this.release(stage);return;}
   await picture.ready;if(this.closed)return;
   const original=picture.pixels,bitmap=await this.exportBitmap(picture);
   if(this.closed){if(bitmap!==original)bitmap.close();return;}
   if(bitmap!==original)original.close();picture.pixels=bitmap;
  })();
  void stage.ready.catch(error=>this.fail(error instanceof Error?error:Error(String(error))));
  return stage;
 }
 private fill(){
  if(this.filling)return this.filling;
  const task=(async()=>{
   const plan=this.source.descriptor(),bytes=plan.width*plan.height*4;
   // Reserve both the owned texture and its future transferable bitmap before
   // either asynchronous export or another native frame can allocate.
   const retained=()=>this.retained+this.queue.length*bytes*2;
   while(!this.closed&&this.queue.length<PRODUCER_AHEAD&&compare(this.next,plan.duration)<0n&&retained()+bytes*2<=64*1024*1024){
    if(this.source.prepareFrame&&this.reading.size>=2){await waitFor(Promise.race(this.reading),this.abort.signal);continue;}
    let pending:PreparedSurfacePicture;
    try{
     if(this.source.prepareFrame)pending=await this.source.prepareFrame(this.next,this.abort.signal,retained());
     else{const picture=await this.source.frame(this.next,this.abort.signal,retained());pending={frame:picture.frame,claimed:Promise.resolve(),complete:Promise.resolve(picture)};}
    }
    catch(error){if(isPressure(error)&&this.queue.length)return;throw error;}
    void pending.claimed.catch(()=>{});
    if(this.closed){void pending.complete.then(p=>p.pixels.close(),()=>{});return;}
    const stage=this.stage(pending);this.next=stage.frame.next_time;this.queue.push(stage);this.headAvailable?.();
    if(this.source.prepareFrame){this.reading.add(stage.ready);void stage.ready.finally(()=>this.reading.delete(stage.ready)).catch(()=>{});}
    // A new native intent retires an unclaimed ticket. Response headers prove
    // the host already owns this reader; byte/hash/bitmap work may continue.
    await waitFor(pending.claimed,this.abort.signal);
   }
  })();this.filling=task;void task.finally(()=>{if(this.filling===task)this.filling=null;}).catch(()=>{});return task;
 }
 private async head(){
  if(this.queue.length)return;
  let wake!:()=>void;const queued=new Promise<void>(resolve=>{wake=resolve;});this.headAvailable=wake;
  try{await waitFor(Promise.race([queued,this.fill()]),this.abort.signal);}
  finally{if(this.headAvailable===wake)this.headAvailable=null;}
 }
 private refill(){void this.fill().catch(error=>{if(!isPressure(error))this.fail(error instanceof Error?error:Error(String(error)));});}
 private async serve(request:Extract<SurfaceEvent,{kind:'frame'}>){
  if(this.serving||!Number.isSafeInteger(request.retainedBytes)||request.retainedBytes<0||request.retainedBytes>64*1024*1024)throw Error('Invalid presentation preparation credit');
  this.serving=true;this.retained=request.retainedBytes;let exported:ImageBitmap|undefined;
  try{
   // A ready head must not wait for unrelated later preparation or body reads.
   await this.head();this.refill();
   const stage=this.queue[0];if(!stage)throw Object.assign(Error('Presentation has no admitted next frame'),{code:'VISUAL_BUDGET_EXCEEDED'});
   const at=stage.frame.plan.identity.time;
   if(compare(request.time,at)<0n||compare(request.time,stage.frame.next_time)>=0n)throw Error('Presentation requested a discontinuous frame');
   await waitFor(Promise.all([stage.claimed,stage.ready]),this.abort.signal);this.abort.signal.throwIfAborted();
   const picture=stage.picture;if(!picture)throw Error('Presentation pixels did not complete');
   exported=picture.pixels;this.abort.signal.throwIfAborted();
   const result:CachedPicture={pixels:exported,frame:{prepare_ahead:true,next_time:picture.frame.next_time,plan:{identity:{time:at}},time_seconds:picture.frame.time_seconds},runtime:picture.runtime};
   this.host.post({token:this.token,kind:'picture',id:request.id,picture:result},[exported]);exported=undefined;
   // Keep the transferred bitmap charged until the next worker credit report.
   const plan=this.source.descriptor();this.retained+=plan.width*plan.height*4;
   this.queue.shift();this.release(stage);this.refill();
  }catch(error){
   exported?.close();
   if(!this.closed)this.host.post({token:this.token,kind:'pictureError',id:request.id,error:surfaceError(error)});
   if(!isPressure(error)&&!this.closed)throw error;
  }finally{this.serving=false;}
 }
 fail(error:Error){if(this.closed)return;this.startReject?.(error);this.failed(error);void this.stop(true).catch(()=>{});}
 stop(clear=false):Promise<SurfaceReport>{
  if(this.stopping)return this.stopping;
  this.closed=true;this.abort.abort();for(const stage of this.queue)this.release(stage);this.queue=[];
  this.stopping=new Promise<SurfaceReport>((resolve,reject)=>{
   const timer=setTimeout(()=>{this.host.retire();reject(Error('Device presentation did not retire'));},3000);
   this.stopResolve=report=>{clearTimeout(timer);resolve(report);};
   this.host.post({token:this.token,kind:'stop',clear});
  }).then(async report=>{await this.filling?.catch(()=>{});return report;});return this.stopping;
 }
}
