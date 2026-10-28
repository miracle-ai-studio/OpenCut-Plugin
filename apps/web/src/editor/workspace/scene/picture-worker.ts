import { createEditorClient, OpenCutApiError } from "../../../lib/editor-sdk";
import { VisualPreviewRenderer } from "./visual-preview-renderer";
import { PictureCache } from "./picture-cache";
import {ScenePictureSource} from "./scene-picture-source";
import {SurfaceWorkerHost,SurfaceWorkerRun} from "./surface-worker-host";
import type {SurfaceReport} from "./surface-player";
import type { PictureCommand, PictureEvent, PictureError } from "./picture-worker-protocol";

const scope = self as unknown as {
  onmessage: ((event: MessageEvent<PictureCommand>) => void) | null;
  postMessage(event: PictureEvent, transfer?: Transferable[]): void;
  close(): void;
};
let token: string | null = null, alive = true, lastId = 0;
let sdk: ReturnType<typeof createEditorClient> | null = null;
let renderer: VisualPreviewRenderer | null = null;
let active: { id: number; abort: AbortController; intent: AbortController } | null = null;
const cache=new PictureCache();
let canvas:OffscreenCanvas|null=null,surface:SurfaceWorkerRun|null=null,surfaceRunId=0;
let displayHost:SurfaceWorkerHost|null=null;
let surfaceDrain:Promise<unknown>=Promise.resolve();
const stopSurface=(clear:boolean)=>{
  const current=surface;surface=null;
  if(current) {
    const runId=surfaceRunId;
    surfaceDrain=current.stop(clear).then(result=>{scope.postMessage({token:token!,kind:"surfaceReport",runId,result,retired:true});});
  }
  return surfaceDrain;
};
const errorValue = (error: unknown): PictureError => {
  const value: PictureError = { name: error instanceof Error ? error.name : "Error",
    message: error instanceof Error ? error.message : "Picture preparation failed." };
  if (error instanceof Error && "code" in error) {value.code = String(error.code);value.message=`${value.code}: ${value.message}`;}
  if (error instanceof Error && "previewMetrics" in error && typeof error.previewMetrics === "object")
    value.metrics = error.previewMetrics as object;
  if (error instanceof OpenCutApiError) value.api = { details: error.details, status: error.status,
    requestId: error.requestId, warnings: error.warnings };
  return value;
};
scope.onmessage = ({ data }) => {
  if (!alive) return;
  if (!data.token || (token !== null && token !== data.token)) return;
  if (data.kind === "init") {
    if (token !== null) return;
    token = data.token; sdk = createEditorClient(data.connection);
    renderer = new VisualPreviewRenderer();
    return;
  }
  if (!token || !sdk || !renderer) return;
  if(data.kind==="surfaceTimestamp") {
    if(data.runId===surfaceRunId)surface?.timestamp(data.timestamp);return;
  }
  if(data.kind==="stopSurface") {
    if(data.runId===surfaceRunId)void stopSurface(data.clear);return;
  }
  if (data.kind === "close") {
    alive = false; active?.abort.abort();
    void stopSurface(true).finally(async()=>{displayHost?.close();renderer?.close();await cache.close().catch(()=>{});
      scope.postMessage({token:token!,kind:"closed"});scope.close();});return;
  }
  if (data.kind === "cancel") {
    if (active?.id === data.id) (data.intent ? active.intent : active.abort).abort();
    return;
  }
  if (active || !Number.isSafeInteger(data.id) || data.id <= lastId) {
    scope.postMessage({ token, kind: "error", id: data.id,
      error: { name: "Error", message: "Picture worker request exceeds its serial budget." } });
    return;
  }
  lastId = data.id;
  const task = { id: data.id, abort: new AbortController(), intent: new AbortController() };
  active = task;
  const client = sdk;
  const api = { sdk: client, scene: {
    resource: (project_id: string, revision: string, resource_id: string, signal?: AbortSignal) =>
      client.call("opencut_project_resource_inspect", { project_id, revision, resource_id, kind: "asset" }, { signal }),
  } };
  if(data.kind==="stillFrame") {
    void (async()=>{
      await stopSurface(false);
      await cache.suspend();
      task.abort.signal.throwIfAborted();
      return renderer!.render(api,data.session,task.abort.signal);
    })().then(result=>{
      if(!alive||task.abort.signal.aborted){result.pixels.close();throw new DOMException("Still preview replaced","AbortError");}
      scope.postMessage({token:token!,kind:"stillPicture",id:data.id,result},[result.pixels]);
    }).catch(error=>{if(alive)scope.postMessage({token:token!,kind:"error",id:data.id,error:errorValue(error)});})
      .finally(()=>{if(active===task)active=null;});return;
  }
  if(data.kind==="suspend") {
    void stopSurface(false).then(()=>cache.park()).then(()=>{
      renderer!.close();renderer=new VisualPreviewRenderer();
      if(alive)scope.postMessage({token:token!,kind:"suspended",id:data.id});
    }).catch(error=>{if(alive)scope.postMessage({token:token!,kind:"error",id:data.id,error:errorValue(error)});})
      .finally(()=>{if(active===task)active=null;});return;
  }
  if(data.kind==="startSurface") {
    void (async()=>{
      await stopSurface(false);task.abort.signal.throwIfAborted();
      const source=new ScenePictureSource(renderer!,api,data.session);
      await source.initialize(task.abort.signal);
      if(data.canvas){if(canvas)throw Error("Presentation canvas already belongs to this worker");canvas=data.canvas;}
      if(!canvas)throw Object.assign(Error("Worker canvas presentation is unavailable"),{code:"CAPABILITY_UNAVAILABLE"});
      surfaceRunId=data.id;
      displayHost??=new SurfaceWorkerHost();
      const current=new SurfaceWorkerRun(displayHost,source,canvas,data.session,data.binding,data.clockPort,
        picture=>source instanceof ScenePictureSource&&!picture.runtime.cacheSegment?renderer!.surfaceBitmap(picture.pixels):picture.pixels,
        (result:SurfaceReport)=>{if(alive&&surface===current)scope.postMessage({token:token!,kind:"surfaceReport",runId:data.id,result,retired:false});},
        error=>{if(alive&&surface===current)scope.postMessage({token:token!,kind:"surfaceFailed",runId:data.id,error:errorValue(error)});});
      surface=current;
      const result=await current.start(task.abort.signal);task.abort.signal.throwIfAborted();
      if(alive)scope.postMessage({token:token!,kind:"surfacePrepared",id:data.id,result});
    })().catch(error=>{data.clockPort.close();if(alive)scope.postMessage({token:token!,kind:"error",id:data.id,error:errorValue(error)});})
      .finally(()=>{if(active===task)active=null;});return;
  }
  if(data.kind==="prepareCache"||data.kind==="cachedFrame") {
    const action=data.kind==="prepareCache"
      ? (async()=>{await stopSurface(false);renderer!.close();renderer=new VisualPreviewRenderer();
          const result=await cache.prepare(client,data.session,task.abort.signal,
          (completed,total)=>scope.postMessage({token:token!,kind:"cacheProgress",id:data.id,completed,total}),data.time,data.wait);
          task.abort.signal.throwIfAborted();return result;})()
      : surface?Promise.reject(Error("Cached presentation already owns its decoder")):cache.frame(data.time,task.abort.signal);
    void action.then(result=>{
      if(!alive){if("pixels" in result)result.pixels.close();return;}
      if(task.abort.signal.aborted){if("pixels" in result)result.pixels.close();throw new DOMException("Preview cache preparation canceled","AbortError");}
      if("pixels" in result)scope.postMessage({token:token!,kind:"cachedPicture",id:data.id,result},[result.pixels]);
      else scope.postMessage({token:token!,kind:"cachePrepared",id:data.id,result});
    }).catch(error=>{if(alive)scope.postMessage({token:token!,kind:"error",id:data.id,error:errorValue(error)});})
      .finally(()=>{if(active===task)active=null;});
    return;
  }
  // Same generated SDK and authorized resource port as the foreground adapter.
  const selectedRenderer=renderer;
  void stopSurface(false).then(()=>cache.suspend(false)).then(()=>selectedRenderer.render(api, data.session, task.abort.signal, data.hitQuery,
    data.draft ? { ...data.draft, intentSignal: task.intent.signal } : undefined, data.preparation))
    .then(result => {
      if (!alive) { result.pixels.close(); return; }
      if (task.abort.signal.aborted) { result.pixels.close(); throw new DOMException("Picture preparation canceled", "AbortError"); }
      scope.postMessage({ token: token!, kind: "picture", id: data.id, result }, [result.pixels]);
    })
    .catch(error => { if (alive) scope.postMessage({ token: token!, kind: "error", id: data.id, error: errorValue(error) }); })
    .finally(() => { if (active === task) active = null; });
};
