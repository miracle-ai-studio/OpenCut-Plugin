import type {AudioPreviewSession} from './types';
import type {SurfacePictureSource,SurfaceReport} from './surface-player';
import type {CachedPicture} from './picture-cache';
import type {DeviceTimestamp,PresentationClockBinding} from './audio-preview/presentation-clock';
export type SurfaceDescriptor=ReturnType<SurfacePictureSource['descriptor']>;
export const PRODUCER_AHEAD=6;
/** Carry the native owner's admitted byte envelope across both worker queues. */
export function surfaceByteCapacity(source:Pick<SurfacePictureSource,'maxRetainedBytes'>):number {
 const capacity=source.maxRetainedBytes??64*1024*1024;
 if(!Number.isSafeInteger(capacity)||capacity<65536||capacity>128*1024*1024)throw Error('Invalid native presentation byte capacity');
 return capacity;
}
/** Native may move an obsolete preparation request to its confirmed consumer.
 * Only that explicitly bound, short forward interval crosses the worker port. */
export function surfacePictureMatches(frame:CachedPicture['frame'],requested:AudioPreviewSession['presentation_time']):boolean {
 const at=frame.plan.identity.time,next=frame.next_time;
 const compare=(a:typeof at,b:typeof at)=>BigInt(a.ticks)*BigInt(b.timebase)-BigInt(b.ticks)*BigInt(a.timebase);
 if(!frame.prepare_ahead||compare(next,at)<=0n)return false;
 if(compare(at,requested)<=0n&&compare(requested,next)<0n)return frame.catchUp===undefined;
 const proof=frame.catchUp;
 if(!proof||compare(at,requested)<=0n||compare(proof.requestedTime,requested)!==0n||
   compare(at,requested)*1000n>150n*BigInt(at.timebase)*BigInt(requested.timebase))return false;
 const seconds=Number(at.ticks)/at.timebase;
 return Number.isFinite(proof.consumerSeconds)&&proof.consumerSeconds>=seconds&&proof.consumerSeconds-seconds<=.150000001;
}
export type SurfaceWorkerError={name:string;message:string;code?:string};
export const surfaceError=(error:unknown):SurfaceWorkerError=>({name:error instanceof Error?error.name:'Error',message:error instanceof Error?error.message:String(error),...(error instanceof Error&&'code' in error?{code:String(error.code)}:{})});
export type SurfaceCommand={token:string}&(
 |{kind:'start';canvas?:OffscreenCanvas;session:AudioPreviewSession;plan:SurfaceDescriptor;capacity:number;maxRetainedBytes:number;clockPort:MessagePort;binding:PresentationClockBinding}
 |{kind:'picture';id:number;picture:CachedPicture}|{kind:'pictureError';id:number;error:SurfaceWorkerError}
 |{kind:'timestamp';timestamp:DeviceTimestamp}|{kind:'stop';clear:boolean}|{kind:'close'});
export type SurfaceEvent={token:string}&(
 |{kind:'frame';id:number;time:AudioPreviewSession['presentation_time'];retainedBytes:number}
 |{kind:'report'|'started'|'stopped';report:SurfaceReport}
 |{kind:'failed';error:SurfaceWorkerError});
