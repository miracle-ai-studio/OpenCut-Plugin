import { clearGraphicSourceCache } from "../src/graphic-source.mjs";
import { test, mock } from "node:test";
import assert from "node:assert/strict";

const opened = [], iterators = [];
let checkSources = sources => assert.equal(sources.get("first"), sources.get("second"));
const values = [
  { timestamp: 0, duration: .5, canvas: { width: 4, height: 2 } },
  { timestamp: .5, duration: .5, canvas: { width: 4, height: 2 } },
  { timestamp: 2, duration: .5, canvas: { width: 4, height: 2 } },
];
class Input {
  constructor({ source }) { this.source = source; this.disposed = false; opened.push(this); }
  async getVideoTracks() { return [{}]; }
  async getPrimaryVideoTrack() { return { displayWidth: 4, displayHeight: 2,
    canBeTransparent: async () => false, canDecode: async () => true }; }
  dispose() { this.disposed = true; this.source.dispose(); }
}
class CanvasSink {
  constructor(_track,options={}){this.compact=options.poolSize===1?{width:4,height:2}:null;}
  frames = structuredClone(values);
  async getCanvas(timestamp) { assert.equal(timestamp, Infinity); return this.frames.at(-1); }
  canvases(start) {
    const frames = this.frames.map(frame=>this.compact?{...frame,canvas:this.compact}:frame);
    let index = values.findIndex(v => v.timestamp + v.duration > start);
    if (index < 0) index = values.length;
    const iterator = { start, returned: false, reads: 0, next: async () => { iterator.reads++; return { value: frames[index++], done: index > frames.length }; },
      return: async () => { iterator.returned = true; return { done: true }; } };
    iterators.push(iterator); return iterator;
  }
}
class VideoSampleSink {
 samples(start){const iterator=new CanvasSink({},{}).canvases(start);return {async *[Symbol.asyncIterator](){try{for(;;){const item=await iterator.next();if(item.done)return;const value=item.value;yield {timestamp:value.timestamp,duration:value.duration,displayWidth:4,displayHeight:2,rotation:0,toVideoFrame:()=>({displayWidth:4,displayHeight:2,close:mock.fn()}),close:mock.fn()};}}finally{await iterator.return();}}};}
 async getSample(time){const value=[...values].reverse().find(v=>v.timestamp<=time);return value?{timestamp:value.timestamp,duration:value.duration,displayWidth:4,displayHeight:2,rotation:0,toVideoFrame:()=>({displayWidth:4,displayHeight:2,close:mock.fn()}),close:mock.fn()}:null;}
}
mock.module("mediabunny", { namedExports: { Input, CanvasSink, VideoSampleSink, StreamSource: class { constructor(options) { this.options = options; } dispose() { this.options.dispose(); } }, ALL_FORMATS: [] } });
mock.module("../src/gpu.mjs", { namedExports: {
  openGpuRealm: async () => ({}),
    clearGpuFrameCaches: clearGraphicSourceCache,
    closeGpuRealm: clearGraphicSourceCache,
  renderGpuFrame: async (_frame, sources, _admission, emit) => {
    checkSources(sources);
    await emit(0, new Uint8Array([42, 31, 19, 255])); return { peakGpuBytes: "4" };
  },
} });
globalThis.OffscreenCanvas = class {
  constructor(width, height) { this.width = width; this.height = height; }
  getContext() { return { drawImage() {} }; }
};
const { VisualDecoderCache, decodeImage, decodeVideo } = await import("../src/decoder.mjs");
const { openVerifiedRangeSource } = await import("@opencut/classic-audio-renderer/verified-range-source");
const resource = { assetId: "asset", contentHash: "a".repeat(64), size: "1", kind: "video" };
const openSource = async cache => {
  const source = await openVerifiedRangeSource(resource, {
    open: async () => ({ transferId: "verified", contentHash: resource.contentHash, byteLength: "1", maxChunkBytes: 65536 }),
    read: async (_id, offset, count) => ({ offset: String(offset), eof: true, bytes: new Uint8Array(count) }),
    release: async () => {},
  }, { reserve: () => () => {} }, 65536);
  return source;
};
function resourceCallback(plan, name, args) {
  if (name === "resolveVerifiedBytes") return { transferId: "verified", contentHash: args.resource.contentHash, byteLength: args.resource.size, maxChunkBytes: 65536 };
  if (name === "releaseVerifiedBytes") return { released: true };
  if (name === "readVerifiedBytes") return { offset: args.offset, bytesHex: "01".repeat(args.count), eof: true };
  return { identity: plan.identity, decoded: args.decoded };
}
const admission = { maxWidth: 4, maxHeight: 2, maxPixels: "8", maxDecodedBytes: "128" };
const decode = requestedTimeSeconds => ({ selectedVideoStream: null, requestedTimeSeconds, eofPolicy: { kind: "noFrame" } });
const signal = new AbortController().signal;
for (const surfaces of [1, 2, 4]) test(`distant forward cuts seek directly while nearby and long-duration frames stay warm (${surfaces} surfaces)`, async () => {
  const saved = values.slice(), cache = new VisualDecoderCache(), source = await openSource();
  const permit = { ...admission, decoderSurfaces: surfaces };
  values.splice(0, values.length, ...Array.from({ length: 241 }, (_, i) => ({
    timestamp: i / 2, duration: .5, canvas: { width: 4, height: 2 },
  })));
  cache.keepRange("cut", source);
  const count = opened.length, before = iterators.length;
  const read = time => cache.decode("cut", source, decode(time), permit, signal);
  try {
    await read(.25); await read(1.75);
    assert.equal(iterators.length, before + 1, "missed nearby pictures must not reopen a GOP");
    const first = iterators.at(-1), reads = first.reads;
    assert.equal((await read(90.25)).actualTimestampSeconds, 90);
    assert.equal(first.returned, true);
    assert.equal(first.reads, reads, "discarded interval must not be decoded sequentially");
    assert.equal(iterators.at(-1).start, 90.25);
    assert.equal(iterators.at(-1).reads, 1);
    assert.equal(opened.length, count + 1, "retain the same verified Input and demuxer");
    assert.equal((await read(.25)).actualTimestampSeconds, 0);
    // A VFR frame that already covers the requested time is still reusable.
    const entry = cache.entries.get("cut"); entry.current.duration = 60;
    const retained = iterators.length;
    assert.equal((await read(30)).actualTimestampSeconds, 0);
    assert.equal(iterators.length, retained);
  } finally { await cache.close(); values.splice(0, values.length, ...saved); }
});
test("a cut decoder adopts its verified source and covering PTS without reopening or decoding on the presentation path",async()=>{
 const {prewarmVideoSources}=await import("../src/index.mjs");
 const future=new VisualDecoderCache(),current=new VisualDecoderCache(),before=opened.length;
 const plan={layers:[{elementId:"cut",resource,decode:{kind:"video",...decode(0)}}]};
 const host={admission:{maxRgbaBytes:String(48*1024*1024),maxEncodedBytes:8*1024*1024},
  rangeChunkBytes:524288,rangeReadAhead:true,
  callback:async(name,args)=>name==="resolveVerifiedBytes"?{transferId:"verified",contentHash:args.resource.contentHash,byteLength:args.resource.size,maxChunkBytes:524288}:resourceCallback(plan,name,args),readVerifiedBytes:async(_id,_offset,count)=>new Uint8Array(count)};
 try{
  assert.equal(await prewarmVideoSources(plan,{cut:admission},host,signal,future,new Set()),true);
  const key=`${resource.assetId}:${resource.contentHash}:${resource.size}`;
  await current.adopt(future);await future.close();assert.equal(opened.length,before+1);assert.equal(opened.at(-1).disposed,false);
  const observed=await current.decode(key,current.range(key),decode(.25),admission,signal);
  assert.equal(observed.actualTimestampSeconds,0);assert.equal(opened.length,before+1);
  assert.equal(await prewarmVideoSources(plan,{cut:admission},host,signal,future,current.keys()),false);
 }finally{await current.close();await future.close();}
 assert.equal(opened.at(-1).disposed,true);
});

