#!/usr/bin/env node
// Independent pixel oracle for the retained legacy YUV grade, not a second
// implementation used to produce the editor's output. All artifacts stay local.
import fs from 'node:fs/promises';
import path from 'node:path';
import http from 'node:http';
import {spawnSync} from 'node:child_process';
import assert from 'node:assert/strict';
import {chromium} from '../packages/classic-audio-renderer/node_modules/playwright-core/index.mjs';
import {legacyColorShader,legacyColorPasses} from '../packages/classic-visual-renderer/src/legacy-color.mjs';
const directory=process.argv[2];if(!directory||!path.isAbsolute(directory))throw Error('Pass an absolute out-of-repository evidence directory');
await fs.mkdir(directory,{recursive:true,mode:0o700});
const width=256,height=32,source=Buffer.alloc(width*height*4);
for(let y=0;y<height;y++)for(let x=0;x<width;x++){const i=(y*width+x)*4;source.set([x,y*8,(x+24*y)%256,x],i);}
await fs.writeFile(path.join(directory,'source.rgba'),source);
const gpuFrame=async({shader,uniforms,source,width,height,rotation=0})=>{
   const adapter=await navigator.gpu?.requestAdapter();if(!adapter)throw Error('No admitted WebGPU adapter');
   const device=await adapter.requestDevice();let errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));
   const vertex=device.createShaderModule({code:`struct Output{@builtin(position) position:vec4f,@location(0) uv:vec2f} @vertex fn vertex_main(@builtin(vertex_index) i:u32)->Output {let p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return Output(vec4f(p[i],0,1),vec2f(0));}`});
   const fragment=device.createShaderModule({code:shader}),info=await fragment.getCompilationInfo();
   if(info.messages.some(m=>m.type==='error'))throw Error(JSON.stringify(info.messages.map(m=>m.message)));
   const pipeline=device.createRenderPipeline({layout:'auto',vertex:{module:vertex,entryPoint:'vertex_main'},fragment:{module:fragment,entryPoint:'fragment_main',targets:[{format:'rgba8unorm'}]},primitive:{topology:'triangle-list'}});
   const input=device.createTexture({size:[width,height],format:'rgba8unorm',usage:GPUTextureUsage.COPY_DST|GPUTextureUsage.TEXTURE_BINDING});
   const targetWidth=rotation%180?height:width,targetHeight=rotation%180?width:height,stride=Math.ceil(targetWidth*4/256)*256;
   const target=device.createTexture({size:[targetWidth,targetHeight],format:'rgba8unorm',usage:GPUTextureUsage.RENDER_ATTACHMENT|GPUTextureUsage.COPY_SRC});
   const uniform=device.createBuffer({size:32,usage:GPUBufferUsage.UNIFORM|GPUBufferUsage.COPY_DST});
   device.queue.writeTexture({texture:input},new Uint8Array(source),{bytesPerRow:width*4},[width,height]);device.queue.writeBuffer(uniform,0,new Int32Array(uniforms));
   const groups=[device.createBindGroup({layout:pipeline.getBindGroupLayout(0),entries:[{binding:0,resource:input.createView()}]}),device.createBindGroup({layout:pipeline.getBindGroupLayout(1),entries:[{binding:0,resource:{buffer:uniform}}]})];
   const encoder=device.createCommandEncoder(),pass=encoder.beginRenderPass({colorAttachments:[{view:target.createView(),loadOp:'clear',storeOp:'store',clearValue:[0,0,0,0]}]});pass.setPipeline(pipeline);groups.forEach((g,i)=>pass.setBindGroup(i,g));pass.draw(3);pass.end();
   const result=device.createBuffer({size:stride*targetHeight,usage:GPUBufferUsage.COPY_DST|GPUBufferUsage.MAP_READ});encoder.copyTextureToBuffer({texture:target},{buffer:result,bytesPerRow:stride},[targetWidth,targetHeight]);device.queue.submit([encoder.finish()]);
   await result.mapAsync(GPUMapMode.READ);const storage=new Uint8Array(result.getMappedRange()),pixels=[];for(let y=0;y<targetHeight;y++)pixels.push(...storage.subarray(y*stride,y*stride+targetWidth*4));result.unmap();await device.queue.onSubmittedWorkDone();device.destroy();
   if(errors.length)throw Error(errors.join('\n'));return pixels;
  };
