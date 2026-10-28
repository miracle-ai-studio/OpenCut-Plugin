import type {createEditorClient,ToolInput,ToolOutput} from "../../../lib/editor-sdk";
import {PreviewSegmentDecoder} from "./visual-preview-renderer";
import {readPreviewBytes} from "./visual-preview-bytes";
export type PictureCachePlan=ToolOutput<"opencut_visual_preview_cache_prepare">;
type Time=PictureCachePlan["duration"];
type Segment=PictureCachePlan["segments"][number];
type Session=ToolInput<"opencut_visual_preview_frame">["session"];
type Entry={segment:Segment;blob?:Blob;size:number};
export type CachedPicture={frame:{prepare_ahead:true;next_time:Time;plan:{identity:{time:Time}};time_seconds:number};pixels:ImageBitmap;runtime:Record<string,string>;ready?:Promise<void>};
const compare=(a:Time,b:Time)=>BigInt(a.ticks)*BigInt(b.timebase)-BigInt(b.ticks)*BigInt(a.timebase);
const windowKey=(s:Segment)=>`${s.start.ticks}/${s.start.timebase}:${s.end.ticks}/${s.end.timebase}`;
const decoderKey=(s:Segment)=>`${windowKey(s)}:${s.job_id}`;
const invalid=()=>Error("Invalid native preview cache admission");
function validate(plan:PictureCachePlan) {
 if(!plan.segments.length||plan.segments.length>300||plan.width<1||plan.width>960||plan.height<1||plan.height>540||
  !Number.isSafeInteger(plan.max_encoded_bytes)||plan.max_encoded_bytes<1||plan.max_encoded_bytes>64*1024*1024||
  !/^[a-f0-9]{64}$/.test(plan.cache_key)||compare(plan.frame_step,{ticks:"0",timebase:1})<=0n)throw invalid();
 let previous:Time|null=null;
 for(const segment of plan.segments){
  if(!segment.job_id||!segment.frames.length||compare(segment.start,segment.end)>=0n||compare(segment.frames.at(-1)!.time,plan.duration)>=0n||
   (previous&&compare(previous,segment.start)>0n))throw invalid();
  let at=segment.start;
  for(const frame of segment.frames){
   if(compare(frame.time,at)!==0n||compare(frame.next_time,frame.time)<=0n||compare(frame.next_time,segment.end)>0n)throw invalid();
   const delta=BigInt(frame.next_time.ticks)*BigInt(frame.time.timebase)-BigInt(frame.time.ticks)*BigInt(frame.next_time.timebase);
   if(delta*BigInt(plan.frame_step.timebase)!==BigInt(plan.frame_step.ticks)*BigInt(frame.next_time.timebase)*BigInt(frame.time.timebase))throw invalid();
   at=frame.next_time;
  }
  if(compare(at,segment.end)!==0n)throw invalid();previous=segment.end;
 }
}
function sameIdentity(a:PictureCachePlan,b:PictureCachePlan){
 return (["project_id","revision","sequence_id","cache_key","width","height","max_encoded_bytes","duration_seconds","frame_step_seconds"] as const)
  .every(key=>a[key]===b[key])&&compare(a.duration,b.duration)===0n&&compare(a.frame_step,b.frame_step)===0n;
}
function sameRaster(a:PictureCachePlan,b:PictureCachePlan){
 return a.project_id===b.project_id&&a.sequence_id===b.sequence_id&&a.width===b.width&&a.height===b.height&&
  a.max_encoded_bytes===b.max_encoded_bytes&&compare(a.frame_step,b.frame_step)===0n;
}
/** A consumer can retire without cancelling a shared render/load operation. */
const waitForPicture=(pending:Promise<void>,signal:AbortSignal)=>new Promise<void>((resolve,reject)=>{
 signal.throwIfAborted();const abort=()=>{signal.removeEventListener("abort",abort);reject(signal.reason);};
 signal.addEventListener("abort",abort,{once:true});
 pending.then(()=>{signal.removeEventListener("abort",abort);resolve();},error=>{signal.removeEventListener("abort",abort);reject(error);});
});
const sleep=(signal:AbortSignal)=>new Promise<void>((resolve,reject)=>{
 signal.throwIfAborted();const done=()=>{clearTimeout(timer);signal.removeEventListener("abort",abort);};
 const abort=()=>{done();reject(signal.reason);};const timer=setTimeout(()=>{done();resolve();},500);
 signal.addEventListener("abort",abort,{once:true});
});
/** Rust owns all time mappings. A partial manifest is extended in its existing
 * authorization scope; array reordering never changes Blob/decoder identity. */
