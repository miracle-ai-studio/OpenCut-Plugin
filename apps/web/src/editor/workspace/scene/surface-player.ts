import {PreviewOutputClock} from "./audio-preview/output-clock";
import type {DeviceReceipt} from "./audio-preview/device-port";
import type {DeviceTimestamp,PresentationClockBinding} from "./audio-preview/presentation-clock";
import {pictureConsumptionLimit} from "./audio-preview/presentation-clock";
import type {AudioPreviewSession} from "./types";
import type {CachedPicture,PictureCachePlan} from "./picture-cache";
import {surfaceByteCapacity} from "./surface-worker-protocol";

type Time=AudioPreviewSession["presentation_time"];
const compare=(a:Time,b:Time)=>BigInt(a.ticks)*BigInt(b.timebase)-BigInt(b.ticks)*BigInt(a.timebase);
async function waitPicture(picture:CachedPicture,signal:AbortSignal){
  if(!picture.ready)return;
  let abort!:()=>void;
  try {await Promise.race([picture.ready,new Promise<never>((_yes,no)=>{abort=()=>no(signal.reason);signal.addEventListener("abort",abort,{once:true});if(signal.aborted)abort();})]);}
  finally {signal.removeEventListener("abort",abort);}
}
export type SurfacePainter={draw(bitmap:ImageBitmap):void;clear():void};
/** Native frames are complete compositions; replace rather than blend them. */
export function createSurfacePainter(canvas:OffscreenCanvas):SurfacePainter {
  const context=canvas.getContext("bitmaprenderer");
  if(!context)throw Error("Preview display is unavailable");
  return {draw:bitmap=>context.transferFromImageBitmap(bitmap),clear:()=>context.transferFromImageBitmap(null)};
}
export type PreparedSurfacePicture={
  frame:CachedPicture["frame"];
  claimed:Promise<void>;
  complete:Promise<CachedPicture>;
};
export interface SurfacePictureSource {
  readonly maxRetainedBytes?:number;
  readonly preparationFrames?:number;
  createPainter?(canvas:OffscreenCanvas):SurfacePainter|undefined;
  descriptor():Pick<PictureCachePlan,"project_id"|"revision"|"sequence_id"|"width"|"height"|"duration"|"frame_step_seconds">;
  hasFrame(time:AudioPreviewSession["presentation_time"]):boolean;
  frame(time:AudioPreviewSession["presentation_time"],signal:AbortSignal,retainedBytes?:number):Promise<CachedPicture>;
  prepareFrame?(time:AudioPreviewSession["presentation_time"],signal:AbortSignal,retainedBytes:number):Promise<PreparedSurfacePicture>;
}
export type SurfaceReport={time:number;frameTime:Time;readyThrough:Time;generation:string;runtime:string;presentedFrames:number;discardedFrames:number;statistics:string};
export type SurfaceScheduler={request(callback:FrameRequestCallback):number;cancel(id:number):void;now():number;timeOrigin:number};
// Page animation callbacks may be delayed by unrelated workbench composition.
// Wake this device-clock consumer independently; native frame intervals still
// decide whether a queued picture is eligible for presentation.
const timers=globalThis as unknown as Pick<Window,"setTimeout"|"clearTimeout">;
const scheduler:SurfaceScheduler={request:callback=>timers.setTimeout(()=>callback(performance.now()),8),cancel:id=>timers.clearTimeout(id),now:()=>performance.now(),timeOrigin:performance.timeOrigin};

/** Cached pixels are presented beside their decoder, outside the UI thread.
 * This samples the same Worklet intervals and device timestamp as the main
 * output adapter. Native frame mappings remain the only composition timeline. */
