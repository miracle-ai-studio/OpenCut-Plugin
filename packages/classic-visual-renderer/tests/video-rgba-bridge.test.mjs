import test from "node:test";
import assert from "node:assert/strict";
import {encodeRgbaHex} from "../src/validation.mjs";
import {executeVideoFrame,videoBrowserHooks} from "../host/video-browser.mjs";
import {openVideoRgbaTransport} from "../host/video-rgba.mjs";
import {validateBlendBegin} from "../host/protocol.mjs";
import {blendBegin,graphicObservations} from "./blend-fixture.mjs";
const origin="http://127.0.0.1:49199";
async function configured(t,onPacket=()=>{}) {
  const handlers=new Map(),packets=[],cancel=new AbortController(),frame=validateBlendBegin(blendBegin(0));
  const realm={begin:{type:"beginBlendVideo",admission:{...frame.admission,maxRgbaBytes:String(4*1024*1024)}},currentFrame:frame,signal:cancel.signal,
    fixedModules:{csp:"connect-src 'none'"},context:{route:async()=>{},unroute:async()=>{},grantPermissions:async()=>{},newCDPSession:async()=>({send:async()=>{}})},
    page:{on:()=>{},exposeFunction:async(name,handler)=>handlers.set(name,handler)},broker:{request:async()=>({})},
    writer:{send:async packet=>{packets.push(packet);await onPacket(packet,realm,cancel);}}};
  await videoBrowserHooks.configurePage(realm);t.after(()=>realm.rgbaTransport.close());
  const callback=handlers.get("__opencutVisualCallback");
  const bind=async()=>{await callback("bindCaptionFrameV3",{rasters:[],captions:[]});await callback("bindGraphicFrameV1",graphicObservations);await callback("bindBlendFrameV1",{});};
  const size=()=>realm.currentFrame.plan.identity.width*realm.currentFrame.plan.identity.height*4;
  const upload=(body,url=realm.rgbaTransport.endpoint+"/0",method="POST",from=origin)=>fetch(url,{method,headers:{Origin:from,"Content-Type":"application/octet-stream"},body});
  return {realm,packets,cancel,bind,size,upload};
}
test("binary video retains final binding and rejects wrong capability, frame and origin",async t=>{
  const gate=await configured(t);
  for(const [url,from] of [[gate.realm.rgbaTransport.endpoint+"/1",origin],[gate.realm.rgbaTransport.endpoint+"/0","http://other.invalid"]]) {
    const r=await gate.upload(new Uint8Array(gate.size()),url,"POST",from);assert.equal(r.status,400);assert.equal((await r.json()).code,"NOT_AUTHORIZED");
  }
  const r=await gate.upload(new Uint8Array(gate.size()));assert.equal(r.status,400);assert.equal((await r.json()).code,"INVALID_VISUAL_PLAN");assert.equal(gate.packets.length,0);
});
test("one binary frame preserves canonical bounded packet bytes and rejects a duplicate upload",async t=>{
  const gate=await configured(t);await gate.bind();
  gate.realm.currentFrame.plan.identity.width=256;gate.realm.currentFrame.plan.identity.height=145;
  const bytes=Buffer.alloc(gate.size());for(let i=0;i<bytes.length;i++)bytes[i]=i%251;
  assert.equal((await gate.upload(bytes)).status,204);
  assert.deepEqual(Buffer.concat(gate.packets.map(p=>Buffer.from(p.bytesHex,"hex"))),bytes);
  let offset=0;for(const p of gate.packets){assert.equal(p.offsetBytes,String(offset));assert.equal(p.frameIndex,0);const n=p.bytesHex.length/2;assert.ok(n<=65536&&n%4===0);offset+=n;}
  assert.equal((await gate.upload(bytes)).status,400);
});
test("short and oversized binary frames never acknowledge completion",async t=>{
  for(const delta of [-4,4]){const gate=await configured(t);await gate.bind();const r=await gate.upload(new Uint8Array(gate.size()+delta));assert.equal(r.status,400);assert.equal((await r.json()).code,"INVALID_OUTPUT_RGBA");}
});
test("encoded output is admitted once after finalization, remains bounded and preserves non-pixel tail bytes",async t=>{
  const realm={begin:{canvasEncoding:{maxOutputBytes:65539}},signal:new AbortController().signal};
  const packets=[];const transport=await openVideoRgbaTransport(realm,async(packet,encoded)=>{assert.equal(encoded,true);packets.push(packet);},origin);
  t.after(()=>transport.close());
  const upload=body=>fetch(transport.endpoint+"/encoded",{method:"POST",headers:{Origin:origin,"Content-Type":"application/octet-stream"},body});
  assert.equal((await upload(Buffer.alloc(4))).status,400);assert.equal(packets.length,0);
  realm.finalizing=true;
  assert.equal((await upload(Buffer.alloc(65540))).status,400);assert.equal(packets.length,0);
  const bytes=Buffer.alloc(65539,47);assert.equal((await upload(bytes)).status,204);
  assert.deepEqual(packets.map(p=>[p.offsetBytes,p.bytesHex.length/2]),[["0",65536],["65536",3]]);
  assert.deepEqual(Buffer.concat(packets.map(p=>Buffer.from(p.bytesHex,"hex"))),bytes);
  assert.equal((await upload(bytes)).status,400);
});
test("retirement between binary chunks cannot relabel remaining pixels",async t=>{
  const gate=await configured(t,(_packet,realm)=>{realm.currentFrame={...realm.currentFrame,frameIndex:1};});
  gate.realm.currentFrame.plan.identity.width=256;gate.realm.currentFrame.plan.identity.height=128;
  await gate.bind();const r=await gate.upload(new Uint8Array(gate.size()));assert.equal(r.status,400);
  assert.equal((await r.json()).code,"RESOURCE_SOURCE_CLOSED");assert.equal(gate.packets.length,1);assert.equal(gate.packets[0].frameIndex,0);
});
test("abort closes the owned upload listener and prevents further chunks",async t=>{
  const realm={signal:new AbortController().signal};const transport=await openVideoRgbaTransport(realm,()=>{},origin);await transport.close();await assert.rejects(fetch(transport.endpoint+"/0"));
  const gate=await configured(t,(_packet,_realm,cancel)=>cancel.abort(Object.assign(Error("cancelled"),{code:"CANCELLED"})));
  gate.realm.currentFrame.plan.identity.width=256;gate.realm.currentFrame.plan.identity.height=128;await gate.bind();
  await assert.rejects(gate.upload(new Uint8Array(gate.size())));assert.equal(gate.packets.length,1);
});
function install(t,render,fetcher) {
  const keys=["__opencutVisualModule","__opencutVideoBytes","__opencutVideoDecoders","__opencutVideoFonts","__opencutVisualRgbaBatch","fetch"];
  const previous=keys.map(key=>[key,Object.getOwnPropertyDescriptor(globalThis,key)]);
  t.after(()=>{for(const [key,descriptor] of previous)if(descriptor)Object.defineProperty(globalThis,key,descriptor);else delete globalThis[key];});
  globalThis.__opencutVisualModule={encodeRgbaHex,VisualDecoderCache:class{},renderClassicTextFrame:render};globalThis.__opencutVisualRgbaBatch=async packets=>{assert(packets.length<=16);for(const packet of packets)await fetcher(packet);};globalThis.fetch=async()=>assert.fail("RGBA must not allocate a full-frame HTTP body");
}
test("browser owns exact copied bytes from a reused renderer buffer and waits for the upload acknowledgement",async t=>{
  let unblock,arrived,finished=false;const blocked=new Promise(r=>unblock=r),received=new Promise(r=>arrived=r);const expected=Buffer.alloc(9*65536);
  install(t,async(_plan,host)=>{assert.equal(host.outputBufferBytes,16*65536*16);const bytes=new Uint8Array(65536);
    for(let i=0;i<9;i++){for(let j=0;j<bytes.length;j++)bytes[j]=expected[i*65536+j]=(i*17+j)%256;await host.rgba(i*bytes.length,bytes);bytes.fill(255);}return {complete:true};},
    async packet=>{const offset=Number(packet.offsetBytes),bytes=Buffer.from(packet.bytesHex,"hex");assert.deepEqual(bytes,expected.subarray(offset,offset+bytes.length));if(offset+bytes.length===expected.length){arrived();await blocked;}});
  const pending=executeVideoFrame({plan:{identity:{width:256,height:576}},admission:{},outputUrl:"http://127.0.0.1/fixed"}).then(v=>{finished=true;return v;});
  await received;assert.equal(finished,false);unblock();assert.deepEqual(await pending,{ok:true,receipt:{complete:true}});
});
test("failed upload or incomplete output never returns a successful frame receipt",async t=>{
  install(t,async(_plan,host)=>{await host.rgba(0,Uint8Array.of(0,1,2,255));return {complete:true};},async()=>{throw Object.assign(Error("Retired"),{code:"RESOURCE_SOURCE_CLOSED"});});
  assert.deepEqual(await executeVideoFrame({plan:{identity:{width:1,height:1}},admission:{},outputUrl:"http://127.0.0.1/fixed"}),{ok:false,error:{code:"RESOURCE_SOURCE_CLOSED"}});
  globalThis.__opencutVisualRgbaBatch=async()=>{};
  assert.deepEqual(await executeVideoFrame({plan:{identity:{width:2,height:1}},admission:{},outputUrl:"http://127.0.0.1/fixed"}),{ok:false,error:{code:"INVALID_OUTPUT_RGBA"}});
});

