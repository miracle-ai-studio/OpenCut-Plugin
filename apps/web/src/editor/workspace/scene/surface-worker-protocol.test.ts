import {expect,it} from 'vitest';
import {surfacePictureMatches,surfaceByteCapacity} from './surface-worker-protocol';
import type {CachedPicture} from './picture-cache';
const requested={ticks:'10',timebase:25};
it('preserves admitted byte limits and rejects invalid or oversized worker credits',()=>{
 expect(surfaceByteCapacity({})).toBe(64*1024*1024);
 for(const capacity of [65536,32*1024*1024,64*1024*1024,128*1024*1024])expect(surfaceByteCapacity({maxRetainedBytes:capacity})).toBe(capacity);
 for(const capacity of [0,-1,65535,65536.5,NaN,Infinity,128*1024*1024+1])expect(()=>surfaceByteCapacity({maxRetainedBytes:capacity})).toThrow('byte capacity');
});
const picture=(at:number,consumerSeconds=at/25):CachedPicture['frame']=>({prepare_ahead:true,plan:{identity:{time:{ticks:String(at),timebase:25}}},next_time:{ticks:String(at+1),timebase:25},time_seconds:at/25,catchUp:{requestedTime:requested,consumerSeconds}});
it('accepts only a native catch-up bound to the original request and confirmed consumer',()=>{
 expect(surfacePictureMatches(picture(12,.50),requested)).toBe(true);
 const normal=picture(10);delete normal.catchUp;expect(surfacePictureMatches(normal,requested)).toBe(true);
 for(const bad of [picture(9),picture(14),picture(12,.47),picture(12,.64),picture(12,NaN),{...picture(12),catchUp:undefined},{...picture(12),catchUp:{requestedTime:{ticks:'11',timebase:25},consumerSeconds:.5}},{...picture(12),next_time:{ticks:'11',timebase:25}}])expect(surfacePictureMatches(bad,requested)).toBe(false);
});
