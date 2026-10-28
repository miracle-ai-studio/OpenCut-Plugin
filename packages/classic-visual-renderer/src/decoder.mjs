import { isVerifiedRangeSource } from "@opencut/classic-audio-renderer/verified-range-source";
import * as mediaBunny from "mediabunny";
const {Input,ALL_FORMATS,CanvasSink,VideoSampleSink}=mediaBunny;
import { staticImageHeader, admitDimensions, decoderSurfaces } from "./image-format.mjs";
import { checkAbort, finite, reject } from "./validation.mjs";
function verifiedSource(value) {
  if (!isVerifiedRangeSource(value)) reject("INVALID_VISUAL_PLAN", "Video requires a verified bounded source");
  return value.source;
}
function canvasCopy(source,width,height) {
  const canvas = new OffscreenCanvas(width,height), context = canvas.getContext("2d");
  if (!context) reject("VISUAL_DECODE_FAILED","Decoded source canvas is unavailable");
  context.drawImage(source,0,0,width,height);return canvas;
}
// Direct video samples avoid a CPU RGBA canvas in colour-background scenes.
// Input owns compressed decoding; each returned VideoFrame has its own lifetime.
function directFrame(sample) {
  if(!sample)return null;
  try {return {timestamp:sample.timestamp,duration:sample.duration,width:sample.displayWidth,height:sample.displayHeight,rotationDegrees:sample.rotation,canvas:sample.toVideoFrame()};}
  finally {sample.close();}
}
class DirectVideoSink {
  constructor(track){this.sink=new VideoSampleSink(track);}
  async *canvases(start){for await(const sample of this.sink.samples(start))yield directFrame(sample);}
  async getCanvas(time){return directFrame(await this.sink.getSample(time));}
}
function closeSurface(surface){if(typeof surface?.close==='function')surface.close();else if(surface){surface.width=0;surface.height=0;}}
function eofWindow(decode) {
  const policy = decode.eofPolicy;
  if (policy?.kind === "noFrame") return null;
  if (policy?.kind !== "holdLastFrame") reject("INVALID_VISUAL_PLAN", "Missing fixed video EOF policy");
  const start = finite(policy.readWindowStartSeconds, "source window start"), end = finite(policy.readWindowEndSeconds, "source window end");
  if (start < 0 || end <= start) reject("INVALID_VISUAL_PLAN", "Invalid fixed video source window");
  return { start, end };
}
function videoObservation(value, admission) {
  const timestamp = finite(value.timestamp, "decoded PTS"), duration = finite(value.duration, "decoded frame duration");
  if (duration <= 0 || !Number.isFinite(timestamp + duration)) reject("VISUAL_DECODE_FAILED", "Decoded video interval is invalid");
  const width = value.width??value.canvas.width??value.canvas.displayWidth, height = value.height??value.canvas.height??value.canvas.displayHeight;
  admitDimensions(width, height, admission);
  return { canvas: value.canvas, width, height, videoRotation:value.rotationDegrees??0, actualTimestampSeconds: timestamp, frameDurationSeconds: duration };
}
function terminalObservation(value, requested, window, admission) {
  if (!value || !window) return null;
  const observed = videoObservation(value, admission), start = observed.actualTimestampSeconds, end = start + observed.frameDurationSeconds;
  // Preserve the decoder's actual interval. Only an observed terminal frame
  // intersecting the Rust window can be held, and never before that interval.
  if (start < 0 || requested < end || start >= window.end || end <= window.start) return null;
  return { ...observed, eofHeld: true };
}
export async function decodeImage(bytes,admission,signal) {
  const header=staticImageHeader(bytes);admitDimensions(header.width,header.height,admission);checkAbort(signal);
  // Dedicated workers have Canvas/ImageBitmap and the same verified byte path,
  // but no HTMLImageElement. Keep decode orientation/color defaults unchanged.
  if (typeof globalThis.Image !== "function") {
    const image=await createImageBitmap(new Blob([bytes],{type:header.mime}));
    try {checkAbort(signal);admitDimensions(image.width,image.height,admission);
      return {canvas:canvasCopy(image,image.width,image.height),width:image.width,height:image.height};
    } finally {image.close();}
  }
  const image=new Image(),url=URL.createObjectURL(new Blob([bytes],{type:header.mime}));
  try {
    await new Promise((resolve,fail)=>{image.onload=resolve;image.onerror=()=>fail(new Error("Static image decode failed"));image.src=url;});
    checkAbort(signal);admitDimensions(image.naturalWidth,image.naturalHeight,admission);
    return {canvas:canvasCopy(image,image.naturalWidth,image.naturalHeight),width:image.naturalWidth,height:image.naturalHeight};
  } catch(error) { if(error.code) throw error;reject("VISUAL_DECODE_FAILED","Static image could not be decoded"); }
  finally {image.onload=null;image.onerror=null;image.src="";URL.revokeObjectURL(url);}
}
export async function decodeVideo(bytes,decode,admission,signal) {
  checkAbort(signal);const requested=finite(decode.requestedTimeSeconds,"observed decode request");
  const window = eofWindow(decode);
  if (decode.selectedVideoStream !== null) reject("UNSUPPORTED_RENDER_FEATURE","Only primary video selection is supported");
  const input=new Input({source:verifiedSource(bytes),formats:ALL_FORMATS});let iterator,ownedFrame;
  try {
    const tracks=await input.getVideoTracks();checkAbort(signal);
    if(tracks.length!==1) reject("UNSUPPORTED_RENDER_FEATURE","Exactly one video stream is required");
    const track=await input.getPrimaryVideoTrack();
    if(!track) reject("VISUAL_DECODE_FAILED","Primary video track is unavailable");
    // The pinned decoder can silently continue with color-only output if its
    // separate-alpha merger cannot initialize. This profile has no independently
    // verified transparent-video path, so refuse before decoder allocation.
    if(await track.canBeTransparent()) reject("UNSUPPORTED_RENDER_FEATURE","Transparent video is not supported by this execution profile");
    if(!await track.canDecode()) reject("VISUAL_DECODE_FAILED","Primary video codec is unavailable");
    // Admit actual probed display dimensions before allocating a CanvasSink.
    admitDimensions(track.displayWidth,track.displayHeight,admission);
    const videoColorSpace=await track.getColorSpace?.()??{};
    const sink=decoderSurfaces(admission)===1?new DirectVideoSink(track):new CanvasSink(track,{poolSize:decoderSurfaces(admission)-1,fit:"contain"});iterator=sink.canvases(requested);
    const item=await iterator.next();if(decoderSurfaces(admission)===1)ownedFrame=item.value?.canvas;checkAbort(signal);
    let observed = item.done || !item.value ? null : videoObservation(item.value, admission);
    const covered = observed && observed.actualTimestampSeconds <= requested && requested < observed.actualTimestampSeconds + observed.frameDurationSeconds;
    if (!covered) {
      const reason = observed ? "timestamp_gap" : "eof";
      if (!window) return { reason };
      await iterator.return(); iterator = null; closeSurface(ownedFrame);ownedFrame=null;checkAbort(signal);
      // Pinned Mediabunny selects the last presentation frame at Infinity.
      // No epsilon timestamp, complete PTS table or whole-source decode.
      const terminal = await sink.getCanvas(Infinity); if(decoderSurfaces(admission)===1)ownedFrame=terminal?.canvas;checkAbort(signal);
      observed = terminalObservation(terminal, requested, window, admission);
      if (!observed) return { reason };
    }
    const canvas=decoderSurfaces(admission)<=2?observed.canvas:canvasCopy(observed.canvas, observed.width, observed.height);
    if(decoderSurfaces(admission)===1)ownedFrame=null;
    return { ...observed, videoColorSpace, canvas };
  } catch(error) {if(error.code)throw error;reject("VISUAL_DECODE_FAILED","Video decoder failed");}
  finally {try{await iterator?.return();}finally{closeSurface(ownedFrame);input.dispose();}}
}