test("binary resource reads retain the native descriptor, current frame and original per-chunk authorization",async t=>{
  const gate=await configured(t),frame=gate.realm.currentFrame,bytes=Buffer.alloc(131075,73),reads=[];
  gate.realm.videoTransfers.set("fixed-transfer",{frame,size:bytes.length});
  gate.realm.videoReadResource=async(id,offset,count)=>{reads.push({id,offset,count});return {offset:String(offset),bytesHex:bytes.subarray(offset,offset+count).toString("hex"),eof:offset+count===bytes.length};};
  const url=gate.realm.rgbaTransport.endpoint+"/resource/0/fixed-transfer";
  for(const [address,from] of [[url.replace('/resource/0/','/resource/1/'),origin],[url,"http://other.invalid"],[url.replace('fixed-transfer','missing'),origin]])assert.equal((await fetch(address,{headers:{Origin:from}})).status,400);
  assert.equal(reads.length,0);
  const response=await fetch(url,{headers:{Origin:origin}});assert.equal(response.status,200);assert.deepEqual(Buffer.from(await response.arrayBuffer()),bytes);
  assert.deepEqual(reads.map(r=>[r.offset,r.count]),[[0,65536],[65536,65536],[131072,3]]);
  reads.length=0;
  gate.realm.videoReadResource=async(...args)=>{gate.realm.currentFrame={...frame,frameIndex:1};return {offset:"0",bytesHex:"00".repeat(65536),eof:false};};
  await assert.rejects(async()=>{const r=await fetch(url,{headers:{Origin:origin}});await r.arrayBuffer();});
});

test("HTTP callbacks preserve the original operation gates and retire with their frame",async t=>{
 const gate=await configured(t),url=gate.realm.rgbaTransport.endpoint+"/callback";
 const call=(packet,from=origin)=>fetch(url,{method:"POST",headers:{Origin:from,"Content-Type":"text/plain"},body:JSON.stringify(packet)});
 assert.equal((await call({op:"bindBlendFrameV1",args:{}})).status,400);
 assert.equal((await call({op:"unknown",args:{}})).status,400);
 assert.equal((await call({op:"bindCaptionFrameV3",args:{rasters:[],captions:[]}},"http://other.invalid")).status,400);
 assert.equal((await call({op:"bindCaptionFrameV3",args:{rasters:[],captions:[]},extra:true})).status,400);
 assert.equal((await call({op:"bindCaptionFrameV3",args:{rasters:[],captions:[]}})).status,200);
 assert.equal((await call({op:"bindGraphicFrameV1",args:graphicObservations})).status,200);
 assert.equal((await call({op:"bindBlendFrameV1",args:{}})).status,200);
 gate.realm.currentFrame=undefined;
 assert.equal((await call({op:"bindBlendFrameV1",args:{}})).status,400);
});
