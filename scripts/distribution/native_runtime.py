"""Trusted build-time native media assembly; no project or application semantics.

Only the five renderer-identified FFmpeg libraries may be linked. Other codec
implementations must be captured in those binaries (e.g. static dav1d), avoiding
unrecorded transitive decoder identities. System frameworks remain platform facts.
"""
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess

_spec = importlib.util.spec_from_file_location('native_runtime_inventory', Path(__file__).with_name('runtime_bundle.py'))
runtime = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(runtime)
LIBRARIES = {'avcodec', 'avformat', 'avutil', 'swresample', 'swscale'}
TARGETS = {'x86_64-apple-darwin', 'aarch64-apple-darwin'}


def system(path):
    return path.startswith(('/System/Library/', '/usr/lib/')) and '..' not in path.split('/')


def library_name(filename):
    match = re.fullmatch(r'lib(avcodec|avformat|avutil|swresample|swscale)\.\d+(?:\.\d+\.\d+)?\.dylib', filename)
    if not match:
        raise ValueError('Native media library is outside the identified ABI set')
    return match[1]


def abi_name(filename):
    name = library_name(filename)
    major = filename[len('lib' + name + '.'):].split('.')[0]
    return 'lib' + name + '.' + major + '.dylib'


def command(args):
    subprocess.run([str(p) for p in args], check=True, capture_output=True, timeout=60)


def source_spec(path, target):
    value = runtime.read_json(path)
    if value.get('format') == 'opencut.native-media-reuse-input':
        if (set(value) != {'format', 'target', 'root', 'manifest_sha256'}
                or value['target'] != target or target not in TARGETS):
            raise ValueError('A pinned matching native runtime is required')
        root = Path(value['root'])
        if not root.is_absolute() or root.is_symlink() or not root.is_dir():
            raise ValueError('The retained runtime must be a real absolute directory')
        if runtime.sha(root / 'manifest.json') != value['manifest_sha256']:
            raise ValueError('Retained native runtime manifest identity mismatch')
        manifest = runtime.read_json(root / 'manifest.json')
        native = verify(root, manifest['native_renderer'])
        if manifest['target'] != target or native['target'] != target:
            raise ValueError('Retained native runtime target mismatch')
        provenance = root / 'THIRD_PARTY/native-media'
        # Validate every provenance file against the retained delivery inventory.
        # The original build proof and corresponding source remain byte-exact.
        entries = runtime.inventory(provenance)
        expected = {p.removeprefix('THIRD_PARTY/native-media/') for p in manifest['files']
                    if p.startswith('THIRD_PARTY/native-media/')}
        if set(entries) != expected:
            raise ValueError('Retained native provenance inventory mismatch')
        for relative, entry in entries.items():
            if (entry['kind'] != 'file' or manifest['files'].get('THIRD_PARTY/native-media/' + relative)
                    != {'sha256': entry.get('sha256'), 'size': entry.get('size')}):
                raise ValueError('Retained native provenance identity mismatch')
        if (provenance / 'runtime-reuse.json').exists():
            raise ValueError('Select the original retained runtime, not a chain of repackaged inputs')
        prepared = {'root': str(root / 'bin'), 'provenance_root': str(provenance),
                    'reuse': {'format': 'opencut.native-media-reuse', 'target': target,
                              'source_manifest_sha256': value['manifest_sha256'],
                              'source_native_renderer': native}}
        libraries = {Path(p).name: native['files'][p] for p in native['libraries'].values()}
        return prepared, {'libraries': libraries}
    if (set(value) != {'format', 'target', 'root', 'provenance_root'}
            or value['format'] != 'opencut.native-media-input' or value['target'] != target or target not in TARGETS):
        raise ValueError('An explicit macOS native media input and matching target are required')
    for name in ('root', 'provenance_root'):
        root = Path(value[name])
        if not root.is_absolute() or root.is_symlink() or not root.is_dir():
            raise ValueError('Native media inputs must be real absolute directories')
    proof = runtime.read_json(Path(value['provenance_root']) / 'build.json')
    if (proof.get('format') != 'opencut.native-media-build' or proof.get('target') != target
            or proof.get('finished') is not True or proof.get('license') != 'LGPL-2.1-or-later'
            or not {'--disable-gpl', '--disable-nonfree', '--disable-version3', '--enable-shared', '--enable-libdav1d'} <= set(proof.get('configuration', []))
            or any(flag in proof['configuration'] for flag in ('--enable-gpl', '--enable-nonfree', '--enable-version3'))
            or {library_name(name) for name in proof.get('libraries', {})} != LIBRARIES
            or len(proof['libraries']) != len(LIBRARIES)):
        raise ValueError('Native media build needs five identified LGPL shared libraries and static codec provenance')
    provenance = Path(value['provenance_root'])
    entries = runtime.inventory(provenance)
    required = {'build.json', 'build-media-runtime.py', 'licenses/FFmpeg-LGPL-2.1.txt', 'licenses/dav1d-COPYING.txt'}
    if not required <= set(entries) or any(entry['kind'] != 'file' for entry in entries.values()):
        raise ValueError('Native media corresponding source, build recipe and licenses are required')
    for source in proof.get('source_inputs', []):
        archive = provenance / 'sources' / Path(source['archive_url']).name
        if runtime.sha(archive) != source['archive_sha256']:
            raise ValueError('Native media corresponding source identity mismatch')
    if not {'ffmpeg', 'dav1d'} <= {Path(s['archive_url']).name.split('-')[0] for s in proof.get('source_inputs', [])}:
        raise ValueError('FFmpeg and static dav1d corresponding sources are required')
    for name, expected in proof['libraries'].items():
        lib = Path(value['root']) / 'lib' / name
        if lib.is_symlink() or not lib.is_file() or expected != {'sha256': runtime.sha(lib), 'size': lib.stat().st_size}:
            raise ValueError('Native media bytes differ from the retained build')
    return value, proof


