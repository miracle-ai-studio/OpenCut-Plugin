import {encodeRgbaBase64} from "../src/validation.mjs";
globalThis.__opencutVisualRgbaBatch=async()=>{};
import test from 'node:test';
import assert from 'node:assert/strict';
import {executeVideoFrame,videoBrowserHooks,configureVideoPageCallbacks} from '../host/video-browser.mjs';
import {validateGraphicBegin} from '../host/protocol.mjs';

function frame(frameIndex=0){
  const fixed={projectId:'p',revision:'r',sequenceId:'s',time:{ticks:String(frameIndex),timebase:30},width:8,height:4};
  const execution={identity:{...fixed,profile:'classic-v31-gpu-normal-text-frame-v2'},media:{identity:{...fixed,profile:'classic-v31-gpu-normal-frame'},resources:[],layers:[]},texts:[],order:[]};
  return validateGraphicBegin({type:'beginGraphicFrame',executionId:'video',frameIndex,plan:{identity:{...fixed,profile:'classic-v31-gpu-normal-graphic-frame-v1'},execution:{identity:{...fixed,profile:'classic-v31-gpu-normal-caption-frame-v3'},execution,captions:[]},graphics:[{elementId:'shape',graphic:{geometry:{kind:'ellipse'},sourceWidth:2,sourceHeight:2,fill:[1,0,0,1],stroke:{widthPx:0,color:[0,0,0,1],align:'inside'}},quad:null}],order:['shape']},resources:[],fonts:[],admission:{maxEncodedBytes:8,maxFramePixels:'32',maxDecodedPixels:'1024',maxGpuBytes:'16384',maxRgbaBytes:'16384',maxOutputBytes:'1048576',maxRssBytes:'536870912',deadlineMs:1000,quotaProfile:'monitored-terminate'}});
}
const graphics={graphics:[{elementId:'shape',width:2,height:2,originX:0,originY:0}]};
async function configured(request=async()=>({})){
  const handlers=new Map(),calls=[],packets=[];
  const realm={begin:{type:'beginGraphicVideo'},currentFrame:frame(),signal:new AbortController().signal,page:{exposeFunction:async(name,fn)=>handlers.set(name,fn)},broker:{request:(op,args)=>{calls.push({op,args});return request(op,args);}},writer:{send:async packet=>packets.push(packet)}};
  const invokeRgba=await configureVideoPageCallbacks(realm);
  return {realm,calls,packets,callback:handlers.get('__opencutVisualCallback'),rgba:packet=>invokeRgba("__opencutVisualRgba",[packet])};
}
test('persistent graphic execution selects the shared graphic renderer and reuses all caches',async t=>{
  const oldFetch=globalThis.fetch;globalThis.fetch=async()=>({ok:true});t.after(()=>globalThis.fetch=oldFetch);
  const calls=[];
  globalThis.__opencutVisualModule={encodeRgbaBase64,VisualDecoderCache:class{},renderClassicGraphicFrame:async(...args)=>{calls.push(args);await args[1].rgba(0,new Uint8Array(4));return {graphics:[]};}};
  try{
    for(let n=0;n<2;n++)assert.deepEqual(await executeVideoFrame({plan:{n,identity:{width:1,height:1}},admission:{},profile:'beginGraphicFrame'}),{ok:true,receipt:{graphics:[]}});
    for(const index of [4,5,6])assert.equal(calls[0][index],calls[1][index]);
    assert.equal(calls[0][1].requireResolveCached,true);
    assert.deepEqual(await executeVideoFrame({plan:{},admission:{},profile:'invented'}),{ok:false,error:{code:'INVALID_HOST_PROTOCOL'}});
  }finally{for(const key of ['__opencutVisualModule','__opencutVideoBytes','__opencutVideoFonts','__opencutVideoDecoders'])delete globalThis[key];}
});
test('video frame callbacks reuse complete child and graphic binding fences and reset per frame',async()=>{
  const gate=await configured();
  for(let index=0;index<2;index++){
    gate.realm.currentFrame=frame(index);
    await assert.rejects(gate.callback('bindGraphicFrameV1',graphics));
    await gate.callback('bindCaptionFrameV3',{rasters:[],captions:[]});
    await assert.rejects(gate.callback('bindGraphicFrameV1',{graphics:[{...graphics.graphics[0],width:3}]}));
    await gate.callback('bindGraphicFrameV1',graphics);
    await assert.rejects(gate.callback('bindGraphicFrameV1',graphics));
    await gate.rgba({offsetBytes:'0',bytesHex:'ff0000ff'});
    assert.equal(gate.packets.at(-1).frameIndex,index);
    await assert.rejects(gate.callback('resolveVerifiedBytes',{resource:{assetId:'outside'}}),e=>e.code==='NOT_AUTHORIZED');
    await assert.rejects(gate.callback('resolveVerifiedFontBytes',{font:{fontId:'outside'}}),e=>e.code==='NOT_AUTHORIZED');
  }
  assert.deepEqual(gate.calls.map(c=>c.op),['bindCaptionFrameV3','bindGraphicFrameV1','bindCaptionFrameV3','bindGraphicFrameV1']);
  gate.realm.currentFrame=undefined;
  await assert.rejects(gate.callback('bindGraphicFrameV1',graphics));await assert.rejects(gate.rgba({offsetBytes:'0',bytesHex:'ff0000ff'}));
});
test('graphic video waits for the child binding reply before admitting graphics, including concurrent requests',async()=>{
  let resolve;const gate=await configured(op=>op==='bindCaptionFrameV3'?new Promise(done=>resolve=done):{});
  const child=gate.callback('bindCaptionFrameV3',{rasters:[],captions:[]});
  await assert.rejects(gate.callback('bindCaptionFrameV3',{rasters:[],captions:[]}));
  await assert.rejects(gate.callback('bindGraphicFrameV1',graphics));
  resolve({});await child;await gate.callback('bindGraphicFrameV1',graphics);
  assert.equal(gate.calls.length,2);
});