test("worker image decoding keeps dimension admission, copies the bitmap and releases it on cancellation", async () => {
  const png=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==','base64');
  const bitmap={width:1,height:1,close:mock.fn()};
  const previous=globalThis.createImageBitmap;
  globalThis.createImageBitmap=mock.fn(async()=>bitmap);
  try {
    const observed=await decodeImage(png,admission,signal);
    assert.equal(observed.width,1);assert.equal(observed.height,1);assert.equal(bitmap.close.mock.callCount(),1);
    const abort=new AbortController();
    globalThis.createImageBitmap=async()=>{abort.abort();return bitmap;};
    await assert.rejects(decodeImage(png,admission,abort.signal),{name:'AbortError'});
    assert.equal(bitmap.close.mock.callCount(),2);
  }finally{globalThis.createImageBitmap=previous;}
});

test("serial cached decoder preserves covering PTS/gaps/EOF, skips missed pictures without reopening a GOP and restarts backward requests", async () => {
  const cache = new VisualDecoderCache(), count = opened.length, sinks = iterators.length;
  const source = await openSource(); cache.keepRange("verified", source);
  const read = t => cache.decode("verified", source, decode(t), admission, signal);
  try {
    const first = await read(.25), repeated = await read(.4);
    assert.equal(first.actualTimestampSeconds, 0);
    assert.equal(repeated.actualTimestampSeconds, 0);
    assert.equal(first.canvas, repeated.canvas);
    assert.equal(first.borrowed, true);
    assert.equal((await read(.75)).actualTimestampSeconds, .5);
    assert.deepEqual(await read(1.25), { reason: "timestamp_gap" });
    const retained = t => cache.decode("verified", source, { ...decode(t), eofPolicy: { kind: "holdLastFrame", readWindowStartSeconds: 0, readWindowEndSeconds: 2.5 } }, admission, signal);
    assert.deepEqual(await retained(1.25), { reason: "timestamp_gap" });
    assert.equal((await read(2.25)).actualTimestampSeconds, 2);
    assert.deepEqual(await read(3), { reason: "eof" });
    const held = await retained(3), later = await retained(3.5);
    assert.equal(held.eofHeld, true);
    assert.equal(held.actualTimestampSeconds, 2);
    assert.equal(held.frameDurationSeconds, .5);
    assert.equal(held.canvas, later.canvas);
    assert.equal(held.borrowed, true);
    assert.deepEqual(await cache.decode("verified", source, { ...decode(4), eofPolicy: { kind: "holdLastFrame", readWindowStartSeconds: 3, readWindowEndSeconds: 4 } }, admission, signal), { reason: "eof" });
    assert.deepEqual(await read(4), { reason: "eof" });
    assert.equal((await read(.25)).actualTimestampSeconds, 0);
    assert.equal(opened.length - count, 1);
    assert.deepEqual(iterators.slice(sinks).map(iterator => iterator.start), [.25, .25]);
    assert.equal(iterators[sinks].returned, true);
    assert.equal(cache.retainedBytes(), 96);
  } finally { await cache.close(); }
  assert.equal(opened.at(-1).disposed, true);
  assert.equal(cache.retainedBytes(), 0);
});