export class PictureCache {
 private retirement:ReturnType<typeof setTimeout>|null=null;
 private wake(){if(this.retirement){clearTimeout(this.retirement);this.retirement=null;}}
 private plan:PictureCachePlan|null=null;
 private entries=new Map<string,Entry>();
 private decoder=new PreviewSegmentDecoder();
 private scope=new AbortController();
 private sdk:ReturnType<typeof createEditorClient>|null=null;
 private session:Session|null=null;
 private loading=new Map<Entry,Promise<void>>();
 private refreshing:Promise<void>|null=null;
 private refreshedAt=-Infinity;
 private focus:Time={ticks:"0",timebase:1};
 private playback=false;
 private backgroundFailure:unknown=null;
 private prefetchScope:AbortController|null=null;
 descriptor(){if(!this.plan)throw Error("Preview cache is not ready");return this.plan;}
 private segment(time:Time){return this.plan?.segments.find(s=>compare(time,s.start)>=0n&&compare(time,s.end)<0n);}
 hasFrame(time:Time){const segment=this.segment(time);return !this.scope.signal.aborted&&!this.backgroundFailure&&!!segment&&!!this.entries.get(windowKey(segment))?.blob;}
 state(){return this.backgroundFailure?"failed":this.hasFrame(this.focus)?"ready":"preparing";}
 follow(time:Time){if(!this.plan||this.backgroundFailure||this.scope.signal.aborted)return;this.focus=time;void this.prefetch(this.scope);}
 nextWindow(time:Time){const segment=this.segment(time);return segment&&this.plan&&compare(segment.end,this.plan.duration)<0n?segment.end:null;}
 /** Reuse a manifest only after the caller obtains a fresh authorized native
  * frame for this generation. Revision/raster changes still require preparation. */
 async rebind(session:Session,identity:ToolOutput<"opencut_visual_preview_frame">["plan"]["identity"],time:Time,signal:AbortSignal){
  signal.throwIfAborted();this.wake();
  const plan=this.plan;
  if(!plan||this.backgroundFailure||plan.project_id!==identity.projectId||plan.revision!==identity.revision||plan.sequence_id!==identity.sequenceId||plan.width!==identity.width||plan.height!==identity.height)return false;
  this.scope.abort();const scope=new AbortController();this.scope=scope;this.loading.clear();this.refreshing=null;
  const abort=()=>scope.abort(signal.reason);signal.addEventListener("abort",abort,{once:true});scope.signal.addEventListener("abort",()=>signal.removeEventListener("abort",abort),{once:true});
  signal.throwIfAborted();this.current(scope);this.session=session;this.focus=time;this.playback=false;void this.prefetch(scope);return true;
 }
 private current(scope:AbortController){scope.signal.throwIfAborted();if(scope!==this.scope)throw new DOMException("Preview cache replaced","AbortError");}
 async prepare(sdk:ReturnType<typeof createEditorClient>,session:Session,signal:AbortSignal,
   progress:(completed:number,total:number)=>void,priority?:Time,wait=true,generate=false) {
  this.wake();
  const previous=this.plan,retained=this.entries,scope=new AbortController();
  this.scope.abort();this.scope=scope;this.plan=null;this.entries=new Map();this.loading.clear();this.refreshing=null;this.refreshedAt=-Infinity;this.backgroundFailure=null;
  const abort=()=>scope.abort(signal.reason);signal.addEventListener("abort",abort,{once:true});
  scope.signal.addEventListener("abort",()=>signal.removeEventListener("abort",abort),{once:true});
  try {
   signal.throwIfAborted();this.current(scope);
   const plan=await sdk.call("opencut_visual_preview_cache_prepare",{session,reuse_only:!wait&&!generate},{signal:scope.signal});this.current(scope);validate(plan);
   if(!previous||!sameIdentity(previous,plan))await this.decoder.close();
   this.current(scope);
   this.plan=plan;this.sdk=sdk;this.session=session;this.playback=wait;
   for(const segment of plan.segments){
    // The newly authorized native manifest proves each receipt's content.
    const old=previous&&sameRaster(previous,plan)?retained.get(windowKey(segment)):undefined;
    this.entries.set(windowKey(segment),old&&JSON.stringify(old.segment)===JSON.stringify(segment)?old:{segment,size:0});
   }
   this.focus=priority??plan.segments[0].start;const segment=this.segment(this.focus);if(!segment)throw invalid();
   const index=plan.segments.indexOf(segment),startup=wait?plan.segments.slice(index,index+3):[segment];
   const report=()=>progress(startup.filter(s=>!!this.entries.get(windowKey(s))?.blob).length,startup.length);
   report();
   // Prime encoded lookahead and both cut decoders before granting the clock
   // credit. Cold publication waits remain explicit preparation in the UI.
   for(const next of startup){await this.load(next,wait);this.current(scope);report();}
   if(wait){
    for(const next of startup.slice(0,2)){
     const blob=this.entries.get(windowKey(next))?.blob;
     if(blob)await this.decoder.warm(decoderKey(next),blob,plan.width,plan.height);
     this.current(scope);
    }
   }
   void this.prefetch(scope);return plan;
  }catch(error){if(scope===this.scope){scope.abort();this.plan=null;this.backgroundFailure=error;}throw error;}
 }
 private refresh(scope:AbortController) {
  if(this.refreshing)return this.refreshing;
  const plan=this.plan,sdk=this.sdk,session=this.session;
  if(!plan||!sdk||!session)return Promise.reject(Error("Preview cache is not ready"));
  this.refreshedAt=performance.now();
  const task=(async()=>{
   const addition=await sdk.call("opencut_visual_preview_cache_prepare",{session,reuse_only:!this.playback},{signal:scope.signal});
   this.current(scope);validate(addition);if(!sameIdentity(plan,addition))throw invalid();
   const merged=new Map(plan.segments.map(s=>[windowKey(s),s]));
   // Validate the entire replacement before publishing any of its entries.
   for(const segment of addition.segments){
    const old=merged.get(windowKey(segment));
    if(old?.job_id===segment.job_id&&JSON.stringify(old)!==JSON.stringify(segment))throw invalid();
    merged.set(windowKey(segment),segment);
   }
   const segments=[...merged.values()].sort((a,b)=>compare(a.start,b.start)<0n?-1:compare(a.start,b.start)>0n?1:0);
   validate({...plan,segments});
   for(const segment of addition.segments){
    const key=windowKey(segment),old=this.entries.get(key);
    if(!old||JSON.stringify(old.segment)!==JSON.stringify(segment))this.entries.set(key,{segment,size:0});
   }
   plan.segments=segments;
  })();
  this.refreshing=task;void task.finally(()=>{if(this.refreshing===task)this.refreshing=null;}).catch(()=>{});return task;
 }
 private reserve(scope:AbortController) {
  const plan=this.plan;if(!plan||!this.playback||this.refreshing||performance.now()-this.refreshedAt<500)return;
  const index=plan.segments.findIndex(s=>compare(this.focus,s.start)>=0n&&compare(this.focus,s.end)<0n);
  // Decoder lookahead can cross a boundary before the actual audio consumer.
  // Retry at most twice per second until Native admits its next reserve.
  if(index>=0&&plan.segments.length-index<3&&compare(plan.segments.at(-1)!.end,plan.duration)<0n)
   void this.refresh(scope).catch(error=>{
    // A rejected authorization/identity must retire even already cached bytes.
    // Preserve the actual error for the next frame and stop shared loads.
    if(scope===this.scope&&!scope.signal.aborted)scope.abort(error);
   });
 }
 private async ensureSegment(time:Time,signal:AbortSignal) {
  signal.throwIfAborted();const scope=this.scope,plan=this.plan;if(!plan)throw Error("Preview cache is not ready");
  if(compare(time,{ticks:"0",timebase:1})<0n||compare(time,plan.duration)>=0n)throw Error("Preview time is outside its timeline");
  let segment=this.segment(time);
  // At most two authorized refreshes, never an unbounded loop around a stale
  // consumer position. Native seek must confirm a new generation first.
  for(let attempt=0;!segment&&attempt<2;attempt++){
   await waitForPicture(this.refresh(scope),signal);this.current(scope);segment=this.segment(time);
  }
  if(!segment)throw Object.assign(Error("Preview position changed. Retry playback."),{code:"PREVIEW_CACHE_POSITION"});
  return segment;
 }
 private async prefetch(scope:AbortController) {
  const plan=this.plan;if(!plan||this.prefetchScope===scope)return;
  this.prefetchScope=scope;const visited=new Set<Entry>();
  try{for(;;){
   if(scope!==this.scope||scope.signal.aborted)return;
   const segment=plan.segments.filter(s=>{const entry=this.entries.get(windowKey(s));return entry&&!visited.has(entry)&&(!this.playback||compare(s.end,this.focus)>0n)&&this.distance(s)<=4;})
    .sort((a,b)=>this.distance(a)-this.distance(b))[0];
   if(!segment)return;visited.add(this.entries.get(windowKey(segment))!);
   try{await this.load(segment,true);}catch(error){if(scope.signal.aborted)return;this.backgroundFailure=error;return;}
  }}finally{if(this.prefetchScope===scope)this.prefetchScope=null;}
 }
 private distance(segment:Segment){
  const segments=this.plan?.segments??[],focus=segments.findIndex(s=>compare(this.focus,s.start)>=0n&&compare(this.focus,s.end)<0n),index=segments.findIndex(s=>windowKey(s)===windowKey(segment));
  return index>=focus?(index-focus)*2:(focus-index)*2+1;
 }
 private load(segment:Segment,wait:boolean):Promise<void> {
  const entry=this.entries.get(windowKey(segment));if(!entry||JSON.stringify(entry.segment)!==JSON.stringify(segment))return Promise.reject(new DOMException("Preview window replaced","AbortError"));
  if(entry.blob)return Promise.resolve();const pending=this.loading.get(entry);if(pending)return pending;
  const plan=this.plan,sdk=this.sdk,scope=this.scope;if(!plan||!sdk)return Promise.reject(Error("Preview cache is not ready"));
  const task=(async()=>{
   const signal=scope.signal;
   for(;;){
    this.current(scope);const job=await sdk.call("opencut_job_view",{job_id:segment.job_id},{signal});this.current(scope);
    if(job.state==="failed"||job.state==="cancelled"||job.result_unavailable)throw Object.assign(Error("Preview optimization could not finish. Retry playback."),{code:"PREVIEW_CACHE_FAILED"});
    if(job.state!=="succeeded"){if(!wait)return;await sleep(signal);continue;}
    const artifact=job.artifacts[0];if(job.artifacts.length!==1||artifact.mime_type!=="video/mp4")throw Error("Invalid preview segment artifact");
    const size=Number(artifact.size_bytes);if(!Number.isSafeInteger(size)||size<1||size>plan.max_encoded_bytes)throw Object.assign(Error("Preview cache exceeds its memory limit."),{code:"PREVIEW_CACHE_LIMIT"});
    const response=await sdk.downloadArtifact(artifact,{signal}),bytes=await readPreviewBytes(response,String(size),signal);
    const hash=[...new Uint8Array(await crypto.subtle.digest("SHA-256",bytes as Uint8Array<ArrayBuffer>))].map(v=>v.toString(16).padStart(2,"0")).join("");
    this.current(scope);if(this.entries.get(windowKey(segment))!==entry)throw new DOMException("Preview window replaced","AbortError");
    if(hash!==artifact.content_hash)throw Error("Preview cache checksum differs from its authorized artifact");
    let retained=[...this.entries.values()].reduce((n,e)=>n+e.size,0);
    const index=plan.segments.findIndex(s=>windowKey(s)===windowKey(this.segment(this.focus)??segment));
    const essential=new Set(plan.segments.slice(index,index+2).map(windowKey));
    const evict=[...this.entries.values()].filter(e=>e!==entry&&!essential.has(windowKey(e.segment))).sort((a,b)=>this.distance(b.segment)-this.distance(a.segment));
    for(const old of evict){if(retained+size<=plan.max_encoded_bytes)break;retained-=old.size;old.blob=undefined;old.size=0;}
    if(retained+size>plan.max_encoded_bytes)throw Object.assign(Error("Preview cache exceeds its memory limit."),{code:"PREVIEW_CACHE_LIMIT"});
    entry.blob=new Blob([bytes as Uint8Array<ArrayBuffer>],{type:"video/mp4"});entry.size=size;
    return;
   }
  })();
  this.loading.set(entry,task);void task.finally(()=>{if(this.loading.get(entry)===task)this.loading.delete(entry);}).catch(()=>{});return task;
 }
 async frame(time:Time,signal:AbortSignal):Promise<CachedPicture> {
  signal.throwIfAborted();const plan=this.plan,scope=this.scope;if(!plan)throw Error("Preview cache is not ready");
  const segment=await this.ensureSegment(time,signal);this.current(scope);signal.throwIfAborted();
  const frame=segment.frames.find(f=>compare(time,f.time)>=0n&&compare(time,f.next_time)<0n);if(!frame)throw Error("Missing native cached-frame mapping");
  const key=windowKey(segment),entry=this.entries.get(key);
  if(!entry||JSON.stringify(entry.segment)!==JSON.stringify(segment))throw new DOMException("Preview window replaced","AbortError");
  const checkEntry=()=>{this.current(scope);signal.throwIfAborted();if(this.entries.get(key)!==entry)throw new DOMException("Preview window replaced","AbortError");};
  this.focus=time;this.reserve(scope);void this.prefetch(scope);const began=performance.now();await waitForPicture(this.load(segment,true),signal);checkEntry();
  const following=plan.segments.find(s=>compare(s.start,segment.end)===0n),next=following&&this.entries.get(windowKey(following));
  await this.decoder.retain(new Set([decoderKey(segment),...(following?[decoderKey(following)]:[])]));
  checkEntry();const pixels=await this.decoder.frame(decoderKey(segment),entry.blob!,plan.width,plan.height,frame.segment_time_seconds,signal);
  try{
   checkEntry();
   if(following&&next?.blob)await this.decoder.warm(decoderKey(following),next.blob,plan.width,plan.height);
   checkEntry();
  }catch(error){pixels.close();throw error;}
  return {pixels,frame:{prepare_ahead:true,next_time:frame.next_time,plan:{identity:{time:frame.time}},time_seconds:frame.time_seconds},
   runtime:{cacheKey:plan.cache_key,cacheSegment:decoderKey(segment),rasterWidth:String(plan.width),rasterHeight:String(plan.height),totalMs:String(performance.now()-began)}};
 }
 async park(){this.scope.abort();this.prefetchScope?.abort();this.wake();this.retirement=setTimeout(()=>{this.retirement=null;void this.decoder.close();},30000);}
 async close(){this.wake();this.scope.abort();this.plan=null;this.entries.clear();await this.decoder.close();}
 async suspend(stopLoading=true){this.wake();if(stopLoading)this.scope.abort();await this.decoder.close();}
}
