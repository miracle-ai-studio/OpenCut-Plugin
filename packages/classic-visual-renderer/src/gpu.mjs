import vertexSource from "/renderer/vendor/classic-v31/fullscreen.wgsl";
import layerSource from "/renderer/vendor/classic-v31/layer.wgsl";
import blendSource from "/renderer/vendor/classic-v31/blend.wgsl";
import maskSource from "/renderer/vendor/classic-v31/mask.wgsl";
import jfaInitSource from "/renderer/vendor/classic-v31/jfa_init.wgsl";
import jfaStepSource from "/renderer/vendor/classic-v31/jfa_step.wgsl";
import blurSource from "/renderer/vendor/classic-v31/gaussian_blur.wgsl";
import { sceneFrameSources, rasterizeSceneMask, rasterizeSceneStroke, releaseSceneSources } from "./scene-source.mjs";
import { legacyColorPasses, legacyColorShader } from "./legacy-color.mjs";
import { sceneEffectPasses } from "./scene-effects.mjs";
import jfaDistanceSource from "/renderer/vendor/classic-v31/jfa_distance.wgsl";
import { byteBudget, checkAbort, finite, integer, reject } from "./validation.mjs";
import { blendModeIndex } from "./blend-modes.mjs";
import { clearGraphicSourceCache, isRetainedGraphicSource } from "./graphic-source.mjs";
import { clearFontHashWorker, enableFontHashWorker } from "./font-hash.mjs";
import { maskFrameSources, rasterizeMaskSource, releaseMaskFrameSources } from "./mask-source.mjs";
import {captureGpuSurfaceFrame,validateGpuSurfaceFrame} from "./gpu-surface.mjs";