test("different per-layer times or admission retain earlier borrowed observations until upload", async () => {
  const { renderClassicFrame } = await import("../src/index.mjs");
  const cache = new VisualDecoderCache(), resource = { assetId: "asset", contentHash: "b".repeat(64), size: "1", kind: "video" };
  const encoded = new Map();
  const plan = { identity: { width: 1, height: 1, profile: "classic-v31-gpu-normal-frame" }, resources: [resource],
    layers: [0, .75, 2.25, 0].map((time, index) => ({ elementId: String(index), resource,
      decode: { kind: "video", ...decode(time), requestedTime: { ticks: String(time * 4), timebase: 4 } } })) };
  const observed = [];
  const host = { admission: { maxFramePixels: "1", maxRgbaBytes: "4194304", maxEncodedBytes: 2097152 },
    callback: async (name, args) => name === "admitDecode" ? (args.elementId === "3" ? { ...admission, maxDecodedBytes: "129" } : admission)
      : resourceCallback(plan, name, args), rgba: async () => {} };
  const originalCheck = checkSources;
  checkSources = sources => {
    for (let index = 0; index < 4; index++) observed.push(sources.get(String(index)));
    assert.deepEqual(observed.map(s => s.actualTimestampSeconds), [0, .5, 2, 0]);
    assert.equal(observed[0].borrowed, true);
    for (const source of observed.slice(1)) {
      assert.notEqual(source.canvas, observed[0].canvas);
      assert.equal(source.borrowed, undefined);
      assert.equal(source.canvas.width, 4);
    }
    assert.equal(observed[0].canvas.width, 4);
  };
  try {
    const count = opened.length;
    await renderClassicFrame(plan, host, {}, signal, encoded, cache);
    assert.equal(opened.length - count, 4);
    assert.equal(cache.retainedBytes(), 96);
    assert.equal(observed[0].canvas.width, 4);
    assert.ok(observed.slice(1).every(s => s.canvas.width === 0));
  } finally { checkSources = originalCheck; await cache.close(); }
  assert.equal(observed[0].canvas.width, 0);
});

