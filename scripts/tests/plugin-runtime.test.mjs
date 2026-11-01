import {test} from 'node:test';
import assert from 'node:assert/strict';
import {workspaceDatabase,dataDirectory} from '../../plugins/opencut/scripts/runtime.mjs';
test('MCP and Web select an explicit absolute database before the default workspace',()=>{
 const previous=process.env.OPENCUT_PLUGIN_DATABASE;
 try {
  delete process.env.OPENCUT_PLUGIN_DATABASE;
  assert.equal(workspaceDatabase(),`${dataDirectory()}/workspace.sqlite`);
  assert.equal(workspaceDatabase({database:'/tmp/selected.sqlite'}),'/tmp/selected.sqlite');
  process.env.OPENCUT_PLUGIN_DATABASE='/tmp/override.sqlite';
  assert.equal(workspaceDatabase({database:'/tmp/selected.sqlite'}),'/tmp/override.sqlite');
  process.env.OPENCUT_PLUGIN_DATABASE='relative.sqlite';assert.throws(()=>workspaceDatabase(),/absolute/);
 }finally{if(previous===undefined)delete process.env.OPENCUT_PLUGIN_DATABASE;else process.env.OPENCUT_PLUGIN_DATABASE=previous;}
});

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {digest,runtimeEnvironment,rendererEnvironment} from '../../plugins/opencut/scripts/runtime.mjs';
function nativeFixture() {
 const root=fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(),'opencut-native-launch-')));
 const target=`${process.arch==='x64'?'x86_64':'aarch64'}-apple-darwin`,files={};
 const write=(name,contents)=>{const file=path.join(root,name);fs.mkdirSync(path.dirname(file),{recursive:true});fs.writeFileSync(file,contents,{mode:0o700});files[name]={sha256:digest(file),size:fs.statSync(file).size};};
 write('bin/opencut','#!/bin/sh\nprintf \'{"data":{},"error":null}\\n\'\n');
 write('bin/opencut-render-worker','worker');
 const components={};for(const name of ['ffmpeg','ffprobe','python','node']){components[name]={entry:`runtimes/${name}/tool`};write('bin/libexec/'+components[name].entry,name);}
 write('bin/libexec/runtime-bundle.json',JSON.stringify({format:'opencut.native-runtime-bundle',components}));
 const manifest={target,binary_sha256:files['bin/opencut'].sha256,files,native_renderer:{format:'opencut.native-renderer',target,cli:'bin/opencut',worker:'bin/opencut-render-worker',files:{'bin/opencut':files['bin/opencut'],'bin/opencut-render-worker':files['bin/opencut-render-worker']}}};
 fs.writeFileSync(path.join(root,'manifest.json'),JSON.stringify(manifest));return root;
}
test('native plugin pins bundled tools and starts without any browser renderer',{skip:process.platform!=='darwin'},async()=>{
 const root=nativeFixture();try{const env=await runtimeEnvironment(root);assert.equal(env.OPENCUT_RENDER_WORKER,path.join(root,'bin/opencut-render-worker'));assert.equal(env.OPENCUT_FFMPEG,path.join(root,'bin/libexec/runtimes/ffmpeg/tool'));assert.equal(env.OPENCUT_CLASSIC_VISUAL_NODE,path.join(root,'bin/libexec/runtimes/node/tool'));assert.equal(env.OPENCUT_CLASSIC_AUDIO_PACKAGE,undefined);}finally{fs.rmSync(root,{recursive:true,force:true});}
});
test('corrupt native worker and missing bundle entry are refused before launch',{skip:process.platform!=='darwin'},async()=>{
 const root=nativeFixture();try{fs.writeFileSync(path.join(root,'bin/opencut-render-worker'),'changed');await assert.rejects(runtimeEnvironment(root),/Runtime file differs/);fs.writeFileSync(path.join(root,'bin/opencut-render-worker'),'worker');fs.unlinkSync(path.join(root,'bin/libexec/runtimes/ffmpeg/tool'));await assert.rejects(runtimeEnvironment(root),/ENOENT/);}finally{fs.rmSync(root,{recursive:true,force:true});}
});
test('native source launch removes obsolete browser and injected library settings',async()=>{
 const root=nativeFixture();try{const env=await rendererEnvironment(path.join(root,'bin/opencut'),{PATH:process.env.PATH,OPENCUT_CLASSIC_AUDIO_CHROMIUM:'/old/browser',OPENCUT_SOURCE_ART_PACKAGE:'/old/package',DYLD_LIBRARY_PATH:'/old/libs',OPENCUT_FFMPEG:'/explicit/ffmpeg'});assert.equal(env.OPENCUT_CLASSIC_AUDIO_CHROMIUM,undefined);assert.equal(env.OPENCUT_SOURCE_ART_PACKAGE,undefined);assert.equal(env.DYLD_LIBRARY_PATH,undefined);assert.equal(env.OPENCUT_FFMPEG,'/explicit/ffmpeg');}finally{fs.rmSync(root,{recursive:true,force:true});}
});
