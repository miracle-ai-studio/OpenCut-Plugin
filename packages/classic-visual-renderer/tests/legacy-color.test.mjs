import {test} from 'node:test';
import assert from 'node:assert/strict';
import {legacyColorPasses} from '../src/legacy-color.mjs';
const grade={enabled:true,effect:{kind:'legacy_color_adjust',exposure_ev:0,contrast:1.1,saturation:1.06}};
test('legacy source grades bind explicit video colour facts and retain the native parameter ranges',()=>{
 const image=legacyColorPasses([grade]),video=legacyColorPasses([grade],{matrix:'bt709',fullRange:false});
 assert.equal(image[0].length,8);assert.equal(image[0][4],0);assert.equal(video[0][4],1);
 for(const facts of [{},{matrix:'bt709'},{matrix:'bt2020-ncl',fullRange:false},{matrix:'bt709',fullRange:true}])
  assert.throws(()=>legacyColorPasses([grade],facts),{code:'UNSUPPORTED_RENDER_FEATURE'});
 assert.equal(legacyColorPasses([{...grade,enabled:false}],{}).length,0);
 assert.throws(()=>legacyColorPasses([{...grade,effect:{...grade.effect,contrast:4.1}}]),{code:'INVALID_VISUAL_PLAN'});
});
