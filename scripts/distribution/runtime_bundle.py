"""Explicit, bounded local runtime delivery. Never acquires or installs software.

Runtime specs are trusted build inputs, not host/API inputs. The emitted manifest
contains relative paths and byte identities only. A declaration is not execution
evidence: bundled installations remain unverified until the relocated doctor runs.
"""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import struct

FORMAT_V1 = 'opencut.runtime-bundle.v1'
FORMAT = 'opencut.runtime-bundle.v2'
NATIVE_FORMAT = 'opencut.native-runtime-bundle'
MAX_FILES = 100000
MAX_FILE = 512 * 1024 * 1024
MAX_TOTAL = 8 * 1024 * 1024 * 1024
MAX_MANIFEST = 16 * 1024 * 1024
COMPONENTS = {'node', 'chromium', 'python'}
NATIVE_COMPONENTS = {'ffmpeg', 'ffprobe', 'node', 'python'}
SCRIPTS = ('run_derived.py', 'run_derived_v2.py', 'derive_static.py',
           'derive_renamed_v2.py', 'generate.py', 'fonttools-lock.json',
           'derived-runtime-lock.json', 'derived-v2-runtime-lock.json',
           'run_color_static_v1.py', 'derive_color_static_v1.py', 'color-static-runtime-lock.json')
WORKERS_V1 = ('run_derived.py', 'run_derived_v2.py')
WORKERS = WORKERS_V1 + ('run_color_static_v1.py',)


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def relative(value):
    if (not isinstance(value, str) or not value or len(value) > 2048
            or '\\' in value or any(ord(c) < 32 for c in value)):
        raise ValueError('Invalid runtime relative path')
    path = PurePosixPath(value)
    if path.is_absolute() or any(p in ('', '.', '..') for p in value.split('/')):
        raise ValueError('Invalid runtime relative path')
    return value