test("changed admission is rechecked before allocation; inactive and aborted resources release", async () => {
  const cache = new VisualDecoderCache();
  const source = await openSource(); cache.keepRange("verified", source);
  await cache.decode("verified", source, decode(0), admission, signal);
  await assert.rejects(cache.decode("verified", source, decode(0), { ...admission, maxPixels: "7" }, signal), e => e.code === "VISUAL_BUDGET_EXCEEDED");
  assert.equal(cache.retainedBytes(), 0);
  assert.equal(opened.at(-1).disposed, true);
  const other = await openSource(); cache.keepRange("other", other);
  await cache.decode("other", other, decode(0), admission, signal);
  await cache.retain(new Set());
  assert.equal(opened.at(-1).disposed, true);
  const abort = new AbortController(); abort.abort();
  const count = opened.length;
  await assert.rejects(cache.decode("aborted", other, decode(0), admission, abort.signal));
  assert.equal(opened.length, count);
  await cache.close();
});

test("retained pool replaces temporary decode capacity within the unchanged whole-frame budget; identical observations share one source", async () => {
  const { renderClassicFrame } = await import("../src/index.mjs");
  const cache = new VisualDecoderCache(), resource = { assetId: "asset", contentHash: "a".repeat(64), size: "1", kind: "video" };
  const encoded = new Map();
  const plan = { identity: { width: 1, height: 1, profile: "classic-v31-gpu-normal-frame", time: { ticks: "0", timebase: 1 } },
    resources: [resource], layers: ["first", "second"].map(elementId => ({ elementId, resource,
      decode: { kind: "video", ...decode(0), requestedTime: { ticks: "0", timebase: 1 } } })) };
  const host = { admission: { maxFramePixels: "1", maxRgbaBytes: "393351", maxEncodedBytes: 2097152 },
    callback: async (name, args) => name === "admitDecode" ? admission : resourceCallback(plan, name, args), rgba: async () => {} };
  try {
    const count = opened.length;
    assert.equal((await renderClassicFrame(plan, host, {}, signal, encoded, cache)).peakAccountedBytes, "393351");
    assert.equal((await renderClassicFrame(plan, host, {}, signal, encoded, cache)).peakAccountedBytes, "393351");
    assert.equal(opened.length - count, 1);
    await assert.rejects(renderClassicFrame(plan, { ...host, admission: { ...host.admission, maxRgbaBytes: "393350" } }, {}, signal, encoded, cache), e => e.code === "VISUAL_BUDGET_EXCEEDED");
  } finally { await cache.close(); }
});

test("late picture rejection keeps the bounded decoder warm; a fresh binding is still required and authority failures dispose it", async () => {
  const { renderClassicFrame } = await import("../src/index.mjs");
  const cache = new VisualDecoderCache(), encoded = new Map();
  const plan = { identity: { width: 1, height: 1, profile: "classic-v31-gpu-normal-frame" },
    resources: [resource], layers: ["first", "second"].map(elementId => ({ elementId, resource,
      decode: { kind: "video", ...decode(0), requestedTime: { ticks: "0", timebase: 1 } } })) };
  let rejection = "PREVIEW_FRAME_STALE", bindings = 0, emitted = 0;
  const host = { admission: { maxFramePixels: "1", maxRgbaBytes: "4194304", maxEncodedBytes: 2097152 },
    callback: async (name, args) => {
      if (name === "admitDecode") return admission;
      if (name === "bindFrame") {
        bindings++;
        if (rejection) throw Object.assign(new Error("Rejected picture"), { code: rejection });
      }
      return resourceCallback(plan, name, args);
    }, rgba: async () => { emitted++; } };
  const count = opened.length;
  try {
    await assert.rejects(renderClassicFrame(plan, host, {}, signal, encoded, cache), e => e.code === "PREVIEW_FRAME_STALE");
    assert.equal(emitted, 0);
    assert.equal(cache.retainedBytes(), 96);
    assert.equal(opened.at(-1).disposed, false);
    rejection = "";
    await renderClassicFrame(plan, host, {}, signal, encoded, cache);
    assert.equal(bindings, 2);
    assert.equal(emitted, 1);
    assert.equal(opened.length - count, 1);
    rejection = "NOT_AUTHORIZED";
    await assert.rejects(renderClassicFrame(plan, host, {}, signal, encoded, cache), e => e.code === rejection);
    assert.equal(emitted, 1);
    assert.equal(cache.retainedBytes(), 0);
    assert.equal(opened.at(-1).disposed, true);
  } finally { await cache.close(); }
});

