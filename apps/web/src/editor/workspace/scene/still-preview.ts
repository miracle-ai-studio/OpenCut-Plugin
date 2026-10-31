import type {EditorApi} from "../api";
import type {Time} from "../../model/types";
import type {SessionHost} from "./session";
import type {AudioPreviewCreate,AudioPreviewSession,AudioPreviewControl} from "./types";
import {compareTime} from "../../model/time";
import {PreviewRecovery} from "./audio-preview/recovery";

type Owner={api:EditorApi;request:AudioPreviewCreate;session:AudioPreviewSession|null;abort:AbortController;key:string;leaseDeadline:number;current():boolean;retiring?:Promise<void>};
export type PreparedPreviewOwner={request:AudioPreviewCreate;session:AudioPreviewSession;recovery:PreviewRecovery};
/** A paused native preview owner, without AudioContext, PCM or an output clock.
 * It uses the existing native create/control/recovery contracts. Pointer moves
 * select native-authorized frames; they never create project edits or Jobs. */
export class StillPreview {
 private owner:Owner|null=null;
 private recovery=new PreviewRecovery();
 private tail:Promise<unknown>=Promise.resolve();
 private heartbeat:ReturnType<typeof setInterval>|null=null;
 private heartbeatBusy=false;
 private reading=0;
 constructor(private host:SessionHost,private failed:(error:Error)=>void=()=>{}){}
 private command(owner:Owner,command:AudioPreviewControl["command"]):AudioPreviewControl {
  const s=owner.session!;return {session_id:s.session_id,generation:s.generation,owner_id:s.owner_id,lease_id:s.lease_id,control_id:crypto.randomUUID(),command};
 }
 private identity(){const state=this.host.state(),snapshot=state&&(state.historical??state.head);return state&&snapshot?JSON.stringify([snapshot.project.id,snapshot.revision,state.sequenceId,this.host.generation(),this.host.viewGeneration()]):null;}
 currentPaused(time:Time):AudioPreviewSession|null {
  const owner=this.owner,session=owner?.session;
  return owner?.current()&&!owner.abort.signal.aborted&&session?.state==="paused"&&compareTime(session.presentation_time,time)===0?session:null;
 }
 canTransfer(time:Time):boolean {
  return !this.reading&&!this.heartbeatBusy&&!!this.currentPaused(time)&&this.owner!.leaseDeadline>performance.now();
 }
 /** Move a fully observed paused owner, including any pending acknowledgement.
  * The recipient becomes solely responsible for controls and retirement. */
 takePaused(api:EditorApi,request:AudioPreviewCreate):PreparedPreviewOwner|null {
  if(!this.canTransfer(request.start_time))return null;
  const owner=this.owner!,session=owner.session!;
  if(owner.api!==api||session.identity.project_id!==request.project_id||session.identity.revision!==request.revision||
   session.identity.sequence_id!==request.sequence_id||session.identity.sample_rate!==request.sample_rate)return null;
  this.owner=null;
  if(this.heartbeat)clearInterval(this.heartbeat);
  this.heartbeat=null;
  return {request:owner.request,session,recovery:this.recovery};
 }
 private observe(owner:Owner,session:AudioPreviewSession){
  owner.session=session;
  owner.leaseDeadline=performance.now()+Math.min(30000,Number(session.lease_remaining_ms));
 }
 read(time:Time):Promise<AudioPreviewSession> {
  this.reading++;
  const task=this.tail.catch(()=>{}).then(async()=>{
   const api=this.host.api(),state=this.host.state(),snapshot=state&&(state.historical??state.head),key=this.identity();
   if(!api||!state||!snapshot||!key)throw new DOMException("Preview view retired","AbortError");
   if(this.owner&&(this.owner.api!==api||this.owner.key!==key))await this.retire(this.owner);
   let owner=this.owner;
   if(!owner){
    owner={api,key,request:{project_id:snapshot.project.id,revision:snapshot.revision,sequence_id:state.sequenceId,start_time:time,sample_rate:48000,idempotency_key:crypto.randomUUID()},session:null,abort:new AbortController(),leaseDeadline:0,
     current:()=>this.owner===owner&&this.host.api()===api&&this.identity()===key};
    this.owner=owner;
    try{
     owner.session=await this.recovery.create(api,owner.request,owner.abort.signal,owner.current);
     owner.abort.signal.throwIfAborted();
     this.observe(owner,await this.recovery.control(api,owner.request,this.command(owner,{kind:"pause"}),owner.abort.signal,owner.current));
     this.armHeartbeat(owner);
    }catch(error){await this.retire(owner).catch(()=>{});throw error;}
   }else if(compareTime(owner.session!.presentation_time,time)!==0){
    this.observe(owner,await this.recovery.control(api,owner.request,this.command(owner,{kind:"seek",sequence_time:time}),owner.abort.signal,owner.current));
   }
   owner.abort.signal.throwIfAborted();if(!owner.current())throw new DOMException("Preview owner replaced","AbortError");
   return owner.session!;
  }).finally(()=>{this.reading--;});
  this.tail=task.catch(()=>{});return task;
 }
 private armHeartbeat(owner:Owner){
  if(this.heartbeat)clearInterval(this.heartbeat);
  this.heartbeat=setInterval(()=>{
   if(this.heartbeatBusy||!owner.current())return;this.heartbeatBusy=true;
   const task=this.tail.catch(()=>{}).then(async()=>{
    if(!owner.current())return;
    this.observe(owner,await owner.api.scene.audioPreviewControl(this.command(owner,{kind:"heartbeat"}),AbortSignal.any([owner.abort.signal,AbortSignal.timeout(3000)])));
   });
   this.tail=task.catch(()=>{});void task.catch(error=>{if(owner.current())this.failed(error instanceof Error?error:Error(String(error)));return this.retire(owner).catch(()=>{});}).finally(()=>{this.heartbeatBusy=false;});
  },5000);
 }
 private retire(owner:Owner,keepalive=false):Promise<void>{
  if(owner.retiring)return owner.retiring;
  if(this.owner===owner){this.owner=null;if(this.heartbeat)clearInterval(this.heartbeat);this.heartbeat=null;}
  owner.abort.abort();
  owner.retiring=owner.session?this.recovery.stop(owner.api,owner.request,this.command(owner,{kind:"stop"}),keepalive):this.recovery.retire(owner.api,owner.request,keepalive);
  return owner.retiring;
 }
 close(keepalive=false):Promise<void>{
  const owner=this.owner;
  // Mark the original operation retired synchronously, before a pending native
  // response can adopt its owner; drain it before a sound session is created.
  if(owner){owner.abort.abort();void this.recovery.retire(owner.api,owner.request,keepalive).catch(()=>{});}
  const task=this.tail.catch(()=>{}).then(()=>owner?this.retire(owner,keepalive):undefined);
  this.tail=task.catch(()=>{});return task;
 }
}