// One serial consumer per verified resource. Rust supplies every requested
// source time; this cache only selects the decoder's observed covering interval.
// Backward requests restart the iterator, and inactive resources are disposed.
export class VisualDecoderCache {
  entries = new Map();
  sources = new Map();
  range(key) { return this.sources.get(key); }
  keys() { return new Set(this.entries.keys()); }
  async adopt(other, replace=false) {
    // Only completed speculative decoders move. Their verified ranges retain
    // the same session lifetime; no source time or observation is fabricated.
    for (const [key,entry] of other.entries) {
      if(replace&&this.entries.has(key))await this.retain(new Set([...this.entries.keys()].filter(existing=>existing!==key)));
      if(this.entries.has(key))continue;
      this.entries.set(key,entry);other.entries.delete(key);
      if(other.sources.has(key)){this.sources.set(key,other.sources.get(key));other.sources.delete(key);}
    }
  }
  keepRange(key, source) {
    if (!isVerifiedRangeSource(source) || this.sources.has(key)) reject("INVALID_VISUAL_PLAN", "Verified decoder source changed");
    this.sources.set(key, source);
  }
  retainedEncodedBytes() { return [...this.sources.values()].reduce((n, source) => n + source.residentBytes, 0); }

  retainedBytes(key) { return key === undefined ? [...this.entries.values()].reduce((n, e) => n + e.bytes, 0) : (this.entries.get(key)?.bytes ?? 0); }
  async retain(keys, cacheBytes = Infinity) {
    const closing = [];
    keys = new Set([...keys].filter(key => !this.sources.has(key) || this.sources.get(key).residentBytes <= Math.min(cacheBytes, this.sources.get(key).length) * 3 + (this.sources.get(key).transportBytes ?? 65536 * 6)));
    for (const [key, e] of this.entries) if (!keys.has(key)) { await this.dispose(e); this.entries.delete(key); }
    for (const [key, source] of this.sources) if (!keys.has(key)) {
      this.sources.delete(key); closing.push(source.close());
    }
    await Promise.allSettled(closing);
  }
  dispose(e) {
    return e.closing ??= (async () => {
    // The iterator may still have a pending demux read. End it before disposing
    // Input, whose source disposal revokes the verified range capability.
    try { await e.iterator?.return(); } catch { /* Retiring an aborted iterator. */ }
    e.input.dispose();
    const owned=new Set(e.canvases??[]);
    if(e.surfaces===1){owned.add(e.current?.canvas);owned.add(e.terminal?.canvas);}
    for(const canvas of owned)if(canvas)closeSurface(canvas);
    e.current = null;
    })();
  }
  close() { return this.retain(new Set()); }
  async prepare(key,bytes,admission,signal) {
    checkAbort(signal);
    const identity = JSON.stringify(admission);
    let e = this.entries.get(key);
    // Fixed source dimensions do not change with a new frame budget. Recheck
    // them without disposing the source/decoder and reopening the entire asset.
    if(e && e.surfaces !== decoderSurfaces(admission))reject("INVALID_VISUAL_PLAN","Retire the decoder before changing its surface contract");
    if (e) {
      try { admitDimensions(e.width, e.height, admission); e.admission = identity; }
      catch (error) { await this.dispose(e); this.entries.delete(key); this.sources.delete(key); await bytes.close(); throw error; }
    }
    if (!e) {
      const input = new Input({ source: verifiedSource(bytes), formats: ALL_FORMATS });
      e = { input, iterator: null, current: null, ended: false, terminal: undefined, last: -Infinity, bytes: 0, admission: identity, canvases: new Set() };
      this.entries.set(key, e);
      try {
        const tracks = await input.getVideoTracks(); checkAbort(signal);
        if (tracks.length !== 1) reject("UNSUPPORTED_RENDER_FEATURE", "Exactly one video stream is required");
        const track = await input.getPrimaryVideoTrack();
        if (!track) reject("VISUAL_DECODE_FAILED", "Primary video track is unavailable");
        if (await track.canBeTransparent()) reject("UNSUPPORTED_RENDER_FEATURE", "Transparent video is not supported by this execution profile");
        if (!await track.canDecode()) reject("VISUAL_DECODE_FAILED", "Primary video codec is unavailable");
        admitDimensions(track.displayWidth, track.displayHeight, admission);
        e.width = track.displayWidth; e.height = track.displayHeight;
        e.videoColorSpace=await track.getColorSpace?.()??{};
        e.surfaces=decoderSurfaces(admission);
        e.sink = e.surfaces===1?new DirectVideoSink(track):new CanvasSink(track, { poolSize:e.surfaces-1, fit: "contain" });
        e.poolSize=e.surfaces===1?1:e.surfaces-1;
        e.bytes = track.displayWidth * track.displayHeight * e.poolSize * 4;
      } catch (error) { await this.dispose(e); this.entries.delete(key); this.sources.delete(key); await bytes.close(); throw error; }
    }
    return e.width*e.height*decoderSurfaces(admission)*4;
  }
  async decode(key, bytes, decode, admission, signal) {
    checkAbort(signal);
    const requested = finite(decode.requestedTimeSeconds, "observed decode request");
    const window = eofWindow(decode);
    if (decode.selectedVideoStream !== null) reject("UNSUPPORTED_RENDER_FEATURE", "Only primary video selection is supported");
    await this.prepare(key,bytes,admission,signal);
    const e=this.entries.get(key);
    try {
      // Keep the decoder warm for monotonic targets. Restarting after every
      // missed half-second picture repeatedly seeks back to the keyframe and
      // re-reads/decompresses the same GOP. Backward user seeks still restart.
      if ((!e.iterator && !e.ended) || requested < e.last) {
        await e.iterator?.return(); checkAbort(signal);
        if(e.surfaces===1){for(const frame of new Set([...e.canvases,e.current?.canvas,e.terminal?.canvas]))if(frame)closeSurface(frame);e.canvases.clear();}
        e.iterator = e.sink.canvases(requested); e.current = null; e.ended = false; e.terminal = undefined;
      }
      e.last = requested;
      for (;;) {
        checkAbort(signal);
        if (e.ended) {
          if (!window) return { reason: "eof" };
          if (e.terminal === undefined) {
            await e.iterator?.return(); e.iterator = null; checkAbort(signal);
            e.terminal = await e.sink.getCanvas(Infinity); checkAbort(signal);
          }
          const held = terminalObservation(e.terminal, requested, window, admission);
          if (!held) return { reason: "eof" };
          e.canvases.add(held.canvas);
          if (e.canvases.size > e.poolSize) reject("VISUAL_BUDGET_EXCEEDED", "Decoder exceeded its admitted canvas pool");
          return { ...held, videoColorSpace:e.videoColorSpace, borrowed: true };
        }
        const value = e.current;
        if (value) {
          const observed = videoObservation(value, admission), timestamp = observed.actualTimestampSeconds, duration = observed.frameDurationSeconds;
          if (timestamp > requested) return { reason: "timestamp_gap" };
          if (requested < timestamp + duration) {
            // The serial execution bridge finishes its GPU upload before the
            // next iterator request. Borrow its bounded pool without another
            // full-frame canvas copy; only this cache owns disposal.
            e.canvases.add(value.canvas);
            if (e.canvases.size > e.poolSize) reject("VISUAL_BUDGET_EXCEEDED", "Decoder exceeded its admitted canvas pool");
            return { ...observed, videoColorSpace:e.videoColorSpace, borrowed: true };
          }
        }
        if(e.surfaces===1 && e.current){closeSurface(e.current.canvas);e.canvases.delete(e.current.canvas);e.current=null;}
        const item = await e.iterator.next(); if(signal?.aborted && e.surfaces===1)closeSurface(item.value?.canvas);checkAbort(signal);
        if (item.done || !item.value) { e.current = null; e.ended = true; continue; }
        e.current = item.value;
      }
    } catch (error) {
      await this.dispose(e); this.entries.delete(key); this.sources.delete(key); await bytes.close();
      if (error.code || signal?.aborted) throw error;
      reject("VISUAL_DECODE_FAILED", "Video decoder failed");
    }
  }
}
