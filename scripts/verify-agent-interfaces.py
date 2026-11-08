#!/usr/bin/env python3
"""Focused CLI/MCP acceptance against one explicit binary and one owned workspace.

Requires jsonschema for an independent draft-2020-12 input-schema oracle.
Creates one tiny synthetic WAV for a probe, never reads user media or installs plugins.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import selectors
import subprocess
import time
import wave

class Mcp:
    def __init__(self, config):
        self.process = subprocess.Popen([config['command'], *config['args']],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.buffer = bytearray()
        self.notifications = []
        self.serial = 0
        self.request('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {},
            'clientInfo': {'name': 'agent-interface-acceptance', 'version': '1'}})
        self.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})

    def send(self, message):
        self.process.stdin.write((json.dumps(message) + '\n').encode())
        self.process.stdin.flush()

    def receive(self, timeout=20):
        end = time.monotonic() + timeout
        while b'\n' not in self.buffer:
            assert self.selector.select(max(0, end - time.monotonic())), 'MCP read timed out'
            chunk = os.read(self.process.stdout.fileno(), 65536)
            assert chunk, ('MCP closed', self.process.poll(), self.process.stderr.read().decode())
            self.buffer.extend(chunk)
        line, _, rest = self.buffer.partition(b'\n')
        self.buffer[:] = rest
        return json.loads(line)

    def request(self, method, params):
        self.serial += 1
        self.send({'jsonrpc': '2.0', 'id': self.serial, 'method': method, 'params': params})
        while True:
            message = self.receive()
            if message.get('id') == self.serial:
                assert 'error' not in message, message
                return message['result']
            self.notifications.append(message)

    def resource(self, uri):
        return json.loads(self.request('resources/read', {'uri': uri})['contents'][0]['text'])

    def updated(self, uri):
        end = time.monotonic() + 5
        while not any(n.get('method') == 'notifications/resources/updated'
                      and n['params']['uri'] == uri for n in self.notifications):
            self.notifications.append(self.receive(max(0, end - time.monotonic())))

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                raise
            assert self.process.returncode == 0, self.process.stderr.read().decode()
        self.selector.close()


def verify(binary, root, *, api_mcp=False):
    from jsonschema import Draft202012Validator

    db, service = root / 'workspace.sqlite', root / 'service'
    owner = root / 'interface-workspace.json'
    identity = {'format': 'opencut.agent-interface-acceptance', 'database': str(db)}
    if db.exists():
        assert owner.exists() and json.loads(owner.read_text()) == identity, 'Never reuse a user workspace'
    else:
        owner.write_text(json.dumps(identity))
    common = [str(binary), '--database', str(db)]
    remote = []
    checks, clients = [], []

    def run(args, body=None, network=False, error=None):
        command = common + (remote if network else []) + ['--json', *args]
        completed = subprocess.run(command, input=None if body is None else json.dumps(body),
                                   capture_output=True, text=True, timeout=30)
        envelope = json.loads(completed.stdout)
        if error:
            assert completed.returncode != 0 and envelope['error']['code'] == error, envelope
            return envelope['error']
        assert completed.returncode == 0 and envelope.get('error') is None, (envelope, completed.stderr)
        return envelope['data']

    def call(name, body, network=True):
        return run(['call', name, '--input', '-'], body, network)

    def server(*args):
        return run(['server', '--directory', str(service), *args])

    try:
        state = server('start', '--listen', '127.0.0.1:0')
        assert state['ready'] is True
        remote[:] = ['--endpoint', state['endpoint'], '--credential-file', str(service / 'api-token')]
        spec = importlib.util.spec_from_file_location('agent_plugin',
            Path(__file__).resolve().parent / 'distribution/agent-plugin.py')
        plugin = importlib.util.module_from_spec(spec); spec.loader.exec_module(plugin)
        bundle = root / ('plugins-' + hashlib.sha256(binary.read_bytes()).hexdigest()[:12])
        if bundle.exists():
            receipt = json.loads((bundle / 'manifest.json').read_text())
        else:
            receipt = plugin.generate(binary, db, bundle, service_directory=service)
        assert receipt['managed_worker'] is False
        configs = [json.loads((bundle / client / 'plugins/opencut/.mcp.json').read_text())['mcpServers']['opencut']
                   for client in ('codex', 'claude')]
        assert configs[0] == configs[1] and '--with-worker' not in configs[0]['args']
        if api_mcp:
            # The same fixture and assertions exercise API-backed stdio. An
            # intentionally absent local DB proves no SQLite fallback is opened.
            unused_mcp_database = root / 'api-mcp-must-not-open' / 'workspace.sqlite'
            api_config = {'command': str(binary), 'args': [
                '--database', str(unused_mcp_database), *remote, 'mcp', 'serve']}
            active_configs = [api_config, api_config]
        else:
            active_configs = configs
        c = Mcp(active_configs[0]); clients.append(c)
        tools = c.request('tools/list', {})['tools']
        catalog = run(['system', 'domains'])
        validators = {}
        for tool in tools:
            domain = next(d for d in catalog if 'opencut_' + d['domain'] == tool['name'])
            schema = tool['inputSchema']
            assert set(schema['properties']['action']['enum']) == {a['action'] for a in domain['actions']}
            assert len(schema['oneOf']) == len(domain['actions'])
            Draft202012Validator.check_schema(schema)
            validators[tool['name']] = Draft202012Validator(schema)
        assert len(tools) == 11
        for domain, action in [('edit', 'timeline.plan'), ('observe', 'event.changes'), ('project', 'package.export')]:
            schema = next(t['inputSchema'] for t in tools if t['name'] == 'opencut_' + domain)
            assert action in schema['properties']['action']['enum']
        for payload in [{'action': 'inspect', 'input': {}},
                        {'action': 'inspect', 'input': {'project_id': 7}},
                        {'action': 'not-an-action', 'input': {}}]:
            assert not validators['opencut_project'].is_valid(payload), payload
        assert validators['opencut_project'].is_valid({'action': 'inspect', 'input': {'project_id': 'acceptance'}})
        (root / 'tools.json').write_text(json.dumps(tools, ensure_ascii=False, indent=2))
        checks.append('11 domain tools advertise every registered action; independent schemas reject missing/wrong input')

        for args in [['system', 'domains'], ['system', 'domains', '--domain', 'edit', '--schemas'],
                     ['system', 'schema'], ['system', 'events-schema']]:
            assert run(args, network=True) == run(args), args
        project = {'id': 'interface-acceptance', 'name': '接口验证', 'sequence_id': 'main',
                   'width': 64, 'height': 48, 'fps': {'numerator': 30, 'denominator': 1}}
        created = run(['project', 'create', '--input', '-', '--idempotency-key', 'create'], project, True)
        pid = project['id']
        initial = run(['project', 'inspect', pid], network=True)
        assert initial == run(['project', 'inspect', pid])
        unused = root / 'remote-must-not-open.sqlite'
        read_remote = subprocess.run([str(binary), '--database', str(unused), *remote,
            '--json', 'project', 'inspect', pid], capture_output=True, text=True, timeout=20)
        assert read_remote.returncode == 0 and json.loads(read_remote.stdout)['data'] == initial
        assert not unused.exists()
        assert run(['project', 'list'], network=True) == run(['project', 'list'])
        templates = c.request('resources/templates/list', {})['resourceTemplates']
        assert any(t['uriTemplate'] == 'opencut://projects/{project}/branches/{branch}/summary' for t in templates)
        head = f'opencut://projects/{pid}/branches/main/summary'
        frozen_uri = f'opencut://projects/{pid}/revisions/{initial["snapshot"]["revision"]}/summary'
        frozen = c.resource(frozen_uri)
        c.request('resources/subscribe', {'uri': head})
        change = {'expected_revision': initial['snapshot']['revision'], 'idempotency_key': 'rename',
                  'label': 'Rename project', 'operations': [{'type': 'rename', 'name': '可读的新名称'}]}
        planned = run(['edit', 'plan', '--project', pid, '--input', '-'], change, True)
        assert planned['name'] == '可读的新名称'
        assert run(['project', 'inspect', pid], network=True) == initial
        validators['opencut_edit'].validate({'action': 'apply', 'input': {'project_id': pid, **change}})
        broken = json.loads(json.dumps(change)); broken['operations'][0]['name'] = 9
        assert not validators['opencut_edit'].is_valid({'action': 'apply', 'input': {'project_id': pid, **broken}})
        if api_mcp:
            request = {'name': 'opencut_edit', 'arguments': {
                'action': 'apply', 'input': {'project_id': pid, **change}}}
            result = c.request('tools/call', request)
            assert not result.get('isError'), result
            applied = result['structuredContent']['data']
            replay = c.request('tools/call', request)
            assert not replay.get('isError') and replay['structuredContent']['data'] == applied
        else:
            applied = run(['edit', 'apply', '--project', pid, '--input', '-'], change, True)
        c.updated(head)
        current = run(['project', 'inspect', pid], network=True)
        assert c.resource(head)['payload']['data']['summary']['revision'] == current['snapshot']['revision']
        assert c.resource(frozen_uri) == frozen
        if api_mcp:
            prompt = c.request('prompts/get', {'name': 'caption_review', 'arguments': {
                'project_id': pid, 'revision': current['snapshot']['revision'], 'sequence_id': 'main'}})
            assert prompt['messages']
            assert not unused_mcp_database.parent.exists()
            checks.append('API-backed stdio tools, fixed resources, prompts and bounded subscriptions preserve writes/replay without opening the local database')
        assert run(['edit', 'apply', '--project', pid, '--input', '-'], change, True) == applied
        run(['edit', 'apply', '--project', pid, '--input', '-'],
            {**change, 'idempotency_key': 'stale'}, True, error='REVISION_CONFLICT')
        assert run(['project', 'history', pid], network=True) == run(['project', 'history', pid])
        assert run(['project', 'revision', pid, initial['snapshot']['revision']], network=True) == initial
        checks.append('Remote create/list/inspect/plan/apply/history/revision reuse local semantics, replay and stale-head rejection')
        checks.append('Discovered current-head subscription observes remote CLI edits; immutable revision stays unchanged')

        events = run(['events', '--project-id', pid], network=True)
        assert events == run(['events', '--project-id', pid])
        args = ['events', '--project-id', pid, '--format', 'ndjson']
        stream = subprocess.run(common + remote + args, text=True, capture_output=True, timeout=20)
        rows = [json.loads(line) for line in stream.stdout.splitlines()]
        assert stream.returncode == 0 and rows[0]['type'] == 'ready' and rows[-1]['type'] == 'end'
        assert [row['event'] for row in rows if row['type'] == 'event'] == [e['event'] for e in events['events']]
        resumed = subprocess.run(common + remote + args + ['--cursor', rows[-1]['cursor'], '--watch', '--timeout-seconds', '1'],
                                 text=True, capture_output=True, timeout=10)
        assert resumed.returncode == 124 and '"type":"event"' not in resumed.stdout
        checks.append('Remote NDJSON emits the same events, resumes after cursor, and times out without cancellation')

        content = b'bounded remote upload fixture'
        chunk = root / 'chunk.bin'; chunk.write_bytes(content)
        upload = call('opencut_upload_begin', {'project_id': pid, 'idempotency_key': 'upload',
                      'name': 'chunk.bin', 'size_bytes': str(len(content))})
        uploaded = run(['upload', 'put', upload['upload_id'], '--offset', '0', '--input', str(chunk)], network=True)
        assert uploaded['next_offset'] == str(len(content))
        assert run(['upload', 'put', upload['upload_id'], '--offset', '0', '--input', str(chunk)], network=True) == uploaded
        assert call('opencut_upload_abort', {'upload_id': upload['upload_id']})['state'] == 'aborted'
        checks.append('Remote raw upload keeps local bytes out of JSON and preserves exact chunk replay')

        c.close()
        assert server('status')['instance'] == state['instance']
        second = Mcp(active_configs[1]); clients.append(second); second.close()
        assert server('status')['ready'] is True
        # Work submitted after both Agent sessions exit still has an independent Worker.
        wav = root / 'probe.wav'
        with wave.open(str(wav), 'wb') as output:
            output.setparams((1, 2, 8000, 800, 'NONE', 'not compressed'))
            output.writeframes(b'\0\0' * 800)
        job = run(['job', 'start', '--input', '-'], {'kind': 'media.probe',
                  'idempotency_key': 'post-disconnect-probe', 'input': {'input': str(wav)}}, True)
        finished = run(['job', 'wait', job['job_id'], '--timeout-seconds', '15'], network=True)
        assert finished['state'] == 'succeeded'
        assert run(['job', 'get', job['job_id']], network=True) == finished
        assert run(['job', 'list'], network=True) == run(['job', 'list'])
        assert run(['job', 'events', '--job-id', job['job_id']], network=True) == run(['job', 'events', '--job-id', job['job_id']])
        assert server('status')['instance'] == state['instance']
        checks.append('Both generated stdio configs disconnect cleanly; same independent service executes and waits for a subsequent probe')

        # Startup identity is pinned; changing the on-disk path alone cannot
        # silently bless a different running executable.
        state_path = service / 'state.json'
        original_state = state_path.read_bytes()
        mismatched_state = json.loads(original_state)
        mismatched_state['executable_sha256'] = '0' * 64
        try:
            state_path.write_text(json.dumps(mismatched_state))
            mismatch = subprocess.run([configs[0]['command'], *configs[0]['args']],
                                      input='', capture_output=True, text=True, timeout=15)
            assert mismatch.returncode != 0 and 'SERVICE_CANDIDATE_MISMATCH' in mismatch.stderr
        finally:
            state_path.write_bytes(original_state)

        wrong = root / 'wrong.sqlite'
        bad = subprocess.run([str(binary), '--database', str(wrong), 'mcp', 'serve', '--service-directory', str(service)],
                             input='', capture_output=True, text=True, timeout=15)
        assert bad.returncode != 0 and not wrong.exists()
        server('stop')
        absent = subprocess.run([configs[0]['command'], *configs[0]['args']], input='', capture_output=True, text=True, timeout=15)
        assert absent.returncode != 0 and 'SERVICE_NOT_READY' in absent.stderr
        checks.append('Mismatched startup build, wrong workspace and unavailable service reject attachment before opening another database')
        return {'status': 'passed', 'binary': str(binary), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                'workspace': str(db), 'api_mcp': api_mcp, 'checks': checks, 'domain_tools': len(tools),
                'advertised_actions': sum(len(t['inputSchema']['oneOf']) for t in tools),
                'compact_tools_json_bytes': len(json.dumps(tools, ensure_ascii=False, separators=(',', ':')).encode()),
                'probe_job_id': job['job_id'], 'limitations': ['Protocol clients only; real model/client compatibility is not accepted',
                'No renderer, UI, remote TLS or full permission matrix acceptance']}
    finally:
        for client in clients:
            if client.process.poll() is None:
                client.process.kill(); client.process.wait(timeout=5)
        if service.exists():
            subprocess.run(common + ['server', '--directory', str(service), 'stop'], capture_output=True, timeout=40)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--artifacts-dir', type=Path, required=True)
    parser.add_argument('--api-mcp', action='store_true', help='Exercise API-backed stdio against the same owned service; never opens the client-side database')
    options = parser.parse_args()
    root = options.artifacts_dir.resolve(); root.mkdir(parents=True, exist_ok=True)
    report = verify(options.binary.resolve(strict=True), root, api_mcp=options.api_mcp)
    (root / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(report, ensure_ascii=False, indent=2))
