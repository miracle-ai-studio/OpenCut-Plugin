import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {workspaceDatabase,digest,runtimeEnvironment,rendererEnvironment} from '../../plugins/opencut/scripts/runtime.mjs';

test('database selection is explicit only when passed by the installer',()=>{
 const root=fs.realpathSync(os.tmpdir());
 const database=path.join(root,'selected.sqlite');
 const explicit=path.join(root,'explicit.sqlite');
 const previous=process.env.OPENCUT_PLUGIN_DATABASE;
 try {
  process.env.OPENCUT_PLUGIN_DATABASE=path.join(root,'stale.sqlite');
  assert.equal(workspaceDatabase({database}),database);
  assert.equal(workspaceDatabase({database},explicit),explicit);
  assert.throws(()=>workspaceDatabase({database:'relative.sqlite'}),/absolute/);
 }finally{if(previous===undefined)delete process.env.OPENCUT_PLUGIN_DATABASE;else process.env.OPENCUT_PLUGIN_DATABASE=previous;}
});

function nativeFixture(binary = '#!/bin/sh\nprintf \'{"data":{},"error":null}\\n\'\n') {
 const root=fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(),'opencut-native-launch-')));
 const target=`${process.arch==='x64'?'x86_64':'aarch64'}-apple-darwin`,files={};
 const write=(name,contents)=>{const file=path.join(root,name);fs.mkdirSync(path.dirname(file),{recursive:true});fs.writeFileSync(file,contents,{mode:0o700});files[name]={sha256:digest(file),size:fs.statSync(file).size};};
 write('bin/opencut',binary);
 write('bin/opencut-render-worker','worker');
 const components={};for(const name of ['ffmpeg','ffprobe','python','node']){components[name]={entry:`runtimes/${name}/tool`};write('bin/libexec/'+components[name].entry,name);}
 write('bin/libexec/runtime-bundle.json',JSON.stringify({format:'opencut.native-runtime-bundle',components}));
 const manifest={target,binary_sha256:files['bin/opencut'].sha256,files,native_renderer:{format:'opencut.native-renderer',target,cli:'bin/opencut',worker:'bin/opencut-render-worker',files:{'bin/opencut':files['bin/opencut'],'bin/opencut-render-worker':files['bin/opencut-render-worker']}}};
 fs.writeFileSync(path.join(root,'manifest.json'),JSON.stringify(manifest));return root;
}
test('Web and MCP use one installed database despite a stale environment override',{skip:process.platform!=='darwin'},()=>{
 const root=nativeFixture(`#!/bin/sh
if [ "$1" = --json ]; then
  printf '{"data":{},"error":null}\n'
else
  database="$2"
  if [ -n "$OPENCUT_TEST_SERVICE_DATABASE" ]; then database="$OPENCUT_TEST_SERVICE_DATABASE"; fi
  printf '{"data":{"database":"%s","action":"%s","directory":"%s"},"error":null}\n' "$database" "$7" "$6"
fi
`);
 try {
  const data=path.join(root,'data');
  const directory=path.join(data,'server');
  fs.mkdirSync(directory,{recursive:true});
  const database=path.join(root,'selected.sqlite');
  const other=path.join(root,'other.sqlite');
  fs.writeFileSync(path.join(data,'install.json'),JSON.stringify({format:'opencut.plugin-install.v1',runtime:root,database}));
  const env={...process.env,OPENCUT_PLUGIN_DATA_DIR:data,OPENCUT_PLUGIN_DATABASE:other};
  const run=(script,args=[],overrides={})=>spawnSync(process.execPath,[
   fileURLToPath(new URL('../../plugins/opencut/scripts/'+script,import.meta.url)),...args
  ],{env:{...env,...overrides},encoding:'utf8'});
  for(const action of ['start','status','stop']){
   const result=run('web.mjs',[action]);
   assert.equal(result.status,0,result.stderr);
   assert.deepEqual(JSON.parse(result.stdout).data,{database,action,directory});
  }
  const mcp=run('launch.mjs');
  assert.equal(mcp.status,0,mcp.stderr);
  assert.equal(JSON.parse(mcp.stdout).data.database,database);
  const wrongStatus=run('web.mjs',['status'],{OPENCUT_TEST_SERVICE_DATABASE:other});
  assert.equal(wrongStatus.status,1);
  assert.equal(wrongStatus.stdout,'');
  assert.match(wrongStatus.stderr,/different database/);
  fs.writeFileSync(path.join(directory,'state.json'),JSON.stringify({database:other}));
  const wrongStop=run('web.mjs',['stop']);
  assert.equal(wrongStop.status,1);
  assert.equal(wrongStop.stdout,'');
  assert.match(wrongStop.stderr,/different database/);
 }finally{fs.rmSync(root,{recursive:true,force:true});}
});
test('native plugin pins bundled tools and starts without any browser renderer',{skip:process.platform!=='darwin'},async()=>{
 const root=nativeFixture();try{const env=await runtimeEnvironment(root);assert.equal(env.OPENCUT_RENDER_WORKER,path.join(root,'bin/opencut-render-worker'));assert.equal(env.OPENCUT_FFMPEG,path.join(root,'bin/libexec/runtimes/ffmpeg/tool'));assert.equal(env.OPENCUT_CLASSIC_VISUAL_NODE,path.join(root,'bin/libexec/runtimes/node/tool'));assert.equal(env.OPENCUT_CLASSIC_AUDIO_PACKAGE,undefined);}finally{fs.rmSync(root,{recursive:true,force:true});}
});
test('corrupt native worker and missing bundle entry are refused before launch',{skip:process.platform!=='darwin'},async()=>{
 const root=nativeFixture();try{fs.writeFileSync(path.join(root,'bin/opencut-render-worker'),'changed');await assert.rejects(runtimeEnvironment(root),/Runtime file differs/);fs.writeFileSync(path.join(root,'bin/opencut-render-worker'),'worker');fs.unlinkSync(path.join(root,'bin/libexec/runtimes/ffmpeg/tool'));await assert.rejects(runtimeEnvironment(root),/ENOENT/);}finally{fs.rmSync(root,{recursive:true,force:true});}
});
test('native source launch removes obsolete browser and injected library settings',async()=>{
 const root=nativeFixture();try{const env=await rendererEnvironment(path.join(root,'bin/opencut'),{PATH:process.env.PATH,OPENCUT_CLASSIC_AUDIO_CHROMIUM:'/old/browser',OPENCUT_SOURCE_ART_PACKAGE:'/old/package',DYLD_LIBRARY_PATH:'/old/libs',OPENCUT_FFMPEG:'/explicit/ffmpeg'});assert.equal(env.OPENCUT_CLASSIC_AUDIO_CHROMIUM,undefined);assert.equal(env.OPENCUT_SOURCE_ART_PACKAGE,undefined);assert.equal(env.DYLD_LIBRARY_PATH,undefined);assert.equal(env.OPENCUT_FFMPEG,'/explicit/ffmpeg');}finally{fs.rmSync(root,{recursive:true,force:true});}
});