const FORMAT = "bgra8unorm";
const QUAD = new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]);
// Like Classic's TextureStore/TexturePool, retain only the current sources and
// three same-size working targets. This realm-local state never carries binds.
const frameCaches = new WeakMap();
export function clearGpuFrameCaches(realm) {
  if (!realm) return;
  clearFontHashWorker(realm);
  const cache = frameCaches.get(realm);
  if (cache) {
    for (const { texture } of cache.sources.values()) texture.destroy();
    for (const texture of cache.targets?.textures ?? []) texture.destroy();
    for (const texture of cache.masks.values()) texture.destroy();
    for (const texture of cache.maskTargets?.textures ?? []) texture.destroy();
    frameCaches.delete(realm);
  }
  clearGraphicSourceCache(realm);
}
export function closeGpuRealm(realm) {
  if (!realm || realm.closed) return;
  realm.closed = true;
  clearGpuFrameCaches(realm);
  realm.device.destroy();
}
function uniformBytes(quad, canvas) {
  // Exact Classic LayerUniforms layout: 10 floats plus 8-byte padding.
  return new Float32Array([canvas.width, canvas.height, quad.centerX, quad.centerY,
    quad.width, quad.height, quad.rotationRadians, quad.opacity,
    quad.flipX ? 1 : 0, quad.flipY ? 1 : 0, 0, 0]);
}
export async function openGpuRealm(options = {}) {
  if (!globalThis.navigator?.gpu) reject("CAPABILITY_UNAVAILABLE", "WebGPU is unavailable in the admitted browser");
  const adapter = await navigator.gpu.requestAdapter();
  if (!adapter) reject("CAPABILITY_UNAVAILABLE", "No WebGPU adapter is available");
  const info = adapter.info ?? await adapter.requestAdapterInfo?.() ?? {};
  const observedDevice = JSON.stringify({ vendor: info.vendor ?? "", architecture: info.architecture ?? "", device: info.device ?? "", description: info.description ?? "", ...(typeof adapter.isFallbackAdapter === "boolean" ? { fallback: adapter.isFallbackAdapter } : {}) });
  const device = await adapter.requestDevice();
  const realm = { device, observation: { gpuBackend: "webgpu", gpuDevice: observedDevice, textureFormat: FORMAT } };
  try {
    if (options.fontHashWorker === true) enableFontHashWorker(realm);
    return realm;
  } catch (error) { closeGpuRealm(realm); throw error; }
}
export async function renderGpuFrame(frame, sources, admission, emit, signal, realm, surface, hitQuery, captureSurface=false) {
  const began=performance.now();let surfaceFrame=null;
  const owned = !realm;
  if (signal?.aborted) { releaseSceneSources(frame); releaseMaskFrameSources(frame); clearGpuFrameCaches(realm); checkAbort(signal); }
  try { realm ??= await openGpuRealm(); }
  catch (error) { releaseSceneSources(frame); releaseMaskFrameSources(frame); throw error; }
  const { device } = realm;
  const allocated = [], buffers = [], releases = [];
  let readback,hitReadback; const canvases = [];
  const hitEntries=[]; let hitPoint;
  let scopes = 0;
  try {
    const budget = byteBudget(admission.maxGpuBytes);
    checkAbort(signal);
    if (realm.closed) reject("VISUAL_GPU_FAILED", "GPU realm is closed");
    const { width, height } = frame.identity;
    integer(width, "canvas width", 1, device.limits.maxTextureDimension2D);
    integer(height, "canvas height", 1, device.limits.maxTextureDimension2D);
    if(hitQuery!==undefined){if(!surface||!hitQuery||Object.keys(hitQuery).sort().join()!=="x,y")reject("INVALID_VISUAL_PLAN","Hit observations require one explicit preview point");hitPoint={x:integer(hitQuery.x,"hit X",0,width-1),y:integer(hitQuery.y,"hit Y",0,height-1)};}
    // Old bound DTOs have no mode; only those use the normal default. Validate
    // every authored entry, including null quads, before GPU allocation.
    const modeIndices = frame.layers.map(layer => blendModeIndex(layer.blendMode === undefined ? "normal" : layer.blendMode));
    let cache = frameCaches.get(realm);
    if (!cache) { cache = { sources: new Map(), targets: null, masks: new Map(), maskTargets: null }; frameCaches.set(realm, cache); }
    const sceneState = sceneFrameSources(frame);
    const maskSources = sceneState ? new Map([...sceneState.masks.values()].flat().map(source => [source.key, source])) : maskFrameSources(frame);
    const maskKeys = new Set([...maskSources.values()].map(source => source.key));
    for (const [key, texture] of cache.masks) if (!maskKeys.has(key)) { texture.destroy(); cache.masks.delete(key); }
    // Three scratch targets suffice for Classic's two independent JFA runs:
    // retain the inside result, then use the other two for outside ping/pong.
    // After distance composition one becomes the masked-layer target. Hard
    // masks only need that last target; no extra source texture is necessary.
    const maskTargetCount = maskSources.size ? ([...maskSources.values()].some(source => source.feather > 0) ? 3 : 1) : 0;
    if (cache.maskTargets && (cache.maskTargets.width !== width || cache.maskTargets.height !== height || cache.maskTargets.textures.length !== maskTargetCount)) {
      for (const texture of cache.maskTargets.textures) texture.destroy(); cache.maskTargets = null;
    }
    const active = new Set(frame.layers.filter(layer => layer.quad !== null).map(layer => sources.get(layer.elementId)));
    for (const [source, entry] of cache.sources) {
      if (!active.has(source) || !isRetainedGraphicSource(source) || source.width !== entry.width || source.height !== entry.height || source.canvas.width !== entry.width || source.canvas.height !== entry.height) {
        entry.texture.destroy(); cache.sources.delete(source);
      }
    }
    if (cache.targets && (cache.targets.width !== width || cache.targets.height !== height)) {
      for (const texture of cache.targets.textures) texture.destroy();
      cache.targets = null;
    }
    // A warm frame must pass the same admission as a cold one. Account all
    // retained GPU storage before allocating uniforms, uploads or output.
    for (const entry of cache.sources.values()) releases.push(budget.reserve(entry.width * entry.height * 4));
    if (cache.targets) releases.push(budget.reserve(width * height * 4 * 3));
    for (const _ of cache.masks.values()) releases.push(budget.reserve(width * height * 4));
    if (cache.maskTargets) releases.push(budget.reserve(width * height * 4 * maskTargetCount));
    device.pushErrorScope("out-of-memory"); device.pushErrorScope("validation"); scopes = 2;
    releases.push(budget.reserve(QUAD.byteLength));
    const vertices = device.createBuffer({ size: QUAD.byteLength, usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST });
    buffers.push(vertices); device.queue.writeBuffer(vertices, 0, QUAD);
    if(!realm.pipelines) {
      const vertex = device.createShaderModule({ code: vertexSource });
      const layer = device.createShaderModule({ code: layerSource });
      const blend = device.createShaderModule({ code: blendSource });
      const pipeline = module => device.createRenderPipeline({ layout: "auto", vertex: { module: vertex, entryPoint: "vertex_main", buffers: [{ arrayStride: 8, attributes: [{ shaderLocation: 0, offset: 0, format: "float32x2" }] }] }, fragment: { module, entryPoint: "fragment_main", targets: [{ format: FORMAT }] }, primitive: { topology: "triangle-list" } });
      const layerPipeline = pipeline(layer), blendPipeline = pipeline(blend);
      const sampler = device.createSampler({ addressModeU: "clamp-to-edge", addressModeV: "clamp-to-edge", magFilter: "linear", minFilter: "linear", mipmapFilter: "nearest" });
      realm.pipelines={layerPipeline,blendPipeline,sampler,pipeline};
    }
    const {layerPipeline,blendPipeline,sampler}=realm.pipelines;
    if (maskSources.size && !realm.maskPipelines) {
      const pipeline = realm.pipelines.pipeline;
      realm.maskPipelines = {
        mask: pipeline(device.createShaderModule({ code: maskSource })),
        init: pipeline(device.createShaderModule({ code: jfaInitSource })),
        step: pipeline(device.createShaderModule({ code: jfaStepSource })),
        distance: pipeline(device.createShaderModule({ code: jfaDistanceSource })),
        nearest: device.createSampler({ addressModeU: "clamp-to-edge", addressModeV: "clamp-to-edge", magFilter: "nearest", minFilter: "nearest", mipmapFilter: "nearest" }),
      };
    }
    function texture(w, h, retained = false) {
      integer(w, "texture width", 1, device.limits.maxTextureDimension2D); integer(h, "texture height", 1, device.limits.maxTextureDimension2D);
      releases.push(budget.reserve(w * h * 4));
      const result = device.createTexture({ size: [w, h], format: FORMAT, usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC | GPUTextureUsage.COPY_DST });
      if (!retained) allocated.push(result); return result;
    }
    function group(p, index, tex, sampling = sampler) { return device.createBindGroup({ layout: p.getBindGroupLayout(index), entries: [{ binding: 0, resource: tex.createView() }, { binding: 1, resource: sampling }] }); }
    function uniforms(p, index, bytes) {
      releases.push(budget.reserve(bytes.byteLength));
      const result = device.createBuffer({ size: bytes.byteLength, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
      buffers.push(result); device.queue.writeBuffer(result, 0, bytes);
      return device.createBindGroup({ layout: p.getBindGroupLayout(index), entries: [{ binding: 0, resource: { buffer: result } }] });
    }
    if (!cache.targets) {
      // Register each allocation immediately so a partial allocation failure
      // also destroys targets already created during this frame.
      cache.targets = { width, height, textures: [] };
      for (let n = 0; n < 3; n++) cache.targets.textures.push(texture(width, height, true));
    }
    if (maskTargetCount && !cache.maskTargets) {
      cache.maskTargets = { width, height, textures: [] };
      for (let n = 0; n < maskTargetCount; n++) cache.maskTargets.textures.push(texture(width, height, true));
    }
    let [scene, nextScene, target] = cache.targets.textures;
    const encoder = device.createCommandEncoder();
    function maskPass(pipeline, targetTexture, groups, white = false) {
      checkAbort(signal);
      const pass = encoder.beginRenderPass({ colorAttachments: [{ view: targetTexture.createView(), loadOp: "clear", storeOp: "store", clearValue: white ? { r: 1, g: 1, b: 1, a: 1 } : { r: 0, g: 0, b: 0, a: 0 } }] });
      pass.setPipeline(pipeline); pass.setVertexBuffer(0, vertices);
      groups.forEach((binding, index) => pass.setBindGroup(index, binding)); pass.draw(6); pass.end();
    }
    function maskTexture(source) {
      let result = cache.masks.get(source.key);
      if (result) return result;
      result = texture(width, height, true); cache.masks.set(source.key, result);
      const canvas = source.sceneSource ? rasterizeSceneMask(source, signal) : rasterizeMaskSource(source, signal);
      try {
        // Classic deliberately uploads straight RGB. JFA init reads red,
        // whereas the zero-feather/apply-mask path samples alpha.
        device.queue.copyExternalImageToTexture({ source: canvas, flipY: false }, { texture: result, colorSpace: "srgb", premultipliedAlpha: false }, [width, height]);
      } finally { canvas.width = 0; canvas.height = 0; }
      if (source.feather > 0) {
        const p = realm.maskPipelines, scratch = cache.maskTargets.textures;
        const run = (invert, first, second) => {
          let ping = first, pong = second;
          maskPass(p.init, ping, [group(p.init, 0, result, p.nearest), uniforms(p.init, 1, new Float32Array([width, height, invert, 0]))], true);
          const steps = Math.ceil(Math.log2(Math.max(width, height)));
          for (let step = steps - 1; step >= 0; step--) {
            maskPass(p.step, pong, [group(p.step, 0, ping, p.nearest), uniforms(p.step, 1, new Float32Array([width, height, 2 ** step, 0]))], true);
            [ping, pong] = [pong, ping];
          }
          return [ping, pong];
        };
        const [inside, spare] = run(0, scratch[0], scratch[1]);
        const [outside] = run(1, spare, scratch[2]);
        // Both init runs have consumed the uploaded mask by this point, so
        // writing its final feathered result in place is safe and bounded.
        maskPass(p.distance, result, [group(p.distance, 0, inside, p.nearest), group(p.distance, 1, outside, p.nearest), uniforms(p.distance, 2, new Float32Array([width, height, source.feather / 2, 0]))]);
      }
      return result;
    }
    const clear = encoder.beginRenderPass({ colorAttachments: [{ view: scene.createView(), loadOp: "clear", storeOp: "store", clearValue: { r: 0, g: 0, b: 0, a: 1 } }] }); clear.end();
    const normal = uniforms(blendPipeline, 2, new Uint32Array([0, 0, 0, 0]));
    const blendUniforms = new Map([[0, normal]]);
    let items = [];
    let effectTargets;
    const applyEffects = (input, effects, logicalQuad) => {
      // Classic resolves visual effects against rounded transformed logical
      // bounds. Rust supplies those f64 bounds without graphic stroke padding;
      // text, background and adjustment effects retain full-canvas dimensions.
      const effectWidth=logicalQuad?Math.round(Math.abs(logicalQuad.width)):width;
      const effectHeight=logicalQuad?Math.round(Math.abs(logicalQuad.height)):height;
      const passes = sceneEffectPasses(effects,effectWidth,effectHeight); if (!passes.length) return input;
      if (!realm.blurPipeline) realm.blurPipeline = realm.pipelines.pipeline(device.createShaderModule({ code: blurSource }));
      effectTargets ??= [texture(width,height), texture(width,height)];
      let current = input;
      for (const pass of passes) {
        const output = current === effectTargets[0] ? effectTargets[1] : effectTargets[0];
        maskPass(realm.blurPipeline, output, [group(realm.blurPipeline,0,current),uniforms(realm.blurPipeline,1,new Float32Array([width,height,...pass.direction,pass.sigma,pass.step,0,0]))]);
        current = output;
      }
      return current;
    };
    // Frozen legacy grades precede placement/opacity, as in the original
    // native filter graph. Reuse bounded ping/pong storage across equal sizes.
    const colorTargets=new Map();
    const applyLegacyColor=(input,effects,videoColorSpace,rotation=0,videoTransfer=0)=>{
      if(![0,90,180,270].includes(rotation))reject('UNSUPPORTED_RENDER_FEATURE','Unsupported decoded video orientation');
      const passes=legacyColorPasses(effects,videoColorSpace);
      if(!passes.length&&!rotation&&!videoTransfer)return input;
      if(!passes.length)passes.push(new Int32Array([4096,0,4096,0,0,0,0,1]));
      passes[0][5]=videoTransfer;passes[0][6]=rotation;
      realm.legacyColorPipeline??=realm.pipelines.pipeline(device.createShaderModule({code:legacyColorShader}));
      const pipeline=realm.legacyColorPipeline,w=rotation%180?input.height:input.width,h=rotation%180?input.width:input.height,key=`${w}:${h}`;
      let targets=colorTargets.get(key);if(!targets){targets=[texture(w,h)];colorTargets.set(key,targets);}
      let current=input;
      for(const pass of passes){const output=current===targets[0]?(targets[1]??=texture(w,h)):targets[0];
        const binding=device.createBindGroup({layout:pipeline.getBindGroupLayout(0),entries:[{binding:0,resource:current.createView()}]});
        maskPass(pipeline,output,[binding,uniforms(pipeline,1,pass)]);current=output;
      }
      return current;
    };
    const fullQuad = {centerX:width/2,centerY:height/2,width,height,rotationRadians:0,opacity:1,flipX:false,flipY:false};
    if (sceneState?.backdrop) {
      const backdrop = texture(width,height);
      device.queue.copyExternalImageToTexture({source:sceneState.backdrop.canvas,flipY:false},{texture:backdrop,colorSpace:"srgb",premultipliedAlpha:false},[width,height]);
      items.push({texture:backdrop,quad:fullQuad,modeIndex:0,effects:[{enabled:true,effect:{kind:"classic_blur",intensity:frame.scene.background.intensity}}]});
    }
    if (frame.background && !sceneState?.backdrop) {
      const bg = texture(1, 1), color = frame.background;
      // Match Classic ColorNode: Canvas fill then unpremultiplied external
      // upload. Straight byte writes would bypass premul/unpremul quantization.
      color.forEach(v => finite(v, "background component"));
      const backgroundCanvas = new OffscreenCanvas(1, 1), context = backgroundCanvas.getContext("2d");
      if (!context) reject("VISUAL_GPU_FAILED", "Background source canvas is unavailable");
      context.fillStyle = `rgba(${color[0] * 255}, ${color[1] * 255}, ${color[2] * 255}, ${color[3]})`;
      context.fillRect(0, 0, 1, 1);
      device.queue.copyExternalImageToTexture({ source: backgroundCanvas, flipY: false }, { texture: bg, colorSpace: "srgb", premultipliedAlpha: false }, [1, 1]);
      canvases.push(backgroundCanvas);
      items.push({ texture: bg, modeIndex: 0, quad: { centerX: width / 2, centerY: height / 2, width, height, rotationRadians: 0, opacity: 1, flipX: false, flipY: false } });
    }
    const uploaded = new Map();
    for (const [index, layer] of frame.layers.entries()) {
      checkAbort(signal);
      if (layer.quad === null) continue;
      const quad = layer.quad;
      const source = sources.get(layer.elementId);
      if (!source) reject("INVALID_VISUAL_PLAN", "Bound quad has no decoded source");
      for (const key of ["centerX", "centerY", "width", "height", "rotationRadians", "opacity"]) finite(quad[key], key);
      if (quad.width <= 0 || quad.height <= 0 || typeof quad.flipX !== "boolean" || typeof quad.flipY !== "boolean") reject("UNSUPPORTED_RENDER_FEATURE", "Degenerate visual quads are unsupported");
      let tex = uploaded.get(source);
      if (!tex) {
        tex = cache.sources.get(source)?.texture;
        if (!tex) {
          const retained = isRetainedGraphicSource(source);
          const rawWidth=source.canvas.displayWidth??source.width,rawHeight=source.canvas.displayHeight??source.height;
          tex = texture(rawWidth, rawHeight, retained);
          if (retained) cache.sources.set(source, { texture: tex, width: source.width, height: source.height });
          device.queue.copyExternalImageToTexture({ source: source.canvas, flipY: false }, { texture: tex, colorSpace: "srgb", premultipliedAlpha: false }, [rawWidth, rawHeight]);
        }
        uploaded.set(source, tex);
      }
      const stack = frame.scene?.layers[index]?.stack;
      items.push({ elementId:layer.elementId, videoColorSpace:source.videoColorSpace, videoRotation:source.videoRotation??0, videoTransfer:typeof VideoFrame!=='undefined'&&source.canvas instanceof VideoFrame&&source.canvas.colorSpace.primaries==='bt709'&&source.canvas.colorSpace.transfer==='bt709'?1:0, texture: tex, quad, effectQuad:frame.scene?.layers[index]?.quad, modeIndex: modeIndices[index], masks:sceneState?.masks.get(layer.elementId),effects:stack?.effects??[],maskSource:sceneState?null:maskSources.get(layer.elementId) });
    }
    if (sceneState) {
      const byId = new Map(items.filter(item=>item.elementId).map(item=>[item.elementId,item]));
      items = [...items.filter(item=>!item.elementId),...frame.scene.steps.flatMap(step=>{
        if(step.kind==='effect')return [{sceneEffect:step.effects}];const item=byId.get(step.element_id);if(!item)return [];
        const strokes=(item.masks??[]).filter(source=>source.mask.stroke.width_px>0).map(source=>{const canvas=rasterizeSceneStroke(source,signal);canvases.push(canvas);const tex=texture(width,height);device.queue.copyExternalImageToTexture({source:canvas,flipY:false},{texture:tex,colorSpace:'srgb',premultipliedAlpha:false},[width,height]);return {texture:tex,quad:fullQuad,modeIndex:0,hitElementId:item.elementId};});
        return [item,...strokes];
      })];
    }
    if(hitPoint){const bytes=Math.max(256,items.length*256);releases.push(budget.reserve(bytes));hitReadback=device.createBuffer({size:bytes,usage:GPUBufferUsage.COPY_DST|GPUBufferUsage.MAP_READ});buffers.push(hitReadback);}
    for (const item of items) {
      checkAbort(signal);
      if (item.sceneEffect) {
        const filtered = applyEffects(applyLegacyColor(scene,item.sceneEffect),item.sceneEffect);
        if (filtered !== scene) {encoder.copyTextureToTexture({texture:filtered},{texture:nextScene},[width,height]);[scene,nextScene]=[nextScene,scene];}
        continue;
      }
      // Resolve every distance texture before storing intermediate foreground
      // in shared mask scratch; a later JFA must never overwrite that foreground.
      const masks = (item.masks??(item.maskSource?[item.maskSource]:[])).map(source=>({texture:maskTexture(source),inverted:source.inverted===true}));
      const gradedSource=applyLegacyColor(item.texture,item.effects,item.videoColorSpace,item.videoRotation,item.videoTransfer);
      const pass = encoder.beginRenderPass({ colorAttachments: [{ view: target.createView(), loadOp: "clear", storeOp: "store", clearValue: { r: 0, g: 0, b: 0, a: 0 } }] });
      pass.setPipeline(layerPipeline); pass.setVertexBuffer(0, vertices);
      pass.setBindGroup(0, group(layerPipeline, 0, gradedSource)); pass.setBindGroup(1, uniforms(layerPipeline, 1, uniformBytes(item.quad, frame.identity)));
      pass.draw(6); pass.end();
      let foreground = applyEffects(target,item.effects??[],item.effectQuad);
      for (const mask of masks) {
        const input = foreground;
        foreground = input === cache.maskTargets.textures[0] ? target : cache.maskTargets.textures[0];
        const pipeline = realm.maskPipelines.mask;
        maskPass(pipeline, foreground, [group(pipeline, 0, input), group(pipeline, 1, mask.texture), uniforms(pipeline, 2, new Float32Array([mask.inverted?1:0, 0, 0, 0]))]);
      }
      const hitElementId=item.hitElementId??item.elementId;
      if(hitReadback&&hitElementId){encoder.copyTextureToBuffer({texture:foreground,origin:{x:hitPoint.x,y:hitPoint.y,z:0}},{buffer:hitReadback,offset:hitEntries.length*256,bytesPerRow:256,rowsPerImage:1},[1,1]);hitEntries.push(hitElementId);}
      const mix = encoder.beginRenderPass({ colorAttachments: [{ view: nextScene.createView(), loadOp: "clear", storeOp: "store", clearValue: { r: 0, g: 0, b: 0, a: 0 } }] });
      if (!blendUniforms.has(item.modeIndex)) blendUniforms.set(item.modeIndex, uniforms(blendPipeline, 2, new Uint32Array([item.modeIndex, 0, 0, 0])));
      mix.setPipeline(blendPipeline); mix.setVertexBuffer(0, vertices);
      mix.setBindGroup(0, group(blendPipeline, 0, scene)); mix.setBindGroup(1, group(blendPipeline, 1, foreground)); mix.setBindGroup(2, blendUniforms.get(item.modeIndex));
      mix.draw(6); mix.end(); [scene, nextScene] = [nextScene, scene];
    }
    if (surface||captureSurface) {
      if(captureSurface){
        if(owned||surface||hitQuery)reject("INVALID_VISUAL_PLAN","Texture output requires one owned preview realm");
        releases.push(budget.reserve(width*height*8));
        surfaceFrame=captureGpuSurfaceFrame(realm,encoder,scene,width,height);
      }else{
      releases.push(budget.reserve(width * height * 4));
      const changed = realm.previewSurface !== surface || surface.width !== width || surface.height !== height;
      if (surface.width !== width) surface.width = width;
      if (surface.height !== height) surface.height = height;
      const context = surface.getContext("webgpu");
      if (!context) reject("VISUAL_GPU_FAILED", "Preview GPU surface is unavailable");
      if (changed) {
        context.configure({ device, format: FORMAT, alphaMode: "opaque", usage: GPUTextureUsage.COPY_DST | GPUTextureUsage.RENDER_ATTACHMENT });
        realm.previewSurface = surface;
      }
      encoder.copyTextureToTexture({ texture: scene }, { texture: context.getCurrentTexture() }, [width, height]);
      }
      device.queue.submit([encoder.finish()]);
      const submitted=performance.now();
      const validationResult = device.popErrorScope(); scopes--;
      const memoryResult = device.popErrorScope(); scopes--;
      if(captureSurface){
        const validation=Promise.all([validationResult,memoryResult]).then(([failure,memory])=>{
          if(failure||memory)reject("VISUAL_GPU_FAILED","GPU rejected preview presentation");checkAbort(signal);
        });
        validateGpuSurfaceFrame(surfaceFrame,validation);
        const captured=surfaceFrame;surfaceFrame=null;
        return {...realm.observation,peakGpuBytes:String(budget.peak),gpuEncodeMs:String(submitted-began),gpuValidationPending:"true",surfaceFrame:captured};
      }
      const [validationFailure, memoryFailure] = await Promise.all([validationResult, memoryResult]);
      if (validationFailure || memoryFailure) reject("VISUAL_GPU_FAILED", "GPU rejected preview presentation");
      checkAbort(signal);
      let hitTest;
      if(hitReadback){await hitReadback.mapAsync(GPUMapMode.READ);checkAbort(signal);const bytes=new Uint8Array(hitReadback.getMappedRange()),hits=[];for(let i=hitEntries.length-1;i>=0;i--)if(bytes[i*256+3]>0&&!hits.some(hit=>hit.elementId===hitEntries[i]))hits.push({elementId:hitEntries[i],alpha:bytes[i*256+3]});hitReadback.unmap();hitTest={identity:structuredClone(frame.identity),point:hitPoint,hits};}
      const captured=surfaceFrame;surfaceFrame=null;
      return { ...realm.observation, peakGpuBytes: String(budget.peak),gpuEncodeMs:String(submitted-began),gpuValidateMs:String(performance.now()-submitted),...(captured?{surfaceFrame:captured}:{}), ...(hitTest?{hitTest}:{}) };
    }
    const rowBytes = width * 4, aligned = Math.ceil(rowBytes / 256) * 256;
    releases.push(budget.reserve(aligned * height));
    readback = device.createBuffer({ size: aligned * height, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ }); buffers.push(readback);
    encoder.copyTextureToBuffer({ texture: scene }, { buffer: readback, bytesPerRow: aligned, rowsPerImage: height }, [width, height]);
    device.queue.submit([encoder.finish()]);
    const validationResult = device.popErrorScope(); scopes--;
    const memoryResult = device.popErrorScope(); scopes--;
    const [validationFailure, memoryFailure] = await Promise.all([validationResult, memoryResult]);
    if (validationFailure || memoryFailure) reject("VISUAL_GPU_FAILED", "GPU rejected the compiled frame execution");
    await readback.mapAsync(GPUMapMode.READ); checkAbort(signal);
    const bytes = new Uint32Array(readback.getMappedRange());
    // BGRA -> RGBA swaps the red/blue lanes without four scalar byte writes
    // per pixel. Both the padded rows and emitted chunks remain exact.
    let offset = 0; const chunk = new Uint8Array(Math.min(65536, rowBytes * height));
    const words = new Uint32Array(chunk.buffer); let fill = 0;
    for (let row = 0; row < height; row++) {
      for (let x = 0; x < width; x++) {
        const pixel = bytes[row * aligned / 4 + x];
        words[fill / 4] = (pixel & 0xff00ff00) | ((pixel & 0xff) << 16) | ((pixel >>> 16) & 0xff);
        fill += 4;
        if (fill === chunk.length) { checkAbort(signal); await emit(offset, chunk); offset += fill; fill = 0; }
      }
    }
    if (fill) { checkAbort(signal); await emit(offset, chunk.subarray(0, fill)); }
    readback.unmap();
    return { ...realm.observation, peakGpuBytes: String(budget.peak) };
  } catch (error) {
    clearGpuFrameCaches(realm);
    throw error;
  } finally {
    surfaceFrame?.close();
    releaseMaskFrameSources(frame);
    releaseSceneSources(frame);
    if (readback?.mapState === "mapped") readback.unmap();
    if (hitReadback?.mapState === "mapped") hitReadback.unmap();
    for (const b of buffers) b.destroy(); for (const t of allocated) t.destroy();
    for (const canvas of canvases) { canvas.width = 0; canvas.height = 0; }
    while (scopes-- > 0) await device.popErrorScope().catch(() => undefined);
    for (const release of releases) release(); if (owned) closeGpuRealm(realm);
  }
}
