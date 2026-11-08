import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {workspaceDatabase,serviceConnection,digest,runtimeEnvironment,rendererEnvironment} from '../../plugins/opencut/scripts/runtime.mjs';

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
 write('bin/libexec/runtimes/node/bin/node', '#!/bin/sh\nexec "$OPENCUT_TEST_NODE" "$@"\n');
 write('bin/libexec/runtime-bundle.json',JSON.stringify({format:'opencut.native-runtime-bundle',components}));
 const manifest={target,binary_sha256:files['bin/opencut'].sha256,files,native_renderer:{format:'opencut.native-renderer',target,cli:'bin/opencut',worker:'bin/opencut-render-worker',files:{'bin/opencut':files['bin/opencut'],'bin/opencut-render-worker':files['bin/opencut-render-worker']}}};
 fs.writeFileSync(path.join(root,'manifest.json'),JSON.stringify(manifest));return root;
}
test('Web and API attachment use one installed database despite a stale environment override',{skip:process.platform!=='darwin'},()=>{
 const root=nativeFixture(`#!/bin/sh
if [ "$1" = --json ]; then
  printf '{"data":{},"error":null}\n'
elif [ "$1" = --endpoint ]; then
  printf '{"data":{"arguments":["%s","%s","%s","%s","%s","%s"]},"error":null}\n' "$1" "$2" "$3" "$4" "$5" "$6"
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
  const state={version:1,instance:'test-instance',database,executable:path.join(root,'bin/opencut'),executable_sha256:digest(path.join(root,'bin/opencut')),endpoint:'http://127.0.0.1:54321'};
  fs.writeFileSync(path.join(directory,'state.json'),JSON.stringify(state));
  // The fixture records invocation without opening any database. No Worker,
  // lifecycle operation or --database may be passed by the API-only launcher.
  const mcp=run('launch.mjs');
  assert.equal(mcp.status,0,mcp.stderr);
  assert.deepEqual(JSON.parse(mcp.stdout).data.arguments,['--endpoint',state.endpoint,'--credential-file',path.join(directory,'api-token'),'mcp','serve']);
  assert.equal(fs.existsSync(database),false);
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

test('API attachment refuses mismatched identity and non-loopback endpoints without filesystem writes',()=>{
 const root=nativeFixture();
 const previous=process.env.OPENCUT_PLUGIN_DATA_DIR;
 try {
  const data=path.join(root,'data');
  process.env.OPENCUT_PLUGIN_DATA_DIR=data;
  const database=path.join(root,'workspace.sqlite'),config={runtime:root,database};
  assert.throws(()=>serviceConnection(config),/Start the configured/);
  assert.equal(fs.existsSync(data),false);
  const directory=path.join(data,'server');fs.mkdirSync(directory,{recursive:true});
  const state={version:1,instance:'test',database,executable:path.join(root,'bin/opencut'),executable_sha256:digest(path.join(root,'bin/opencut')),endpoint:'http://127.0.0.1:54321'};
  const write=overrides=>fs.writeFileSync(path.join(directory,'state.json'),JSON.stringify({...state,...overrides}));
  write({});
  assert.deepEqual(serviceConnection(config),{endpoint:state.endpoint,executable:state.executable,tokenFile:path.join(directory,'api-token')});
  for(const overrides of [{version:2},{instance:''},{database:path.join(root,'other.sqlite')},{database:'relative.sqlite'}]){
   write(overrides);assert.throws(()=>serviceConnection(config),/database|state/);
  }
  write({executable_sha256:'different'});assert.throws(()=>serviceConnection(config),/executable/);
  for(const endpoint of ['http://example.com:54321','http://127.0.0.1:54321/path','http://user:secret@127.0.0.1:54321','http://127.0.0.1:54321?query','https://127.0.0.1:54321']){
   write({endpoint});assert.throws(()=>serviceConnection(config),/loopback/);
  }
  assert.equal(fs.existsSync(database),false);
  assert.equal(fs.existsSync(path.join(directory,'api-token')),false);
  assert.equal(fs.existsSync(path.join(directory,'command.lock')),false);
 }finally{
  if(previous===undefined)delete process.env.OPENCUT_PLUGIN_DATA_DIR;else process.env.OPENCUT_PLUGIN_DATA_DIR=previous;
  fs.rmSync(root,{recursive:true,force:true});
 }
});

test('installer registers one stable Codex command and keeps API attachment across versions', {
  skip: process.platform !== 'darwin',
}, () => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'opencut desktop mcp ')));
  const fixtureBinary = `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const args = process.argv.slice(2);
const reply = data => console.log(JSON.stringify({data,error:null}));
if (args[0] === '--json') reply({});
else if (args[0] === '--endpoint') {
  const input = JSON.parse(fs.readFileSync(0,'utf8'));
  console.log(JSON.stringify({id:input.id,args,executable:process.argv[1]}));
} else {
  const directory = args[5];
  const file = path.join(directory,'state.json');
  if (args[6] === 'status' && !fs.existsSync(file)) {
    reply({state:'stopped'});
    process.exitCode = 1;
  } else if (args[6] === 'stop') {
    fs.rmSync(file, { force: true });
    reply({state:'stopped',ready:false});
  } else if (args[6] === 'start' || args[6] === 'status') {
    fs.mkdirSync(directory,{recursive:true});
    const executable = fs.realpathSync(process.argv[1]);
    const state = {version:1,instance:'fixture',state:'running',ready:true,database:args[1],executable,
      executable_sha256:crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex'),
      endpoint:'http://127.0.0.1:'+process.env.OPENCUT_TEST_PORT};
    state.web_url = state.endpoint + '/editor';
    if (args[6] === 'start') fs.writeFileSync(file,JSON.stringify(state));
    reply(JSON.parse(fs.readFileSync(file,'utf8')));
  } else process.exitCode = 2;
}
`;
  const native = nativeFixture(fixtureBinary);
  try {
    const scripts = fileURLToPath(new URL('../../plugins/opencut/scripts', import.meta.url));
    const bundle = path.join(root, 'package');
    fs.cpSync(scripts, path.join(bundle, 'scripts'), { recursive: true });
    const archive = path.join(bundle, 'runtime.tar.gz');
    const packed = spawnSync('tar', ['-czf', archive, '-C', path.dirname(native), `${path.basename(native)}/`]);
    assert.equal(packed.status, 0, String(packed.stderr));
    const release = version => fs.writeFileSync(path.join(bundle, 'release.json'), JSON.stringify({
      version, runtime_top_dir: path.basename(native), runtime_archive_sha256: digest(archive),
      binary_sha256: digest(path.join(native, 'bin/opencut')),
      target: `${process.arch === 'x64' ? 'x86_64' : 'aarch64'}-apple-darwin`,
    }));
    const testBin = path.join(root, 'tools');
    fs.mkdirSync(testBin);
    const registry = path.join(root, 'codex-registry.json');
    const chatcut = { name: 'chatcut_desktop', enabled: true, transport: { type: 'stdio', command: '/fixture/chatcut' } };
    fs.writeFileSync(registry, JSON.stringify([chatcut]));
    fs.writeFileSync(path.join(testBin, 'codex'), `#!/usr/bin/env node
const fs = require('node:fs');
const file = process.env.OPENCUT_TEST_REGISTRY;
const entries = JSON.parse(fs.readFileSync(file,'utf8'));
const args = process.argv.slice(2);
if (args.join(' ') === 'mcp list --json') console.log(JSON.stringify(entries));
else if (args.length === 5 && args.slice(0,4).join(' ') === 'mcp add opencut_desktop --' &&
  !entries.some(e => e.name === 'opencut_desktop')) {
  entries.push({name:args[2],enabled:true,transport:{type:'stdio',command:args[4],args:[]}});
  fs.writeFileSync(file,JSON.stringify(entries));
} else process.exitCode = 2;
`, { mode: 0o700 });
    const data = path.join(root, 'data directory');
    const prefix = path.join(root, 'versions');
    const database = path.join(root, 'shared.sqlite');
    const environment = { ...process.env, PATH: `${testBin}:${process.env.PATH}`,
      OPENCUT_TEST_REGISTRY: registry, OPENCUT_TEST_PORT: '54321', OPENCUT_TEST_NODE: process.execPath,
      OPENCUT_PLUGIN_INSTALL_DIR: prefix, OPENCUT_PLUGIN_DATA_DIR: data, OPENCUT_PLUGIN_DATABASE: database };
    const install = (...args) => spawnSync(process.execPath, [path.join(bundle, 'scripts/install.mjs'), ...args], {
      env: environment, encoding: 'utf8', timeout: 20000,
    });
    const bootstrap = fileURLToPath(new URL('../install-desktop.sh', import.meta.url));
    const desktop = (...args) => spawnSync('/bin/sh', [bootstrap, ...args], {
      env: environment, encoding: 'utf8', timeout: 20000,
    });
    const successful = result => {
      assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`);
      return JSON.parse(result.stdout);
    };
    release('0.5.0-beta.7');
    const missing = desktop('doctor');
    assert.equal(missing.status, 1);
    assert.match(missing.stderr, /No OpenCut installation record/);
    const first = successful(install());
    const diagnosis = successful(desktop('doctor'));
    assert.equal(diagnosis.ready, true);
    assert.equal(diagnosis.binary_sha256, digest(path.join(first.runtime, 'bin/opencut')));
    assert.equal(fs.existsSync(path.join(data, 'server')), false);
    const stoppedCheck = desktop('check');
    assert.equal(stoppedCheck.status, 1);
    assert.equal(JSON.parse(stoppedCheck.stdout).service, 'stopped');
    const worker = path.join(first.runtime, 'bin/opencut-render-worker');
    const originalWorker = fs.readFileSync(worker);
    try {
      fs.appendFileSync(worker, 'corrupt');
      const damaged = desktop('doctor');
      assert.equal(damaged.status, 1);
      assert.match(damaged.stderr, /Runtime file differs: bin\/opencut-render-worker/);
    } finally {
      fs.writeFileSync(worker, originalWorker);
    }
    const command = path.join(data, 'opencut-mcp');
    assert.deepEqual(first.desktop_mcp, { name: 'opencut_desktop', transport: 'stdio', command, status: 'not_requested' });
    assert.deepEqual(JSON.parse(fs.readFileSync(registry)), [chatcut]);
    const defaultEnvironment = { ...environment, OPENCUT_PLUGIN_DATA_DIR: path.join(root, 'fresh-user') };
    delete defaultEnvironment.OPENCUT_PLUGIN_DATABASE;
    const fresh = successful(spawnSync(process.execPath, [path.join(bundle, 'scripts/install.mjs')], {
      env: defaultEnvironment, encoding: 'utf8', timeout: 20000,
    }));
    assert.equal(fresh.database, workspaceDatabase({}, path.join(os.homedir(), '.opencut', 'workspace.sqlite')));

    const zip = path.join(root, 'release fixture.zip');
    const pack = () => {
      fs.cpSync(bundle, path.join(root, 'opencut'), { recursive: true });
      fs.rmSync(zip, { force: true });
      const zipped = spawnSync('/usr/bin/zip', ['-qr', zip, 'opencut'], { cwd: root });
      assert.equal(zipped.status, 0, String(zipped.stderr));
      fs.writeFileSync(zip + '.sha256', `${digest(zip)}  release fixture.zip\n`);
    };
    pack();
    const installZip = () => desktop('install', '--archive', zip);
    fs.writeFileSync(zip + '.sha256', `${'0'.repeat(64)}  release fixture.zip\n`);
    assert.match(installZip().stderr, /checksum mismatch/);
    assert.deepEqual(JSON.parse(fs.readFileSync(registry)), [chatcut]);
    fs.writeFileSync(zip + '.sha256', `${digest(zip)}  release fixture.zip\n`);
    const registered = successful(installZip());
    assert.equal(registered.desktop_mcp.status, 'registered');
    assert.equal(registered.desktop_mcp.web_url, 'http://127.0.0.1:54321/editor');
    const initialRegistry = fs.readFileSync(registry, 'utf8');
    assert.deepEqual(JSON.parse(initialRegistry), [chatcut, { name: 'opencut_desktop', enabled: true,
      transport: { type: 'stdio', command, args: [] } }]);
    successful(install('--register-codex'));
    assert.equal(fs.readFileSync(registry, 'utf8'), initialRegistry);
    const invoke = (version, port) => {
      const result = successful(spawnSync(command, [], {
        input: JSON.stringify({ id: 31 }), encoding: 'utf8', timeout: 10000,
        env: { ...environment, OPENCUT_PLUGIN_DATA_DIR: path.join(root, 'incorrect'), OPENCUT_PLUGIN_DATABASE: '/wrong.sqlite' },
      }));
      assert.equal(result.id, 31);
      assert.equal(result.executable, path.join(prefix, version, 'runtime/bin/opencut'));
      assert.deepEqual(result.args, ['--endpoint', `http://127.0.0.1:${port}`, '--credential-file', path.join(data, 'server/api-token'), 'mcp', 'serve']);
    };
    invoke('0.5.0-beta.7', 54321);
    release('0.5.0-beta.8');
    pack();
    const update = () => desktop('update', '--archive', zip);
    const running = update();
    assert.equal(running.status, 1);
    assert.match(running.stderr, /Stop the current OpenCut service/);
    invoke('0.5.0-beta.7', 54321);
    assert.equal(successful(desktop('stop')).data.state, 'stopped');
    environment.OPENCUT_TEST_PORT = '54322';
    environment.OPENCUT_PLUGIN_DATABASE = path.join(root, 'stale.sqlite');
    const upgraded = successful(update());
    assert.equal(upgraded.desktop_mcp.command, command);
    assert.equal(upgraded.database, database);
    invoke('0.5.0-beta.8', 54322);
    assert.equal(fs.readFileSync(registry, 'utf8'), initialRegistry);
    release('0.5.0-beta.7');
    pack();
    assert.match(update().stderr, /Downgrades are disabled/);
    release('0.5.0-beta.8');
    const altered = JSON.parse(initialRegistry);
    altered[1].enabled = false;
    fs.writeFileSync(registry, JSON.stringify(altered));
    assert.equal(successful(install('--register-codex')).desktop_mcp.status, 'disabled');
    altered[1].transport.command = '/fixture/custom-command';
    fs.writeFileSync(registry, JSON.stringify(altered));
    const conflict = install('--register-codex');
    assert.equal(conflict.status, 1);
    const failed = JSON.parse(conflict.stdout);
    assert.equal(failed.installed, true);
    assert.equal(failed.desktop_mcp.status, 'setup_failed');
    assert.match(failed.desktop_mcp.error, /different opencut_desktop entry/);
    assert.deepEqual(JSON.parse(fs.readFileSync(registry)), altered);
    assert.equal(fs.existsSync(database), false);
  } finally {
    fs.rmSync(native, { recursive: true, force: true });
    fs.rmSync(root, { recursive: true, force: true });
  }
});
