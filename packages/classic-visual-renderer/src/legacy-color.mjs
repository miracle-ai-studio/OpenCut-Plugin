// OpenCut-authored execution of the retained 8-bit limited-range YUV grade.
// The oracle is media/historical_visual_composition.rs's external FFmpeg chain;
// no upstream shader or FFmpeg source is copied into this module.
import { finite, reject } from './validation.mjs';
function plane(contrast, brightness) {
  if (contrast === 1 && brightness === 0) return [4096, 0];
  const gain = Math.trunc(contrast * 4096);
  const offset = Math.floor(Math.floor(brightness * 100 + 100) * 511 / 200) - 128 - Math.floor(gain / 32);
  return [gain, offset];
}
export function legacyColorPasses(effects,videoColorSpace) {
  const passes=[];
  for(const entry of effects??[]){
    if(!entry?.enabled || entry.effect?.kind!=='legacy_color_adjust')continue;
    const matrix=videoColorSpace?.matrix;
    if(videoColorSpace&&(videoColorSpace.fullRange!==false||!['bt709','bt470bg','smpte170m'].includes(matrix)))
      reject('UNSUPPORTED_RENDER_FEATURE','Legacy grades require explicit limited-range BT.601/BT.709 video colour metadata');
    const e=entry.effect, exposure=finite(e.exposure_ev,'legacy exposure'),contrast=finite(e.contrast,'legacy contrast'),saturation=finite(e.saturation,'legacy saturation');
    if(exposure < -8 || exposure > 8 || contrast < 0 || contrast > 4 || saturation < 0 || saturation > 4)reject('INVALID_VISUAL_PLAN','Legacy grade parameters exceed their frozen range');
    // The retained native filter serializes parameters to nine decimal places.
    const brightness=Number(Math.max(-1,Math.min(1,2**exposure-1)).toFixed(9));
    passes.push(new Int32Array([...plane(Number(contrast.toFixed(9)),brightness),...plane(Math.min(3,Number(saturation.toFixed(9))),0),matrix==='bt709'?1:0,0,0,0]));
  }
  return passes;
}
export const legacyColorShader = `
struct VertexOutput { @builtin(position) position: vec4f, @location(0) uv: vec2f }
@group(0) @binding(0) var source: texture_2d<f32>;
struct Grade { plane:vec4i, source:vec4i }
@group(1) @binding(0) var<uniform> grade: Grade;
fn adjusted(value:i32,gain:i32,offset:i32)->i32{return clamp(((value*gain) >> 12u)+offset,0,255);}
@fragment fn fragment_main(input:VertexOutput)->@location(0) vec4f {
 let size=vec2i(textureDimensions(source));let pixel=vec2i(input.position.xy);var coordinate=pixel;
 if(grade.source.z==90){coordinate=vec2i(pixel.y,size.y-1-pixel.x);}
 if(grade.source.z==180){coordinate=size-vec2i(1)-pixel;}
 if(grade.source.z==270){coordinate=vec2i(size.x-1-pixel.y,pixel.x);}
 var rgba=textureLoad(source,coordinate,0);
 // Direct BT.709 VideoFrame uploads convert their transfer function to sRGB.
 // The retained source-space FFmpeg grade consumes the encoded BT.709 values.
 // Undo only that observed upload conversion, once, before grading or opacity.
 if(grade.source.y==1){
  let encoded=clamp(rgba.rgb,vec3f(0),vec3f(1));
  let linearLight=select(pow((encoded+vec3f(0.055))/1.055,vec3f(2.4)),encoded/12.92,encoded<=vec3f(0.04045));
  rgba=vec4f(select(1.099*pow(linearLight,vec3f(0.45))-vec3f(0.099),4.5*linearLight,linearLight<vec3f(0.018)),rgba.a);
 }
 if(grade.source.w==1){return rgba;}
 let rgb=vec3i(round(clamp(rgba.rgb,vec3f(0),vec3f(1))*255.0));
 var y=(rgb.r*8414+rgb.g*16519+rgb.b*3208+540672)>>15u;
 var u=(rgb.r*(-4857)+rgb.g*(-9535)+rgb.b*14392+4210688)>>15u;
 var v=(rgb.r*14392+rgb.g*(-12052)+rgb.b*(-2340)+4210688)>>15u;
 if(grade.source.x==1){
  y=(rgb.r*5983+rgb.g*20127+rgb.b*2032+540672)>>15u;
  u=(rgb.r*(-3298)+rgb.g*(-11094)+rgb.b*14392+4210688)>>15u;
  v=(rgb.r*14392+rgb.g*(-13073)+rgb.b*(-1319)+4210688)>>15u;
 }
 let yy=adjusted(y,grade.plane.x,grade.plane.y)-16;
 let uu=adjusted(u,grade.plane.z,grade.plane.w)-128;
 let vv=adjusted(v,grade.plane.z,grade.plane.w)-128;
 var result=vec3i((76309*yy+104597*vv+32768)>>16u,(76309*yy-25675*uu-53279*vv+32768)>>16u,(76309*yy+132201*uu+32768)>>16u);
 if(grade.source.x==1){result=vec3i((76309*yy+117489*vv+32768)>>16u,(76309*yy-13975*uu-34925*vv+32768)>>16u,(76309*yy+138438*uu+32768)>>16u);}
 return vec4f(vec3f(clamp(result,vec3i(0),vec3i(255)))/255.0,rgba.a);
}`;