export class SurfacePlayer {
  private abort=new AbortController();
  private clock:PreviewOutputClock;
  private projection:DeviceTimestamp|null=null;
  private queue:CachedPicture[]=[];
  private next:Time|null=null;
  private filling:Promise<void>|null=null;
  private frame:number|null=null;
  private closed=false;
  private stopping:Promise<SurfaceReport>|null=null;
  private last:CachedPicture["frame"]|null=null;
  private lastRuntime:Record<string,string>={};
  private presented=0;
  private discarded=0;
  private missed=0;
  private maximumGap=0;
  private previousOutput:Time|null=null;
  private lags:number[]=[];
  private skipped:{from:number;to:number}[]=[];
  private reportAt=-Infinity;
  private painter:SurfacePainter;
  private capacity:number;
  private maxRetainedBytes:number;
  private validated=new WeakSet<CachedPicture>();
  private validating=0;
  private slowPreparations:Record<string,number>[]=[];
  private credited=0n;
  private projectedFrames:string|null=null;
  private wallDrawAt:number|null=null;
  private maximumWallGap=0;
  private maximumWallGapAt=0;
  private maximumWallGapBuffered=0;
  constructor(private cache:SurfacePictureSource,private canvas:OffscreenCanvas,private session:AudioPreviewSession,
    private binding:PresentationClockBinding,private port:MessagePort,private updated:(report:SurfaceReport)=>void,
    private failed:(error:Error)=>void,private screen:SurfaceScheduler=scheduler) {
    if(binding.sampleRate!==session.identity.sample_rate||!Number.isFinite(binding.graphLatencySeconds)||binding.graphLatencySeconds<0||binding.graphLatencySeconds>1)
      throw Error("Invalid presentation clock binding");
    const plan=cache.descriptor();
    this.capacity=Math.min(64,Math.max(1,cache.preparationFrames??8));
    this.maxRetainedBytes=surfaceByteCapacity(cache);
    if(plan.project_id!==session.identity.project_id||plan.revision!==session.identity.revision||plan.sequence_id!==session.identity.sequence_id)
      throw Error("Presentation cache differs from its fixed session");
    this.clock=new PreviewOutputClock(binding.sampleRate,binding.graphLatencySeconds);
    if(canvas.width!==plan.width)canvas.width=plan.width;if(canvas.height!==plan.height)canvas.height=plan.height;
    this.painter=cache.createPainter?.(canvas)??createSurfacePainter(canvas);
    port.onmessage=({data}:MessageEvent<DeviceReceipt|{kind:"stopped";token:string;consumedFrames:string}>)=>{
      if(this.closed||data.token!==binding.token)return;
      try {if(data.kind==="progress")this.clock.observe(data);}
      catch(error){this.failure(error);}
    };
    port.start();
  }
  timestamp(value:DeviceTimestamp) {
    if(this.closed)return;
    if(!Number.isFinite(value.contextTime)||!Number.isFinite(value.performanceTime)||!Number.isFinite(value.timeOrigin)) {
      this.failure(Error("Invalid device timestamp projection"));return;
    }
    this.projection=value;
  }
  async start(signal:AbortSignal) {
    const abort=()=>this.abort.abort(signal.reason);
    signal.addEventListener("abort",abort,{once:true});
    try {
      signal.throwIfAborted();
      const first=await this.cache.frame(this.session.presentation_time,this.abort.signal);
      try {await waitPicture(first,this.abort.signal);}catch(error){first.pixels.close();throw error;}
      if(this.closed||signal.aborted||this.abort.signal.aborted){first.pixels.close();throw new DOMException("Cached presentation replaced","AbortError");}
      this.next=first.frame.next_time;this.draw(first,this.session.presentation_time);
      // Present the completed first picture while startup lookahead is still
      // preparing. The completed native horizon bounds the short audio lead.
      this.publish(true);
      // Establish the existing bounded reserve before the first PCM starts.
      // Otherwise cold clip entry consumes a four-frame prefix before the
      // producer can build its steady lookahead. Cold windows and pending GPU
      // validation still fence preparation; no extra memory or audio lead.
      await this.fill(false);signal.throwIfAborted();this.abort.signal.throwIfAborted();
      await Promise.all(this.queue.map(picture=>waitPicture(picture,this.abort.signal)));
      if(this.closed)throw new DOMException("Cached presentation closed","AbortError");
      this.publish(true);this.frame=this.screen.request(this.tick);
      return this.snapshot();
    } catch(error) {await this.stop(true);throw error;}
    finally {signal.removeEventListener("abort",abort);}
  }
  private fill(waitForCold=true) {
    if(this.filling)return this.filling;
    const task=(async()=>{
      const plan=this.cache.descriptor();
      while(!this.closed&&!this.abort.signal.aborted&&this.queue.length<this.capacity&&this.validating<4&&this.next&&compare(this.next,plan.duration)<0n) {
        // Startup only credits decoded pictures from an already published
        // window. A cold reserve is filled asynchronously after audio starts.
        if(!waitForCold&&!this.cache.hasFrame(this.next))break;
        const retained=this.canvas.width*this.canvas.height*4+this.queue.reduce((bytes,p)=>bytes+p.pixels.width*p.pixels.height*4,0);
        if(this.queue.length&&retained+plan.width*plan.height*4>this.maxRetainedBytes)break;
        let picture:CachedPicture;
        try {picture=await this.cache.frame(this.next,this.abort.signal,retained);}
        catch(error){
          if(this.queue.length&&!this.abort.signal.aborted&&error instanceof Error&&"code" in error&&["VISUAL_BUDGET_EXCEEDED","PREVIEW_FRAME_STALE"].includes(String(error.code)))break;
          throw error;
        }
        if(this.closed||this.abort.signal.aborted){picture.pixels.close();return;}
        if(Number(picture.runtime.totalMs)>80){
          const sample:Record<string,number>={time:picture.frame.time_seconds};
          for(const key of ["totalMs","planMs","resourcePrepareMs","decodeMs","sourceOpenMs","sourceDecodeMs","mediaBindMs","composeMs","gpuMs","rangeReadMs"]){const value=Number(picture.runtime[key]);if(Number.isFinite(value))sample[key]=value;}
          this.slowPreparations.push(sample);
          this.slowPreparations.sort((a,b)=>(b.totalMs??0)-(a.totalMs??0));this.slowPreparations.length=Math.min(16,this.slowPreparations.length);
        }
        if(compare(picture.frame.next_time,picture.frame.plan.identity.time)<=0n){picture.pixels.close();throw Error("Invalid cached frame interval");}
        this.next=picture.frame.next_time;this.queue.push(picture);
        if(picture.ready){
          this.validating++;
          void picture.ready.then(()=>{this.validating--;if(this.closed)return;this.validated.add(picture);this.publish(true);},error=>{this.validating--;this.failure(error);});
        }else this.validated.add(picture);
      }
    })();
    this.filling=task;void task.then(()=>{if(this.filling===task)this.filling=null;},()=>{if(this.filling===task)this.filling=null;});
    return task;
  }
  private tick:FrameRequestCallback=()=>{
    this.frame=null;if(this.closed)return;
    try {
      const projection=this.projection;
      if(projection) {
        const observed=this.clock.sample({contextTime:projection.contextTime,
          performanceTime:projection.timeOrigin+projection.performanceTime-this.screen.timeOrigin},this.screen.now());
        if(observed?.output) {
          this.projectedFrames=observed.output.frames;
          if(this.wallDrawAt===null&&BigInt(observed.output.frames)>0n)this.wallDrawAt=this.screen.now();
          this.binding.outputPort?.postMessage({kind:'output',token:this.binding.token,observation:observed.output,stalled:observed.stalled});
          const at={ticks:String(BigInt(this.session.actual_start_sample)+BigInt(observed.output.frames)),timebase:this.binding.sampleRate};
          let chosen:CachedPicture|undefined;
          while(this.queue.length&&this.validated.has(this.queue[0])&&compare(this.queue[0].frame.plan.identity.time,at)<=0n) {
            if(chosen){chosen.pixels.close();this.discarded++;}chosen=this.queue.shift();
          }
          if(chosen) {
            const lag=Number(compare(at,chosen.frame.plan.identity.time))*1000/(at.timebase*chosen.frame.plan.identity.time.timebase);
            if(lag>125){chosen.pixels.close();this.discarded++;}else this.draw(chosen,at);
          }
          if(this.last) {
            const lag=Number(compare(at,this.last.plan.identity.time))*1000/(at.timebase*this.last.plan.identity.time.timebase);
            if(lag>=0&&Number.isFinite(lag)){if(this.lags.length>=4096)this.lags.shift();this.lags.push(lag);}
          }
        }
      }
      if(this.queue.length<this.capacity&&!this.filling)void this.fill().catch(error=>this.failure(error));
      this.publish();this.frame=this.screen.request(this.tick);
    } catch(error){this.failure(error);}
  };
  private draw(picture:CachedPicture,at:Time) {
    if(this.wallDrawAt!==null){const now=this.screen.now(),gap=now-this.wallDrawAt;if(gap>this.maximumWallGap){this.maximumWallGap=gap;this.maximumWallGapAt=picture.frame.time_seconds;this.maximumWallGapBuffered=this.queue.length;}this.wallDrawAt=now;}
    const previous=this.last,step=this.cache.descriptor().frame_step_seconds;
    if(previous&&step>0) {
      const missed=Math.max(0,Math.round((picture.frame.time_seconds-previous.time_seconds)/step)-1);this.missed+=missed;
      if(missed&&this.skipped.length<64)this.skipped.push({from:previous.time_seconds,to:picture.frame.time_seconds});
    }
    if(this.previousOutput)this.maximumGap=Math.max(this.maximumGap,
      Number(compare(at,this.previousOutput))*1000/(at.timebase*this.previousOutput.timebase));
    this.previousOutput=at;
    try {this.painter.draw(picture.pixels);}finally{picture.pixels.close();}
    this.last=picture.frame;this.lastRuntime=picture.runtime;this.presented++;
  }
  snapshot():SurfaceReport {
    const sorted=[...this.lags].sort((a,b)=>a-b),percentile=(p:number)=>sorted[Math.max(0,Math.ceil(sorted.length*p)-1)]??null;
    const readyThrough=this.readyThrough();
    return {time:this.last?.time_seconds??0,frameTime:this.last?.plan.identity.time??this.session.presentation_time,readyThrough,generation:this.session.generation,runtime:JSON.stringify(this.lastRuntime),
      presentedFrames:this.presented,discardedFrames:this.discarded,statistics:JSON.stringify({observations:sorted.length,
        lagP95Ms:percentile(.95),lagP99Ms:percentile(.99),maxLagMs:sorted.at(-1)??null,missedCachedFrames:this.missed,
        maxFrameGapMs:this.maximumGap,maxWallFrameGapMs:this.maximumWallGap,maxWallFrameGapAtSeconds:this.maximumWallGapAt,maxWallFrameGapBufferedFrames:this.maximumWallGapBuffered,skippedIntervals:this.skipped,presentationThread:"worker",
        preparedFrames:this.queue.length,validatingFrames:this.validating,capacity:this.capacity,readyThroughSeconds:Number(readyThrough.ticks)/readyThrough.timebase,slowPreparations:this.slowPreparations,
        submittedDeviceFrames:this.clock.submittedFrames,projectedDeviceFrames:this.projectedFrames,pictureCreditFrames:String(this.credited),
        projectionAgeMs:this.projection?this.screen.now()-(this.projection.timeOrigin+this.projection.performanceTime-this.screen.timeOrigin):null,
        nextPreparation:this.next,firstQueued:this.queue[0]?.frame.plan.identity.time??null,lastQueued:this.queue.at(-1)?.frame.next_time??null,filling:!!this.filling})};
  }
  private readyThrough(){
    let time=this.last?.next_time??this.session.presentation_time;
    for(const picture of this.queue){if(!this.validated.has(picture))break;time=picture.frame.next_time;}
    return time;
  }
  private publish(force=false) {
    if(this.binding.directPictureCredit){
      const frames=pictureConsumptionLimit(this.readyThrough(),this.session);
      if(frames>this.credited){this.credited=frames;this.port.postMessage({kind:'picture-limit',token:this.binding.token,frames:String(frames)});}
    }
    const now=this.screen.now();if(!force&&now-this.reportAt<100)return;this.reportAt=now;this.updated(this.snapshot());
  }
  private failure(error:unknown) {
    if(this.closed)return;const failure=error instanceof Error?error:Error(String(error));
    void this.stop(true);this.failed(failure);
  }
  stop(clear=false):Promise<SurfaceReport> {
    if(this.stopping)return this.stopping;
    this.closed=true;this.abort.abort();if(this.frame!==null)this.screen.cancel(this.frame);this.frame=null;
    this.port.onmessage=null;this.port.close();
    this.binding.outputPort?.postMessage({kind:'retired',token:this.binding.token});this.binding.outputPort?.close();
    for(const picture of this.queue)picture.pixels.close();this.queue=[];
    if(clear)this.painter.clear();
    const report=this.snapshot();
    this.stopping=(async()=>{await this.filling?.catch(()=>{});return report;})();return this.stopping;
  }
}
