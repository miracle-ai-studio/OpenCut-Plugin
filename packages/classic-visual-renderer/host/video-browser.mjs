import fs from "node:fs/promises";
import { moduleResponse, ORIGIN } from "@opencut/classic-audio-renderer/internal-host-runtime";
import { openVideoRgbaTransport } from "./video-rgba.mjs";
// Private persistent execution adapter. Layout/decoding/composition remain in
// the same versioned frame renderers used by PNG and live preview.
import { browserHooks } from "./browser.mjs";
import { checkAbort, reject } from "../src/validation.mjs";
export async function configureVideoPageCallbacks(realm) {
    let activeFrame,configured;
    const mediaTransfers=new Map(),waiting=new Set(),inFlight=new Set();let parked=false;
    realm.videoTransfers=new Map();
    // Mediabunny intentionally reads ahead after yielding a canvas. The native
    // owner admits callbacks inside an active frame only. Park bounded media
    // I/O between frames, then recheck the next frame's fixed resource set;
    // never relabel a binding or read a retired source under another frame.
    realm.resumeVideoReads=()=>{parked=false;for(const resume of [...waiting])resume();};
    realm.parkVideoReads=async()=>{parked=true;await Promise.allSettled([...inFlight]);};
    const waitForFrame=async()=>{
      checkAbort(realm.signal);
      if(realm.currentFrame&&!parked)return;
      if(waiting.size>=64)reject("RESOURCE_BUDGET_EXCEEDED","Too many parked video reads");
      await new Promise((resolve,fail)=>{
        const cleanup=()=>{waiting.delete(resume);realm.signal?.removeEventListener("abort",abort);};
        const resume=()=>{cleanup();resolve();};
        const abort=()=>{cleanup();fail(realm.signal.reason);};
        waiting.add(resume);realm.signal?.addEventListener("abort",abort,{once:true});
      });
      checkAbort(realm.signal);
    };
    // Capture the ordinary single-frame handlers instead of reimplementing
    // their resource authorization and text/caption/graphic binding fences.
    // Only the handlers reset per frame; browser, decoder and font caches persist.
    const invoke=async(name,args)=>{
      checkAbort(realm.signal);
      const op=name==="__opencutVisualCallback"?args[0]:null,argument=args[1];
      if(!realm.currentFrame||parked){
        if(!["readVerifiedBytes","releaseVerifiedBytes"].includes(op)||!mediaTransfers.has(argument?.transferId))
          reject("RESOURCE_SOURCE_CLOSED","No active frame admits this source callback");
        await waitForFrame();
      }
      const frame=realm.currentFrame;
      if(!frame)reject("INVALID_HOST_CALLBACK","Callback outside active video frame");
      if(op==="readVerifiedBytes"&&mediaTransfers.has(argument?.transferId)){
        let plan=frame.plan;while(plan.execution)plan=plan.execution;
        const current=(plan.media??plan).resources;
        const original=mediaTransfers.get(argument.transferId);
        if(!current.some(resource=>["assetId","contentHash","kind","size"].every(key=>resource[key]===original[key])))
          reject("RESOURCE_SOURCE_CLOSED","Video source retired before speculative read");
      }
      if(frame!==activeFrame) {
        activeFrame=frame;const handlers=new Map();
        configured=browserHooks.configurePage({...realm,begin:frame,
          page:{exposeFunction:async(key,handler)=>handlers.set(key,handler)},
          writer:{send:packet=>realm.writer.send({...packet,frameIndex:frame.frameIndex})},
        }).then(()=>handlers);
      }
      const handlers=await configured;checkAbort(realm.signal);
      if(realm.currentFrame!==frame)reject("INVALID_HOST_CALLBACK","Video frame ended before callback admission");
      const operation=Promise.resolve().then(()=>handlers.get(name)(...args));
      inFlight.add(operation);
      let result;try{result=await operation;}finally{inFlight.delete(operation);}
      if(op==="resolveVerifiedBytes"&&argument?.resource?.kind==="video"){
        if(mediaTransfers.size>=32&&!mediaTransfers.has(result.transferId))reject("RESOURCE_BUDGET_EXCEEDED","Too many active video descriptors");
        mediaTransfers.set(result.transferId,structuredClone(argument.resource));
      }
      if((op==="resolveVerifiedFontBytes"||(op==="resolveVerifiedBytes"&&argument?.resource?.kind!=="video"))&&typeof result?.transferId==="string") {
        const size=Number(result.byteLength);
        if(!Number.isSafeInteger(size)||size<1||size>realm.begin.admission.maxEncodedBytes)reject("INVALID_VISUAL_PLAN","Invalid fixed resource transfer");
        realm.videoTransfers.set(result.transferId,{frame,size});
      }
      if(op==="releaseVerifiedBytes"){mediaTransfers.delete(argument.transferId);realm.videoTransfers.delete(argument.transferId);}
      return result;
    };
    realm.videoCallback=(op,args)=>invoke("__opencutVisualCallback",[op,args]);
    realm.videoReadResource=(transferId,offset,count)=>invoke("__opencutVisualCallback",["readVerifiedBytes",{transferId,offset:String(offset),count}]);
    await realm.page.exposeFunction("__opencutVisualCallback",(...args)=>invoke("__opencutVisualCallback",args));
    await realm.page.exposeFunction("__opencutVisualRgbaBatch",async packets=>{
      if(!Array.isArray(packets)||packets.length<1||packets.length>16)reject("INVALID_OUTPUT_RGBA","Invalid bounded RGBA batch");
      for(const packet of packets)await invoke("__opencutVisualRgba",[packet]);
    });
    return invoke;
}
export const videoBrowserHooks={...browserHooks,loopbackTransport:true,
  async configurePage(realm) {
    const invoke=await configureVideoPageCallbacks(realm);
    realm.rgbaTransport=await openVideoRgbaTransport(realm,
      (packet,encoded)=>encoded?realm.writer.send({...packet,type:"encoded"}):invoke("__opencutVisualRgba",[packet]),"http://127.0.0.1:49199");
    const endpoint=realm.rgbaTransport.endpoint;
    // Only this fresh capability may leave the captured module registry.
    // All other HTTP/WebSocket requests retain the existing deny-by-default routes.
    realm.fixedModules.csp=realm.fixedModules.csp.replace("connect-src 'none'",`connect-src ${endpoint}/`);
    await realm.context.grantPermissions(["local-network-access"],{origin:"http://127.0.0.1:49199"});
    // Fetch interception serializes POST bodies through CDP even when continued.
    // Block all other URLs in Chromium itself; only module requests need the
    // JavaScript route; encoded output crosses this capability once per window.
    const network=await realm.context.newCDPSession(realm.page);
    await network.send("Network.enable",{maxPostDataSize:0});
    await network.send("Network.setBlockedURLs",{urls:["*"],urlPatterns:[
      {urlPattern:`${ORIGIN}/*`,block:false},{urlPattern:`${endpoint}/*`,block:false}]});
    await realm.context.unroute("**/*");
    await realm.context.route(`${ORIGIN}/**`,route=>{
      const response=moduleResponse(realm.fixedModules,route.request().url(),route.request().method());
      return response?route.fulfill(response):route.abort("blockedbyclient");
    });

  },
  async finishCanvas(realm,count) {
    return realm.page.evaluate(async({count,address})=>{
      const output=globalThis.__opencutCanvasVideo;if(!output)throw Error("Missing canvas encoder");
      const blob=await output.finish(count);
      const response=await fetch(address,{method:"POST",headers:{"Content-Type":"application/octet-stream"},body:blob});
      if(!response.ok){const error=await response.json();throw Object.assign(Error("Encoded output rejected"),{code:error.code});}
      return blob.size;
    },{count,address:`${realm.rgbaTransport.endpoint}/encoded`});
  },
  async render(realm) {
    realm.resumeVideoReads?.();
    const result=await realm.page.evaluate(executeVideoFrame,{profileTimings:!!process.env.OPENCUT_VIDEO_PROFILE_DIR,canvasEncoding:realm.begin.canvasEncoding,frameIndex:realm.currentFrame.frameIndex,plan:realm.currentFrame.plan,admission:realm.begin.admission,profile:realm.currentFrame.type,outputUrl:`${realm.rgbaTransport.endpoint}/${realm.currentFrame.frameIndex}`,resourceUrl:`${realm.rgbaTransport.endpoint}/resource/${realm.currentFrame.frameIndex}`});
    await realm.parkVideoReads?.();
    if(result.timings&&realm.currentFrame.frameIndex<50)await fs.writeFile(`${process.env.OPENCUT_VIDEO_PROFILE_DIR}/direct-${process.pid}-${realm.currentFrame.frameIndex}.json`,JSON.stringify({...result.timings,...realm.rgbaTransport.stats,postDataBytes:realm.uploadPostDataBytes}));
    if(result?.ok!==true)reject(result?.error?.code??"VISUAL_BROWSER_EXECUTION_FAILED","Persistent video frame failed");
    return result.receipt;
  }
};
export async function executeVideoFrame({plan,admission,outputUrl,resourceUrl,canvasEncoding=null,frameIndex=0,profileTimings=false,profile="beginTextFrame"}) {
  try {
    const module=globalThis.__opencutVisualModule;
    globalThis.__opencutVideoBytes??=new Map();
    globalThis.__opencutVideoFonts??=new Map();
    globalThis.__opencutVideoDecoders??=new module.VisualDecoderCache();
    const render={beginTextFrame:module.renderClassicTextFrame,beginCaptionFrame:module.renderClassicCaptionFrame,beginGraphicFrame:module.renderClassicGraphicFrame,beginBlendFrame:module.renderClassicBlendFrame,beginMaskFrame:module.renderClassicMaskFrame,beginSceneFrame:module.renderClassicSceneFrame}[profile];
    if(typeof render!=="function")return {ok:false,error:{code:"INVALID_HOST_PROTOCOL"}};
    // One bounded frame crosses the browser boundary as binary, then the
    // original handler/native sink validates every canonical 64KiB chunk.
    const expected=plan.identity.width*plan.identity.height*4;
    const timings={};let callbackMs=0;
    const video=canvasEncoding?(globalThis.__opencutCanvasVideo??=await module.createCanvasVideo(plan.identity,canvasEncoding)):null;
    let offset=0,batch=[];
    const flush=async()=>{if(batch.length){const packets=batch;batch=[];await globalThis.__opencutVisualRgbaBatch(packets);}};
    const callback=resourceUrl?async(op,args)=>{
      const response=await fetch(`${resourceUrl.replace(/\/resource\/[0-9]+$/,"/callback")}`,{method:"POST",headers:{"Content-Type":"text/plain"},body:JSON.stringify({op,args})});
      const result=await response.json();if(!response.ok)throw Object.assign(Error("Fixed callback rejected"),{code:result.code});return result;
    }:globalThis.__opencutVisualCallback;
    const host={admission,rangePrefetch:false,requireResolveCached:true,rangeCacheLimit:512*1024,
      ...(video?{accountOutputBytes:reserve=>video.account(reserve),completeOutput:()=>video.add(plan.identity,frameIndex)}:{outputBufferBytes:16*65536*16}),
      ...(video?{surface:video.canvas}:{}),
      ...(resourceUrl?{readVerifiedResource:async(transferId)=>{
        const response=await fetch(`${resourceUrl}/${encodeURIComponent(transferId)}`);
        if(!response.ok){const error=await response.json();throw Object.assign(Error("Fixed resource rejected"),{code:error.code});}
        return new Uint8Array(await response.arrayBuffer());
      }}:{}),
      observeExecution:value=>Object.assign(timings,value),callback:async(op,args)=>{const start=performance.now();try{return await callback(op,args);}finally{callbackMs+=performance.now()-start;}},rgba:async(at,bytes)=>{
        if(at!==offset||bytes.length<1||bytes.length>65536||bytes.length%4||bytes.length>expected-offset)
          throw Object.assign(Error("Invalid bounded RGBA output"),{code:"INVALID_OUTPUT_RGBA"});
        // Reuse the original bounded native handler. Each acknowledged 64KiB
        // chunk retires before the GPU reuses its scratch buffer. A full-frame
        // POST/Blob also duplicates its body through CDP network observation.
        batch.push({offsetBytes:String(at),bytesHex:module.encodeRgbaHex(bytes)});offset+=bytes.length;
        if(batch.length===16)await flush();
      }};
    const receipt=await render(plan,host,globalThis.__opencutVisualGpu,undefined,globalThis.__opencutVideoBytes,globalThis.__opencutVideoDecoders,globalThis.__opencutVideoFonts);
    if(video){return {ok:true,receipt,...(profileTimings?{timings:{...timings,callbackMs}}:{})};}
    if(offset!==expected)throw Object.assign(Error("Incomplete RGBA output"),{code:"INVALID_OUTPUT_RGBA"});
    await flush();
    return {ok:true,receipt,...(profileTimings?{timings:{...timings,callbackMs}}:{})};
  } catch(error) {return {ok:false,error:{code:error.code??"VISUAL_BROWSER_EXECUTION_FAILED"},...(profileTimings&&error.code==="VISUAL_BUDGET_EXCEEDED"?{timings:{admissionError:error.message}}:{})};}
}