def bundle(host, cli, worker, target, spec_path):
    """Relocate copies only; never rewrite build inputs or developer libraries."""
    host = Path(host)
    spec, proof = source_spec(spec_path, target)
    binaries = {'opencut': Path(cli), 'opencut-render-worker': Path(worker)}
    for source in binaries.values():
        slices = runtime.macho_commands(source)
        if source.is_symlink() or not source.is_file() or not source.stat().st_mode & 0o111 or not slices or not all(s['executable'] for s in slices):
            raise ValueError('Native CLI and worker must be explicit Mach-O executables')
    destination = host / 'bin'
    destination.mkdir(parents=True, exist_ok=True)
    (destination / 'lib').mkdir()
    for name, source in binaries.items():
        shutil.copyfile(source, destination / name)
        (destination / name).chmod(0o755)
    libraries = {abi_name(name): name for name in proof['libraries']}
    for name, original in libraries.items():
        shutil.copyfile(Path(spec['root']) / 'lib' / original, destination / 'lib' / name)
    paths = [destination / name for name in binaries] + [destination / 'lib' / name for name in libraries]
    for path in paths:
        executable = path.parent == destination
        slices = runtime.macho_commands(path)
        if not slices:
            raise ValueError('Native media input is not Mach-O')
        dependencies = {name for part in slices for name in part['libraries'] if not system(name)}
        for original in sorted(dependencies):
            name = abi_name(Path(original).name)
            if name not in libraries:
                raise ValueError('Unidentified transitive native dependency: ' + name)
            replacement = ('@executable_path/lib/' if executable else '@loader_path/') + name
            if original != replacement:
                command(['/usr/bin/install_name_tool', '-change', original, replacement, path])
        for rpath in sorted({p for part in slices for p in part['rpaths']}):
            # Every non-system load now uses an exact relative path; no search
            # path can pull a codec from a developer installation.
            command(['/usr/bin/install_name_tool', '-delete_rpath', rpath, path])
        if not executable:
            command(['/usr/bin/install_name_tool', '-id', '@loader_path/' + path.name, path])
        command(['/usr/bin/codesign', '--force', '--sign', '-', path])
    runtime.copy_tree(spec['provenance_root'], host / 'THIRD_PARTY/native-media')
    if 'reuse' in spec:
        (host / 'THIRD_PARTY/native-media/runtime-reuse.json').write_text(json.dumps(spec['reuse'], indent=2) + '\n')
    records = {p.relative_to(host).as_posix(): {'sha256': runtime.sha(p), 'size': p.stat().st_size} for p in paths}
    value = {'format': 'opencut.native-renderer', 'target': target, 'cli': 'bin/opencut',
             'worker': 'bin/opencut-render-worker', 'libraries': {library_name(p.name): p.relative_to(host).as_posix() for p in paths if p.parent.name == 'lib'},
             'build': 'THIRD_PARTY/native-media/build.json', 'build_sha256': runtime.sha(host / 'THIRD_PARTY/native-media/build.json'),
             'files': records}
    verify(host, value)
    return value


