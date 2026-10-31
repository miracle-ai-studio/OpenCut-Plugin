import {expect,it} from 'vitest';
import {pictureConsumptionLimit} from './presentation-clock';
import type {AudioPreviewSession} from '../types';
it('bounds preview lead to100ms without granting initial, negative, or unvalidated final samples',()=>{
 for(const rate of [44100,48000]) {
  const session={identity:{sample_rate:rate},actual_start_sample:String(rate*7),total_frames:String(rate*8)} as AudioPreviewSession;
  const limit=(ticks:string,timebase=rate)=>pictureConsumptionLimit({ticks,timebase},session);
  expect(limit(String(rate*6))).toBe(0n);
  expect(limit(String(rate*7))).toBe(0n);
  expect(limit(String(rate*7+rate/10))).toBe(BigInt(rate/5));
  expect(limit(String(rate*8-rate/20))).toBe(BigInt(rate-1));
  expect(limit(String(rate*8))).toBe(BigInt(rate));
  expect(limit(String(rate*9))).toBe(BigInt(rate));
 }
});
