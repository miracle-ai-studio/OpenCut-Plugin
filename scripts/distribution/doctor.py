#!/usr/bin/env python3
"""Inspect a local bundle and its actual media runtime without opening a workspace."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile

# Required by the current renderer; project-specific preflight is authoritative.
FILTERS = {
    'video': {'color', 'fps', 'scale', 'pad', 'crop', 'format', 'overlay', 'trim', 'setpts', 'select'},
    'visual': {'geq', 'eq', 'gblur'},
    'audio': {'anullsrc', 'atrim', 'asetpts', 'aresample', 'aformat', 'asetrate', 'volume', 'adelay', 'apad', 'amix'},
}
ENCODERS = {'libx264', 'aac', 'png'}
CATALOG_FILES = {'bin/libexec/classic-visual-renderer/package.json',
                 'bin/libexec/classic-visual-renderer/host/catalog-archive.mjs'}


def runtime_module():
    spec = importlib.util.spec_from_file_location('opencut_runtime_bundle_doctor', Path(__file__).with_name('runtime_bundle.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def runtime_probe(args, timeout=20):
    # Probes may create child processes. Terminate the whole owned process
    # group on failure/timeout, including when the direct Node child exits first.
    child = subprocess.Popen([str(p) for p in args], stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, start_new_session=True)
    try:
        stdout, stderr = child.communicate(timeout=timeout)
        if child.returncode != 0 or len(stdout) > 1024 * 1024 or len(stderr) > 1024 * 1024:
            detail = stderr[-2048:].decode('utf-8', errors='replace').strip()
            raise ValueError('Bounded runtime probe failed' + (': ' + detail if detail else ''))
        return stdout.decode('utf-8')
    finally:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()


def declared_runtime(root, outer):
    declaration = outer['runtime_bundle']
    if set(declaration) != {'manifest', 'sha256', 'delivery'} or declaration['manifest'] != 'bin/libexec/runtime-bundle.json':
        raise ValueError('Invalid runtime manifest pointer')
    path = root / declaration['manifest']
    if declaration['sha256'] != sha(path) or outer['files'][declaration['manifest']]['sha256'] != declaration['sha256']:
        raise ValueError('Runtime manifest identity mismatch')
    runtime = runtime_module().verify(path.parent)
    if runtime['target'] != outer['target'] or runtime['delivery'] != declaration['delivery']:
        raise ValueError('Runtime target or delivery state mismatch')
    expected_links = {'bin/libexec/' + name: value['target'] for name, value in runtime['files'].items() if value['kind'] == 'symlink'}
    if outer.get('links') != expected_links:
        raise ValueError('Runtime links differ from bundle inventory')
    for name, value in runtime['files'].items():
        if value['kind'] == 'file' and outer['files'].get('bin/libexec/' + name) != {'sha256': value['sha256'], 'size': value['size']}:
            raise ValueError('Runtime bytes differ from bundle inventory')
    return runtime


def runtime_executable(root, runtime, component, variables, path_fallback=False, configured_relative=False):
    for variable in variables:
        if variable in os.environ:
            value = os.environ[variable]
            if value and configured_relative and not Path(value).is_absolute():
                value = shutil.which(value) if '/' not in value else str(Path(value).resolve())
            break
    else:
        bundled = runtime.get('components', {}).get(component) if runtime else None
        value = str(root / 'bin/libexec' / bundled['entry']) if bundled else (shutil.which(component) if path_fallback else None)
    if value and configured_relative:
        value = str(Path(value).resolve())
    if not value or not Path(value).is_absolute() or not Path(value).is_file() or not os.access(value, os.X_OK):
        raise ValueError('Configured ' + component + ' executable is unavailable')
    return Path(value).resolve()


def check_renderer_and_fonts(root, runtime, checks):
    libexec = root / 'bin/libexec'
    try:
        spec = importlib.util.spec_from_file_location('opencut_native_runtime_doctor', Path(__file__).with_name('native_runtime.py'))
        native = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(native)
        outer = json.loads((root / 'manifest.json').read_text())
        declaration = native.verify(root, outer['native_renderer'])
        if any(outer['files'].get(name) != entry for name, entry in declaration['files'].items()):
            raise ValueError('Native renderer differs from outer bundle inventory')
        value = native.probe_worker(root, declaration)
        checks.append({'check': 'native_renderer_runtime_executes', 'passed': True, **value})
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
        checks.append({'check': 'native_renderer_runtime_executes', 'passed': False, 'error': str(error)})
    try:
        python = runtime_executable(root, runtime, 'python', ('OPENCUT_FONT_DERIVE_PYTHON',))
        provenance = json.loads(runtime_probe([python, '-I', '-B', '-c',
            'import sys,json,fontTools; print(json.dumps({"prefix":sys.prefix,"fonttools":fontTools.__file__}))']))
        if runtime['components'].get('python') and 'OPENCUT_FONT_DERIVE_PYTHON' not in os.environ:
            prefix = (libexec / 'runtimes/python').resolve()
            if not all(Path(provenance[k]).resolve().is_relative_to(prefix) for k in ('prefix', 'fonttools')):
                raise ValueError('Bundled Python still depends on an external installation')
        identities = {}
        workers = [('run_derived.py', 'OPENCUT_FONT_DERIVE_SCRIPT', 'derived-runtime-lock.json'),
                   ('run_derived_v2.py', 'OPENCUT_FONT_NAMED_SCRIPT', 'derived-v2-runtime-lock.json')]
        if runtime and 'run_color_static_v1.py' in runtime['scripts']:
            workers.append(('run_color_static_v1.py', 'OPENCUT_FONT_COLOR_STATIC_SCRIPT', 'color-static-runtime-lock.json'))
        for script, variable, lock in workers:
            if variable in os.environ:
                filename = Path(os.environ[variable])
            elif script != 'run_derived.py' and 'OPENCUT_FONT_DERIVE_SCRIPT' in os.environ:
                if not os.environ['OPENCUT_FONT_DERIVE_SCRIPT']:
                    raise ValueError('Configured font script is unavailable')
                filename = Path(os.environ['OPENCUT_FONT_DERIVE_SCRIPT']).with_name(script)
            else:
                filename = libexec / runtime['scripts'][script]
            if not filename.is_absolute() or not filename.is_file():
                raise ValueError('Configured font script is unavailable')
            actual = json.loads(runtime_probe([python, '-I', '-B', filename, '--identity']))
            if actual != json.loads((libexec / 'font-catalog' / lock).read_text()):
                raise ValueError('Actual Python/fontTools/script identity differs from lock')
            identities[script] = actual
        checks.append({'check': 'font_derivation_runtime_executes', 'passed': True,
                       'python_sha256': sha(python), 'identities': identities,
                       'bundled_executable_used': bool(runtime['components']) and python.is_relative_to((libexec / 'runtimes/python').resolve()),
                       'scope': 'isolated runtime identity only; no font source download or project import'})
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
        checks.append({'check': 'font_derivation_runtime_executes', 'passed': False, 'error': str(error)})


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def registered_names(output):
    return {parts[1] for line in output.splitlines()
            if len(parts := line.split()) >= 3 and set(parts[0]) <= set('.TSCAVFBDX') and parts[1] != '='}


def catalog_runtime_files(root, manifest):
    declaration = manifest['catalog_archive_runtime']
    if (declaration.get('profile') != 'catalog-archive-v1-node-zlib'
            or declaration.get('external_runtime') != 'node'
            or declaration.get('source_archive_included') is not False
            or declaration.get('source_archive_sha256') != '946fc23a63d535d693eb31b2eabb80c8c28d6351e2186b344ceb71b2cb1d5eb6'
            or declaration.get('download_switch') != 'OPENCUT_ALLOW_PUBLIC_RESOURCE_DOWNLOAD'
            or set(declaration.get('files', {})) != CATALOG_FILES):
        raise ValueError('Invalid fixed catalog runtime declaration')
    for name, expected in declaration['files'].items():
        path = root / name
        if expected != manifest['files'].get(name):
            raise ValueError('Catalog runtime identity differs from bundle file inventory')
        for ancestor in [path, *path.parents]:
            if ancestor.is_symlink():
                raise ValueError('Symlink in catalog runtime')
            if ancestor == root:
                break
        limit = 65536 if path.name == 'package.json' else 262144
        if (not path.resolve().is_relative_to(root) or not path.is_file()
                or not 0 < path.stat().st_size <= limit
                or path.stat().st_size != expected['size'] or sha(path) != expected['sha256']):
            raise ValueError('Catalog runtime bytes differ from pinned manifest')
    metadata = json.loads((root / 'bin/libexec/classic-visual-renderer/package.json').read_text())
    if (metadata.get('name'), metadata.get('version'), metadata.get('type')) != ('@opencut/classic-visual-renderer', '0.1.0', 'module'):
        raise ValueError('Invalid catalog runtime package metadata')
    return declaration


def inspect(root):
    root = Path(root).resolve()
    manifest = json.loads((root / 'manifest.json').read_text())
    checks = []
    runtime, runtime_invalid = None, False
    if 'runtime_bundle' in manifest:
        try:
            runtime = declared_runtime(root, manifest)
            checks.append({'check': 'runtime_bundle_identity', 'passed': True,
                           'delivery': runtime['delivery'], 'target': runtime['target'],
                           'files': len(runtime['files']), 'components': sorted(runtime['components']),
                           'execution_verified': False})
        except (OSError, ValueError, KeyError, TypeError) as error:
            runtime_invalid = True
            checks.append({'check': 'runtime_bundle_identity', 'passed': False, 'error': str(error)})
    for name, expected in manifest['files'].items():
        try:
            path = (root / name).resolve()
            valid = path.is_relative_to(root) and path.is_file() and not (root / name).is_symlink()
            valid = valid and path.stat().st_size == expected['size'] and sha(path) == expected['sha256']
            checks.append({'check': 'bundle_file', 'path': name, 'passed': valid})
        except OSError as error:
            checks.append({'check': 'bundle_file', 'path': name, 'passed': False, 'error': str(error)})

    def run(args):
        return subprocess.run([str(p) for p in args], capture_output=True, text=True, timeout=20, check=True)

    installed = {}
    needs_node = 'classic_packet_rate_runtime' in manifest or 'catalog_archive_runtime' in manifest or 'node' in manifest.get('external_dependencies', [])
    for name in ['ffmpeg', 'ffprobe'] + (['node'] if needs_node else []):
        node_variable = 'OPENCUT_CLASSIC_VISUAL_NODE' if 'catalog_archive_runtime' in manifest and 'classic_packet_rate_runtime' not in manifest else 'OPENCUT_CLASSIC_AUDIO_NODE'
        variables = (node_variable,) if name == 'node' else ('OPENCUT_' + name.upper(),)
        try:
            if runtime_invalid and not any(variable in os.environ for variable in variables):
                raise ValueError('Bundled runtime is damaged; external fallback refused')
            path = str(runtime_executable(root, runtime, name, variables, True, configured_relative=name != 'node'))
            value = run([path, '--version' if name == 'node' else '-version'])
            installed[name] = path
            checks.append({'check': 'external_tool', 'tool': name, 'passed': True,
                           'path': path, 'version': value.stdout.splitlines()[0],
                           'sha256': sha(Path(path)),
                           'bundled_executable_used': Path(path).is_relative_to((root / 'bin/libexec/runtimes' / name).resolve())})
        except (OSError, subprocess.SubprocessError, IndexError, ValueError) as error:
            checks.append({'check': 'external_tool', 'tool': name, 'passed': False, 'error': str(error)})
    try:
        value = run([root / 'bin/opencut', '--version'])
        checks.append({'check': 'binary_executes', 'passed': True, 'version': value.stdout.strip()})
    except (OSError, subprocess.SubprocessError) as error:
        checks.append({'check': 'binary_executes', 'passed': False, 'error': str(error)})
    if 'catalog_archive_runtime' in manifest:
        try:
            declaration = catalog_runtime_files(root, manifest)
            configured_node = os.environ.get('OPENCUT_CLASSIC_VISUAL_NODE')
            if configured_node is None and runtime_invalid:
                raise ValueError('Bundled runtime is damaged; external fallback refused')
            node = (configured_node if configured_node is not None else
                    str(runtime_executable(root, runtime, 'node', (), True)) if runtime else shutil.which('node'))
            if not node or not Path(node).is_absolute() or not Path(node).is_file() or not os.access(node, os.X_OK):
                raise FileNotFoundError('Catalog Node executable is unavailable')
            node = Path(node).resolve()
            version = run([node, '--version']).stdout.strip()
            # Built-in DEFLATE only: no audio, browser, network or workspace.
            probe = run([node, '--input-type=module', '-e', "import { inflateRawSync, deflateRawSync } from 'node:zlib'; const b=Buffer.from('catalog-zlib-v1'); if(!inflateRawSync(deflateRawSync(b),{maxOutputLength:64}).equals(b)) process.exit(1); process.stdout.write('catalog-zlib-v1');"])
            if probe.stdout != 'catalog-zlib-v1' or 'ffprobe' not in installed:
                raise ValueError('Catalog DEFLATE or media probe runtime is unavailable')
            checks.append({'check': 'catalog_archive_runtime', 'passed': True,
                           'profile': declaration['profile'], 'files': declaration['files'],
                           'node_path': str(node), 'node_version': version, 'node_sha256': sha(node),
                           'public_download_enabled': os.environ.get('OPENCUT_ALLOW_PUBLIC_RESOURCE_DOWNLOAD') == '1',
                           'source_archive_verified': False, 'browser_runtime_included': bool(runtime and 'chromium' in runtime['components']),
                           'scope': 'fixed archive adapter and media probe available; source audio and project use require separate admission'})
        except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
            checks.append({'check': 'catalog_archive_runtime', 'passed': False, 'error': str(error)})
    if runtime is not None:
        check_renderer_and_fonts(root, runtime, checks)
    if 'ffmpeg' in installed:
        try:
            filters = registered_names(run([installed['ffmpeg'], '-hide_banner', '-filters']).stdout)
            encoders = registered_names(run([installed['ffmpeg'], '-hide_banner', '-encoders']).stdout)
            for profile, required in {**FILTERS, 'export_encoders': ENCODERS}.items():
                missing = sorted(required - (encoders if profile == 'export_encoders' else filters))
                checks.append({'check': 'media_capabilities', 'profile': profile, 'passed': not missing, 'missing': missing})
            checks.append({'check': 'configurable_export_codec_inventory', 'passed': True,
                           'video_formats': [format for format, encoder in [('mp4', 'libx264'), ('webm', 'libvpx-vp9')] if encoder in encoders],
                           'video_codecs': [codec for codec, encoder in [('h264','libx264'), ('hevc','libx265'), ('vp9','libvpx-vp9')] if encoder in encoders],
                           'audio_formats': [format for format, video, audio in [('mp4', 'libx264', 'aac'), ('webm', 'libvpx-vp9', 'libopus')] if {video, audio} <= encoders],
                           'scope': 'codec inventory only; native renderer execution and fixed project admission remain required'})
        except (OSError, subprocess.SubprocessError) as error:
            checks.append({'check': 'media_capabilities', 'passed': False, 'error': str(error)})
    if {'ffmpeg', 'ffprobe'} <= installed.keys():
        try:
            with tempfile.TemporaryDirectory(prefix='opencut-doctor-') as temporary:
                output = Path(temporary) / 'smoke.mp4'
                run([installed['ffmpeg'], '-v', 'error', '-f', 'lavfi', '-i', 'color=c=red:s=32x32:r=10:d=0.2',
                     '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=0.2',
                     '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-shortest', output])
                probe = json.loads(run([installed['ffprobe'], '-v', 'error', '-show_streams', '-of', 'json', output]).stdout)
                streams = {stream['codec_type']: stream['codec_name'] for stream in probe['streams']}
                checks.append({'check': 'media_encode_probe', 'passed': streams == {'video': 'h264', 'audio': 'aac'}, 'streams': streams})
        except (OSError, subprocess.SubprocessError, ValueError, KeyError) as error:
            checks.append({'check': 'media_encode_probe', 'passed': False, 'error': str(error)})
    return {'status': 'passed' if all(item['passed'] for item in checks) else 'failed',
            'target': manifest['target'], 'checks': checks,
            'scope': 'Bundle integrity and baseline H.264/AAC runtime; project fonts, alternate codecs and exact rendering require project preflight'
                     + ('; declared Node runtime executable checked, source media acceptance remains separate' if needs_node else '')
                     + ('; fixed catalog helper identity and Node DEFLATE checked; source audio acceptance remains separate' if 'catalog_archive_runtime' in manifest else '')
                     + ('; native worker/media and font runtime identities checked; project rendering still requires independent acceptance' if 'runtime_bundle' in manifest else '')}


if __name__ == '__main__':
    try:
        report = inspect(Path(__file__).resolve().parent)
    except (OSError, ValueError, KeyError, TypeError) as error:
        report = {'status': 'failed', 'error': str(error)}
    print(json.dumps(report, indent=2))
    raise SystemExit(0 if report['status'] == 'passed' else 1)
