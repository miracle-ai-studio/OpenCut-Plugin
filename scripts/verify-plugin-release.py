#!/usr/bin/env python3
"""Install an exact plugin ZIP in an owned synthetic workspace and exercise its launcher."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time
import zipfile

HERE = Path(__file__).resolve().parent


def sha(file):
    return hashlib.sha256(Path(file).read_bytes()).hexdigest()


def run(args, env=None, input=None):
    result = subprocess.run([str(x) for x in args], env=env, input=input, capture_output=True, text=True, timeout=180)
    if result.returncode:
        raise RuntimeError(result.stderr[-3000:] or result.stdout[-3000:])
    return result.stdout


def verify(archive, root):
    root.mkdir(parents=True, exist_ok=True)
    package_sha = sha(archive)
    source = root / ('unpacked-' + package_sha[:12])
    if not source.exists():
        with zipfile.ZipFile(archive) as package:
            if any(Path(n).is_absolute() or '..' in Path(n).parts for n in package.namelist()):
                raise ValueError('Unsafe ZIP path')
            package.extractall(source)
    plugin = source / 'opencut'
    env = {key: value for key, value in os.environ.items() if not key.startswith('OPENCUT_')}
    env.update(OPENCUT_PLUGIN_INSTALL_DIR=str(root / ('installation-' + package_sha[:12])),
               OPENCUT_PLUGIN_DATA_DIR=str(root / 'data'), OPENCUT_PLUGIN_DATABASE=str(root / 'data/workspace.sqlite'))
    fresh_data = root / ('fresh-data-' + package_sha[:12])
    fresh_env = dict(env, OPENCUT_PLUGIN_DATA_DIR=str(fresh_data), OPENCUT_PLUGIN_DATABASE=str(fresh_data / 'workspace.sqlite'))
    fresh_preexisting = (Path(fresh_env['OPENCUT_PLUGIN_DATA_DIR']) / 'install.json').exists()
    fresh = json.loads(run(['node', plugin / 'scripts/install.mjs'], fresh_env))
    fresh_binary = Path(fresh['runtime']) / 'bin/opencut'
    def fresh_budget(action, value):
        response = json.loads(run([fresh_binary, '--database', fresh['database'], '--json', 'domain', 'workspace', action, '--input', '/dev/stdin'], fresh_env, json.dumps(value)))
        assert response['error'] is None, response
        return response['data']
    current = fresh_budget('storage.budget.inspect', {})
    if not fresh_preexisting: assert current['media_output_limit_bytes'] == '9223372036854775807'
    assert current['media_output_limit_bytes'] in ('9223372036854775807', '8589934592')
    if current['media_output_limit_bytes'] == '9223372036854775807': fresh_budget('storage.budget.configure', {'expected_revision': current['policy']['revision'], 'idempotency_key': 'preserve-custom-test-budget', 'payload_limit_bytes': current['policy']['payload_limit_bytes'], 'minimum_available_bytes': current['policy']['minimum_available_bytes'], 'media_output_limit_bytes': '8589934592'})
    run(['node', plugin / 'scripts/install.mjs'], fresh_env)
    assert fresh_budget('storage.budget.inspect', {})['media_output_limit_bytes'] == '8589934592'
    installed = json.loads(run(['node', plugin / 'scripts/install.mjs'], env))
    repeated = json.loads(run(['node', plugin / 'scripts/install.mjs'], env))
    assert installed['marketplace'] == repeated['marketplace']
    doctor = json.loads(run(['node', plugin / 'scripts/doctor.mjs'], env))
    assert doctor['ready']
    runtime = Path(installed['runtime']); binary = runtime / 'bin/opencut'
    installed_plugin = Path(installed['marketplace']) / 'plugins/opencut'
    for name in ('plugin.json', 'mcp.json', 'scripts/launch.sh', 'scripts/launch.mjs', 'scripts/runtime.mjs'):
        assert sha(installed_plugin / name) == sha(plugin / name), name
    manifest = json.loads((runtime / 'manifest.json').read_text())
    assert manifest['native_renderer']['format'] == 'opencut.native-renderer'
    state_file = root / ('checkpoint-' + sha(binary)[:12] + '.json')
    if state_file.exists():
        state = json.loads(state_file.read_text())
    else:
        state = {'binary_sha256': sha(binary), 'stages': {}}
        retained = root / 'checkpoint.json'
        if retained.exists():
            parent = json.loads(retained.read_text())
            state['fixture_origin_binary_sha256'] = parent['binary_sha256']
            # Editing contracts are unchanged. Reuse only immutable fixture
            # setup; no render receipt from another binary counts as passed.
            state['stages'] = {k: v for k, v in parent['stages'].items() if k in ('create', 'media', 'plan', 'apply', 'cover')}
    assert state['binary_sha256'] == sha(binary)

    def stage(name, action):
        if name not in state['stages']:
            state['stages'][name] = action()
            state_file.write_text(json.dumps(state, indent=2) + '\n')
        return state['stages'][name]

    spec = importlib.util.spec_from_file_location('editorial_release_mcp', HERE / 'verify-editorial.py')
    editorial = importlib.util.module_from_spec(spec); spec.loader.exec_module(editorial)
    mcp = editorial.Mcp(binary, root / 'data/workspace.sqlite', root, ['/bin/sh', str(installed_plugin / 'scripts/launch.sh')], env=env)
    project_id = 'plugin-release-check'
    checks = ['ZIP and host runtime file identities', 'Relocated versioned installation', 'Idempotent installation preserving an existing custom budget', 'Fresh workspace retains automatic output sizing; no fixed small file cap', 'Base dependency and executable doctor', 'Packaged stdio launcher initialization']
    try:
        tools = mcp.request('tools/list', {})['tools']
        assert 0 < len(tools) <= 12
        mcp.tool('opencut_system', {'action': 'capabilities', 'input': {}})
        mcp.request('resources/read', {'uri': 'opencut://schemas/domains'})
        checks.append('Tool discovery, resources and capabilities through the installed launcher')
        budget = mcp.tool('opencut_storage_budget_inspect', {})
        if budget['media_output_limit_bytes'] != '9223372036854775807':
            assert state.get('fixture_origin_binary_sha256'), 'Only the retained owned fixture may restore its temporary test cap'
            mcp.tool('opencut_storage_budget_configure', {
                'expected_revision': budget['policy']['revision'],
                'idempotency_key': 'plugin-owned-fixture-restore-auto-output',
                'payload_limit_bytes': budget['policy']['payload_limit_bytes'],
                'minimum_available_bytes': budget['policy']['minimum_available_bytes'],
                'media_output_limit_bytes': '9223372036854775807'})
        assert mcp.tool('opencut_storage_budget_inspect', {})['media_output_limit_bytes'] == '9223372036854775807'
        checks.append('Automatic output sizing with independent database heartbeat headroom')
        initial = stage('create', lambda: mcp.tool('opencut_project_create', {'project': {'id': project_id, 'name': 'Synthetic plugin check', 'sequence_id': 'main', 'width': 160, 'height': 90, 'fps': {'numerator': 10, 'denominator': 1}}, 'idempotency_key': 'plugin-create'}))

        def wait_job(request):
            deadline = time.monotonic() + 300
            while time.monotonic() < deadline:
                view = mcp.tool('opencut_job_view', {'job_id': request['job_id']})
                if view['state'] == 'succeeded': return view
                assert view['state'] not in ('failed', 'cancelled'), view
                time.sleep(.25)
            raise AssertionError('Observe the existing job before retrying; checkpoint retains its request')

        def import_media():
            file = root / 'red.mp4'
            if not file.exists(): run(['ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', 'color=red:s=160x90:r=10:d=1', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', file])
            upload = mcp.tool('opencut_upload_begin', {'project_id': project_id, 'idempotency_key': 'plugin-upload', 'name': 'red.mp4', 'size_bytes': str(file.stat().st_size)})
            run([binary, '--database', root / 'data/workspace.sqlite', '--json', 'upload', 'put', upload['upload_id'], '--offset', '0', '--input', file])
            imported = mcp.tool('opencut_upload_complete', {'upload_id': upload['upload_id'], 'content_hash': sha(file)})
            view = wait_job(imported)
            return mcp.tool('opencut_project_media_inspect', {'project_id': project_id, 'media_id': view['result']['data']['media_id']})

        media = stage('media', import_media)
        inspected = mcp.tool('opencut_project_inspect', {'project_id': project_id})
        assert inspected['kind'] == 'editor'
        track = inspected['snapshot']['project']['sequences'][0]['main_track_id']
        transaction = stage('plan', lambda: mcp.tool('opencut_media_import_plan', {
            'project_id': project_id, 'expected_revision': initial['revision'], 'sequence_id': 'main',
            'idempotency_key': 'plugin-seed', 'label': 'Synthetic red clip', 'ripple': False, 'new_tracks': [],
            'bindings': [{'media_id': media['media_id'], 'version': media['asset']['version'], 'asset_id': 'red', 'name': 'red', 'mode': 'bind'}],
            'placements': [{'element_id': 'red-clip', 'name': 'Red', 'asset_id': 'red', 'track_id': track, 'start': {'ticks': '0', 'timebase': 10}, 'content': {'kind': 'video', 'timing': {'kind': 'source_range', 'range': {'start': {'ticks': '0', 'timebase': 10}, 'duration': {'ticks': '10', 'timebase': 10}}, 'rate': {'numerator': 1, 'denominator': 1}, 'maintain_pitch': False}, 'source_audio_enabled': False}}]
        })['transaction'])
        seeded = stage('apply', lambda: mcp.tool('opencut_edit_apply', transaction))
        replay = mcp.tool('opencut_edit_apply', transaction)
        assert replay == seeded
        checks.append('Retained CLI/MCP import and edit fixture reused; current binary verified inspection and idempotent edit replay' if state.get('fixture_origin_binary_sha256') else 'CLI byte upload plus MCP import, plan/apply and idempotent replay')
        cover_request = {'project_id': project_id, 'expected_revision': seeded['revision'], 'idempotency_key': 'plugin-cover', 'label': 'Source frame cover', 'operations': [{'type': 'cover_set', 'cover': {'kind': 'video_frame', 'asset_id': 'red', 'time': {'ticks': '2', 'timebase': 10}}}]}
        covered = stage('cover', lambda: mcp.tool('opencut_edit_apply', cover_request))
        snapshot = mcp.tool('opencut_project_inspect', {'project_id': project_id})
        assert snapshot['snapshot']['project']['cover']['kind'] == 'video_frame'
        checks.append('Video-frame cover persisted in the project revision')
        submitted = stage('automatic-render-submit', lambda: mcp.tool('opencut_render_submit', {'kind': 'video', 'request': {'project_id': project_id, 'revision': covered['revision'], 'sequence_id': 'main', 'idempotency_key': 'plugin-render-auto-' + sha(binary)[:12], 'options': {'format': 'mp4', 'quality': 'high', 'include_audio': False}}}))
        result = stage('automatic-render-result', lambda: wait_job(submitted))
        artifact = result['artifacts'][0]
        # The verifier inspects its own fixture's CAS bytes; clients use authorized artifact handles.
        candidates = [p for p in (root / 'data').rglob('*') if p.is_file() and p.stat().st_size == int(artifact['size_bytes'])]
        output = next(p for p in candidates if sha(p) == artifact['content_hash'])
        frame = subprocess.check_output(['ffmpeg', '-v', 'error', '-i', str(output), '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'], timeout=30)
        center = frame[(45 * 160 + 80) * 3:(45 * 160 + 80) * 3 + 3]
        pixel_probe = {'passed': center[0] > 230 and center[1] < 15 and center[2] < 15, 'actual_rgb': list(center), 'expected': 'red > 230; green < 15; blue < 15', 'scope': 'Center pixel of one untagged synthetic H264 source through the delivered renderer; thresholds unchanged'}
        checks.append('Completed export and artifact content hash matched')
    finally:
        mcp.close()
    report = {'status': 'passed' if pixel_probe['passed'] else 'package_and_workflow_passed_pixel_probe_failed', 'package_ready': True, 'workflow_passed': True, 'pixel_probe': pixel_probe, 'fixture_origin_binary_sha256': state.get('fixture_origin_binary_sha256'), 'fixture_reuse_scope': 'Existing imported/edited project and source-frame cover reused; current binary verified inspection, idempotent replay and a new export. No earlier render receipt is counted for this binary.' if state.get('fixture_origin_binary_sha256') else 'Created all fixture stages with this binary', 'package_sha256': package_sha, 'binary_sha256': sha(binary), 'runtime_manifest_sha256': sha(runtime / 'manifest.json'), 'native_renderer': manifest['native_renderer'], 'runtime_bundle': manifest['runtime_bundle'], 'target': doctor['target'], 'checks': checks, 'default_mcp_tools': len(tools), 'client_conversation_acceptance': 'not_run; direct MCP protocol client exercises the installed launcher', 'clean_machine_acceptance': 'not_run; isolated installation on the current host, bundled native dependencies and external Node bootstrap', 'optional_font_transcription': 'not_run', 'scope': 'macOS local plugin package and one synthetic import/edit/cover/export flow; not whole-product or physical audio acceptance'}
    (root / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--workspace', type=Path, required=True)
    args = parser.parse_args()
    report = verify(args.archive.resolve(), args.workspace.resolve())
    raise SystemExit(0 if report['pixel_probe']['passed'] else 1)