test("cache retirement drains the decoder iterator before closing its verified source", async () => {
  const cache = new VisualDecoderCache(), source = await openSource();
  cache.keepRange("retiring", source);
  await cache.decode("retiring", source, decode(.25), admission, signal);
  const input = opened.at(-1), iterator = iterators.at(-1), original = iterator.return;
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  iterator.return = async () => { await gate; return original(); };
  let disposals = 0; const dispose = input.dispose.bind(input);
  input.dispose = () => { disposals++; dispose(); };
  const closing = cache.close(), repeated = cache.close();
  try {
    assert.equal(input.disposed, false, "The live iterator still owns its source");
    assert.equal((await source.readBytes(0, 1)).length, 1);
  } finally { release(); await Promise.all([closing, repeated]); }
  assert.equal(disposals, 1);
  assert.equal(iterator.returned, true);
  assert.equal(input.disposed, true);
  await assert.rejects(source.readBytes(0, 1), error => error.code === "RESOURCE_SOURCE_CLOSED");
});

test("foreground ranges negotiate512 KiB, charge bounded buffers and keep ordinary64 KiB clients strict", async () => {
  const length = 1024 * 1024, fixed = { ...resource, size: String(length) };
  let charged = 0, released = 0, reads = 0;
  const access = {
    open: async () => ({ transferId: "large", contentHash: fixed.contentHash, byteLength: fixed.size, maxChunkBytes: 512 * 1024 }),
    read: async (_id, offset, count) => { reads++; return { offset: String(offset), eof: offset + count === length, bytes: Uint8Array.from({ length: count }, (_, i) => (offset + i) % 251) }; },
    release: async () => { released++; },
  };
  const budget = { reserve: n => { charged += n; return () => { charged -= n; }; } };
  await assert.rejects(openVerifiedRangeSource(fixed, access, budget, 65536), /descriptor changed/);
  assert.equal(charged, 0);
  const source = await openVerifiedRangeSource(fixed, access, budget, 65536, undefined, { maximumChunkBytes: 512 * 1024 });
  assert.equal(charged, 65536 * 3 + 512 * 1024 * 6);
  const value = await source.readBytes(0, 512 * 1024);
  assert.equal(value.length, 512 * 1024); assert.equal(value[300000], 300000 % 251);
  await assert.rejects(source.readBytes(0, 512 * 1024 + 1), /admitted bounds/);
  assert.equal(reads, 1); await source.close(); assert.equal(charged, 0); assert.equal(released, 2);
});

for(const chunk of [65536,512*1024])test(`foreground range read-ahead (${chunk}) shares two exact bounded lines, clamps EOF and evicts before release`, async () => {
  const length=chunk*2+7,fixed={...resource,size:String(length)},requests=[];
  let charged=0,released=0;
  const source=await openVerifiedRangeSource(fixed,{
    open:async()=>({transferId:'lines',contentHash:fixed.contentHash,byteLength:fixed.size,maxChunkBytes:chunk}),
    read:async(_id,offset,count)=>{requests.push([offset,count]);return{offset:String(offset),eof:offset+count===length,bytes:Uint8Array.from({length:count},(_,i)=>(offset+i)%251)};},
    release:async()=>{released++;},
  },{reserve:n=>{charged+=n;return()=>{charged-=n;};}},65536,undefined,{maximumChunkBytes:chunk,readAhead:true});
  assert.equal(charged,65536*3+chunk*8);
  const read=async(start,end)=>{
    const reader=source.source.options.read(start,end).getReader();let n=start;
    for(;;){const r=await reader.read();if(r.done)break;for(const v of r.value)assert.equal(v,(n++)%251);}
    assert.equal(n,end);
  };
  await Promise.all([read(100,200),read(300,500)]);assert.deepEqual(requests,[[0,chunk]]);
  await read(chunk-10,chunk+20);assert.deepEqual(requests,[[0,chunk],[chunk,chunk]]);
  await read(100,120);assert.equal(requests.length,2);
  await read(chunk*2, length);assert.deepEqual(requests.at(-1),[chunk*2,7]);
  await read(chunk+100,chunk+110);assert.equal(requests.length,4,'least recently used line was evicted');
  await source.close();assert.equal(charged,0);assert.equal(released,1);
  await assert.rejects(read(0,1),e=>e.code==='RESOURCE_SOURCE_CLOSED');
});

