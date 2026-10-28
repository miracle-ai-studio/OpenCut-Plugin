import { openVerifiedRangeSource, rangeCacheBytes } from "@opencut/classic-audio-renderer/verified-range-source";
export { prepareClassicAssSubtitle } from "./classic-subtitle-layout.mjs";
export { renderClassicGraphicFrame } from "./graphic-frame.mjs";
export { renderClassicBlendFrame } from "./blend-frame.mjs";
export { renderClassicMaskFrame } from "./mask-frame.mjs";
export { prepareCaptionText } from "./caption-layout.mjs";
export { renderClassicCaptionFrame } from "./caption-frame.mjs";
import { decodeImage, decodeVideo } from "./decoder.mjs";
export { VisualDecoderCache } from "./decoder.mjs";
export { measureClassicText } from "./text-layout.mjs";
export { enableImmutablePreviewFonts } from "./text-layout.mjs";
import { openGpuRealm, closeGpuRealm, clearGpuFrameCaches, renderGpuFrame } from "./gpu.mjs";
import { prepareGraphicSources, clearGraphicSourceCache, retainedGraphicRgbaBytes } from "./graphic-source.mjs";
import { createFrameFontChecks } from "./text-layout.mjs";
import { hasFontHashWorker } from "./font-hash.mjs";
import { byteBudget, checkAbort, integer, natural, reject, sameJsonFacts } from "./validation.mjs";
import { composeTextFrame, validateTextFramePlan, retainTextFrameFonts, fontResource } from "./text-frame.mjs";
export { openGpuRealm, closeGpuRealm };
const clone = value => structuredClone(value);
const preparedFontFrames = new WeakMap();
const fontKey = font => `${font.fontId}:${font.contentHash}:${font.size}`;
const frameScope = identity => [identity.projectId, identity.revision, identity.sequenceId, identity.width, identity.height];

/** Browser-only preparation within ONE render invocation. The caller obtains a
 * fresh authoritative plan afterwards. No bound frame, clock or grant is cached.
 * Existing retained resources remain charged while private font snapshots live. */
