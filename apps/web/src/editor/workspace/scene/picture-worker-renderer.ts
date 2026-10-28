import { OpenCutApiError } from "../../../lib/editor-sdk";
import type { EditorApi } from "../api";
import type { VisualPreviewRenderer } from "./visual-preview-renderer";
import type { PictureCommand, PictureEvent, PictureResult, PictureRequest,PictureError } from "./picture-worker-protocol";
import type { PictureCachePlan, CachedPicture } from "./picture-cache";
import type {AudioPreviewSession} from "./types";
import type {DeviceTimestamp,PresentationClockSource} from "./audio-preview/presentation-clock";
import type {SurfaceReport} from "./surface-player";

type Args = Parameters<VisualPreviewRenderer["render"]>;
type CommandBody = PictureCommand extends infer C ? C extends PictureCommand ? Omit<C, "token"> : never : never;
type Result = PictureResult|CachedPicture|PictureCachePlan|SurfaceReport|void;
/** One serial executor in a dedicated thread. Rust still owns every plan/bind.
 * Static bitmaps cross to the UI; live cached pixels stay on the transferred surface. */
export class PictureWorkerRenderer {
  private worker: Worker | null = null;
  private api: EditorApi | null = null;
  private token = crypto.randomUUID();
  private sequence = 0;
  private closed = false;
  private surfaceRunId=0;
  private surfaceUpdated:((report:SurfaceReport,retired:boolean)=>void)|null=null;
  private surfaceFailure:((error:Error)=>void)|null=null;
  private pending: { id: number; resolve(result: Result): void; reject(error: unknown): void; cleanup(): void;progress?:(completed:number,total:number)=>void } | null = null;
  private send(body: CommandBody,transfer:Transferable[]=[]) {
    const command={...body,token:this.token};
    if(transfer.length)this.worker!.postMessage(command,transfer);else this.worker!.postMessage(command);
  }
  private error(e:PictureError) {
    const error=e.api?new OpenCutApiError(e.code!,e.message,e.api.details,e.api.status,e.api.requestId,e.api.warnings)
      :e.name==="AbortError"?new DOMException(e.message,"AbortError"):Object.assign(Error(e.message),e.code?{code:e.code}:{});
    if(e.metrics)Object.defineProperty(error,"previewMetrics",{value:e.metrics});return error;
  }
  private initialize(api:EditorApi) {
    if(this.closed||this.pending)throw Error("Picture worker is closed or busy.");
    if (!this.worker) {
      this.api = api;
      this.worker = new Worker(new URL("./picture-worker.ts", import.meta.url), { type: "module", name: "opencut-picture" });
      this.worker.onmessage = ({ data }: MessageEvent<PictureEvent>) => {
        if (data.kind === "closed") {
          if (data.token === this.token && this.closed) { this.worker?.terminate(); this.worker = null; }
          return;
        }
        if(data.token!==this.token){if(data.kind==="picture"||(data.kind==="cachedPicture"||data.kind==="stillPicture"))data.result.pixels.close();return;}
        if(data.kind==="surfaceReport") {
          if(data.runId===this.surfaceRunId&&(!this.closed||data.retired))this.surfaceUpdated?.(data.result,data.retired);
          return;
        }
        if(data.kind==="surfaceFailed") {
          if(data.runId===this.surfaceRunId&&!this.closed)this.surfaceFailure?.(this.error(data.error));return;
        }
        const pending = this.pending;
        if (data.token !== this.token || !pending || data.id !== pending.id) {
          if (data.kind === "picture"||(data.kind==="cachedPicture"||data.kind==="stillPicture")) data.result.pixels.close();
          return;
        }
        if(data.kind==="cacheProgress"){pending.progress?.(data.completed,data.total);return;}
        pending.cleanup(); this.pending = null;
        if(data.kind==="suspended")pending.resolve();
        else if (data.kind === "picture"||(data.kind==="cachedPicture"||data.kind==="stillPicture")||data.kind==="cachePrepared"||data.kind==="surfacePrepared") pending.resolve(data.result);
        else {
          pending.reject(this.error(data.error));
        }
      };
      this.worker.onerror = () => this.fail(Error("Picture preparation worker failed."));
      this.worker.postMessage({ token: this.token, kind: "init", connection: api.audioPreviewConnection } satisfies PictureCommand);
    }
    if (api !== this.api) throw Error("Picture worker connection changed.");
  }
  render(api: EditorApi, session: Args[1], signal: Args[2], hitQuery?: Args[3], draft?: Args[4], preparation?: Args[5]): Promise<PictureResult> {
    signal.throwIfAborted(); draft?.intentSignal?.throwIfAborted();
    try{this.initialize(api);}catch(error){return Promise.reject(error);}
    const id = ++this.sequence;
    return new Promise((resolve, reject) => {
      const abort = () => this.send({ kind: "cancel", id, intent: false });
      const intent = () => this.send({ kind: "cancel", id, intent: true });
      const timer = setTimeout(() => this.fail(Error("Picture preparation timed out.")), 15000);
      this.pending = { id, resolve:value=>resolve(value as PictureResult), reject, cleanup: () => {
        clearTimeout(timer); signal.removeEventListener("abort", abort); draft?.intentSignal?.removeEventListener("abort", intent);
      } };
      signal.addEventListener("abort", abort, { once: true });
      draft?.intentSignal?.addEventListener("abort", intent, { once: true });
      const { intentSignal: _intent, ...capturedDraft } = draft ?? {};
      this.worker!.postMessage({ token: this.token, kind: "render", id, session, hitQuery,
        draft: draft ? capturedDraft as PictureRequest["draft"] : undefined,
        preparation } satisfies PictureCommand);
    });
  }
  prepareCache(api:EditorApi,session:Args[1],signal:AbortSignal,progress:(completed:number,total:number)=>void,time?:PictureCachePlan["duration"],wait=true):Promise<PictureCachePlan> {
    return this.cacheRequest(api,signal,{kind:"prepareCache",id:0,session,time,wait},progress) as Promise<PictureCachePlan>;
  }
  suspend():Promise<void>{
    if(!this.api||this.closed)return Promise.resolve();
    return this.cacheRequest(this.api,new AbortController().signal,{kind:"suspend",id:0}) as Promise<void>;
  }
  cachedFrame(api:EditorApi,time:PictureCachePlan["duration"],signal:AbortSignal):Promise<CachedPicture> {
    return this.cacheRequest(api,signal,{kind:"cachedFrame",id:0,time}) as Promise<CachedPicture>;
  }
  stillFrame(api:EditorApi,session:AudioPreviewSession,signal:AbortSignal):Promise<PictureResult|CachedPicture>{
    const {session_id,generation,overlay_epoch,owner_id,lease_id}=session;
    return this.cacheRequest(api,signal,{kind:"stillFrame",id:0,session:{session_id,generation,overlay_epoch,owner_id,lease_id},time:session.presentation_time}) as Promise<PictureResult|CachedPicture>;
  }
  startSurface(api:EditorApi,session:AudioPreviewSession,canvas:OffscreenCanvas|undefined,source:PresentationClockSource,signal:AbortSignal,
    updated:(report:SurfaceReport,retired:boolean)=>void,failed:(error:Error)=>void,warmTimes:PictureCachePlan["duration"][]=[],producer:"scene"|"cache"="cache"):Promise<SurfaceReport> {
    signal.throwIfAborted();
    try{this.initialize(api);}catch(error){return Promise.reject(error);}
    const channel=new MessageChannel();
    let binding:ReturnType<PresentationClockSource["bind"]>;
    try{binding=source.bind(channel.port1);}catch(error){channel.port1.close();channel.port2.close();return Promise.reject(error);}
    this.surfaceUpdated=updated;this.surfaceFailure=failed;
    return this.cacheRequest(api,signal,{kind:"startSurface",id:0,session,canvas,clockPort:channel.port2,binding,producer,warmTimes},undefined,
      [...(canvas?[canvas]:[]),channel.port2,...(binding.outputPort?[binding.outputPort]:[])],id=>{this.surfaceRunId=id;}) as Promise<SurfaceReport>;
  }
  startLiveSurface(...args:Parameters<PictureWorkerRenderer["startSurface"]>):Promise<SurfaceReport> {
    return this.startSurface(args[0],args[1],args[2],args[3],args[4],args[5],args[6],args[7],"scene");
  }
  surfaceTimestamp(timestamp:DeviceTimestamp) {
    if(this.worker&&!this.closed&&this.surfaceRunId)this.send({kind:"surfaceTimestamp",runId:this.surfaceRunId,timestamp});
  }
  stopSurface(clear=true) {
    if(this.worker&&!this.closed&&this.surfaceRunId)this.send({kind:"stopSurface",runId:this.surfaceRunId,clear});
  }
  private cacheRequest(api:EditorApi,signal:AbortSignal,command:Extract<CommandBody,{kind:"prepareCache"|"cachedFrame"|"startSurface"|"suspend"|"stillFrame"}>,progress?:(completed:number,total:number)=>void,
    transfer:Transferable[]=[],started?:(id:number)=>void):Promise<Result> {
    signal.throwIfAborted();try{this.initialize(api);}catch(error){return Promise.reject(error);}
    const id=++this.sequence;
    started?.(id);
    return new Promise((resolve,reject)=>{
      const abort=()=>this.send({kind:"cancel",id,intent:false});
      // A live surface may prepare the same cold native cache as prepareCache.
      // Keep cancellation and its backend fences, rather than treating that
      // preparation as a ten-second ordinary worker acknowledgement.
      const preparing=command.kind==="prepareCache"||command.kind==="startSurface";
      const timer=setTimeout(()=>this.fail(Error(`Preview ${command.kind} timed out.`)),preparing?1800000:10000);
      this.pending={id,resolve,reject,progress,cleanup:()=>{clearTimeout(timer);signal.removeEventListener("abort",abort);}};
      signal.addEventListener("abort",abort,{once:true});this.send({...command,id},transfer);
    });
  }
  private fail(error: Error) {
    const pending = this.pending; this.pending = null;
    const notify=!pending&&!this.closed&&this.surfaceRunId?this.surfaceFailure:null;
    pending?.cleanup(); pending?.reject(error);
    this.close();
    notify?.(error);
  }
  close() {
    if (this.closed) return;
    this.closed = true;
    const pending = this.pending; this.pending = null;
    pending?.cleanup(); pending?.reject(new DOMException("Picture preparation closed", "AbortError"));
    if (this.worker) {
      const worker = this.worker;
      worker.postMessage({ token: this.token, kind: "close" } satisfies PictureCommand);
      setTimeout(() => { worker.terminate(); if (this.worker === worker) this.worker = null; }, 1000);
    }
  }
}