def verify(host, value):
    host = Path(host).resolve(strict=True)
    if (set(value) != {'format', 'target', 'cli', 'worker', 'libraries', 'build', 'build_sha256', 'files'}
            or value['format'] != 'opencut.native-renderer' or value['target'] not in TARGETS
            or value['cli'] != 'bin/opencut' or value['worker'] != 'bin/opencut-render-worker'
            or value['build'] != 'THIRD_PARTY/native-media/build.json' or set(value['libraries']) != LIBRARIES):
        raise ValueError('Invalid native renderer declaration')
    expected = {value['cli'], value['worker']} | set(value['libraries'].values())
    if set(value['files']) != expected or runtime.sha(host / value['build']) != value['build_sha256']:
        raise ValueError('Native renderer file/provenance identity mismatch')
    for name, path in value['libraries'].items():
        if runtime.relative(path) != 'bin/lib/' + Path(path).name or library_name(Path(path).name) != name:
            raise ValueError('Invalid native media library location')
    for relative, entry in value['files'].items():
        path = host / runtime.relative(relative)
        if (path.is_symlink() or not path.resolve(strict=True).is_relative_to(host) or not path.is_file()
                or entry != {'sha256': runtime.sha(path), 'size': path.stat().st_size}):
            raise ValueError('Native renderer bytes differ from declaration')
        parts = runtime.macho_commands(path)
        executable = relative in (value['cli'], value['worker'])
        allowed = {(('@executable_path/lib/' if executable else '@loader_path/') + Path(p).name) for p in value['libraries'].values()}
        if (not parts or any(part['executable'] != executable or part['rpaths']
                             or any(not system(p) and p not in allowed for p in part['libraries']) for part in parts)):
            raise ValueError('Native renderer contains an external or unidentified dynamic dependency')
    return value


