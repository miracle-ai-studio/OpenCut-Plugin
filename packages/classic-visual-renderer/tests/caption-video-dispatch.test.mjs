globalThis.__opencutVisualRgbaBatch=async()=>{};
import {test} from "node:test";
import assert from "node:assert/strict";
import {executeVideoFrame} from "../host/video-browser.mjs";
import {encodeRgbaBase64} from "../src/validation.mjs";
test("persistent caption video chooses shared V3 renderer and retains caches",async t=>{
 const oldFetch=globalThis.fetch;globalThis.fetch=async(_url,request)=>{pixels.push(Buffer.from(request.body));return {ok:true};};t.after(()=>globalThis.fetch=oldFetch);
  const calls=[],pixels=[];
  globalThis.__opencutVisualRgbaBatch=async packets=>pixels.push(...packets.map(packet=>Buffer.from(packet.bytesBase64,"base64")));
  globalThis.__opencutVisualModule={encodeRgbaBase64,VisualDecoderCache:class{},renderClassicTextFrame:async(...args)=>{calls.push(["v2",args]);await args[1].rgba(0,new Uint8Array(8));return {old:true};},renderClassicCaptionFrame:async(...args)=>{calls.push(["v3",args]);await args[1].rgba(0,new Uint8Array([99,0,1,15,16,127,128,254,255,100]).subarray(1,9));return {captions:[]};}};
  try{
    assert.deepEqual(await executeVideoFrame({plan:{identity:{width:2,height:1}},admission:{},profile:"beginCaptionFrame"}),{ok:true,receipt:{captions:[]}});
    assert.deepEqual(await executeVideoFrame({plan:{identity:{width:2,height:1}},admission:{}}),{ok:true,receipt:{old:true}});
    assert.equal(calls[0][0],"v3");assert.equal(calls[1][0],"v2");
    assert.equal(pixels[0].toString("hex"),"00010f107f80feff");
    assert.equal(calls[0][1][4],calls[1][1][4]);assert.equal(calls[0][1][5],calls[1][1][5]);assert.equal(calls[0][1][6],calls[1][1][6]);
  }finally{for(const key of ["__opencutVisualRgbaBatch","__opencutVisualModule","__opencutVideoBytes","__opencutVideoFonts","__opencutVideoDecoders"])delete globalThis[key];}
});
