/** Two readers of the same physical output clock. These are device facts, not
 * project time or a second playback clock. Absolute monotonic timestamps let a
 * worker sample the same affine mapping between main-thread observations. */
export type DeviceTimestamp = {contextTime:number;performanceTime:number;timeOrigin:number};
export type PresentationClockBinding={token:string;sampleRate:number;graphLatencySeconds:number;directPictureCredit?:boolean;outputPort?:MessagePort};
export type PresentationClockSource={bind(port:MessagePort):PresentationClockBinding};

/** A short presentation grace avoids inserting silence for every late picture.
 * This is a device consumption bound, not proof that future pixels exist.
 * Keep the final sample fenced until the native end picture is complete. */
export function pictureConsumptionLimit(
  through: import("../types").AudioPreviewSession["presentation_time"],
  session: import("../types").AudioPreviewSession,
): bigint {
  const rate=BigInt(session.identity.sample_rate),start=BigInt(session.actual_start_sample);
  const total=BigInt(session.total_frames)-start;
  const ready=BigInt(through.ticks)*rate/BigInt(through.timebase)-start;
  if(ready<=0n||total<=0n)return 0n;
  if(ready>=total)return total;
  const limit=ready+rate/10n;
  return limit<total?limit:total-1n;
}