def probe_worker(host, value, timeout=20):
    """Exercise the existing private worker protocol with an authored tiny WAV.

    This proves relocated worker/libav startup and bounded source callbacks only,
    not GPU rendering, decoded pixels/PCM, or project authorization acceptance.
    """
    import json
    import os
    import selectors
    import signal
    import struct
    import time
    verify(host, value)
    host = Path(host)
    pcm = bytes(256)
    wave = b'RIFF' + struct.pack('<I', 36 + len(pcm)) + b'WAVEfmt ' + struct.pack('<IHHIIHH', 16, 1, 1, 48000, 96000, 2, 16) + b'data' + struct.pack('<I', len(pcm)) + pcm
    env = {key: val for key, val in os.environ.items() if not key.startswith(('DYLD_', 'OPENCUT_'))}
    child = subprocess.Popen([host / value['worker']], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, start_new_session=True, env=env)
    selector = selectors.DefaultSelector()
    for pipe in (child.stdout, child.stderr):
        os.set_blocking(pipe.fileno(), False)
        selector.register(pipe, selectors.EVENT_READ)
    end = time.monotonic() + timeout
    buffer, stderr = bytearray(), bytearray()
    ready, identity, requests, last_id = False, None, 0, 0

    def send(meta, data=b''):
        header = json.dumps(meta, separators=(',', ':')).encode()
        child.stdin.write(struct.pack('<II', len(header), len(data)) + header + data)
        child.stdin.flush()

    try:
        send({'type': 'initialize', 'purpose': 'packet_rate', 'plan': {'size': len(wave)}})
        while time.monotonic() < end:
            events = selector.select(min(.25, max(0, end - time.monotonic())))
            for key, _ in events:
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                if key.fileobj is child.stderr:
                    stderr.extend(data)
                    if len(stderr) > 65536:
                        raise ValueError('Native probe stderr exceeds budget')
                else:
                    buffer.extend(data)
                    if len(buffer) > 2 * 1024 * 1024 + 65536 + 8:
                        raise ValueError('Native probe packet exceeds budget')
            while len(buffer) >= 8:
                meta_length, payload_length = struct.unpack('<II', buffer[:8])
                if not 0 < meta_length <= 2 * 1024 * 1024 or payload_length > 65536:
                    raise ValueError('Invalid native probe packet envelope')
                length = 8 + meta_length + payload_length
                if len(buffer) < length:
                    break
                meta = json.loads(buffer[8:8 + meta_length], object_pairs_hook=runtime.objects_only)
                payload = buffer[8 + meta_length:length]
                del buffer[:length]
                if payload:
                    raise ValueError('Unexpected native probe output data')
                if meta.get('type') == 'ready':
                    identity = meta['identity']
                    libraries = identity['libraries']
                    if (ready or meta.get('purpose') != 'packet_rate' or meta.get('pid') != child.pid
                            or identity['worker_sha256'] != value['files'][value['worker']]['sha256']
                            or len(libraries) != len(LIBRARIES) or {lib['name'] for lib in libraries} != LIBRARIES
                            or any(lib['binary_sha256'] != value['files'][value['libraries'][lib['name']]]['sha256'] for lib in libraries)):
                        raise ValueError('Relocated native probe identity mismatch')
                    ready = True
                elif meta.get('type') == 'source_read':
                    offset, count, seq = meta['offset'], meta['count'], meta['id']
                    if (not ready or type(offset) is not int or type(count) is not int or type(seq) is not int
                            or not 0 <= offset <= len(wave) or not 1 <= count <= 65536 or seq != last_id + 1 or requests >= 256):
                        raise ValueError('Invalid native probe source request')
                    requests += 1
                    last_id = seq
                    data = wave[offset:offset + count]
                    send({'type': 'source_reply', 'id': seq, 'offset': offset, 'eof': offset + len(data) == len(wave)}, data)
                elif meta.get('type') == 'packet_complete':
                    if (not ready or not requests or meta.get('identity') != identity
                            or meta.get('observation') != {'schema_version': 1, 'packet_count': 0, 'average_packet_rate': None}):
                        raise ValueError('Native probe completion mismatch')
                    child.stdin.close()
                    child.wait(timeout=max(.01, end - time.monotonic()))
                    if child.returncode or buffer:
                        raise ValueError('Native probe did not exit successfully')
                    verify(host, value)
                    return {'identity': identity, 'source_requests': requests, 'source_bytes': len(wave),
                            'scope': 'relocated worker/libav startup and authorized-byte protocol smoke; no GPU/pixel/PCM or project acceptance'}
                else:
                    raise ValueError('Unexpected native probe reply: ' + str(meta.get('type')))
            if not selector.get_map() or child.poll() is not None:
                raise ValueError('Native probe exited before completion: ' + stderr[-2048:].decode(errors='replace'))
        raise ValueError('Native probe exceeded deadline')
    finally:
        selector.close()
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()
        for pipe in (child.stdin, child.stdout, child.stderr):
            pipe.close()