const server=http.createServer((_request,response)=>{response.setHeader('Content-Type','text/html');response.end('<!doctype html><title>Owned grade oracle</title>');});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
let browser;const results=[];
try {
 browser=await chromium.launch({headless:true,executablePath:process.env.OPENCUT_CLASSIC_VISUAL_CHROMIUM||chromium.executablePath()});const page=await browser.newPage();
 await page.goto(`http://127.0.0.1:${server.address().port}`);
 for(const matrix of ["bt601","bt709"])for(const [exposure,contrast,saturation] of [[0,1.1,1.06],[0,1,1],[.25,1,.5],[-.5,.9,2.2]]){
  const brightness=Math.max(-1,Math.min(1,2**exposure-1)).toFixed(9),output=path.join(directory,`expected-${results.length}.rgba`);
  const run=await fs.stat(output).then(()=>({status:0})).catch(()=>spawnSync('ffmpeg',['-v','error','-y','-f','rawvideo','-pixel_format','rgba','-video_size',`${width}x${height}`,'-i',path.join(directory,'source.rgba'),'-frames:v','1','-vf',`scale=out_color_matrix=${matrix},format=yuva444p,eq=brightness=${brightness}:contrast=${contrast.toFixed(9)}:saturation=${saturation.toFixed(9)},scale=in_color_matrix=${matrix},format=rgba`,'-f','rawvideo',output],{encoding:'utf8'}));
  assert.equal(run.status,0,run.stderr);
  const uniforms=[...legacyColorPasses([{enabled:true,effect:{kind:'legacy_color_adjust',exposure_ev:exposure,contrast,saturation}}],matrix==='bt709'?{matrix:'bt709',fullRange:false}:undefined)[0]];
  const observed=await page.evaluate(gpuFrame,{shader:legacyColorShader,uniforms,source:[...source],width,height});
  const expected=await fs.readFile(output);let maxRgb=0,maxAlpha=0,sourceAlpha=0,sum=0;
  for(let i=0;i<observed.length;i++){const delta=Math.abs(observed[i]-expected[i]);if(i%4===3){maxAlpha=Math.max(maxAlpha,delta);sourceAlpha=Math.max(sourceAlpha,Math.abs(observed[i]-source[i]));}else{maxRgb=Math.max(maxRgb,delta);sum+=delta;}}
  // Existing project-render scene pixel oracle permits 12 for YUV round trips.
  // Keep that established gate; report measured differences rather than widen it.
  assert(maxRgb<=12,`RGB gate: ${maxRgb} > 12`);assert(maxAlpha<=12,`RGBA oracle gate: ${maxAlpha} > 12`);assert.equal(sourceAlpha,0);
  await fs.writeFile(path.join(directory,`actual-${results.length}.rgba`),Buffer.from(observed));results.push({matrix,exposure,contrast,saturation,maxRgbError:maxRgb,meanRgbError:sum/(width*height*3),maxAlphaError:maxAlpha,sourceAlphaError:sourceAlpha});
 }
 for(const rotation of [90,180,270]){
  const observed=Buffer.from(await page.evaluate(gpuFrame,{shader:legacyColorShader,uniforms:[0,0,0,0,0,0,rotation,1],source:[...source],width,height,rotation}));
  const outWidth=rotation%180?height:width,expected=Buffer.alloc(source.length);
  for(let y=0;y<height;y++)for(let x=0;x<width;x++){
   let ox,oy;if(rotation===90){ox=height-1-y;oy=x;}else if(rotation===180){ox=width-1-x;oy=height-1-y;}else{ox=y;oy=width-1-x;}
   source.copy(expected,(oy*outWidth+ox)*4,(y*width+x)*4,(y*width+x+1)*4);
  }
  assert.deepEqual(observed,expected);results.push({orientation:rotation,exactRgba:true});
 }
 await fs.writeFile(path.join(directory,'result.json'),JSON.stringify({gate:{maxRgba:12,sourceAlpha:0},results},null,2));console.log(JSON.stringify(results));
}finally{await browser?.close();await new Promise(resolve=>server.close(resolve));}
