#!/usr/bin/env python3
"""Exercise editorial plans through CLI/HTTP/MCP, atomic history and real A/V.

All media is synthetic. The stdio host runs its own managed Worker; no external
worker process is required. Reports bind to a frozen executable identity.
"""
import argparse,array,hashlib,json,math,os,selectors,shutil,socket,subprocess,tempfile,time,urllib.request,wave
from fractions import Fraction
from pathlib import Path

class Mcp:
    def __init__(self,binary,database,root,launch=None,trace=None,env=None):
        self.trace=trace
        self.log=(root/f'mcp-{time.time_ns()}.log').open('wb')
        self.process=subprocess.Popen(launch or [str(binary),'--database',str(database),'mcp','serve','--with-worker'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.log,env=env)
        self.selector=selectors.DefaultSelector();self.selector.register(self.process.stdout,selectors.EVENT_READ)
        self.buffer=bytearray();self.serial=0
        try:
            self.request('initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'editorial-acceptance','version':'1'}})
            self.send({'jsonrpc':'2.0','method':'notifications/initialized'})
        except BaseException:
            self.process.kill();self.process.wait();self.close();raise
    def send(self,value):
        self.process.stdin.write((json.dumps(value)+'\n').encode());self.process.stdin.flush()
    def request(self,method,params):
        self.serial+=1;self.send({'jsonrpc':'2.0','id':self.serial,'method':method,'params':params});deadline=time.monotonic()+60
        while time.monotonic()<deadline:
            if b'\n' not in self.buffer:
                assert self.selector.select(max(0,deadline-time.monotonic())),method
                chunk=os.read(self.process.stdout.fileno(),65536);assert chunk,'MCP exited'
                self.buffer.extend(chunk)
            while b'\n' in self.buffer:
                raw,_,rest=self.buffer.partition(b'\n');self.buffer[:]=rest;reply=json.loads(raw)
                if reply.get('id')==self.serial:
                    assert 'error' not in reply,reply
                    return reply['result']
        raise AssertionError(f'MCP timed out: {method}')
    def tool(self,name,value):
        reply=self.request('tools/call',{'name':name,'arguments':value})['structuredContent']
        assert reply['error'] is None,reply
        if self.trace is not None:self.trace.append({'transport':'mcp','tool':name,'request':value,'reply':reply})
        return reply['data']
    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try: assert self.process.wait(timeout=20)==0
            except BaseException:
                self.process.kill();self.process.wait();raise
        self.selector.close();self.process.stdout.close();self.log.close()

def verify(binary, root, config_generator=None):
    # Use the exact requested executable in place. Moving only its bytes can
    # select a different adjacent runtime and is not package identity evidence.
    binary = binary.resolve(strict=True)
    root.mkdir(parents=True, exist_ok=False)
    def digest(path):
        value = hashlib.sha256()
        with Path(path).open('rb') as source:
            for block in iter(lambda: source.read(65536), b''): value.update(block)
        return value.hexdigest()
    binary_sha = digest(binary)
    database = root / 'workspace.sqlite'
    checks, trace, executions = [], [], []
    mcp = host = None
    launch = None
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0)); port = listener.getsockname()[1]
    token = root / 'token'
    if config_generator:
        import tomllib
        subprocess.run(['python3', str(config_generator), '--binary', str(binary), '--database', str(database), '--output', str(root / 'agent-config'), '--with-worker'], check=True, capture_output=True, timeout=30)
        server = json.loads((root / 'agent-config/claude.mcp.json').read_text())['mcpServers']['opencut']
        codex = tomllib.loads((root / 'agent-config/codex.toml').read_text())['mcp_servers']['opencut']
        assert server['command'] == codex['command'] and server['args'] == codex['args']
        launch = [server['command'], *server['args']]
        checks.append('Delivered Codex/Claude configurations drive the actual managed MCP process')
    def call(name, value, error=None, credential=None):
        command = [str(binary), '--database', str(database), '--json']
        if credential: command += ['--credential-file', str(credential)]
        process = subprocess.run(command + ['call', name, '--input', '-'], input=json.dumps(value), text=True, capture_output=True, timeout=90)
        reply = json.loads(process.stdout)
        trace.append({'transport':'cli', 'tool':name, 'request':value, 'reply':reply})
        if error: assert process.returncode and reply['error']['code'] == error, reply
        else: assert process.returncode == 0 and reply['error'] is None, reply
        return reply['data']
    def current(project='film'):
        result = call('opencut_project_inspect', {'project_id':project, 'revision':None})
        assert result['kind'] == 'editor', result
        return result['snapshot']
    def tick(n): return {'ticks':str(n), 'timebase':10}
    def span(start, count): return {'start':tick(start), 'duration':tick(count)}
    def seconds(value): return Fraction(int(value['ticks']), value['timebase'])
    def interval(value): return (seconds(value['start']), seconds(value['duration']))
    def target(track, element): return {'track_id':track, 'element_id':element}
    def tracks(snapshot): return {row['id']:row for row in snapshot['project']['sequences'][0]['tracks']}
    def wait_job(accepted):
        deadline = time.monotonic() + 180
        while True:
            row = call('opencut_job_view', {'job_id':accepted['job_id']})
            if row['state'] in ('succeeded','failed','cancelled'): break
            assert time.monotonic() < deadline, row
            time.sleep(.1)
        assert row['state'] == 'succeeded', row
        executions.append({'job_id':row['job_id'], 'kind':row['kind'], 'result':row['result'], 'artifacts':row['artifacts']})
        return row
    def start_http():
        nonlocal host
        assert host is None
        with (root / 'http.log').open('ab') as log:
            host = subprocess.Popen([str(binary), '--database', str(database), 'serve', '--listen', f'127.0.0.1:{port}', '--token-file', str(token)], stdout=log, stderr=log)
        deadline = time.monotonic() + 20
        while True:
            assert host.poll() is None
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.2):
                    if token.exists(): return
            except OSError: pass
            assert time.monotonic() < deadline
            time.sleep(.05)
    def stop_http():
        nonlocal host
        if host is not None:
            host.terminate(); assert host.wait(timeout=20) == 0; host = None
    def http_tool(name, value):
        request = urllib.request.Request(f'http://127.0.0.1:{port}/api/tools/{name}', data=json.dumps(value).encode(), headers={'Authorization':'Bearer '+token.read_text().strip(), 'Content-Type':'application/json'})
        with urllib.request.urlopen(request, timeout=30) as response: reply = json.load(response)
        assert reply['error'] is None, reply
        trace.append({'transport':'http', 'tool':name, 'request':value, 'reply':reply})
        return reply['data']
    def download(job, filename):
        from urllib.parse import quote
        artifact = job['artifacts'][0]
        request = urllib.request.Request(f"http://127.0.0.1:{port}/api/artifacts/{quote(artifact['artifact_id'], safe='')}", headers={'Authorization':'Bearer '+token.read_text().strip()})
        path = root / filename
        with urllib.request.urlopen(request, timeout=30) as response, path.open('wb') as output:
            shutil.copyfileobj(response, output, 65536)
        assert path.stat().st_size == int(artifact['size_bytes']) and digest(path) == artifact['content_hash']
        return path
    def import_asset(file, identity):
        upload = mcp.tool('opencut_upload_begin', {'project_id':'film','idempotency_key':'import-'+identity,'name':file.name,'size_bytes':str(file.stat().st_size)})
        result = subprocess.run([str(binary),'--json','--database',str(database),'upload','put',upload['upload_id'],'--offset','0','--input',str(file)],capture_output=True,text=True,timeout=30)
        assert result.returncode == 0 and json.loads(result.stdout)['data']['next_offset'] == str(file.stat().st_size), result.stderr
        completed = mcp.tool('opencut_upload_complete', {'upload_id':upload['upload_id'],'content_hash':digest(file)})
        job = wait_job(completed)
        assert job['result']['kind'] == 'media.import'
        media = mcp.tool('opencut_project_media_inspect', {'project_id':'film','media_id':job['result']['data']['media_id']})
        mcp.tool('opencut_upload_release', {'upload_id':upload['upload_id']})
        return media
    def video(project, revision, key):
        return wait_job(mcp.tool('opencut_render_submit', {'kind':'video','request':{'project_id':project,'revision':revision,'sequence_id':'main','idempotency_key':key,'options':{'format':'mp4','quality':'high','include_audio':True}}}))
    def rgb(path, at):
        args = ['ffmpeg','-v','error']
        if at is not None: args += ['-ss', str(at)]
        return subprocess.check_output(args + ['-i',str(path),'-frames:v','1','-f','rawvideo','-pix_fmt','rgb24','-'],timeout=30)
    def pixels(path, only_blue=False):
        # Original independent solid-color/masked-black bounds; no renderer
        # helper or preview output generates the expected values.
        for at,point,channel in ([(None,(120,45),2),(None,(40,45),None)] if only_blue else [(0.5,(80,45),0),(1.5,(120,45),2),(1.5,(40,45),None)]):
            raw = rgb(path, at)
            value = raw[(point[1]*160+point[0])*3:(point[1]*160+point[0])*3+3]
            assert len(value) == 3
            if channel is None: assert max(value) < 8, list(value)
            else: assert value[channel] > 230 and all(v < 15 for i,v in enumerate(value) if i != channel), list(value)
    def sound(path, expected_first, excluded):
        metadata = json.loads(subprocess.check_output(['ffprobe','-v','error','-show_format','-of','json',str(path)],timeout=30))
        assert abs(float(metadata['format']['duration']) - 3) < .12, metadata
        pcm = array.array('f'); pcm.frombytes(subprocess.check_output(['ffmpeg','-v','error','-i',str(path),'-map','0:a:0','-f','f32le','-ac','1','-ar','48000','-'],timeout=30))
        if os.sys.byteorder != 'little': pcm.byteswap()
        for at,expected in [(.2,expected_first),(1.2,660),(2.2,660)]:
            segment = pcm[int(at*48000):int((at+.2)*48000)]
            energy = lambda hz: abs(sum(v*complex(math.cos(2*math.pi*hz*n/48000),math.sin(2*math.pi*hz*n/48000)) for n,v in enumerate(segment)))/len(segment)
            assert energy(expected) > .1 and energy(excluded) < .005, (at,energy(expected),energy(excluded))
    try:
        initial = call('opencut_project_create', {'project':{'id':'film','name':'Editorial acceptance','sequence_id':'main','width':160,'height':90,'fps':{'numerator':10,'denominator':1}},'idempotency_key':'create'})
        mcp = Mcp(binary,database,root,launch,trace)
        resources = {}
        for color in ('red','blue'):
            file = root / (color+'.mp4')
            subprocess.run(['ffmpeg','-v','error','-f','lavfi','-i',f'color={color}:s=160x90:r=10:d=6','-c:v','libx264','-pix_fmt','yuv420p',str(file)],check=True,timeout=30)
            resources[color] = import_asset(file,color)
        tone = root / 'tone.wav'; samples = array.array('h')
        for sample in range(48000*4):
            frequency = 330 if sample < 48000 else (440 if sample < 96000 else 660)
            value = round(32767*.25*math.sin(2*math.pi*frequency*sample/48000)); samples.extend((value,value))
        if os.sys.byteorder != 'little': samples.byteswap()
        with wave.open(str(tone),'wb') as wav:
            wav.setnchannels(2);wav.setsampwidth(2);wav.setframerate(48000);wav.writeframes(samples.tobytes())
        resources['tone'] = import_asset(tone,'tone')
        video_track = initial['project']['sequences'][0]['main_track_id']
        def placement(element, asset, track, start, offset, count, kind):
            content = {'kind':kind,'timing':{'kind':'source_range','range':span(offset,count),'rate':{'numerator':1,'denominator':1},'maintain_pitch':False}}
            if kind == 'video': content['source_audio_enabled'] = False
            return {'element_id':element,'name':element,'asset_id':asset,'track_id':track,'start':tick(start),'content':content}
        proposal = call('opencut_media_import_plan', {'project_id':'film','expected_revision':initial['revision'],'sequence_id':'main','idempotency_key':'seed','label':'Synthetic A/V',
            'bindings':[{'media_id':row['media_id'],'version':row['asset']['version'],'asset_id':name,'name':name,'mode':'bind'} for name,row in resources.items()],
            'new_tracks':[{'id':'audio','name':'Audio','kind':'audio'}], 'ripple':False,
            'placements':[placement('red-clip','red',video_track,0,10,20,'video'),placement('blue-clip','blue',video_track,20,10,20,'video'),placement('music','tone','audio',0,0,40,'audio')]})
        seed = proposal['transaction']
        blue = next(op['element'] for op in seed['operations'] if op['type'] == 'element_put' and op['element']['id'] == 'blue-clip')
        blue['visual']['masks'] = [{'id':'blue-mask','enabled':True,'inverted':False,'shape':{'kind':'rectangle','geometry':{'center_x':.25,'center_y':0.,'width':.5,'height':1.,'rotation_degrees':0.,'scale':1.}},'feather':{'unit':'classic_pixels','value':0.},'stroke':{'width_px':0.,'align':'outside','color':{'encoding':'srgb','red':0.,'green':0.,'blue':0.,'alpha':1.}}}]
        call('opencut_edit_plan',seed); call('opencut_edit_apply',seed)
        for file in (root/'red.mp4',root/'blue.mp4',tone): file.unlink()
        before = current()
        checks.append('MCP/CLI upload/import pins survive original source removal; one complete editor seed binds all resources and the independent right-half mask')
        split = lambda row, at, right: {'kind':'split','targets':[{'target':row,'right_element_id':right}],'at':tick(at),'retain':'both','ripple':False}
        request = {'project_id':'film','expected_revision':before['revision'],'idempotency_key':'shorten','label':'Remove middle second','steps':[
            {'kind':'timeline','sequence_id':'main','command':split(target(video_track,'red-clip'),10,'red-cut')},
            {'kind':'timeline','sequence_id':'main','command':split(target('audio','music'),10,'music-cut')},
            {'kind':'timeline','sequence_id':'main','command':split(target('audio','music-cut'),20,'music-tail')},
            {'kind':'timeline','sequence_id':'main','command':{'kind':'delete','targets':[target(video_track,'red-cut'),target('audio','music-cut')],'ripple':True}}]}
        plan = call('opencut_edit_agent_plan',request)
        assert plan == mcp.tool('opencut_edit',{'action':'agent.plan','input':request}) and current() == before
        retained = {'project_id':'film','expected_revision':before['revision'],'sequence_id':'main','idempotency_key':'retained-right','label':'Retain right','command':{'kind':'split','targets':[{'target':target(video_track,'red-clip'),'right_element_id':'red-right'},{'target':target('audio','music'),'right_element_id':'music-right'}],'at':tick(10),'retain':'right','ripple':True}}
        retained_plan = call('opencut_edit_timeline_plan',retained)
        assert retained_plan == mcp.tool('opencut_edit_timeline_plan',retained)
        for principal,actions in [('reader',['read']),('editor',['read','edit'])]:
            policy = {'principal_id':principal,'expected_revision':'0','enabled':True,'grants':[{'project_id':'film','actions':actions}]}
            subprocess.run([str(binary),'--database',str(database),'--json','access','set-policy','--input','-'],input=json.dumps(policy),text=True,capture_output=True,check=True,timeout=30)
            credential = root / (principal+'.credential')
            subprocess.run([str(binary),'--database',str(database),'--json','access','issue',principal,'--output',str(credential)],text=True,capture_output=True,check=True,timeout=30)
            observed = call('opencut_edit_agent_plan',request,credential=credential,error='NOT_AUTHORIZED' if principal == 'reader' else None)
            if principal == 'editor': assert observed == plan
            observed = call('opencut_edit_timeline_plan',retained,credential=credential,error='NOT_AUTHORIZED' if principal == 'reader' else None)
            if principal == 'editor': assert observed == retained_plan
            denied = subprocess.run([str(binary),'--database',str(database),'--credential-file',str(credential),'--json','mcp','serve','--with-worker'],input='',text=True,capture_output=True,timeout=30)
            assert denied.returncode == 3 and json.loads(denied.stdout)['error']['code'] == 'NOT_AUTHORIZED'
        start_http()
        assert http_tool('opencut_edit_agent_plan',request) == plan and http_tool('opencut_edit_timeline_plan',retained) == retained_plan
        assert current() == before
        stop_http()
        checks.append('CLI/HTTP/MCP agree on the sole complete planning contracts without writes;scoped readers cannot plan and scoped editors cannot launch a Worker')
        edit = mcp.tool('opencut_edit_apply',plan['transaction']); assert mcp.tool('opencut_edit_apply',plan['transaction']) == edit
        call('opencut_edit_agent_plan',request,error='REVISION_CONFLICT')
        after = current()
        assert [interval(row['timeline_range']) for row in tracks(after)[video_track]['elements']] == [(0,1),(1,2)]
        assert len(tracks(after)['audio']['elements']) == 2
        undo = {'project_id':'film','expected_revision':after['revision'],'target_transaction_id':edit['transaction_id'],'idempotency_key':'undo'}
        assert not call('opencut_edit_undo_plan',undo)['conflicts']
        undone = call('opencut_edit_undo_apply',undo); assert current()['project'] == before['project']
        call('opencut_edit_redo_apply',{'project_id':'film','expected_revision':undone['transaction']['revision'],'undo_transaction_id':undone['transaction']['transaction_id'],'idempotency_key':'redo'})
        assert current()['project'] == after['project']
        first_job = video('film',current()['revision'],'export')
        call('opencut_edit_undo_apply',{'project_id':'film','expected_revision':current()['revision'],'target_transaction_id':edit['transaction_id'],'idempotency_key':'return-before-split'})
        assert current()['project'] == before['project']
        retained.update(expected_revision=current()['revision'],idempotency_key='retained-right-commit')
        retained_plan = call('opencut_edit_timeline_plan',retained)
        committed = mcp.tool('opencut_edit_apply',retained_plan['transaction']); assert mcp.tool('opencut_edit_apply',retained_plan['transaction']) == committed
        split_after = current()
        right_video = next(row for row in tracks(split_after)[video_track]['elements'] if row['id'] == 'red-right')
        right_audio = next(row for row in tracks(split_after)['audio']['elements'] if row['id'] == 'music-right')
        assert interval(right_video['timeline_range']) == (0,1) and seconds(right_video['source_span']['trim_start']) == 2
        assert interval(right_audio['timeline_range']) == (0,3) and seconds(right_audio['source_span']['trim_start']) == 1
        retained_job = video('film',split_after['revision'],'retained-export')
        # Serve only completed outputs between media batches;no concurrent render
        # or browser acceptance is delegated to the supplemental HTTP host.
        start_http(); output = download(first_job,'shortened.mp4'); retained_output = download(retained_job,'retained.mp4'); stop_http()
        pixels(output); sound(output,330,440); pixels(retained_output); sound(retained_output,440,330)
        checks.append('One native commit preserves exact history;two combined A/V outputs satisfy original red/blue/black bounds and independent330/440/660Hz exclusion oracles')
        call('opencut_edit_undo_apply',{'project_id':'film','expected_revision':current()['revision'],'target_transaction_id':committed['transaction_id'],'idempotency_key':'retained-undo'})
        assert current()['project'] == before['project']
        request.update(expected_revision=current()['revision'],idempotency_key='restore-shortened-checkpoint')
        final_plan = call('opencut_edit_agent_plan',request); call('opencut_edit_apply',final_plan['transaction']); assert current()['project'] == after['project']
        original = current()
        copied = mcp.tool('opencut_project_copy',{'source_project_id':'film','source_revision':original['revision'],'project_id':'copy','name':'Copy','idempotency_key':'copy'})
        assert copied['project']['id'] == 'copy'
        upload = mcp.tool('opencut_upload_begin',{'project_id':'film','idempotency_key':'unfinished-upload','name':'pending.bin','size_bytes':'4'})
        chunk = root / 'chunk.bin'; chunk.write_bytes(b'abcd')
        sent = subprocess.run([str(binary),'--json','--database',str(database),'upload','put',upload['upload_id'],'--offset','0','--input',str(chunk)],capture_output=True,text=True,timeout=30)
        assert sent.returncode == 0 and json.loads(sent.stdout)['data']['next_offset'] == '4'
        deletion = {'projects':[{'project_id':'film','expected_revision':original['revision']}],'idempotency_key':'delete-film'}
        deleted = mcp.tool('opencut_project_delete',deletion); assert mcp.tool('opencut_project_delete',deletion) == deleted
        assert not deleted['cleanup_pending'] and deleted['receipt']['staging_upload_ids'] == [upload['upload_id']]
        released = mcp.tool('opencut_upload_retention',{'upload_id':upload['upload_id']}); assert released['state'] == 'released' and released['reserved_bytes'] == '0'
        call('opencut_project_inspect',{'project_id':'film','revision':original['revision']},error='NOT_FOUND')
        assert [row['project_id'] for row in mcp.tool('opencut_project_list',{'cursor':None,'limit':100})['items']] == ['copy']
        copy_before = current('copy')
        removal = mcp.tool('opencut_media_remove_plan',{'project_id':'copy','expected_revision':copy_before['revision'],'asset_ids':[row['id'] for row in copy_before['project']['assets']],'idempotency_key':'remove-media','label':'Remove media'})
        removed = mcp.tool('opencut_edit_apply',removal)
        empty = current('copy'); assert not empty['project']['assets'] and all(not row['elements'] for row in tracks(empty).values())
        mcp.tool('opencut_edit_undo_apply',{'project_id':'copy','expected_revision':empty['revision'],'target_transaction_id':removed['transaction_id'],'idempotency_key':'undo-media'})
        assert current('copy')['project'] == copy_before['project']
        copy_target = {'project_id':'copy','revision':current('copy')['revision'],'sequence_id':'main'}
        copy_job = video('copy',copy_target['revision'],'copy-export')
        mcp.close(); mcp = None
        queued = call('opencut_render_submit',{'kind':'frame','request':{**copy_target,'idempotency_key':'after-restart','time':tick(15)}})
        assert queued['state'] == 'queued'
        mcp = Mcp(binary,database,root,launch,trace); frame_job = wait_job(queued)
        mcp.process.terminate(); assert mcp.process.wait(timeout=20) == 0; mcp.close(); mcp = None
        start_http(); copy_export = download(copy_job,'copy.mp4'); frame = download(frame_job,'copy.png'); stop_http()
        pixels(copy_export); sound(copy_export,330,440); pixels(frame,True)
        checks.append('Complete copy survives original deletion,actual upload cleanup and media-removal undo;one persisted queued PNG proves managed Worker restart and SIGTERM cleanup without rerendering successful jobs')
        assert digest(binary) == binary_sha, 'Explicit candidate executable changed during verification'
        report = {'status':'passed','scope':'Sole-contract synthetic editorial/authority/history/retention/lifecycle with4combined outputs;not full S1/A7/D8 or exact reproducibility.','binary':str(binary),'binary_sha256':binary_sha,'checks':checks,'artifact':str(output),'artifact_sha256':digest(output),'copy_artifact':str(copy_export),'copy_artifact_sha256':digest(copy_export),'execution_observations':executions,'whole_migration_complete':False,'evidence':str(root)}
        (root/'report.json').write_text(json.dumps(report,indent=2)+'\n'); print(json.dumps(report,indent=2))
    finally:
        if mcp: mcp.close()
        if host is not None: stop_http()
        (root/'trace.json').write_text(json.dumps(trace,indent=2)+'\n')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=Path('target/debug/opencut'));parser.add_argument('--artifacts-dir',type=Path,default=Path(tempfile.gettempdir())/f'opencut-editorial-{time.time_ns()}');parser.add_argument('--agent-config-generator',type=Path);args=parser.parse_args()
    verify(args.binary.resolve(),args.artifacts_dir.resolve(),args.agent_config_generator.resolve() if args.agent_config_generator else None)