export async function preparePreviewFontFrame(fonts, identity, admission, realm, signal, cache, decoders, fontCache) {
  checkAbort(signal);
  if (!hasFontHashWorker(realm) || !fonts.length || fonts.length > 32 ||
      fonts.some(font => !fontCache.has(fontKey(font)))) return null;
  const entries = new Map(fonts.map(font => [fontKey(font), { item: fontCache.get(fontKey(font)), font: clone(font) }]));
  const budget = byteBudget(admission.maxRgbaBytes), releases = [];
  let checks;
  try {
    const retained = [...cache?.values() ?? [], ...[...fontCache.values()].map(item => item.bytes)];
    const encoded = retained.reduce((sum, bytes) => sum + bytes.byteLength, 0);
    if (encoded + (decoders?.retainedEncodedBytes?.() ?? 0) > Number(admission.maxEncodedBytes)) return null;
    for (const bytes of retained) releases.push(budget.reserve(bytes.byteLength * 3));
    releases.push(budget.reserve(decoders?.retainedBytes?.() ?? 0));
    releases.push(budget.reserve(decoders?.retainedEncodedBytes?.() ?? 0));
    releases.push(budget.reserve(retainedGraphicRgbaBytes(realm)));
    releases.push(budget.reserve(identity.width * identity.height * 4));
    checks = createFrameFontChecks(signal, realm);
    checks.start(fonts.map(font => ({ font })), fontCache);
    await checks.verify();
    checkAbort(signal);
    const token = Object.freeze({});
    const record = { checks, realm, signal, fontCache, entries, scope: frameScope(identity), admission: clone(admission) };
    preparedFontFrames.set(token, record);
    return { token, async close() {
      preparedFontFrames.delete(token);
      await checks.close();
    } };
  } catch (error) {
    await checks?.close();
    // A previous retained set may be larger than the newly sampled frame. The
    // ordinary executor evicts it before admission; no snapshot was allocated
    // when reservation failed, so preserve that existing route.
    if (!checks && error?.code === "VISUAL_BUDGET_EXCEEDED") return null;
    throw error;
  } finally { for (const release of releases) release(); }
}
function takePreparedFontFrame(token, identity, admission, realm, signal, fontCache) {
  const prepared = preparedFontFrames.get(token);
  preparedFontFrames.delete(token);
  if (!prepared || prepared.realm !== realm || prepared.signal !== signal || prepared.fontCache !== fontCache ||
      !sameJsonFacts(prepared.scope, frameScope(identity)) || !sameJsonFacts(prepared.admission, admission) ||
      prepared.entries.size !== fontCache.size || [...prepared.entries].some(([key, entry]) => fontCache.get(key) !== entry.item))
    reject("INVALID_VISUAL_PLAN", "Prepared font resources changed scope, admission or retained set");
  // tokenFor checks the original byte reference as well as full font identity.
  for (const { item, font } of prepared.entries.values()) prepared.checks.tokenFor(font, item.bytes);
  return prepared.checks;
}
async function encodedResource(resource,host,budget,signal,operation="resolveVerifiedBytes",argument="resource") {
  const expected=natural(resource.size,"encoded resource size");
  if(expected===0n||expected>BigInt(host.admission.maxEncodedBytes)||expected>BigInt(Number.MAX_SAFE_INTEGER)) reject("VISUAL_BUDGET_EXCEEDED","Encoded resource exceeds admission");
  const release=budget.reserve(Number(expected)*3);let transfer;
  try {
    checkAbort(signal);transfer=await host.callback(operation,{[argument]:resource});checkAbort(signal);
    if(natural(transfer.byteLength,"verified byte length")!==expected) reject("INVALID_VISUAL_PLAN","Verified byte length changed");
    const direct=typeof host.readVerifiedResource==="function";
    const bytes=direct?await host.readVerifiedResource(transfer.transferId,Number(expected)):new Uint8Array(Number(expected));
    if(!(bytes instanceof Uint8Array)||!(bytes.buffer instanceof ArrayBuffer)||bytes.byteLength!==Number(expected))reject("INVALID_VISUAL_PLAN","Fixed resource bytes changed length or storage");
    let offset=direct?bytes.length:0;
    while(offset<bytes.length) {
      checkAbort(signal);const count=Math.min(65536,bytes.length-offset);
      if(typeof host.readVerifiedBytes==="function") {
        // Typed in-process adapter; the original cross-process hex protocol is
        // unchanged. Copy before hash/use, within the same admitted 3N budget.
        const chunk=await host.readVerifiedBytes(transfer.transferId,offset,count);checkAbort(signal);
        if(!(chunk instanceof Uint8Array)||!(chunk.buffer instanceof ArrayBuffer)||chunk.byteLength!==count)reject("INVALID_VISUAL_PLAN","Verified typed resource chunk changed length or storage");
        bytes.set(chunk,offset);
      } else {
        const chunk=await host.callback("readVerifiedBytes",{transferId:transfer.transferId,offset:String(offset),count});
        checkAbort(signal);
        if(chunk.offset!==String(offset)||typeof chunk.bytesHex!=="string"||chunk.bytesHex.length!==count*2||!/^(?:[a-f0-9]{2})+$/.test(chunk.bytesHex)||chunk.eof!==(offset+count===bytes.length)) reject("INVALID_VISUAL_PLAN","Verified resource chunks changed length or order");
        for(let n=0;n<count;n++)bytes[offset+n]=Number.parseInt(chunk.bytesHex.slice(n*2,n*2+2),16);
      }
      offset+=count;
    }
    const digest=new Uint8Array(await crypto.subtle.digest("SHA-256",bytes));checkAbort(signal);
    const hash=Array.from(digest,b=>b.toString(16).padStart(2,"0")).join("");
    if(hash!==resource.contentHash) reject("ASSET_HASH_MISMATCH","Verified visual bytes changed identity");
    return {bytes,release};
  } catch(error) {release();throw error;}
  finally {if(transfer)await host.callback("releaseVerifiedBytes",{transferId:transfer.transferId});}
}
/** Warm one newly visible video through the same verified range and decoder
 * adapters. No compositor, bound picture, source clock or presentation credit. */