test("metadata preparation reserves exact fixed sink geometry without decoding, and a fresh smaller native grant still rejects it",async()=>{
 const cache=new VisualDecoderCache(),source=await openSource(),before=iterators.length;
 cache.keepRange("verified-actual",source);
 const allowance={...admission,maxPixels:"1000",maxDecodedBytes:"16000"};
 assert.equal(await cache.prepare("verified-actual",source,allowance,signal),4*2*16);
 assert.equal(iterators.length,before,"metadata preparation must not materialize a decoder canvas");
 await assert.rejects(cache.prepare("verified-actual",source,{...allowance,maxPixels:"7",maxDecodedBytes:"112"},signal),e=>e.code==="VISUAL_BUDGET_EXCEEDED");
 assert.equal(cache.retainedBytes(),0);assert.equal(opened.at(-1).disposed,true);await cache.close();
});

test("scene two-surface admission keeps a single borrowed canvas pool and preserves seek clocks",async()=>{
 const cache=new VisualDecoderCache(),source=await openSource(cache),permit={...admission,decoderSurfaces:2,maxDecodedBytes:"64"};
 try {
  assert.equal(await cache.prepare("compact",source,permit,signal),64);
  const first=await cache.decode("compact",source,decode(0),permit,signal);
  assert.equal(first.borrowed,true);assert.equal(first.actualTimestampSeconds,0);
  const next=await cache.decode("compact",source,decode(.5),permit,signal);
  assert.equal(next.actualTimestampSeconds,.5);assert.equal(next.canvas,first.canvas);
  assert.equal(cache.retainedBytes(),32);
  await assert.rejects(cache.prepare("compact",source,{...admission,maxDecodedBytes:"128"},signal),{code:"INVALID_VISUAL_PLAN"});
 } finally {await cache.close();}
});
test("direct scene video retains one frame, closes expired frames and seeks without a CPU canvas",async()=>{
 const cache=new VisualDecoderCache(),source=await openSource(cache),permit={...admission,decoderSurfaces:1,maxDecodedBytes:"32"};
 try{
  const before=iterators.length;
  assert.equal(await cache.prepare("direct",source,permit,signal),32);
  const a=await cache.decode("direct",source,decode(0),permit,signal);assert.equal(a.width,4);assert.equal(a.videoRotation,0);
  const b=await cache.decode("direct",source,decode(.5),permit,signal);assert.equal(b.actualTimestampSeconds,.5);assert.equal(a.canvas.close.mock.callCount(),1);
  assert.equal(cache.retainedBytes(),32);assert.equal(cache.entries.get("direct").canvases.size,1);
  assert.equal(iterators.length,before+1,"Monotonic direct frames reuse one decoder iterator");
  const back=await cache.decode("direct",source,decode(.1),permit,signal);assert.equal(back.actualTimestampSeconds,0);
  assert.equal(iterators.length,before+2,"Backward seek replaces the prior decoder iterator");
  await cache.close();assert.equal(back.canvas.close.mock.callCount(),1);assert.equal(b.canvas.close.mock.callCount(),1);
 }finally{await cache.close();}
});
test("one-shot direct video transfers the selected frame after disposing its input",async()=>{
 const cache=new VisualDecoderCache(),source=await openSource(cache);
 const result=await decodeVideo(source,decode(0),{...admission,decoderSurfaces:1,maxDecodedBytes:"32"},signal);
 assert.equal(result.width,4);assert.equal(result.canvas.close.mock.callCount(),0);result.canvas.close();await cache.close();
});
