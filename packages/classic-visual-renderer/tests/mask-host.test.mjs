import {encodeRgbaHex} from "../src/validation.mjs";
globalThis.__opencutVisualRgbaBatch=async()=>{};
import test from 'node:test';
import assert from 'node:assert/strict';
import {maskBegin} from './mask-fixture.mjs';
import {graphicObservations} from './blend-fixture.mjs';
import {validateMaskBegin,validateBlendBegin} from '../host/protocol.mjs';
import {browserHooks} from '../host/browser.mjs';
import {executeMaskVisualPage} from '../host/page.mjs';
import {validateVideoBegin} from '../host/video-runner.mjs';
import {executeVideoFrame} from '../host/video-browser.mjs';
test('mask begin preserves complete private handles and cannot enter old blend wire',()=>{
 const p=maskBegin();assert.equal(validateMaskBegin(p).plan.masks.length,2);
 assert.throws(()=>validateBlendBegin({...p,type:'beginBlendFrame'}));
 for(const change of [p=>p.resources=[],p=>p.plan.masks.reverse(),p=>p.plan.execution.execution.identity.revision='other',p=>p.fonts=[{fontId:'invented',handle:'private'}]]){const q=maskBegin();change(q);assert.throws(()=>validateMaskBegin(q));}
 assert.equal(validateVideoBegin({...p,type:'beginMaskVideo',frameCount:2,executionLimit:{kind:"progress",milliseconds:p.admission.deadlineMs}}).type,'beginMaskVideo');
});
test('mask host blocks pixels and injection until its own final bind succeeds once',async()=>{
 const exposed=new Map(),packets=[];let complete;
 await browserHooks.configurePage({begin:validateMaskBegin(maskBegin()),signal:new AbortController().signal,
 page:{exposeFunction:async(name,fn)=>exposed.set(name,fn)},broker:{request:async(op)=>op==='bindMaskFrameV1'?new Promise(resolve=>complete=resolve):{}},writer:{send:async p=>packets.push(p)}});
 const call=exposed.get('__opencutVisualCallback'),rgba=exposed.get('__opencutVisualRgba'),packet={offsetBytes:'0',bytesHex:'ff0000ff'};
 assert.throws(()=>call('bindMaskFrameV1',{}));await assert.rejects(rgba(packet));
 await call('bindCaptionFrameV3',{rasters:[],captions:[]});await call('bindGraphicFrameV1',graphicObservations);await call('bindBlendFrameV1',{});
 assert.throws(()=>call('bindMaskFrameV1',{masks:[]}));const pending=call('bindMaskFrameV1',{});
 assert.throws(()=>call('bindMaskFrameV1',{}));await assert.rejects(rgba(packet));complete({});await pending;await rgba(packet);assert.equal(packets.length,1);
});
test('native frame and persistent video dispatch choose only the additive mask renderer',async t=>{
 const oldFetch=globalThis.fetch;globalThis.fetch=async()=>({ok:true});t.after(()=>globalThis.fetch=oldFetch);
 let selected;await browserHooks.render({begin:maskBegin(),page:{evaluate:async fn=>{selected=fn;return{ok:true,receipt:{}}}}});assert.equal(selected,executeMaskVisualPage);
 globalThis.__opencutVisualModule={encodeRgbaHex,VisualDecoderCache:class{},renderClassicMaskFrame:async(_plan,host)=>{await host.rgba(0,new Uint8Array(4));return {masks:[]};}};
 try{assert.deepEqual(await executeVideoFrame({plan:{identity:{width:1,height:1}},admission:{},profile:'beginMaskFrame'}),{ok:true,receipt:{masks:[]}});}finally{for(const key of ['__opencutVisualModule','__opencutVideoBytes','__opencutVideoFonts','__opencutVideoDecoders'])delete globalThis[key];}
});