export async function prewarmVideoSources(plan,permits,host,signal,decoders,activeKeys) {
  const budget=byteBudget(host.admission.maxRgbaBytes),encoded=byteBudget(String(host.admission.maxEncodedBytes));
  const rangeBudget={reserve(bytes){const release=budget.reserve(bytes);try{const done=encoded.reserve(bytes);return()=>{done();release();};}catch(error){release();throw error;}}};
  const key=r=>`${r.assetId}:${r.contentHash}:${r.size}`;
  // A single future decoder is enough to hide a new topmost source opening;
  // its four-surface admission and encoded ranges share one bounded allowance.
  const layer=[...plan.layers].reverse().find(l=>l.decode.kind==="video"&&!l.decode.noFrameReason&&!activeKeys.has(key(l.resource)));
  if(!layer)return false;
  checkAbort(signal);const resource=layer.resource,permit=permits[layer.elementId];
  if(!permit)reject("INVALID_VISUAL_PLAN","Missing cut decode admission");
  const source=await openVerifiedRangeSource(resource,{
    open:fixed=>host.callback("resolveVerifiedBytes",{resource:fixed}),
    async read(transferId,offset,count){const bytes=await host.readVerifiedBytes(transferId,offset,count);return {offset:String(offset),bytes,eof:offset+count===Number(resource.size)};},
    release:transferId=>host.callback("releaseVerifiedBytes",{transferId}),
  // Chunk width participates in the verified descriptor's identity; preserve
  // the native host's admitted transport contract during speculative reads.
  },rangeBudget,512*1024,host.rangeSignal??signal,{prefetch:true,maximumChunkBytes:host.rangeChunkBytes??65536,readAhead:host.rangeReadAhead===true});
  decoders.keepRange(key(resource),source);
  const allocation=await decoders.prepare(key(resource),source,permit,signal);
  if(!Number.isSafeInteger(allocation)||allocation<1||BigInt(allocation)>natural(permit.maxDecodedBytes,"decoded byte cap"))reject("VISUAL_BUDGET_EXCEEDED","Cut decoder exceeds native admission");
  budget.reserve(allocation);
  await decoders.decode(key(resource),source,layer.decode,permit,signal);checkAbort(signal);
  return true;
}
export async function renderClassicFrame(fixedPlan,host,realm,signal,cache,decoders) {
  return renderFrame(fixedPlan,host,realm,signal,cache,decoders);
}
export async function renderClassicTextFrame(fixedPlan,host,realm,signal,cache,decoders,fontCache) {
  checkAbort(signal);const plan=clone(fixedPlan);validateTextFramePlan(plan,host.admission);
  retainTextFrameFonts(fontCache, plan.texts.map(t => t.font));
  return renderFrame(plan.media,host,realm,signal,cache,decoders,state=>composeTextFrame(plan,host,state,signal),fontCache,undefined,plan.texts.map(t=>fontResource(t.font)));
}
export async function renderFrame(fixedPlan,host,realm,signal,cache,decoders,compose,fontCache,graphicPlan,fixedFonts) {
  const sources=new Map(),encoded=new Map(),decodedSources=new Map(),borrowedRequests=new Map(),canvases=new Set(),decoded=[],releases=[];
  const timings = {}, began = performance.now();
  let stage = began;
  let retainedDecoded = 0;
  let frameFontChecks;
  let surfaceFrame;
  try {
  checkAbort(signal);const plan=clone(fixedPlan),admission=host.admission;
  const width=integer(plan.identity.width,"frame width",1,16384),height=integer(plan.identity.height,"frame height",1,16384);
  const rgba=BigInt(width)*BigInt(height)*4n;
  if(plan.identity.profile!=="classic-v31-gpu-normal-frame"||BigInt(width)*BigInt(height)>natural(admission.maxFramePixels,"frame pixels")||rgba>natural(admission.maxRgbaBytes,"RGBA bytes")||plan.layers.length>60)reject("INVALID_VISUAL_PLAN","Fixed visual frame exceeds admission");
  const budget=byteBudget(admission.maxRgbaBytes);
  // Retained graphics count even while media/fonts are being resolved. Reserve
  // the entire current set once, before any fresh source allocation.
  const preparedGraphics=graphicPlan?prepareGraphicSources(realm,graphicPlan,admission):undefined;
  if(!graphicPlan)clearGraphicSourceCache(realm);
  for(const layout of preparedGraphics?.layouts??[])releases.push(budget.reserve(Math.max(layout.peakRgbaBytes,layout.width*layout.height*16)));
  // Output readback is admitted separately from all decoded source canvases.
  releases.push(budget.reserve(Number(rgba)));
  if(host.outputBufferBytes!==undefined)releases.push(budget.reserve(integer(host.outputBufferBytes,"output bridge bytes",0)));
  if(host.accountOutputBytes)releases.push(host.accountOutputBytes(bytes=>budget.reserve(bytes)));
  const resourceKey = r => `${r.assetId}:${r.contentHash}:${r.size}`;
  const encodedBudget = byteBudget(String(integer(admission.maxEncodedBytes, "encoded byte admission", 1)));
  const rangeBudget = { reserve(bytes) {
    const release = budget.reserve(bytes);
    try { const encodedRelease = encodedBudget.reserve(bytes); return () => { encodedRelease(); release(); }; }
    catch (error) { release(); throw error; }
  } };
  const videoCount = plan.resources.filter(r => r.kind === "video").length;
  const wholeBytes = plan.resources.filter(r => r.kind !== "video").reduce((n, r) => n + Number(r.size), 0)
    + [...new Map((fixedFonts ?? graphicPlan?.execution.execution.texts.map(t=>fontResource(t.font)) ?? []).map(f=>[f.fontId,f])).values()].reduce((n, f) => n + Number(f.size), 0);
  // Keep demux caches modest so ordinary multi-layer HD frames retain space
  // for decoder surfaces, text and effects under the original total admission.
  const maximumChunkBytes = host.rangeChunkBytes ?? 65536;
  const cacheBytes = videoCount ? Math.min(host.rangeCacheLimit??2 * 1024 * 1024, rangeCacheBytes(Number(admission.maxEncodedBytes) - wholeBytes, Math.min(32, videoCount + 1), maximumChunkBytes, host.rangeReadAhead === true)) : 0;
  await decoders?.retain(new Set(plan.layers.filter(l=>l.decode.kind==="video"&&!l.decode.noFrameReason).map(l=>resourceKey(l.resource))), cacheBytes);
  releases.push(rangeBudget.reserve(decoders?.retainedEncodedBytes?.() ?? 0));
  const openVideo = resource => openVerifiedRangeSource(resource, {
    open: fixed => host.callback("resolveVerifiedBytes", { resource: fixed }),
    async read(transferId, offset, count) {
      if (typeof host.readVerifiedBytes === "function") {
        const bytes = await host.readVerifiedBytes(transferId, offset, count);
        return { offset: String(offset), eof: offset + count === Number(resource.size), bytes };
      }
      return host.callback("readVerifiedBytes", { transferId, offset: String(offset), count });
    },
    release: transferId => host.callback("releaseVerifiedBytes", { transferId }),
  }, rangeBudget, cacheBytes, host.rangeSignal ?? signal, { prefetch: host.rangePrefetch !== false, maximumChunkBytes, readAhead: host.rangeReadAhead === true });
  retainedDecoded = decoders?.retainedBytes() ?? 0;
  releases.push(budget.reserve(retainedDecoded));
  // Optional session cache contains verified immutable encoded bytes only.
  // Its complete retained set is charged to every frame; changed resource sets
  // evict obsolete entries before admission. The optional decoded pool above
  // is independently retained/accounted; native one-shot canvases still release.
  if(cache) {
    const keys=new Set(plan.resources.filter(r=>r.kind!=="video").map(r=>`${r.assetId}:${r.contentHash}:${r.size}`));
    for(const key of cache.keys())if(!keys.has(key))cache.delete(key);
    for(const bytes of cache.values()) { releases.push(budget.reserve(bytes.byteLength*3)); releases.push(encodedBudget.reserve(bytes.byteLength)); }
  }
  if(fontCache)for(const item of fontCache.values()) { releases.push(budget.reserve(item.bytes.byteLength*3)); releases.push(encodedBudget.reserve(item.bytes.byteLength)); }
  if(host.preparedFontFrame) {
    frameFontChecks=takePreparedFontFrame(host.preparedFontFrame,plan.identity,admission,realm,signal,fontCache);
  } else if(graphicPlan && fontCache && hasFontHashWorker(realm)) {
    // An offscreen surface is private until this function completes. Previously
    // verified immutable FontFaces can draw while fresh byte checks run, but no
    // pixel/hit observation may leave this frame before every check succeeds.
    frameFontChecks=createFrameFontChecks(signal,realm,{deferLoadedFaces:typeof OffscreenCanvas==="function" && host.surface instanceof OffscreenCanvas});
    frameFontChecks.start(graphicPlan.execution.execution.texts,fontCache);
  }
    for(const layer of plan.layers) {
      checkAbort(signal);const permit=clone(await host.callback("admitDecode",{elementId:layer.elementId}));checkAbort(signal);
      const decode=layer.decode;
      if(decode.noFrameReason!==undefined&&decode.noFrameReason!==null) {
        if(decode.noFrameReason!=="outside_read_window")reject("INVALID_VISUAL_PLAN","Unknown prepared no-frame fact");
        decoded.push({kind:"noFrame",elementId:layer.elementId,requestedTime:clone(decode.requestedTime),reason:"outside_read_window"});continue;
      }
      let encodedSource=encoded.get(layer.resource.assetId);
      if(!encodedSource && decode.kind !== "video") {
        const key=`${layer.resource.assetId}:${layer.resource.contentHash}:${layer.resource.size}`,hit=cache?.get(key);
        if(hit) {
          if(host.requireResolveCached)await verifyCachedResource(layer.resource,host,signal);
          encodedSource={bytes:hit,release:()=>{}};
        }
        else {
          encodedSource=await encodedResource(layer.resource,host,budget,signal);
          releases.push(encodedBudget.reserve(encodedSource.bytes.byteLength));
          if(cache) {
            const retained=[...cache.values()].reduce((n,b)=>n+b.byteLength,0)+encodedSource.bytes.byteLength;
            if(retained>Number(admission.maxEncodedBytes)){encodedSource.release();reject("VISUAL_BUDGET_EXCEEDED","Retained preview bytes exceed admission");}
            cache.set(key,encodedSource.bytes);
          }
        }
        encoded.set(layer.resource.assetId,encodedSource);
      }
      // Admission is checked before the decoder allocates; the cap remains
      // reserved until actual output dimensions replace it with factual bytes.
      const sourceKey = `${resourceKey(layer.resource)}:${JSON.stringify([decode, permit])}`;
      let source = decodedSources.get(sourceKey);
      if (!source) {
      const key = resourceKey(layer.resource);
      // Keep at most one borrowed observation per resource in a composed frame.
      // A different time or permit must not recycle/dispose an earlier layer's
      // pool canvas before all layers have finished uploading to the GPU.
      const usePool = decoders && decode.kind === "video" && (!borrowedRequests.has(key) || borrowedRequests.get(key) === sourceKey);
      if (usePool) borrowedRequests.set(key, sourceKey);
      const cap=natural(permit.maxDecodedBytes,"decoded byte cap");
      if(cap>BigInt(Number.MAX_SAFE_INTEGER))reject("VISUAL_BUDGET_EXCEEDED","Decoded admission cannot be allocated safely");
      // Metadata is probed before any CanvasSink canvas is materialized. Its
      // fixed display geometry is still checked against the fresh native cap;
      // reserve the admitted transfer surfaces at that geometry instead of the policy maximum.
      const retainedBefore=usePool?decoders.retainedBytes(key):0;
      let pooledRange=null,allocation=Number(cap);
      if(usePool&&typeof decoders.prepare==="function") {
        if(decode.selectedVideoStream!==null)reject("UNSUPPORTED_RENDER_FEATURE","Only primary video selection is supported");
        pooledRange=decoders.range(key);
        const openedAt=performance.now();
        if(!pooledRange){pooledRange=await openVideo(layer.resource);decoders.keepRange(key,pooledRange);}
        allocation=await decoders.prepare(key,pooledRange,permit,signal);
        if(!Number.isSafeInteger(allocation)||allocation<1||allocation>Number(cap))reject("VISUAL_BUDGET_EXCEEDED","Decoded geometry exceeds native allocation admission");
        timings.sourceOpenMs=String(Number(timings.sourceOpenMs??0)+performance.now()-openedAt);
      }
      const releaseCap=budget.reserve(Math.max(0,allocation-retainedBefore));
      try {
        if (decode.kind === "video") {
          let range = pooledRange??(usePool ? decoders.range(key) : null);
          const openedAt = performance.now();
          if (!range) {
            range = await openVideo(layer.resource);
            if (usePool) decoders.keepRange(key, range);
          }
          timings.sourceOpenMs = String(Number(timings.sourceOpenMs ?? 0) + performance.now() - openedAt);
          const decodedAt = performance.now();
          try {
            source = usePool ? await decoders.decode(key, range, decode, permit, signal) : await decodeVideo(range, decode, permit, signal);
          } finally { if (!usePool) await range.close(); }
          timings.sourceDecodeMs = String(Number(timings.sourceDecodeMs ?? 0) + performance.now() - decodedAt);
        } else if (decode.kind === "image") source = await decodeImage(encodedSource.bytes, permit, signal);
        else reject("UNSUPPORTED_RENDER_FEATURE", "Unknown visual source");
        if (source.canvas && !source.borrowed) canvases.add(source.canvas);
      }
      finally {releaseCap();}
      const retained = decoders?.retainedBytes() ?? 0;
      if (retained > retainedDecoded) releases.push(budget.reserve(retained - retainedDecoded));
      retainedDecoded = retained;
      decodedSources.set(sourceKey, source);
      if (!source.reason) {
        sources.set(layer.elementId, source);
        if (!source.borrowed) releases.push(budget.reserve(source.width*source.height*(permit.decoderSurfaces??4)*4));
      }
      }
      checkAbort(signal);
      if(source.reason) {decoded.push({kind:"noFrame",elementId:layer.elementId,requestedTime:clone(decode.requestedTime),reason:source.reason});continue;}
      sources.set(layer.elementId,source);
      decoded.push({kind:decode.kind==="image"?"readyImage":"readyVideo",elementId:layer.elementId,width:source.width,height:source.height,...(decode.kind==="video"?{requestedTime:clone(decode.requestedTime),selectedVideoStream:null,actualTimestampSeconds:source.actualTimestampSeconds,frameDurationSeconds:source.frameDurationSeconds,...(source.eofHeld?{eofHeld:true}:{})}:{})});
    }
    timings.decodeMs = String(performance.now() - stage); stage = performance.now();
    checkAbort(signal);let bound=clone(await host.callback("bindFrame",{decoded:clone(decoded)}));checkAbort(signal);
    timings.mediaBindMs = String(performance.now() - stage); stage = performance.now();
    if(!sameJsonFacts(bound.identity,plan.identity)||!sameJsonFacts(bound.decoded,decoded))reject("INVALID_VISUAL_PLAN","Bound frame changed fixed identity or decoder facts");
    const frameFonts=new Map();
    if(compose)bound=await compose({bound,sources,budget,canvases,releases,encoded,retainedDecoded,fontCache,preparedGraphics,frameFontChecks,resolveFont:async resource=>{
      const key=`${resource.fontId}:${resource.contentHash}:${resource.size}`,hit=fontCache?.get(key);
      const prior=frameFonts.get(resource.fontId);if(prior){if(!sameJsonFacts(prior.resource,resource))reject("INVALID_VISUAL_PLAN","Shared frame font resource changed");return prior.loaded;}
      if(hit) {await verifyCachedResource(resource,host,signal,"resolveVerifiedFontBytes","font");const loaded={bytes:hit.bytes,release:()=>{}};frameFonts.set(resource.fontId,{resource:clone(resource),loaded});return loaded;}
      const loaded=await encodedResource(resource,host,budget,signal,"resolveVerifiedFontBytes","font");
      releases.push(encodedBudget.reserve(loaded.bytes.byteLength));
      if(fontCache) {
        const total=[...fontCache.values()].reduce((n,v)=>n+v.bytes.byteLength,0)+[...cache?.values()??[]].reduce((n,v)=>n+v.byteLength,0)+loaded.bytes.byteLength;
        if(total>Number(admission.maxEncodedBytes)){loaded.release();reject("VISUAL_BUDGET_EXCEEDED","Retained fonts exceed encoded admission");}
        fontCache.set(key,{bytes:loaded.bytes});
      }
      frameFonts.set(resource.fontId,{resource:clone(resource),loaded});return loaded;
    }});
    timings.composeMs = String(performance.now() - stage);
    await host.beforeGpu?.();
    stage = performance.now();
    const gpu = await renderGpuFrame(bound,sources,admission,async(offset,bytes)=>{checkAbort(signal);await host.rgba(offset,bytes);checkAbort(signal);},signal,realm,host.surface,host.hitQuery,typeof host.captureSurface==="function");
    surfaceFrame=gpu.surfaceFrame;
    timings.gpuMs = String(performance.now() - stage);
    await frameFontChecks?.verify();
    await host.completeOutput?.();
    if(gpu?.hitTest)host.observeHitTest?.(gpu.hitTest);
    const {hitTest: _hitTest,surfaceFrame:_surfaceFrame,...executionGpu}=gpu??{};
    host.observeExecution?.({ ...executionGpu, peakAccountedBytes: String(budget.peak), retainedDecodedBytes: String(retainedDecoded), retainedEncodedBytes: String([...cache?.values() ?? []].reduce((n,b)=>n+b.byteLength,0) + (decoders?.retainedEncodedBytes?.() ?? 0)) });
    checkAbort(signal);
    if(surfaceFrame){host.captureSurface(surfaceFrame);surfaceFrame=null;}
    return {identity:clone(compose?bound.identity:plan.identity),engineIdentity:compose?bound.identity.profile:"classic-v31-gpu-normal-frame",bound,rgbaBytes:host.surface||host.captureSurface ? "0" : String(rgba),peakAccountedBytes:String(budget.peak)};
  } catch(error) {
    clearGpuFrameCaches(realm);
    // A consumer-clock rejection retires this picture, not its immutable
    // sources. Closing the decoder here makes every retry pay the same cold
    // seek cost, so a single late frame can prevent playback from catching up.
    // The next frame still obtains fresh admission and binding from the host.
    // Authority, generation, decode and allocation failures retain full cleanup.
    if (error?.code !== "PREVIEW_FRAME_STALE" || signal?.aborted)
      await decoders?.close();
    throw error;
  } finally {
    surfaceFrame?.close();
    host.observeExecution?.({ ...timings, executionMs: String(performance.now() - began) });
    await frameFontChecks?.close();
    for(const canvas of canvases){if(typeof canvas.close==='function')canvas.close();else{canvas.width=0;canvas.height=0;}}
    for(const item of encoded.values())item.release();for(const release of releases)release();
  }
}

async function verifyCachedResource(resource,host,signal,operation="resolveVerifiedBytes",argument="resource") {
  checkAbort(signal);const transfer=await host.callback(operation,{[argument]:resource});
  try {if(natural(transfer.byteLength,"cached verified bytes")!==natural(resource.size,"cached resource size"))reject("INVALID_VISUAL_PLAN","Cached resource identity changed");checkAbort(signal);}
  finally {await host.callback("releaseVerifiedBytes",{transferId:transfer.transferId});}
}

export { renderClassicSceneFrame } from "./scene-frame.mjs";
export { encodeRgbaHex } from "./validation.mjs";
export { PreviewSegmentDecoder } from "./preview-segment.mjs";
export async function createCanvasVideo(...args) {
  const encoder = await import("./canvas-video.mjs");
  return encoder.createCanvasVideo(...args);
}

export {gpuSurfacePresenter,gpuSurfaceBitmap} from "./gpu-surface.mjs";
