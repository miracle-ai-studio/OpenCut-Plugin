import type {AudioPreviewSession} from "./types";
import type {CachedPicture,PictureCache} from "./picture-cache";
import type {SurfacePictureSource} from "./surface-player";
import type {VisualPreviewRenderer,VisualPreviewExecutionApi} from "./visual-preview-renderer";

type Time=AudioPreviewSession["presentation_time"];
const compare=(a:Time,b:Time)=>BigInt(a.ticks)*BigInt(b.timebase)-BigInt(b.ticks)*BigInt(a.timebase);
/** Execution adapter only: every time/grid, layer and binding comes from Rust. */
export class ScenePictureSource implements SurfacePictureSource {
  private first:CachedPicture|null=null;
  private plan:ReturnType<SurfacePictureSource["descriptor"]>|null=null;
  private pictureCapacity=64*1024*1024;
  private hits=0;
  private usingCache=false;
  constructor(private renderer:Pick<VisualPreviewRenderer,"render">&Partial<Pick<VisualPreviewRenderer,"surfacePresenter">>,private api:VisualPreviewExecutionApi,private session:AudioPreviewSession,private cache?:PictureCache,_cuts:Time[]=[],private prepareCache?:()=>Promise<void>){
  }
  async initialize(signal:AbortSignal){
    const picture=await this.render(this.session.presentation_time,signal,0);
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
  get preparationFrames(){const plan=this.descriptor();return Math.max(1,Math.min(24,Math.floor(.96/plan.frame_step_seconds),Math.floor(this.pictureCapacity/(plan.width*plan.height*4))-4));}
  createPainter(canvas:OffscreenCanvas){return this.renderer.surfacePresenter?.(canvas);}
  descriptor(){if(!this.plan)throw Error("Scene picture source is not initialized");return this.plan;}
  hasFrame(time:Time){return compare(time,{ticks:"0",timebase:1})>=0n&&compare(time,this.descriptor().duration)<0n;}
  async frame(time:Time,signal:AbortSignal,retainedBytes=0):Promise<CachedPicture>{
    signal.throwIfAborted();
    if(this.first){const first=this.first;this.first=null;if(compare(time,first.frame.plan.identity.time)<0n||compare(time,first.frame.next_time)>=0n){first.pixels.close();throw Error("Initial scene picture changed requested interval");}return first;}
    this.cache?.follow(time);
    const cached=!!this.cache?.hasFrame(time);
    if(cached){
      const plan=this.cache!.descriptor();
      if(plan.project_id!==this.session.identity.project_id||plan.revision!==this.session.identity.revision||plan.sequence_id!==this.session.identity.sequence_id)throw Error("Derived picture changed its fixed session");
      const picture=await this.cache!.frame(time,signal);this.usingCache=true;this.hits++;return {...picture,runtime:{...picture.runtime,cacheHits:String(this.hits)}};
    }
    if(this.usingCache){await this.cache!.suspend(false);this.usingCache=false;signal.throwIfAborted();}
    return this.render(time,signal,retainedBytes);
  }
  private async render(time:Time,signal:AbortSignal,retainedBytes:number):Promise<CachedPicture>{
    const {session_id,generation,overlay_epoch,owner_id,lease_id}=this.session;
    const result=await this.renderer.render(this.api,{session_id,generation,overlay_epoch,owner_id,lease_id},signal,undefined,undefined,{time,reservedBytes:retainedBytes,end:{ticks:this.session.total_frames,timebase:this.session.identity.sample_rate},captureSurface:true});
    try {
      signal.throwIfAborted();
      const identity=result.frame.plan.identity;
      if(!result.frame.prepare_ahead||!result.frame.next_time||compare(result.frame.next_time,identity.time)<=0n||compare(time,identity.time)<0n||compare(time,result.frame.next_time)>=0n||
        identity.projectId!==this.session.identity.project_id||identity.revision!==this.session.identity.revision||identity.sequenceId!==this.session.identity.sequence_id||
        (this.plan&&(result.pixels.width!==this.plan.width||result.pixels.height!==this.plan.height)))throw Error("Native scene picture changed its fixed session");
      return {...result,runtime:{...result.runtime,cacheState:this.cache?.state()??"disabled",cacheHits:String(this.hits)}} as CachedPicture;
    }catch(error){result.pixels.close();throw error;}
  }
}
type ToolFrameIdentity=import("../../../lib/editor-sdk").ToolOutput<"opencut_visual_preview_frame">["plan"]["identity"];