def objects_only(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('Duplicate runtime manifest key')
        result[key] = value
    return result


def read_json(path, limit=MAX_MANIFEST):
    if not Path(path).is_file() or not 0 < Path(path).stat().st_size <= limit:
        raise ValueError('Runtime JSON exceeds budget')
    return json.loads(Path(path).read_text(), object_pairs_hook=objects_only)


def inventory(root, excluded=()):
    """Do not traverse links. Every link must remain inside the delivered root."""
    root = Path(root).resolve(strict=True)
    result, total, count = {}, 0, 0
    for directory, directories, files in os.walk(root, followlinks=False):
        for name in sorted(directories + files):
            count += 1
            if count > MAX_FILES:
                raise ValueError('Runtime inventory exceeds count budget')
            path = Path(directory) / name
            key = path.relative_to(root).as_posix()
            relative(key)
            if key in excluded:
                continue
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                target = os.readlink(path)
                if (not target or Path(target).is_absolute() or '\\' in target
                        or any(ord(c) < 32 for c in target)
                        or not path.resolve(strict=True).is_relative_to(root)):
                    raise ValueError(f'Runtime link escapes delivery: {key}')
                result[key] = {'kind': 'symlink', 'target': target}
            elif stat.S_ISREG(metadata.st_mode):
                size = metadata.st_size
                total += size
                if size > MAX_FILE or total > MAX_TOTAL or metadata.st_mode & 0o6000:
                    raise ValueError(f'Runtime file exceeds budget or has special mode: {key}')
                result[key] = {'kind': 'file', 'sha256': sha(path), 'size': size,
                               'executable': bool(metadata.st_mode & 0o111)}
            elif not stat.S_ISDIR(metadata.st_mode):
                raise ValueError(f'Unsupported runtime entry: {key}')
            if len(result) > MAX_FILES:
                raise ValueError('Runtime inventory exceeds count budget')
    return dict(sorted(result.items()))


def copy_tree(source, target):
    source = Path(source)
    if source.is_symlink() or not source.is_dir():
        raise ValueError('Runtime root must be a real directory')
    # Full preflight before creating any links in the destination.
    entries = inventory(source)
    target.mkdir(parents=True, exist_ok=True)
    for directory, directories, _ in os.walk(source, followlinks=False):
        for name in directories:
            path = Path(directory) / name
            if not path.is_symlink():
                (target / path.relative_to(source)).mkdir(parents=True, exist_ok=True)
    for name, entry in entries.items():
        destination = target / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        if entry['kind'] == 'symlink':
            destination.symlink_to(entry['target'])
        else:
            shutil.copyfile(source / name, destination)
            destination.chmod(0o755 if entry['executable'] else 0o644)
    if inventory(target) != entries:
        raise ValueError('Runtime source changed during copy')


def macho_commands(path):
    """Read load commands only; no execution of a supplied runtime or shell tool."""
    with Path(path).open('rb') as file:
        length = Path(path).stat().st_size

        def read(offset, count):
            if offset < 0 or count < 0 or offset + count > length or count > 16 * 1024 * 1024:
                raise ValueError('Invalid Mach-O load command envelope')
            file.seek(offset)
            value = file.read(count)
            if len(value) != count:
                raise ValueError('Truncated Mach-O runtime')
            return value

        def thin(offset, available):
            magic = read(offset, 4)
            layouts = {b'\xcf\xfa\xed\xfe': ('<', 32), b'\xce\xfa\xed\xfe': ('<', 28),
                       b'\xfe\xed\xfa\xcf': ('>', 32), b'\xfe\xed\xfa\xce': ('>', 28)}
            if magic not in layouts:
                raise ValueError('Unsupported binary in universal Mach-O runtime')
            endian, header_size = layouts[magic]
            header = read(offset, header_size)
            kind, count, size = struct.unpack_from(endian + 'III', header, 12)
            if count > 65536 or size > available - header_size:
                raise ValueError('Mach-O command budget exceeded')
            commands = read(offset + header_size, size)
            cursor, libraries, rpaths = 0, [], []
            for _ in range(count):
                if cursor + 8 > len(commands):
                    raise ValueError('Invalid Mach-O command')
                command, command_size = struct.unpack_from(endian + 'II', commands, cursor)
                if command_size < 8 or cursor + command_size > len(commands):
                    raise ValueError('Invalid Mach-O command size')
                # LC_LOAD_DYLIB / WEAK / REEXPORT / UPWARD; exclude LC_ID_DYLIB.
                if command in (0xc, 0x80000018, 0x8000001f, 0x80000023, 0x8000001c):
                    if command_size < 12:
                        raise ValueError('Invalid Mach-O dependency command')
                    start = struct.unpack_from(endian + 'I', commands, cursor + 8)[0]
                    if not 12 <= start < command_size:
                        raise ValueError('Invalid Mach-O dependency path')
                    raw = commands[cursor + start:cursor + command_size]
                    if b'\0' not in raw:
                        raise ValueError('Unterminated Mach-O dependency path')
                    value = raw.split(b'\0', 1)[0].decode('utf-8')
                    (rpaths if command == 0x8000001c else libraries).append(value)
                cursor += command_size
            if cursor != len(commands):
                raise ValueError('Unconsumed Mach-O command bytes')
            return {'executable': kind == 2, 'libraries': libraries, 'rpaths': rpaths}

        if length < 4:
            return []
        magic = read(0, 4)
        if magic in (b'\xca\xfe\xba\xbe', b'\xca\xfe\xba\xbf'):
            count = struct.unpack('>I', read(4, 4))[0]
            if not 1 <= count <= 16:
                raise ValueError('Mach-O architecture budget exceeded')
            wide = magic[-1] == 0xbf
            entry_size = 32 if wide else 20
            result = []
            for index in range(count):
                entry = read(8 + index * entry_size, entry_size)
                offset, size = struct.unpack_from('>QQ' if wide else '>II', entry, 8)
                if offset + size > length:
                    raise ValueError('Invalid universal Mach-O slice')
                result.append(thin(offset, size))
            return result
        if magic in (b'\xcf\xfa\xed\xfe', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xfe\xed\xfa\xce'):
            return [thin(0, length)]
        return []


def verify_macos_linkage(root, entry):
    """Conservative closure: host OS libraries or paths inside this component.

    Unresolved @rpath is refused, even if a developer machine could satisfy it.
    Doctor still executes the relocated entry: this parser is not execution proof.
    """
    root, entry = Path(root).resolve(strict=True), Path(entry).resolve(strict=True)
    main = macho_commands(entry)
    if not main or not all(item['executable'] for item in main):
        raise ValueError('Bundled macOS entry must be a Mach-O executable')
    main_rpaths = [rpath for item in main for rpath in item['rpaths']]

    def system(value):
        return value.startswith(('/System/Library/', '/usr/lib/')) and '..' not in value.split('/')

    for name, record in inventory(root).items():
        if record['kind'] != 'file':
            continue
        path = root / name
        for commands in macho_commands(path):
            executable = path if commands['executable'] else entry

            def expanded(value):
                if value.startswith('@loader_path/'):
                    resolved = path.parent / value[len('@loader_path/'):]
                elif value.startswith('@executable_path/'):
                    resolved = executable.parent / value[len('@executable_path/'):]
                elif value in ('@loader_path', '@executable_path'):
                    resolved = path.parent if value == '@loader_path' else executable.parent
                elif system(value):
                    return Path(value)
                else:
                    raise ValueError('Runtime has an external or unresolved dynamic-library path')
                resolved = resolved.resolve()
                if not resolved.is_relative_to(root):
                    raise ValueError('Runtime dynamic-library path escapes its component')
                return resolved

            rpaths = [expanded(value) for value in commands['rpaths']]
            # Main executable RPATH values are relative to its own loader, not
            # whichever dependent dylib is currently being inspected.
            for value in main_rpaths:
                if value.startswith(('@loader_path/', '@executable_path/')):
                    candidate = (entry.parent / value.split('/', 1)[1]).resolve()
                    if not candidate.is_relative_to(root):
                        raise ValueError('Runtime main RPATH escapes its component')
                    rpaths.append(candidate)
                elif system(value):
                    rpaths.append(Path(value))
            for library in commands['libraries']:
                if system(library):
                    continue
                if library.startswith('@rpath/'):
                    suffix = library[len('@rpath/'):]
                    if not suffix or '..' in suffix.split('/'):
                        raise ValueError('Invalid runtime RPATH dependency')
                    candidates = [(prefix / suffix).resolve() for prefix in rpaths]
                    if not any(candidate.is_relative_to(root) and candidate.is_file() for candidate in candidates):
                        raise ValueError('Runtime RPATH dependency is not bundled')
                else:
                    resolved = expanded(library)
                    if not resolved.is_relative_to(root) or not resolved.is_file():
                        raise ValueError('Runtime dynamic library is not bundled')


def validate_spec(path, target):
    spec = read_json(path, 65536)
    if (set(spec) != {'format', 'target', 'components'}
            or spec['format'] != 'opencut.native-runtime-input' or spec['target'] != target
            or not isinstance(spec['components'], dict) or not set(spec['components']) <= NATIVE_COMPONENTS):
        raise ValueError('Native runtime spec must name the target and only FFmpeg/ffprobe/Node/Python components')
    if target not in ('x86_64-apple-darwin', 'aarch64-apple-darwin'):
        raise ValueError('Bundled executable linkage validation currently supports macOS only; use explicit external runtimes on this target')
    for name, component in spec['components'].items():
        if (not isinstance(component, dict)
                or set(component) != {'root', 'entry', 'version', 'license_spdx', 'license_files', 'redistributable'} | ({'corresponding_sources'} if name in {'ffmpeg', 'ffprobe'} else set())
                or component['redistributable'] is not True
                or not isinstance(component['version'], str) or not 0 < len(component['version']) <= 128
                or not isinstance(component['license_spdx'], str) or not 0 < len(component['license_spdx']) <= 128
                or any(ord(c) < 32 for c in component['version'] + component['license_spdx'])):
            raise ValueError('Invalid explicit runtime component declaration')
        root = Path(component['root'])
        if not root.is_absolute() or root.is_symlink() or not root.is_dir():
            raise ValueError('Runtime input must be an absolute real directory')
        root = root.resolve(strict=True)
        entry = root / relative(component['entry'])
        if not entry.resolve(strict=True).is_relative_to(root) or not entry.is_file() or not entry.stat().st_mode & 0o111:
            raise ValueError('Runtime entry must be an internal executable')
        licenses = component['license_files']
        if not isinstance(licenses, list) or not 1 <= len(licenses) <= 128 or len(set(licenses)) != len(licenses):
            raise ValueError('Explicit redistribution licenses are required')
        for notice in licenses:
            notice = root / relative(notice)
            if (not notice.resolve(strict=True).is_relative_to(root) or not notice.is_file()
                    or not 0 < notice.stat().st_size <= 8 * 1024 * 1024):
                raise ValueError('Runtime license missing or invalid')
        if name in {'ffmpeg', 'ffprobe'}:
            sources = component['corresponding_sources']
            if not isinstance(sources, list) or not 2 <= len(sources) <= 128 or len(set(sources)) != len(sources):
                raise ValueError('Media tools require corresponding source archives and a build recipe')
            for source in sources:
                source = root / relative(source)
                if not source.resolve(strict=True).is_relative_to(root) or not source.is_file() or not 0 < source.stat().st_size <= MAX_FILE:
                    raise ValueError('Media tool corresponding source is missing or invalid')
        if name == 'python' and any(root.rglob('pyvenv.cfg')):
            raise ValueError('A virtual environment is not a standalone Python distribution')
    return spec


def python_identity(root, lock):
    """Read the locked implementation without executing the input interpreter."""
    roots = list(root.rglob('fontTools/__init__.py'))
    if len(roots) != 1 or not list(root.rglob('encodings/__init__.py')) or not list(root.rglob('os.py')):
        raise ValueError('Standalone Python standard library and one fontTools are required')
    tools = roots[0].parent
    metadata = list(tools.parent.glob('fonttools-*.dist-info/METADATA'))
    if len(metadata) != 1 or ('Version: ' + lock['fonttools_version'] + '\n') not in metadata[0].read_text():
        raise ValueError('fontTools distribution metadata differs from lock')
    notices = [p for p in metadata[0].parent.rglob('*') if p.is_file() and p.name.lower().startswith(('license', 'copying'))]
    if not notices or not all(0 < p.stat().st_size <= 8 * 1024 * 1024 for p in notices):
        raise ValueError('fontTools redistribution license is required')
    files = [(p.relative_to(tools).as_posix(), sha(p)) for p in sorted(tools.rglob('*.py'))]
    digest = hashlib.sha256(json.dumps(files, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()
    if digest != lock['fonttools_implementation_sha256']:
        raise ValueError('fontTools implementation differs from pinned derivation runtime')
    return {'fonttools_version': lock['fonttools_version'], 'implementation_sha256': digest,
            'licenses': [p.relative_to(root).as_posix() for p in sorted(notices)]}


def bundle(repository, destination, target, spec_path=None):
    repository, destination = Path(repository), Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    # The catalog adapter is copied by the host assembler. Native execution
    # requires no browser renderer modules or third-party Node packages.
    shutil.copyfile(repository / 'LICENSE', destination / 'LICENSE')
    scripts = destination / 'font-catalog'
    scripts.mkdir()
    for name in SCRIPTS:
        source = repository / 'scripts/font-catalog' / name
        if source.is_symlink() or not source.is_file() or not 0 < source.stat().st_size <= 8 * 1024 * 1024:
            raise ValueError('Missing fixed font derivation implementation')
        shutil.copyfile(source, scripts / name)
    shutil.copyfile(repository / 'LICENSE', scripts / 'LICENSE')
    components = {}
    if spec_path is not None:
        spec = validate_spec(spec_path, target)
        lock = read_json(scripts / 'derived-runtime-lock.json', 65536)
        lock_v2 = read_json(scripts / 'derived-v2-runtime-lock.json', 65536)
        lock_color = read_json(scripts / 'color-static-runtime-lock.json', 65536)
        for key in ('fonttools_version', 'fonttools_implementation_sha256', 'python_version'):
            if lock[key] != lock_v2[key] or lock[key] != lock_color[key]:
                raise ValueError('Derivation runtimes require incompatible Python installations')
        for name, component in spec['components'].items():
            prefix = 'runtimes/' + name
            copied = destination / prefix
            copy_tree(Path(component['root']), copied)
            verify_macos_linkage(copied, copied / component['entry'])
            components[name] = {'entry': prefix + '/' + component['entry'],
                                'version': component['version'], 'license_spdx': component['license_spdx'],
                                'licenses': [prefix + '/' + p for p in component['license_files']]}
            if name in {'ffmpeg', 'ffprobe'}:
                components[name]['corresponding_sources'] = [prefix + '/' + p for p in component['corresponding_sources']]
            if name == 'python':
                if component['version'] != lock['python_version']:
                    raise ValueError('Python version declaration differs from derivation lock')
                components[name]['fonttools'] = python_identity(copied, lock)
    result = {'format': NATIVE_FORMAT, 'schema_version': 1, 'target': target,
              'delivery': 'bundled_unverified' if components else 'external_executables',
              'components': components,
              'scripts': {name: 'font-catalog/' + name for name in WORKERS},
              'external_dependencies': sorted(NATIVE_COMPONENTS - set(components)),
              'files': inventory(destination)}
    serialized = json.dumps(result, indent=2) + '\n'
    if len(serialized.encode()) > MAX_MANIFEST:
        raise ValueError('Runtime manifest exceeds budget')
    (destination / 'runtime-bundle.json').write_text(serialized)
    return result


def verify(root):
    root = Path(root).resolve(strict=True)
    path = root / 'runtime-bundle.json'
    if path.is_symlink():
        raise ValueError('Runtime manifest must not be a link')
    manifest = read_json(path)
    version = (manifest.get('format'), manifest.get('schema_version'))
    native = version == (NATIVE_FORMAT, 1)
    components = set(manifest.get('components', {}))
    allowed_components = components <= NATIVE_COMPONENTS if native else components in (set(), COMPONENTS)
    dependencies = (sorted(NATIVE_COMPONENTS - components) if native
                    else ['ffmpeg', 'ffprobe'] + ([] if components else sorted(COMPONENTS)))
    workers = WORKERS_V1 if version == (FORMAT_V1, 1) else WORKERS
    if (set(manifest) != {'format', 'schema_version', 'target', 'delivery', 'components', 'scripts', 'external_dependencies', 'files'}
            or version not in ((FORMAT_V1, 1), (FORMAT, 2), (NATIVE_FORMAT, 1))
            or not allowed_components
            or manifest['delivery'] != ('bundled_unverified' if manifest['components'] else 'external_executables')
            or manifest['scripts'] != {name: 'font-catalog/' + name for name in workers}
            or manifest['external_dependencies'] != dependencies):
        raise ValueError('Invalid runtime bundle declaration')
    if native and any(not native_payload(name, components) for name in manifest['files']):
        raise ValueError('Native runtime contains an undeclared or browser renderer payload')
    if inventory(root, ('runtime-bundle.json',)) != manifest['files']:
        raise ValueError('Runtime mirror differs from manifest')
    for name, component in manifest['components'].items():
        expected_keys = {'entry', 'version', 'license_spdx', 'licenses'} | ({'fonttools'} if name == 'python' else set()) | ({'corresponding_sources'} if name in {'ffmpeg', 'ffprobe'} else set())
        if (not isinstance(component, dict) or set(component) != expected_keys
                or any(not isinstance(component[key], str) or not 0 < len(component[key]) <= 128
                       or any(ord(c) < 32 for c in component[key]) for key in ('version', 'license_spdx'))
                or not isinstance(component['licenses'], list) or not 1 <= len(component['licenses']) <= 128):
            raise ValueError('Invalid runtime component identity')
        prefix = root / 'runtimes' / name
        executable = root / relative(component['entry'])
        if (not executable.resolve(strict=True).is_relative_to(prefix)
                or not executable.is_file() or not executable.stat().st_mode & 0o111):
            raise ValueError('Runtime executable escapes its component')
        for license in component['licenses']:
            if (not (root / relative(license)).resolve(strict=True).is_relative_to(prefix)
                    or not (root / license).is_file()):
                raise ValueError('Runtime license escapes its component')
        if name in {'ffmpeg', 'ffprobe'}:
            sources = component['corresponding_sources']
            if not isinstance(sources, list) or not 2 <= len(sources) <= 128 or len(set(sources)) != len(sources):
                raise ValueError('Media tools require corresponding source archives and a build recipe')
            for source in sources:
                path = root / relative(source)
                if not path.resolve(strict=True).is_relative_to(prefix) or not path.is_file() or not 0 < path.stat().st_size <= MAX_FILE:
                    raise ValueError('Media tool corresponding source escapes its component or is empty')
        for path in manifest['files']:
            if path.startswith('runtimes/' + name + '/') and not (root / path).resolve(strict=True).is_relative_to(prefix):
                raise ValueError('Runtime link escapes its component')
        if name == 'python':
            lock = read_json(root / 'font-catalog/derived-runtime-lock.json', 65536)
            if component['version'] != lock['python_version'] or component['fonttools'] != python_identity(prefix, lock):
                raise ValueError('Bundled fontTools identity differs from lock')
    return manifest


def native_payload(name, components):
    return (name == 'LICENSE' or name in {'font-catalog/' + p for p in (*SCRIPTS, 'LICENSE')}
            or name in {'classic-visual-renderer/package.json', 'classic-visual-renderer/host/catalog-archive.mjs'}
            or any(name.startswith('runtimes/' + component + '/') for component in components))
