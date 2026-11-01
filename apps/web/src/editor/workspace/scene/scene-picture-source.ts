import type {AudioPreviewSession} from "./types";
import type {CachedPicture,PictureCache} from "./picture-cache";
import type {PreparedSurfacePicture,SurfacePictureSource} from "./surface-player";
import type {VisualPreviewRenderer,VisualPreviewExecutionApi} from "./visual-preview-renderer";
import type {EditorApi} from "../api";
import {surfacePictureMatches} from "./surface-worker-protocol";

type Time=AudioPreviewSession["presentation_time"];
export type ScenePictureExecutionApi=VisualPreviewExecutionApi&{sdk:Pick<EditorApi["sdk"],"call"|"waitForJob">};
const compare=(a:Time,b:Time)=>BigInt(a.ticks)*BigInt(b.timebase)-BigInt(b.ticks)*BigInt(a.timebase);
/** Execution adapter only: every time/grid, layer and binding comes from Rust. */
export class ScenePictureSource implements SurfacePictureSource {
  private first:CachedPicture|null=null;
  private plan:ReturnType<SurfacePictureSource["descriptor"]>|null=null;
  private pictureCapacity=64*1024*1024;
  private hits=0;
  private usingCache=false;
  constructor(private renderer:Pick<VisualPreviewRenderer,"prepare">&Partial<Pick<VisualPreviewRenderer,"surfacePresenter">>,private api:ScenePictureExecutionApi,private session:AudioPreviewSession,private cache?:PictureCache,_cuts:Time[]=[],private prepareCache?:()=>Promise<void>){
  }
  async initialize(signal:AbortSignal){
    signal.throwIfAborted();
    // Rust selects the whole scene's required sources and reuses the accepted
    // jobs. Do not compete with their decoders or switch to published sources
    // halfway through playback. Stopping this wait does not cancel shared jobs.
    const {project_id,revision,sequence_id}=this.session.identity;
    const preparation=await this.api.sdk.call("opencut_source_preview_prepare",{
      project_id,revision,sequence_id,asset_id:null,selected_video_stream:null,
    },{signal});
    for(const job of preparation.jobs){
      signal.throwIfAborted();
      if(job.state!=="succeeded")await this.api.sdk.waitForJob(job.job_id,{signal,timeoutMs:1800000,pollIntervalMs:500});
    }
    signal.throwIfAborted();
    const picture=await (await this.prepareNative(this.session.presentation_time,signal,0)).complete;
    try {
      const at=picture.frame.plan.identity.time,next=picture.frame.next_time;
      const delta=compare(next,at);
      if(delta<=0n)throw Error("Invalid native scene frame grid");
      this.pictureCapacity=Number(picture.runtime.maxRetainedBytes??64*1024*1024);
      this.plan={project_id:this.session.identity.project_id,revision:this.session.identity.revision,sequence_id:this.session.identity.sequence_id,
        width:picture.pixels.width,height:picture.pixels.height,
        duration:{ticks:this.session.total_frames,timebase:this.session.identity.sample_rate},
        frame_step_seconds:Number(delta)/(next.timebase*at.timebase)};
      if(this.cache){
        const {session_id,generation,overlay_epoch,owner_id,lease_id}=this.session;
        const identity=picture.frame.plan.identity as ToolFrameIdentity;
        if(!await this.cache.rebind({session_id,generation,overlay_epoch,owner_id,lease_id},identity,at,signal)){
          if(this.prepareCache)await this.prepareCache();else await this.cache.suspend();
        }
      }
      signal.throwIfAborted();this.first=picture;
    }catch(error){picture.pixels.close();throw error;}
  }
  get maxRetainedBytes(){return this.pictureCapacity;}
  get preparationFrames(){
    const plan=this.descriptor();
    // Keep four native/read/transfer slots inside the same advertised budget.
    // Two seconds cover measured long-GOP cold cuts; smaller owner budgets
    // still constrain this window without changing the preview resolution.
    return Math.max(1,Math.min(64,Math.floor(2/plan.frame_step_seconds),Math.floor(this.pictureCapacity/(plan.width*plan.height*4))-4));
  }
  createPainter(canvas:OffscreenCanvas){return this.renderer.surfacePresenter?.(canvas);}
  descriptor(){if(!this.plan)throw Error("Scene picture source is not initialized");return this.plan;}
  hasFrame(time:Time){return compare(time,{ticks:"0",timebase:1})>=0n&&compare(time,this.descriptor().duration)<0n;}
  async frame(time:Time,signal:AbortSignal,retainedBytes=0):Promise<CachedPicture>{
    return (await this.prepareFrame(time,signal,retainedBytes)).complete;
  }
  async prepareFrame(time:Time,signal:AbortSignal,retainedBytes:number):Promise<PreparedSurfacePicture>{
    signal.throwIfAborted();
    const completed=(picture:CachedPicture):PreparedSurfacePicture=>({frame:picture.frame,claimed:Promise.resolve(),complete:Promise.resolve(picture)});
    if(this.first){const first=this.first;this.first=null;if(compare(time,first.frame.plan.identity.time)<0n||compare(time,first.frame.next_time)>=0n){first.pixels.close();throw Error("Initial scene picture changed requested interval");}return completed(first);}
    this.cache?.follow(time);
    const cached=!!this.cache?.hasFrame(time);
    if(cached){
      const plan=this.cache!.descriptor();
      if(plan.project_id!==this.session.identity.project_id||plan.revision!==this.session.identity.revision||plan.sequence_id!==this.session.identity.sequence_id)throw Error("Derived picture changed its fixed session");
      const picture=await this.cache!.frame(time,signal);this.usingCache=true;this.hits++;return completed({...picture,runtime:{...picture.runtime,cacheHits:String(this.hits)}});
    }
    if(this.usingCache){await this.cache!.suspend(false);this.usingCache=false;signal.throwIfAborted();}
    return this.prepareNative(time,signal,retainedBytes);
  }
  private async prepareNative(time:Time,signal:AbortSignal,retainedBytes:number):Promise<PreparedSurfacePicture>{
    const {session_id,generation,overlay_epoch,owner_id,lease_id}=this.session;
    const abort=new AbortController();
    const pending=await this.renderer.prepare(this.api,{session_id,generation,overlay_epoch,owner_id,lease_id},AbortSignal.any([signal,abort.signal]),undefined,undefined,{time,reservedBytes:retainedBytes,end:{ticks:this.session.total_frames,timebase:this.session.identity.sample_rate},captureSurface:true});
    const frame=pending.frame;
    let surfaceFrame:CachedPicture['frame'];
    try {
      signal.throwIfAborted();
      const identity=frame.plan.identity;
      const catchUp=this.plan&&compare(identity.time,time)>0n&&frame.max_lag_ms===150
        ?{requestedTime:time,consumerSeconds:frame.consumer_time_seconds}:undefined;
      surfaceFrame={prepare_ahead:true,next_time:frame.next_time,plan:{identity},time_seconds:frame.time_seconds,...(catchUp?{catchUp}: {})};
      if(!frame.prepare_ahead||!surfacePictureMatches(surfaceFrame,time)||
        identity.projectId!==this.session.identity.project_id||identity.revision!==this.session.identity.revision||identity.sequenceId!==this.session.identity.sequence_id||
        (this.plan&&(frame.picture.width!==this.plan.width||frame.picture.height!==this.plan.height)))throw Error("Native scene picture changed its fixed session");
    }catch(error){abort.abort(error);void pending.complete.then(result=>result.pixels.close(),()=>{});throw error;}
    const complete=pending.complete.then(result=>{
      try {
        signal.throwIfAborted();
        if(this.plan&&(result.pixels.width!==this.plan.width||result.pixels.height!==this.plan.height))throw Error("Native scene picture changed its fixed session");
        return {...result,frame:surfaceFrame,runtime:{...result.runtime,cacheState:this.cache?.state()??"disabled",cacheHits:String(this.hits)}} as CachedPicture;
      }catch(error){result.pixels.close();throw error;}
    });
    void complete.catch(()=>{});
    return {frame:surfaceFrame,claimed:pending.claimed,complete};
  }
}
type ToolFrameIdentity=import("../../../lib/editor-sdk").ToolOutput<"opencut_visual_preview_frame">["plan"]["identity"];
