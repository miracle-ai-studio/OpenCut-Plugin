// Classic effects/definitions/blur.ts: exact per-axis sigma and bounded passes.
import {legacyColorPasses} from './legacy-color.mjs';
import { finite, reject } from './validation.mjs';
export function sceneEffectPasses(effects,width,height) {
  if(!Array.isArray(effects)||effects.length>64)reject('INVALID_VISUAL_PLAN','Invalid ordered scene effects');
  const result=[];
  for(const entry of effects){if(!entry||typeof entry.enabled!=='boolean')reject('INVALID_VISUAL_PLAN','Invalid effect flag');if(!entry.enabled)continue;
    if(entry.effect?.kind==='legacy_color_adjust'){legacyColorPasses([entry]);continue;}
    if(entry.effect?.kind!=='classic_blur')reject('INVALID_VISUAL_PLAN','Unsupported scene effect');
    const intensity=finite(entry.effect.intensity,'blur intensity');if(intensity<0)reject('INVALID_VISUAL_PLAN','Negative blur intensity');
    const sx=intensity/5*width/1920,sy=intensity/5*height/1080,m=Math.max(sx,sy);if(m<.001)continue;
    const n=Math.min(8,Math.max(1,Math.ceil(m*m/(40*40))));
    const x=finite(sx/Math.sqrt(n),'blur sigma X'),y=finite(sy/Math.sqrt(n),'blur sigma Y');
    for(let i=0;i<n;i++){if(x>0)result.push({sigma:x,step:Math.max(1,x/10),direction:[1,0]});if(y>0)result.push({sigma:y,step:Math.max(1,y/10),direction:[0,1]});}
  }
  return result;
}
